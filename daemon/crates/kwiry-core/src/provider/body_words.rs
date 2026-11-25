// SPDX-License-Identifier: MIT OR Apache-2.0
//! Port of the partner's `current-body-words-v1` matcher (OpenClast
//! `sync-core/src/search-match.ts`, commit 5eefd8bf): the body words a host
//! re-checks a lexical candidate against. The provider uses it only to decide
//! which notes contain all of a query's `terms` (the lexical recall
//! obligation); Kwiry's own analyser still finds and orders candidates.
//!
//! Definition, as in the original: the eligible body is the text after an
//! initial YAML block (line one exactly `---`, closed by a later whole line
//! `---` or `...`; an initial BOM is ignored for this detection only; an
//! unclosed block leaves no body). Folding is NFD, full default
//! (non-Turkic) case folding, then dropping every `M*` code point. A word is
//! a maximal run of `L*` or `Nd` code points of the folded text. General
//! categories and case folding are pinned to Unicode 15.1 by the same
//! generated tables (`unicode_tables.rs`). NFD uses `unicode-normalization`,
//! where the original uses the JavaScript runtime's; the two can differ only
//! for code points assigned after the older of the two Unicode versions.

use std::sync::OnceLock;

use unicode_normalization::UnicodeNormalization;

use super::unicode_tables::{
    DECIMAL_NUMBER_RANGES, FOLD_MULTI, FOLD_RUNS, LETTER_RANGES, MARK_RANGES,
};

/// Matcher version this port implements.
pub(crate) const MATCHER_VERSION: &str = "current-body-words-v1";

/// Words longer than this many bytes are indexed and asked for by a digest,
/// so an over-long word can neither be dropped by the index (Tantivy drops
/// terms over `MAX_TOKEN_LEN`) nor miss its query term.
const LONG_WORD_BYTES: usize = 240;

fn in_ranges(table: &[u32], cp: u32) -> bool {
    let pairs = table.len() / 2;
    let (mut lo, mut hi) = (0usize, pairs);
    while lo < hi {
        let mid = (lo + hi) / 2;
        if cp < table[2 * mid] {
            hi = mid;
        } else if cp > table[2 * mid + 1] {
            lo = mid + 1;
        } else {
            return true;
        }
    }
    false
}

fn is_letter(cp: u32) -> bool {
    if cp < 0x80 {
        return (cp | 0x20) >= 0x61 && (cp | 0x20) <= 0x7a;
    }
    in_ranges(LETTER_RANGES, cp)
}

fn is_digit(cp: u32) -> bool {
    if cp < 0x80 {
        return (0x30..=0x39).contains(&cp);
    }
    in_ranges(DECIMAL_NUMBER_RANGES, cp)
}

fn is_mark(cp: u32) -> bool {
    cp >= 0x300 && in_ranges(MARK_RANGES, cp)
}

fn is_word_char(ch: char) -> bool {
    let cp = u32::from(ch);
    is_letter(cp) || is_digit(cp)
}

/// Code points that fold to several code points (status F).
fn fold_multi() -> &'static [(u32, Vec<char>)] {
    static MULTI: OnceLock<Vec<(u32, Vec<char>)>> = OnceLock::new();
    MULTI.get_or_init(|| {
        let mut out = Vec::new();
        let mut index = 0;
        while index + 1 < FOLD_MULTI.len() {
            let cp = FOLD_MULTI[index];
            let count = FOLD_MULTI[index + 1] as usize;
            let targets = FOLD_MULTI[index + 2..index + 2 + count]
                .iter()
                .filter_map(|&target| char::from_u32(target))
                .collect();
            out.push((cp, targets));
            index += 2 + count;
        }
        out.sort_by_key(|(cp, _)| *cp);
        out
    })
}

