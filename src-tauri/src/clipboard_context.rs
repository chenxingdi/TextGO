//! Recover surrounding text from a text the user copied shortly before.
//!
//! Applications that expose no accessibility text pattern (Qt readers, some PDF viewers) cannot
//! report the paragraph around a selection. The user often copied that paragraph themselves a few
//! seconds earlier, so the clipboard copy is the only available source of surrounding text. This
//! module turns such a copy into a bounded window around the selection; the freshness of the copy is
//! decided by [`crate::clipboard_watch`], not here.

use crate::platform::clip_at;
use crate::text::{flat_text, locate_flat, MIN_SELECTION_CHARS};

/// Extra characters a copy must carry beyond the selection to be worth using.
const MIN_GAIN: usize = 20;

/// Shortest copy that can still add context.
const MIN_COPIED_CHARS: usize = MIN_SELECTION_CHARS + MIN_GAIN;

/// Longest copy accepted; anything above this is a whole-document copy that costs more than it adds.
const MAX_COPIED_CHARS: usize = 64_000;

/// Find the selection inside a copied text and return a bounded window around it.
///
/// The comparison ignores whitespace, line-end hyphenation, ligatures and case, because a copy and a
/// selection of the same words differ in exactly those ways.
///
/// @param copied - text that was on the clipboard
/// @param selection - selected text
/// @param limit - maximum number of characters kept from the returned window
/// @returns surrounding text, or none when the copy cannot add context
pub(crate) fn paragraph_in(copied: &str, selection: &str, limit: usize) -> Option<String> {
    let copied = copied.trim();
    let copied_chars = copied.chars().count();
    if limit == 0 || copied_chars > MAX_COPIED_CHARS {
        return None;
    }

    let selection_chars = selection.trim().chars().count();
    if selection_chars < MIN_SELECTION_CHARS
        || copied_chars < MIN_COPIED_CHARS
        || copied_chars < selection_chars + MIN_GAIN
    {
        return None;
    }

    let needle = flat_text(selection);
    if needle.chars().count() < MIN_SELECTION_CHARS {
        return None;
    }

    // repeated words appear more than once in a paragraph; the window with room on both sides is
    // the one that carries context instead of a fragment clipped at the copy's edge
    let positions = locate_flat(copied, &needle);
    let best = positions.into_iter().max_by_key(|offset| {
        let before = copied
            .get(..*offset)
            .map(|head| head.chars().count())
            .unwrap_or(0);
        before.min(copied_chars.saturating_sub(before))
    })?;

    Some(clip_at(copied, best, selection_chars, limit))
}

#[cfg(test)]
mod tests {
    use super::*;

    const ARTICLE: &str = "Surface tension is the tendency of liquid surfaces to shrink into the \
                           minimum surface area.\r\n\r\nIt allows insects to float on water. \
                           Surface tension is caused by cohesion between molecules.";

    #[test]
    fn a_selection_inside_the_copy_returns_the_paragraph() {
        let context = paragraph_in(ARTICLE, "insects to float", 80).expect("context");
        assert!(context.contains("insects to float"), "{context}");
    }

    #[test]
    fn line_breaks_and_blank_lines_do_not_prevent_a_match() {
        let context = paragraph_in("first line\r\n\r\nsecond line with the needle", "second line", 60);
        assert!(context.is_some());
    }

    #[test]
    fn pdf_hyphenation_matches_without_the_hyphen() {
        let copied = "the inter-\nnational standards body";
        let context = paragraph_in(copied, "international", 60).expect("context");
        assert!(context.contains("inter-"), "{context}");
    }

    #[test]
    fn ligatures_and_invisible_characters_match() {
        let copied = "an e\u{fb03}cient method, with micro\u{200b}soft and soft\u{ad}hyphen inside";
        assert!(paragraph_in(copied, "efficient", 60).is_some());
        assert!(paragraph_in(copied, "microsoft", 60).is_some());
    }

    #[test]
    fn case_is_folded() {
        let copied = "Open the Chrome DevTools panel on the right";
        assert!(paragraph_in(copied, "chrome devtools", 60).is_some());
    }

    #[test]
    fn a_short_selection_is_never_matched() {
        assert!(paragraph_in(ARTICLE, "of", 80).is_none());
        assert!(paragraph_in(ARTICLE, "液态表", 80).is_none());
    }

    #[test]
    fn a_copy_that_barely_differs_from_the_selection_is_ignored() {
        let copied = "prefix target words here suffix";
        assert!(paragraph_in(copied, "target words here", 200).is_none());
    }

    #[test]
    fn a_selection_longer_than_the_copy_is_ignored() {
        assert!(paragraph_in("short copy text", "a much longer selection", 90).is_none());
    }

    #[test]
    fn a_copy_above_the_size_cap_is_ignored() {
        let copied = "x".repeat(MAX_COPIED_CHARS + 1);
        assert!(paragraph_in(&copied, "xxxx", 100).is_none());
    }

    #[test]
    fn the_centred_occurrence_is_chosen() {
        let filler = "filler ".repeat(30);
        let copied = format!("target words here {filler}target words here {}", "tail ".repeat(30));
        let context = paragraph_in(&copied, "target words here", 100).expect("context");
        assert!(context.starts_with('…'), "{context}");
        assert!(context.ends_with('…'), "{context}");
    }

    #[test]
    fn the_window_never_exceeds_the_limit() {
        for limit in [20, 60, 200] {
            let context = paragraph_in(ARTICLE, "Surface tension is the tendency", limit)
                .expect("context");
            assert!(context.chars().count() <= limit + 2, "limit {limit}: {context}");
        }
    }

    #[test]
    fn a_missing_selection_yields_nothing() {
        assert!(paragraph_in(ARTICLE, "completely absent words", 80).is_none());
        assert!(paragraph_in(ARTICLE, "Surface tension", 0).is_none());
    }
}
