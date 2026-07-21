use std::collections::BTreeSet;
use std::sync::OnceLock;

use regex::Regex;

const MAX_RAW_CHARS: usize = 256;

pub(crate) fn normalize_raw(value: &str) -> Option<String> {
    let normalized = value
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase();
    if normalized.is_empty() {
        return None;
    }
    Some(normalized.chars().take(MAX_RAW_CHARS).collect())
}

pub(crate) fn technical_identifiers(source: &str) -> Vec<String> {
    let mut seen = BTreeSet::new();
    let mut identifiers = Vec::new();
    for candidate in identifier_pattern()
        .find_iter(source)
        .map(|found| found.as_str())
    {
        let has_alpha = candidate.chars().any(char::is_alphabetic);
        let has_digit = candidate
            .chars()
            .any(|character| character.is_ascii_digit());
        let has_separator = candidate
            .chars()
            .any(|character| matches!(character, '-' | '_' | '.' | '/' | ':' | '+'));
        if !has_digit || (!has_alpha && !has_separator) {
            continue;
        }
        let Some(identifier) = normalize_raw(candidate) else {
            continue;
        };
        if seen.insert(identifier.clone()) {
            identifiers.push(identifier);
        }
    }
    identifiers
}

fn identifier_pattern() -> &'static Regex {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    PATTERN.get_or_init(|| {
        Regex::new(r"(?i)\b(?:cve-\d{4}-\d+|rfc(?:[- ]?\d+)|[a-z0-9][a-z0-9._/:+\-]{2,})\b")
            .expect("technical identifier regex is valid")
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn raw_normalization_is_lowercase_whitespace_bounded() {
        assert_eq!(
            normalize_raw("  IIA   2 LINE ").as_deref(),
            Some("iia 2 line")
        );
        assert_eq!(normalize_raw(" \n\t "), None);
        assert_eq!(
            normalize_raw(&"A".repeat(300)).unwrap().chars().count(),
            256
        );
    }

    #[test]
    fn extracts_and_deduplicates_technical_identifiers() {
        let identifiers = technical_identifiers(
            "CVE-2026-1234 RFC9110 product/v2.4.1 CVE-2026-1234 ordinary words incident_identifier_that_is_deliberately_longer_than_forty_characters_001",
        );

        assert_eq!(
            identifiers,
            [
                "cve-2026-1234",
                "rfc9110",
                "product/v2.4.1",
                "incident_identifier_that_is_deliberately_longer_than_forty_characters_001",
            ]
        );
    }
}
