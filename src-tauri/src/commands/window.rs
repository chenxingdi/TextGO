use crate::error::AppError;
use crate::platform;
use crate::{ENIGO, TOOLBAR_MENU_OPEN};
use enigo::Mouse;
use serde::{Deserialize, Serialize};
use std::future::Future;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{LazyLock, Mutex};
use std::time::Duration;
use tauri::{
    AppHandle, Emitter, LogicalPosition, LogicalSize, Manager, Monitor, Position, WebviewWindow,
};

// structure to hold window placement information
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WindowPlacement {
    pub screen_size: Option<LogicalSize<f64>>,
    pub screen_position: Option<LogicalPosition<f64>>,
    pub window_position: LogicalPosition<f64>,
}

/// Logical screen rectangle, expressed as `(left, top, right, bottom)`.
type LogicalRect = (f64, f64, f64, f64);

/// Where the toolbar is anchored while it is visible.
#[derive(Clone, Copy)]
enum ToolbarAnchor {
    /// Last line of the current selection, in logical screen pixels.
    Selection(LogicalRect),
    /// Cursor position, used when the application exposes no selection geometry.
    Cursor(LogicalPosition<f64>),
}

/// Logical size the toolbar window holds or is about to take.
#[derive(Clone, Copy)]
struct ToolbarSize {
    width: f64,
    height: f64,
}

/// Size and placement of the toolbar window, measured by the toolbar page and passed back with
/// every geometry update.
#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolbarGeometry {
    /// Window width in logical pixels.
    width: f64,
    /// Window height in logical pixels.
    height: f64,
    /// Transparent padding between each window edge and the visible content, in logical pixels.
    /// Callers that cannot measure it yet omit it and fall back to the default.
    #[serde(default)]
    inset: Option<f64>,
    /// Whether to position the window near the mouse cursor instead of the selection.
    mouse: bool,
    /// Whether to place the window on its anchor instead of keeping it where it is.
    reposition: bool,
    /// Whether the window renders the HTML action menu, which is only kept inside the safe area.
    menu_mode: bool,
    /// Visible gap between the toolbar and the text line, in logical pixels.
    gap: f64,
    /// Position of the anchor across the toolbar width, in percent.
    anchor_percent: f64,
    /// Estimated distance from the cursor down to the bottom of its text line, in logical pixels.
    line_offset: f64,
}

/// Placement values configured by the user and passed with every toolbar command.
#[derive(Clone, Copy)]
struct ToolbarPlacement {
    /// Visible gap between the toolbar and the text line, in logical pixels.
    gap: f64,
    /// Position of the anchor across the toolbar width, in percent: 0 puts the toolbar left
    /// edge on the anchor, 50 centers it, 100 puts its right edge on the anchor.
    anchor_percent: f64,
    /// Estimated distance from the cursor down to the bottom of its text line, in logical
    /// pixels, used when no selection geometry is available.
    line_offset: f64,
}

// window position offset from cursor, used when a window falls back to the mouse position
const WINDOW_OFFSET: i32 = 5;

// default gap between the text and the visible toolbar, in logical pixels
const DEFAULT_TOOLBAR_TEXT_GAP: f64 = 4.0;

// default position of the anchor across the toolbar width, in percent
const DEFAULT_TOOLBAR_ANCHOR_PERCENT: f64 = 25.0;

// Default distance between the cursor and the bottom of the text line it sits in, used when the
// focused application exposes no selection geometry (GoldenDict and similar readers). Double
// clicks land around the middle of a line, so roughly half a line is a good default.
const DEFAULT_TOOLBAR_LINE_OFFSET: f64 = 14.0;

// fallback transparent padding between the window edges and the visible toolbar content, used
// until the toolbar page reports the padding it actually renders with
const DEFAULT_TOOLBAR_CONTENT_INSET: f64 = 4.0;

// minimum gap kept between the window and the screen edges
const SCREEN_EDGE_MARGIN: i32 = 2;

// gap between the popup and the toolbar when the popup is placed above it
const POPUP_TOOLBAR_GAP: f64 = 8.0;

// bottom safe area offset to avoid taskbar/dock
const SAFE_AREA_BOTTOM: i32 = 80;

// maximum wait time for window initialization
const INITIALIZATION_TIMEOUT_MS: u64 = 5000;

// maximum time to wait for the popup source application to regain focus
const FOCUS_RESTORE_TIMEOUT_MS: u64 = 500;

// interval between source focus checks
const FOCUS_RESTORE_INTERVAL_MS: u64 = 10;

