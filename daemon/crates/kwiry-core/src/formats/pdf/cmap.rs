// SPDX-License-Identifier: MIT OR Apache-2.0

//! Predefined CJK CMaps. **Shared by both extraction tiers.**
//!
//! # What this fixes
//!
//! A composite (`/Type0`) font selects its character codes with a CMap. The
//! predefined *legacy* CMaps — `90ms-RKSJ-H` (Shift_JIS), `EUC-H` (EUC-JP),
//! `GBK-EUC-H` (GBK), `ETen-B5-H` (Big5), `KSCms-UHC-H` (UHC) and their
//! relatives — code their glyphs as legacy multi-byte character codes with a
//! **mixed codespace**: one byte for ASCII, two or three for the rest.
//!
//! `lopdf` has no table for these names. `Dictionary::get_font_encoding_inner`
//! resolves `/Encoding` first, fails to match, warns, and falls back to
//! `STANDARD_ENCODING` — so a Shift_JIS document read through it produces
//! Latin letters for kanji bytes. That is worse than producing nothing: it puts
//! text into the index that the author never wrote.
//!
//! Decoding those bytes correctly needs the legacy decoder tables, which are
//! already in the portable graph: `quick-xml` enables `encoding_rs` for DOCX.
//! So this is not a tier divergence — it costs the plugin bundle nothing and
//! **both tiers do it identically**. The enhanced tier's actual divergence
//! lives in `super::embedded`.
//!
//! Resolving these CMaps the way Adobe specifies — code → CID → Unicode through
//! the Adobe-Japan1/GB1/CNS1/Korea1 tables — would need multiple megabytes of
//! data neither tier ships. Decoding the code bytes with the encoding the CMap
//! names is the approximation both tiers make, together, so no source is
//! segmented one way here and another way there.

/// The legacy byte encoding a predefined CMap's codespace is expressed in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum LegacyCharset {
    /// Adobe-Japan1 `…-RKSJ-…`: Shift_JIS.
    ShiftJis,
    /// Adobe-Japan1 `EUC-…`: EUC-JP.
    EucJp,
    /// Adobe-GB1 `GB-EUC-…`, `GBK-EUC-…`, `GBK2K-…`, `GBpc-EUC-…`.
    Gbk,
    /// Adobe-CNS1 `B5pc-…`, `ETen-B5-…`, `ETenms-B5-…`, `HKscs-B5-…`.
    Big5,
    /// Adobe-Korea1 `KSC-EUC-…`, `KSCms-UHC-…`, `KSCpc-EUC-…`.
    Uhc,
}

impl LegacyCharset {
    /// How many bytes the code starting with `first` occupies.
    ///
    /// These codespaces are exactly the legacy encodings' own byte structures,
    /// which is what makes a byte-level decode correct rather than a guess. A
    /// lead byte with no continuation at the end of a show operand yields a
    /// short slice, which the decoder then maps to nothing — the same "skip,
    /// never substitute" rule the rest of the reader follows.
    pub(super) const fn code_len(self, first: u8) -> usize {
        match self {
            // JIS X 0201 in 0x00..=0x80 and 0xA1..=0xDF; two-byte lead
            // elsewhere (PDF 1.7 §9.7.5.2, Adobe-Japan1 RKSJ codespace).
            Self::ShiftJis => match first {
                0x81..=0x9F | 0xE0..=0xFC => 2,
                _ => 1,
            },
            // 0x8E selects half-width katakana (two bytes total), 0x8F selects
            // JIS X 0212 (three bytes total).
            Self::EucJp => match first {
                0x8E => 2,
                0x8F => 3,
                0xA1..=0xFE => 2,
                _ => 1,
            },
            Self::Gbk | Self::Big5 | Self::Uhc => match first {
                0x81..=0xFE => 2,
                _ => 1,
            },
        }
    }
}

/// Classify a `/Encoding` CMap name.
///
/// `None` means the name is not a legacy predefined CMap — either it is one of
/// the Identity/Unicode families the reader already handles, or it is an
/// embedded CMap stream rather than a name, or the font is not composite. Those
/// all keep their existing behaviour in both tiers.
pub(super) fn legacy_charset(name: &[u8]) -> Option<LegacyCharset> {
    // Horizontal and vertical variants of a CMap share a codespace; the writing
    // mode is handled separately and does not change how codes are decoded.
    let base = name
        .strip_suffix(b"-H")
        .or_else(|| name.strip_suffix(b"-V"))
        .unwrap_or(name);

    // `Uni…-UCS2` and `Uni…-UTF16` are Unicode-coded, not legacy-coded, and
    // `Identity` is resolved through `/ToUnicode`. Excluded before the table so
    // a new `Uni…` family cannot be misfiled by a prefix match.
    if base.starts_with(b"Identity") || base.starts_with(b"Uni") {
        return None;
    }

    // Closed table, never a prefix match. The Adobe-Japan1 `H`/`V`, `Add-H`,
    // and `Ext-H` CMaps are ISO-2022-coded rather than EUC-coded, and
    // `CNS-EUC-H` is EUC-TW rather than Big5; none of them is listed, because
    // classifying a font under the wrong encoding is worse than declining it.
    match base {
        b"83pv-RKSJ" | b"90ms-RKSJ" | b"90msp-RKSJ" | b"90pv-RKSJ" | b"Add-RKSJ" | b"Ext-RKSJ" => {
            Some(LegacyCharset::ShiftJis)
        }
        b"EUC" => Some(LegacyCharset::EucJp),
        b"GB-EUC" | b"GBpc-EUC" | b"GBK-EUC" | b"GBKp-EUC" | b"GBK2K" => Some(LegacyCharset::Gbk),
        b"B5pc" | b"HKscs-B5" | b"ETen-B5" | b"ETenms-B5" => Some(LegacyCharset::Big5),
        b"KSC-EUC" | b"KSCpc-EUC" | b"KSCms-UHC" | b"KSCms-UHC-HW" => Some(LegacyCharset::Uhc),
        _ => None,
    }
}

