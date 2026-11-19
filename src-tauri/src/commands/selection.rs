use crate::clipboard_context;
use crate::clipboard_watch;
#[cfg(not(target_os = "windows"))]
use crate::commands::clipboard::clear_clipboard;
#[cfg(target_os = "windows")]
use crate::commands::clipboard::{clipboard_sequence, CLIPBOARD_CONSUMED_SEQUENCE};
use crate::commands::clipboard::{get_clipboard_text, with_clipboard_backup};
use crate::commands::keyboard::{configured_copy_insert, send_copy_keys_with};
use crate::commands::shortcut::ShortcutHandlerGuard;
use crate::error::AppError;
use crate::platform::{self, SelectionContext};
use crate::selection_profile;
use crate::{
    CONTEXT_CLIPBOARD, CONTEXT_CLIPBOARD_AGE_MAX, CONTEXT_CLIPBOARD_AGE_MIN,
    CONTEXT_CLIPBOARD_MAX_AGE_SECS, CONTEXT_FINISHED, CONTEXT_GENERATION, CONTEXT_LIMIT,
    FORCE_GET_SELECTION, SELECTION_CONTEXT_CACHE, SELECTION_TEXT_CACHE,
};
use log::{debug, warn};
use std::sync::atomic::{AtomicU64, AtomicU8, Ordering};
use std::time::{Duration, Instant};
use tauri::AppHandle;

// maximum wait time in milliseconds for clipboard to update
static MAX_WAIT_TIME: AtomicU64 = AtomicU64::new(1000);

// how often the Windows path checks the clipboard sequence number
#[cfg(target_os = "windows")]
const CLIPBOARD_CHECK_INTERVAL: Duration = Duration::from_millis(10);

// how many times a published but still unreadable clipboard is retried
#[cfg(target_os = "windows")]
const CLIPBOARD_READ_ATTEMPTS: usize = 5;

// time the second copy combination may take: the first one already proved the application ignores
// it, so this attempt only has to cover a copy that works
#[cfg(not(target_os = "macos"))]
const SECOND_ATTEMPT_WAIT: Duration = Duration::from_millis(400);

// how long a selection waits for its context before the popup opens without it; only covers the
// moment the capture thread needs to record the foreground window and reach the accessibility tree
const CONTEXT_INLINE_BUDGET: Duration = Duration::from_millis(50);

// how long reading the context waits for a capture that is still running
const CONTEXT_READ_BUDGET: Duration = Duration::from_millis(1500);

// poll interval while waiting for a capture to finish
const CONTEXT_WAIT_INTERVAL: Duration = Duration::from_millis(5);

// copy combination that produced text last, so it is tried first on the next selection
const COPY_COMBINATION_UNKNOWN: u8 = u8::MAX;
static COPY_COMBINATION: AtomicU8 = AtomicU8::new(COPY_COMBINATION_UNKNOWN);

