use crate::clipboard_watch;
use crate::error::AppError;
use crate::platform;
use crate::{CLIPBOARD, CLIPBOARD_RESTORE_INTERRUPTED};
use clipboard_rs::{Clipboard, ClipboardContent, ContentFormat};
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
    platform::clipboard_change_count()
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

/// Write text into the clipboard, marked as our own write.
///
/// The change monitor must not mistake a text we write for a copy of the user's. The caller decides
/// what the write means: a copy the user asked for ([`copy_text`]) or content the application
/// produced ([`set_clipboard_text`]).
///
/// @param text - text placed on the clipboard
pub(crate) fn write_clipboard_text(text: String) -> Result<(), AppError> {
    run(|| {
        let _own = clipboard_watch::begin_own_write();
        Ok(CLIPBOARD.lock()?.as_ref()?.set_text(text)?)
    })
}

/// Set clipboard text content produced by the application itself.
///
/// The recorded user copy is forgotten: an answer, a translation or a script result replaces the
/// pasteboard slot, so the paragraph copied a moment ago is gone and its moment must not be used to
/// date this text as surrounding context.
///
/// @param text - text produced by the application
#[tauri::command]
pub fn set_clipboard_text(text: String) -> Result<(), AppError> {
    write_clipboard_text(text)?;
    clipboard_watch::forget_copy();
    Ok(())
}

/// Write text the user asked us to copy.
///
/// The write is dated as a user copy so that a selection made shortly afterwards can use the text
/// as surrounding context. Every copy the user starts through the UI must use this command,
/// otherwise copying a paragraph with our own toolbar never becomes usable as context.
///
/// @param text - text the user asked to copy
#[tauri::command]
pub fn copy_text(text: String) -> Result<(), AppError> {
    write_clipboard_text(text)?;
    clipboard_watch::note_user_copy();
    Ok(())
}

/// Clear clipboard contents.
#[tauri::command]
pub fn clear_clipboard() -> Result<(), AppError> {
    run(|| {
        let _own = clipboard_watch::begin_own_write();
        Ok(CLIPBOARD.lock()?.as_ref()?.clear()?)
    })
}

/// Largest backup text kept for context, in bytes.
///
/// Anything larger is a whole-document copy: cloning it on every selection would cost more than the
/// context could ever use, and the surrounding-text window is bounded anyway.
const MAX_BACKUP_TEXT_BYTES: usize = 512 * 1024;

/// Plain text of a clipboard backup, when the copy published any.
///
/// @param contents - formats read from the clipboard before our operation
/// @returns the text format payload, if the copy carried one
fn backup_text(contents: &[ClipboardContent]) -> Option<String> {
    contents.iter().find_map(|content| match content {
        ClipboardContent::Text(text) if text.len() <= MAX_BACKUP_TEXT_BYTES => Some(text.clone()),
        _ => None,
    })
}

/// Backup clipboard contents, execute operation, then restore clipboard contents.
///
/// @param operation - work performed while the clipboard is backed up
/// @returns the operation result and the text the clipboard held before it ran
pub async fn with_clipboard_backup<F, Fut, T>(operation: F) -> Result<(T, Option<String>), AppError>
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

    // what the user copied before this selection, used as a source of surrounding text
    let previous_text = backup_text(&contents);

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
            run(|| {
                let _own = clipboard_watch::begin_own_write();
                Ok(CLIPBOARD.lock()?.as_ref()?.set(contents)?)
            })?;
        } else {
            clear_clipboard()?;
        }
    }

    Ok((result, previous_text))
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
