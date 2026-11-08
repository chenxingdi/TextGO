#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "windows")]
mod windows;

#[cfg(target_os = "macos")]
pub use macos::{
    activate_focus_target, get_app_id, get_cursor_location, get_cursor_position, get_focus_target,
    get_frontmost_app_id, get_frontmost_url, get_selection, get_selection_context,
    get_selection_rect, is_cursor_editable, is_focus_password, is_focus_target_active,
    is_ibeam_cursor, select_backward_chars, FocusTarget,
};
#[cfg(target_os = "windows")]
pub use windows::{
    activate_focus_target, get_app_id, get_cursor_location, get_cursor_position, get_focus_target,
    get_frontmost_app_id, get_frontmost_url, get_selection, get_selection_context,
    get_selection_rect, get_text_scale_factor, is_cursor_editable, is_focus_password,
    is_focus_target_active, is_ibeam_cursor, select_backward_chars, FocusTarget,
};

/// Smallest selectable surrounding text limit.
pub const CONTEXT_TEXT_LIMIT_MIN: usize = 20;

/// Default surrounding text limit used when the setting is missing.
pub const CONTEXT_TEXT_LIMIT_DEFAULT: usize = 200;

/// Largest selectable surrounding text limit.
pub const CONTEXT_TEXT_LIMIT_MAX: usize = 200;

/// Rectangle covered by the current selection in physical screen pixels.
#[derive(Clone, Copy, Debug)]
pub struct SelectionRect {
    /// Left edge in screen coordinates.
    pub left: i32,
    /// Top edge in screen coordinates.
    pub top: i32,
    /// Rectangle width.
    pub width: i32,
    /// Rectangle height.
    pub height: i32,
}

/// Number of characters read on each side of the selection before clipping.
///
/// This is the floor used when the configured limit is at its minimum; larger limits read a
/// proportionally wider window, otherwise the limit would only ever trim text the platform had
/// already reported and raising it could not add any context.
pub const CONTEXT_WINDOW_RADIUS: usize = 900;

/// Characters read on each side of the selection for a given limit.
///
/// The window must be wider than the limit so the clipping step still has text to center on and
/// trim: reading exactly `limit` characters would leave a window that is off-center or truncated.
///
/// @param limit - maximum number of characters kept from the captured text
/// @returns radius in characters for the platform-specific text lookup
pub fn context_window_radius(limit: usize) -> usize {
    CONTEXT_WINDOW_RADIUS.max(limit)
}

/// Length accepted as an enclosing paragraph for a given limit.
///
/// A control that reports the whole document as its paragraph would otherwise hand back a wall of
/// text; anything at or above this cap is treated as ignored paragraph units.
///
/// @param limit - maximum number of characters kept from the captured text
/// @returns cap in characters
pub fn context_paragraph_cap(limit: usize) -> usize {
    (limit.saturating_mul(4)).max(CONTEXT_WINDOW_RADIUS * 2)
}

/// Text captured around a selection so the model can disambiguate wording.
#[derive(Clone, Debug, Default, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SelectionContext {
    /// Identifier of the application containing the selection.
    pub app_id: String,
    /// Title of the window that owned focus.
    pub title: String,
    /// Website URL when the source application is a browser.
    pub url: String,
    /// Text surrounding the selection, already clipped to the configured limit.
    pub paragraph: String,
}

/// Shrink the text around a selection to a bounded window centered on it.
///
/// Ellipses mark clipped ends, so the model can tell the paragraph is partial.
///
/// @param text - full surrounding text as reported by the platform
/// @param selection - selected text used to center the window
/// @param limit - maximum number of characters to keep
/// @returns clipped text without leading or trailing whitespace
pub fn clip_around_selection(text: &str, selection: &str, limit: usize) -> String {
    let trimmed = text.trim();
    let total = trimmed.chars().count();
    if total <= limit {
        return trimmed.to_string();
    }

    // center the window on the selection when it can be located in the captured text
    let needle = selection.trim();
    let offset = if needle.is_empty() {
        None
    } else {
        trimmed
            .find(needle)
            .map(|byte_index| trimmed[..byte_index].chars().count())
    };
    let (start, end) = match offset {
        Some(offset) => {
            let half = limit.saturating_sub(needle.chars().count()) / 2;
            let start = offset.saturating_sub(half);
            (start, (start + limit).min(total))
        }
        None => (0, limit.min(total)),
    };

    let mut clipped: String = trimmed.chars().skip(start).take(end - start).collect();
    if start > 0 {
        clipped.insert(0, '…');
    }
    if end < total {
        clipped.push('…');
    }
    clipped
}

