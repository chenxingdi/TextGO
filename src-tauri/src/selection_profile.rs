//! Per-application measurements that keep the next selection on the fastest safe path.
//!
//! Every selection pays for the source application, and those costs differ by orders of magnitude.
//! A browser answers the accessibility queries TextGO needs within tens of milliseconds; a Qt4
//! reader such as GoldenDict answers the same queries in hundreds of milliseconds and publishes no
//! text control at all. The fixed timeouts the selection path relied on therefore fit neither: they
//! are too long for the browser and still too short for the reader.
//!
//! This module remembers what each application actually did, so a selection can skip work that has
//! already proved useless for that application:
//!
//! * the surrounding-text capture overran its inline budget, so the next selection does not wait
//!   for it - the popup still reads the finished capture through `get_selection_context`;
//! * the application publishes no text control, so its next selection goes straight to the document
//!   fallback instead of spending hundreds of milliseconds in a walk that returns nothing.
//!
//! It also holds the clipboard settle wait. That one is a fixed decision rather than a measurement:
//! a browser publishes a copy almost immediately, while a Qt4 reader keeps writing after its
//! clipboard sequence number moves, and another process opening the clipboard inside that window
//! crashes it.
//!
//! The measurements live in memory only. They describe this machine's current behaviour, cost
//! nothing to rebuild after a restart, and a stale entry can only make the next selection do less
//! work - never read a copy earlier than the safe wait.

use crate::platform;
use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};

/// Settle wait used for applications that are not browsers.
///
/// A Qt4 based reader keeps writing its clipboard formats after the sequence number moves, and
/// another process opening the clipboard inside that window crashes it, so this wait stays.
pub const DEFAULT_SETTLE_MS: u64 = 50;

/// Settle wait used for browsers, whose copies are readable almost immediately.
///
/// Measured against Chrome: the sequence number moves about 30 ms after the copy keys are sent and
/// the text is readable 5 ms later. The read that follows still polls, so a browser needing
/// slightly longer is covered as well.
pub const BROWSER_SETTLE_MS: u64 = 15;

/// Applications tracked at once; one entry is dropped beyond this.
const MAX_PROFILES: usize = 32;

/// What one application was measured doing.
#[derive(Clone, Copy, Debug, Default)]
struct Profile {
    /// Whether the application published readable text controls for its document; `None` until it
    /// has been looked at. Only the Windows capture walks text controls, so on macOS this stays
    /// `None`.
    text_nodes: Option<bool>,
    /// Whether the surrounding-text capture overran the inline budget last time.
    slow_capture: bool,
}

impl Profile {
    /// State of an application that has not been measured yet.
    const UNMEASURED: Profile = Profile {
        text_nodes: None,
        slow_capture: false,
    };
}

static PROFILES: LazyLock<Mutex<HashMap<String, Profile>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// Read a profile, or the empty one when the application is unknown.
fn with_profile<T>(app_id: &str, read: impl FnOnce(&Profile) -> T) -> T {
    let profiles = PROFILES.lock().unwrap_or_else(|error| error.into_inner());
    read(profiles.get(app_id).unwrap_or(&Profile::UNMEASURED))
}

/// Update a profile, creating it on first use and keeping the table bounded.
fn update_profile(app_id: &str, update: impl FnOnce(&mut Profile)) {
    if app_id.is_empty() {
        return;
    }

    let mut profiles = PROFILES.lock().unwrap_or_else(|error| error.into_inner());
    if !profiles.contains_key(app_id) && profiles.len() >= MAX_PROFILES {
        // drop an entry that still holds only defaults first, any entry otherwise
        let removable = profiles
            .iter()
            .find(|(_key, profile)| profile.text_nodes.is_none() && !profile.slow_capture)
            .map(|(key, _profile)| key.clone())
            .or_else(|| profiles.keys().next().cloned());
        if let Some(key) = removable {
            profiles.remove(&key);
        }
    }
    update(profiles.entry(app_id.to_string()).or_default());
}

/// Settle wait that is safe for this application.
///
/// Only a browser gets the short wait. Every other application keeps the full one, because reading
/// a copy while the source application is still publishing is what crashes Qt4 based readers.
///
/// @param app_id - identifier of the application the selection came from
/// @returns milliseconds to wait after the clipboard sequence number changes
pub fn settle_ms(app_id: &str) -> u64 {
    if platform::is_browser(app_id) {
        BROWSER_SETTLE_MS
    } else {
        DEFAULT_SETTLE_MS
    }
}

