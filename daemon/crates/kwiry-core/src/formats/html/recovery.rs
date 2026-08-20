// SPDX-License-Identifier: MIT OR Apache-2.0

use crate::extract::ContentRole;

use super::error::{HtmlError, HtmlStage};
use super::limits::{Budget, HtmlLimits};
use super::model::{Arena, ElementNode, NodeId, NodeKind, RecoveredDocument, TextNode};
use super::tokenizer::{Attributes, Tag, Token, Tokenizer};

pub(super) fn recover(
    input: &str,
    budget: &mut Budget,
    limits: &HtmlLimits,
) -> Result<RecoveredDocument, HtmlError> {
    let arena = Arena::new(budget, limits)?;
    let root = arena.root();
    let mut open = Vec::new();
    open.try_reserve_exact(1)
        .map_err(|_| HtmlError::limit(HtmlStage::Recover))?;
    open.push(root);
    let mut recovery = Recovery {
        arena,
        open,
        active_formatting: Vec::new(),
        template_modes: Vec::new(),
        omitted: Vec::new(),
        title_capture: None,
        title: None,
        description: None,
        in_head: false,
        head_seen: false,
        body_started: false,
        pending_table_text: Vec::new(),
    };
    let mut tokenizer = Tokenizer::new(input);
    loop {
        let token = tokenizer.next_token(budget, limits)?;
        let eof = matches!(token, Token::Eof);
        recovery.process(token, budget, limits)?;
        if eof {
            break;
        }
    }
    Ok(RecoveredDocument {
        arena: recovery.arena,
        title: recovery.title,
        description: recovery.description,
        recovered_errors: budget.parse_errors,
    })
}

struct Recovery {
    arena: Arena,
    open: Vec<NodeId>,
    active_formatting: Vec<(Tag, NodeId)>,
    template_modes: Vec<()>,
    omitted: Vec<Tag>,
    title_capture: Option<String>,
    title: Option<String>,
    description: Option<String>,
    in_head: bool,
    head_seen: bool,
    body_started: bool,
    pending_table_text: Vec<String>,
}

impl Recovery {
    fn process(
        &mut self,
        token: Token,
        budget: &mut Budget,
        limits: &HtmlLimits,
    ) -> Result<(), HtmlError> {
        if !matches!(token, Token::Text(_)) {
            self.flush_pending_table_text(budget, limits)?;
        }

        if self.title_capture.is_some() {
            return self.process_title(token, budget, limits);
        }
        if !self.omitted.is_empty() {
            return self.process_omitted(token, budget, limits);
        }

        match token {
            Token::StartTag {
                tag,
                attributes,
                self_closing,
            } => self.start_tag(tag, attributes, self_closing, budget, limits),
            Token::EndTag(tag) => self.end_tag(tag, budget, limits),
            Token::Text(text) => self.text(text, budget, limits),
            Token::Ignored => Ok(()),
            Token::Eof => {
                self.flush_pending_table_text(budget, limits)?;
                if !self.open.is_empty() {
                    self.pop_from(1, budget);
                }
                Ok(())
            }
        }
    }

    fn process_title(
        &mut self,
        token: Token,
        budget: &mut Budget,
        limits: &HtmlLimits,
    ) -> Result<(), HtmlError> {
        match token {
            Token::Text(text) => {
                budget.retained_text(limits, text.len())?;
                let capture = self.title_capture.as_mut().expect("title capture exists");
                let new_len = capture
                    .len()
                    .checked_add(text.len())
                    .filter(|length| *length <= limits.logical_heading_bytes)
                    .ok_or_else(|| HtmlError::limit(HtmlStage::Recover))?;
                if new_len > capture.capacity() {
                    capture
                        .try_reserve_exact(new_len - capture.capacity())
                        .map_err(|_| HtmlError::limit(HtmlStage::Recover))?;
                }
                capture.push_str(&text);
                Ok(())
            }
            Token::EndTag(Tag::Title) | Token::Eof => {
                let raw = self.title_capture.take().expect("title capture exists");
                let title = normalize_text(&raw, budget, limits, HtmlStage::Recover)?;
                drop(raw);
                budget.leave_heading_scratch(title.len());
                if !title.is_empty() && self.title.is_none() {
                    self.title = Some(title);
                }
                Ok(())
            }
            Token::StartTag { .. } | Token::EndTag(_) | Token::Ignored => {
                budget.parse_error(limits)
            }
        }
    }