/// Shrink the text around a selection to one paragraph and bound its length.
///
/// Platforms without a paragraph text unit report a character window instead, so the window is
/// trimmed back to blank-line boundaries before clipping.
///
/// @param text - text around the selection
/// @param selection - selected text used to locate the paragraph
/// @param limit - maximum number of characters to keep
/// @returns paragraph text around the selection without leading or trailing whitespace
pub fn paragraph_around(text: &str, selection: &str, limit: usize) -> String {
    let needle = selection.trim();
    let offset = if needle.is_empty() {
        0
    } else {
        text.find(needle).unwrap_or(0)
    };

    // blank lines separate paragraphs in every plain-text representation used here
    let start = text[..offset]
        .rfind("\n\n")
        .map(|index| index + 2)
        .or_else(|| text[..offset].rfind("\r\n\r\n").map(|index| index + 4))
        .unwrap_or(0);
    let end = text[offset..]
        .find("\n\n")
        .map(|index| offset + index)
        .or_else(|| text[offset..].find("\r\n\r\n").map(|index| offset + index))
        .unwrap_or(text.len());

    clip_around_selection(&text[start.min(end)..end], selection, limit)
}

/// Check whether an application identifier belongs to a web browser.
///
/// Address bar lookups are expensive, so the surrounding text capture only reads a URL from
/// applications that are known browsers.
///
/// @param app_id - executable path on Windows or bundle identifier on macOS
/// @returns whether the identifier looks like a browser
pub fn is_browser(app_id: &str) -> bool {
    const BROWSER_IDS: &[&str] = &[
        "chrome",
        "chromium",
        "msedge",
        "firefox",
        "brave",
        "opera",
        "vivaldi",
        "safari",
        "librewolf",
        "waterfox",
        // Arc and Zen expose their name inside the bundle identifier only
        "thebrowser",
        "zen-browser",
    ];

    // identifiers are split into path segments and bundle identifiers: "chrome.exe" becomes
    // "chrome" and "com.google.Chrome" keeps a "chrome" segment, so exact matches are enough
    app_id
        .to_lowercase()
        .split(['/', '\\', '.'])
        .any(|segment| BROWSER_IDS.contains(&segment))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_text_is_returned_without_ellipses() {
        let text = "  The container is running.  ";
        assert_eq!(
            clip_around_selection(text, "container", 100),
            "The container is running."
        );
    }

    #[test]
    fn long_text_is_centered_on_the_selection() {
        let text = format!("{}middle{}", "a".repeat(100), "b".repeat(100));
        let clipped = clip_around_selection(&text, "middle", 40);
        assert!(clipped.starts_with('…'));
        assert!(clipped.ends_with('…'));
        assert!(clipped.contains("middle"));
        assert_eq!(clipped.chars().count(), 42);
    }

    #[test]
    fn missing_selection_is_clipped_from_the_start() {
        let text = "x".repeat(100);
        assert_eq!(
            clip_around_selection(&text, "absent", 10),
            format!("{}…", "x".repeat(10))
        );
    }

    #[test]
    fn paragraph_is_trimmed_to_blank_line_boundaries() {
        let text = "First paragraph.\n\nThe container is ready.\n\nLast paragraph.";
        assert_eq!(
            paragraph_around(text, "container", 200),
            "The container is ready."
        );
    }

    #[test]
    fn paragraph_without_separators_keeps_the_whole_text() {
        let text = "no blank line here";
        assert_eq!(paragraph_around(text, "blank", 200), text);
    }

    #[test]
    fn multi_byte_text_is_clipped_on_character_boundaries() {
        let text = "中文".repeat(50);
        let clipped = clip_around_selection(&text, "中文中文", 11);
        assert!(clipped.chars().all(|c| c == '中' || c == '文' || c == '…'));
        assert_eq!(clipped.chars().count(), 12);
    }

    #[test]
    fn read_window_covers_the_configured_limit() {
        // the platform lookup stays at least as wide as the limit so clipping still has text on
        // both sides; with the current 20-200 range the fixed floor covers every limit
        for limit in [CONTEXT_TEXT_LIMIT_MIN, CONTEXT_TEXT_LIMIT_DEFAULT, CONTEXT_TEXT_LIMIT_MAX] {
            assert!(context_window_radius(limit) >= limit);
            assert!(context_window_radius(limit) >= CONTEXT_WINDOW_RADIUS);
        }
    }

    #[test]
    fn paragraph_cap_stays_above_the_limit() {
        for limit in [CONTEXT_TEXT_LIMIT_MIN, CONTEXT_TEXT_LIMIT_DEFAULT, CONTEXT_TEXT_LIMIT_MAX] {
            assert!(context_paragraph_cap(limit) > limit);
        }
    }

    #[test]
    fn browsers_are_recognized_across_platform_identifiers() {
        assert!(is_browser("C:\\Program Files\\Google\\Chrome\\chrome.exe"));
        assert!(is_browser("com.google.Chrome"));
        assert!(is_browser("org.mozilla.firefox"));
        assert!(!is_browser("C:\\Code\\VSCode\\Code.exe"));
        assert!(!is_browser("com.apple.TextEdit"));
    }
}
