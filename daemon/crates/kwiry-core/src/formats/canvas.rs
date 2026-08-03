// SPDX-License-Identifier: MIT OR Apache-2.0

use std::collections::{BTreeMap, BTreeSet};

use crate::extract::{
    ExtractedSection, ExtractedSource, ExtractionBudget, ExtractionCompleteness,
    ExtractionCoverage, ExtractionError, ExtractionNotice,
};
use crate::model::{Frontmatter, PropertyBag, PropertyValue};

use super::decode_utf8;
use super::markdown::markdown_sections_with_budget;

const MAX_CANVAS_NOTICES: usize = 32;

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
    let root = match serde_json::from_str::<PropertyValue>(source) {
        Ok(PropertyValue::Map(root)) => root,
        Ok(_) => {
            return Ok(ExtractedSource::skipped(
                ExtractionCoverage::Quarantined,
                ExtractionNotice::new(
                    "canvas_root_not_object",
                    "Canvas root must be a JSON object; quarantined as an invalid source preparation",
                ),
            ));
        }
        Err(_) => {
            return Ok(ExtractedSource::skipped(
                ExtractionCoverage::Quarantined,
                ExtractionNotice::new(
                    "invalid_canvas_json",
                    "invalid Canvas JSON; quarantined as an invalid source preparation",
                ),
            ));
        }
    };
    let nodes = match root.get("nodes") {
        None => &[],
        Some(PropertyValue::Sequence(nodes)) => nodes.as_slice(),
        Some(_) => {
            return Ok(ExtractedSource::skipped(
                ExtractionCoverage::Quarantined,
                ExtractionNotice::new(
                    "canvas_nodes_not_array",
                    "Canvas nodes must be a JSON array; quarantined as an invalid source preparation",
                ),
            ));
        }
    };
    let edges = match root.get("edges") {
        None => &[],
        Some(PropertyValue::Sequence(edges)) => edges.as_slice(),
        Some(_) => {
            return Ok(ExtractedSource::skipped(
                ExtractionCoverage::Quarantined,
                ExtractionNotice::new(
                    "canvas_edges_not_array",
                    "Canvas edges must be a JSON array; quarantined as an invalid source preparation",
                ),
            ));
        }
    };

    let mut sections = Vec::new();
    let mut budget = ExtractionBudget::default();
    let mut notices = NoticeCollector::default();
    let mut authored_ids = BTreeSet::new();
    let mut node_ids = BTreeSet::new();

    for (index, node) in nodes.iter().enumerate() {
        extract_node(
            index,
            node,
            &mut authored_ids,
            &mut node_ids,
            &mut sections,
            &mut budget,
            &mut notices,
        )?;
    }
    for (index, edge) in edges.iter().enumerate() {
        extract_edge(
            index,
            edge,
            &mut authored_ids,
            &node_ids,
            &mut sections,
            &mut budget,
            &mut notices,
        )?;
    }

    if sections.is_empty() {
        if notices.has_defects() {
            return Ok(skipped_with_notices(
                ExtractionCoverage::Quarantined,
                notices.into_notices(),
            ));
        }
        return Ok(ExtractedSource::skipped(
            ExtractionCoverage::SkippedNoExtractableText,
            ExtractionNotice::new(
                "canvas_no_extractable_text",
                "Canvas contains no authored text, labels, URLs, or file references; skipped with no extractable text",
            ),
        ));
    }

    let mut properties = BTreeMap::new();
    properties.insert("canvas".to_owned(), PropertyValue::Map(root));
    let completeness = if notices.has_defects() {
        ExtractionCompleteness::Partial
    } else {
        ExtractionCompleteness::Complete
    };
    Ok(ExtractedSource::indexed(
        PropertyBag::from_properties(properties),
        Frontmatter::default(),
        Vec::new(),
        Vec::new(),
        sections,
        completeness,
        notices.into_notices(),
    ))
}