/// Full default case folding of one code point (Unicode 15.1), appended to
/// `out` with every `M*` code point dropped.
fn push_folded_without_marks(ch: char, out: &mut String) {
    let cp = u32::from(ch);
    if cp < 0x80 {
        out.push(ch.to_ascii_lowercase());
        return;
    }
    let multi = fold_multi();
    if let Ok(found) = multi.binary_search_by_key(&cp, |(key, _)| *key) {
        for &target in &multi[found].1 {
            if !is_mark(u32::from(target)) {
                out.push(target);
            }
        }
        return;
    }
    let runs = FOLD_RUNS.len() / 4;
    let (mut lo, mut hi) = (0usize, runs);
    // Last run whose start is at or below `cp`.
    while lo < hi {
        let mid = (lo + hi) / 2;
        if FOLD_RUNS[4 * mid] as u32 <= cp {
            lo = mid + 1;
        } else {
            hi = mid;
        }
    }
    let mut folded = cp;
    if lo > 0 {
        let at = lo - 1;
        let (start, end, stride, delta) = (
            FOLD_RUNS[4 * at] as u32,
            FOLD_RUNS[4 * at + 1] as u32,
            FOLD_RUNS[4 * at + 2] as u32,
            FOLD_RUNS[4 * at + 3],
        );
        if cp <= end && (cp - start) % stride == 0 {
            folded = (i64::from(cp) + i64::from(delta)) as u32;
        }
    }
    if !is_mark(folded)
        && let Some(folded) = char::from_u32(folded)
    {
        out.push(folded);
    }
}

/// Match the partner's bounded NFD pieces, including its UTF-16 length
/// threshold and cuts within runs of non-starters. A new Unicode mark may
/// survive our pinned category table, so cutting at the same point matters.
pub(crate) fn fold_for_match(text: &str) -> String {
    const FOLD_MARK_RUN_PIECE: usize = 64;
    const FOLD_PIECE_UNITS: usize = 4096;

    let mut out = String::with_capacity(text.len());
    let mut start = 0;
    let mut units = 0;
    let mut run = 0;
    for (offset, ch) in text.char_indices() {
        let cp = u32::from(ch);
        let stable = ch.is_ascii() || is_letter(cp) || is_digit(cp);
        if offset > start
            && (if stable {
                units >= FOLD_PIECE_UNITS
            } else {
                run >= FOLD_MARK_RUN_PIECE
            })
        {
            fold_piece(&text[start..offset], &mut out);
            start = offset;
            units = 0;
            run = 0;
        }
        run = if stable { 0 } else { run + 1 };
        units += ch.len_utf16();
    }
    fold_piece(&text[start..], &mut out);
    out
}

fn fold_piece(piece: &str, out: &mut String) {
    for ch in piece.nfd() {
        push_folded_without_marks(ch, out);
    }
}

/// Maximal runs of `L*` or `Nd` code points in already folded text.
fn word_tokens(folded: &str) -> impl Iterator<Item = &str> {
    folded
        .split(|ch: char| !is_word_char(ch))
        .filter(|word| !word.is_empty())
}

fn line_end(text: &str, from: usize) -> (usize, usize) {
    let bytes = text.as_bytes();
    let mut index = from;
    while index < bytes.len() {
        match bytes[index] {
            b'\n' => return (index, index + 1),
            b'\r' => {
                let next = if bytes.get(index + 1) == Some(&b'\n') {
                    index + 2
                } else {
                    index + 1
                };
                return (index, next);
            }
            _ => index += 1,
        }
    }
    (bytes.len(), bytes.len())
}

/// The eligible body: the text after an initial YAML block, or the whole
/// text when there is none; `None` when the block is never closed.
pub(crate) fn eligible_body(text: &str) -> Option<&str> {
    let start = if text.starts_with('\u{feff}') { 3 } else { 0 };
    let (end, mut at) = line_end(text, start);
    if &text[start..end] != "---" {
        return Some(text);
    }
    while at < text.len() {
        let (end, next) = line_end(text, at);
        let line = &text[at..end];
        if line == "---" || line == "..." {
            return Some(&text[next..]);
        }
        at = next;
    }
    None
}

/// Index form of one folded word: itself, or a digest when over-long.
pub(crate) fn index_word(word: &str) -> String {
    if word.len() <= LONG_WORD_BYTES {
        return word.to_owned();
    }
    format!("\u{1}{}", super::hex_sha256(word.as_bytes()))
}

