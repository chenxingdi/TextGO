//! Recover the surrounding text of a selection from the document a reader has open.
//!
//! Readers such as SumatraPDF paint their pages themselves: the window exposes no accessibility
//! text at all, so neither the selection nor its paragraph can be read through UI Automation.
//! What the window does carry is the document name in its caption, so the text is taken from the
//! file instead: the open document is resolved from the window title and from the recent-file index
//! the reader keeps, the selection is located inside the extracted text, and the text around it is
//! returned as context.

use log::debug;
use std::fs;
use std::io::Read;
use std::panic::AssertUnwindSafe;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, LazyLock, Mutex};
use std::time::Duration;

/// File extensions whose text can be read back.
const DOCUMENT_EXTENSIONS: [&str; 2] = [".pdf", ".epub"];

/// Segment separator readers put between the document name and their own title segments.
const TITLE_SEPARATOR: &str = " - ";

/// Documents above this size are skipped: reading them would occupy the machine for too long.
const MAX_DOCUMENT_BYTES: u64 = 160 * 1024 * 1024;

/// Characters kept from one extracted document.
const MAX_DOCUMENT_CHARS: usize = 8 * 1024 * 1024;

/// Shortest selection worth searching for; below this a match says nothing about position.
pub(crate) const MIN_SELECTION_CHARS: usize = 4;

/// Longest prefix of the selection used for locating it.
///
/// A two-column selection comes out of the clipboard in a different order than the page reads it,
/// so only the head of the selection is trusted to be contiguous.
const MAX_NEEDLE_CHARS: usize = 40;

/// How long the selection path waits for an extraction before continuing without it.
///
/// A whole book takes longer than a popup may take, so a slow extraction finishes in the
/// background and only the next selection in the same document profits from it.
const INLINE_BUDGET: Duration = Duration::from_millis(600);

/// Reader settings files that list recently opened documents, relative to an environment variable.
const RECENT_INDEXES: [(&str, &str); 1] = [("LOCALAPPDATA", r"SumatraPDF\SumatraPDF-settings.txt")];

/// Line feed, the break extracted PDF text and copied text both use.
const LINE_BREAK: char = '\u{a}';

/// Carriage return, which may precede a line feed.
const CARRIAGE_RETURN: char = '\u{d}';

/// Text of a document, one entry per page for PDF files.
struct CachedDocument {
    path: PathBuf,
    pages: Arc<Vec<String>>,
}

/// Last extracted document, reused by later selections inside the same file.
static DOCUMENT_CACHE: LazyLock<Mutex<Option<CachedDocument>>> = LazyLock::new(|| Mutex::new(None));

/// Whether a full extraction is already running in the background.
static EXTRACTION_RUNNING: AtomicBool = AtomicBool::new(false);

/// Open document resolved from a window title.
#[derive(Debug, Clone)]
pub struct DocumentRef {
    path: PathBuf,
    page: Option<u32>,
}

/// Read the text around a selection out of the document a window title names.
///
/// @param title - caption of the window that owns the selection
/// @param selection - selected text, used to locate the position inside the document
/// @param limit - maximum number of characters kept from the returned text
/// @returns surrounding text, or none when no document was resolved or the selection is absent
pub fn context_for(title: &str, selection: &str, limit: usize) -> Option<String> {
    if limit == 0 || flat_text(selection).chars().count() < MIN_SELECTION_CHARS {
        return None;
    }

    let document = resolve(title)?;

    // an already extracted document answers immediately
    if let Some(pages) = cached_pages(&document.path) {
        return locate_in_pages(&pages, selection, document.page, limit);
    }

    // the page the reader displays holds the selection in almost every case, and reading a single
    // page is cheap enough to stay inside the popup latency
    if let Some(page) = document.page {
        let page_context = page_text(&document.path, page)
            .as_deref()
            .and_then(|text| locate_in_text(text, selection, limit));
        if let Some(context) = page_context {
            debug!("Recovered context from page {} of {:?}", page, document.path);
            return Some(context);
        }
    }

    // otherwise extract the whole document, which also seeds the cache for later selections
    extract_with_budget(&document.path)
        .and_then(|pages| locate_in_pages(&pages, selection, document.page, limit))
}

