// SPDX-License-Identifier: MIT OR Apache-2.0

//! Operation-boundary splitter for a page's content stream.
//!
//! # Why this exists
//!
//! [`lopdf::content::Content::decode`] materializes the **whole** page as a
//! `Vec<Operation>` before returning a single operator. `MAX_OPERATIONS_PER_PAGE`
//! is a loop guard applied to that already-allocated vector, so it bounds
//! interpretation and not memory: a 33 KB PDF whose flate-compressed content
//! stream inflates to just under `MAX_CONTENT_STREAM_BYTES` of `q Q ` drove
//! 9.5 GB of peak RSS with the operation limit firing on schedule. Peak
//! allocation tracked the *decompressed stream*, at roughly 285 bytes of heap
//! per content byte, and `MAX_CONTENT_STREAM_BYTES` was the only thing between
//! an attacker and that number.
//!
//! That matters most in the tier that can be selected by a wasm32 build:
//! `internal-pdf-extractor` is a member of `portable`, an Obsidian worker
//! cannot allocate 9.5 GB, and Rust's allocation-failure path aborts rather
//! than unwinds — no `catch_unwind` would help.
//!
//! So the stream is decoded in **windows**: each window is at least
//! [`limits::MAX_CONTENT_WINDOW_BYTES`] long and ends exactly one byte past a
//! complete operation, its operations are interpreted, and its `Vec<Operation>`
//! is dropped before the next window is decoded. Peak allocation then tracks
//! the window, not the stream.
//!
//! # Why the boundaries are safe
//!
//! The scanner below mirrors `lopdf`'s own content-stream grammar
//! (`lopdf::parser::{content, operation, operand, operator}`) token for token:
//! the same whitespace set, the same comment rule, the same literal-string
//! escape and nesting rules, the same `<<`-versus-`<` disambiguation, the same
//! `BI … ID … EI` inline-image shape, and the same nesting caps. It computes
//! offsets only — it allocates nothing and builds no objects.
//!
//! Two independent guards cover the case where the mirror is nonetheless wrong:
//!
//! * Every window is decoded with `Content::decode_strict`, which fails unless
//!   the parser consumed the window exactly. A boundary that landed mid-token
//!   therefore cannot be interpreted as if it had been a clean split; it
//!   degrades to the lenient decode this module used before, with a notice.
//! * When the scanner cannot parse an operation, it stops there. `lopdf`'s
//!   `many0(operation)` stops in exactly the same place and silently discards
//!   the rest, so no readable content is lost — but the tail is now *declared*
//!   through `pdf_page_content_unparsable` instead of vanishing.
//!
//! `super::tests` pins the equivalence directly: for every content stream in
//! the suite, the windowed operation sequence is asserted equal to the
//! single-shot `Content::decode` sequence at window sizes 1, 2, 7, 64 and 4096,
//! so a splitter that diverged from `lopdf` would fail the suite rather than
//! the field.

/// Matches `lopdf::reader::MAX_NESTING_DEPTH`, the array/dictionary depth its
/// content parser accepts.
const MAX_NESTING_DEPTH: usize = 100;

/// Matches `lopdf::reader::MAX_BRACKET`, the `(`-nesting depth its literal
/// string parser accepts.
const MAX_BRACKET: usize = 100;

/// Outcome of scanning forward from one window start.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Window {
    /// `end` is one byte past a complete operation and at least the requested
    /// budget past the start. Scanning continues from `end`.
    Boundary { end: usize },
    /// `end` is the end of the stream: everything from the last operation on is
    /// whitespace and comments, which is what a well-formed stream ends with.
    End { end: usize },
    /// No further operation parses at `end`. `lopdf` stops here too.
    Unparsable { end: usize },
    /// The single operation spanning `start..end` declares more operands than
    /// [`limits::MAX_OPERANDS_PER_OPERATION`]. An operation is never split, so
    /// the only way to keep it from being materialized is not to decode it.
    Oversize { end: usize },
}

impl Window {
    pub(super) const fn end(self) -> usize {
        match self {
            Self::Boundary { end }
            | Self::End { end }
            | Self::Unparsable { end }
            | Self::Oversize { end } => end,
        }
    }
}

