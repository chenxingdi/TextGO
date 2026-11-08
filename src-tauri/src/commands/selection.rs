#[cfg(not(target_os = "windows"))]
use crate::commands::clipboard::clear_clipboard;
#[cfg(target_os = "windows")]
use crate::commands::clipboard::{clipboard_sequence, CLIPBOARD_CONSUMED_SEQUENCE};
use crate::commands::clipboard::{get_clipboard_text, with_clipboard_backup};
use crate::commands::keyboard::{configured_copy_insert, send_copy_keys_with};
use crate::capture::{self, CaptureSettings};
use crate::commands::shortcut::ShortcutHandlerGuard;
use crate::error::AppError;
use crate::platform::{self, SelectionContext};
use crate::{
    CONTEXT_LIMIT, FORCE_GET_SELECTION, SELECTION_CONTEXT_CACHE, SELECTION_TEXT_CACHE,
    VISION_CAPTURE,
};
use log::{debug, warn};
use std::sync::atomic::{AtomicU64, AtomicU8, Ordering};
use std::time::Duration;
#[cfg(target_os = "windows")]
use std::time::Instant;
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
#[tauri::command]
pub async fn get_selection(app: AppHandle, mouse: Option<bool>) -> Result<String, AppError> {
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

            return Ok(with_context(text));
        }
    }

    // if force get selection is disabled, skip clipboard fallback
    if !FORCE_GET_SELECTION.load(Ordering::Relaxed) {
        return Err("Native API failed and clipboard fallback is disabled".into());
    }

    // if native API fails, fall back to clipboard method
    warn!("Failed to get selection natively, fallback to clipboard method");
    let text = get_selection_fallback(app, mouse.unwrap_or(false)).await?;
    Ok(with_context(text))
}

/// Capture the context of the current selection before returning the text to the frontend.
///
/// The source application still owns focus here, which the window title and surrounding text both
/// depend on; the popup only opens after this command resolves. The selected text is passed along
/// because the fallbacks locate it inside the application's own text when no native selection
/// exists.
///
/// @param text - selected text that was just obtained
/// @returns the same text, unchanged
fn with_context(text: String) -> String {
    capture_context(&text);
    text
}

/// Capture the surrounding context of the focused selection while the source application still
/// owns focus, and remember it for the popup that is about to open.
///
/// The capture is best-effort: failures only leave the context empty and never block the selection.
///
/// @param selection - selected text, used by the fallbacks that locate it in the application
fn capture_context(selection: &str) {
    let limit = CONTEXT_LIMIT.load(Ordering::Relaxed);
    if limit == 0 {
        return;
    }

    match platform::get_selection_context(limit, selection) {
        Ok(context) => {
            debug!("Captured selection context from {:?}", context.app_id);
            if let Ok(mut cache) = SELECTION_CONTEXT_CACHE.lock() {
                *cache = Some(context);
            }
        }
        Err(error) => debug!("Failed to capture selection context: {}", error),
    }
}

/// Capture the current selection area for the vision picking fallback.
///
/// The frontend calls this only when an AI action needs context and the platform exposed none, so
/// the source application still owns focus here. Failures are reported as "no image": the action
/// then simply runs without context.
///
/// @returns base64 encoded PNG, or `None` when the feature is off or the capture failed
#[tauri::command]
pub async fn capture_selection_image() -> Result<Option<String>, AppError> {
    let settings = VISION_CAPTURE.lock().ok().and_then(|slot| *slot);
    let Some(settings) = settings.filter(|settings| settings.enabled) else {
        return Ok(None);
    };
    // a password field would only produce masked characters, so it is never captured
    if platform::is_focus_password() {
        debug!("Skipped the screenshot because the focused control is a password field");
        return Ok(None);
    }

    match capture::capture(&settings) {
        Ok(image) => Ok(Some(image)),
        Err(error) => {
            debug!("Failed to capture the selection image: {}", error);
            Ok(None)
        }
    }
}

/// Set the screenshot area used by the vision picking fallback.
///
/// @param enabled - whether a screenshot may be taken for context
/// @param full_screen - whether to capture the whole virtual screen
/// @param top - pixels captured above the selection
/// @param bottom - pixels captured below the selection
/// @param left - pixels captured to the left of the selection
/// @param right - pixels captured to the right of the selection
#[tauri::command]
pub fn set_vision_capture(
    enabled: bool,
    full_screen: bool,
    top: u32,
    bottom: u32,
    left: u32,
    right: u32,
) -> Result<(), AppError> {
    if let Ok(mut slot) = VISION_CAPTURE.lock() {
        *slot = Some(CaptureSettings {
            enabled,
            full_screen,
            margins: capture::CaptureMargins {
                // the settings page already clamps these, the backend only guards the buffer size
                top: top.min(1080),
                bottom: bottom.min(1080),
                left: left.min(1920),
                right: right.min(1920),
            },
        });
    }
    Ok(())
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
async fn get_selection_fallback(app: AppHandle, mouse: bool) -> Result<String, AppError> {
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