/// Get selected text.
///
/// @param app - application handle
/// @param mouse - whether the selection came from a mouse gesture
/// @returns selected text
#[tauri::command]
pub async fn get_selection(app: AppHandle, mouse: Option<bool>) -> Result<String, AppError> {
    // catch a copy the monitor has not polled yet; the monitor runs every 250 ms, and looking once
    // here keeps a copy made moments before the selection from being missed
    clipboard_watch::observe_now();

    // the moment the user selected, used to decide whether a copy is still fresh enough
    let selection_at = Instant::now();

    // everything this capture writes (the selection copy and the restore) advances the counter and
    // must not be mistaken for a copy the user made
    let _observation = clipboard_watch::suspend_observation();

    // the channel is available to every trigger; the freshness window is what keeps it meaningful
    let clipboard_allowed =
        CONTEXT_CLIPBOARD.load(Ordering::Relaxed) && CONTEXT_LIMIT.load(Ordering::Relaxed) > 0;

    // suspend shortcut handling to avoid interference
    let _guard = ShortcutHandlerGuard::suspend();

    // drop the context of the previous selection before looking up the new one
    if let Ok(mut cache) = SELECTION_CONTEXT_CACHE.lock() {
        *cache = None;
    }

    // try using platform native API to get selected text first
    if let Ok(text) = platform::get_selection() {
        if !text.is_empty() {
            // clear cache to avoid stale data
            if let Ok(mut cache) = SELECTION_TEXT_CACHE.lock() {
                *cache = None;
            }

            // the native path needs the identifier too: it decides whether the surrounding-text
            // capture is worth waiting for
            let app_id = platform::get_frontmost_app_id().unwrap_or_default();
            return Ok(with_context(text, None, selection_at, clipboard_allowed, &app_id).await);
        }
    }

    // if force get selection is disabled, skip clipboard fallback
    if !FORCE_GET_SELECTION.load(Ordering::Relaxed) {
        return Err("Native API failed and clipboard fallback is disabled".into());
    }

    // if native API fails, fall back to clipboard method
    warn!("Failed to get selection natively, fallback to clipboard method");

    // the application identifier decides how long its copies have to be waited for and which
    // surrounding-text channels are worth trying; the lookup costs well under a millisecond
    let app_id = platform::get_frontmost_app_id().unwrap_or_default();
    let (text, previous_text) = get_selection_fallback(app, mouse.unwrap_or(false), &app_id).await?;
    Ok(with_context(text, previous_text, selection_at, clipboard_allowed, &app_id).await)
}

/// Start capturing the context of the current selection and wait a short budget for it.
///
/// The capture runs on its own thread: the surrounding text is read through the accessibility
/// tree, and a single call there can block for hundreds of milliseconds or longer. The toolbar must
/// not wait for that, so the selected text is returned as soon as the budget passes; the popup
/// reads the result through `get_selection_context`, which waits for the capture to finish. The
/// source application still owns focus when the thread starts, which the window title and the
/// focused element both depend on.
///
/// @param text - selected text that was just obtained
/// @param previous_text - text the clipboard held before the selection copy, if any
/// @param selection_at - moment the user made the selection
/// @param clipboard_allowed - whether the clipboard channel is enabled by the settings
/// @param app_id - identifier of the application the selection came from
/// @returns the same text, unchanged
async fn with_context(
    text: String,
    previous_text: Option<String>,
    selection_at: Instant,
    clipboard_allowed: bool,
    app_id: &str,
) -> String {
    let generation =
        start_context_capture(&text, previous_text, selection_at, clipboard_allowed, app_id);
    wait_for_context(generation, inline_budget(app_id)).await;
    text
}

/// How long this selection may wait for its context before the toolbar opens without it.
///
/// The wait only helps an application whose capture finishes almost immediately. A Qt4 reader needs
/// hundreds of milliseconds for the same capture, so waiting for it on every selection would add
/// the whole budget to every toolbar for a result that never arrives in time. Once a capture has
/// overrun the budget for an application, its next selection does not wait at all: the popup still
/// reads the finished capture through `get_selection_context`.
///
/// @param app_id - identifier of the application the selection came from
/// @returns the longest time to wait for the capture
fn inline_budget(app_id: &str) -> Duration {
    if selection_profile::capture_is_slow(app_id) {
        Duration::ZERO
    } else {
        CONTEXT_INLINE_BUDGET
    }
}