// initialization flags for popup and toolbar windows
static POPUP_INITIALIZED: AtomicBool = AtomicBool::new(false);
static TOOLBAR_INITIALIZED: AtomicBool = AtomicBool::new(false);
static POPUP_SOURCE_FOCUS: LazyLock<Mutex<Option<platform::FocusTarget>>> =
    LazyLock::new(|| Mutex::new(None));

/// Anchor captured when the toolbar is shown, reused while it is still visible so that a
/// resize never makes the toolbar jump.
static TOOLBAR_ANCHOR: LazyLock<Mutex<Option<ToolbarAnchor>>> = LazyLock::new(|| Mutex::new(None));

/// Activate a focus target and wait until the operating system reports it as active.
async fn restore_focus_with<T, Activate, IsActive, Sleep, SleepFuture>(
    target: T,
    activate: Activate,
    is_active: IsActive,
    sleep: Sleep,
) -> Result<(), AppError>
where
    T: Copy,
    Activate: FnOnce(T) -> Result<(), AppError>,
    IsActive: Fn(T) -> bool,
    Sleep: Fn(Duration) -> SleepFuture,
    SleepFuture: Future<Output = ()>,
{
    activate(target)?;

    let interval = Duration::from_millis(FOCUS_RESTORE_INTERVAL_MS);
    let max_checks = FOCUS_RESTORE_TIMEOUT_MS / FOCUS_RESTORE_INTERVAL_MS;
    for check in 0..=max_checks {
        if is_active(target) {
            return Ok(());
        }
        if check < max_checks {
            sleep(interval).await;
        }
    }

    Err("Failed to restore focus to popup source application".into())
}

/// Show main window.
#[tauri::command]
pub fn show_main_window(app: AppHandle) {
    show_window(&app, "main");
}

/// Hide main window.
#[tauri::command]
pub fn hide_main_window(app: AppHandle) {
    hide_window(&app, "main");
}

/// Toggle main window visibility.
#[tauri::command]
pub fn toggle_main_window(app: AppHandle) {
    toggle_window(&app, "main");
}

/// Navigate to a specific page in the main window.
#[tauri::command]
pub fn navigate_to(app: AppHandle, url: String) {
    if let Some(window) = show_window(&app, "main") {
        // emit page navigation event
        let _ = window.emit("goto", url);
    }
}

/// Mark popup window as initialized.
#[tauri::command]
pub fn mark_popup_initialized() {
    POPUP_INITIALIZED.store(true, Ordering::Relaxed);
}

/// Mark toolbar window as initialized.
#[tauri::command]
pub fn mark_toolbar_initialized() {
    TOOLBAR_INITIALIZED.store(true, Ordering::Relaxed);
}

/// Get the stable content scale used to size the toolbar window.
#[tauri::command]
pub fn get_toolbar_zoom_factor() -> f64 {
    #[cfg(target_os = "windows")]
    {
        platform::get_text_scale_factor()
    }
    #[cfg(not(target_os = "windows"))]
    {
        1.0
    }
}

/// Set toolbar native menu open state.
#[tauri::command]
pub fn set_toolbar_menu_open(open: bool) {
    TOOLBAR_MENU_OPEN.store(open, Ordering::Relaxed);
}

/// Screen bounds of the monitor the given window currently sits on.
fn screen_bounds(
    window: &WebviewWindow,
) -> (Option<LogicalSize<f64>>, Option<LogicalPosition<f64>>) {
    let Ok(Some(monitor)) = window.current_monitor() else {
        return (None, None);
    };

    let scale_factor = monitor.scale_factor();
    let size = monitor.size();
    let position = monitor.position();

    (
        Some(LogicalSize::new(
            size.width as f64 / scale_factor,
            size.height as f64 / scale_factor,
        )),
        Some(LogicalPosition::new(
            position.x as f64 / scale_factor,
            position.y as f64 / scale_factor,
        )),
    )
}

/// Clamp a logical window position so the window stays inside the screen safe area.
fn clamp_to_screen(
    position: LogicalPosition<f64>,
    window_width: f64,
    window_height: f64,
    screen_size: Option<LogicalSize<f64>>,
    screen_position: Option<LogicalPosition<f64>>,
    safe_area_bottom: f64,
) -> LogicalPosition<f64> {
    let (Some(screen_size), Some(screen_position)) = (screen_size, screen_position) else {
        return position;
    };

    let min_x = screen_position.x;
    let max_x = (screen_position.x + screen_size.width - window_width).max(min_x);
    let min_y = screen_position.y;
    let max_y =
        (screen_position.y + screen_size.height - window_height - safe_area_bottom).max(min_y);

    LogicalPosition::new(
        position.x.clamp(min_x, max_x),
        position.y.clamp(min_y, max_y),
    )
}

