#[cfg(not(target_os = "windows"))]
use crate::commands::clipboard::clear_clipboard;
#[cfg(target_os = "windows")]
use crate::commands::clipboard::{clipboard_sequence, CLIPBOARD_CONSUMED_SEQUENCE};
use crate::commands::clipboard::{get_clipboard_text, with_clipboard_backup};
use crate::commands::keyboard::send_copy_keys;
use crate::commands::shortcut::ShortcutHandlerGuard;
use crate::error::AppError;
use crate::platform::{self, SelectionContext};
use crate::{CONTEXT_LIMIT, FORCE_GET_SELECTION, SELECTION_CONTEXT_CACHE, SELECTION_TEXT_CACHE};
use log::{debug, warn};
use std::sync::atomic::{AtomicU64, Ordering};
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
/// depend on; the popup only opens after this command resolves.
///
/// @param text - selected text that was just obtained
/// @returns the same text, unchanged
fn with_context(text: String) -> String {
    capture_context();
    text
}

/// Capture the surrounding context of the focused selection while the source application still
/// owns focus, and remember it for the popup that is about to open.
///
/// The capture is best-effort: failures only leave the context empty and never block the selection.
fn capture_context() {
    let limit = CONTEXT_LIMIT.load(Ordering::Relaxed);
    if limit == 0 {
        return;
    }

    match platform::get_selection_context(limit) {
        Ok(context) => {
            debug!("Captured selection context from {:?}", context.app_id);
            if let Ok(mut cache) = SELECTION_CONTEXT_CACHE.lock() {
                *cache = Some(context);
            }
        }
        Err(error) => debug!("Failed to capture selection context: {}", error),
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

/// Get selected text through clipboard.
async fn get_selection_fallback(app: AppHandle, mouse: bool) -> Result<String, AppError> {
    if let Ok(mut cache) = SELECTION_TEXT_CACHE.lock() {
        *cache = None;
    }

    // use backup-operation-restore mode
    with_clipboard_backup(|| async move {
        // Windows: leave the clipboard alone until the copy has landed. Qt4 based readers such as
        // GoldenDict crash when another process opens the clipboard while they publish a copy:
        // QClipboard hands the mime data to OLE, then keeps writing WebKit's pasteboard formats to
        // it. Clearing the clipboard or polling it by reading races with that window, so track the
        // clipboard sequence number instead (a plain Win32 counter, it never opens the clipboard).
        #[cfg(target_os = "windows")]
        let baseline = clipboard_sequence();

        #[cfg(not(target_os = "windows"))]
        clear_clipboard()?;

        // send copy shortcut
        // https://github.com/enigo-rs/enigo/issues/153
        let _ = app.run_on_main_thread(move || {
            // mouse-triggered selections don't need to release modifier keys
            let _ = send_copy_keys(Some(false), Some(!mouse));
        });

        // wait for clipboard content to change in a loop
        let max_wait_time = Duration::from_millis(MAX_WAIT_TIME.load(Ordering::Relaxed));
        let mut selected_text = String::new();

        #[cfg(target_os = "windows")]
        {
            let deadline = Instant::now() + max_wait_time;
            let mut published = false;

            while Instant::now() < deadline {
                tokio::time::sleep(CLIPBOARD_CHECK_INTERVAL).await;

                if clipboard_sequence() != baseline {
                    published = true;
                    break;
                }
            }

            if published {
                // the sequence number moves before the owner finishes; give it the rest of the write
                tokio::time::sleep(CLIPBOARD_SETTLE_TIME).await;

                for _attempt in 0..CLIPBOARD_READ_ATTEMPTS {
                    match get_clipboard_text() {
                        Ok(current_text) if !current_text.is_empty() => {
                            selected_text = current_text;
                            break;
                        }
                        _ => tokio::time::sleep(CLIPBOARD_CHECK_INTERVAL).await,
                    }
                }

                // remember what we consumed, so the restore step can tell our copy apart from a
                // newer one the user made in the meantime
                if let Some(sequence) = clipboard_sequence() {
                    CLIPBOARD_CONSUMED_SEQUENCE.store(sequence, Ordering::Relaxed);
                }
            }
        }

        #[cfg(not(target_os = "windows"))]
        {
            let check_interval = Duration::from_millis(5); // check interval 5ms
            let max_attempts = max_wait_time.as_millis() / check_interval.as_millis();

            for _attempt in 0..max_attempts {
                tokio::time::sleep(check_interval).await;

                // read current clipboard text
                if let Ok(current_text) = get_clipboard_text() {
                    if !current_text.is_empty() {
                        // if clipboard content changed, copy operation completed
                        selected_text = current_text;
                        break;
                    }
                }
            }
        }

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