/// Start the background capture of the surrounding context.
///
/// Older captures are superseded: their result is dropped instead of overwriting the cache, and the
/// generation advances so the popup only waits for the newest one.
///
/// @param selection - selected text, used by the fallbacks that locate it in the application
/// @param previous_text - text the clipboard held before the selection copy, if any
/// @param selection_at - moment the user made the selection
/// @param clipboard_allowed - whether the clipboard channel is enabled by the settings
/// @param app_id - identifier of the application, used to remember how slow its capture is
/// @returns generation of the started capture
fn start_context_capture(
    selection: &str,
    previous_text: Option<String>,
    selection_at: Instant,
    clipboard_allowed: bool,
    app_id: &str,
) -> u64 {
    let generation = CONTEXT_GENERATION
        .fetch_add(1, Ordering::AcqRel)
        .wrapping_add(1);
    if let Ok(mut cache) = SELECTION_CONTEXT_CACHE.lock() {
        *cache = None;
    }

    // context capture disabled: the cache stays empty and no thread is started
    if CONTEXT_LIMIT.load(Ordering::Relaxed) == 0 {
        CONTEXT_FINISHED.fetch_max(generation, Ordering::AcqRel);
        return generation;
    }

    let selection = selection.to_string();
    let app_id = app_id.to_string();
    std::thread::spawn(move || {
        // records the finish even when the capture panics, so the popup stops waiting
        let _finished = ContextCompletion(generation);
        let started_at = Instant::now();
        let context = capture_context(&selection, previous_text, selection_at, clipboard_allowed);
        // remember whether this application can answer inside the inline budget, so its next
        // selection does not wait for a capture that is known to overrun it
        selection_profile::record_capture_speed(
            &app_id,
            started_at.elapsed() > CONTEXT_INLINE_BUDGET,
        );
        // a newer selection, or turning the channel off, has already cleared the cache; its owner
        // writes the entry now
        if CONTEXT_GENERATION.load(Ordering::Acquire) == generation
            && CONTEXT_LIMIT.load(Ordering::Relaxed) > 0
        {
            if let Ok(mut cache) = SELECTION_CONTEXT_CACHE.lock() {
                *cache = Some(context);
            }
        }
    });

    generation
}

/// Mark a capture as finished, including when it panics.
struct ContextCompletion(u64);

impl Drop for ContextCompletion {
    fn drop(&mut self) {
        CONTEXT_FINISHED.fetch_max(self.0, Ordering::AcqRel);
    }
}

/// Wait for a capture to finish, up to the given budget.
///
/// @param generation - capture generation to wait for
/// @param budget - longest time to wait
async fn wait_for_context(generation: u64, budget: Duration) {
    let deadline = Instant::now() + budget;
    while CONTEXT_FINISHED.load(Ordering::Acquire) < generation {
        if Instant::now() >= deadline {
            return;
        }
        tokio::time::sleep(CONTEXT_WAIT_INTERVAL).await;
    }
}

/// Capture the surrounding context of the focused selection while the source application still
/// owns focus, and return it for the popup that is about to open.
///
/// The capture is best-effort: failures only leave the context empty and never block the selection.
///
/// @param selection - selected text, used by the fallbacks that locate it in the application
/// @param previous_text - text the clipboard held before the selection copy, if any
/// @param selection_at - moment the user made the selection
/// @param clipboard_allowed - whether the clipboard channel is enabled by the settings
/// @returns captured context, with empty fields when nothing could be read
fn capture_context(
    selection: &str,
    previous_text: Option<String>,
    selection_at: Instant,
    clipboard_allowed: bool,
) -> SelectionContext {
    let limit = CONTEXT_LIMIT.load(Ordering::Relaxed);
    if limit == 0 {
        return SelectionContext::default();
    }

    let mut context = match platform::get_selection_context(limit, selection) {
        Ok(context) => context,
        Err(error) => {
            debug!("Failed to capture selection context: {}", error);
            return SelectionContext::default();
        }
    };

    // A fresh copy that contains the selection is the most precise surrounding text available:
    // it is the exact text the user had on the clipboard, so it wins even when the accessibility
    // tree already returned a paragraph. On UIA-capable applications such as Chrome this lets the
    // clipboard channel take part instead of being skipped entirely. When there is no usable copy
    // the paragraph from the accessibility tree is kept unchanged.
    if clipboard_allowed {
        let max_age = Duration::from_secs(CONTEXT_CLIPBOARD_MAX_AGE_SECS.load(Ordering::Relaxed));
        match clipboard_watch::fresh_change(selection_at, max_age) {
            // the recorded moment belongs to the text it copied; anything written afterwards leaves
            // content on the clipboard that the moment does not describe
            Some(change) if !clipboard_watch::holds_copy(change) => debug!(
                "Clipboard changed after copy #{}, not used as context",
                change.sequence
            ),
            Some(change) => {
                // prefer the text this selection already read back as a backup; reading again is
                // only needed when the selection came from the native path
                let copied = previous_text
                    .or_else(|| get_clipboard_text().ok())
                    .filter(|copied| !copied.trim().is_empty());

                if let Some(copied) = copied {
                    if let Some(paragraph) = clipboard_context::paragraph_in(&copied, selection, limit)
                    {
                        debug!(
                            "Context recovered from clipboard change #{}",
                            change.sequence
                        );
                        context = prefer_clipboard_paragraph(context, paragraph);
                    } else {
                        debug!(
                            "Clipboard copy #{} cannot locate the selection, not used as context",
                            change.sequence
                        );
                    }
                } else {
                    debug!(
                        "Clipboard copy #{} carries no text, not used as context",
                        change.sequence
                    );
                }
            }
            None => debug!(
                "No clipboard copy within {}s, not used as context",
                max_age.as_secs()
            ),
        }
    }

    debug!("Captured selection context from {:?}", context.app_id);
    context
}

