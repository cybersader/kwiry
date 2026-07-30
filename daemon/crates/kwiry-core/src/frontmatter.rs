// SPDX-License-Identifier: MIT OR Apache-2.0

use std::collections::HashSet;

use crate::model::{
    Frontmatter, MAX_FILE_BYTES, MAX_PROPERTY_NESTING_DEPTH, PropertyBag, PropertyValue,
};

// This mirrors the whole-file ingest boundary: it protects standalone parser callers from
// corrupt input without inventing a smaller cap that could repeat the links_out rejection.
const MAX_FRONTMATTER_BYTES: usize = MAX_FILE_BYTES as usize;

// This caps expanded deserializer work, not authored aliases, properties, keys, or array entries.
// A repeated alias to a small value remains valid; only the total replayed event stream is bounded.
// The boundary regression measured 48,800 KiB peak RSS and 0.51 s wall time versus 13,172 KiB for
// the 99/100-reference control in an x86_64 native debug build (2026-07-29).
const MAX_ALIAS_REPLAYED_EVENTS: usize = 262_144;

pub(crate) fn parse_frontmatter(
    source: &str,
) -> (PropertyBag, Frontmatter, Vec<String>, &str, Option<String>) {
    let Some((yaml, body)) = split_frontmatter(source) else {
        return (
            PropertyBag::default(),
            Frontmatter::default(),
            Vec::new(),
            source,
            None,
        );
    };

    if yaml.len() > MAX_FRONTMATTER_BYTES {
        return (
            PropertyBag::default(),
            Frontmatter::default(),
            Vec::new(),
            body,
            Some(format!(
                "frontmatter exceeds {} bytes and was ignored",
                MAX_FRONTMATTER_BYTES
            )),
        );
    }

    let options = serde_saphyr::options! {
        // The accepted input is already byte-bounded. Disabling the dependency's pre-parse budget
        // removes its hidden alias/anchor ratio and cardinality limits; replay work remains bounded
        // explicitly below, and recursive construction remains bounded by PropertyValue's visitor.
        budget: None,
        duplicate_keys: serde_saphyr::DuplicateKeyPolicy::Error,
        merge_keys: serde_saphyr::MergeKeyPolicy::Merge,
        alias_limits: serde_saphyr::alias_limits! {
            max_total_replayed_events: MAX_ALIAS_REPLAYED_EVENTS,
            max_replay_stack_depth: MAX_PROPERTY_NESTING_DEPTH,
            max_alias_expansions_per_anchor: usize::MAX,
        },
        with_snippet: false,
    };
    match serde_saphyr::from_str_with_options::<PropertyValue>(yaml, options) {
        Ok(PropertyValue::Map(properties)) => {
            let properties = PropertyBag::from_properties(properties);
            let frontmatter = Frontmatter::from_properties(&properties);
            let aliases = select_aliases(&properties);
            (properties, frontmatter, aliases, body, None)
        }
        Ok(_) => (
            PropertyBag::default(),
            Frontmatter::default(),
            Vec::new(),
            body,
            None,
        ),
        // serde-saphyr errors can quote source text. Diagnostics disclose only the fixed class.
        Err(_) => (
            PropertyBag::default(),
            Frontmatter::default(),
            Vec::new(),
            body,
            Some("invalid YAML frontmatter".to_owned()),
        ),
    }
}

fn split_frontmatter(source: &str) -> Option<(&str, &str)> {
    if !(source.starts_with("---\n") || source.starts_with("---\r\n")) {
        return None;
    }

    let opening_end = source.find('\n')? + 1;
    let mut cursor = opening_end;
    for line in source[opening_end..].split_inclusive('\n') {
        let trimmed = line.trim_end_matches(['\r', '\n']);
        if trimmed == "---" {
            let yaml = &source[opening_end..cursor];
            let body = &source[cursor + line.len()..];
            return Some((yaml, body));
        }
        cursor += line.len();
    }
    None
}

fn select_aliases(properties: &PropertyBag) -> Vec<String> {
    let Some(value) = properties.get("aliases") else {
        return Vec::new();
    };

    let mut aliases = Vec::new();
    let mut seen = HashSet::new();
    for alias in property_strings(value) {
        let alias = alias.trim();
        if alias.is_empty() {
            continue;
        }
        let alias = alias.to_owned();
        if seen.insert(alias.clone()) {
            aliases.push(alias);
        }
    }
    aliases
}

fn property_strings(value: &PropertyValue) -> Vec<String> {
    match value {
        PropertyValue::Sequence(values) => values.iter().filter_map(property_string).collect(),
        _ => property_string(value).into_iter().collect(),
    }
}

