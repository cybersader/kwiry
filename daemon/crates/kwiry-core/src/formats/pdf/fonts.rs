// SPDX-License-Identifier: MIT OR Apache-2.0

//! Project-owned font model.
//!
//! `lopdf` exposes no font metrics: `src/font.rs` is a font *builder* for
//! embedding, and `Dictionary::get_font_encoding` only turns bytes into a
//! `String`. `Document::get_page_fonts` hands back the raw `&Dictionary`. So
//! every width used to advance the text matrix is read out of the font
//! dictionary here.
//!
//! Two rules this module exists to enforce:
//!
//! * **Codes are segmented before decoding.** `Encoding::write_to_string` is
//!   string-level and `bytes_to_string` silently `continue`s on an unmapped
//!   byte, so the decoded character count is not the code count. Zipping
//!   decoded characters against codes would therefore misalign text and
//!   geometry. Each code is decoded on its own instead.
//! * **A width that was guessed is labelled.** `exact` is `true` only when the
//!   number came out of `/Widths` or `/W`. Every fallback — `/MissingWidth`,
//!   `/DW`, the 1000 default for a CID font with neither — clears it, and the
//!   flag rides out on the run so the segmentation step can widen its
//!   thresholds instead of trusting a number nobody measured.

use std::collections::BTreeMap;

use lopdf::{Dictionary, Document, Encoding, Object};

use super::cmap::{self, LegacyCharset};
use super::embedded::{self, GlyphMap};
use super::limits;

/// How many bytes one character code occupies in a show operand.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum CodeWidth {
    One,
    Two,
}

impl CodeWidth {
    pub(super) const fn bytes(self) -> usize {
        match self {
            Self::One => 1,
            Self::Two => 2,
        }
    }
}

#[derive(Debug, Clone)]
enum Widths {
    /// Simple font: `/Widths` indexed from `/FirstChar`, `/MissingWidth`
    /// otherwise.
    Simple {
        first_char: i64,
        widths: Vec<f64>,
        missing: f64,
    },
    /// Composite font: sorted `/W` ranges, `/DW` otherwise. PDF 1.7 §9.7.4.3
    /// makes the `/DW` default 1000, which is why a Type0 font with neither is
    /// still 1000 — and still `exact == false`.
    Composite {
        ranges: Vec<(u32, u32, f64)>,
        default_width: f64,
    },
    /// No usable metric source. The dominant real case: base-14 Type1 fonts
    /// carry no `/Widths`, no `/FirstChar`, and no `/FontDescriptor`.
    Unknown { missing: f64 },
}

/// How a font's character codes become characters.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum FontMapping {
    /// Through the font's own encoding: a `/ToUnicode` CMap, a simple-font
    /// encoding, or an Identity/Unicode predefined CMap. Both extraction tiers
    /// read these identically.
    Encoded,
    /// Through a predefined legacy CJK CMap. `lopdf` would silently fall back
    /// to `STANDARD_ENCODING` here; both tiers decode it properly instead. See
    /// `super::cmap`.
    Legacy(LegacyCharset),
    /// `Identity-H`/`Identity-V` with no `/ToUnicode`: the codes are glyph ids
    /// meaningful only inside the embedded font program. Classified in both
    /// tiers; recovered only in the enhanced one. See `super::embedded`.
    EmbeddedGlyphIds,
}

#[derive(Debug, Clone)]
pub(super) struct FontModel {
    pub(super) code_width: CodeWidth,
    pub(super) mapping: FontMapping,
    pub(super) vertical: bool,
    /// `/Subtype /Type3` glyph widths are in glyph space scaled by
    /// `/FontMatrix`, not by 1/1000. This reader does not implement that, so a
    /// Type3 font never reports an exact width.
    pub(super) type3: bool,
    /// The font declared more width entries than [`limits::MAX_FONT_WIDTH_ENTRIES`]
    /// and the tail was not retained. Codes past the cap take the
    /// `/MissingWidth` or `/DW` path, which already clears `exact`.
    pub(super) widths_truncated: bool,
    widths: Widths,
}