/// Flatten a text fragment into a form that survives copying.
///
/// Selections arrive from the clipboard with line breaks, extracted pages carry line-end
/// hyphenation, and both differ in spacing. Whitespace, hyphenation and unsupported glyphs are
/// removed and the case is folded, so a copied word and its typeset original compare equal.
///
/// @param text - text to flatten
/// @returns whitespace-free, case-folded text
pub(crate) fn flat_text(text: &str) -> String {
    squash(text).0
}

/// Flatten text and keep the byte position of every retained character.
///
/// The offsets turn a match in the flattened text back into a slice of the original, which is what
/// the excerpt is cut from.
///
/// @param text - text to flatten
/// @returns flattened text and the original byte offset of each of its characters
fn squash(text: &str) -> (String, Vec<usize>) {
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

/// Find the selection inside extracted text and return the text around it.
///
/// @param text - extracted text of one page or chapter
/// @param selection - selected text
/// @param limit - maximum number of characters kept from the returned text
/// @returns surrounding text, or none when the selection is absent
fn locate_in_text(text: &str, selection: &str, limit: usize) -> Option<String> {
    let (needle, _) = squash(selection);
    let prefix: String = needle.chars().take(MAX_NEEDLE_CHARS).collect();
    let needle_length = prefix.chars().count();
    if needle_length < MIN_SELECTION_CHARS {
        return None;
    }

    let (haystack, offsets) = squash(text);
    let position = haystack.find(&prefix)?;

    let start_char = haystack[..position].chars().count();
    let start_byte = *offsets.get(start_char)?;
    let end_char = (start_char + needle_length).min(offsets.len().saturating_sub(1));
    let end_byte = (*offsets.get(end_char)?).max(start_byte);

    let excerpt = clip_between(text, start_byte, end_byte, limit);
    (!excerpt.trim().is_empty()).then_some(excerpt)
}

/// Look for the selection in an extracted document, starting with the page the reader shows.
///
/// @param pages - extracted pages of the document
/// @param selection - selected text
/// @param page - page number the reader displays, when known
/// @param limit - maximum number of characters kept from the returned text
/// @returns surrounding text, or none when no page contains the selection
fn locate_in_pages(
    pages: &[String],
    selection: &str,
    page: Option<u32>,
    limit: usize,
) -> Option<String> {
    if let Some(page) = page {
        let index = page.saturating_sub(1) as usize;
        // the selection usually sits on the visible page, but a selection that starts at a page
        // break is reported from the following one
        for candidate in [index.saturating_sub(1), index, index + 1] {
            let context = pages
                .get(candidate)
                .and_then(|text| locate_in_text(text, selection, limit));
            if context.is_some() {
                return context;
            }
        }
    }

    pages
        .iter()
        .find_map(|text| locate_in_text(text, selection, limit))
}

/// Return the text between two byte positions, widened to the limit and trimmed to whole lines.
///
/// @param text - text the positions belong to
/// @param start - byte offset of the first selected character
/// @param end - byte offset after the last selected character
/// @param limit - maximum number of characters kept
/// @returns excerpt with clipped ends marked by an ellipsis
fn clip_between(text: &str, start: usize, end: usize, limit: usize) -> String {
    let total = text.chars().count();
    if total <= limit {
        return text.trim().to_string();
    }

    let start_char = text[..start.min(text.len())].chars().count();
    let end_char = text[..end.min(text.len())]
        .chars()
        .count()
        .max(start_char)
        .min(total);

    // center the window on the selection, then pull it back inside the text
    let half = limit.saturating_sub(end_char - start_char) / 2;
    let from = start_char.saturating_sub(half);
    let to = (from + limit).min(total);
    let from = to.saturating_sub(limit);

    let excerpt: String = text.chars().skip(from).take(to - from).collect();
    tidy_excerpt(&excerpt, from > 0, to < total)
}

/// Cut a raw slice back to whole lines, join hyphenated words and mark clipped ends.
///
/// @param excerpt - raw slice of the extracted text
/// @param clipped_start - whether text was dropped before the slice
/// @param clipped_end - whether text was dropped after the slice
/// @returns readable excerpt
fn tidy_excerpt(excerpt: &str, clipped_start: bool, clipped_end: bool) -> String {
    let mut text = excerpt.to_string();

    // the first and the last line are cut in the middle of a word whenever text was dropped
    if clipped_start {
        if let Some(newline) = text.find(LINE_BREAK) {
            text = text[newline + 1..].to_string();
        }
    }
    if clipped_end {
        if let Some(newline) = text.rfind(LINE_BREAK) {
            text.truncate(newline);
        }
    }

    let text = dehyphenate(text.trim());
    if text.is_empty() {
        return String::new();
    }

    let mut result = String::with_capacity(text.len() + 4);
    if clipped_start {
        result.push('…');
    }
    result.push_str(&text);
    if clipped_end {
        result.push('…');
    }
    result
}

/// Join words the typesetter split at a line break.
///
/// @param text - excerpt whose line-end hyphens are typesetting artefacts
/// @returns text with those hyphens and their line breaks removed
fn dehyphenate(text: &str) -> String {
    let characters: Vec<char> = text.chars().collect();
    let mut joined = String::with_capacity(text.len());
    let mut index = 0;

    while index < characters.len() {
        let character = characters[index];
        let is_line_end_hyphen = character == '-'
            && index > 0
            && characters[index - 1].is_alphabetic()
            && index + 1 < characters.len()
            && is_line_break(characters[index + 1]);

        if is_line_end_hyphen {
            // only the single line break the typesetter inserted is joined; a paragraph gap keeps
            // its hyphen
            let mut next = index + 1;
            if characters[next] == CARRIAGE_RETURN {
                next += 1;
            }
            if characters.get(next) == Some(&LINE_BREAK) {
                next += 1;
            }
            if characters.get(next).is_some_and(|next| next.is_alphabetic()) {
                index = next;
                continue;
            }
        }

        joined.push(character);
        index += 1;
    }

    joined
}

/// Check whether a character ends a line.
///
/// @param character - character to inspect
/// @returns whether the character is a line break
fn is_line_break(character: char) -> bool {
    character == LINE_BREAK || character == CARRIAGE_RETURN
}

/// Resolve the document a reader window shows from its title.
///
/// @param title - window caption
/// @returns open document with the displayed page, or none when the title names no document
fn resolve(title: &str) -> Option<DocumentRef> {
    let candidates = document_name_candidates(title);
    if candidates.is_empty() {
        return None;
    }

    // a caption may carry the full path already
    for candidate in &candidates {
        let path = Path::new(candidate);
        if path.is_absolute() && path.is_file() {
            return Some(DocumentRef {
                path: path.to_path_buf(),
                page: None,
            });
        }
    }

    // readers normally show the file name alone, so the name is looked up in their recent files
    recent_documents()
        .into_iter()
        .find(|document| document.matches_name(&candidates))
}

impl DocumentRef {
    /// Check whether the document carries one of the file names taken from a window title.
    ///
    /// @param candidates - file names a title may contain
    /// @returns whether any candidate names this document
    fn matches_name(&self, candidates: &[String]) -> bool {
        let name = file_name(&self.path.to_string_lossy());
        !name.is_empty()
            && candidates
                .iter()
                .any(|candidate| name.eq_ignore_ascii_case(&file_name(candidate)))
    }
}

/// Collect every document name a window title can contain.
///
/// Readers append their own segments - `book.pdf - Chapter 3 - SumatraPDF` - and the file name
/// itself may contain the same separator, so every suffix that ends at a document extension is
/// kept instead of splitting the caption at the first separator.
///
/// @param title - window caption
/// @returns candidate file names, most specific first
fn document_name_candidates(title: &str) -> Vec<String> {
    let mut candidates: Vec<String> = Vec::new();

    for extension in DOCUMENT_EXTENSIONS {
        let mut search_from = 0;
        while let Some(offset) = find_ignore_case(&title[search_from..], extension) {
            let end = search_from + offset + extension.len();

            // ".pdfx" is a different file, not a document
            let continues = title[end..]
                .chars()
                .next()
                .is_some_and(|character| character.is_alphanumeric() || character == '_');
            if !continues {
                for start in candidate_starts(&title[..end]) {
                    let name = title[start..end].trim();
                    let known = candidates
                        .iter()
                        .any(|candidate| candidate.eq_ignore_ascii_case(name));
                    if !name.is_empty() && !known {
                        candidates.push(name.to_string());
                    }
                }
            }

            search_from = end;
        }
    }

    candidates
}

/// Byte offsets a document name can start at inside a window title.
///
/// @param prefix - caption slice that ends with the document extension
/// @returns offsets, starting at the beginning of the caption
fn candidate_starts(prefix: &str) -> Vec<usize> {
    let mut starts = vec![0];
    let mut search_from = 0;

    while let Some(offset) = prefix[search_from..].find(TITLE_SEPARATOR) {
        let start = search_from + offset + TITLE_SEPARATOR.len();
        starts.push(start);
        search_from = start;
    }

    starts
}

/// Extract the text of one page through the cheap path.
///
/// Reading a single page keeps the popup responsive for large books: the whole document is only
/// parsed when the cheap path found nothing.
///
/// @param path - PDF file
/// @param page - page number the reader displays
/// @returns page text, or none when the page cannot be read
fn page_text(path: &Path, page: u32) -> Option<String> {
    if !has_extension(path, "pdf") {
        return None;
    }

    let result = std::panic::catch_unwind(AssertUnwindSafe(|| {
        let document = lopdf::Document::load(path)?;
        document.extract_text(&[page])
    }));

    match result {
        Ok(Ok(text)) if !text.trim().is_empty() => Some(text),
        Ok(Err(error)) => {
            debug!("Failed to read page {} of {:?}: {}", page, path, error);
            None
        }
        _ => None,
    }
}

/// Extract the whole document inside the inline budget.
///
/// The extraction runs on its own thread so a slow book cannot hold the popup back: past the
/// budget the result is only stored in the cache, where the next selection picks it up.
///
/// @param path - document file
/// @returns extracted pages when the extraction finished in time
fn extract_with_budget(path: &Path) -> Option<Arc<Vec<String>>> {
    // one extraction at a time: a second reader window must not start a parallel job
    if EXTRACTION_RUNNING.swap(true, Ordering::SeqCst) {
        return None;
    }

    let (sender, receiver) = mpsc::channel();
    let target = path.to_path_buf();
    let worker = target.clone();

    let spawned = std::thread::Builder::new()
        .name("textgo-document".into())
        .spawn(move || {
            // the running flag has to drop even when the extraction panics
            let _running = RunningGuard;

            let pages = extract_pages(&worker).map(Arc::new);
            if let Ok(pages) = &pages {
                if let Ok(mut cache) = DOCUMENT_CACHE.lock() {
                    *cache = Some(CachedDocument {
                        path: worker.clone(),
                        pages: Arc::clone(pages),
                    });
                }
            }

            let _ = sender.send(pages);
        });

    if spawned.is_err() {
        EXTRACTION_RUNNING.store(false, Ordering::SeqCst);
        return None;
    }

    match receiver.recv_timeout(INLINE_BUDGET) {
        Ok(Ok(pages)) => Some(pages),
        Ok(Err(error)) => {
            debug!("Failed to extract {:?}: {}", target, error);
            None
        }
        Err(_) => {
            debug!(
                "Document extraction still running for {:?}, context skipped",
                target
            );
            None
        }
    }
}

/// Clears the extraction flag when the worker leaves, including on panic.
struct RunningGuard;

impl Drop for RunningGuard {
    fn drop(&mut self) {
        EXTRACTION_RUNNING.store(false, Ordering::SeqCst);
    }
}

/// Pages of the cached document, when it is the one that was asked for.
///
/// @param path - document file
/// @returns cached pages, or none when another document is cached
fn cached_pages(path: &Path) -> Option<Arc<Vec<String>>> {
    let cache = DOCUMENT_CACHE.lock().ok()?;
    cache
        .as_ref()
        .filter(|cached| cached.path == path)
        .map(|cached| Arc::clone(&cached.pages))
}

/// Extract the text of a document, one entry per page for PDF files.
///
/// @param path - document file
/// @returns extracted pages
fn extract_pages(path: &Path) -> Result<Vec<String>, String> {
    let size = fs::metadata(path)
        .map_err(|error| format!("document unreadable: {error}"))?
        .len();
    if size == 0 {
        return Err("document is empty".into());
    }
    if size > MAX_DOCUMENT_BYTES {
        return Err(format!("document of {size} bytes is too large to read"));
    }

    let pages = if has_extension(path, "pdf") {
        pdf_pages(path)?
    } else if has_extension(path, "epub") {
        vec![epub_text(path)?]
    } else {
        return Err("document type is not supported".into());
    };

    if pages.iter().all(|page| page.trim().is_empty()) {
        return Err("document carries no text layer".into());
    }

    let characters: usize = pages.iter().map(|page| page.chars().count()).sum();
    if characters > MAX_DOCUMENT_CHARS {
        return Err(format!("document text of {characters} characters is too large"));
    }

    Ok(pages)
}

/// Extract every page of a PDF.
///
/// @param path - PDF file
/// @returns text of each page
fn pdf_pages(path: &Path) -> Result<Vec<String>, String> {
    // the extraction library aborts on malformed files, and a panic must not take the capture down
    match std::panic::catch_unwind(AssertUnwindSafe(|| {
        pdf_extract::extract_text_by_pages(path)
    })) {
        Ok(Ok(pages)) => Ok(pages),
        Ok(Err(error)) => Err(format!("pdf extraction failed: {error}")),
        Err(_) => Err("pdf extraction panicked".into()),
    }
}

/// Extract the text of an EPUB, one entry for the whole book.
///
/// The chapters are joined in entry-name order, which is the reading order of every EPUB layout
/// this module has to handle.
///
/// @param path - EPUB file
/// @returns extracted text
fn epub_text(path: &Path) -> Result<String, String> {
    let file = fs::File::open(path).map_err(|error| format!("document unreadable: {error}"))?;
    let mut archive =
        zip::ZipArchive::new(file).map_err(|error| format!("archive unreadable: {error}"))?;

    let mut chapters: Vec<(String, String)> = Vec::new();
    for index in 0..archive.len() {
        let mut entry = archive
            .by_index(index)
            .map_err(|error| format!("chapter {index} unreadable: {error}"))?;
        if entry.is_dir() {
            continue;
        }

        let name = entry.name().to_string();
        let lower = name.to_lowercase();
        if !(lower.ends_with(".xhtml") || lower.ends_with(".html") || lower.ends_with(".htm")) {
            continue;
        }

        let mut bytes = Vec::new();
        entry
            .read_to_end(&mut bytes)
            .map_err(|error| format!("chapter {name} unreadable: {error}"))?;
        chapters.push((name, html_text(&String::from_utf8_lossy(&bytes))));
    }

    chapters.sort_by(|left, right| left.0.cmp(&right.0));

    let mut text = String::new();
    for (_, chapter) in chapters {
        if text.chars().count() >= MAX_DOCUMENT_CHARS {
            break;
        }
        let chapter = chapter.trim();
        if chapter.is_empty() {
            continue;
        }
        text.push_str(chapter);
        text.push_str("\n\n");
    }

    if text.trim().is_empty() {
        return Err("no readable chapters".into());
    }

    Ok(text)
}

/// Convert one XHTML chapter into searchable plain text.
///
/// @param html - chapter markup
/// @returns text with block breaks and decoded entities
fn html_text(html: &str) -> String {
    let mut text = String::with_capacity(html.len() / 2);
    let mut cursor = 0;

    while let Some(offset) = html[cursor..].find('<') {
        let position = cursor + offset;
        push_words(&mut text, &html[cursor..position]);

        let Some(tag_offset) = html[position..].find('>') else {
            // unterminated markup: the rest of the chapter is still worth keeping
            push_words(&mut text, &html[position..]);
            cursor = html.len();
            break;
        };

        let tag = &html[position + 1..position + tag_offset];
        let name = tag_name(tag);
        let body_start = position + tag_offset + 1;

        // a script or style body is not text, so it is skipped up to its closing tag
        if name.eq_ignore_ascii_case("script") || name.eq_ignore_ascii_case("style") {
            let closing = format!("</{name}");
            let body = &html[body_start..];
            let skipped = match find_ignore_case(body, &closing) {
                Some(close) => match body[close..].find('>') {
                    Some(tail) => close + tail + 1,
                    None => body.len(),
                },
                None => body.len(),
            };
            cursor = body_start + skipped;
            continue;
        }

        if is_block_tag(name) {
            push_break(&mut text);
        }

        cursor = body_start;
    }

    push_words(&mut text, &html[cursor.min(html.len())..]);
    decode_entities(text.trim())
}

/// Read the element name out of a tag body.
///
/// @param tag - markup between the angle brackets
/// @returns element name without the leading slash
fn tag_name(tag: &str) -> &str {
    let name = tag.trim_start_matches('/');
    let end = name
        .find(|character: char| !character.is_ascii_alphanumeric())
        .unwrap_or(name.len());
    &name[..end]
}

/// Check whether a tag starts a new block of text.
///
/// @param name - element name
/// @returns whether a line break has to follow the element
fn is_block_tag(name: &str) -> bool {
    matches!(
        name,
        "br" | "p" | "div" | "li" | "tr" | "td" | "section" | "article" | "blockquote" | "pre"
    ) || (name.len() == 2 && name.starts_with('h') && name.as_bytes()[1].is_ascii_digit())
}

/// Append plain words to the text, collapsing their spacing.
///
/// @param text - text built so far
/// @param raw - markup-free fragment
fn push_words(text: &mut String, raw: &str) {
    for word in raw.split_whitespace() {
        if !text.is_empty() && !text.ends_with(LINE_BREAK) {
            text.push(' ');
        }
        text.push_str(word);
    }
}

/// Start a new line in the text.
///
/// @param text - text built so far
fn push_break(text: &mut String) {
    while text.ends_with(' ') {
        text.pop();
    }
    if !text.is_empty() && !text.ends_with(LINE_BREAK) {
        text.push(LINE_BREAK);
    }
}

/// Decode the character references an XHTML chapter uses.
///
/// @param text - text that may contain references such as `&amp;`
/// @returns text with references replaced
fn decode_entities(text: &str) -> String {
    let mut decoded = String::with_capacity(text.len());
    let mut cursor = 0;

    while let Some(offset) = text[cursor..].find('&') {
        let position = cursor + offset;
        decoded.push_str(&text[cursor..position]);

        // a reference is short; anything longer is a literal ampersand
        let end = text[position..].find(';').filter(|end| *end <= 10);
        let replacement =
            end.and_then(|end| entity_character(&text[position + 1..position + end]));

        match (end, replacement) {
            (Some(end), Some(character)) => {
                decoded.push(character);
                cursor = position + end + 1;
            }
            _ => {
                decoded.push('&');
                cursor = position + 1;
            }
        }
    }

    decoded.push_str(&text[cursor..]);
    decoded
}

/// Resolve one character reference.
///
/// @param entity - reference body without the leading ampersand and trailing semicolon
/// @returns decoded character, or none when the reference is unknown
fn entity_character(entity: &str) -> Option<char> {
    match entity {
        "amp" => Some('&'),
        "lt" => Some('<'),
        "gt" => Some('>'),
        "quot" => Some('"'),
        "apos" => Some('\''),
        "nbsp" => Some(' '),
        _ => {
            // numeric references are written as #65 or #x41
            let number = entity.strip_prefix('#')?;
            let code = match number.strip_prefix('x').or_else(|| number.strip_prefix('X')) {
                Some(hexadecimal) => u32::from_str_radix(hexadecimal, 16).ok()?,
                None => number.parse().ok()?,
            };
            char::from_u32(code)
        }
    }
}

/// Find a needle in a text without changing byte offsets.
///
/// Lower-casing a whole document can change its byte length, which would invalidate the offsets a
/// match is translated back with, so the comparison is done per byte instead.
///
/// @param haystack - text to search
/// @param needle - ASCII needle, matched case-insensitively
/// @returns byte offset of the first match
fn find_ignore_case(haystack: &str, needle: &str) -> Option<usize> {
    let haystack = haystack.as_bytes();
    let needle = needle.as_bytes();
    if needle.is_empty() || haystack.len() < needle.len() {
        return None;
    }

    (0..=haystack.len() - needle.len())
        .find(|start| haystack[*start..start + needle.len()].eq_ignore_ascii_case(needle))
}

/// Last segment of a Windows or POSIX path.
///
/// @param value - path or bare file name
/// @returns file name without directory
fn file_name(value: &str) -> String {
    value
        .rsplit(['\\', '/'])
        .next()
        .unwrap_or(value)
        .trim()
        .to_string()
}

/// Check whether a path carries a given extension, ignoring case.
///
/// @param path - file path
/// @param extension - extension without the dot
/// @returns whether the extension matches
fn has_extension(path: &Path, extension: &str) -> bool {
    path.extension()
        .is_some_and(|value| value.to_string_lossy().eq_ignore_ascii_case(extension))
}

/// Recently opened documents of the readers that keep such an index.
///
/// @returns existing documents, most recently used first
fn recent_documents() -> Vec<DocumentRef> {
    let mut documents = Vec::new();

    for (variable, relative) in RECENT_INDEXES {
        let Ok(root) = std::env::var(variable) else {
            continue;
        };
        let Ok(settings) = fs::read_to_string(Path::new(&root).join(relative)) else {
            continue;
        };
        documents.extend(parse_recent_index(&settings));
    }

    documents
}

/// Parse the documents a reader lists in its settings file.
///
/// SumatraPDF writes one block per document and keeps them most recently used first, so the order
/// is preserved and the first name match wins.
///
/// @param settings - contents of the reader settings file
/// @returns existing documents with the page the reader displayed
fn parse_recent_index(settings: &str) -> Vec<DocumentRef> {
    let mut documents: Vec<DocumentRef> = Vec::new();

    for line in settings.lines() {
        let line = line.trim();
        if let Some(value) = line.strip_prefix("FilePath = ") {
            documents.push(DocumentRef {
                path: PathBuf::from(value.trim()),
                page: None,
            });
        } else if let Some(value) = line.strip_prefix("PageNo = ") {
            if let Some(document) = documents.last_mut() {
                document.page = value.trim().parse().ok();
            }
        }
    }

    documents.retain(|document| document.path.is_file());
    documents
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reader_caption_yields_the_document_names_it_contains() {
        let title = "Horowitz P., Hill W. - The Art of Electronics, Third Edition - 2015.pdf - \
                     [The Art of Electronics (3rd edition)] - SumatraPDF";
        let candidates = document_name_candidates(title);

        assert_eq!(
            candidates.first().map(String::as_str),
            Some("Horowitz P., Hill W. - The Art of Electronics, Third Edition - 2015.pdf")
        );
        assert!(candidates.contains(&"2015.pdf".to_string()));
    }

    #[test]
    fn caption_without_a_document_yields_nothing() {
        assert!(document_name_candidates("Untitled - Notepad").is_empty());
        assert!(document_name_candidates("page.pdfx - Reader").is_empty());
    }

    #[test]
    fn recent_index_keeps_the_displayed_page() {
        let missing = "FileStates [\n\t[\n\t\tFilePath = C:\\books\\first.pdf\n\t\tPageNo = 38\n\t]\n]\n";
        // the fixture file does not exist, so the entry has to be dropped
        assert!(parse_recent_index(missing).is_empty());

        let existing = format!(
            "FileStates [\n\t[\n\t\tFilePath = {}\n\t\tPageNo = 38\n\t]\n]\n",
            file!()
        );
        let documents = parse_recent_index(&existing);
        assert_eq!(documents.len(), 1);
        assert_eq!(documents[0].page, Some(38));
    }

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
    fn selection_is_located_across_a_hyphenated_line_break() {
        let page = "Resistors can be made from metal films, or from carbon-composition moldings; \
                    commonly from semiconductor elements.";
        let context = locate_in_text(page, "com-\nmonly from semiconductor", 200).expect("context");
        assert!(context.contains("commonly from semiconductor"), "{context}");
    }

    #[test]
    fn missing_selection_yields_no_context() {
        assert!(locate_in_text("Some other text entirely.", "nowhere to be found", 200).is_none());
    }

    #[test]
    fn short_selection_is_ignored() {
        assert!(locate_in_text("a b c", "a b", 200).is_none());
    }

    #[test]
    fn excerpt_is_centered_and_bounded() {
        let text = format!("{}needle in the middle{}", "a ".repeat(200), " b".repeat(200));
        let context = locate_in_text(&text, "needle in the middle", 60).expect("context");

        assert!(context.contains("needle in the middle"), "{context}");
        assert!(context.chars().count() <= 62, "{}", context.chars().count());
        assert!(context.ends_with('…'), "{context}");
    }

    #[test]
    fn short_documents_are_returned_whole() {
        let text = "Only one line of text here.";
        assert_eq!(locate_in_text(text, "one line", 200), Some(text.to_string()));
    }

    #[test]
    fn chapter_markup_becomes_plain_text() {
        let html = "<html><head><style>p { color: red }</style></head><body>\
                    <h1>Chapter &amp; One</h1><p>First   paragraph.</p>\
                    <script>var noise = 1;</script><p>Second&nbsp;paragraph.</p></body></html>";
        let text = html_text(html);

        assert_eq!(text, "Chapter & One\nFirst paragraph.\nSecond paragraph.");
        assert!(!text.contains("color"), "{text}");
        assert!(!text.contains("noise"), "{text}");
    }

    #[test]
    fn unknown_entities_stay_literal() {
        assert_eq!(decode_entities("a &unknown; b &#65; &#x42;"), "a &unknown; b A B");
    }

    #[test]
    fn hyphenated_words_are_joined_for_display() {
        assert_eq!(
            dehyphenate("com-\nmonly used\n\npara-\ngraph"),
            "commonly used\n\npara-\ngraph"
        );
    }
}