fn property_string(value: &PropertyValue) -> Option<String> {
    match value {
        PropertyValue::Bool(value) => Some(value.to_string()),
        PropertyValue::I64(value) => Some(value.to_string()),
        PropertyValue::U64(value) => Some(value.to_string()),
        PropertyValue::F64(value) => Some(value.to_string()),
        PropertyValue::String(value) => Some(value.clone()),
        PropertyValue::Null | PropertyValue::Sequence(_) | PropertyValue::Map(_) => None,
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;

    #[test]
    fn retains_open_properties_and_derives_legacy_fields() {
        let source = "---\ntitle: Note\ndescription: 42\ntags: [one, 2, true, null]\nstatus: active\ndate: 2026-07-28\ncustom: value\naliases: [Alias]\n---\n# Body\n";
        let (properties, frontmatter, aliases, body, warning) = parse_frontmatter(source);

        assert_eq!(frontmatter.title(), Some("Note"));
        assert_eq!(frontmatter.description(), Some("42"));
        assert_eq!(frontmatter.tags(), ["one", "2", "true"]);
        assert_eq!(frontmatter.status(), Some("active"));
        assert_eq!(frontmatter.date(), Some("2026-07-28"));
        assert_eq!(
            properties.get("custom"),
            Some(&PropertyValue::String("value".to_owned()))
        );
        assert_eq!(
            properties.get("aliases"),
            Some(&PropertyValue::Sequence(vec![PropertyValue::String(
                "Alias".to_owned()
            )]))
        );
        assert_eq!(aliases, ["Alias"]);
        assert_eq!(body, "# Body\n");
        assert!(warning.is_none());
    }

    #[test]
    fn preserves_each_occurrence_type_without_a_vault_wide_schema() {
        let numeric = parse_frontmatter("---\npriority: 7\n---\n").0;
        let textual = parse_frontmatter("---\npriority: '7'\n---\n").0;

        assert_eq!(numeric.get("priority"), Some(&PropertyValue::I64(7)));
        assert_eq!(
            textual.get("priority"),
            Some(&PropertyValue::String("7".to_owned()))
        );
    }

    #[test]
    fn preserves_all_scalar_and_recursive_yaml_types() {
        let source = "---\nnull_value: null\nbool_value: true\ni64_value: -7\nu64_value: 18446744073709551615\nf64_value: 1.25e2\nstring_value: '125'\nsequence: [1, two, false]\nmap:\n  zed: null\n  alpha: 3.5\n---\n";
        let (properties, _, _, _, warning) = parse_frontmatter(source);

        assert_eq!(properties.get("null_value"), Some(&PropertyValue::Null));
        assert_eq!(
            properties.get("bool_value"),
            Some(&PropertyValue::Bool(true))
        );
        assert_eq!(properties.get("i64_value"), Some(&PropertyValue::I64(-7)));
        assert_eq!(
            properties.get("u64_value"),
            Some(&PropertyValue::U64(u64::MAX))
        );
        assert_eq!(
            properties.get("f64_value"),
            Some(&PropertyValue::F64(125.0))
        );
        assert_eq!(
            properties.get("string_value"),
            Some(&PropertyValue::String("125".to_owned()))
        );
        assert_eq!(
            properties.get("sequence"),
            Some(&PropertyValue::Sequence(vec![
                PropertyValue::I64(1),
                PropertyValue::String("two".to_owned()),
                PropertyValue::Bool(false),
            ]))
        );
        assert_eq!(
            properties.get("map"),
            Some(&PropertyValue::Map(BTreeMap::from([
                ("alpha".to_owned(), PropertyValue::F64(3.5)),
                ("zed".to_owned(), PropertyValue::Null),
            ])))
        );
        assert!(warning.is_none());
    }

    #[test]
    fn retains_yaml_date_as_the_original_string() {
        let (properties, frontmatter, _, _, warning) =
            parse_frontmatter("---\ndate: 2026-07-28\n---\n");

        assert_eq!(
            properties.get("date"),
            Some(&PropertyValue::String("2026-07-28".to_owned()))
        );
        assert_eq!(frontmatter.date.as_deref(), Some("2026-07-28"));
        assert!(warning.is_none());
    }

    #[test]
    fn compact_frontmatter_serializes_only_legacy_projection() {
        let (_, frontmatter, _, _, warning) = parse_frontmatter(
            "---\ntitle: Note\ntags: [one, 2]\ndate: 2026-07-28\nnested: {enabled: true}\n---\n",
        );
        let json = serde_json::to_string(&frontmatter).unwrap();
        let restored: Frontmatter = serde_json::from_str(&json).unwrap();

        assert_eq!(restored, frontmatter);
        assert_eq!(
            json,
            r#"{"title":"Note","tags":["one","2"],"date":"2026-07-28"}"#
        );
        assert!(!json.contains("nested"));
        assert!(warning.is_none());
    }

    #[test]
    fn malformed_frontmatter_warns_but_keeps_body() {
        let source = "---\ntags: [broken\n---\nBody\n";
        let (properties, frontmatter, aliases, body, warning) = parse_frontmatter(source);

        assert!(properties.is_empty());
        assert_eq!(frontmatter, Frontmatter::default());
        assert!(aliases.is_empty());
        assert_eq!(body, "Body\n");
        assert!(warning.unwrap().contains("invalid YAML"));
    }

    #[test]
    fn selects_trimmed_unique_aliases_and_retains_the_property() {
        let source =
            "---\ntitle: Note\naliases: [IIA 2 line, ' IIA 2 line ', Second Line, '']\n---\nBody\n";
        let (properties, frontmatter, aliases, body, warning) = parse_frontmatter(source);

        assert_eq!(frontmatter.title.as_deref(), Some("Note"));
        assert!(properties.get("aliases").is_some());
        assert_eq!(aliases, ["IIA 2 line", "Second Line"]);
        assert_eq!(body, "Body\n");
        assert!(warning.is_none());
    }

    #[test]
    fn accepts_scalar_alias() {
        let source = "---\naliases: IIA 2 line\n---\nBody\n";
        let (properties, _, aliases, _, warning) = parse_frontmatter(source);

        assert_eq!(
            properties.get("aliases"),
            Some(&PropertyValue::String("IIA 2 line".to_owned()))
        );
        assert_eq!(aliases, ["IIA 2 line"]);
        assert!(warning.is_none());
    }

    #[test]
    fn accepts_repeated_aliases_across_the_dependency_default_boundary() {
        for count in [99, 100] {
            let aliases = std::iter::repeat_n("*shared", count)
                .collect::<Vec<_>>()
                .join(", ");
            let source = format!("---\nshared: &shared {{value: ok}}\nrefs: [{aliases}]\n---\n");
            let (properties, _, _, _, warning) = parse_frontmatter(&source);

            assert!(warning.is_none(), "{count} aliases must parse");
            let Some(PropertyValue::Sequence(values)) = properties.get("refs") else {
                panic!("refs must remain a sequence");
            };
            assert_eq!(values.len(), count);
        }
    }

    #[test]
    fn rejects_alias_expansion_that_exceeds_the_owned_replay_budget() {
        let mut yaml =
            String::from("---\na0: &a0 [leaf, leaf, leaf, leaf, leaf, leaf, leaf, leaf]\n");
        for depth in 1..=6 {
            let previous = depth - 1;
            yaml.push_str(&format!(
                "a{depth}: &a{depth} [*a{previous}, *a{previous}, *a{previous}, *a{previous}, *a{previous}, *a{previous}, *a{previous}, *a{previous}]\n"
            ));
        }
        yaml.push_str("value: *a6\n---\nBody\n");

        let (properties, _, _, body, warning) = parse_frontmatter(&yaml);

        assert!(properties.is_empty());
        assert_eq!(body, "Body\n");
        assert_eq!(warning.as_deref(), Some("invalid YAML frontmatter"));
    }

    #[test]
    fn rejects_alias_replay_deeper_than_the_owned_stack_boundary() {
        let mut yaml = String::from("---\na0: &a0 [leaf]\n");
        for depth in 1..=MAX_PROPERTY_NESTING_DEPTH {
            let previous = depth - 1;
            yaml.push_str(&format!("a{depth}: &a{depth} [*a{previous}]\n"));
        }
        yaml.push_str(&format!(
            "value: *a{}\n---\nBody\n",
            MAX_PROPERTY_NESTING_DEPTH
        ));

        let (properties, _, _, body, warning) = parse_frontmatter(&yaml);

        assert!(properties.is_empty());
        assert_eq!(body, "Body\n");
        assert_eq!(warning.as_deref(), Some("invalid YAML frontmatter"));
    }

    #[test]
    fn rejects_only_pathological_recursive_nesting() {
        let mut source = String::from("---\nvalue: ");
        for _ in 0..=MAX_PROPERTY_NESTING_DEPTH {
            source.push('[');
        }
        source.push_str("null");
        for _ in 0..=MAX_PROPERTY_NESTING_DEPTH {
            source.push(']');
        }
        source.push_str("\n---\nBody\n");

        let (properties, frontmatter, aliases, body, warning) = parse_frontmatter(&source);

        assert!(properties.is_empty());
        assert_eq!(frontmatter, Frontmatter::default());
        assert!(aliases.is_empty());
        assert_eq!(body, "Body\n");
        assert_eq!(warning.as_deref(), Some("invalid YAML frontmatter"));
        assert!(!warning.unwrap().contains("null"));
    }
}