/// Show popup window and position it near the cursor.
///
/// `memory_position` is the position remembered for this action; when set it wins over the cursor.
#[tauri::command]
pub fn show_popup(
    app: AppHandle,
    payload: String,
    mouse: Option<bool>,
    memory_position: Option<LogicalPosition<f64>>,
) -> Result<(), AppError> {
    *POPUP_SOURCE_FOCUS.lock()? = platform::get_focus_target();

    if let Some(window) = app.get_webview_window("popup") {
        match memory_position {
            // keep the popup where the user last moved it
            Some(saved) => {
                let scale_factor = window.scale_factor()?;
                let window_size = window.outer_size()?;
                let window_width = window_size.width as f64 / scale_factor;
                let window_height = window_size.height as f64 / scale_factor;
                let (screen_size, screen_position) = screen_bounds(&window);
                let position = clamp_to_screen(
                    saved,
                    window_width,
                    window_height,
                    screen_size,
                    screen_position,
                    SAFE_AREA_BOTTOM as f64 / scale_factor,
                );

                window.set_position(Position::Logical(position))?;
            }
            // otherwise position window near cursor
            None => {
                let size = window_logical_size(&window)?;
                position_window_near_cursor(&window, mouse.unwrap_or(false), size)?
            }
        }

        // show and focus window
        if !POPUP_INITIALIZED.load(Ordering::Relaxed) {
            show_window(&app, "popup");
        }

        // wait for initialization and emit event
        wait_and_emit(&POPUP_INITIALIZED, window, payload);
    } else {
        return Err("Popup window not found".into());
    }

    Ok(())
}

/// Vertical bounds of the toolbar window in logical pixels.
///
/// Returns `(top, bottom)` and `(None, None)` when the toolbar window is unavailable.
fn toolbar_vertical_bounds(app: &AppHandle, fallback_scale: f64) -> (Option<f64>, Option<f64>) {
    let Some(toolbar) = app.get_webview_window("toolbar") else {
        return (None, None);
    };

    let scale_factor = toolbar.scale_factor().unwrap_or(fallback_scale);
    let (Ok(position), Ok(size)) = (toolbar.outer_position(), toolbar.outer_size()) else {
        return (None, None);
    };

    // convert to logical coordinates on macOS
    let top = position.y as f64 / scale_factor;
    let height = size.height as f64 / scale_factor;

    (Some(top), Some(top + height))
}

/// Show popup window and position it at the given logical position.
///
/// `memory_position` is the position remembered for this action; when set it wins over the
/// toolbar placement. Otherwise the popup prefers the area above the visible toolbar, then
/// below it, so the toolbar stays usable.
#[tauri::command]
pub fn show_popup_sameplace(
    app: AppHandle,
    payload: String,
    placement: WindowPlacement,
    memory_position: Option<LogicalPosition<f64>>,
) -> Result<(), AppError> {
    let mut source_focus = POPUP_SOURCE_FOCUS.lock()?;
    if source_focus.is_none() {
        *source_focus = platform::get_focus_target();
    }
    drop(source_focus);

    if let Some(window) = app.get_webview_window("popup") {
        // get popup window size in logical pixels
        let scale_factor = window.scale_factor()?;
        let window_size = window.outer_size()?;
        let window_width = window_size.width as f64 / scale_factor;
        let window_height = window_size.height as f64 / scale_factor;
        let safe_area_bottom = SAFE_AREA_BOTTOM as f64 / scale_factor;

        // screen bounds reported by the caller, falling back to the popup monitor
        let (screen_size, screen_position) =
            match (placement.screen_size, placement.screen_position) {
                (Some(size), Some(position)) => (Some(size), Some(position)),
                _ => screen_bounds(&window),
            };

        // a position the user moved the popup to wins over the toolbar placement
        let position = match memory_position {
            Some(saved) => clamp_to_screen(
                saved,
                window_width,
                window_height,
                screen_size,
                screen_position,
                safe_area_bottom,
            ),
            None => {
                // otherwise prefer the area above the toolbar, then below it, then the placement
                let candidate = match toolbar_vertical_bounds(&app, scale_factor) {
                    (Some(toolbar_top), Some(toolbar_bottom)) => {
                        let above = toolbar_top - window_height - POPUP_TOOLBAR_GAP;
                        let below = toolbar_bottom + POPUP_TOOLBAR_GAP;
                        let fits_above = screen_position
                            .map(|screen| above >= screen.y)
                            .unwrap_or(true);
                        let fits_below = match (screen_position, screen_size) {
                            (Some(screen), Some(size)) => {
                                below <= screen.y + size.height - window_height - safe_area_bottom
                            }
                            _ => true,
                        };

                        if fits_above {
                            LogicalPosition::new(placement.window_position.x, above)
                        } else if fits_below {
                            LogicalPosition::new(placement.window_position.x, below)
                        } else {
                            placement.window_position
                        }
                    }
                    _ => placement.window_position,
                };

                clamp_to_screen(
                    candidate,
                    window_width,
                    window_height,
                    screen_size,
                    screen_position,
                    safe_area_bottom,
                )
            }
        };

        window.set_position(Position::Logical(position))?;

        // show and focus window
        if !POPUP_INITIALIZED.load(Ordering::Relaxed) {
            show_window(&app, "popup");
        }

        // wait for initialization and emit event
        wait_and_emit(&POPUP_INITIALIZED, window, payload);
    } else {
        return Err("Popup window not found".into());
    }

    Ok(())
}

