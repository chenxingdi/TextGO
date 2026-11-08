//! Screenshot capture used when a selection exposes no selectable text.
//!
//! Only the area the user configured is captured: either the whole virtual screen or the selection
//! rectangle expanded by a per-direction margin. The result is a base64 encoded PNG that the
//! frontend sends to a vision model, so nothing is written to disk here.

use crate::error::AppError;
#[cfg(target_os = "windows")]
use crate::platform::SelectionRect;

/// Pixels captured around the selection when the capture area follows the selection.
#[derive(Clone, Copy, Debug, Default)]
pub struct CaptureMargins {
    /// Pixels added above the selection.
    pub top: u32,
    /// Pixels added below the selection.
    pub bottom: u32,
    /// Pixels added to the left of the selection.
    pub left: u32,
    /// Pixels added to the right of the selection.
    pub right: u32,
}

/// Screenshot settings synced from the settings page.
#[derive(Clone, Copy, Debug)]
pub struct CaptureSettings {
    /// Whether the capture may run at all; false when context enhancement, the provider or the
    /// model is not configured.
    pub enabled: bool,
    /// Whether the whole virtual screen is captured instead of the selection area.
    pub full_screen: bool,
    /// Pixels added around the selection when the capture area follows the selection.
    pub margins: CaptureMargins,
}

/// Capture the configured area and return it as a base64 encoded PNG.
///
/// @param settings - capture area and margins
/// @returns base64 encoded PNG
#[cfg(target_os = "windows")]
pub fn capture(settings: &CaptureSettings) -> Result<String, AppError> {
    use windows::Win32::UI::WindowsAndMessaging::{
        GetSystemMetrics, SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN,
        SM_YVIRTUALSCREEN,
    };

    // the virtual screen is the union of all monitors, which keeps the capture coordinates
    // consistent no matter which monitor owns the selection
    let screen_left = unsafe { GetSystemMetrics(SM_XVIRTUALSCREEN) };
    let screen_top = unsafe { GetSystemMetrics(SM_YVIRTUALSCREEN) };
    let screen_width = unsafe { GetSystemMetrics(SM_CXVIRTUALSCREEN) };
    let screen_height = unsafe { GetSystemMetrics(SM_CYVIRTUALSCREEN) };
    if screen_width <= 0 || screen_height <= 0 {
        return Err("Failed to read the screen size".into());
    }

    let area = if settings.full_screen {
        CaptureArea {
            left: screen_left,
            top: screen_top,
            width: screen_width,
            height: screen_height,
        }
    } else {
        selection_area(settings, screen_left, screen_top, screen_width, screen_height)?
    };

    let mut pixels = unsafe { capture_pixels(&area) }?;
    swap_channels(&mut pixels);
    encode_png(area.width as u32, area.height as u32, &pixels)
}

/// Capture is not implemented for this platform yet; the frontend treats the error as "no text".
#[cfg(not(target_os = "windows"))]
pub fn capture(_settings: &CaptureSettings) -> Result<String, AppError> {
    Err("Screen capture is not supported on this platform yet".into())
}

/// Pixels covered by one screenshot in physical screen coordinates.
#[cfg(target_os = "windows")]
#[derive(Clone, Copy, Debug)]
struct CaptureArea {
    left: i32,
    top: i32,
    width: i32,
    height: i32,
}

/// Width assumed for the capture band around the cursor when no text range exists.
#[cfg(target_os = "windows")]
const FALLBACK_BAND_WIDTH: i32 = 640;

/// Line height assumed for the capture band around the cursor when no text range exists.
#[cfg(target_os = "windows")]
const FALLBACK_BAND_HEIGHT: i32 = 40;

/// Build the capture area around the current selection, expanded by the configured margins.
///
/// A text range is not available for images and rendered pages, so the area falls back to a band
/// around the cursor. The result is clamped to the virtual screen.
///
/// @param settings - capture margins
/// @param screen_left - left edge of the virtual screen
/// @param screen_top - top edge of the virtual screen
/// @param screen_width - width of the virtual screen
/// @param screen_height - height of the virtual screen
/// @returns the area to capture
#[cfg(target_os = "windows")]
fn selection_area(
    settings: &CaptureSettings,
    screen_left: i32,
    screen_top: i32,
    screen_width: i32,
    screen_height: i32,
) -> Result<CaptureArea, AppError> {
    let anchor = crate::platform::get_selection_rect()
        .or_else(fallback_band)
        .ok_or_else(|| AppError::from("Failed to locate the selection"))?;
    let margins = settings.margins;

    let left = anchor.left.saturating_sub(margins.left as i32);
    let top = anchor.top.saturating_sub(margins.top as i32);
    let right = anchor
        .left
        .saturating_add(anchor.width)
        .saturating_add(margins.right as i32);
    let bottom = anchor
        .top
        .saturating_add(anchor.height)
        .saturating_add(margins.bottom as i32);

    let left = left.max(screen_left);
    let top = top.max(screen_top);
    let right = right.min(screen_left.saturating_add(screen_width));
    let bottom = bottom.min(screen_top.saturating_add(screen_height));
    if right <= left || bottom <= top {
        return Err("The capture area is empty".into());
    }

    Ok(CaptureArea {
        left,
        top,
        width: right - left,
        height: bottom - top,
    })
}