impl FontModel {
    /// Bytes occupied by the code that starts at `first`.
    ///
    /// Fixed for every mapping this reader handles except the legacy CJK
    /// codespaces, which are mixed-width by construction.
    pub(super) fn code_len(&self, first: u8) -> usize {
        match self.mapping {
            FontMapping::Encoded | FontMapping::EmbeddedGlyphIds => self.code_width.bytes(),
            FontMapping::Legacy(charset) => charset.code_len(first),
        }
    }

    /// The legacy CMap this font is coded in, if any.
    pub(super) fn legacy_charset(&self) -> Option<LegacyCharset> {
        match self.mapping {
            FontMapping::Legacy(charset) => Some(charset),
            FontMapping::Encoded | FontMapping::EmbeddedGlyphIds => None,
        }
    }

    /// Horizontal displacement for `code`, in text-space units per unit font
    /// size (i.e. glyph-space thousandths already divided by 1000), plus
    /// whether the number came from real font data.
    pub(super) fn glyph_width(&self, code: u32) -> (f64, bool) {
        if self.type3 {
            return (0.0, false);
        }
        match &self.widths {
            Widths::Simple {
                first_char,
                widths,
                missing,
            } => {
                let index = i64::from(code) - *first_char;
                let found = usize::try_from(index)
                    .ok()
                    .and_then(|index| widths.get(index))
                    .copied();
                match found {
                    Some(width) => (width / 1000.0, true),
                    None => (*missing / 1000.0, false),
                }
            }
            Widths::Composite {
                ranges,
                default_width,
            } => match lookup_range(ranges, code) {
                Some(width) => (width / 1000.0, true),
                None => (*default_width / 1000.0, false),
            },
            Widths::Unknown { missing } => (*missing / 1000.0, false),
        }
    }
}

fn lookup_range(ranges: &[(u32, u32, f64)], code: u32) -> Option<f64> {
    let index = ranges.partition_point(|(low, _, _)| *low <= code);
    let (low, high, width) = *ranges.get(index.checked_sub(1)?)?;
    (code >= low && code <= high).then_some(width)
}

/// A page's font resources, resolved once and reused across every `Tf`.
pub(super) struct PageFonts<'a> {
    models: BTreeMap<Vec<u8>, FontModel>,
    encodings: BTreeMap<Vec<u8>, Encoding<'a>>,
    /// Glyph-id maps recovered from embedded font programs. Populated only by
    /// the enhanced tier; a `FontMapping::EmbeddedGlyphIds` font with no entry
    /// here is one this build cannot decode.
    glyph_maps: BTreeMap<Vec<u8>, GlyphMap>,
    /// `true` when the page declared more font resources than
    /// [`limits::MAX_FONTS_PER_PAGE`] and the tail was dropped.
    pub(super) truncated: bool,
    /// `true` when any font on the page declared more width entries than
    /// [`limits::MAX_FONT_WIDTH_ENTRIES`].
    pub(super) widths_truncated: bool,
}

impl<'a> PageFonts<'a> {
    pub(super) fn resolve(document: &'a Document, page_id: lopdf::ObjectId) -> Self {
        let mut models = BTreeMap::new();
        let mut encodings = BTreeMap::new();
        let mut glyph_maps = BTreeMap::new();
        let mut truncated = false;
        let mut widths_truncated = false;

        let declared = document.get_page_fonts(page_id).unwrap_or_default();
        for (name, dictionary) in declared {
            if models.len() >= limits::MAX_FONTS_PER_PAGE {
                truncated = true;
                break;
            }
            let model = build_model(document, dictionary);
            widths_truncated |= model.widths_truncated;
            if model.mapping == FontMapping::EmbeddedGlyphIds
                && let Some(descendant) = descendant_font(document, dictionary)
                && let Some(map) = embedded::glyph_map(document, descendant)
            {
                glyph_maps.insert(name.clone(), map);
            }
            if let Ok(encoding) =
                dictionary.get_font_encoding_with_limit(document, limits::MAX_TOUNICODE_BYTES)
            {
                encodings.insert(name.clone(), encoding);
            }
            models.insert(name, model);
        }

        Self {
            models,
            encodings,
            glyph_maps,
            truncated,
            widths_truncated,
        }
    }