/// Resize the toolbar window to the measured content size and place it.
///
/// Size and placement travel in one call so the placement math uses the size the window is about
/// to take: the platform applies a resize asynchronously, and reading the window size back right
/// after resizing would still report the previous content size.
#[tauri::command]
pub fn apply_toolbar_geometry(app: AppHandle, geometry: ToolbarGeometry) -> Result<(), AppError> {
    let window = app
        .get_webview_window("toolbar")
        .ok_or("Toolbar window not found")?;

    let size = ToolbarSize {
        width: geometry.width.max(1.0),
        height: geometry.height.max(1.0),
    };

    window.set_size(LogicalSize::new(size.width, size.height))?;

    if geometry.reposition {
        // reuse the anchor captured while showing, so a resize never makes the toolbar jump
        let anchor = *TOOLBAR_ANCHOR.lock()?;
        let placement = ToolbarPlacement::from_options(
            Some(geometry.gap),
            Some(geometry.anchor_percent),
            Some(geometry.line_offset),
        );
        let inset = geometry
            .inset
            .unwrap_or(DEFAULT_TOOLBAR_CONTENT_INSET * toolbar_content_scale());
        position_toolbar_at(&window, anchor, geometry.mouse, placement, size, inset)?;
    } else if geometry.menu_mode {
        // the HTML action menu grows and shrinks with its item count, keep it inside the safe area
        clamp_toolbar_to_safe_area(&window, size)?;
    }

    Ok(())
}

/// Keep the toolbar window inside the safe area of the monitor it currently sits on.
fn clamp_toolbar_to_safe_area(window: &WebviewWindow, size: ToolbarSize) -> Result<(), AppError> {
    let scale_factor = window.scale_factor()?;
    let position = window.outer_position()?;
    let x = position.x as f64 / scale_factor;
    let y = position.y as f64 / scale_factor;

    let monitor = monitor_at_logical(window, x, y)?;
    let area = safe_area(size.width, size.height, &monitor);

    window.set_position(Position::Logical(LogicalPosition::new(
        x.clamp(area.min_x, area.max_x),
        y.clamp(area.min_y, area.max_y),
    )))?;

    Ok(())
}

/// Show toolbar window and position it near the selection.
#[tauri::command]
pub fn show_toolbar(
    app: AppHandle,
    payload: String,
    mouse: Option<bool>,
    gap: Option<f64>,
    anchor_percent: Option<f64>,
    line_offset: Option<f64>,
) -> Result<(), AppError> {
    *POPUP_SOURCE_FOCUS.lock()? = platform::get_focus_target();

    if let Some(window) = app.get_webview_window("toolbar") {
        // capture the anchor before showing; it is reused when the toolbar is resized
        let anchor = capture_toolbar_anchor(&window);
        *TOOLBAR_ANCHOR.lock()? = anchor;

        // position before setup so native-menu actions also inherit the current placement; the
        // size is provisional until the toolbar reports its measured content size
        let placement = ToolbarPlacement::from_options(gap, anchor_percent, line_offset);
        let size = window_logical_size(&window)?;
        let inset = DEFAULT_TOOLBAR_CONTENT_INSET * toolbar_content_scale();
        position_toolbar_at(
            &window,
            anchor,
            mouse.unwrap_or(false),
            placement,
            size,
            inset,
        )?;

        // show window without focusing
        if !TOOLBAR_INITIALIZED.load(Ordering::Relaxed) {
            show_toolbar_regardless(app.clone(), None)?;
        }

        // wait for initialization and emit event
        wait_and_emit(&TOOLBAR_INITIALIZED, window, payload);
    } else {
        return Err("Toolbar window not found".into());
    }

    Ok(())
}

