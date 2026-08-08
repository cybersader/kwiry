// SPDX-License-Identifier: MIT OR Apache-2.0

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::extract::{MAX_EXTRACTED_HEADING_BYTES_PER_SOURCE, MAX_EXTRACTED_SECTIONS_PER_SOURCE};

use super::error::DocxError;
use super::limits::{MAX_EXTRACTED_TEXT_BYTES, MAX_OUTLINE_LEVEL};
use super::xml::{XmlBudget, XmlElement, XmlEvent, parse_xml, parse_xml_events};

const WORDPROCESSING_NS_TRANSITIONAL: &[u8] =
    b"http://schemas.openxmlformats.org/wordprocessingml/2006/main";
const WORDPROCESSING_NS_STRICT: &[u8] = b"http://purl.oclc.org/ooxml/wordprocessingml/main";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum PartKind {
    Document,
    Header,
    Footer,
    Footnotes,
    Endnotes,
    Comments,
    Styles,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum ContentRole {
    Primary,
    Supporting,
    Latent,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ExtractionScope {
    CurrentView,
    AllContent,
}

impl ExtractionScope {
    pub(super) const fn includes(self, role: ContentRole) -> bool {
        match self {
            Self::CurrentView => !matches!(role, ContentRole::Latent),
            Self::AllContent => true,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SemanticSection {
    pub heading_path: Vec<String>,
    pub content: String,
    pub role: ContentRole,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum HeaderFooterKind {
    Header,
    Footer,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct HeaderFooterReference {
    pub(super) kind: HeaderFooterKind,
    pub(super) relationship_id: String,
}

#[derive(Debug, Default)]
pub(super) struct DocumentExtraction {
    pub(super) sections: Vec<SemanticSection>,
    pub(super) header_footer_references: Vec<HeaderFooterReference>,
    pub(super) unsupported_markup: bool,
}

#[derive(Debug, Default)]
pub(super) struct SupportingExtraction {
    pub(super) sections: Vec<SemanticSection>,
    pub(super) unsupported_markup: bool,
}

#[derive(Debug, Clone, Default)]
pub(super) struct Styles {
    headings: BTreeMap<String, usize>,
    /// Style ids whose run properties hide their text. Word renders these as
    /// invisible, so text carrying them is latent even though the run itself
    /// declares no `w:vanish`.
    hidden: BTreeSet<String>,
}

#[derive(Debug, Default)]
pub(super) struct OutputBudget {
    bytes: usize,
    sections: usize,
    heading_bytes: usize,
}

impl OutputBudget {
    /// Takes the live heading stack rather than a materialized path so the
    /// per-section clone happens only after the fragment is known to be
    /// non-empty and only after its cost has been charged. A paragraph that
    /// contributes no text to a role must not pay for the heading path at all,
    /// and heading bytes are charged because they are duplicated into every
    /// section and would otherwise amplify a small input without bound.
    fn push(
        &mut self,
        output: &mut Vec<SemanticSection>,
        heading_stack: &[(usize, String)],
        content: String,
        role: ContentRole,
    ) -> Result<(), DocxError> {
        let content = normalize_fragment(content);
        if content.is_empty() {
            return Ok(());
        }
        self.bytes = self
            .bytes
            .checked_add(content.len())
            .filter(|bytes| *bytes <= MAX_EXTRACTED_TEXT_BYTES)
            .ok_or(DocxError::XmlLimitExceeded)?;
        self.sections = self
            .sections
            .checked_add(1)
            .filter(|sections| *sections <= MAX_EXTRACTED_SECTIONS_PER_SOURCE)
            .ok_or(DocxError::XmlLimitExceeded)?;
        let heading_bytes = heading_stack
            .iter()
            .try_fold(0usize, |total, (_, heading)| {
                total.checked_add(heading.len())
            })
            .ok_or(DocxError::XmlLimitExceeded)?;
        self.heading_bytes = self
            .heading_bytes
            .checked_add(heading_bytes)
            .filter(|total| *total <= MAX_EXTRACTED_HEADING_BYTES_PER_SOURCE)
            .ok_or(DocxError::XmlLimitExceeded)?;
        output.push(SemanticSection {
            heading_path: heading_stack
                .iter()
                .map(|(_, heading)| heading.clone())
                .collect(),
            content,
            role,
        });
        Ok(())
    }
}

pub(super) fn validate_part(
    bytes: &[u8],
    kind: PartKind,
    budget: &mut XmlBudget,
) -> Result<(), DocxError> {
    let expected_root = match kind {
        PartKind::Document => b"document".as_slice(),
        PartKind::Header => b"hdr".as_slice(),
        PartKind::Footer => b"ftr".as_slice(),
        PartKind::Footnotes => b"footnotes".as_slice(),
        PartKind::Endnotes => b"endnotes".as_slice(),
        PartKind::Comments => b"comments".as_slice(),
        PartKind::Styles => b"styles".as_slice(),
    };
    let mut root_checked = false;
    parse_xml(bytes, budget, |element| {
        if root_checked {
            return Ok(());
        }
        root_checked = true;
        if element.local_name != expected_root || !is_wordprocessing(element.namespace.as_deref()) {
            return Err(DocxError::RequiredPartInvalid);
        }
        Ok(())
    })?;
    root_checked
        .then_some(())
        .ok_or(DocxError::RequiredPartInvalid)
}

pub(super) fn parse_styles(bytes: &[u8], budget: &mut XmlBudget) -> Result<Styles, DocxError> {
    #[derive(Default)]
    struct StyleBuilder {
        id: String,
        paragraph: bool,
        name: Option<String>,
        outline: Option<usize>,
        hidden: bool,
    }

    let mut styles = Styles::default();
    let mut current: Option<StyleBuilder> = None;
    parse_xml_events(bytes, budget, |event| {
        match event {
            XmlEvent::Start(element) if word_element(element, b"style") => {
                let id = attribute_local(element, b"styleId")
                    .filter(|id| !id.is_empty())
                    .ok_or(DocxError::RequiredPartInvalid)?;
                current = Some(StyleBuilder {
                    id: id.to_owned(),
                    paragraph: attribute_local(element, b"type") == Some("paragraph"),
                    ..StyleBuilder::default()
                });
            }
            XmlEvent::Empty(element) | XmlEvent::Start(element)
                if current.is_some() && word_element(element, b"name") =>
            {
                current.as_mut().expect("style exists").name =
                    attribute_local(element, b"val").map(str::to_owned);
            }
            XmlEvent::Empty(element) | XmlEvent::Start(element)
                if current.is_some() && word_element(element, b"outlineLvl") =>
            {
                current.as_mut().expect("style exists").outline = attribute_local(element, b"val")
                    .and_then(|value| value.parse::<usize>().ok())
                    .filter(|value| *value <= MAX_OUTLINE_LEVEL)
                    .map(|value| value + 1);
            }
            XmlEvent::Empty(element) | XmlEvent::Start(element)
                if current.is_some()
                    && (word_element(element, b"vanish")
                        || word_element(element, b"webHidden"))
                    && on_off_enabled(element) =>
            {
                current.as_mut().expect("style exists").hidden = true;
            }
            XmlEvent::End {
                namespace,
                local_name,
            } if local_name == b"style" && is_wordprocessing(namespace.as_deref()) => {
                let style = current.take().ok_or(DocxError::RequiredPartInvalid)?;
                if style.hidden {
                    styles.hidden.insert(style.id.clone());
                }
                if style.paragraph {
                    let level = style
                        .outline
                        .or_else(|| style.name.as_deref().and_then(heading_level_from_name))
                        .or_else(|| heading_level_from_name(&style.id));
                    if let Some(level) = level.filter(|level| *level > 0) {
                        styles.headings.insert(style.id, level);
                    }
                }
            }
            _ => {}
        }
        Ok(())
    })?;
    Ok(styles)
}

pub(super) fn extract_document(
    bytes: &[u8],
    styles: &Styles,
    xml_budget: &mut XmlBudget,
    output_budget: &mut OutputBudget,
) -> Result<DocumentExtraction, DocxError> {
    let mut parser = StoryParser::new(ContentRole::Primary, styles, output_budget);
    parser.parse(bytes, xml_budget, ContainerMode::None)?;
    Ok(DocumentExtraction {
        sections: parser.sections,
        header_footer_references: parser.header_footer_references,
        unsupported_markup: parser.unsupported_markup,
    })
}

pub(super) fn extract_supporting_story(
    bytes: &[u8],
    styles: &Styles,
    xml_budget: &mut XmlBudget,
    output_budget: &mut OutputBudget,
) -> Result<SupportingExtraction, DocxError> {
    let mut parser = StoryParser::new(ContentRole::Supporting, styles, output_budget);
    parser.parse(bytes, xml_budget, ContainerMode::None)?;
    Ok(SupportingExtraction {
        sections: parser.sections,
        unsupported_markup: parser.unsupported_markup,
    })
}

pub(super) fn extract_notes(
    bytes: &[u8],
    kind: PartKind,
    styles: &Styles,
    xml_budget: &mut XmlBudget,
    output_budget: &mut OutputBudget,
) -> Result<SupportingExtraction, DocxError> {
    let local_name = match kind {
        PartKind::Footnotes => b"footnote".as_slice(),
        PartKind::Endnotes => b"endnote".as_slice(),
        _ => return Err(DocxError::RequiredPartInvalid),
    };
    let mut parser = StoryParser::new(ContentRole::Supporting, styles, output_budget);
    parser.parse(bytes, xml_budget, ContainerMode::Numeric(local_name))?;
    let mut ordered = BTreeMap::<i64, Vec<SemanticSection>>::new();
    for raw in parser.container_sections {
        let id = raw
            .container_id
            .parse::<i64>()
            .map_err(|_| DocxError::RequiredPartInvalid)?;
        if id > 0 {
            ordered.entry(id).or_default().push(raw.section);
        }
    }
    parser.sections = ordered.into_values().flatten().collect();
    Ok(SupportingExtraction {
        sections: parser.sections,
        unsupported_markup: parser.unsupported_markup,
    })
}

pub(super) fn extract_comments(
    bytes: &[u8],
    styles: &Styles,
    xml_budget: &mut XmlBudget,
    output_budget: &mut OutputBudget,
) -> Result<SupportingExtraction, DocxError> {
    let mut parser = StoryParser::new(ContentRole::Supporting, styles, output_budget);
    parser.parse(bytes, xml_budget, ContainerMode::Comment)?;
    let mut by_id = BTreeMap::<String, Vec<SemanticSection>>::new();
    for raw in parser.container_sections {
        by_id.entry(raw.container_id).or_default().push(raw.section);
    }
    let ordered = parser
        .container_order
        .into_iter()
        .flat_map(|id| by_id.remove(&id).unwrap_or_default())
        .collect();
    Ok(SupportingExtraction {
        sections: ordered,
        unsupported_markup: parser.unsupported_markup,
    })
}

#[derive(Debug, Clone, Copy)]
enum ContainerMode {
    None,
    Numeric(&'static [u8]),
    Comment,
}

#[derive(Debug)]
struct ContainerSection {
    container_id: String,
    section: SemanticSection,
}

#[derive(Debug, Default)]
struct Paragraph {
    fragments: Vec<(ContentRole, String)>,
    style: Option<String>,
    outline: Option<usize>,
}

impl Paragraph {
    fn append(&mut self, role: ContentRole, text: &str) {
        if text.is_empty() {
            return;
        }
        if let Some((last_role, value)) = self.fragments.last_mut()
            && *last_role == role
        {
            value.push_str(text);
        } else {
            self.fragments.push((role, text.to_owned()));
        }
    }

    fn by_role(&self, role: ContentRole) -> String {
        self.fragments
            .iter()
            .filter(|(fragment_role, _)| *fragment_role == role)
            .map(|(_, text)| text.as_str())
            .collect()
    }
}

#[derive(Debug, Default)]
struct TableRow {
    cells: Vec<BTreeMap<ContentRole, String>>,
    current_cell: Option<BTreeMap<ContentRole, String>>,
}

impl TableRow {
    fn start_cell(&mut self) -> Result<(), DocxError> {
        if self.current_cell.is_some() {
            return Err(DocxError::RequiredPartInvalid);
        }
        self.current_cell = Some(BTreeMap::new());
        Ok(())
    }

    fn push_paragraph(&mut self, paragraph: &Paragraph) -> Result<(), DocxError> {
        let cell = self
            .current_cell
            .as_mut()
            .ok_or(DocxError::RequiredPartInvalid)?;
        for role in [
            ContentRole::Primary,
            ContentRole::Supporting,
            ContentRole::Latent,
        ] {
            let text = normalize_fragment(paragraph.by_role(role));
            if text.is_empty() {
                continue;
            }
            let value = cell.entry(role).or_default();
            if !value.is_empty() {
                value.push('\n');
            }
            value.push_str(&text);
        }
        Ok(())
    }

    fn end_cell(&mut self) -> Result<(), DocxError> {
        self.cells.push(
            self.current_cell
                .take()
                .ok_or(DocxError::RequiredPartInvalid)?,
        );
        Ok(())
    }

    fn render(&self, role: ContentRole) -> String {
        self.cells
            .iter()
            .map(|cell| cell.get(&role).map_or("", String::as_str))
            .collect::<Vec<_>>()
            .join("\t")
    }
}

struct StoryParser<'a> {
    base_role: ContentRole,
    styles: &'a Styles,
    output_budget: &'a mut OutputBudget,
    sections: Vec<SemanticSection>,
    container_sections: Vec<ContainerSection>,
    container_id: Option<String>,
    container_order: Vec<String>,
    seen_container_ids: BTreeSet<String>,
    header_footer_references: Vec<HeaderFooterReference>,
    heading_stack: Vec<(usize, String)>,
    paragraph: Option<Paragraph>,
    row: Option<TableRow>,
    table_depth: usize,
    latent_depth: usize,
    run_hidden: bool,
    paragraph_hidden: bool,
    text_role: Option<ContentRole>,
    unsupported_markup: bool,
}

impl<'a> StoryParser<'a> {
    fn new(
        base_role: ContentRole,
        styles: &'a Styles,
        output_budget: &'a mut OutputBudget,
    ) -> Self {
        Self {
            base_role,
            styles,
            output_budget,
            sections: Vec::new(),
            container_sections: Vec::new(),
            container_id: None,
            container_order: Vec::new(),
            seen_container_ids: BTreeSet::new(),
            header_footer_references: Vec::new(),
            heading_stack: Vec::new(),
            paragraph: None,
            row: None,
            table_depth: 0,
            latent_depth: 0,
            run_hidden: false,
            paragraph_hidden: false,
            text_role: None,
            unsupported_markup: false,
        }
    }

    fn parse(
        &mut self,
        bytes: &[u8],
        budget: &mut XmlBudget,
        container_mode: ContainerMode,
    ) -> Result<(), DocxError> {
        parse_xml_events(bytes, budget, |event| self.event(event, container_mode))?;
        if self.paragraph.is_some()
            || self.row.is_some()
            || self.container_id.is_some()
            || self.latent_depth != 0
        {
            return Err(DocxError::RequiredPartInvalid);
        }
        Ok(())
    }

    fn event(&mut self, event: &XmlEvent, container_mode: ContainerMode) -> Result<(), DocxError> {
        match event {
            XmlEvent::Start(element) => self.start(element, false, container_mode),
            XmlEvent::Empty(element) => self.start(element, true, container_mode),
            XmlEvent::End {
                namespace,
                local_name,
            } => self.end(namespace.as_deref(), local_name, container_mode),
            XmlEvent::Text(text) => {
                if let (Some(paragraph), Some(role)) = (self.paragraph.as_mut(), self.text_role) {
                    paragraph.append(role, text);
                }
                Ok(())
            }
        }
    }

    fn start(
        &mut self,
        element: &XmlElement,
        empty: bool,
        container_mode: ContainerMode,
    ) -> Result<(), DocxError> {
        if !is_wordprocessing(element.namespace.as_deref()) {
            return Ok(());
        }
        let name = element.local_name.as_slice();
        if container_name(container_mode) == Some(name) {
            if self.container_id.is_some() {
                return Err(DocxError::RequiredPartInvalid);
            }
            let id = attribute_local(element, b"id")
                .filter(|id| !id.is_empty())
                .ok_or(DocxError::RequiredPartInvalid)?
                .to_owned();
            if !self.seen_container_ids.insert(id.clone()) {
                return Err(DocxError::RequiredPartInvalid);
            }
            self.container_order.push(id.clone());
            self.heading_stack.clear();
            self.container_id = Some(id);
        }
        match name {
            b"p" => {
                if self.paragraph.is_some() {
                    return Err(DocxError::RequiredPartInvalid);
                }
                self.paragraph_hidden = false;
                self.paragraph = Some(Paragraph::default());
            }
            b"pStyle" => {
                if let Some(paragraph) = self.paragraph.as_mut() {
                    let style = attribute_local(element, b"val").map(str::to_owned);
                    self.paragraph_hidden = style
                        .as_deref()
                        .is_some_and(|style| self.styles.hidden.contains(style));
                    paragraph.style = style;
                }
            }
            b"rStyle" => {
                if let Some(style) = attribute_local(element, b"val")
                    && self.styles.hidden.contains(style)
                {
                    self.run_hidden = true;
                }
            }
            b"outlineLvl" => {
                if let Some(paragraph) = self.paragraph.as_mut() {
                    paragraph.outline = attribute_local(element, b"val")
                        .and_then(|value| value.parse::<usize>().ok())
                        .filter(|value| *value <= MAX_OUTLINE_LEVEL)
                        .map(|value| value + 1);
                }
            }
            b"r" => self.run_hidden = false,
            b"vanish" | b"webHidden" if on_off_enabled(element) => {
                self.run_hidden = true;
            }
            b"del" | b"moveFrom" => {
                self.latent_depth = self
                    .latent_depth
                    .checked_add(1)
                    .ok_or(DocxError::XmlLimitExceeded)?;
            }
            b"t" => self.text_role = Some(self.active_role()),
            b"instrText" | b"delText" => self.text_role = Some(ContentRole::Latent),
            b"tab" => self.append_control("\t"),
            b"br" | b"cr" => self.append_control("\n"),
            b"fldSimple" => {
                if let Some(instruction) = attribute_local(element, b"instr")
                    && let Some(paragraph) = self.paragraph.as_mut()
                {
                    paragraph.append(ContentRole::Latent, instruction);
                }
            }
            b"tbl" => {
                self.table_depth = self
                    .table_depth
                    .checked_add(1)
                    .ok_or(DocxError::XmlLimitExceeded)?;
                if self.table_depth > 1 {
                    self.unsupported_markup = true;
                }
            }
            b"tr" if self.table_depth == 1 => {
                if self.row.is_some() {
                    return Err(DocxError::RequiredPartInvalid);
                }
                self.row = Some(TableRow::default());
            }
            b"tc" if self.table_depth == 1 => {
                self.row
                    .as_mut()
                    .ok_or(DocxError::RequiredPartInvalid)?
                    .start_cell()?;
            }
            b"headerReference" | b"footerReference" => {
                let relationship_id = attribute_local(element, b"id")
                    .filter(|id| !id.is_empty())
                    .ok_or(DocxError::RequiredPartInvalid)?;
                self.header_footer_references.push(HeaderFooterReference {
                    kind: if name == b"headerReference" {
                        HeaderFooterKind::Header
                    } else {
                        HeaderFooterKind::Footer
                    },
                    relationship_id: relationship_id.to_owned(),
                });
            }
            b"altChunk" | b"subDoc" => self.unsupported_markup = true,
            _ => {}
        }

        if empty {
            self.end(element.namespace.as_deref(), name, container_mode)?;
        }
        Ok(())
    }

    fn end(
        &mut self,
        namespace: Option<&[u8]>,
        name: &[u8],
        container_mode: ContainerMode,
    ) -> Result<(), DocxError> {
        if !is_wordprocessing(namespace) {
            return Ok(());
        }
        match name {
            b"p" => self.finish_paragraph()?,
            b"r" => self.run_hidden = false,
            b"del" | b"moveFrom" => {
                self.latent_depth = self
                    .latent_depth
                    .checked_sub(1)
                    .ok_or(DocxError::RequiredPartInvalid)?;
            }
            b"t" | b"instrText" | b"delText" => self.text_role = None,
            b"tc" if self.table_depth == 1 => {
                self.row
                    .as_mut()
                    .ok_or(DocxError::RequiredPartInvalid)?
                    .end_cell()?;
            }
            b"tr" if self.table_depth == 1 => self.finish_row()?,
            b"tbl" => {
                self.table_depth = self
                    .table_depth
                    .checked_sub(1)
                    .ok_or(DocxError::RequiredPartInvalid)?;
            }
            _ => {}
        }
        if container_name(container_mode) == Some(name) {
            self.container_id
                .take()
                .ok_or(DocxError::RequiredPartInvalid)?;
        }
        Ok(())
    }

    fn active_role(&self) -> ContentRole {
        if self.latent_depth != 0 || self.run_hidden || self.paragraph_hidden {
            ContentRole::Latent
        } else {
            self.base_role
        }
    }

    fn append_control(&mut self, text: &str) {
        let role = self.active_role();
        if let Some(paragraph) = self.paragraph.as_mut() {
            paragraph.append(role, text);
        }
    }

    fn finish_paragraph(&mut self) -> Result<(), DocxError> {
        let paragraph = self
            .paragraph
            .take()
            .ok_or(DocxError::RequiredPartInvalid)?;
        let heading_level = paragraph
            .outline
            .or_else(|| {
                paragraph
                    .style
                    .as_ref()
                    .and_then(|style| self.styles.headings.get(style).copied())
            })
            .or_else(|| paragraph.style.as_deref().and_then(heading_level_from_name));
        if let Some(level) = heading_level {
            let heading = normalize_fragment(paragraph.by_role(self.base_role));
            if !heading.is_empty() {
                while self
                    .heading_stack
                    .last()
                    .is_some_and(|(active_level, _)| *active_level >= level)
                {
                    self.heading_stack.pop();
                }
                self.heading_stack.push((level, heading));
            }
        }

        if self.table_depth == 1 {
            self.row
                .as_mut()
                .ok_or(DocxError::RequiredPartInvalid)?
                .push_paragraph(&paragraph)?;
            return Ok(());
        }
        for role in [
            ContentRole::Primary,
            ContentRole::Supporting,
            ContentRole::Latent,
        ] {
            self.emit(paragraph.by_role(role), role)?;
        }
        Ok(())
    }

    fn finish_row(&mut self) -> Result<(), DocxError> {
        let row = self.row.take().ok_or(DocxError::RequiredPartInvalid)?;
        if row.current_cell.is_some() {
            return Err(DocxError::RequiredPartInvalid);
        }
        for role in [
            ContentRole::Primary,
            ContentRole::Supporting,
            ContentRole::Latent,
        ] {
            self.emit(row.render(role), role)?;
        }
        Ok(())
    }

    fn emit(&mut self, content: String, role: ContentRole) -> Result<(), DocxError> {
        let mut temporary = Vec::new();
        // Disjoint field borrows keep the heading stack borrowed immutably
        // while the budget is charged, so no copy is made for empty roles.
        let heading_stack = &self.heading_stack;
        self.output_budget
            .push(&mut temporary, heading_stack, content, role)?;
        for section in temporary {
            if let Some(container_id) = &self.container_id {
                self.container_sections.push(ContainerSection {
                    container_id: container_id.clone(),
                    section,
                });
            } else {
                self.sections.push(section);
            }
        }
        Ok(())
    }
}

fn container_name(mode: ContainerMode) -> Option<&'static [u8]> {
    match mode {
        ContainerMode::None => None,
        ContainerMode::Numeric(name) => Some(name),
        ContainerMode::Comment => Some(b"comment"),
    }
}

fn word_element(element: &XmlElement, local_name: &[u8]) -> bool {
    element.local_name == local_name && is_wordprocessing(element.namespace.as_deref())
}

fn is_wordprocessing(namespace: Option<&[u8]>) -> bool {
    namespace.is_some_and(|namespace| {
        namespace == WORDPROCESSING_NS_TRANSITIONAL || namespace == WORDPROCESSING_NS_STRICT
    })
}

fn attribute_local<'a>(element: &'a XmlElement, local_name: &[u8]) -> Option<&'a str> {
    element
        .attributes
        .iter()
        .find(|(name, _)| {
            name.as_slice() == local_name
                || name
                    .rsplit(|byte| *byte == b':')
                    .next()
                    .is_some_and(|name| name == local_name)
        })
        .map(|(_, value)| value.as_str())
}

fn on_off_enabled(element: &XmlElement) -> bool {
    !matches!(
        attribute_local(element, b"val")
            .map(str::to_ascii_lowercase)
            .as_deref(),
        Some("0" | "false" | "off" | "no")
    )
}

fn heading_level_from_name(value: &str) -> Option<usize> {
    let normalized = value
        .chars()
        .filter(|character| !character.is_ascii_whitespace() && *character != '-')
        .flat_map(char::to_lowercase)
        .collect::<String>();
    normalized
        .strip_prefix("heading")?
        .parse::<usize>()
        .ok()
        .filter(|level| (1..=9).contains(level))
}

fn normalize_fragment(value: String) -> String {
    value
        .split('\n')
        .map(str::trim)
        .collect::<Vec<_>>()
        .join("\n")
        .trim()
        .to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn encoded_document(encoding: &str) -> Vec<u8> {
        let xml = format!(
            r#"<?xml version="1.0" encoding="{encoding}"?><w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body><w:p><w:r><w:t>Portable text</w:t></w:r></w:p></w:body></w:document>"#,
        );
        match encoding {
            "UTF-8" => xml.into_bytes(),
            "UTF-16LE" => {
                let mut bytes = vec![0xff, 0xfe];
                for unit in xml.encode_utf16() {
                    bytes.extend_from_slice(&unit.to_le_bytes());
                }
                bytes
            }
            "UTF-16BE" => {
                let mut bytes = vec![0xfe, 0xff];
                for unit in xml.encode_utf16() {
                    bytes.extend_from_slice(&unit.to_be_bytes());
                }
                bytes
            }
            _ => unreachable!(),
        }
    }

    #[test]
    fn duplicate_comment_ids_are_rejected() {
        let comments = br#"<w:comments xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:comment w:id="1"><w:p><w:r><w:t>One</w:t></w:r></w:p></w:comment><w:comment w:id="1"><w:p><w:r><w:t>Two</w:t></w:r></w:p></w:comment></w:comments>"#;
        assert_eq!(
            extract_comments(
                comments,
                &Styles::default(),
                &mut XmlBudget::default(),
                &mut OutputBudget::default(),
            )
            .unwrap_err(),
            DocxError::RequiredPartInvalid
        );
    }

    #[test]
    fn utf8_utf16le_and_utf16be_extract_identically() {
        let mut results = Vec::new();
        for encoding in ["UTF-8", "UTF-16LE", "UTF-16BE"] {
            let extracted = extract_document(
                &encoded_document(encoding),
                &Styles::default(),
                &mut XmlBudget::default(),
                &mut OutputBudget::default(),
            )
            .unwrap_or_else(|error| panic!("{encoding}: {error:?}"));
            results.push(extracted.sections);
        }
        assert_eq!(results[0], results[1]);
        assert_eq!(results[1], results[2]);
        assert_eq!(results[0][0].content, "Portable text");
    }
}