    fn process_omitted(
        &mut self,
        token: Token,
        budget: &mut Budget,
        limits: &HtmlLimits,
    ) -> Result<(), HtmlError> {
        match token {
            Token::StartTag {
                tag, self_closing, ..
            } if !tag.is_void() && !self_closing => {
                if self.omitted.len() == limits.open_elements {
                    return Err(HtmlError::limit(HtmlStage::Recover));
                }
                if tag == Tag::Template {
                    if self.template_modes.len() == limits.template_modes {
                        return Err(HtmlError::limit(HtmlStage::Recover));
                    }
                    self.template_modes
                        .try_reserve_exact(1)
                        .map_err(|_| HtmlError::limit(HtmlStage::Recover))?;
                    self.template_modes.push(());
                }
                self.omitted
                    .try_reserve_exact(1)
                    .map_err(|_| HtmlError::limit(HtmlStage::Recover))?;
                self.omitted.push(tag);
                Ok(())
            }
            Token::EndTag(tag) => {
                if let Some(index) = self.omitted.iter().rposition(|candidate| *candidate == tag) {
                    for removed in self.omitted.drain(index..) {
                        if removed == Tag::Template {
                            self.template_modes.pop();
                        }
                    }
                } else {
                    budget.parse_error(limits)?;
                }
                Ok(())
            }
            Token::Eof => {
                self.omitted.clear();
                self.template_modes.clear();
                Ok(())
            }
            Token::StartTag { .. } | Token::Text(_) | Token::Ignored => Ok(()),
        }
    }

