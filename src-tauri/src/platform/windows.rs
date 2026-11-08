use crate::document;
use crate::error::AppError;
use crate::platform::{
    clip_around_selection, context_paragraph_cap, context_window_radius, is_browser, paragraph_around,
    SelectionContext,
};
use log::info;
use std::fs;
use std::path::Path;
use std::sync::mpsc;
use std::time::Duration;
use windows::core::{Interface, Owned, PWSTR};
use windows::Win32::Foundation::{HWND, MAX_PATH};
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoUninitialize, CLSCTX_ALL, COINIT_APARTMENTTHREADED,
};
use windows::Win32::System::DataExchange::GetClipboardSequenceNumber;
use windows::Win32::System::Threading::{
    OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows::Win32::UI::Accessibility::{
    CUIAutomation, IUIAutomation, IUIAutomationElement, IUIAutomationLegacyIAccessiblePattern,
    IUIAutomationTextPattern, IUIAutomationTextRange, IUIAutomationValuePattern,
    TextPatternRangeEndpoint_End, TextPatternRangeEndpoint_Start, TextUnit_Character,
    TextUnit_Paragraph, TreeScope_Descendants, UIA_ControlTypePropertyId,
    UIA_DocumentControlTypeId, UIA_EditControlTypeId, UIA_LegacyIAccessiblePatternId,
    UIA_TextControlTypeId, UIA_TextPatternId, UIA_ValuePatternId,
};
use windows::Win32::UI::WindowsAndMessaging::{
    GetCursorInfo, GetForegroundWindow, GetWindowTextLengthW, GetWindowTextW,
    GetWindowThreadProcessId, LoadCursorW, SetForegroundWindow, CURSORINFO, CURSOR_SHOWING,
    IDC_IBEAM,
};
use windows::UI::ViewManagement::UISettings;

/// Native identifier for the window that owned focus before the popup opened.
pub type FocusTarget = isize;

/// Get the Windows accessibility text scale applied by WebView2.
pub fn get_text_scale_factor() -> f64 {
    UISettings::new()
        .and_then(|settings| settings.TextScaleFactor())
        .ok()
        .filter(|scale| scale.is_finite() && *scale > 0.0)
        .unwrap_or(1.0)
}

// bounds validation constants
const MIN_VALID_WIDTH: f64 = 1.0;
const MAX_VALID_HEIGHT: f64 = 100.0;
const MAX_VALID_COORDINATE: f64 = 10000.0;

// editable legacy control roles
const ROLE_SYSTEM_TEXT: u32 = 42;
const ROLE_SYSTEM_COMBOBOX: u32 = 46;

// text controls inspected when no text pattern carries the selection
const MAX_TEXT_NODES: i32 = 1200;

// how long the text control search may take before the capture gives up on it
const TEXT_NODE_BUDGET: Duration = Duration::from_millis(400);

// text controls joined around the matching one
const TEXT_NODE_NEIGHBOURS: usize = 2;

// import SafeArray functions from oleaut32.dll
#[link(name = "oleaut32")]
unsafe extern "system" {
    unsafe fn SafeArrayAccessData(
        psa: *mut std::ffi::c_void,
        ppv_data: *mut *mut std::ffi::c_void,
    ) -> i32;

    unsafe fn SafeArrayUnaccessData(psa: *mut std::ffi::c_void) -> i32;

    unsafe fn SafeArrayGetLBound(
        psa: *mut std::ffi::c_void,
        n_dim: u32,
        pl_lbound: *mut i32,
    ) -> i32;

    unsafe fn SafeArrayGetUBound(
        psa: *mut std::ffi::c_void,
        n_dim: u32,
        pl_ubound: *mut i32,
    ) -> i32;
}

/// COM resource guard.
struct ComGuard {
    initialized: bool,
}

/// Capture the window that currently owns focus.
pub fn get_focus_target() -> Option<FocusTarget> {
    unsafe {
        let window = GetForegroundWindow();
        (!window.is_invalid()).then_some(window.0 as isize)
    }
}

/// Activate the window that owned focus before the popup opened.
pub fn activate_focus_target(target: FocusTarget) -> Result<(), AppError> {
    unsafe {
        let window = HWND(target as *mut _);
        if window.is_invalid() || !SetForegroundWindow(window).as_bool() {
            return Err("Failed to activate popup source window".into());
        }
        Ok(())
    }
}

/// Check whether the popup source window currently owns focus.
pub fn is_focus_target_active(target: FocusTarget) -> bool {
    unsafe { GetForegroundWindow().0 as isize == target }
}

impl ComGuard {
    /// Initialize COM environment.
    fn new() -> Result<Self, AppError> {
        unsafe {
            let result = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
            if result.is_ok() {
                // successfully initialized, need to release on drop
                Ok(ComGuard { initialized: true })
            } else if result == windows::Win32::Foundation::RPC_E_CHANGED_MODE {
                // COM already initialized by other code, no need to release
                Ok(ComGuard { initialized: false })
            } else {
                // other errors
                Err("Failed to initialize COM".into())
            }
        }
    }
}

impl Drop for ComGuard {
    /// Release COM resources.
    fn drop(&mut self) {
        if self.initialized {
            unsafe {
                CoUninitialize();
            }
        }
    }
}

/// Get currently focused UI element.
fn get_focused_element() -> Result<IUIAutomationElement, AppError> {
    unsafe {
        // create UI Automation instance
        let automation: IUIAutomation = CoCreateInstance(&CUIAutomation, None, CLSCTX_ALL)
            .map_err(|e| format!("Failed to create UI Automation instance: {}", e))?;

        // get focused element
        automation
            .GetFocusedElement()
            .map_err(|e| format!("Failed to get focused element: {}", e).into())
    }
}

/// Get first selected text range from given element.
fn get_selected_range(element: &IUIAutomationElement) -> Result<IUIAutomationTextRange, AppError> {
    unsafe {
        // get text pattern from element
        let text_pattern: IUIAutomationTextPattern = element
            .GetCurrentPattern(UIA_TextPatternId)
            .and_then(|p| p.cast())
            .map_err(|_| "Failed to get text pattern")?;

        // get currently selected text ranges
        let text_ranges = text_pattern
            .GetSelection()
            .map_err(|_| "Failed to get text selection")?;

        if text_ranges.Length().unwrap_or(0) == 0 {
            return Err("No text selection found".into());
        }

        // get first selection range
        text_ranges
            .GetElement(0)
            .map_err(|_| "Failed to get first selection range".into())
    }
}

/// Clipboard change counter.
///
/// Reading it never opens the clipboard, so the change monitor can poll it without disturbing
/// another application that is still publishing a copy.
///
/// @returns monotonically increasing counter value
pub fn clipboard_change_count() -> Option<u64> {
    Some(unsafe { GetClipboardSequenceNumber() } as u64)
}

/// Get selected text in currently focused element.
pub fn get_selection() -> Result<String, AppError> {
    unsafe {
        // initialize COM
        let _com = ComGuard::new()?;

        // get focused element
        let focused_element = get_focused_element()?;

        // get first selected text range
        let text_range = get_selected_range(&focused_element)?;

        // extract text from range
        let text = text_range
            .GetText(-1)
            .map_err(|_| "Failed to get text from selection")?;

        Ok(text.to_string())
    }
}

/// Capture the title, identifier, website and surrounding text of the current selection.
///
/// @param limit - maximum number of characters kept from the text around the selection
/// @param selection - selected text, used to locate it when the accessibility tree has no selection
/// @returns best-effort context; parts that cannot be read stay empty
pub fn get_selection_context(limit: usize, selection: &str) -> Result<SelectionContext, AppError> {
    unsafe {
        // initialize COM
        let _com = ComGuard::new()?;

        let mut context = SelectionContext {
            app_id: get_frontmost_app_id().unwrap_or_default(),
            ..SelectionContext::default()
        };

        // window title of the foreground window
        let window = GetForegroundWindow();
        if !window.is_invalid() {
            context.title = get_window_title(window).unwrap_or_default();
        }

        // text around the selection, read through the accessibility text pattern
        context.paragraph = read_selected_paragraph(limit).unwrap_or_default();

        let selection = selection.trim();

        // readers expose neither a text pattern nor a selection, so the text is recovered from the
        // paragraph text controls they publish, or from the document they have open
        if context.paragraph.trim().is_empty() && !selection.is_empty() && !is_browser(&context.app_id)
        {
            context.paragraph = text_node_context(selection, limit)
                .or_else(|| document_context(&context.title, selection, limit))
                .unwrap_or_default();
        }

        // the address bar lookup is only meaningful in browsers and is comparatively slow
        if is_browser(&context.app_id) {
            context.url = get_frontmost_url().unwrap_or_default();
        }

        Ok(context)
    }
}

/// Read the text around a selection from the text controls below the foreground window.
///
/// Reader applications split a page into one text control per paragraph and publish those names
/// instead of a text pattern, so the paragraph is found by its text: the control whose name carries
/// the selection is the paragraph, and the controls around it widen the context.
///
/// @param selection - selected text, already read through the clipboard
/// @param limit - maximum number of characters kept from the resulting text
/// @returns text around the selection, or none when no control carries the selection
fn text_node_context(selection: &str, limit: usize) -> Option<String> {
    let needle = document::flat_text(selection);
    if needle.chars().count() < document::MIN_SELECTION_CHARS {
        return None;
    }

    let window = unsafe { GetForegroundWindow() };
    if window.is_invalid() {
        return None;
    }

    // The walk runs on its own thread. UIA answers arrive from another process and a single call
    // can block for seconds when that process does not answer, so the popup waits a bounded time
    // and gives up; the window handle is captured first because the popup takes focus afterwards.
    let (sender, receiver) = mpsc::channel();
    let handle = window.0 as isize;
    std::thread::spawn(move || {
        let names = text_control_names(HWND(handle as *mut _));
        let _ = sender.send(names);
    });

    let names = receiver.recv_timeout(TEXT_NODE_BUDGET).ok()?;
    let matched = names
        .iter()
        .position(|name| document::flat_text(name).contains(&needle))?;
    let start = matched.saturating_sub(TEXT_NODE_NEIGHBOURS);
    let end = (matched + TEXT_NODE_NEIGHBOURS + 1).min(names.len());
    let joined = names[start..end].join("\n\n");
    let paragraph = clip_around_selection(&joined, &names[matched], limit);

    info!(
        "Recovered context from {} of {} text controls",
        end - start,
        names.len()
    );
    (!paragraph.trim().is_empty()).then_some(paragraph)
}

/// Collect the names of the text controls below a window.
///
/// Paragraph text controls report their content as the element name; layout spacers report a
/// non-breaking space and are dropped.
///
/// @param window - window whose accessibility tree is read
/// @returns trimmed element names in tree order, empty when the tree cannot be read
fn text_control_names(window: HWND) -> Vec<String> {
    unsafe {
        let Ok(_com) = ComGuard::new() else {
            return Vec::new();
        };

        let automation: IUIAutomation = match CoCreateInstance(&CUIAutomation, None, CLSCTX_ALL) {
            Ok(automation) => automation,
            Err(_) => return Vec::new(),
        };

        let root = match automation.ElementFromHandle(window) {
            Ok(root) => root,
            Err(_) => return Vec::new(),
        };

        let condition = match automation
            .CreatePropertyCondition(UIA_ControlTypePropertyId, &UIA_TextControlTypeId.0.into())
        {
            Ok(condition) => condition,
            Err(_) => return Vec::new(),
        };

        let elements = match root.FindAll(TreeScope_Descendants, &condition) {
            Ok(elements) => elements,
            Err(_) => return Vec::new(),
        };

        let count = elements.Length().unwrap_or(0).min(MAX_TEXT_NODES);
        let mut names: Vec<String> = Vec::new();

        for index in 0..count {
            let Ok(element) = elements.GetElement(index) else {
                continue;
            };
            let Ok(name) = element.CurrentName() else {
                continue;
            };

            let name = name.to_string();
            let name = name.trim();
            if name.is_empty() || name == "\u{a0}" {
                continue;
            }
            names.push(name.to_string());
        }

        names
    }
}

/// Read the text around a selection out of the document the foreground window shows.
///
/// @param title - caption of the foreground window
/// @param selection - selected text
/// @param limit - maximum number of characters kept from the resulting text
/// @returns text around the selection, or none when no document is named or readable
fn document_context(title: &str, selection: &str, limit: usize) -> Option<String> {
    if title.is_empty() {
        return None;
    }

    let context = document::context_for(title, selection, limit);
    if context.is_some() {
        info!("Recovered context from the document named by the window title");
    }
    context
}

/// Read the paragraph around the selection of the focused element.
///
/// @param limit - maximum number of characters kept from the resulting text
/// @returns clipped text around the selection, or none when no text selection exists
fn read_selected_paragraph(limit: usize) -> Option<String> {
    let element = get_focused_element().ok()?;
    let text_range = get_selected_range(&element).ok()?;
    read_around_range(&text_range, limit)
}

/// Read the window title of a window handle.
///
/// @param window - window whose caption should be read
/// @returns trimmed caption, or none when the window has no title
fn get_window_title(window: HWND) -> Option<String> {
    unsafe {
        let length = GetWindowTextLengthW(window);
        if length <= 0 {
            return None;
        }

        let mut buffer = vec![0u16; length as usize + 1];
        let written = GetWindowTextW(window, &mut buffer);
        if written <= 0 {
            return None;
        }

        let title = String::from_utf16_lossy(&buffer[..written as usize]);
        let title = title.trim();
        if title.is_empty() {
            None
        } else {
            Some(title.to_string())
        }
    }
}

/// Read the text around a selection range.
///
/// The enclosing paragraph is preferred when it is long enough to fill the configured limit,
/// because it carries the sentence-level cues that resolve ambiguous wording. Short paragraphs -
/// a list entry, a headline, a single short sentence - are widened to a character window so that
/// raising the limit actually reads more surrounding text instead of re-reading the same line.
/// Controls that report the whole document as their paragraph are always dropped in favor of the
/// bounded window.
///
/// @param text_range - first selected range of the focused element
/// @param limit - maximum number of characters kept from the resulting text
/// @returns clipped text around the selection, or none when the control has no text
fn read_around_range(text_range: &IUIAutomationTextRange, limit: usize) -> Option<String> {
    unsafe {
        let selection = text_range.GetText(-1).ok()?.to_string();
        let radius = context_window_radius(limit);

        // a paragraph wider than the sanity cap means the control ignored the unit
        if let Some(text) = enclosing_paragraph(text_range, context_paragraph_cap(limit), &selection)
        {
            let paragraph = clip_around_selection(&text, &selection, limit);
            let wide_enough = paragraph.chars().count() >= limit.saturating_sub(2);
            if wide_enough {
                return Some(paragraph);
            }

            // the paragraph is shorter than the requested limit: widen to a character window and
            // keep whichever reaches further, so a small limit still favours the tidy paragraph
            if let Some(wider) = char_window(text_range, radius, &selection, limit) {
                return Some(if wider.chars().count() > paragraph.chars().count() {
                    wider
                } else {
                    paragraph
                });
            }
            return Some(paragraph);
        }

        char_window(text_range, radius, &selection, limit)
    }
}

/// Read a fixed character window on each side of the selection and trim it to paragraph bounds.
///
/// @param text_range - first selected range of the focused element
/// @param radius - characters read on each side of the selection
/// @param selection - selected text, used to locate the paragraph inside the window
/// @param limit - maximum number of characters kept from the resulting text
/// @returns clipped text around the selection, or none when the window is empty
fn char_window(
    text_range: &IUIAutomationTextRange,
    radius: usize,
    selection: &str,
    limit: usize,
) -> Option<String> {
    unsafe {
        let window = text_range.Clone().ok()?;
        window
            .MoveEndpointByUnit(
                TextPatternRangeEndpoint_Start,
                TextUnit_Character,
                -(radius as i32),
            )
            .ok()?;
        window
            .MoveEndpointByUnit(
                TextPatternRangeEndpoint_End,
                TextUnit_Character,
                radius as i32,
            )
            .ok()?;

        let text = window.GetText(-1).ok()?.to_string();
        if text.trim().is_empty() {
            None
        } else {
            Some(paragraph_around(&text, selection, limit))
        }
    }
}

/// Read the enclosing paragraph of a text range.
///
/// @param text_range - selected range
/// @param cap - maximum length accepted as a paragraph
/// @param selection - selected text that the paragraph must contain
/// @returns paragraph text, or none when the control ignored the paragraph unit
fn enclosing_paragraph(
    text_range: &IUIAutomationTextRange,
    cap: usize,
    selection: &str,
) -> Option<String> {
    unsafe {
        let paragraph = text_range.Clone().ok()?;
        paragraph.ExpandToEnclosingUnit(TextUnit_Paragraph).ok()?;
        let text = paragraph.GetText(cap as i32).ok()?.to_string();
        (text.chars().count() < cap && !text.trim().is_empty() && text.contains(selection.trim()))
            .then_some(text)
    }
}

/// Get the coordinates of the bottom-right corner of the selected text.
pub fn get_cursor_location() -> Result<(i32, i32), AppError> {
    unsafe {
        // initialize COM
        let _com = ComGuard::new()?;

        // get focused element
        let focused_element = get_focused_element()?;

        // get first selected text range
        let text_range = get_selected_range(&focused_element)?;

        // get bounding rectangles for the text range
        let rect_array = text_range
            .GetBoundingRectangles()
            .map_err(|_| "Failed to get bounding rectangles")?;

        // access the SafeArray data
        let mut rect_ptr: *mut f64 = std::ptr::null_mut();
        let hr = SafeArrayAccessData(rect_array as *mut _, &mut rect_ptr as *mut _ as *mut _);
        if hr != 0 {
            return Err("Failed to access SafeArray data".into());
        }

        // get array bounds
        let mut lower_bound: i32 = 0;
        let mut upper_bound: i32 = 0;
        let hr = SafeArrayGetLBound(rect_array as *mut _, 1, &mut lower_bound);
        if hr != 0 {
            SafeArrayUnaccessData(rect_array as *mut _);
            return Err("Failed to get lower bound".into());
        }
        let hr = SafeArrayGetUBound(rect_array as *mut _, 1, &mut upper_bound);
        if hr != 0 {
            SafeArrayUnaccessData(rect_array as *mut _);
            return Err("Failed to get upper bound".into());
        }

        let rect_count = ((upper_bound - lower_bound + 1) / 4) as usize;
        if rect_count == 0 {
            SafeArrayUnaccessData(rect_array as *mut _);
            return Err("No bounding rectangles found".into());
        }

        // find last valid rectangle and calculate coordinates
        let mut result = Err("No valid rectangle found".into());
        for i in (0..rect_count).rev() {
            let rect_index = i * 4;
            let left = *rect_ptr.add(rect_index);
            let top = *rect_ptr.add(rect_index + 1);
            let width = *rect_ptr.add(rect_index + 2);
            let height = *rect_ptr.add(rect_index + 3);

            // validate rectangle bounds
            if width > MIN_VALID_WIDTH
                && height > 0.0
                && height < MAX_VALID_HEIGHT
                && left >= 0.0
                && top >= 0.0
                && left < MAX_VALID_COORDINATE
                && top < MAX_VALID_COORDINATE
            {
                // calculate bottom-right corner coordinates
                let bottom_right_x = (left + width) as i32;
                let bottom_right_y = (top + height) as i32;
                result = Ok((bottom_right_x, bottom_right_y));
                break;
            }
        }

        // unaccess the SafeArray data
        SafeArrayUnaccessData(rect_array as *mut _);

        result
    }
}

/// Check if currently focused element is editable.
pub fn is_cursor_editable() -> Result<bool, AppError> {
    unsafe {
        // initialize COM
        let _com = ComGuard::new()?;

        // get focused element
        let focused_element = get_focused_element()?;

        // Strategy 1: check if control type is editable
        if let Ok(control_type) = focused_element.CurrentControlType() {
            let is_edit_control = control_type.0 == UIA_EditControlTypeId.0;
            let is_document_control = control_type.0 == UIA_DocumentControlTypeId.0;
            if is_edit_control || is_document_control {
                return Ok(true);
            }
        }

        // Strategy 2: check if value pattern is not read-only
        if let Ok(is_readonly) = focused_element
            .GetCurrentPattern(UIA_ValuePatternId)
            .and_then(|p| p.cast::<IUIAutomationValuePattern>())
            .and_then(|vp| vp.CurrentIsReadOnly())
        {
            if !is_readonly.as_bool() {
                return Ok(true);
            }
        }

        // Strategy 3: check if legacy control role is editable
        if let Ok(role) = focused_element
            .GetCurrentPattern(UIA_LegacyIAccessiblePatternId)
            .and_then(|p| p.cast::<IUIAutomationLegacyIAccessiblePattern>())
            .and_then(|lp| lp.CurrentRole())
        {
            if role == ROLE_SYSTEM_TEXT || role == ROLE_SYSTEM_COMBOBOX {
                return Ok(true);
            }
        }

        Ok(false)
    }
}

/// Check if current cursor is I-Beam (text cursor).
pub fn is_ibeam_cursor() -> bool {
    unsafe {
        let mut cursor_info = CURSORINFO {
            cbSize: std::mem::size_of::<CURSORINFO>() as u32,
            flags: Default::default(),
            hCursor: Default::default(),
            ptScreenPos: Default::default(),
        };

        // get current cursor information
        if GetCursorInfo(&mut cursor_info).is_err() {
            return false;
        }

        // check if cursor is showing
        if cursor_info.flags != CURSOR_SHOWING {
            return false;
        }

        // load the system I-Beam cursor to get its handle
        let Ok(ibeam_cursor) = LoadCursorW(None, IDC_IBEAM) else {
            return false;
        };

        // compare cursor handles
        cursor_info.hCursor == ibeam_cursor
    }
}

/// Select specified number of characters from current cursor position backward.
pub fn select_backward_chars(chars: usize) -> Result<(), AppError> {
    unsafe {
        // initialize COM
        let _com = ComGuard::new()?;

        // get focused element
        let focused_element = get_focused_element()?;

        // get first selected text range
        let text_range = get_selected_range(&focused_element)?;

        // move endpoint backward
        text_range
            .MoveEndpointByUnit(
                TextPatternRangeEndpoint_Start,
                TextUnit_Character,
                -(chars as i32),
            )
            .map_err(|_| "Failed to move endpoint backward")?;

        // select new range
        text_range
            .Select()
            .map_err(|_| "Failed to select new range")?;

        Ok(())
    }
}

/// Get application identifier from an application path.
pub fn get_app_id(app_path: &Path) -> Result<String, AppError> {
    // canonicalize the application path
    if let Ok(canonical_path) = fs::canonicalize(app_path) {
        if let Some(normalized_path) = canonical_path.to_str() {
            // remove the "\\?\" prefix that Windows adds for long paths
            return Ok(normalized_path.trim_start_matches(r"\\?\").to_string());
        }
    }

    Err("Failed to get application identifier".into())
}

/// Get the executable path of the frontmost application.
pub fn get_frontmost_app_id() -> Option<String> {
    unsafe {
        // get foreground window
        let hwnd = GetForegroundWindow();
        if hwnd.is_invalid() {
            return None;
        }

        // get process ID
        let mut pid: u32 = 0;
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
        if pid == 0 {
            return None;
        }

        // open process with query information permission
        let process_handle =
            Owned::new(OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?);

        // get process executable path
        let mut buffer = vec![0u16; MAX_PATH as usize];
        let mut size = buffer.len() as u32;
        let result = QueryFullProcessImageNameW(
            *process_handle,
            PROCESS_NAME_WIN32,
            PWSTR(buffer.as_mut_ptr()),
            &mut size,
        );
        if result.is_err() || size == 0 {
            return None;
        }

        // convert to string
        String::from_utf16(&buffer[..size as usize]).ok()
    }
}

/// Get the current website URL from the frontmost browser.
pub fn get_frontmost_url() -> Option<String> {
    unsafe {
        // initialize COM
        let _com = ComGuard::new().ok()?;

        // get foreground window
        let hwnd = GetForegroundWindow();
        if hwnd.is_invalid() {
            return None;
        }

        // create UI Automation instance
        let automation: IUIAutomation = CoCreateInstance(&CUIAutomation, None, CLSCTX_ALL).ok()?;

        // get root UI element from window handle
        let element = automation.ElementFromHandle(hwnd).ok()?;

        // Strategy 1: try to find first edit control
        if let Some(url) = find_url_in_element(&element, &automation, UIA_EditControlTypeId.0) {
            return Some(url);
        }

        // Strategy 2: try to find first document control
        if let Some(url) = find_url_in_element(&element, &automation, UIA_DocumentControlTypeId.0) {
            return Some(url);
        }

        None
    }
}

/// Try to extract URL from element with specified control type.
unsafe fn find_url_in_element(
    root_element: &IUIAutomationElement,
    automation: &IUIAutomation,
    control_type_id: i32,
) -> Option<String> {
    // create property condition for specified control type
    let condition = automation
        .CreatePropertyCondition(UIA_ControlTypePropertyId, &control_type_id.into())
        .ok()?;

    // find element with specified control type
    let element = root_element
        .FindFirst(TreeScope_Descendants, &condition)
        .ok()?;

    // get value pattern from element
    let value_pattern = element
        .GetCurrentPattern(UIA_ValuePatternId)
        .and_then(|p| p.cast::<IUIAutomationValuePattern>())
        .ok()?;

    // extract value
    let value = value_pattern.CurrentValue().ok()?;

    // validate URL format
    let url = value.to_string();
    if !url.is_empty() && (url.starts_with("http://") || url.starts_with("https://")) {
        Some(url)
    } else {
        None
    }
}