impl ToolbarPlacement {
    /// Read user placement values, falling back to the defaults and clamping them.
    fn from_options(
        gap: Option<f64>,
        anchor_percent: Option<f64>,
        line_offset: Option<f64>,
    ) -> Self {
        Self {
            gap: gap.unwrap_or(DEFAULT_TOOLBAR_TEXT_GAP).clamp(0.0, 200.0),
            anchor_percent: anchor_percent
                .unwrap_or(DEFAULT_TOOLBAR_ANCHOR_PERCENT)
                .clamp(0.0, 100.0),
            line_offset: line_offset
                .unwrap_or(DEFAULT_TOOLBAR_LINE_OFFSET)
                .clamp(0.0, 200.0),
        }
    }
}

/// Capture the anchor the toolbar should be placed against.
///
/// The selection rectangle is preferred; applications that expose no selection geometry (such
/// as GoldenDict) fall back to the cursor position.
fn capture_toolbar_anchor(window: &WebviewWindow) -> Option<ToolbarAnchor> {
    capture_selection_anchor(window)
        .map(ToolbarAnchor::Selection)
        .or_else(|_| capture_cursor_anchor(window).map(ToolbarAnchor::Cursor))
        .ok()
}

/// Logical size the window currently occupies.
///
/// Used where the caller does not pass the size itself: the provisional toolbar placement taken
/// before the toolbar reports its measured content size, and the popup positioned near the cursor.
fn window_logical_size(window: &WebviewWindow) -> Result<ToolbarSize, AppError> {
    let scale_factor = window.scale_factor()?;
    let size = window.outer_size()?;

    Ok(ToolbarSize {
        width: size.width as f64 / scale_factor,
        height: size.height as f64 / scale_factor,
    })
}

/// Place the toolbar according to the captured anchor.
fn position_toolbar_at(
    window: &WebviewWindow,
    anchor: Option<ToolbarAnchor>,
    mouse: bool,
    placement: ToolbarPlacement,
    size: ToolbarSize,
    inset: f64,
) -> Result<(), AppError> {
    match anchor {
        Some(ToolbarAnchor::Selection(rect)) => {
            position_toolbar_below_selection(window, rect, placement, size, inset)
        }
        Some(ToolbarAnchor::Cursor(cursor)) => {
            position_toolbar_near_cursor(window, cursor, placement, size, inset)
        }
        None => position_window_near_cursor(window, mouse, size),
    }
}

/// Show toolbar window without focusing it.
///
/// When `only_if_hidden` is true, leave an already visible toolbar unchanged.
/// Return an error if the window/panel is missing or a visibility operation fails.
#[tauri::command]
pub fn show_toolbar_regardless(
    app: AppHandle,
    only_if_hidden: Option<bool>,
) -> Result<(), AppError> {
    let window = app
        .get_webview_window("toolbar")
        .ok_or("Toolbar window not found")?;
    if only_if_hidden.unwrap_or(false) && window.is_visible()? {
        return Ok(());
    }

    #[cfg(target_os = "macos")]
    {
        use tauri_nspanel::ManagerExt;

        let panel = app
            .get_webview_panel("toolbar")
            .map_err(|_| "Toolbar panel not found")?;
        // bring to front without making key
        panel.order_front_regardless();
    }
    #[cfg(not(target_os = "macos"))]
    {
        window.show()?;
        // refresh z-order to ensure toolbar stays on top
        window.set_always_on_top(false)?;
        window.set_always_on_top(true)?;
    }

    Ok(())
}

/// Restore focus to the application that opened the popup.
#[tauri::command]
pub async fn focus_popup_source() -> Result<(), AppError> {
    let target = POPUP_SOURCE_FOCUS
        .lock()?
        .take()
        .ok_or("Popup source focus target not found")?;

    restore_focus_with(
        target,
        platform::activate_focus_target,
        platform::is_focus_target_active,
        tokio::time::sleep,
    )
    .await
}

