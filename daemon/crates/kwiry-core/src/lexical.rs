use std::collections::BTreeSet;
use std::ops::Range;
use std::sync::OnceLock;

use regex::Regex;
use unicode_normalization::{UnicodeNormalization, char::is_combining_mark};

const MAX_NORMALIZED_EXACT_BYTES: usize = 4_096;

pub(crate) fn fold_lexical(value: &str) -> String {
    value
        .chars()
        .flat_map(char::to_lowercase)
        .nfd()
        .filter(|character| !is_combining_mark(*character))
        .collect()
}

pub fn normalize_lexical_value(value: &str) -> Option<String> {
    normalize_raw(value)
}

pub(crate) fn normalize_raw(value: &str) -> Option<String> {
    let collapsed = value.split_whitespace().collect::<Vec<_>>().join(" ");
    let normalized = fold_lexical(&collapsed);
    if normalized.is_empty() || normalized.len() > MAX_NORMALIZED_EXACT_BYTES {
        return None;
    }
    Some(normalized)
}

pub(crate) fn technical_identifier_spans(source: &str) -> Vec<(Range<usize>, String)> {
    identifier_pattern()
        .find_iter(source)
        .filter_map(|found| {
            let candidate = found.as_str();
            let has_alpha = candidate.chars().any(char::is_alphabetic);
            let has_digit = candidate
                .chars()
                .any(|character| character.is_ascii_digit());
            let has_separator = candidate
                .chars()
                .any(|character| matches!(character, '-' | '_' | '.' | '/' | ':' | '+' | ' '));
            if !has_digit || (!has_alpha && !has_separator) {
                return None;
            }
            normalize_raw(candidate).map(|identifier| (found.range(), identifier))
        })
        .collect()
}

pub(crate) fn technical_identifiers(source: &str) -> Vec<String> {
    let mut seen = BTreeSet::new();
    technical_identifier_spans(source)
        .into_iter()
        .map(|(_, identifier)| identifier)
        .filter(|identifier| seen.insert(identifier.clone()))
        .collect()
}

fn identifier_pattern() -> &'static Regex {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    PATTERN.get_or_init(|| {
        Regex::new(
            r"(?i)\b(?:cve(?:-\d{4}-\d+| \d{4} \d+)|rfc(?:[- ]?\d+)|[a-z0-9][a-z0-9._/:+\-]{2,})\b",
        )
        .expect("technical identifier regex is valid")
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn raw_normalization_is_lowercase_whitespace_bounded_without_lossy_prefixes() {
        assert_eq!(
            normalize_raw("  IIA   2 LINE ").as_deref(),
            Some("iia 2 line")
        );
        assert_eq!(normalize_raw(" \n\t "), None);
        assert_eq!(
            normalize_raw("Résumé NAÏVE e\u{301}"),
            Some("resume naive e".into())
        );
        assert_eq!(normalize_raw(&"A".repeat(300)).unwrap().len(), 300);
        assert_eq!(
            normalize_raw(&"A".repeat(MAX_NORMALIZED_EXACT_BYTES + 1)),
            None
        );
    }

    #[test]
    fn extracts_and_deduplicates_technical_identifiers() {
        let identifiers = technical_identifiers(
            "CVE-2026-1234 CVE 2026 1234 RFC9110 RFC 9110 product/v2.4.1 CVE-2026-1234 ordinary words incident_identifier_that_is_deliberately_longer_than_forty_characters_001",
        );

        assert_eq!(
            identifiers,
            [
                "cve-2026-1234",
                "cve 2026 1234",
                "rfc9110",
                "rfc 9110",
                "product/v2.4.1",
                "incident_identifier_that_is_deliberately_longer_than_forty_characters_001",
            ]
        );
    }
}