/// Put a recovered clipboard paragraph into a captured context.
///
/// The copy is the exact text the user had on the clipboard and contains the selection, so it is
/// preferred over any paragraph the accessibility tree returned. Keeping this as a small function
/// makes the precedence testable without opening the clipboard or the accessibility tree.
///
/// @param context - context already captured from the platform
/// @param paragraph - surrounding text recovered from the clipboard copy
/// @returns the context with the clipboard paragraph installed
fn prefer_clipboard_paragraph(mut context: SelectionContext, paragraph: String) -> SelectionContext {
    context.paragraph = paragraph;
    context
}

#[cfg(test)]
mod context_tests {
    use super::*;

    #[test]
    fn a_clipboard_paragraph_overrides_the_accessibility_paragraph() {
        let context = SelectionContext {
            app_id: "chrome.exe".into(),
            title: "An article - Chrome".into(),
            url: "https://example.com/article".into(),
            paragraph: "UIA paragraph around the selection".into(),
        };

        let merged = prefer_clipboard_paragraph(
            context,
            "The exact copied sentence around the selection".into(),
        );

        assert_eq!(
            merged.paragraph,
            "The exact copied sentence around the selection"
        );
        // only the surrounding text changes; the source application identity is still UIA's
        assert_eq!(merged.app_id, "chrome.exe");
        assert_eq!(merged.url, "https://example.com/article");
    }

    #[test]
    fn a_usable_copy_is_found_even_when_the_platform_paragraph_exists() {
        let copied = "A longer copied sentence that contains the selected words along with context";
        let selection = "selected words";
        let paragraph = clipboard_context::paragraph_in(copied, selection, 60);

        assert!(
            paragraph.as_deref().is_some_and(|text| text.contains(selection)),
            "a copied sentence must still be usable as context when the platform has a paragraph"
        );
    }
}

/// Get the context captured together with the current selection.
///
/// The capture starts as soon as the selection is made and may still be running; this waits for it
/// within a bounded time, so a slow accessibility provider cannot hold the request forever.
///
/// @returns captured context, or empty fields when nothing was captured
#[tauri::command]
pub async fn get_selection_context() -> Result<SelectionContext, AppError> {
    let generation = CONTEXT_GENERATION.load(Ordering::Acquire);
    wait_for_context(generation, CONTEXT_READ_BUDGET).await;
    Ok(SELECTION_CONTEXT_CACHE
        .lock()
        .ok()
        .and_then(|cache| cache.as_ref().cloned())
        .unwrap_or_default())
}