/// Whether this application published readable text controls the last time it was looked at.
///
/// Only the Windows capture walks text controls, so the measurements are kept there.
///
/// @param app_id - identifier of the application
/// @returns `Some(false)` when the walk found nothing, `None` when it has not been tried
#[cfg(target_os = "windows")]
pub fn has_text_nodes(app_id: &str) -> Option<bool> {
    with_profile(app_id, |profile| profile.text_nodes)
}

/// Record whether the text-control walk found anything in this application.
///
/// @param app_id - identifier of the application
/// @param found - whether the walk returned any control name
#[cfg(target_os = "windows")]
pub fn record_text_nodes(app_id: &str, found: bool) {
    update_profile(app_id, |profile| profile.text_nodes = Some(found));
}

/// Whether this application's capture overran the inline budget last time.
///
/// @param app_id - identifier of the application
/// @returns whether the selection should not wait for the capture at all
pub fn capture_is_slow(app_id: &str) -> bool {
    with_profile(app_id, |profile| profile.slow_capture)
}

/// Record whether the capture finished inside the inline budget.
///
/// @param app_id - identifier of the application
/// @param slow - whether the capture overran the budget
pub fn record_capture_speed(app_id: &str, slow: bool) {
    update_profile(app_id, |profile| profile.slow_capture = slow);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Profiles are process wide, so tests that touch them must not interleave.
    static STATE: Mutex<()> = Mutex::new(());

    fn lock_state() -> std::sync::MutexGuard<'static, ()> {
        STATE.lock().unwrap_or_else(|error| error.into_inner())
    }

    /// Start a test from an empty table, so assertions do not depend on test order.
    fn reset() -> std::sync::MutexGuard<'static, ()> {
        let guard = lock_state();
        PROFILES
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clear();
        guard
    }

    #[test]
    fn an_unseen_application_has_no_measurements() {
        let _state = reset();
        assert!(!capture_is_slow("unknown-reader"));
        #[cfg(target_os = "windows")]
        assert_eq!(has_text_nodes("unknown-reader"), None);
    }

    #[test]
    fn a_browser_gets_the_short_settle() {
        let _state = reset();
        // identifiers are paths on Windows and bundle ids on macOS; both carry the browser name
        for app_id in [
            r"C:\Program Files\Google\Chrome\Application\chrome.exe",
            r"C:\Program Files\Mozilla Firefox\firefox.exe",
            "com.google.Chrome",
        ] {
            assert_eq!(settle_ms(app_id), BROWSER_SETTLE_MS, "{app_id}");
        }
    }

    #[test]
    fn a_reader_keeps_the_full_settle() {
        let _state = reset();
        // Qt4 readers publish their formats after the sequence number moves
        for app_id in [
            r"D:\Program Files (x86)\GoldenDict\GoldenDict.exe",
            r"C:\Program Files\SumatraPDF\SumatraPDF.exe",
            "",
        ] {
            assert_eq!(settle_ms(app_id), DEFAULT_SETTLE_MS, "{app_id}");
        }
    }

    #[test]
    #[cfg(target_os = "windows")]
    fn text_node_support_is_remembered_per_application() {
        let _state = reset();
        record_text_nodes("reader-without-text", false);
        record_text_nodes("reader-with-text", true);

        assert_eq!(has_text_nodes("reader-without-text"), Some(false));
        assert_eq!(has_text_nodes("reader-with-text"), Some(true));
        assert_eq!(has_text_nodes("never-looked"), None);
    }

    #[test]
    fn capture_speed_is_remembered_per_application() {
        let _state = reset();
        record_capture_speed("slow-reader", true);
        record_capture_speed("quick-reader", false);

        assert!(capture_is_slow("slow-reader"));
        assert!(!capture_is_slow("quick-reader"));
    }

    #[test]
    fn an_empty_identifier_records_nothing() {
        let _state = reset();
        record_capture_speed("", true);

        assert!(!capture_is_slow(""));
        #[cfg(target_os = "windows")]
        {
            record_text_nodes("", false);
            assert_eq!(has_text_nodes(""), None);
        }
    }

    #[test]
    fn the_profile_table_stays_bounded() {
        let _state = reset();
        for index in 0..(MAX_PROFILES * 2) {
            record_capture_speed(&format!("application-{index}"), true);
        }
        let profiles = PROFILES.lock().unwrap_or_else(|error| error.into_inner());
        assert!(
            profiles.len() <= MAX_PROFILES,
            "table grew to {}",
            profiles.len()
        );
    }
}
