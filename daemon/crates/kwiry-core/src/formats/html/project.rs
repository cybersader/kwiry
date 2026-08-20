// SPDX-License-Identifier: MIT OR Apache-2.0

use crate::extract::{
    ContentRole, ExtractedSection, ExtractedSource, ExtractionCompleteness, ExtractionCoverage,
    ExtractionNotice,
};
use crate::model::{Frontmatter, PropertyBag};

use super::error::{HtmlError, HtmlStage};
use super::limits::{Budget, HtmlLimits, MAX_HEADING_DEPTH};
use super::model::{Arena, NodeId, NodeKind, RecoveredDocument};
use super::tokenizer::Tag;

pub(super) fn project(
    document: RecoveredDocument,
    budget: &mut Budget,
    limits: &HtmlLimits,
) -> Result<ExtractedSource, HtmlError> {
    let mut projector = Projector {
        arena: &document.arena,
        sections: Vec::new(),
        primary_headings: std::array::from_fn(|_| None),
        latent_headings: std::array::from_fn(|_| None),
        current: String::new(),
        current_role: None,
        output_text_bytes: 0,
    };

    if let Some(description) = document.description {
        projector.push_content_section(description, ContentRole::Latent, budget, limits)?;
    }
    projector.walk(budget, limits)?;
    projector.flush(budget, limits)?;

    let frontmatter = Frontmatter {
        title: document.title,
        ..Frontmatter::default()
    };
    if projector.sections.is_empty() && frontmatter.title.is_none() {
        return Ok(ExtractedSource::skipped(
            ExtractionCoverage::SkippedNoExtractableText,
            ExtractionNotice::new(
                "html_no_extractable_text",
                "HTML source contains no searchable reader text",
            ),
        ));
    }

    let mut notices = Vec::new();
    if document.recovered_errors != 0 {
        budget.notice(limits)?;
        notices
            .try_reserve_exact(1)
            .map_err(|_| HtmlError::limit(HtmlStage::Project))?;
        notices.push(ExtractionNotice::new(
            "html_recovered_parse_error",
            "HTML markup was deterministically recovered",
        ));
    }
    Ok(ExtractedSource::indexed(
        PropertyBag::default(),
        frontmatter,
        Vec::new(),
        Vec::new(),
        projector.sections,
        ExtractionCompleteness::Complete,
        notices,
    ))
}

struct Projector<'a> {
    arena: &'a Arena,
    sections: Vec<ExtractedSection>,
    primary_headings: [Option<String>; MAX_HEADING_DEPTH],
    latent_headings: [Option<String>; MAX_HEADING_DEPTH],
    current: String,
    current_role: Option<ContentRole>,
    output_text_bytes: usize,
}

enum Frame {
    Enter(NodeId),
    Exit {
        block: bool,
        restore_latent: Option<[Option<String>; MAX_HEADING_DEPTH]>,
    },
}

