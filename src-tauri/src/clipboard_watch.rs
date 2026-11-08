//! Clipboard change monitor.
//!
//! The monitor answers one question: when did the clipboard last change? It never reads what the
//! clipboard contains, because opening the clipboard while another application is publishing a copy
//! can crash Qt4 based readers such as GoldenDict. Only the platform change counter is polled, which
//! is a plain integer read that does not touch the clipboard itself.
//!
//! The content is read later, when a selection is made, and paired with the moment recorded here.
//! That keeps "this text was copied a few seconds ago" decidable without inspecting every copy the
//! user makes.

use crate::platform;
use log::debug;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};

/// How often the platform change counter is polled.
const POLL_INTERVAL: Duration = Duration::from_millis(250);

/// Sequence number produced by our own last clipboard write.
///
/// Our copy and restore steps advance the counter too; treating them as user copies would refresh
/// the freshness window forever, so the monitor skips them.
static OWN_SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// Whether one of our own clipboard writes is in flight.
static WRITING: AtomicBool = AtomicBool::new(false);

/// Number of selection captures in flight.
///
/// A capture copies the selection and restores the backup, which advances the counter twice through
/// other applications and ourselves. None of those writes is a copy the user made, so observation is
/// suspended for the whole capture.
static SUPPRESS: AtomicUsize = AtomicUsize::new(0);

/// Most recent foreign clipboard change.
static LAST_CHANGE: LazyLock<Mutex<Option<Change>>> = LazyLock::new(|| Mutex::new(None));

/// A clipboard change seen by the monitor.
#[derive(Clone, Copy, Debug)]
pub struct Change {
    /// Change counter value the clipboard held after the write.
    pub sequence: u64,
    /// When the monitor first observed that value.
    pub observed_at: Instant,
}

/// Start the monitor thread; it runs for the lifetime of the process.
pub fn start() {
    std::thread::spawn(|| loop {
        observe();
        std::thread::sleep(POLL_INTERVAL);
    });
}

/// Read the counter once and remember a change that we did not cause ourselves.
///
/// Called by the monitor thread on its own schedule, and once at the start of a selection capture so
/// that a copy made less than one poll interval earlier is still noticed.
pub fn observe_now() {
    observe();
}

/// Read the counter once and remember a change that we did not cause ourselves.
fn observe() {
    let Some(sequence) = platform::clipboard_change_count() else {
        return;
    };

    if !should_observe(sequence) {
        return;
    }

    let Ok(mut last) = LAST_CHANGE.lock() else {
        return;
    };
    if last.as_ref().map(|change| change.sequence) != Some(sequence) {
        debug!("Clipboard changed: sequence {sequence}");
        *last = Some(Change {
            sequence,
            observed_at: Instant::now(),
        });
    }
}

/// Whether a counter value belongs to a foreign write worth remembering.
///
/// Our own writes are recognised by their sequence number; the in-flight flag also covers the window
/// between the write and the counter read that records it.
///
/// @param sequence - counter value read from the platform
/// @returns whether the change came from another application
fn should_observe(sequence: u64) -> bool {
    SUPPRESS.load(Ordering::Relaxed) == 0
        && !WRITING.load(Ordering::Relaxed)
        && sequence != OWN_SEQUENCE.load(Ordering::Relaxed)
}

/// Guard that keeps the monitor from observing while a selection capture runs.
pub struct ObservationGuard;

/// Suspend observation until the returned guard is dropped.
pub fn suspend_observation() -> ObservationGuard {
    SUPPRESS.fetch_add(1, Ordering::Relaxed);
    ObservationGuard
}

impl Drop for ObservationGuard {
    fn drop(&mut self) {
        SUPPRESS.fetch_sub(1, Ordering::Relaxed);
    }
}

/// Most recent change that is still inside the freshness window.
///
/// @param now - moment the selection was made
/// @param max_age - how long a copy stays usable as context
/// @returns the change, or none when the clipboard is older than the window or was never seen
pub fn fresh_change(now: Instant, max_age: Duration) -> Option<Change> {
    let last = (*LAST_CHANGE.lock().ok()?)?;
    let age = now
        .checked_duration_since(last.observed_at)
        .unwrap_or_default();
    (age <= max_age).then_some(last)
}

/// Record the clipboard as if the user had copied it themselves.
///
/// The copy action of this application writes the text through our own clipboard handle, so the
/// monitor skips that change as one of our own writes. The text is still what the user just put on
/// the clipboard on purpose, which is exactly what the surrounding-text channel looks for, so the
/// change is dated here explicitly. Without this a copy made with our own toolbar would never be
/// usable as context.
pub fn note_user_copy() {
    let Some(sequence) = platform::clipboard_change_count() else {
        return;
    };

    if let Ok(mut last) = LAST_CHANGE.lock() {
        *last = Some(Change {
            sequence,
            observed_at: Instant::now(),
        });
    }
}

/// Whether the clipboard still holds the text of a recorded copy.
///
/// The recorded moment belongs to one specific write. Anything that replaces the content
/// afterwards without putting it back - another application's copy inside a suspended observation
/// window, or an interrupted restore that leaves our own text behind - leaves text on the clipboard
/// that the moment no longer describes, and matching a selection against it would date unrelated
/// text as surrounding context.
///
/// @param change - recorded copy
/// @returns whether the clipboard content is unchanged since that copy
pub fn holds_copy(change: Change) -> bool {
    platform::clipboard_change_count() == Some(change.sequence)
}

/// Keep the recorded copy usable after the clipboard content was put back.
///
/// Restoring a backup writes the same text again, which advances the counter without changing what
/// the clipboard holds, so the recorded moment stays valid and only the sequence that identifies
/// the content moves.
pub fn keep_copy_current() {
    let Some(sequence) = platform::clipboard_change_count() else {
        return;
    };

    if let Ok(mut last) = LAST_CHANGE.lock() {
        if let Some(change) = last.as_mut() {
            change.sequence = sequence;
        }
    }
}

