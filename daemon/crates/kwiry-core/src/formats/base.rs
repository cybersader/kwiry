// SPDX-License-Identifier: MIT OR Apache-2.0

use std::collections::{BTreeMap, HashSet};
use std::fmt::Write as _;

use crate::extract::{
    ExtractedSection, ExtractedSource, ExtractionCompleteness, ExtractionCoverage, ExtractionError,
    ExtractionNotice, SourceLocator,
};
use crate::frontmatter::parse_yaml_value;
use crate::links::extract_wikilinks;
use crate::model::{Frontmatter, PropertyBag, PropertyValue};

use super::decode_utf8;

pub(super) fn extract(bytes: &[u8]) -> Result<ExtractedSource, ExtractionError> {
    let source = match decode_utf8(bytes) {
        Ok(source) => source,
        Err(notice) => {
            return Ok(ExtractedSource::skipped(
                ExtractionCoverage::Unreadable,
                notice,
            ));
        }
    };
    let root = match parse_yaml_value(source) {
        Ok(PropertyValue::Map(root)) => root,
        Ok(_) => {
            return Ok(ExtractedSource::skipped(
                ExtractionCoverage::SkippedNoExtractableText,
                ExtractionNotice::new(
                    "base_root_not_mapping",
                    "base file root must be a YAML mapping; skipped with no extractable text",
                ),
            ));
        }
        Err(()) => {
            return Ok(ExtractedSource::skipped(
                ExtractionCoverage::Quarantined,
                ExtractionNotice::new(
                    "invalid_base_yaml",
                    "invalid Base YAML; quarantined as an invalid source preparation",
                ),
            ));
        }
    };
    if root.is_empty() {
        return Ok(ExtractedSource::skipped(
            ExtractionCoverage::SkippedNoExtractableText,
            ExtractionNotice::new(
                "empty_base_configuration",
                "base file contains no authored configuration; skipped with no extractable text",
            ),
        ));
    }

    let properties = source_properties(&root);
    let frontmatter = Frontmatter::from_properties(&properties);
    let mut top_level = root.clone();
    let views = top_level.remove("views");
    let mut sections = vec![ExtractedSection {
        heading_path: Vec::new(),
        content: render_map(&top_level),
        locator: None,
    }];
    let mut notices = Vec::new();
    let mut used_view_headings = HashSet::new();

    match views {
        None => {}
        Some(PropertyValue::Sequence(views)) => {
            for (index, view) in views.into_iter().enumerate() {
                let PropertyValue::Map(view) = view else {
                    notices.push(ExtractionNotice::new(
                        "base_view_not_mapping",
                        format!(
                            "base view at position {index} is not a mapping and was not extracted"
                        ),
                    ));
                    continue;
                };
                let Some(PropertyValue::String(name)) = view.get("name") else {
                    notices.push(ExtractionNotice::new(
                        "base_view_missing_name",
                        format!("base view at position {index} has no authored string name and was not extracted"),
                    ));
                    continue;
                };
                let name = name.trim();
                if name.is_empty() {
                    notices.push(ExtractionNotice::new(
                        "base_view_missing_name",
                        format!(
                            "base view at position {index} has an empty name and was not extracted"
                        ),
                    ));
                    continue;
                }
                let heading = unique_view_heading(name, &mut used_view_headings);
                sections.push(ExtractedSection {
                    heading_path: vec![heading],
                    content: render_map(&view),
                    locator: Some(SourceLocator::BaseView {
                        view: name.to_owned(),
                    }),
                });
            }
        }
        Some(_) => notices.push(ExtractionNotice::new(
            "base_views_not_sequence",
            "base views configuration is not a sequence and was not extracted into view sections",
        )),
    }

    let completeness = if notices.is_empty() {
        ExtractionCompleteness::Complete
    } else {
        ExtractionCompleteness::Partial
    };
    Ok(ExtractedSource::indexed(
        properties,
        frontmatter,
        Vec::new(),
        extract_wikilinks(source),
        sections,
        completeness,
        notices,
    ))
}

fn source_properties(root: &BTreeMap<String, PropertyValue>) -> PropertyBag {
    let mut properties = BTreeMap::new();
    properties.insert("base".to_owned(), PropertyValue::Map(root.clone()));
    for shared_name in ["title", "tags"] {
        if let Some(value) = root.get(shared_name) {
            properties.insert(shared_name.to_owned(), value.clone());
        }
    }
    PropertyBag::from_properties(properties)
}

fn unique_view_heading(name: &str, used: &mut HashSet<String>) -> String {
    if used.insert(name.to_owned()) {
        return name.to_owned();
    }

    for occurrence in 2_u64.. {
        let candidate = format!("{name} ({occurrence})");
        if used.insert(candidate.clone()) {
            return candidate;
        }
    }
    unreachable!("an unbounded numeric suffix always yields a unique view heading")
}