/// Wait for window initialization and emit event.
///
/// If already initialized, emit event immediately.
/// Otherwise, spawn async task to wait and then emit.
fn wait_and_emit(flag: &'static AtomicBool, window: WebviewWindow, payload: String) {
    // get window label and construct event name
    let window_label = window.label().to_string();
    let event_name = format!("show-{}", window_label);

    // broadcast to every window so listeners such as the toolbar can react as well
    let app_handle = window.app_handle().clone();

    // if already initialized, emit immediately
    if flag.load(Ordering::Relaxed) {
        let _ = app_handle.emit(&event_name, payload);
        return;
    }

    // spawn async task to wait for initialization and send data
    tauri::async_runtime::spawn(async move {
        // wait for initialization with timeout
        const CHECK_INTERVAL_MS: u64 = 10;
        const MAX_CHECKS: u64 = INITIALIZATION_TIMEOUT_MS / CHECK_INTERVAL_MS;
        for _ in 0..MAX_CHECKS {
            tokio::time::sleep(Duration::from_millis(CHECK_INTERVAL_MS)).await;
            if flag.load(Ordering::Relaxed) {
                break;
            }
        }

        // emit event after initialization or timeout
        let _ = app_handle.emit(&event_name, payload);
    });
}

/// Monitor that contains the given point, expressed in the raw coordinate space of platform
/// input: physical pixels on Windows, logical points on macOS.
fn monitor_at(window: &WebviewWindow, x: i32, y: i32) -> Result<Monitor, AppError> {
    window
        .available_monitors()?
        .into_iter()
        .find(|m| {
            let pos = m.position();
            let size = m.size();

            // check against physical coordinates on Windows, logical on macOS
            #[cfg(target_os = "windows")]
            {
                x >= pos.x
                    && x < pos.x + size.width as i32
                    && y >= pos.y
                    && y < pos.y + size.height as i32
            }
            #[cfg(not(target_os = "windows"))]
            {
                let scale = m.scale_factor();
                let logical_x = (pos.x as f64 / scale) as i32;
                let logical_y = (pos.y as f64 / scale) as i32;
                let logical_width = (size.width as f64 / scale) as i32;
                let logical_height = (size.height as f64 / scale) as i32;

                x >= logical_x
                    && x < logical_x + logical_width
                    && y >= logical_y
                    && y < logical_y + logical_height
            }
        })
        .or_else(|| window.current_monitor().ok().flatten())
        .ok_or_else(|| AppError::from("No monitor found"))
}

/// Monitor that contains the given logical point.
fn monitor_at_logical(window: &WebviewWindow, x: f64, y: f64) -> Result<Monitor, AppError> {
    window
        .available_monitors()?
        .into_iter()
        .find(|m| {
            let scale = m.scale_factor();
            let pos = m.position();
            let size = m.size();
            let left = pos.x as f64 / scale;
            let top = pos.y as f64 / scale;
            let width = size.width as f64 / scale;
            let height = size.height as f64 / scale;

            x >= left && x < left + width && y >= top && y < top + height
        })
        .or_else(|| window.current_monitor().ok().flatten())
        .ok_or_else(|| AppError::from("No monitor found"))
}

/// Scale applied to the toolbar webview content relative to logical window pixels.
fn toolbar_content_scale() -> f64 {
    #[cfg(target_os = "windows")]
    {
        platform::get_text_scale_factor()
    }
    #[cfg(not(target_os = "windows"))]
    {
        1.0
    }
}

/// Safe area of a monitor, in logical pixels, for a window of the given size.
struct SafeArea {
    min_x: f64,
    max_x: f64,
    min_y: f64,
    max_y: f64,
}

/// Compute the logical safe area that keeps a window inside the monitor, leaving a small gap
/// at the screen edges and avoiding the bottom taskbar/dock area.
fn safe_area(window_width: f64, window_height: f64, monitor: &Monitor) -> SafeArea {
    let scale_factor = monitor.scale_factor();
    let monitor_size = monitor.size();
    let monitor_position = monitor.position();

    let margin = SCREEN_EDGE_MARGIN as f64;
    let safe_area_bottom = SAFE_AREA_BOTTOM as f64 / scale_factor;
    let screen_x = monitor_position.x as f64 / scale_factor;
    let screen_y = monitor_position.y as f64 / scale_factor;
    let screen_width = monitor_size.width as f64 / scale_factor;
    let screen_height = monitor_size.height as f64 / scale_factor;

    let min_x = screen_x + margin;
    let max_x = (screen_x + screen_width - window_width - margin).max(min_x);
    let min_y = screen_y + margin;
    let max_y = (screen_y + screen_height - window_height - safe_area_bottom - margin).max(min_y);

    SafeArea {
        min_x,
        max_x,
        min_y,
        max_y,
    }
}