    /// `true` when this build cannot turn the font's codes into characters at
    /// all. The caller declines the whole source rather than emitting the
    /// subset it happens to be able to read.
    pub(super) fn is_undecodable(&self, name: &[u8], model: &FontModel) -> bool {
        model.mapping == FontMapping::EmbeddedGlyphIds && !self.glyph_maps.contains_key(name)
    }

    pub(super) fn model(&self, name: &[u8]) -> Option<&FontModel> {
        self.models.get(name)
    }

    /// Decode exactly one character code. Returns `None` when the font has no
    /// resolvable encoding or the code maps to nothing, which is a *skip*, not
    /// a substitution: inventing a replacement character would put bytes into
    /// the index that the author never wrote.
    pub(super) fn decode_code(
        &self,
        name: &[u8],
        model: &FontModel,
        code_bytes: &[u8],
        out: &mut String,
    ) {
        let before = out.len();
        if let Some(charset) = model.legacy_charset() {
            cmap::decode(charset, code_bytes, out);
        } else if model.mapping == FontMapping::EmbeddedGlyphIds {
            // Two-byte big-endian glyph id. A glyph the font's `cmap` does not
            // reach contributes nothing rather than a replacement character.
            if let Some(map) = self.glyph_maps.get(name)
                && let [high, low] = code_bytes
                && let Some(character) = map.get(&u16::from_be_bytes([*high, *low]))
            {
                out.push(*character);
            }
        } else if let Some(encoding) = self.encodings.get(name) {
            let _ = encoding.write_to_string(code_bytes, out);
        }
        drop_untext(out, before);
    }
}

/// Remove anything from `out[from..]` that is not renderable text.
///
/// Every decoding path can reach one. A glyph id whose `cmap` entry is a C0
/// control inverts to that control — an `Identity-H` subset with no
/// `/ToUnicode` and a `0..=0xFFFF → gid 1` group put a literal `U+0000` into a
/// section — and a simple font's `/Encoding` can name one directly.
///
/// That matters beyond tidiness. `formats::decode_utf8` refuses any file
/// containing a NUL byte as `binary_source` for every text format; PDF sections
/// do not route through it, so a NUL here would travel into `Chunk.content`,
/// the index, and the `POST /v0/search` JSON envelope having bypassed the rule
/// every other format obeys. `U+0009` and `U+000A` are equally unwelcome: the
/// segmentation layer above owns tabs and newlines as *structure*, so a font
/// that draws one would forge a cell or line boundary that the author did not.
///
/// The common case adds no control and pays only a scan of the bytes just
/// appended.
fn drop_untext(out: &mut String, from: usize) {
    if !out[from..].chars().any(is_untext) {
        return;
    }
    let kept: String = out[from..].chars().filter(|c| !is_untext(*c)).collect();
    out.truncate(from);
    out.push_str(&kept);
}

/// C0 and C1 controls, `U+007F`, and the replacement character `encoding_rs`
/// emits for input it cannot map.
fn is_untext(character: char) -> bool {
    character.is_control() || character == '\u{fffd}'
}