/// Every body word of a note, in order, in index form (repeats kept, so an
/// index can count frequencies). `None` when the bytes are not UTF-8: the
/// host's matcher cannot read such a note either.
pub(crate) fn note_words(bytes: &[u8]) -> Option<Vec<String>> {
    let text = std::str::from_utf8(bytes).ok()?;
    let Some(body) = eligible_body(text) else {
        return Some(Vec::new());
    };
    // The partner folds each line separately, so earlier lines do not
    // shift the UTF-16 boundary of the piece containing a word.
    let mut words = Vec::new();
    for line in body.split(['\r', '\n']) {
        let folded = fold_for_match(line);
        words.extend(word_tokens(&folded).map(index_word));
    }
    Some(words)
}

/// The distinct folded words of a query text, first occurrence first, in
/// index form. Used only when a request carries no `terms`.
pub(crate) fn query_words(text: &str) -> Vec<String> {
    let folded = fold_for_match(text);
    let mut seen = std::collections::HashSet::new();
    word_tokens(&folded)
        .filter(|word| seen.insert(*word))
        .map(index_word)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words(text: &str) -> Vec<String> {
        note_words(text.as_bytes()).unwrap()
    }

    #[test]
    fn folds_case_marks_and_multi_code_point_folds() {
        assert_eq!(fold_for_match("Café ÉCOLE"), "cafe ecole");
        // ß folds to "ss" (status F), the Kelvin sign to k, final sigma to σ.
        assert_eq!(
            fold_for_match("Straße \u{212a}elvin ΟΔΟΣ"),
            "strasse kelvin οδοσ"
        );
        // U+0345 is a mark that folds to a letter (ι) and survives.
        assert_eq!(fold_for_match("\u{3b1}\u{345}"), "\u{3b1}\u{3b9}");
        // İ (U+0130) folds to i + U+0307; the mark is dropped.
        assert_eq!(fold_for_match("\u{130}"), "i");
    }

    #[test]
    fn words_are_letter_and_decimal_runs() {
        assert_eq!(
            words("Galka-moon e-mail x2 ½ 3.14\tβeta_γ"),
            ["galka", "moon", "e", "mail", "x2", "3", "14", "βeta", "γ"]
        );
        // A combining mark between letters is dropped, so the word continues.
        assert_eq!(words("ab\u{301}c"), ["abc"]);
        // Superscript digits are No, not Nd: separators.
        assert_eq!(words("m²x"), ["m", "x"]);
    }

    #[test]
    fn eligible_body_skips_an_initial_yaml_block_only() {
        assert_eq!(words("---\ntitle: galka\n---\nbody text"), ["body", "text"]);
        assert_eq!(words("\u{feff}---\r\nx: galka\r\n...\r\nok"), ["ok"]);
        assert_eq!(words("---\nunclosed galka\n"), Vec::<String>::new());
        assert_eq!(words("--- \nnot yaml\n---\nz"), ["not", "yaml", "z"]);
        assert_eq!(words("text\n---\nmore\n---\n"), ["text", "more"]);
        assert_eq!(note_words(b"\xff\xfe"), None);
    }

    #[test]
    fn matches_openclast_at_the_non_starter_split() {
        // OpenClast 5eefd8bf matchBody(parseQuery("aι"), body) is true.
        // U+0897 has a nonzero combining class in newer Unicode, but is
        // not a mark in the partner's pinned Unicode 15.1 category table.
        let body = format!("a{}\u{345}\u{897}b", "\u{301}".repeat(63));
        assert_eq!(words(&body), ["aι", "b"]);
        assert_eq!(query_words("aι"), ["aι"]);
    }

    #[test]
    fn long_words_use_a_digest_on_both_sides() {
        let long = "a".repeat(300);
        let indexed = words(&long);
        assert_eq!(indexed.len(), 1);
        assert!(indexed[0].starts_with('\u{1}'));
        assert_eq!(query_words(&long.to_uppercase()), indexed);
    }

    #[test]
    fn query_words_are_distinct_in_order() {
        assert_eq!(query_words("Quokka habitat QUOKKA"), ["quokka", "habitat"]);
    }
}