impl Projector<'_> {
    fn walk(&mut self, budget: &mut Budget, limits: &HtmlLimits) -> Result<(), HtmlError> {
        let mut frames = Vec::new();
        push_children_reverse(self.arena, self.arena.root(), &mut frames, budget, limits)?;

        while let Some(frame) = frames.pop() {
            budget.leave_project_traversal(1);
            match frame {
                Frame::Enter(node) => match &self.arena.get(node).kind {
                    NodeKind::Root => {}
                    NodeKind::Text(text) => {
                        self.append_text(&text.text, text.role, budget, limits)?;
                    }
                    NodeKind::Element(element) => {
                        let block = element.tag.is_block() || element.tag == Tag::Br;
                        if block {
                            self.flush(budget, limits)?;
                        }
                        let parent_role = self
                            .arena
                            .get(node)
                            .parent
                            .and_then(|parent| node_role(self.arena, parent))
                            .unwrap_or(ContentRole::Primary);
                        let restore_latent = if element.role == ContentRole::Latent
                            && parent_role == ContentRole::Primary
                        {
                            let local =
                                try_clone_heading_stack(&self.primary_headings, budget, limits)?;
                            Some(std::mem::replace(&mut self.latent_headings, local))
                        } else {
                            None
                        };

                        if let Tag::H(level) = element.tag {
                            self.flush(budget, limits)?;
                            let runs = collect_text_runs(self.arena, node, budget, limits)?;
                            let mut heading = String::new();
                            for (_, _, text) in runs
                                .iter()
                                .filter(|(role, synthetic, _)| *role == element.role && !synthetic)
                            {
                                append_normalized_heading(&mut heading, text, budget, limits)?;
                            }
                            if !heading.is_empty() {
                                self.set_heading(level, heading, element.role, budget);
                            }
                            for (role, _, text) in runs {
                                budget.leave_heading_runs(1);
                                budget.leave_heading_scratch(text.len());
                                self.push_content_section(text, role, budget, limits)?;
                            }
                            if let Some(saved) = restore_latent {
                                self.restore_latent(saved, budget);
                            }
                            continue;
                        }

                        push_exit_and_children_reverse(
                            self.arena,
                            node,
                            Frame::Exit {
                                block,
                                restore_latent,
                            },
                            &mut frames,
                            budget,
                            limits,
                        )?;
                    }
                },
                Frame::Exit {
                    block,
                    restore_latent,
                } => {
                    if block {
                        self.flush(budget, limits)?;
                    }
                    if let Some(saved) = restore_latent {
                        self.restore_latent(saved, budget);
                    }
                }
            }
        }
        Ok(())
    }

    fn restore_latent(&mut self, saved: [Option<String>; MAX_HEADING_DEPTH], budget: &mut Budget) {
        let local = std::mem::replace(&mut self.latent_headings, saved);
        budget.leave_heading_scratch(heading_stack_bytes(&local));
    }

    fn append_text(
        &mut self,
        text: &str,
        role: ContentRole,
        budget: &mut Budget,
        limits: &HtmlLimits,
    ) -> Result<(), HtmlError> {
        if self.current_role.is_some_and(|current| current != role) {
            self.flush(budget, limits)?;
        }
        self.current_role = Some(role);
        let ceiling = limits
            .retained_text_bytes
            .checked_sub(self.output_text_bytes)
            .ok_or_else(|| HtmlError::limit(HtmlStage::Project))?;
        append_normalized(&mut self.current, text, ceiling)
    }

    fn flush(&mut self, budget: &mut Budget, limits: &HtmlLimits) -> Result<(), HtmlError> {
        let content = std::mem::take(&mut self.current);
        let Some(role) = self.current_role.take() else {
            return Ok(());
        };
        if content.is_empty() {
            return Ok(());
        }
        self.push_content_section(content, role, budget, limits)
    }

    fn push_content_section(
        &mut self,
        content: String,
        role: ContentRole,
        budget: &mut Budget,
        limits: &HtmlLimits,
    ) -> Result<(), HtmlError> {
        if content.is_empty() {
            return Ok(());
        }
        let next_text = self
            .output_text_bytes
            .checked_add(content.len())
            .filter(|bytes| *bytes <= limits.retained_text_bytes)
            .ok_or_else(|| HtmlError::limit(HtmlStage::Project))?;
        let heading_stack = self.heading_stack(role);
        let heading_bytes = heading_stack
            .iter()
            .filter_map(Option::as_ref)
            .try_fold(0_usize, |total, heading| total.checked_add(heading.len()))
            .ok_or_else(|| HtmlError::limit(HtmlStage::Project))?;
        budget.output_section(limits, heading_bytes)?;
        let heading_count = heading_stack
            .iter()
            .filter(|heading| heading.is_some())
            .count();
        let mut heading_path = Vec::new();
        heading_path
            .try_reserve_exact(heading_count)
            .map_err(|_| HtmlError::limit(HtmlStage::Project))?;
        for heading in heading_stack.iter().filter_map(Option::as_ref) {
            heading_path.push(try_clone_string(heading)?);
        }
        if self.sections.len() == self.sections.capacity() {
            self.sections
                .try_reserve_exact(1)
                .map_err(|_| HtmlError::limit(HtmlStage::Project))?;
        }
        self.output_text_bytes = next_text;
        self.sections.push(ExtractedSection {
            heading_path,
            content,
            role,
            locator: None,
        });
        Ok(())
    }

    fn set_heading(&mut self, level: u8, heading: String, role: ContentRole, budget: &mut Budget) {
        let index = usize::from(level.saturating_sub(1)).min(MAX_HEADING_DEPTH - 1);
        let stack = if role.is_primary() {
            &mut self.primary_headings
        } else {
            &mut self.latent_headings
        };
        if let Some(previous) = stack[index].take() {
            budget.leave_heading_scratch(previous.len());
        }
        stack[index] = Some(heading);
        for slot in &mut stack[index + 1..] {
            if let Some(previous) = slot.take() {
                budget.leave_heading_scratch(previous.len());
            }
        }
    }

    fn heading_stack(&self, role: ContentRole) -> &[Option<String>; MAX_HEADING_DEPTH] {
        if role.is_primary() {
            &self.primary_headings
        } else if self.latent_headings.iter().any(Option::is_some) {
            &self.latent_headings
        } else {
            &self.primary_headings
        }
    }
}