    fn start_tag(
        &mut self,
        tag: Tag,
        attributes: Attributes,
        self_closing: bool,
        budget: &mut Budget,
        limits: &HtmlLimits,
    ) -> Result<(), HtmlError> {
        match tag {
            Tag::Head => {
                if self.in_head || self.head_seen || self.body_started {
                    // A body-level or duplicate head token is ignored by the
                    // text/html tree builder; it must not switch ordinary body
                    // descendants into a dropping mode.
                    budget.parse_error(limits)?;
                    return Ok(());
                }
                self.in_head = true;
                self.head_seen = true;
                return Ok(());
            }
            Tag::Body => {
                self.in_head = false;
                self.body_started = true;
            }
            Tag::Title if !self.body_started => {
                if self.title.is_none() {
                    self.title_capture = Some(String::new());
                } else if !self_closing {
                    if self.omitted.len() == limits.open_elements {
                        return Err(HtmlError::limit(HtmlStage::Recover));
                    }
                    self.omitted
                        .try_reserve_exact(1)
                        .map_err(|_| HtmlError::limit(HtmlStage::Recover))?;
                    self.omitted.push(Tag::Title);
                }
                return Ok(());
            }
            Tag::Meta if !self.body_started => {
                if self.description.is_none()
                    && attributes
                        .meta_name
                        .as_deref()
                        .is_some_and(|name| name.trim().eq_ignore_ascii_case("description"))
                    && let Some(content) = attributes.meta_content
                {
                    budget.retained_text(limits, content.len())?;
                    let description = normalize_text(&content, budget, limits, HtmlStage::Recover)?;
                    drop(content);
                    budget.leave_heading_scratch(description.len());
                    if !description.is_empty() {
                        self.description = Some(description);
                    }
                }
                return Ok(());
            }
            _ => {}
        }

        if tag.is_omitted_subtree() {
            if !tag.is_void() && !self_closing {
                if self.omitted.len() == limits.open_elements {
                    return Err(HtmlError::limit(HtmlStage::Recover));
                }
                if tag == Tag::Template {
                    if self.template_modes.len() == limits.template_modes {
                        return Err(HtmlError::limit(HtmlStage::Recover));
                    }
                    self.template_modes
                        .try_reserve_exact(1)
                        .map_err(|_| HtmlError::limit(HtmlStage::Recover))?;
                    self.template_modes.push(());
                }
                self.omitted
                    .try_reserve_exact(1)
                    .map_err(|_| HtmlError::limit(HtmlStage::Recover))?;
                self.omitted.push(tag);
            }
            return Ok(());
        }
        if self.in_head && !self.in_head_noscript() && !matches!(tag, Tag::Noscript | Tag::Html) {
            // The in-head insertion mode implicitly closes the head and
            // reprocesses the first body token. Dropping that token loses valid
            // recoverable reader text when `</head>` is omitted.
            self.in_head = false;
            self.body_started = true;
        } else if !self.in_head && !matches!(tag, Tag::Html | Tag::Head | Tag::Meta | Tag::Title) {
            self.body_started = true;
        }

        self.apply_start_recovery(tag, budget, limits)?;
        let role = self.effective_role(tag, &attributes);
        let fostered = self.in_table_without_cell() && !tag.is_table_structural();
        let node = self.arena.push(
            NodeKind::Element(ElementNode {
                tag,
                role,
                fostered,
            }),
            budget,
            limits,
        )?;
        self.attach(node, fostered, budget, limits)?;

        if matches!(tag, Tag::Td | Tag::Th) {
            budget.table_cell(limits)?;
        }
        if tag == Tag::Table {
            budget.table_context(limits)?;
        }

        if let Some(label) = attributes.aria_label {
            self.append_text_node(node, label, ContentRole::Latent, true, budget, limits)?;
        }
        if matches!(tag, Tag::Img | Tag::Area)
            && let Some(alt) = attributes.alt
        {
            self.append_text_node(node, alt, role, true, budget, limits)?;
        }

        if tag.is_formatting() {
            if self.active_formatting.len() == limits.active_formatting {
                return Err(HtmlError::limit(HtmlStage::Recover));
            }
            self.active_formatting
                .try_reserve_exact(1)
                .map_err(|_| HtmlError::limit(HtmlStage::Recover))?;
            self.active_formatting.push((tag, node));
        }
        if !tag.is_void() && !self_closing {
            if self.open.len() == limits.open_elements {
                return Err(HtmlError::limit(HtmlStage::Recover));
            }
            self.open
                .try_reserve_exact(1)
                .map_err(|_| HtmlError::limit(HtmlStage::Recover))?;
            self.open.push(node);
        }
        Ok(())
    }

    fn end_tag(
        &mut self,
        tag: Tag,
        budget: &mut Budget,
        limits: &HtmlLimits,
    ) -> Result<(), HtmlError> {
        if tag == Tag::Head {
            if self.in_head && !self.in_head_noscript() {
                self.in_head = false;
            } else {
                budget.parse_error(limits)?;
            }
            return Ok(());
        }
        if tag.is_formatting() {
            return self.adoption_agency(tag, budget, limits);
        }
        let Some(index) = self
            .open
            .iter()
            .rposition(|node| self.node_tag(*node) == Some(tag))
        else {
            budget.parse_error(limits)?;
            return Ok(());
        };
        self.pop_from(index, budget);
        Ok(())
    }

    fn text(
        &mut self,
        text: String,
        budget: &mut Budget,
        limits: &HtmlLimits,
    ) -> Result<(), HtmlError> {
        if text.trim().is_empty() {
            return Ok(());
        }
        if self.in_head && !self.in_head_noscript() {
            self.in_head = false;
            self.body_started = true;
        }
        if self.in_table_without_cell() {
            budget.pending_table_text(limits, text.len())?;
            self.pending_table_text
                .try_reserve_exact(1)
                .map_err(|_| HtmlError::limit(HtmlStage::Recover))?;
            self.pending_table_text.push(text);
            return Ok(());
        }
        let parent = *self.open.last().expect("root remains open");
        let role = self.node_role(parent).unwrap_or(ContentRole::Primary);
        self.append_text_node(parent, text, role, false, budget, limits)
    }

