// SPDX-License-Identifier: MIT OR Apache-2.0

//! Byte-level screen applied **before** the buffer reaches `lopdf`.
//!
//! Two jobs, both of them budgets `lopdf` does not provide:
//!
//! 1. total input bytes, and
//! 2. object nesting depth.
//!
//! The nesting screen is a lexer, not a parser: it walks the raw file counting
//! `<< >>` and `[ ]` depth while skipping the three constructs that can contain
//! those bytes without meaning them — comments, literal strings, and stream
//! payloads. It is deliberately a *pre*-screen and its coverage is exactly the
//! uncompressed portion of the file. Objects living inside an object stream are
//! opaque here; those are bounded by
//! [`limits::MAX_DECOMPRESSED_STREAM_BYTES`](super::limits::MAX_DECOMPRESSED_STREAM_BYTES)
//! and by `lopdf`'s own dereference limit. Claiming otherwise would be the kind
//! of unearned guarantee this layer exists to avoid.
//!
//! The screen never *accepts* on the basis of a guess: it only refuses. A file
//! it passes is still fully re-validated by the parser.

use super::error::PdfReadError;
use super::limits;

/// Longest prefix searched for the `%PDF-` header. PDF 1.7 §7.5.2 puts it at
/// byte 0; Acrobat tolerates junk before it, and so do we, but not unboundedly.
const MAX_HEADER_SEARCH_BYTES: usize = 1_024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Prescreen {
    pub(super) max_nesting_depth: usize,
}

pub(super) fn prescreen(bytes: &[u8]) -> Result<Prescreen, PdfReadError> {
    if bytes.len() > limits::MAX_INPUT_BYTES {
        return Err(PdfReadError::InputTooLarge);
    }
    if !has_pdf_header(bytes) {
        return Err(PdfReadError::NotAPdf);
    }
    let max_nesting_depth = scan_nesting_depth(bytes)?;
    Ok(Prescreen { max_nesting_depth })
}

fn has_pdf_header(bytes: &[u8]) -> bool {
    let window = &bytes[..bytes.len().min(MAX_HEADER_SEARCH_BYTES)];
    window
        .windows(b"%PDF-".len())
        .any(|candidate| candidate == b"%PDF-")
}

fn scan_nesting_depth(bytes: &[u8]) -> Result<usize, PdfReadError> {
    let mut index = 0usize;
    let mut depth = 0usize;
    let mut max_depth = 0usize;

    while index < bytes.len() {
        match bytes[index] {
            b'%' => index = skip_comment(bytes, index),
            b'(' => index = skip_literal_string(bytes, index),
            b'<' if bytes.get(index + 1) == Some(&b'<') => {
                depth += 1;
                max_depth = max_depth.max(depth);
                if depth > limits::MAX_OBJECT_NESTING_DEPTH {
                    return Err(PdfReadError::ObjectNestingTooDeep);
                }
                index += 2;
            }
            b'<' => index = skip_hex_string(bytes, index),
            b'>' if bytes.get(index + 1) == Some(&b'>') => {
                depth = depth.saturating_sub(1);
                index += 2;
            }
            b'[' => {
                depth += 1;
                max_depth = max_depth.max(depth);
                if depth > limits::MAX_OBJECT_NESTING_DEPTH {
                    return Err(PdfReadError::ObjectNestingTooDeep);
                }
                index += 1;
            }
            b']' => {
                depth = depth.saturating_sub(1);
                index += 1;
            }
            b's' if starts_stream_keyword(bytes, index) => {
                index = skip_stream_payload(bytes, index)
            }
            _ => index += 1,
        }
    }

    Ok(max_depth)
}

fn skip_comment(bytes: &[u8], start: usize) -> usize {
    let mut index = start + 1;
    while index < bytes.len() && bytes[index] != b'\r' && bytes[index] != b'\n' {
        index += 1;
    }
    index
}

/// PDF 1.7 §7.3.4.2: balanced unescaped parentheses nest, `\` escapes the next
/// byte. An unterminated string consumes the remainder, which is the safe
/// direction: it can only reduce the depth we observe, and the parser rejects
/// the file anyway.
fn skip_literal_string(bytes: &[u8], start: usize) -> usize {
    let mut index = start + 1;
    let mut nesting = 1usize;
    while index < bytes.len() {
        match bytes[index] {
            b'\\' => index += 2,
            b'(' => {
                nesting += 1;
                index += 1;
            }
            b')' => {
                nesting -= 1;
                index += 1;
                if nesting == 0 {
                    return index;
                }
            }
            _ => index += 1,
        }
    }
    bytes.len()
}

fn skip_hex_string(bytes: &[u8], start: usize) -> usize {
    let mut index = start + 1;
    while index < bytes.len() && bytes[index] != b'>' {
        index += 1;
    }
    index.saturating_add(1).min(bytes.len())
}

/// True when `index` begins the `stream` keyword as a token: preceded by a
/// delimiter or whitespace and followed by an end-of-line marker, per PDF 1.7
/// §7.3.8.1. `endstream` is excluded because its `s` is preceded by `d`.
fn starts_stream_keyword(bytes: &[u8], index: usize) -> bool {
    const KEYWORD: &[u8] = b"stream";
    if bytes.len() < index + KEYWORD.len() || &bytes[index..index + KEYWORD.len()] != KEYWORD {
        return false;
    }
    let preceded_ok = match index.checked_sub(1).map(|previous| bytes[previous]) {
        None => true,
        Some(byte) => is_whitespace(byte) || byte == b'>',
    };
    let followed_ok = matches!(bytes.get(index + KEYWORD.len()), Some(b'\r') | Some(b'\n'));
    preceded_ok && followed_ok
}

/// Stream payloads are arbitrary bytes — compressed data routinely contains
/// `<<`, `[`, and `(`. Counting them as structure would refuse ordinary files,
/// so the whole payload is skipped. An unterminated payload consumes the
/// remainder.
fn skip_stream_payload(bytes: &[u8], start: usize) -> usize {
    const END: &[u8] = b"endstream";
    let from = start + b"stream".len();
    match bytes[from..]
        .windows(END.len())
        .position(|candidate| candidate == END)
    {
        Some(offset) => from + offset + END.len(),
        None => bytes.len(),
    }
}

const fn is_whitespace(byte: u8) -> bool {
    matches!(byte, b'\0' | b'\t' | b'\n' | 0x0c | b'\r' | b' ')
}