fn render_map(map: &BTreeMap<String, PropertyValue>) -> String {
    let mut output = String::new();
    for (name, value) in map {
        render_value(&mut output, name, value);
    }
    output
}

fn render_value(output: &mut String, path: &str, value: &PropertyValue) {
    match value {
        PropertyValue::Null => {
            writeln!(output, "{path}: null").expect("writing to a String cannot fail");
        }
        PropertyValue::Bool(value) => {
            writeln!(output, "{path}: {value}").expect("writing to a String cannot fail");
        }
        PropertyValue::I64(value) => {
            writeln!(output, "{path}: {value}").expect("writing to a String cannot fail");
        }
        PropertyValue::U64(value) => {
            writeln!(output, "{path}: {value}").expect("writing to a String cannot fail");
        }
        PropertyValue::F64(value) => {
            writeln!(output, "{path}: {value}").expect("writing to a String cannot fail");
        }
        PropertyValue::String(value) => {
            writeln!(output, "{path}: {value}").expect("writing to a String cannot fail");
        }
        PropertyValue::Sequence(values) => {
            if values.is_empty() {
                writeln!(output, "{path}: []").expect("writing to a String cannot fail");
            }
            for (index, value) in values.iter().enumerate() {
                render_value(output, &format!("{path}[{index}]"), value);
            }
        }
        PropertyValue::Map(values) => {
            if values.is_empty() {
                writeln!(output, "{path}: {{}}").expect("writing to a String cannot fail");
            }
            for (name, value) in values {
                render_value(output, &format!("{path}.{name}"), value);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_authored_configuration_and_named_views_in_yaml_order() {
        let source = br#"title: Project dashboard
tags: [projects, active]
filters:
  and:
    - file.inFolder("Projects")
views:
  - type: table
    name: Active
    order: [file.name, status]
  - type: cards
    name: Gallery
    order: [cover, file.name]
"#;

        let extracted = extract(source).unwrap();

        assert_eq!(extracted.coverage, ExtractionCoverage::IndexedComplete);
        assert_eq!(extracted.sections.len(), 3);
        assert!(extracted.sections[0].content.contains("filters.and[0]"));
        assert!(!extracted.sections[0].content.contains("views"));
        assert_eq!(extracted.sections[1].heading_path, ["Active"]);
        assert_eq!(
            extracted.sections[1].locator,
            Some(SourceLocator::BaseView {
                view: "Active".to_owned()
            })
        );
        assert_eq!(extracted.sections[2].heading_path, ["Gallery"]);
        assert_eq!(
            extracted.frontmatter.title.as_deref(),
            Some("Project dashboard")
        );
        assert_eq!(extracted.frontmatter.tags, ["projects", "active"]);
        let Some(PropertyValue::Map(base)) = extracted.properties.get("base") else {
            panic!("complete Base YAML must remain under the base property root");
        };
        assert!(base.contains_key("views"));
    }

    #[test]
    fn never_extracts_materialized_rows_and_reports_malformed_views_as_partial() {
        let source = br#"filters: status == "active"
views:
  - type: table
  - not-a-view
"#;

        let extracted = extract(source).unwrap();

        assert_eq!(extracted.coverage, ExtractionCoverage::IndexedPartial);
        assert_eq!(extracted.sections.len(), 1);
        assert_eq!(extracted.notices.len(), 2);
        assert!(!extracted.sections[0].content.contains("materialized"));
    }

    #[test]
    fn invalid_base_yaml_is_quarantined() {
        let extracted = extract(b"not: [valid").unwrap();

        assert_eq!(extracted.coverage, ExtractionCoverage::Quarantined);
        assert_eq!(extracted.notices[0].code, "invalid_base_yaml");
    }

    #[test]
    fn empty_or_non_mapping_base_is_an_explicit_no_text_skip() {
        for source in [b"[]".as_slice(), b"{}".as_slice()] {
            let extracted = extract(source).unwrap();
            assert_eq!(
                extracted.coverage,
                ExtractionCoverage::SkippedNoExtractableText
            );
            assert_eq!(extracted.notices.len(), 1);
        }
    }

    #[test]
    fn duplicate_view_names_get_unique_headings_without_changing_locators() {
        let source = br#"views:
  - name: Active
    type: table
  - name: Active
    type: cards
"#;

        let extracted = extract(source).unwrap();

        assert_eq!(extracted.sections[1].heading_path, ["Active"]);
        assert_eq!(extracted.sections[2].heading_path, ["Active (2)"]);
        assert_eq!(
            extracted.sections[2].locator,
            Some(SourceLocator::BaseView {
                view: "Active".to_owned()
            })
        );
    }
}
