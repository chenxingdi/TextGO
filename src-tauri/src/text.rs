//! Text normalization shared by document matching and clipboard matching.
//!
//! Selections arrive from the clipboard with line breaks, extracted pages carry line-end
//! hyphenation, and both differ in spacing. Comparing them requires removing the characters that
//! carry no meaning for a match: whitespace, soft hyphens, byte order marks and the noncharacters
//! PDF extraction leaves behind. Case is folded and ligatures are spelled out so a copied word and
//! its typeset original compare equal.
//!
//! The helpers live outside the document module because macOS needs the same comparison for
//! clipboard context, while document extraction is Windows-only.

/// Shortest selection worth searching for; below this a match says nothing about position.
pub(crate) const MIN_SELECTION_CHARS: usize = 4;

/// Most occurrences reported for one needle, so a pathological clipboard cannot stall a capture.
const MAX_OCCURRENCES: usize = 64;

/// Flatten a text fragment into a form that survives copying.
///
/// @param text - text to flatten
/// @returns whitespace-free, case-folded text
pub(crate) fn flat_text(text: &str) -> String {
    squash(text).0
}

/// Locate a flattened needle inside a text and return the byte offset of every occurrence.
///
/// The offsets are byte offsets on a character boundary of the original text, ready for slicing.
///
/// @param text - original text to search
/// @param needle - flattened needle, as returned by `flat_text`
/// @returns byte offsets of the beginnings of all occurrences, in order
pub(crate) fn locate_flat(text: &str, needle: &str) -> Vec<usize> {
    if needle.is_empty() {
        return Vec::new();
    }

    let (squashed, offsets) = squash(text);
    let mut found = Vec::new();
    let mut from = 0;
    // characters of the flattened text that precede `from`, so every match costs one short scan
    let mut char_index = 0;

    while let Some(position) = squashed[from..].find(needle) {
        let start = from + position;
        char_index += squashed[from..start].chars().count();
        if let Some(offset) = offsets.get(char_index) {
            found.push(*offset);
        }

        // a pathological clipboard (one repeated character) must not turn this into a long loop
        if found.len() >= MAX_OCCURRENCES {
            break;
        }

        from = start + needle.len();
        char_index += needle.chars().count();
    }

    found
}

/// Flatten text and keep the byte position of every retained character.
///
/// The offsets turn a match in the flattened text back into a slice of the original, which is what
/// an excerpt is cut from.
///
/// @param text - text to flatten
/// @returns flattened text and the original byte offset of each of its characters
pub(crate) fn squash(text: &str) -> (String, Vec<usize>) {
    let characters: Vec<(usize, char)> = text.char_indices().collect();
    let mut squashed = String::with_capacity(text.len());
    let mut offsets = Vec::with_capacity(text.len());

    for (index, (offset, character)) in characters.iter().enumerate() {
        // soft hyphens, byte order marks and the noncharacters PDF extraction leaves behind carry
        // no meaning for a comparison
        if character.is_whitespace()
            || matches!(
                *character,
                '\u{ad}' | '\u{feff}' | '\u{fffe}' | '\u{fffd}' | '\u{200b}'
            )
        {
            continue;
        }

        // a hyphen at the end of a line joins a word the typesetter had to split
        if *character == '-' {
            let next = characters.get(index + 1).map(|(_, next)| *next);
            if next.is_none_or(char::is_whitespace) {
                continue;
            }
        }

        for folded in character.to_lowercase() {
            if let Some(expansion) = expand_ligature(folded) {
                for expanded in expansion.chars() {
                    squashed.push(expanded);
                    offsets.push(*offset);
                }
            } else {
                squashed.push(folded);
                offsets.push(*offset);
            }
        }
    }

    (squashed, offsets)
}

/// Spell out a ligature so the copied and the typeset spelling compare equal.
///
/// @param character - character to expand
/// @returns ASCII expansion, or none when the character is not a ligature
fn expand_ligature(character: char) -> Option<&'static str> {
    match character {
        '\u{fb00}' => Some("ff"),
        '\u{fb01}' => Some("fi"),
        '\u{fb02}' => Some("fl"),
        '\u{fb03}' => Some("ffi"),
        '\u{fb04}' => Some("ffl"),
        '\u{fb05}' | '\u{fb06}' => Some("st"),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flattened_text_drops_spacing_and_line_end_hyphens() {
        assert_eq!(flat_text("com-\nmonly  used"), "commonlyused");
        assert_eq!(flat_text("well-known"), "well-known");
    }

    #[test]
    fn ligatures_are_spelled_out_before_comparing() {
        assert_eq!(flat_text("Flux \u{fb01}eld"), "fluxfield");
    }

    #[test]
    fn offsets_point_back_into_the_original_text() {
        let text = "A\n\nb";
        let (squashed, offsets) = squash(text);
        assert_eq!(squashed, "ab");
        assert_eq!(offsets.len(), squashed.chars().count());
        assert_eq!(&text[offsets[1]..offsets[1] + 1], "b");
    }

    #[test]
    fn located_offsets_slice_the_original_text() {
        let text = "第一段。\n\n目标词语出现在这里，后面还有内容。";
        let offsets = locate_flat(text, &flat_text("目标词语"));
        assert_eq!(offsets.len(), 1);
        assert!(text[offsets[0]..].starts_with("目标词语"));
    }

    #[test]
    fn every_occurrence_is_reported_in_order() {
        let text = "needle one, two, needle again, needle";
        let offsets = locate_flat(text, &flat_text("needle"));
        assert_eq!(offsets.len(), 3);
        assert!(offsets.windows(2).all(|pair| pair[0] < pair[1]));
    }

    #[test]
    fn match_across_a_hyphenated_line_break_maps_to_the_first_fragment() {
        // the flattened needle spans the line break, so the offset points at the hyphenated start
        let text = "the inter-\nnational standard";
        let offsets = locate_flat(text, &flat_text("international"));
        assert_eq!(offsets.len(), 1);
        assert!(text[offsets[0]..].starts_with("inter-"));
    }

    #[test]
    fn an_absent_needle_yields_no_offsets() {
        assert!(locate_flat("some text", &flat_text("nowhere")).is_empty());
        assert!(locate_flat("some text", "").is_empty());
    }
}