    fn flush_pending_table_text(
        &mut self,
        budget: &mut Budget,
        limits: &HtmlLimits,
    ) -> Result<(), HtmlError> {
        let pending = std::mem::take(&mut self.pending_table_text);
        for text in pending {
            if text.trim().is_empty() {
                continue;
            }
            let role = self.current_role();
            budget.retained_text(limits, text.len())?;
            let node = self.arena.push(
                NodeKind::Text(TextNode {
                    text,
                    role,
                    synthetic: false,
                }),
                budget,
                limits,
            )?;
            self.attach(node, true, budget, limits)?;
        }
        budget.clear_pending_table_text();
        Ok(())
    }

    fn append_text_node(
        &mut self,
        parent: NodeId,
        text: String,
        role: ContentRole,
        synthetic: bool,
        budget: &mut Budget,
        limits: &HtmlLimits,
    ) -> Result<(), HtmlError> {
        if text.trim().is_empty() {
            return Ok(());
        }
        budget.retained_text(limits, text.len())?;
        let node = self.arena.push(
            NodeKind::Text(TextNode {
                text,
                role,
                synthetic,
            }),
            budget,
            limits,
        )?;
        self.arena.append_child(parent, node, budget, limits)
    }

    fn attach(
        &mut self,
        node: NodeId,
        foster: bool,
        budget: &mut Budget,
        limits: &HtmlLimits,
    ) -> Result<(), HtmlError> {
        if foster
            && let Some(table_index) = self
                .open
                .iter()
                .rposition(|candidate| self.node_tag(*candidate) == Some(Tag::Table))
        {
            let table = self.open[table_index];
            if let Some(parent) = self.arena.get(table).parent {
                return self
                    .arena
                    .insert_before(parent, table, node, budget, limits);
            }
            if table_index > 0 {
                return self
                    .arena
                    .append_child(self.open[table_index - 1], node, budget, limits);
            }
        }
        let parent = *self.open.last().expect("root remains open");
        self.arena.append_child(parent, node, budget, limits)
    }

    fn apply_start_recovery(
        &mut self,
        tag: Tag,
        budget: &mut Budget,
        limits: &HtmlLimits,
    ) -> Result<(), HtmlError> {
        if (tag == Tag::P || (tag.is_block() && !matches!(tag, Tag::Html | Tag::Body)))
            && let Some(index) = self
                .open
                .iter()
                .rposition(|node| self.node_tag(*node) == Some(Tag::P))
        {
            self.pop_from(index, budget);
        }
        match tag {
            Tag::H(_) => {
                if let Some(index) = self
                    .open
                    .iter()
                    .rposition(|node| matches!(self.node_tag(*node), Some(Tag::H(_))))
                {
                    self.pop_from(index, budget);
                }
            }
            Tag::Li => self.close_same_until(tag, &[Tag::Ul, Tag::Ol], budget),
            Tag::Dt | Tag::Dd => self.close_either_until(&[Tag::Dt, Tag::Dd], &[Tag::Dl], budget),
            Tag::Tr => self.close_same_until(
                Tag::Tr,
                &[Tag::Table, Tag::Tbody, Tag::Thead, Tag::Tfoot],
                budget,
            ),
            Tag::Td | Tag::Th => self.close_either_until(&[Tag::Td, Tag::Th], &[Tag::Tr], budget),
            Tag::Tbody | Tag::Thead | Tag::Tfoot => self.close_either_until(
                &[Tag::Tbody, Tag::Thead, Tag::Tfoot],
                &[Tag::Table],
                budget,
            ),
            Tag::A
                if self
                    .active_formatting
                    .iter()
                    .any(|(candidate, _)| *candidate == Tag::A) =>
            {
                self.adoption_agency(Tag::A, budget, limits)?;
            }
            _ => {}
        }
        Ok(())
    }

