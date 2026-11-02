use crate::error::AppError;
use crate::{CLIPBOARD, CLIPBOARD_RESTORE_INTERRUPTED};
use clipboard_rs::{Clipboard, ContentFormat};
use log::debug;
use std::sync::atomic::{AtomicU64, Ordering};

/// Sequence number of the clipboard content the last fallback read consumed.
///
/// Zero means no read is in flight. Only meaningful on Windows, where the restore step uses it to
/// avoid overwriting content another application copied right after us.
pub static CLIPBOARD_CONSUMED_SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// Clipboard update counter that changes whenever any application publishes new content.
///
/// Reading it never opens the clipboard, so polling it cannot disturb another application that is
/// still writing. Returns `None` on platforms that do not expose such a counter.
pub fn clipboard_sequence() -> Option<u64> {
    #[cfg(target_os = "windows")]
    {
        Some(unsafe { windows::Win32::System::DataExchange::GetClipboardSequenceNumber() } as u64)
    }

    #[cfg(not(target_os = "windows"))]
    {
        None
    }
}

// all supported clipboard content formats
const ALL_FORMATS: [ContentFormat; 5] = [
    ContentFormat::Text,
    ContentFormat::Rtf,
    ContentFormat::Html,
    ContentFormat::Image,
    ContentFormat::Files,
];

/// Get clipboard text content.
#[tauri::command]
pub fn get_clipboard_text() -> Result<String, AppError> {
    run(|| match CLIPBOARD.lock()?.as_ref()?.get_text() {
        Ok(text) => Ok(text),
        Err(_) => Ok(String::new()),
    })
}

/// Set clipboard text content.
#[tauri::command]
pub fn set_clipboard_text(text: String) -> Result<(), AppError> {
    run(|| Ok(CLIPBOARD.lock()?.as_ref()?.set_text(text)?))
}

/// Clear clipboard contents.
#[tauri::command]
pub fn clear_clipboard() -> Result<(), AppError> {
    run(|| Ok(CLIPBOARD.lock()?.as_ref()?.clear()?))
}

/// Backup clipboard contents, execute operation, then restore clipboard contents.
pub async fn with_clipboard_backup<F, Fut, T>(operation: F) -> Result<T, AppError>
where
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = Result<T, AppError>>,
{
    // reset interrupted state before operation
    CLIPBOARD_RESTORE_INTERRUPTED.store(false, Ordering::Relaxed);
    CLIPBOARD_CONSUMED_SEQUENCE.store(0, Ordering::Relaxed);

    // backup all format contents
    let contents = run(|| Ok(CLIPBOARD.lock()?.as_ref()?.get(&ALL_FORMATS)?))?;
    debug!("Clipboard backup: saved original clipboard contents");

    // execute operation
    let result = operation().await?;

    // A failing clipboard write is what crashes Qt4-based readers, so skip the restore when the
    // clipboard changed after our read: another application owns it now and must win.
    let consumed = CLIPBOARD_CONSUMED_SEQUENCE.swap(0, Ordering::Relaxed);
    let externally_changed = consumed != 0
        && clipboard_sequence().is_some_and(|sequence| sequence != consumed);

    // check if restore was interrupted by user copy shortcut during the operation
    if CLIPBOARD_RESTORE_INTERRUPTED.swap(false, Ordering::Relaxed) {
        debug!("Clipboard restore skipped: interrupted by user copy shortcut");
    } else if externally_changed {
        debug!("Clipboard restore skipped: clipboard changed after the read");
    } else {
        // restore original clipboard contents
        debug!("Clipboard restore: restoring original clipboard contents");
        if !contents.is_empty() {
            run(|| Ok(CLIPBOARD.lock()?.as_ref()?.set(contents)?))?;
        } else {
            clear_clipboard()?;
        }
    }

    Ok(result)
}

/// Run function on main thread if on macOS, otherwise run directly.
fn run<F, T>(func: F) -> Result<T, AppError>
where
    F: FnOnce() -> Result<T, AppError> + Send + 'static,
    T: Send + 'static,
{
    #[cfg(target_os = "macos")]
    {
        use crate::APP_HANDLE;

        let (tx, rx) = std::sync::mpsc::channel();
        if let Some(app) = APP_HANDLE.lock()?.clone() {
            app.run_on_main_thread(move || {
                let _ = tx.send(func());
            })?;
        }
        rx.recv()?
    }

    #[cfg(not(target_os = "macos"))]
    {
        func()
    }
}