fn count_children(arena: &Arena, node: NodeId) -> Result<usize, HtmlError> {
    let mut count = 0_usize;
    let mut child = arena.first_child(node);
    while let Some(id) = child {
        count = count
            .checked_add(1)
            .ok_or_else(|| HtmlError::limit(HtmlStage::Project))?;
        child = arena.next_sibling(id);
    }
    Ok(count)
}

fn push_children_reverse(
    arena: &Arena,
    node: NodeId,
    frames: &mut Vec<Frame>,
    budget: &mut Budget,
    limits: &HtmlLimits,
) -> Result<(), HtmlError> {
    let child_count = count_children(arena, node)?;
    budget.project_traversal(limits, child_count)?;
    frames
        .try_reserve_exact(child_count)
        .map_err(|_| HtmlError::limit(HtmlStage::Project))?;
    let mut child = arena.last_child(node);
    while let Some(id) = child {
        frames.push(Frame::Enter(id));
        child = arena.previous_sibling(id);
    }
    Ok(())
}

fn push_exit_and_children_reverse(
    arena: &Arena,
    node: NodeId,
    exit: Frame,
    frames: &mut Vec<Frame>,
    budget: &mut Budget,
    limits: &HtmlLimits,
) -> Result<(), HtmlError> {
    let child_count = count_children(arena, node)?;
    let additional = child_count
        .checked_add(1)
        .ok_or_else(|| HtmlError::limit(HtmlStage::Project))?;
    budget.project_traversal(limits, additional)?;
    frames
        .try_reserve_exact(additional)
        .map_err(|_| HtmlError::limit(HtmlStage::Project))?;
    frames.push(exit);
    let mut child = arena.last_child(node);
    while let Some(id) = child {
        frames.push(Frame::Enter(id));
        child = arena.previous_sibling(id);
    }
    Ok(())
}

fn collect_text_runs(
    arena: &Arena,
    root: NodeId,
    budget: &mut Budget,
    limits: &HtmlLimits,
) -> Result<Vec<(ContentRole, bool, String)>, HtmlError> {
    let mut output: Vec<(ContentRole, bool, String)> = Vec::new();
    let mut stack = Vec::new();
    push_node_children_reverse(arena, root, &mut stack, budget, limits)?;
    while let Some(node) = stack.pop() {
        budget.leave_project_traversal(1);
        match &arena.get(node).kind {
            NodeKind::Text(text) => {
                if let Some((role, synthetic, current)) = output.last_mut()
                    && *role == text.role
                    && *synthetic == text.synthetic
                {
                    append_normalized_heading(current, &text.text, budget, limits)?;
                    continue;
                }
                let mut current = String::new();
                append_normalized_heading(&mut current, &text.text, budget, limits)?;
                if !current.is_empty() {
                    budget.heading_run(limits)?;
                    output
                        .try_reserve_exact(1)
                        .map_err(|_| HtmlError::limit(HtmlStage::Project))?;
                    output.push((text.role, text.synthetic, current));
                }
            }
            NodeKind::Element(_) | NodeKind::Root => {
                push_node_children_reverse(arena, node, &mut stack, budget, limits)?;
            }
        }
    }
    Ok(output)
}