/// Scan forward from `start` for the first operation boundary at least `budget`
/// bytes along.
///
/// `budget` is a floor, never a ceiling: a single operation larger than the
/// budget — a `TJ` array holding an entire page, say — is never split, because
/// splitting it would change what the file says.
pub(super) fn window(content: &[u8], start: usize, budget: usize) -> Window {
    let mut scan = Scan {
        bytes: content,
        at: start,
        operands: 0,
    };
    // `_content` opens with `content_space`; a window boundary always lands
    // after one, so this is a no-op except on the first window.
    scan.content_space();
    let first = scan.at;

    loop {
        let before = scan.at;
        scan.operands = 0;
        if !scan.operation() {
            // A stream that ends in trailing comments is well-formed:
            // `_content` closes with `many0(terminated(comment, content_space))`.
            let mut tail = Scan {
                bytes: content,
                at: before,
                operands: 0,
            };
            tail.comments();
            tail.content_space();
            if tail.at >= content.len() {
                return Window::End { end: content.len() };
            }
            return Window::Unparsable { end: before };
        }
        if scan.operands > super::limits::MAX_OPERANDS_PER_OPERATION {
            // Flush whatever already parsed cleanly, so the oversize operation
            // is the sole content of the next window and can be skipped whole.
            if before > first {
                return Window::Boundary { end: before };
            }
            return Window::Oversize { end: scan.at };
        }
        if scan.at >= content.len() {
            return Window::End { end: content.len() };
        }
        if scan.at - start >= budget {
            return Window::Boundary { end: scan.at };
        }
    }
}

struct Scan<'a> {
    bytes: &'a [u8],
    at: usize,
    /// Objects parsed so far in the operation being scanned, counting nested
    /// array and dictionary members. Each one becomes an `Object` when the
    /// window is decoded, so this — not the operation's byte span — is what
    /// tracks the allocation. A one-megabyte literal string is a single
    /// operand; a one-megabyte `TJ` array is half a million of them.
    operands: usize,
}