    fn close_same_until(&mut self, target: Tag, barriers: &[Tag], budget: &mut Budget) {
        for index in (1..self.open.len()).rev() {
            let tag = self.node_tag(self.open[index]);
            if tag == Some(target) {
                self.pop_from(index, budget);
                return;
            }
            if tag.is_some_and(|tag| barriers.contains(&tag)) {
                return;
            }
        }
    }

    fn close_either_until(&mut self, targets: &[Tag], barriers: &[Tag], budget: &mut Budget) {
        for index in (1..self.open.len()).rev() {
            let tag = self.node_tag(self.open[index]);
            if tag.is_some_and(|tag| targets.contains(&tag)) {
                self.pop_from(index, budget);
                return;
            }
            if tag.is_some_and(|tag| barriers.contains(&tag)) {
                return;
            }
        }
    }

    fn adoption_agency(
        &mut self,
        tag: Tag,
        budget: &mut Budget,
        limits: &HtmlLimits,
    ) -> Result<(), HtmlError> {
        let Some(open_index) = self
            .open
            .iter()
            .rposition(|node| self.node_tag(*node) == Some(tag))
        else {
            self.active_formatting
                .retain(|(candidate, _)| *candidate != tag);
            budget.parse_error(limits)?;
            return Ok(());
        };
        let mut reopen = Vec::new();
        for node in self.open.iter().copied().skip(open_index + 1) {
            if let NodeKind::Element(element) = self.arena.get(node).kind
                && element.tag.is_formatting()
            {
                if reopen.len() == limits.active_formatting {
                    return Err(HtmlError::limit(HtmlStage::Recover));
                }
                budget.recovery_scratch_entry(limits)?;
                reopen
                    .try_reserve_exact(1)
                    .map_err(|_| HtmlError::limit(HtmlStage::Recover))?;
                reopen.push(element);
            }
        }
        let removed = &self.open[open_index..];
        self.active_formatting
            .retain(|(_, node)| !removed.contains(node));
        self.pop_from(open_index, budget);
        let reopen_count = reopen.len();
        for element in reopen {
            budget.mutation(limits, 1)?;
            let node = self
                .arena
                .push(NodeKind::Element(element), budget, limits)?;
            self.attach(node, element.fostered, budget, limits)?;
            if self.open.len() == limits.open_elements
                || self.active_formatting.len() == limits.active_formatting
            {
                return Err(HtmlError::limit(HtmlStage::Recover));
            }
            self.open
                .try_reserve_exact(1)
                .map_err(|_| HtmlError::limit(HtmlStage::Recover))?;
            self.active_formatting
                .try_reserve_exact(1)
                .map_err(|_| HtmlError::limit(HtmlStage::Recover))?;
            self.open.push(node);
            self.active_formatting.push((element.tag, node));
        }
        budget.leave_recovery_scratch(reopen_count);
        Ok(())
    }

    fn effective_role(&self, tag: Tag, attributes: &Attributes) -> ContentRole {
        let parent_role = self.current_role();
        if !parent_role.is_primary() {
            return parent_role;
        }
        let landmark = attributes
            .role
            .as_deref()
            .is_some_and(first_recognized_role_is_chrome);
        let header_is_chrome = tag == Tag::Header
            && !self.open.iter().rev().any(|node| {
                matches!(
                    self.node_tag(*node),
                    Some(Tag::Main | Tag::Article | Tag::Section)
                )
            });
        if attributes.hidden
            || attributes.aria_hidden
            || attributes.display_none
            || landmark
            || header_is_chrome
            || matches!(tag, Tag::Noscript | Tag::Nav | Tag::Aside | Tag::Footer)
        {
            ContentRole::Latent
        } else {
            ContentRole::Primary
        }
    }

    fn in_head_noscript(&self) -> bool {
        self.open
            .iter()
            .rev()
            .any(|node| self.node_tag(*node) == Some(Tag::Noscript))
    }

