// SPDX-License-Identifier: MIT OR Apache-2.0

//! The enhanced tier: Unicode recovered from an embedded font program.
//!
//! # The gap
//!
//! A `/Type0` font with `/Encoding /Identity-H` codes its glyphs by **glyph
//! id** — a number meaningful only inside that one embedded font file. The
//! `/ToUnicode` map is what turns those numbers back into characters, and it is
//! *optional*. Subsetting producers routinely omit it: LaTeX with `pdftex`,
//! older InDesign exports, and most "print to PDF" paths emit `Identity-H`
//! subsets with no `/ToUnicode` at all.
//!
//! Such a document renders perfectly and yields **no text whatsoever**. It is
//! one of the two largest real-world reasons a PDF search returns nothing (the
//! other being a scan with no text layer).
//!
//! The recovery is well-defined: the embedded font program carries a `cmap`
//! table mapping Unicode → glyph id, and inverting it recovers glyph id →
//! Unicode. That needs a font parser, and it needs to parse an arbitrary,
//! author-supplied, multi-megabyte binary.
//!
//! # Why this is the tier divergence and the CJK CMaps are not
//!
//! `super::cmap` needs `encoding_rs`, which the portable graph already carries
//! for DOCX — so it costs the plugin bundle nothing and both tiers do it. This
//! needs `ttf-parser`, which nothing in the portable graph pulls, and it runs
//! a whole font parser over an unbounded embedded blob inside Obsidian's
//! worker heap. So it is compiled only under `native-pdf-extractor`, which also
//! requires `native` and therefore cannot be selected by a wasm32 build.
//!
//! # The divergence is coverage, never segmentation
//!
//! Which tier is compiled changes **whether** a source is indexed, never
//! **how** an indexed source is segmented. A font this build cannot decode
//! declines the whole source — see `super::cmap` for why partial decoding is
//! the worse failure — so the portable tier contributes no chunk identity at
//! all and switching tiers is a pure insertion.

use std::collections::BTreeMap;

#[cfg(feature = "native-pdf-extractor")]
use lopdf::Object;
use lopdf::{Dictionary, Document};

#[cfg(feature = "native-pdf-extractor")]
use super::limits;

/// Glyph id to the character it renders, inverted out of the font's `cmap`.
pub(super) type GlyphMap = BTreeMap<u16, char>;

/// Largest Unicode scalar value. The inversion walk's domain, and the exact
/// domain `char::from_u32` already admitted.
#[cfg(feature = "native-pdf-extractor")]
const UNICODE_MAX: u32 = 0x10_FFFF;

/// Recover a glyph-id map for a composite font's descendant, or `None` when
/// this build or this font cannot produce one.
///
/// `None` is not a failure to report loudly — it is the ordinary answer in the
/// portable tier, and in the enhanced tier it is the honest answer for a font
/// that is not embedded, not a parseable format, or not identity-mapped. Every
/// one of those cases declines the source rather than guessing.
#[cfg(feature = "native-pdf-extractor")]
pub(super) fn glyph_map(document: &Document, descendant: &Dictionary) -> Option<GlyphMap> {
    // `/CIDToGIDMap` may be a stream remapping CID to glyph id. Applying it is
    // a separate capability; without it the codes are not glyph ids and the
    // recovery below would be reading the wrong table, so decline instead.
    match descendant.get(b"CIDToGIDMap") {
        Ok(Object::Name(name)) if name.as_slice() == b"Identity" => {}
        Ok(Object::Null) | Err(_) => {}
        Ok(_) => return None,
    }

    let program = font_program(document, descendant)?;
    let face = ttf_parser::Face::parse(&program, 0).ok()?;
    let cmap = face.tables().cmap?;

    let mut map = GlyphMap::new();
    let mut examined = 0usize;
    for subtable in cmap.subtables {
        // A Macintosh or symbol subtable maps code points that are not Unicode,
        // so inverting it would produce characters nobody wrote.
        if !subtable.is_unicode() {
            continue;
        }
        if examined >= limits::MAX_GLYPH_MAP_SUBTABLES || map.len() >= limits::MAX_GLYPH_MAP_ENTRIES
        {
            break;
        }
        examined += 1;
        // Deliberately *not* `Subtable::codepoints`. That callback iterates
        // `start_char_code..=end_char_code` for every group a format-12
        // subtable declares, with no cap of its own and no check that the end
        // is a Unicode scalar value — one group may span the whole `u32` range.
        // The cap below bounded the map, not the walk, so it never fired: a
        // 1,249-byte PDF declaring one `0..=0xFFFFFFFF` group cost 2.5 s, and a
        // 1,753-byte one declaring 20,000 such groups did not finish in an hour.
        //
        // Probing the Unicode scalar range instead is bounded by construction
        // and loses nothing: `char::from_u32` already discarded everything
        // outside it, so no code point that could ever have reached the map is
        // skipped. Ascending order also makes "first entry wins" independent of
        // how the font happens to order its groups.
        for code_point in 0..=UNICODE_MAX {
            if map.len() >= limits::MAX_GLYPH_MAP_ENTRIES {
                break;
            }
            let Some(character) = char::from_u32(code_point) else {
                continue;
            };
            if let Some(glyph) = subtable.glyph_index(code_point) {
                // First subtable wins, so the result does not depend on how
                // many equivalent Unicode subtables the font happens to ship.
                map.entry(glyph.0).or_insert(character);
            }
        }
    }

    (!map.is_empty()).then_some(map)
}

#[cfg(not(feature = "native-pdf-extractor"))]
pub(super) fn glyph_map(_document: &Document, _descendant: &Dictionary) -> Option<GlyphMap> {
    None
}

/// The embedded font program, bounded on decompression.
///
/// `/FontFile2` is TrueType. `/FontFile3` is CFF-family; only its `/OpenType`
/// subtype carries the sfnt wrapper (and therefore the `cmap` table) that the
/// recovery needs, so a bare `/Type1C` or `/CIDFontType0C` is declined rather
/// than fed to a parser that would reject it anyway.
#[cfg(feature = "native-pdf-extractor")]
fn font_program(document: &Document, descendant: &Dictionary) -> Option<Vec<u8>> {
    let descriptor = descendant
        .get_deref(b"FontDescriptor", document)
        .ok()
        .and_then(|object| match object {
            Object::Dictionary(dictionary) => Some(dictionary),
            Object::Reference(id) => document.get_dictionary(*id).ok(),
            _ => None,
        })?;

    for key in [b"FontFile2".as_slice(), b"FontFile3".as_slice()] {
        let Ok(stream) = descriptor
            .get_deref(key, document)
            .and_then(Object::as_stream)
        else {
            continue;
        };
        if key == b"FontFile3" {
            let subtype = stream.dict.get(b"Subtype").and_then(Object::as_name);
            if subtype.ok() != Some(b"OpenType".as_slice()) {
                continue;
            }
        }
        if let Ok(bytes) = stream.decompressed_content_with_limit(limits::MAX_EMBEDDED_FONT_BYTES) {
            return Some(bytes);
        }
    }
    None
}