/// Capture the logical screen rectangle of the last line of the current text selection.
///
/// Returns `(left, top, right, bottom)` and fails when the focused application exposes no
/// selection geometry, so callers can fall back to the cursor position.
fn capture_selection_anchor(window: &WebviewWindow) -> Result<LogicalRect, AppError> {
    let (left, top, right, bottom) = platform::get_selection_rect()?;

    #[cfg(target_os = "windows")]
    {
        // UI Automation reports physical pixels, normalize them with the monitor scale
        let scale_factor = monitor_at(window, right, bottom)?.scale_factor();

        Ok((
            left as f64 / scale_factor,
            top as f64 / scale_factor,
            right as f64 / scale_factor,
            bottom as f64 / scale_factor,
        ))
    }
    #[cfg(not(target_os = "windows"))]
    {
        let _ = window;

        Ok((left as f64, top as f64, right as f64, bottom as f64))
    }
}

/// Capture the logical cursor position, used when no selection geometry is available.
fn capture_cursor_anchor(window: &WebviewWindow) -> Result<LogicalPosition<f64>, AppError> {
    let (x, y) = ENIGO.lock()?.as_ref()?.location()?;

    #[cfg(target_os = "windows")]
    {
        // enigo reports physical pixels, normalize them with the monitor scale
        let scale_factor = monitor_at(window, x, y)?.scale_factor();

        Ok(LogicalPosition::new(
            x as f64 / scale_factor,
            y as f64 / scale_factor,
        ))
    }
    #[cfg(not(target_os = "windows"))]
    {
        let _ = window;

        Ok(LogicalPosition::new(x as f64, y as f64))
    }
}

/// Position the toolbar below the selected text.
///
/// The toolbar keeps `placement.gap` between its content and the selected text, so the text is
/// never covered, and is aligned horizontally so that the end of the selection sits at
/// `placement.anchor_percent` of its width. When the space below is insufficient the toolbar
/// flips above the selection instead.
fn position_toolbar_below_selection(
    window: &WebviewWindow,
    anchor: LogicalRect,
    placement: ToolbarPlacement,
    size: ToolbarSize,
    inset: f64,
) -> Result<(), AppError> {
    let (_, text_top, text_right, text_bottom) = anchor;

    // the anchor is logical, so resolve the monitor in logical space as well
    let monitor = monitor_at_logical(window, text_right, text_bottom)?;
    let area = safe_area(size.width, size.height, &monitor);

    // the window keeps a transparent inset around its content, so compensate it to leave the
    // configured visible gap between the toolbar content and the selected text
    let ratio = placement.anchor_percent / 100.0;

    // place the end of the selection at the configured position across the toolbar width
    let x = (text_right - size.width * ratio).clamp(area.min_x, area.max_x);

    // preferred placement: just below the selected text
    let below = text_bottom + placement.gap - inset;
    let y = if below <= area.max_y {
        below.clamp(area.min_y, area.max_y)
    } else {
        // no room below: place the toolbar above the selection instead of covering it
        let above = text_top - size.height + inset - placement.gap;
        if above >= area.min_y {
            above
        } else {
            below.clamp(area.min_y, area.max_y)
        }
    };

    window.set_position(Position::Logical(LogicalPosition { x, y }))?;

    Ok(())
}

/// Position the toolbar below the cursor.
///
/// Used when the focused application exposes no selection geometry, so the text line box is
/// unknown: the cursor is assumed to sit inside the selected line and the toolbar is dropped
/// `placement.line_offset` below it, aligned to the cursor like the selection case.
fn position_toolbar_near_cursor(
    window: &WebviewWindow,
    cursor: LogicalPosition<f64>,
    placement: ToolbarPlacement,
    size: ToolbarSize,
    inset: f64,
) -> Result<(), AppError> {
    let monitor = monitor_at_logical(window, cursor.x, cursor.y)?;
    let area = safe_area(size.width, size.height, &monitor);

    let ratio = placement.anchor_percent / 100.0;
    let x = (cursor.x - size.width * ratio).clamp(area.min_x, area.max_x);

    let below = cursor.y + placement.line_offset + placement.gap - inset;
    let y = if below <= area.max_y {
        below.clamp(area.min_y, area.max_y)
    } else {
        let above = cursor.y - placement.line_offset - size.height + inset - placement.gap;
        if above >= area.min_y {
            above
        } else {
            below.clamp(area.min_y, area.max_y)
        }
    };

    window.set_position(Position::Logical(LogicalPosition { x, y }))?;

    Ok(())
}

