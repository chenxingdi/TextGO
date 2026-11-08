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
use crate::{
    CONTEXT_CLIPBOARD, CONTEXT_CLIPBOARD_AGE_MAX, CONTEXT_CLIPBOARD_AGE_MIN,
    CONTEXT_CLIPBOARD_MAX_AGE_SECS, CONTEXT_LIMIT, FORCE_GET_SELECTION, SELECTION_CONTEXT_CACHE,
    SELECTION_TEXT_CACHE,
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

// grace period between the sequence number change and the first read, so the source application
// can finish publishing the remaining clipboard formats
#[cfg(target_os = "windows")]
const CLIPBOARD_SETTLE_TIME: Duration = Duration::from_millis(50);

// how many times a published but still unreadable clipboard is retried
#[cfg(target_os = "windows")]
const CLIPBOARD_READ_ATTEMPTS: usize = 5;

// time the second copy combination may take: the first one already proved the application ignores
// it, so this attempt only has to cover a copy that works
#[cfg(not(target_os = "macos"))]
const SECOND_ATTEMPT_WAIT: Duration = Duration::from_millis(400);

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

            return Ok(with_context(text, None, selection_at, clipboard_allowed));
        }
    }

    // if force get selection is disabled, skip clipboard fallback
    if !FORCE_GET_SELECTION.load(Ordering::Relaxed) {
        return Err("Native API failed and clipboard fallback is disabled".into());
    }

    // if native API fails, fall back to clipboard method
    warn!("Failed to get selection natively, fallback to clipboard method");
    let (text, previous_text) = get_selection_fallback(app, mouse.unwrap_or(false)).await?;
    Ok(with_context(text, previous_text, selection_at, clipboard_allowed))
}

/// Capture the context of the current selection before returning the text to the frontend.
///
/// The source application still owns focus here, which the window title and surrounding text both
/// depend on; the popup only opens after this command resolves. The selected text is passed along
/// because the fallbacks locate it inside the application's own text when no native selection
/// exists.
///
/// @param text - selected text that was just obtained
/// @param previous_text - text the clipboard held before the selection copy, if any
/// @param selection_at - moment the user made the selection
/// @param clipboard_allowed - whether the clipboard channel is enabled by the settings
/// @returns the same text, unchanged
fn with_context(
    text: String,
    previous_text: Option<String>,
    selection_at: Instant,
    clipboard_allowed: bool,
) -> String {
    capture_context(&text, previous_text, selection_at, clipboard_allowed);
    text
}

/// Capture the surrounding context of the focused selection while the source application still
/// owns focus, and remember it for the popup that is about to open.
///
/// The capture is best-effort: failures only leave the context empty and never block the selection.
///
/// @param selection - selected text, used by the fallbacks that locate it in the application
/// @param previous_text - text the clipboard held before the selection copy, if any
/// @param selection_at - moment the user made the selection
/// @param clipboard_allowed - whether the clipboard channel is enabled by the settings
fn capture_context(
    selection: &str,
    previous_text: Option<String>,
    selection_at: Instant,
    clipboard_allowed: bool,
) {
    let limit = CONTEXT_LIMIT.load(Ordering::Relaxed);
    if limit == 0 {
        return;
    }

    let mut context = match platform::get_selection_context(limit, selection) {
        Ok(context) => context,
        Err(error) => {
            debug!("Failed to capture selection context: {}", error);
            return;
        }
    };

    // surrounding text from the accessibility tree always wins; the clipboard is only a fallback
    if context.paragraph.trim().is_empty() && clipboard_allowed {
        let max_age = Duration::from_secs(CONTEXT_CLIPBOARD_MAX_AGE_SECS.load(Ordering::Relaxed));
        match clipboard_watch::fresh_change(selection_at, max_age) {
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
                        context.paragraph = paragraph;
                    } else {
                        debug!(
                            "Clipboard copy #{} does not carry the selection, not used as context",
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
                "Clipboard change is older than {}s, not used as context",
                max_age.as_secs()
            ),
        }
    }

    debug!("Captured selection context from {:?}", context.app_id);
    if let Ok(mut cache) = SELECTION_CONTEXT_CACHE.lock() {
        *cache = Some(context);
    }
}

/// Get the context captured together with the current selection.
///
/// @returns captured context, or empty fields when nothing was captured
#[tauri::command]
pub fn get_selection_context() -> Result<SelectionContext, AppError> {
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
/// @returns the copied selection and the text the clipboard held before the copy
async fn get_selection_fallback(
    app: AppHandle,
    mouse: bool,
) -> Result<(String, Option<String>), AppError> {
    if let Ok(mut cache) = SELECTION_TEXT_CACHE.lock() {
        *cache = None;
    }

    // use backup-operation-restore mode
    with_clipboard_backup(|| async move {
        let max_wait_time = Duration::from_millis(MAX_WAIT_TIME.load(Ordering::Relaxed));
        let configured = copy_selection(&app, mouse, preferred_copy_insert(), max_wait_time).await;

        #[cfg(target_os = "macos")]
        let selected_text = configured;

        #[cfg(not(target_os = "macos"))]
        let selected_text = if configured.is_empty() {
            debug!("Preferred copy shortcut produced no text, retrying with the other one");
            let other = !preferred_copy_insert();
            let text = copy_selection(&app, mouse, other, SECOND_ATTEMPT_WAIT).await;
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
/// @returns copied text, or an empty string when the combination copied nothing
async fn copy_selection(app: &AppHandle, mouse: bool, insert: bool, timeout: Duration) -> String {
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

        // the sequence number moves before the owner finishes; give it the rest of the write
        tokio::time::sleep(CLIPBOARD_SETTLE_TIME).await;

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