fn push_node_children_reverse(
    arena: &Arena,
    node: NodeId,
    stack: &mut Vec<NodeId>,
    budget: &mut Budget,
    limits: &HtmlLimits,
) -> Result<(), HtmlError> {
    let child_count = count_children(arena, node)?;
    budget.project_traversal(limits, child_count)?;
    stack
        .try_reserve_exact(child_count)
        .map_err(|_| HtmlError::limit(HtmlStage::Project))?;
    let mut child = arena.last_child(node);
    while let Some(id) = child {
        stack.push(id);
        child = arena.previous_sibling(id);
    }
    Ok(())
}

fn try_clone_heading_stack(
    stack: &[Option<String>; MAX_HEADING_DEPTH],
    budget: &mut Budget,
    limits: &HtmlLimits,
) -> Result<[Option<String>; MAX_HEADING_DEPTH], HtmlError> {
    let mut cloned = std::array::from_fn(|_| None);
    for (target, source) in cloned.iter_mut().zip(stack) {
        if let Some(source) = source {
            budget.heading_scratch(limits, source.len(), HtmlStage::Project)?;
            *target = Some(try_clone_string(source)?);
        }
    }
    Ok(cloned)
}

fn try_clone_string(source: &str) -> Result<String, HtmlError> {
    let mut output = String::new();
    output
        .try_reserve_exact(source.len())
        .map_err(|_| HtmlError::limit(HtmlStage::Project))?;
    output.push_str(source);
    Ok(output)
}

fn heading_stack_bytes(stack: &[Option<String>; MAX_HEADING_DEPTH]) -> usize {
    stack
        .iter()
        .filter_map(Option::as_ref)
        .map(String::len)
        .sum()
}

fn node_role(arena: &Arena, node: NodeId) -> Option<ContentRole> {
    match &arena.get(node).kind {
        NodeKind::Element(element) => Some(element.role),
        NodeKind::Text(text) => Some(text.role),
        NodeKind::Root => None,
    }
}

fn append_normalized(output: &mut String, input: &str, ceiling: usize) -> Result<(), HtmlError> {
    let mut pending_space = !output.is_empty() && !output.ends_with(' ');
    for character in input.chars() {
        if character.is_whitespace() {
            pending_space = !output.is_empty();
            continue;
        }
        let extra = character.len_utf8() + usize::from(pending_space);
        let new_len = output
            .len()
            .checked_add(extra)
            .filter(|length| *length <= ceiling)
            .ok_or_else(|| HtmlError::limit(HtmlStage::Project))?;
        if new_len > output.capacity() {
            output
                .try_reserve_exact(new_len - output.capacity())
                .map_err(|_| HtmlError::limit(HtmlStage::Project))?;
        }
        if pending_space {
            output.push(' ');
        }
        output.push(character);
        pending_space = false;
    }
    Ok(())
}

fn append_normalized_heading(
    output: &mut String,
    input: &str,
    budget: &mut Budget,
    limits: &HtmlLimits,
) -> Result<(), HtmlError> {
    let mut pending_space = !output.is_empty() && !output.ends_with(' ');
    for character in input.chars() {
        if character.is_whitespace() {
            pending_space = !output.is_empty();
            continue;
        }
        let extra = character.len_utf8() + usize::from(pending_space);
        let new_len = output
            .len()
            .checked_add(extra)
            .filter(|length| *length <= limits.heading_component_bytes)
            .ok_or_else(|| HtmlError::limit(HtmlStage::Project))?;
        budget.heading_scratch(limits, extra, HtmlStage::Project)?;
        if new_len > output.capacity() {
            output
                .try_reserve_exact(new_len - output.capacity())
                .map_err(|_| HtmlError::limit(HtmlStage::Project))?;
        }
        if pending_space {
            output.push(' ');
        }
        output.push(character);
        pending_space = false;
    }
    Ok(())
}