/// Decode one legacy character code.
///
/// Codes are decoded individually rather than as a run so
/// the decoded text stays aligned with the per-code geometry the reader
/// measures; each of these codespaces is self-contained per code, so decoding
/// one at a time loses nothing.
///
/// A code the encoding does not map contributes nothing — never a replacement
/// character. `encoding_rs` emits U+FFFD for unmappable input, so that is
/// filtered here rather than written into the index as text nobody wrote.
pub(super) fn decode(charset: LegacyCharset, code_bytes: &[u8], out: &mut String) {
    let encoding = match charset {
        LegacyCharset::ShiftJis => encoding_rs::SHIFT_JIS,
        LegacyCharset::EucJp => encoding_rs::EUC_JP,
        LegacyCharset::Gbk => encoding_rs::GBK,
        LegacyCharset::Big5 => encoding_rs::BIG5,
        LegacyCharset::Uhc => encoding_rs::EUC_KR,
    };
    let (decoded, _) = encoding.decode_without_bom_handling(code_bytes);
    out.extend(decoded.chars().filter(|character| *character != '\u{fffd}'));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn horizontal_and_vertical_variants_classify_identically() {
        assert_eq!(
            legacy_charset(b"90ms-RKSJ-H"),
            Some(LegacyCharset::ShiftJis)
        );
        assert_eq!(
            legacy_charset(b"90ms-RKSJ-V"),
            Some(LegacyCharset::ShiftJis)
        );
        assert_eq!(legacy_charset(b"GBK-EUC-H"), Some(LegacyCharset::Gbk));
        assert_eq!(legacy_charset(b"ETen-B5-V"), Some(LegacyCharset::Big5));
        assert_eq!(legacy_charset(b"KSCms-UHC-H"), Some(LegacyCharset::Uhc));
    }

    #[test]
    fn unicode_and_identity_families_are_not_legacy() {
        for name in [
            &b"Identity-H"[..],
            b"Identity-V",
            b"UniJIS-UCS2-H",
            b"UniGB-UTF16-H",
            b"UniKS-UCS2-H",
            b"UniCNS-UTF32-V",
        ] {
            assert_eq!(legacy_charset(name), None, "{name:?}");
        }
    }

    #[test]
    fn an_unknown_name_is_not_guessed_at() {
        assert_eq!(legacy_charset(b"NotACMap"), None);
        assert_eq!(legacy_charset(b""), None);
    }

    #[test]
    fn shift_jis_codespace_is_mixed_width() {
        assert_eq!(LegacyCharset::ShiftJis.code_len(b'A'), 1);
        assert_eq!(LegacyCharset::ShiftJis.code_len(0xB1), 1);
        assert_eq!(LegacyCharset::ShiftJis.code_len(0x82), 2);
        assert_eq!(LegacyCharset::ShiftJis.code_len(0xE0), 2);
    }

    #[test]
    fn euc_jp_codespace_admits_a_three_byte_code() {
        assert_eq!(LegacyCharset::EucJp.code_len(0x8E), 2);
        assert_eq!(LegacyCharset::EucJp.code_len(0x8F), 3);
        assert_eq!(LegacyCharset::EucJp.code_len(0xA4), 2);
        assert_eq!(LegacyCharset::EucJp.code_len(b'x'), 1);
    }

    #[test]
    fn legacy_codes_decode_in_both_tiers() {
        let mut out = String::new();
        // Shift_JIS 0x82 0xA0 is HIRAGANA LETTER A.
        decode(LegacyCharset::ShiftJis, &[0x82, 0xA0], &mut out);
        assert_eq!(out, "あ");
        out.clear();
        decode(LegacyCharset::ShiftJis, b"A", &mut out);
        assert_eq!(out, "A");
    }

    #[test]
    fn an_unmappable_legacy_code_contributes_nothing() {
        let mut out = String::new();
        decode(LegacyCharset::ShiftJis, &[0x82], &mut out);
        assert_eq!(out, "");
    }
}