/// Build a capture band around the cursor for selections without a text range.
///
/// @returns band rectangle, or `None` when the cursor position is unavailable
#[cfg(target_os = "windows")]
fn fallback_band() -> Option<SelectionRect> {
    let (cursor_x, cursor_y) = crate::platform::get_cursor_position()?;
    Some(SelectionRect {
        left: cursor_x - FALLBACK_BAND_WIDTH / 2,
        top: cursor_y - FALLBACK_BAND_HEIGHT / 2,
        width: FALLBACK_BAND_WIDTH,
        height: FALLBACK_BAND_HEIGHT,
    })
}

/// Copy the requested screen area into a top-down BGRA pixel buffer.
///
/// @param area - screen rectangle to copy
/// @returns BGRA pixels, four bytes per pixel
#[cfg(target_os = "windows")]
unsafe fn capture_pixels(area: &CaptureArea) -> Result<Vec<u8>, AppError> {
    use windows::Win32::Graphics::Gdi::{
        BitBlt, CreateCompatibleBitmap, CreateCompatibleDC, DeleteDC, DeleteObject, GetDC, GetDIBits,
        SelectObject, BITMAPINFO, BITMAPINFOHEADER, DIB_RGB_COLORS, HGDIOBJ, ReleaseDC, SRCCOPY,
    };

    let screen_dc = GetDC(None);
    if screen_dc.is_invalid() {
        return Err("Failed to get the screen device context".into());
    }
    let memory_dc = CreateCompatibleDC(Some(screen_dc));
    if memory_dc.is_invalid() {
        let _ = ReleaseDC(None, screen_dc);
        return Err("Failed to create a capture device context".into());
    }
    let bitmap = CreateCompatibleBitmap(screen_dc, area.width, area.height);
    if bitmap.is_invalid() {
        let _ = DeleteDC(memory_dc);
        let _ = ReleaseDC(None, screen_dc);
        return Err("Failed to create a capture bitmap".into());
    }

    let previous = SelectObject(memory_dc, HGDIOBJ(bitmap.0));
    // a failed copy leaves the buffer blank, which the model reports as no text
    let _ = BitBlt(
        memory_dc,
        0,
        0,
        area.width,
        area.height,
        Some(screen_dc),
        area.left,
        area.top,
        SRCCOPY,
    );

    let mut pixels = vec![0u8; area.width as usize * area.height as usize * 4];
    // zeroed is the documented "no palette, no masks" state for a plain 32 bit bitmap
    let mut info: BITMAPINFO = std::mem::zeroed();
    info.bmiHeader.biSize = std::mem::size_of::<BITMAPINFOHEADER>() as u32;
    info.bmiHeader.biWidth = area.width;
    // a negative height asks for top-down rows, matching the PNG row order
    info.bmiHeader.biHeight = -area.height;
    info.bmiHeader.biPlanes = 1;
    info.bmiHeader.biBitCount = 32;
    // BI_RGB
    info.bmiHeader.biCompression = 0;
    let lines = GetDIBits(
        memory_dc,
        bitmap,
        0,
        area.height as u32,
        Some(pixels.as_mut_ptr() as *mut std::ffi::c_void),
        &mut info,
        DIB_RGB_COLORS,
    );

    SelectObject(memory_dc, previous);
    let _ = DeleteObject(HGDIOBJ(bitmap.0));
    let _ = DeleteDC(memory_dc);
    let _ = ReleaseDC(None, screen_dc);

    if lines == 0 {
        return Err("Failed to read the screenshot pixels".into());
    }
    Ok(pixels)
}

/// Convert GDI BGRA pixels to the RGBA order PNG expects.
///
/// GDI leaves the alpha channel at zero, which would make the image fully transparent.
///
/// @param pixels - pixels to convert in place
#[cfg(target_os = "windows")]
fn swap_channels(pixels: &mut [u8]) {
    for pixel in pixels.chunks_exact_mut(4) {
        pixel.swap(0, 2);
        pixel[3] = 255;
    }
}

/// Encode raw RGBA pixels as a base64 encoded PNG.
///
/// @param width - image width in pixels
/// @param height - image height in pixels
/// @param rgba - RGBA pixels, four bytes per pixel
/// @returns base64 encoded PNG
#[cfg(target_os = "windows")]
fn encode_png(width: u32, height: u32, rgba: &[u8]) -> Result<String, AppError> {
    use base64::prelude::*;
    use image::{ImageBuffer, ImageFormat, Rgba};

    let image = ImageBuffer::<Rgba<u8>, _>::from_raw(width, height, rgba.to_vec())
        .ok_or_else(|| AppError::from("Failed to build the screenshot image"))?;
    let mut buffer = std::io::Cursor::new(Vec::new());
    image
        .write_to(&mut buffer, ImageFormat::Png)
        .map_err(|error| AppError::from(format!("Failed to encode the screenshot: {error}")))?;
    Ok(BASE64_STANDARD.encode(buffer.into_inner()))
}