/// Position a window near the mouse or selection with safe area constraints.
fn position_window_near_cursor(
    window: &WebviewWindow,
    mouse: bool,
    size: ToolbarSize,
) -> Result<(), AppError> {
    // get cursor position (may be physical or logical depending on platform)
    let mut mouse_position = true;

    #[allow(unused_mut)]
    let (mut x, mut y) = if mouse {
        // directly use mouse position from enigo
        ENIGO.lock()?.as_ref()?.location()?
    } else {
        // try to get the end of the selection first, fall back to mouse position if failed
        match platform::get_selection_rect() {
            Ok((_, _, right, bottom)) => {
                mouse_position = false;
                (right, bottom)
            }
            Err(_) => ENIGO.lock()?.as_ref()?.location()?,
        }
    };

    // get monitor at the referenced point
    let monitor = monitor_at(window, x, y)?;

    let monitor_size = monitor.size();
    let monitor_position = monitor.position();
    let scale_factor = monitor.scale_factor();

    // convert physical pixels to logical pixels
    #[cfg(target_os = "windows")]
    {
        x = (x as f64 / scale_factor) as i32;
        y = (y as f64 / scale_factor) as i32;
    }

    let window_width = size.width as i32;
    let window_height = size.height as i32;
    let screen_width = (monitor_size.width as f64 / scale_factor) as i32;
    let screen_height = (monitor_size.height as f64 / scale_factor) as i32;
    let screen_x = (monitor_position.x as f64 / scale_factor) as i32;
    let screen_y = (monitor_position.y as f64 / scale_factor) as i32;

    // calculate safe area for window, keeping a small gap from the screen edges
    let safe_area_bottom = (SAFE_AREA_BOTTOM as f64 / scale_factor) as i32;
    let min_x = screen_x + SCREEN_EDGE_MARGIN;
    let max_x = (screen_x + screen_width - window_width - SCREEN_EDGE_MARGIN).max(min_x);
    let min_y = screen_y + SCREEN_EDGE_MARGIN;
    let max_y = (screen_y + screen_height - window_height - safe_area_bottom - SCREEN_EDGE_MARGIN)
        .max(min_y);

    // set adjusted window position
    let window_offset = if mouse_position {
        WINDOW_OFFSET
    } else {
        -WINDOW_OFFSET
    };
    // the fixed offset keeps the window clear of the reference point
    window.set_position(Position::Logical(LogicalPosition {
        x: (x + window_offset).clamp(min_x, max_x) as f64,
        y: (y + window_offset).clamp(min_y, max_y) as f64,
    }))?;

    Ok(())
}

/// Show and focus window.
pub fn show_window(app: &AppHandle, label: &str) -> Option<WebviewWindow> {
    if let Some(window) = app.get_webview_window(label) {
        if window.is_minimized().unwrap_or(false) {
            // unminimize
            let _ = window.unminimize();
        } else {
            // show window
            let _ = window.show();
        }
        // focus window
        let _ = window.set_focus();

        Some(window)
    } else {
        None
    }
}

/// Hide window.
pub fn hide_window(app: &AppHandle, label: &str) -> Option<WebviewWindow> {
    if let Some(window) = app.get_webview_window(label) {
        let _ = window.hide();

        // also hide dock icon on macOS
        #[cfg(target_os = "macos")]
        if label == "main" {
            let _ = app.set_dock_visibility(false);
        }

        Some(window)
    } else {
        None
    }
}

/// Toggle window visibility.
pub fn toggle_window(app: &AppHandle, label: &str) -> Option<WebviewWindow> {
    if let Some(window) = app.get_webview_window(label) {
        // check if window is minimized
        if window.is_minimized().unwrap_or(false) {
            let _ = window.unminimize();
            return Some(window);
        }

        // check if window is not visible
        if !window.is_visible().unwrap_or(false) {
            return show_window(app, label);
        }

        // check if window is not focused
        #[cfg(target_os = "macos")]
        if !window.is_focused().unwrap_or(false) {
            return show_window(app, label);
        }

        // hide when window is visible and not minimized
        hide_window(app, label)
    } else {
        None
    }
}