/// Set the maximum number of characters captured from around the selection.
///
/// @param limit - character limit; zero disables context capture
#[tauri::command]
pub fn set_context_limit(limit: usize) -> Result<(), AppError> {
    let limit = limit.clamp(0, platform::CONTEXT_TEXT_LIMIT_MAX);
    CONTEXT_LIMIT.store(limit, Ordering::Relaxed);
    Ok(())
}

/// Enable or disable using clipboard copies as surrounding text.
///
/// @param enabled - whether copies made just before a selection may provide context
#[tauri::command]
pub fn set_context_clipboard(enabled: bool) -> Result<(), AppError> {
    CONTEXT_CLIPBOARD.store(enabled, Ordering::Relaxed);
    if !enabled {
        // turning the channel off must not leave an observed copy behind
        clipboard_watch::clear();
    }
    Ok(())
}

/// Set how long a clipboard copy stays usable as surrounding text.
///
/// @param seconds - validity window in seconds, clamped to the supported range
#[tauri::command]
pub fn set_context_clipboard_max_age(seconds: u64) -> Result<(), AppError> {
    let seconds = seconds.clamp(CONTEXT_CLIPBOARD_AGE_MIN, CONTEXT_CLIPBOARD_AGE_MAX);
    CONTEXT_CLIPBOARD_MAX_AGE_SECS.store(seconds, Ordering::Relaxed);
    Ok(())
}

/// Whether the next copy attempt should send Ctrl+Insert.
///
/// The combination that produced text last time goes first: applications implement one of the two,
/// and starting with the one the source application ignores costs a full timeout.
///
/// @returns whether Ctrl+Insert is tried first
fn preferred_copy_insert() -> bool {
    match COPY_COMBINATION.load(Ordering::Relaxed) {
        COPY_COMBINATION_UNKNOWN => configured_copy_insert(),
        known => known == 1,
    }
}

/// Get selected text through clipboard.
///
/// The two copy combinations are tried one after the other: applications implement at most one of
/// them, and a reader that ignores Ctrl+C still copies with Ctrl+Insert. The combination that worked
/// last time goes first, so the common case keeps its single attempt.
///
/// @param app - application handle
/// @param mouse - whether the selection came from a mouse gesture
/// @param app_id - identifier of the application the selection came from, used to pick the waits
/// @returns the copied selection and the text the clipboard held before the copy
async fn get_selection_fallback(
    app: AppHandle,
    mouse: bool,
    app_id: &str,
) -> Result<(String, Option<String>), AppError> {
    if let Ok(mut cache) = SELECTION_TEXT_CACHE.lock() {
        *cache = None;
    }

    let app_id = app_id.to_string();

    // use backup-operation-restore mode
    with_clipboard_backup(|| async move {
        let max_wait_time = Duration::from_millis(MAX_WAIT_TIME.load(Ordering::Relaxed));
        let configured =
            copy_selection(&app, mouse, preferred_copy_insert(), max_wait_time, &app_id).await;

        #[cfg(target_os = "macos")]
        let selected_text = configured;

        #[cfg(not(target_os = "macos"))]
        let selected_text = if configured.is_empty() {
            debug!("Preferred copy shortcut produced no text, retrying with the other one");
            let other = !preferred_copy_insert();
            let text = copy_selection(&app, mouse, other, SECOND_ATTEMPT_WAIT, &app_id).await;
            if !text.is_empty() {
                // starting with the combination the application ignores costs a full timeout on
                // every selection, so the working one is remembered
                COPY_COMBINATION.store(u8::from(other), Ordering::Relaxed);
            }
            text
        } else {
            configured
        };

        if selected_text.is_empty() {
            warn!(
                "Clipboard did not change within {} ms, possibly no text selected",
                max_wait_time.as_millis()
            );
        } else {
            // cache the selected text with current timestamp
            if let Ok(mut cache) = SELECTION_TEXT_CACHE.lock() {
                *cache = Some((selected_text.clone(), std::time::Instant::now()));
            }

            // adjust max wait time for next time
            MAX_WAIT_TIME
                .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
                    if current > 200 {
                        Some((current - 100).max(200))
                    } else {
                        Some(current)
                    }
                })
                .ok();
        }

        Ok(selected_text)
    })
    .await
}