fn build_model(document: &Document, dictionary: &Dictionary) -> FontModel {
    let subtype = dictionary
        .get(b"Subtype")
        .and_then(Object::as_name)
        .unwrap_or(b"");
    let type3 = subtype == b"Type3";

    if subtype == b"Type0" {
        let vertical = is_vertical(dictionary);
        let descendant = dictionary
            .get_deref(b"DescendantFonts", document)
            .ok()
            .and_then(|object| match object {
                Object::Array(array) => array.first(),
                other => Some(other),
            })
            .and_then(|object| resolve_dict(document, object));
        let (widths, widths_truncated) = descendant
            .map(|descendant| composite_widths(document, descendant))
            .unwrap_or((
                Widths::Composite {
                    ranges: Vec::new(),
                    default_width: 1000.0,
                },
                false,
            ));
        // Classified on the CMap name alone, deliberately, even when the font
        // also carries a `/ToUnicode` map. `lopdf` resolves `/Encoding` first
        // and never reaches `/ToUnicode` when one is present
        // (`Dictionary::get_font_encoding_inner`), and it has no table for
        // these names — so it falls back to `STANDARD_ENCODING` and reads
        // Shift_JIS bytes as Latin. Deferring to the `/ToUnicode` map here
        // would be describing a code path that does not run.
        let encoding_name = dictionary.get(b"Encoding").and_then(Object::as_name).ok();
        if let Some(charset) = encoding_name.and_then(cmap::legacy_charset) {
            // `/W` is indexed by CID, and resolving a legacy code to a CID
            // needs the Adobe CID tables neither tier ships. So no width here
            // came from font data, whatever `/W` says. Reporting the default
            // and clearing `exact` keeps the run honest and makes the
            // segmentation step widen its thresholds instead of trusting a
            // number that indexes the wrong table.
            let missing = match &widths {
                Widths::Composite { default_width, .. } => *default_width,
                Widths::Simple { missing, .. } | Widths::Unknown { missing } => *missing,
            };
            return FontModel {
                // Unused: `code_len` routes every legacy code through the
                // charset's own mixed-width rule. Set to the narrower value so
                // a future caller that ignored `code_len` would under-segment
                // visibly rather than silently swallow a byte.
                code_width: CodeWidth::One,
                mapping: FontMapping::Legacy(charset),
                vertical,
                type3: false,
                widths_truncated,
                widths: Widths::Unknown { missing },
            };
        }

        // An identity CMap with no `/ToUnicode` codes glyph ids, not
        // characters. Classified in both tiers so both agree on exactly which
        // sources diverge; only the enhanced tier can act on it.
        let identity =
            encoding_name.is_some_and(|name| name == b"Identity-H" || name == b"Identity-V");
        let mapping = if identity && dictionary.get(b"ToUnicode").is_err() {
            FontMapping::EmbeddedGlyphIds
        } else {
            FontMapping::Encoded
        };

        return FontModel {
            // Every CMap this reader can decode — `/Identity-H`, `/Identity-V`,
            // `UniGB-UCS2-H`, `UniGB-UTF16-H`, and a `/ToUnicode` map on a
            // composite font — is two bytes per code.
            code_width: CodeWidth::Two,
            mapping,
            vertical,
            type3: false,
            widths_truncated,
            widths,
        };
    }

    let (widths, widths_truncated) = simple_widths(document, dictionary);
    FontModel {
        code_width: CodeWidth::One,
        mapping: FontMapping::Encoded,
        vertical: false,
        type3,
        widths_truncated,
        widths,
    }
}

/// The first `/DescendantFonts` entry of a composite font.
fn descendant_font<'a>(
    document: &'a Document,
    dictionary: &'a Dictionary,
) -> Option<&'a Dictionary> {
    dictionary
        .get_deref(b"DescendantFonts", document)
        .ok()
        .and_then(|object| match object {
            Object::Array(array) => array.first(),
            other => Some(other),
        })
        .and_then(|object| resolve_dict(document, object))
}

fn is_vertical(dictionary: &Dictionary) -> bool {
    dictionary
        .get(b"Encoding")
        .and_then(Object::as_name)
        .map(|name| name.ends_with(b"-V") || name == b"Identity-V")
        .unwrap_or(false)
}