    fn current_role(&self) -> ContentRole {
        self.open
            .last()
            .and_then(|node| self.node_role(*node))
            .unwrap_or(ContentRole::Primary)
    }

    fn node_tag(&self, node: NodeId) -> Option<Tag> {
        match self.arena.get(node).kind {
            NodeKind::Element(element) => Some(element.tag),
            NodeKind::Root | NodeKind::Text(_) => None,
        }
    }

    fn node_role(&self, node: NodeId) -> Option<ContentRole> {
        match self.arena.get(node).kind {
            NodeKind::Element(element) => Some(element.role),
            NodeKind::Text(ref text) => Some(text.role),
            NodeKind::Root => None,
        }
    }

    fn in_table_without_cell(&self) -> bool {
        for node in self.open.iter().rev().copied() {
            match self.arena.get(node).kind {
                NodeKind::Element(ElementNode {
                    tag: Tag::Td | Tag::Th,
                    ..
                }) => return false,
                NodeKind::Element(ElementNode { fostered: true, .. }) => return false,
                NodeKind::Element(ElementNode {
                    tag: Tag::Table, ..
                }) => return true,
                _ => {}
            }
        }
        false
    }

    fn pop_from(&mut self, index: usize, budget: &mut Budget) {
        let tables = self.open[index..]
            .iter()
            .filter(|node| self.node_tag(**node) == Some(Tag::Table))
            .count();
        self.open.truncate(index);
        for _ in 0..tables {
            budget.leave_table_context();
        }
    }
}

fn first_recognized_role_is_chrome(value: &str) -> bool {
    for token in value.split_ascii_whitespace() {
        if ["navigation", "complementary", "contentinfo", "banner"]
            .iter()
            .any(|role| token.eq_ignore_ascii_case(role))
        {
            return true;
        }
        if [
            "alert",
            "alertdialog",
            "application",
            "article",
            "button",
            "cell",
            "checkbox",
            "columnheader",
            "combobox",
            "command",
            "definition",
            "dialog",
            "directory",
            "document",
            "feed",
            "figure",
            "form",
            "grid",
            "gridcell",
            "group",
            "heading",
            "img",
            "link",
            "list",
            "listbox",
            "listitem",
            "log",
            "main",
            "marquee",
            "math",
            "menu",
            "menubar",
            "menuitem",
            "menuitemcheckbox",
            "menuitemradio",
            "meter",
            "none",
            "note",
            "option",
            "presentation",
            "progressbar",
            "radio",
            "radiogroup",
            "region",
            "row",
            "rowgroup",
            "rowheader",
            "scrollbar",
            "search",
            "searchbox",
            "separator",
            "slider",
            "spinbutton",
            "status",
            "switch",
            "tab",
            "table",
            "tablist",
            "tabpanel",
            "term",
            "textbox",
            "timer",
            "toolbar",
            "tooltip",
            "tree",
            "treegrid",
            "treeitem",
        ]
        .iter()
        .any(|role| token.eq_ignore_ascii_case(role))
        {
            return false;
        }
    }
    false
}

fn normalize_text(
    input: &str,
    budget: &mut Budget,
    limits: &HtmlLimits,
    stage: HtmlStage,
) -> Result<String, HtmlError> {
    let mut output = String::new();
    let mut pending_space = false;
    for character in input.chars() {
        if character.is_whitespace() {
            pending_space = !output.is_empty();
            continue;
        }
        let extra = character.len_utf8() + usize::from(pending_space);
        let new_len = output
            .len()
            .checked_add(extra)
            .filter(|length| *length <= limits.logical_heading_bytes)
            .ok_or_else(|| HtmlError::limit(stage))?;
        budget.heading_scratch(limits, extra, stage)?;
        if new_len > output.capacity() {
            output
                .try_reserve_exact(new_len - output.capacity())
                .map_err(|_| HtmlError::limit(stage))?;
        }
        if pending_space {
            output.push(' ');
        }
        output.push(character);
        pending_space = false;
    }
    Ok(output)
}