impl Scan<'_> {
    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.at).copied()
    }

    fn starts_with(&self, tag: &[u8]) -> bool {
        self.bytes[self.at..].starts_with(tag)
    }

    /// `lopdf::parser::content_space`: only these four bytes, deliberately not
    /// the wider `is_whitespace` set.
    fn content_space(&mut self) {
        while matches!(self.peek(), Some(b' ' | b'\t' | b'\r' | b'\n')) {
            self.at += 1;
        }
    }

    /// `lopdf::parser::is_whitespace`.
    fn whitespace(&mut self) {
        while matches!(
            self.peek(),
            Some(b' ' | b'\t' | b'\n' | b'\r' | b'\0' | b'\x0c')
        ) {
            self.at += 1;
        }
    }

    /// `lopdf::parser::comment`: `%`, then everything up to an end-of-line, then
    /// the end-of-line itself. A comment that runs to the end of the stream
    /// without a terminator does not parse — matching `lopdf` exactly.
    fn comment(&mut self) -> bool {
        if self.peek() != Some(b'%') {
            return false;
        }
        let mut at = self.at + 1;
        while let Some(byte) = self.bytes.get(at).copied() {
            if byte == b'\r' || byte == b'\n' {
                break;
            }
            at += 1;
        }
        if self.bytes[at..].starts_with(b"\r\n") {
            self.at = at + 2;
        } else if matches!(self.bytes.get(at), Some(b'\r' | b'\n')) {
            self.at = at + 1;
        } else {
            return false;
        }
        true
    }

    fn comments(&mut self) {
        while self.comment() {}
    }

    /// `lopdf::parser::space`: whitespace and comments, interleaved.
    fn space(&mut self) {
        loop {
            let before = self.at;
            self.whitespace();
            self.comment();
            if self.at == before {
                return;
            }
        }
    }

    /// `lopdf::parser::operation`.
    fn operation(&mut self) -> bool {
        self.comments();
        let before = self.at;
        if self.inline_image() {
            return true;
        }
        self.at = before;
        while self.operand() {}
        if !self.operator() {
            self.at = before;
            return false;
        }
        self.content_space();
        true
    }

    /// `lopdf::parser::operator`: ASCII letters plus `*`, `'` and `"`.
    fn operator(&mut self) -> bool {
        let start = self.at;
        while let Some(byte) = self.peek() {
            if byte.is_ascii_alphabetic() || matches!(byte, b'*' | b'\'' | b'"') {
                self.at += 1;
            } else {
                break;
            }
        }
        self.at > start
    }

    /// `lopdf::parser::operand`: a direct object without references, then
    /// `content_space`.
    fn operand(&mut self) -> bool {
        let before = self.at;
        if !self.object(MAX_NESTING_DEPTH, false) {
            self.at = before;
            return false;
        }
        self.content_space();
        true
    }

    /// One direct object. `with_reference` mirrors `_direct_objects`, which
    /// admits `n g R` inside arrays and dictionaries but not as a top-level
    /// operand.
    fn object(&mut self, depth: usize, with_reference: bool) -> bool {
        self.operands += 1;
        if self.starts_with(b"null") {
            self.at += 4;
            return true;
        }
        if self.starts_with(b"true") {
            self.at += 4;
            return true;
        }
        if self.starts_with(b"false") {
            self.at += 5;
            return true;
        }
        if with_reference && self.reference() {
            return true;
        }
        if self.number() {
            return true;
        }
        if self.name() {
            return true;
        }
        if self.literal_string() {
            return true;
        }
        // `<<` is tried before `<` because `hexadecimal_string` fails on a
        // dictionary open anyway (`<` is not a hex digit and the next byte is
        // not `>`), so the order is an optimization, not a semantic choice.
        if self.dictionary(depth) {
            return true;
        }
        if self.hex_string() {
            return true;
        }
        if self.array(depth) {
            return true;
        }
        false
    }

    /// `lopdf::parser::reference`: `integer space integer space 'R'`.
    fn reference(&mut self) -> bool {
        let before = self.at;
        if !self.integer() {
            self.at = before;
            return false;
        }
        self.space();
        if !self.integer() {
            self.at = before;
            return false;
        }
        self.space();
        if self.peek() == Some(b'R') {
            self.at += 1;
            return true;
        }
        self.at = before;
        false
    }

    fn integer(&mut self) -> bool {
        let before = self.at;
        if matches!(self.peek(), Some(b'+' | b'-')) {
            self.at += 1;
        }
        let digits = self.at;
        while matches!(self.peek(), Some(byte) if byte.is_ascii_digit()) {
            self.at += 1;
        }
        if self.at == digits {
            self.at = before;
            return false;
        }
        true
    }

    /// `real` then `integer`, in that order: `real` requires a `.` and so is the
    /// longer match wherever both apply.
    fn number(&mut self) -> bool {
        let before = self.at;
        if matches!(self.peek(), Some(b'+' | b'-')) {
            self.at += 1;
        }
        let leading = self.at;
        while matches!(self.peek(), Some(byte) if byte.is_ascii_digit()) {
            self.at += 1;
        }
        let integral = self.at - leading;
        if self.peek() == Some(b'.') {
            let dot = self.at;
            self.at += 1;
            let fraction = self.at;
            while matches!(self.peek(), Some(byte) if byte.is_ascii_digit()) {
                self.at += 1;
            }
            // `digit1 '.' digit0` or `'.' digit1`; neither admits a bare `.`.
            if integral > 0 || self.at > fraction {
                return true;
            }
            self.at = dot;
        }
        if integral > 0 {
            return true;
        }
        self.at = before;
        false
    }

    /// `lopdf::parser::name`: `/`, then regular bytes, with `#` admitted only as
    /// the lead of a two-digit hex escape.
    fn name(&mut self) -> bool {
        if self.peek() != Some(b'/') {
            return false;
        }
        self.at += 1;
        loop {
            match self.peek() {
                Some(b'#') => {
                    let hex = self.bytes.get(self.at + 1..self.at + 3);
                    match hex {
                        Some(digits) if digits.iter().all(u8::is_ascii_hexdigit) => self.at += 3,
                        _ => return true,
                    }
                }
                Some(byte) if is_regular(byte) => self.at += 1,
                _ => return true,
            }
        }
    }

    /// `lopdf::parser::literal_string`, including nested parentheses and the
    /// full escape set.
    fn literal_string(&mut self) -> bool {
        if self.peek() != Some(b'(') {
            return false;
        }
        let before = self.at;
        self.at += 1;
        let mut depth = 1usize;
        while depth > 0 {
            match self.peek() {
                None => {
                    self.at = before;
                    return false;
                }
                Some(b'\\') => {
                    self.at += 1;
                    self.escape();
                }
                Some(b'(') => {
                    depth += 1;
                    if depth > MAX_BRACKET {
                        self.at = before;
                        return false;
                    }
                    self.at += 1;
                }
                Some(b')') => {
                    depth -= 1;
                    self.at += 1;
                }
                Some(_) => self.at += 1,
            }
        }
        true
    }

    /// `lopdf::parser::escape_sequence`: an end-of-line, one to three octal
    /// digits, or any single byte.
    fn escape(&mut self) {
        match self.peek() {
            None => {}
            Some(b'\r') => {
                self.at += 1;
                if self.peek() == Some(b'\n') {
                    self.at += 1;
                }
            }
            Some(byte) if (b'0'..=b'7').contains(&byte) => {
                let start = self.at;
                while self.at < start + 3
                    && matches!(self.peek(), Some(byte) if (b'0'..=b'7').contains(&byte))
                {
                    self.at += 1;
                }
            }
            Some(_) => self.at += 1,
        }
    }

    /// `lopdf::parser::hexadecimal_string`: hex digits separated by whitespace,
    /// closed by `>`.
    fn hex_string(&mut self) -> bool {
        if self.peek() != Some(b'<') {
            return false;
        }
        let before = self.at;
        self.at += 1;
        loop {
            self.whitespace();
            match self.peek() {
                Some(b'>') => {
                    self.at += 1;
                    return true;
                }
                Some(byte) if byte.is_ascii_hexdigit() => self.at += 1,
                _ => {
                    self.at = before;
                    return false;
                }
            }
        }
    }

    fn array(&mut self, depth: usize) -> bool {
        if self.peek() != Some(b'[') {
            return false;
        }
        let before = self.at;
        if depth == 0 {
            return false;
        }
        self.at += 1;
        self.space();
        while self.at < self.bytes.len() && self.peek() != Some(b']') {
            if !self.object(depth - 1, true) {
                self.at = before;
                return false;
            }
            self.space();
        }
        if self.peek() == Some(b']') {
            self.at += 1;
            return true;
        }
        self.at = before;
        false
    }

    fn dictionary(&mut self, depth: usize) -> bool {
        if !self.starts_with(b"<<") {
            return false;
        }
        let before = self.at;
        if depth == 0 {
            return false;
        }
        self.at += 2;
        self.space();
        loop {
            if self.starts_with(b">>") {
                self.at += 2;
                return true;
            }
            if !self.name() {
                self.at = before;
                return false;
            }
            self.space();
            if !self.object(depth - 1, true) {
                self.at = before;
                return false;
            }
            self.space();
        }
    }

    /// `lopdf::parser::inline_image`: `BI`, a bare dictionary body, `ID`, the
    /// image bytes, then `EI`.
    ///
    /// The image bytes are located by the whitespace-delimited `EI` search
    /// `lopdf` itself falls back to, rather than by recomputing the length from
    /// `/W`, `/H`, `/BPC` and `/CS`. That can end the image earlier than `lopdf`
    /// would on crafted data containing ` EI `, which is exactly the case
    /// `Content::decode_strict` rejects, so a divergence degrades to the lenient
    /// path with a notice rather than to a mis-split window.
    fn inline_image(&mut self) -> bool {
        if !self.starts_with(b"BI") {
            return false;
        }
        let before = self.at;
        self.at += 2;
        self.content_space();
        // `inner_dictionary` without the `<<`/`>>` wrapper.
        loop {
            if self.starts_with(b"ID") {
                break;
            }
            if !self.name() {
                self.at = before;
                return false;
            }
            self.space();
            if !self.object(MAX_NESTING_DEPTH, true) {
                self.at = before;
                return false;
            }
            self.space();
        }
        self.at += 2;
        self.content_space();
        let Some(found) = self.bytes[self.at..].windows(4).position(|bytes| {
            matches!(bytes[0], b' ' | b'\n' | b'\r')
                && bytes[1] == b'E'
                && bytes[2] == b'I'
                && matches!(bytes[3], b' ' | b'\n' | b'\r')
        }) else {
            self.at = before;
            return false;
        };
        self.at += found + 3;
        self.content_space();
        true
    }
}

/// `lopdf::parser::is_regular`.
const fn is_regular(byte: u8) -> bool {
    !matches!(byte, b' ' | b'\t' | b'\n' | b'\r' | b'\0' | b'\x0c')
        && !matches!(
            byte,
            b'(' | b')' | b'<' | b'>' | b'[' | b']' | b'{' | b'}' | b'/' | b'%'
        )
}