/// Forget the recorded copy because the clipboard now holds text nobody copied on purpose.
///
/// Output the application produced itself (an answer, a translation, a script result) replaces what
/// the user had copied, so keeping the old moment would date text that is no longer on the
/// clipboard.
pub fn forget_copy() {
    clear();
}

/// Forget the last change, used when the feature is turned off.
pub fn clear() {
    if let Ok(mut last) = LAST_CHANGE.lock() {
        *last = None;
    }
}

/// Guard that marks a clipboard write of ours for the duration of its scope.
///
/// The guard sets the writing flag before the caller touches the clipboard, and records the counter
/// value the write produced when it is dropped.
pub struct OwnWrite;

/// Mark the beginning of a clipboard write performed by this application.
pub fn begin_own_write() -> OwnWrite {
    WRITING.store(true, Ordering::Relaxed);
    OwnWrite
}

impl Drop for OwnWrite {
    fn drop(&mut self) {
        if let Some(sequence) = platform::clipboard_change_count() {
            OWN_SEQUENCE.store(sequence, Ordering::Relaxed);
        }
        WRITING.store(false, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The monitor state is process wide, so tests that read or write it must not interleave: a
    /// `clear` in one test would otherwise erase the copy another test just recorded.
    static STATE: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// Serialize a test that touches the monitor state.
    fn lock_state() -> std::sync::MutexGuard<'static, ()> {
        STATE.lock().unwrap_or_else(|error| error.into_inner())
    }

    fn record_at(now: Instant, sequence: u64) {
        let mut last = LAST_CHANGE.lock().expect("lock");
        *last = Some(Change {
            sequence,
            observed_at: now,
        });
    }

    #[test]
    fn fresh_change_honours_the_window() {
        let _state = lock_state();
        let copied_at = Instant::now();
        record_at(copied_at, 7);

        let within = fresh_change(copied_at + Duration::from_secs(29), Duration::from_secs(30));
        assert_eq!(within.map(|change| change.sequence), Some(7));

        let outside = fresh_change(copied_at + Duration::from_secs(31), Duration::from_secs(30));
        assert!(outside.is_none());
        clear();
    }

    #[test]
    fn fresh_change_is_none_without_an_observed_copy() {
        let _state = lock_state();
        clear();
        assert!(fresh_change(Instant::now(), Duration::from_secs(30)).is_none());
    }

    #[test]
    fn clear_forgets_the_observed_copy() {
        let _state = lock_state();
        record_at(Instant::now(), 3);
        clear();
        assert!(fresh_change(Instant::now(), Duration::from_secs(30)).is_none());
    }

    #[test]
    fn our_own_writes_are_not_observed() {
        let _state = lock_state();
        OWN_SEQUENCE.store(42, Ordering::Relaxed);
        WRITING.store(false, Ordering::Relaxed);
        assert!(!should_observe(42), "our own counter value must be skipped");
        assert!(should_observe(43), "a foreign counter value must be observed");

        // while a write of ours is in flight, even a foreign value cannot be trusted yet
        let guard = begin_own_write();
        assert!(!should_observe(43));
        drop(guard);
        OWN_SEQUENCE.store(0, Ordering::Relaxed);
        clear();
    }

    #[test]
    fn observation_is_suspended_during_a_capture() {
        let _state = lock_state();
        OWN_SEQUENCE.store(0, Ordering::Relaxed);
        WRITING.store(false, Ordering::Relaxed);
        let guard = suspend_observation();
        assert!(!should_observe(99), "a capture advances the counter itself");
        drop(guard);
        assert!(should_observe(99), "observation resumes after the capture");
    }

    #[test]
    fn a_copy_we_performed_on_request_is_dated_immediately() {
        let _state = lock_state();
        clear();
        note_user_copy();
        // platforms without a change counter cannot date the copy; every other platform must
        if crate::platform::clipboard_change_count().is_some() {
            assert!(fresh_change(Instant::now(), Duration::from_secs(1)).is_some());
        }
        clear();
    }

    #[test]
    fn a_recorded_copy_only_holds_while_the_content_is_unchanged() {
        let _state = lock_state();
        clear();
        note_user_copy();
        let Some(change) = fresh_change(Instant::now(), Duration::from_secs(1)) else {
            // platforms without a change counter cannot decide identity at all
            return;
        };
        assert!(holds_copy(change), "the recorded sequence must still be current");

        // restoring the same text advances the counter, and the recorded copy follows it
        keep_copy_current();
        let Some(restored) = fresh_change(Instant::now(), Duration::from_secs(1)) else {
            return;
        };
        assert!(holds_copy(restored), "a restore of the same text keeps the copy");

        // text written after the recorded copy belongs to a different sequence
        let stale = Change {
            sequence: restored.sequence.wrapping_sub(1),
            observed_at: restored.observed_at,
        };
        assert!(!holds_copy(stale), "a superseded sequence must not match");
        clear();
    }

    #[test]
    fn keeping_a_copy_current_without_one_records_nothing() {
        let _state = lock_state();
        clear();
        keep_copy_current();
        assert!(fresh_change(Instant::now(), Duration::from_secs(1)).is_none());
        clear();
    }

    #[test]
    fn a_newer_change_replaces_the_previous_one() {
        let _state = lock_state();
        let first = Instant::now();
        record_at(first, 1);
        record_at(first + Duration::from_secs(1), 2);
        let last = fresh_change(first + Duration::from_secs(2), Duration::from_secs(30)).expect("copy");
        assert_eq!(last.sequence, 2);
        clear();
    }
}