fn resolve_dict<'a>(document: &'a Document, object: &'a Object) -> Option<&'a Dictionary> {
    match object {
        Object::Dictionary(dictionary) => Some(dictionary),
        Object::Reference(id) => document.get_dictionary(*id).ok(),
        _ => None,
    }
}

fn simple_widths(document: &Document, dictionary: &Dictionary) -> (Widths, bool) {
    let missing = dictionary
        .get_deref(b"FontDescriptor", document)
        .ok()
        .and_then(|object| resolve_dict(document, object))
        .and_then(|descriptor| descriptor.get(b"MissingWidth").ok())
        .and_then(number)
        .unwrap_or(0.0);

    let first_char = dictionary
        .get_deref(b"FirstChar", document)
        .ok()
        .and_then(|object| object.as_i64().ok());
    let declared = dictionary
        .get_deref(b"Widths", document)
        .ok()
        .and_then(|object| object.as_array().ok());
    let truncated = declared.is_some_and(|array| array.len() > limits::MAX_FONT_WIDTH_ENTRIES);
    let widths = declared.map(|array| {
        array
            .iter()
            .take(limits::MAX_FONT_WIDTH_ENTRIES)
            .map(|entry| number(entry).unwrap_or(0.0))
    });

    match (first_char, widths) {
        (Some(first_char), Some(widths)) => (
            Widths::Simple {
                first_char,
                widths: widths.collect(),
                missing,
            },
            truncated,
        ),
        _ => (Widths::Unknown { missing }, truncated),
    }
}

/// `/W` accepts two interleaved forms (PDF 1.7 §9.7.4.3):
/// `c [w1 w2 …]` and `c_first c_last w`. Both are flattened into sorted
/// non-overlapping ranges so a lookup is a binary search rather than a scan.
fn composite_widths(document: &Document, descendant: &Dictionary) -> (Widths, bool) {
    let default_width = descendant
        .get_deref(b"DW", document)
        .ok()
        .and_then(number)
        .unwrap_or(1000.0);

    let mut ranges: Vec<(u32, u32, f64)> = Vec::new();
    let mut truncated = false;
    if let Ok(entries) = descendant
        .get_deref(b"W", document)
        .and_then(Object::as_array)
    {
        let mut index = 0usize;
        while index < entries.len() {
            if ranges.len() >= limits::MAX_FONT_WIDTH_ENTRIES {
                truncated = true;
                break;
            }
            let Some(first) = number(&entries[index]).and_then(finite_code) else {
                index += 1;
                continue;
            };
            match entries.get(index + 1) {
                Some(Object::Array(widths)) => {
                    for (offset, width) in widths.iter().enumerate() {
                        let Ok(offset) = u32::try_from(offset) else {
                            break;
                        };
                        let Some(code) = first.checked_add(offset) else {
                            break;
                        };
                        ranges.push((code, code, number(width).unwrap_or(0.0)));
                    }
                    index += 2;
                }
                Some(second) => {
                    let last = number(second).and_then(finite_code).unwrap_or(first);
                    let width = entries
                        .get(index + 2)
                        .and_then(number)
                        .unwrap_or(default_width);
                    ranges.push((first, last.max(first), width));
                    index += 3;
                }
                None => break,
            }
        }
    }

    ranges.sort_by_key(|(low, high, _)| (*low, *high));
    ranges.dedup_by_key(|(low, _, _)| *low);
    (
        Widths::Composite {
            ranges,
            default_width,
        },
        truncated,
    )
}

fn finite_code(value: f64) -> Option<u32> {
    (value.is_finite() && (0.0..=f64::from(u32::MAX)).contains(&value)).then_some(value as u32)
}

pub(super) fn number(object: &Object) -> Option<f64> {
    match object {
        Object::Integer(value) => Some(*value as f64),
        Object::Real(value) => Some(f64::from(*value)),
        _ => None,
    }
}
