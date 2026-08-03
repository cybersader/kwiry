// SPDX-License-Identifier: MIT OR Apache-2.0

use pulldown_cmark::{Event, Options, Parser, Tag, TagEnd};

use crate::extract::{
    ExtractedSection, ExtractedSource, ExtractionBudget, ExtractionCompleteness, ExtractionError,
    ExtractionNotice,
};
use crate::frontmatter::parse_frontmatter;
use crate::links::extract_wikilinks;

use super::decode_utf8;

#[derive(Debug)]
struct HeadingMarker {
    start: usize,
    path: Vec<String>,
}

pub(super) fn extract(bytes: &[u8]) -> Result<ExtractedSource, ExtractionError> {
    let source = match decode_utf8(bytes) {
        Ok(source) => source,
        Err(notice) => {
            return Ok(ExtractedSource::skipped(
                crate::extract::ExtractionCoverage::Unreadable,
                notice,
            ));
        }
    };
    let (properties, frontmatter, aliases, body, warning) = parse_frontmatter(source);
    let links_out = extract_wikilinks(body);
    let sections = markdown_sections(body)?;
    let (completeness, notices) = match warning {
        Some(message) => (
            ExtractionCompleteness::Partial,
            vec![ExtractionNotice::new("frontmatter_not_extracted", message)],
        ),
        None => (ExtractionCompleteness::Complete, Vec::new()),
    };

    Ok(ExtractedSource::indexed(
        properties,
        frontmatter,
        aliases,
        links_out,
        sections,
        completeness,
        notices,
    ))
}

fn markdown_sections(source: &str) -> Result<Vec<ExtractedSection>, ExtractionError> {
    markdown_sections_with_budget(source, &mut ExtractionBudget::default())
}

pub(super) fn markdown_sections_with_budget(
    source: &str,
    budget: &mut ExtractionBudget,
) -> Result<Vec<ExtractedSection>, ExtractionError> {
    let mut markers = Vec::new();
    let mut heading_stack: Vec<(usize, String)> = Vec::new();
    let mut active_heading: Option<(usize, usize, String)> = None;

    for (event, range) in Parser::new_ext(source, Options::all()).into_offset_iter() {
        match event {
            Event::Start(Tag::Heading { level, .. }) => {
                active_heading = Some((heading_level(level), range.start, String::new()));
            }
            Event::Text(text) | Event::Code(text) => {
                if let Some((_, _, heading)) = active_heading.as_mut() {
                    heading.push_str(&text);
                }
            }
            Event::SoftBreak | Event::HardBreak => {
                if let Some((_, _, heading)) = active_heading.as_mut() {
                    heading.push(' ');
                }
            }
            Event::End(TagEnd::Heading(_)) => {
                if let Some((level, start, heading)) = active_heading.take() {
                    while heading_stack
                        .last()
                        .is_some_and(|(parent_level, _)| *parent_level >= level)
                    {
                        heading_stack.pop();
                    }
                    heading_stack.push((level, heading.trim().to_owned()));
                    let path = heading_stack
                        .iter()
                        .map(|(_, heading)| heading.clone())
                        .collect::<Vec<_>>();
                    budget.reserve_section(&path)?;
                    markers.push(HeadingMarker { start, path });
                }
            }
            _ => {}
        }
    }

    if markers.is_empty() || markers[0].start > 0 {
        budget.reserve_section(&[])?;
    }
    Ok(sections_from_markers(source, markers))
}

fn sections_from_markers(source: &str, markers: Vec<HeadingMarker>) -> Vec<ExtractedSection> {
    if markers.is_empty() {
        return vec![ExtractedSection {
            heading_path: Vec::new(),
            content: source.to_owned(),
            locator: None,
        }];
    }

    let mut sections = Vec::new();
    if markers[0].start > 0 {
        sections.push(ExtractedSection {
            heading_path: Vec::new(),
            content: source[..markers[0].start].to_owned(),
            locator: None,
        });
    }

    let ends = markers
        .iter()
        .skip(1)
        .map(|marker| marker.start)
        .chain(std::iter::once(source.len()))
        .collect::<Vec<_>>();
    for (marker, end) in markers.into_iter().zip(ends) {
        sections.push(ExtractedSection {
            heading_path: marker.path,
            content: source[marker.start..end].to_owned(),
            locator: None,
        });
    }
    sections
}

fn heading_level(level: pulldown_cmark::HeadingLevel) -> usize {
    match level {
        pulldown_cmark::HeadingLevel::H1 => 1,
        pulldown_cmark::HeadingLevel::H2 => 2,
        pulldown_cmark::HeadingLevel::H3 => 3,
        pulldown_cmark::HeadingLevel::H4 => 4,
        pulldown_cmark::HeadingLevel::H5 => 5,
        pulldown_cmark::HeadingLevel::H6 => 6,
    }
}