/// Send one copy combination and wait for the clipboard to publish the selection.
///
/// @param app - application handle, used to send the keys on the main thread
/// @param mouse - whether the selection came from a mouse gesture
/// @param insert - send Ctrl+Insert instead of Ctrl+C; ignored on macOS
/// @param timeout - how long the clipboard may take to publish the copied text
/// @param app_id - identifier of the source application, used to size the settle wait on Windows
/// @returns copied text, or an empty string when the combination copied nothing
async fn copy_selection(
    app: &AppHandle,
    mouse: bool,
    insert: bool,
    timeout: Duration,
    #[cfg_attr(not(target_os = "windows"), allow(unused_variables))] app_id: &str,
) -> String {
    // Windows: leave the clipboard alone until the copy has landed. Qt4 based readers such as
    // GoldenDict crash when another process opens the clipboard while they publish a copy:
    // QClipboard hands the mime data to OLE, then keeps writing WebKit's pasteboard formats to
    // it. Clearing the clipboard or polling it by reading races with that window, so track the
    // clipboard sequence number instead (a plain Win32 counter, it never opens the clipboard).
    #[cfg(target_os = "windows")]
    let baseline = clipboard_sequence();

    #[cfg(not(target_os = "windows"))]
    let _ = clear_clipboard();

    // send copy shortcut
    // https://github.com/enigo-rs/enigo/issues/153
    let _ = app.run_on_main_thread(move || {
        // mouse-triggered selections don't need to release modifier keys
        let _ = send_copy_keys_with(insert, Some(false), Some(!mouse));
    });

    #[cfg(target_os = "windows")]
    {
        let deadline = Instant::now() + timeout;
        let mut published = false;

        while Instant::now() < deadline {
            tokio::time::sleep(CLIPBOARD_CHECK_INTERVAL).await;

            if clipboard_sequence() != baseline {
                published = true;
                break;
            }
        }

        if !published {
            return String::new();
        }

        // the sequence number moves before the owner finishes publishing, so the read waits for the
        // application to be done. A browser is readable almost immediately; a Qt4 reader is not, and
        // opening its clipboard while it is still writing crashes it, so it keeps the full wait.
        // The read below still polls, which covers a browser that needs slightly longer.
        tokio::time::sleep(Duration::from_millis(selection_profile::settle_ms(app_id))).await;

        let mut selected_text = String::new();
        for _attempt in 0..CLIPBOARD_READ_ATTEMPTS {
            match get_clipboard_text() {
                Ok(current_text) if !current_text.is_empty() => {
                    selected_text = current_text;
                    break;
                }
                _ => tokio::time::sleep(CLIPBOARD_CHECK_INTERVAL).await,
            }
        }

        // remember what we consumed, so the restore step can tell our copy apart from a newer one
        // the user made in the meantime
        if let Some(sequence) = clipboard_sequence() {
            CLIPBOARD_CONSUMED_SEQUENCE.store(sequence, Ordering::Relaxed);
        }

        selected_text
    }

    #[cfg(not(target_os = "windows"))]
    {
        let check_interval = Duration::from_millis(5); // check interval 5ms
        let max_attempts = timeout.as_millis() / check_interval.as_millis();

        for _attempt in 0..max_attempts {
            tokio::time::sleep(check_interval).await;

            // read current clipboard text
            if let Ok(current_text) = get_clipboard_text() {
                if !current_text.is_empty() {
                    // if clipboard content changed, copy operation completed
                    return current_text;
                }
            }
        }

        String::new()
    }
}