#[allow(clippy::too_many_arguments)]
fn extract_node(
    index: usize,
    value: &PropertyValue,
    authored_ids: &mut BTreeSet<String>,
    node_ids: &mut BTreeSet<String>,
    sections: &mut Vec<ExtractedSection>,
    budget: &mut ExtractionBudget,
    notices: &mut NoticeCollector,
) -> Result<(), ExtractionError> {
    let PropertyValue::Map(node) = value else {
        notices.push(
            "canvas_node_not_object",
            format!("Canvas node at position {index} is not an object and was not extracted"),
        );
        return Ok(());
    };
    let Some(id) = non_empty_string(node.get("id")) else {
        notices.push(
            "canvas_node_missing_id",
            format!(
                "Canvas node at position {index} has no non-empty string ID and was not extracted"
            ),
        );
        return Ok(());
    };
    if !authored_ids.insert(id.to_owned()) {
        notices.push(
            "canvas_duplicate_id",
            format!("Canvas node at position {index} has a duplicate ID and was not extracted"),
        );
        return Ok(());
    }
    let Some(kind) = non_empty_string(node.get("type")) else {
        notices.push(
            "canvas_node_missing_type",
            format!(
                "Canvas node at position {index} has no supported string type and was not extracted"
            ),
        );
        return Ok(());
    };
    if ["x", "y", "width", "height"]
        .iter()
        .any(|field| !is_integer(node.get(*field)))
    {
        notices.push(
            "canvas_node_invalid_geometry",
            format!("Canvas node at position {index} has invalid geometry and was not extracted"),
        );
        return Ok(());
    }

    match kind {
        "text" => {
            let Some(text) = string(node.get("text")) else {
                notices.push(
                    "canvas_text_node_missing_text",
                    format!("Canvas text node at position {index} has no string text and was not extracted"),
                );
                return Ok(());
            };
            if !text.trim().is_empty() {
                sections.extend(markdown_sections_with_budget(text, budget)?);
            }
        }
        "file" => {
            let Some(file) = non_empty_string(node.get("file")) else {
                notices.push(
                    "canvas_file_node_missing_file",
                    format!("Canvas file node at position {index} has no non-empty string path and was not extracted"),
                );
                return Ok(());
            };
            let subpath = match node.get("subpath") {
                None => None,
                Some(PropertyValue::String(subpath)) => Some(subpath.as_str()),
                Some(_) => {
                    notices.push(
                        "canvas_file_node_invalid_subpath",
                        format!("Canvas file node at position {index} has a non-string subpath and was not extracted"),
                    );
                    return Ok(());
                }
            };
            let mut content = file.to_owned();
            if let Some(subpath) = subpath.filter(|subpath| !subpath.trim().is_empty()) {
                content.push('\n');
                content.push_str(subpath);
            }
            push_section(sections, budget, content)?;
        }
        "link" => {
            let Some(url) = non_empty_string(node.get("url")) else {
                notices.push(
                    "canvas_link_node_missing_url",
                    format!("Canvas link node at position {index} has no non-empty string URL and was not extracted"),
                );
                return Ok(());
            };
            push_section(sections, budget, url.to_owned())?;
        }
        "group" => match node.get("label") {
            None => {}
            Some(PropertyValue::String(label)) if label.trim().is_empty() => {}
            Some(PropertyValue::String(label)) => {
                push_section(sections, budget, label.clone())?;
            }
            Some(_) => {
                notices.push(
                    "canvas_group_node_invalid_label",
                    format!("Canvas group node at position {index} has a non-string label and was not extracted"),
                );
                return Ok(());
            }
        },
        _ => {
            notices.push(
                "canvas_node_unsupported_type",
                format!(
                    "Canvas node at position {index} has an unsupported type and was not extracted"
                ),
            );
            return Ok(());
        }
    }

    node_ids.insert(id.to_owned());
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn extract_edge(
    index: usize,
    value: &PropertyValue,
    authored_ids: &mut BTreeSet<String>,
    node_ids: &BTreeSet<String>,
    sections: &mut Vec<ExtractedSection>,
    budget: &mut ExtractionBudget,
    notices: &mut NoticeCollector,
) -> Result<(), ExtractionError> {
    let PropertyValue::Map(edge) = value else {
        notices.push(
            "canvas_edge_not_object",
            format!("Canvas edge at position {index} is not an object and was not extracted"),
        );
        return Ok(());
    };
    let Some(id) = non_empty_string(edge.get("id")) else {
        notices.push(
            "canvas_edge_missing_id",
            format!(
                "Canvas edge at position {index} has no non-empty string ID and was not extracted"
            ),
        );
        return Ok(());
    };
    if !authored_ids.insert(id.to_owned()) {
        notices.push(
            "canvas_duplicate_id",
            format!("Canvas edge at position {index} has a duplicate ID and was not extracted"),
        );
        return Ok(());
    }
    let Some(from_node) = non_empty_string(edge.get("fromNode")) else {
        notices.push(
            "canvas_edge_missing_endpoint",
            format!("Canvas edge at position {index} has no valid fromNode and was not extracted"),
        );
        return Ok(());
    };
    let Some(to_node) = non_empty_string(edge.get("toNode")) else {
        notices.push(
            "canvas_edge_missing_endpoint",
            format!("Canvas edge at position {index} has no valid toNode and was not extracted"),
        );
        return Ok(());
    };
    if !node_ids.contains(from_node) || !node_ids.contains(to_node) {
        notices.push(
            "canvas_edge_unknown_endpoint",
            format!("Canvas edge at position {index} references an unavailable node and was not extracted"),
        );
        return Ok(());
    }

    match edge.get("label") {
        None => {}
        Some(PropertyValue::String(label)) if label.trim().is_empty() => {}
        Some(PropertyValue::String(label)) => {
            push_section(sections, budget, label.clone())?;
        }
        Some(_) => {
            notices.push(
                "canvas_edge_invalid_label",
                format!(
                    "Canvas edge at position {index} has a non-string label and was not extracted"
                ),
            );
        }
    }
    Ok(())
}

fn push_section(
    sections: &mut Vec<ExtractedSection>,
    budget: &mut ExtractionBudget,
    content: String,
) -> Result<(), ExtractionError> {
    budget.reserve_section(&[])?;
    sections.push(ExtractedSection {
        heading_path: Vec::new(),
        content,
        locator: None,
    });
    Ok(())
}

fn string(value: Option<&PropertyValue>) -> Option<&str> {
    match value {
        Some(PropertyValue::String(value)) => Some(value),
        _ => None,
    }
}

fn non_empty_string(value: Option<&PropertyValue>) -> Option<&str> {
    string(value).filter(|value| !value.trim().is_empty())
}

fn is_integer(value: Option<&PropertyValue>) -> bool {
    matches!(value, Some(PropertyValue::I64(_) | PropertyValue::U64(_)))
}

fn skipped_with_notices(
    coverage: ExtractionCoverage,
    notices: Vec<ExtractionNotice>,
) -> ExtractedSource {
    debug_assert!(!coverage.is_indexed());
    debug_assert!(!notices.is_empty());
    ExtractedSource {
        properties: PropertyBag::default(),
        frontmatter: Frontmatter::default(),
        aliases: Vec::new(),
        links_out: Vec::new(),
        sections: Vec::new(),
        coverage,
        notices,
    }
}

#[derive(Debug, Default)]
struct NoticeCollector {
    notices: Vec<ExtractionNotice>,
    defects: usize,
    truncated: bool,
}

impl NoticeCollector {
    fn push(&mut self, code: impl Into<String>, message: impl Into<String>) {
        self.defects += 1;
        if self.notices.len() < MAX_CANVAS_NOTICES {
            self.notices.push(ExtractionNotice::new(code, message));
        } else if !self.truncated {
            self.notices.pop();
            self.notices.push(ExtractionNotice::new(
                "canvas_notices_truncated",
                "additional malformed or unsupported Canvas entries were not reported",
            ));
            self.truncated = true;
        }
    }

    fn has_defects(&self) -> bool {
        self.defects != 0
    }

    fn into_notices(self) -> Vec<ExtractionNotice> {
        self.notices
    }
}
