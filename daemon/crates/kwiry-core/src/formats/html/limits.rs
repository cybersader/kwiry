// SPDX-License-Identifier: MIT OR Apache-2.0

use crate::model::MAX_FILE_BYTES;

use super::error::{HtmlError, HtmlStage};

pub(super) const OMITTED_SCAN_BUFFER_BYTES: usize = 8_192;
pub(super) const MAX_TOKENIZER_STEPS: usize = 67_108_864;
pub(super) const MAX_TOKENS: usize = 2_000_000;
pub(super) const MAX_TEXT_TOKEN_BYTES: usize = 65_536;
pub(super) const MAX_CHARACTER_REFERENCES: usize = 1_000_000;
pub(super) const MAX_CHARACTER_REFERENCE_STEPS: usize = 8_000_000;
pub(super) const MAX_CHARACTER_REFERENCE_SCRATCH: usize = 64;
pub(super) const MAX_NAME_BYTES: usize = 1_024;
pub(super) const MAX_OPEN_ELEMENTS: usize = 256;
pub(super) const MAX_ACTIVE_FORMATTING: usize = 256;
pub(super) const MAX_TEMPLATE_MODES: usize = 64;
pub(super) const MAX_NODES: usize = 100_000;
pub(super) const MAX_TREE_MUTATIONS: usize = 2_000_000;
pub(super) const MAX_ATTRIBUTES_PER_ELEMENT: usize = 256;
pub(super) const MAX_ATTRIBUTES_TOTAL: usize = 1_000_000;
pub(super) const MAX_ATTRIBUTE_BYTES_PER_ELEMENT: usize = 65_536;
pub(super) const MAX_ATTRIBUTE_BYTES_TOTAL: usize = 8_388_608;
pub(super) const MAX_PARSE_ERRORS: usize = 4_096;
pub(super) const MAX_NOTICES: usize = 32;
pub(super) const MAX_TABLE_CONTEXTS: usize = 64;
pub(super) const MAX_TABLE_CELLS: usize = 100_000;
pub(super) const MAX_PENDING_TABLE_RUNS: usize = 4_096;
pub(super) const MAX_PENDING_TABLE_BYTES: usize = 65_536;
pub(super) const MAX_RETAINED_TEXT_BYTES: usize = MAX_FILE_BYTES as usize;
pub(super) const MAX_LOGICAL_HEADING_BYTES: usize = 1_048_576;
pub(super) const MAX_HEADING_COMPONENT_BYTES: usize = 1_024;
pub(super) const MAX_OUTPUT_SECTIONS: usize = 100_000;
pub(super) const MAX_HEADING_DEPTH: usize = 6;
pub(super) const MAX_REPEATED_HEADING_BYTES: usize = MAX_FILE_BYTES as usize;
pub(super) const MAX_RECOVERY_SCRATCH_ENTRIES: usize = MAX_TREE_MUTATIONS;
pub(super) const MAX_PROJECT_TRAVERSAL_ENTRIES: usize = MAX_TREE_MUTATIONS;
pub(super) const MAX_HEADING_RUN_ENTRIES: usize = MAX_NODES;
pub(super) const MAX_HEADING_SCRATCH_BYTES: usize = 4 * MAX_FILE_BYTES as usize;

#[derive(Debug, Clone)]
pub(super) struct HtmlLimits {
    pub source_bytes: usize,
    pub decoded_bytes: usize,
    pub tokenizer_steps: usize,
    pub tokens: usize,
    pub text_token_bytes: usize,
    pub character_references: usize,
    pub character_reference_steps: usize,
    pub character_reference_scratch: usize,
    pub name_bytes: usize,
    pub open_elements: usize,
    pub active_formatting: usize,
    pub template_modes: usize,
    pub nodes: usize,
    pub tree_mutations: usize,
    pub attributes_per_element: usize,
    pub attributes_total: usize,
    pub attribute_bytes_per_element: usize,
    pub attribute_bytes_total: usize,
    pub parse_errors: usize,
    pub notices: usize,
    pub table_contexts: usize,
    pub table_cells: usize,
    pub pending_table_runs: usize,
    pub pending_table_bytes: usize,
    pub retained_text_bytes: usize,
    pub logical_heading_bytes: usize,
    pub heading_component_bytes: usize,
    pub output_sections: usize,
    pub repeated_heading_bytes: usize,
    pub recovery_scratch_entries: usize,
    pub project_traversal_entries: usize,
    pub heading_run_entries: usize,
    pub heading_scratch_bytes: usize,
}

impl Default for HtmlLimits {
    fn default() -> Self {
        Self {
            source_bytes: MAX_FILE_BYTES as usize,
            decoded_bytes: MAX_FILE_BYTES as usize,
            tokenizer_steps: MAX_TOKENIZER_STEPS,
            tokens: MAX_TOKENS,
            text_token_bytes: MAX_TEXT_TOKEN_BYTES,
            character_references: MAX_CHARACTER_REFERENCES,
            character_reference_steps: MAX_CHARACTER_REFERENCE_STEPS,
            character_reference_scratch: MAX_CHARACTER_REFERENCE_SCRATCH,
            name_bytes: MAX_NAME_BYTES,
            open_elements: MAX_OPEN_ELEMENTS,
            active_formatting: MAX_ACTIVE_FORMATTING,
            template_modes: MAX_TEMPLATE_MODES,
            nodes: MAX_NODES,
            tree_mutations: MAX_TREE_MUTATIONS,
            attributes_per_element: MAX_ATTRIBUTES_PER_ELEMENT,
            attributes_total: MAX_ATTRIBUTES_TOTAL,
            attribute_bytes_per_element: MAX_ATTRIBUTE_BYTES_PER_ELEMENT,
            attribute_bytes_total: MAX_ATTRIBUTE_BYTES_TOTAL,
            parse_errors: MAX_PARSE_ERRORS,
            notices: MAX_NOTICES,
            table_contexts: MAX_TABLE_CONTEXTS,
            table_cells: MAX_TABLE_CELLS,
            pending_table_runs: MAX_PENDING_TABLE_RUNS,
            pending_table_bytes: MAX_PENDING_TABLE_BYTES,
            retained_text_bytes: MAX_RETAINED_TEXT_BYTES,
            logical_heading_bytes: MAX_LOGICAL_HEADING_BYTES,
            heading_component_bytes: MAX_HEADING_COMPONENT_BYTES,
            output_sections: MAX_OUTPUT_SECTIONS,
            repeated_heading_bytes: MAX_REPEATED_HEADING_BYTES,
            recovery_scratch_entries: MAX_RECOVERY_SCRATCH_ENTRIES,
            project_traversal_entries: MAX_PROJECT_TRAVERSAL_ENTRIES,
            heading_run_entries: MAX_HEADING_RUN_ENTRIES,
            heading_scratch_bytes: MAX_HEADING_SCRATCH_BYTES,
        }
    }
}

#[derive(Debug, Default)]
pub(super) struct Budget {
    pub tokenizer_steps: usize,
    pub tokens: usize,
    pub character_references: usize,
    pub character_reference_steps: usize,
    pub nodes: usize,
    pub tree_mutations: usize,
    pub attributes_total: usize,
    pub attribute_bytes_total: usize,
    pub parse_errors: usize,
    pub notices: usize,
    pub table_contexts: usize,
    pub table_cells: usize,
    pub pending_table_runs: usize,
    pub pending_table_bytes: usize,
    pub retained_text_bytes: usize,
    pub output_sections: usize,
    pub repeated_heading_bytes: usize,
    pub recovery_scratch_entries: usize,
    pub project_traversal_entries: usize,
    pub heading_run_entries: usize,
    pub heading_scratch_bytes: usize,
}

impl Budget {
    fn charge(
        value: &mut usize,
        amount: usize,
        ceiling: usize,
        stage: HtmlStage,
    ) -> Result<(), HtmlError> {
        *value = value
            .checked_add(amount)
            .filter(|value| *value <= ceiling)
            .ok_or_else(|| HtmlError::limit(stage))?;
        Ok(())
    }

    pub fn tokenizer_step(&mut self, limits: &HtmlLimits, amount: usize) -> Result<(), HtmlError> {
        Self::charge(
            &mut self.tokenizer_steps,
            amount,
            limits.tokenizer_steps,
            HtmlStage::Tokenize,
        )
    }

    pub fn token(&mut self, limits: &HtmlLimits) -> Result<(), HtmlError> {
        Self::charge(&mut self.tokens, 1, limits.tokens, HtmlStage::Tokenize)
    }

    pub fn character_reference(&mut self, limits: &HtmlLimits) -> Result<(), HtmlError> {
        Self::charge(
            &mut self.character_references,
            1,
            limits.character_references,
            HtmlStage::Tokenize,
        )
    }

    pub fn character_reference_steps(
        &mut self,
        limits: &HtmlLimits,
        amount: usize,
    ) -> Result<(), HtmlError> {
        Self::charge(
            &mut self.character_reference_steps,
            amount,
            limits.character_reference_steps,
            HtmlStage::Tokenize,
        )
    }

    pub fn node(&mut self, limits: &HtmlLimits) -> Result<(), HtmlError> {
        Self::charge(&mut self.nodes, 1, limits.nodes, HtmlStage::Recover)
    }

    pub fn mutation(&mut self, limits: &HtmlLimits, amount: usize) -> Result<(), HtmlError> {
        Self::charge(
            &mut self.tree_mutations,
            amount,
            limits.tree_mutations,
            HtmlStage::Recover,
        )
    }

    pub fn attributes(
        &mut self,
        limits: &HtmlLimits,
        count: usize,
        bytes: usize,
    ) -> Result<(), HtmlError> {
        Self::charge(
            &mut self.attributes_total,
            count,
            limits.attributes_total,
            HtmlStage::Tokenize,
        )?;
        Self::charge(
            &mut self.attribute_bytes_total,
            bytes,
            limits.attribute_bytes_total,
            HtmlStage::Tokenize,
        )
    }

    pub fn parse_error(&mut self, limits: &HtmlLimits) -> Result<(), HtmlError> {
        Self::charge(
            &mut self.parse_errors,
            1,
            limits.parse_errors,
            HtmlStage::Tokenize,
        )
    }

    pub fn notice(&mut self, limits: &HtmlLimits) -> Result<(), HtmlError> {
        Self::charge(&mut self.notices, 1, limits.notices, HtmlStage::Project)
    }

    pub fn table_context(&mut self, limits: &HtmlLimits) -> Result<(), HtmlError> {
        Self::charge(
            &mut self.table_contexts,
            1,
            limits.table_contexts,
            HtmlStage::Recover,
        )
    }

    pub fn leave_table_context(&mut self) {
        self.table_contexts = self.table_contexts.saturating_sub(1);
    }

    pub fn table_cell(&mut self, limits: &HtmlLimits) -> Result<(), HtmlError> {
        Self::charge(
            &mut self.table_cells,
            1,
            limits.table_cells,
            HtmlStage::Recover,
        )
    }

    pub fn pending_table_text(
        &mut self,
        limits: &HtmlLimits,
        bytes: usize,
    ) -> Result<(), HtmlError> {
        Self::charge(
            &mut self.pending_table_runs,
            1,
            limits.pending_table_runs,
            HtmlStage::Recover,
        )?;
        Self::charge(
            &mut self.pending_table_bytes,
            bytes,
            limits.pending_table_bytes,
            HtmlStage::Recover,
        )
    }

    pub fn clear_pending_table_text(&mut self) {
        self.pending_table_runs = 0;
        self.pending_table_bytes = 0;
    }

    pub fn retained_text(&mut self, limits: &HtmlLimits, bytes: usize) -> Result<(), HtmlError> {
        Self::charge(
            &mut self.retained_text_bytes,
            bytes,
            limits.retained_text_bytes,
            HtmlStage::Recover,
        )
    }

    pub fn output_section(
        &mut self,
        limits: &HtmlLimits,
        heading_bytes: usize,
    ) -> Result<(), HtmlError> {
        Self::charge(
            &mut self.output_sections,
            1,
            limits.output_sections,
            HtmlStage::Project,
        )?;
        Self::charge(
            &mut self.repeated_heading_bytes,
            heading_bytes,
            limits.repeated_heading_bytes,
            HtmlStage::Project,
        )
    }

    pub fn recovery_scratch_entry(&mut self, limits: &HtmlLimits) -> Result<(), HtmlError> {
        Self::charge(
            &mut self.recovery_scratch_entries,
            1,
            limits.recovery_scratch_entries,
            HtmlStage::Recover,
        )
    }

    pub fn leave_recovery_scratch(&mut self, amount: usize) {
        self.recovery_scratch_entries = self.recovery_scratch_entries.saturating_sub(amount);
    }

    pub fn project_traversal(
        &mut self,
        limits: &HtmlLimits,
        amount: usize,
    ) -> Result<(), HtmlError> {
        Self::charge(
            &mut self.project_traversal_entries,
            amount,
            limits.project_traversal_entries,
            HtmlStage::Project,
        )
    }

    pub fn leave_project_traversal(&mut self, amount: usize) {
        self.project_traversal_entries = self.project_traversal_entries.saturating_sub(amount);
    }

    pub fn heading_run(&mut self, limits: &HtmlLimits) -> Result<(), HtmlError> {
        Self::charge(
            &mut self.heading_run_entries,
            1,
            limits.heading_run_entries,
            HtmlStage::Project,
        )
    }

    pub fn leave_heading_runs(&mut self, amount: usize) {
        self.heading_run_entries = self.heading_run_entries.saturating_sub(amount);
    }

    pub fn heading_scratch(
        &mut self,
        limits: &HtmlLimits,
        bytes: usize,
        stage: HtmlStage,
    ) -> Result<(), HtmlError> {
        Self::charge(
            &mut self.heading_scratch_bytes,
            bytes,
            limits.heading_scratch_bytes,
            stage,
        )
    }

    pub fn leave_heading_scratch(&mut self, bytes: usize) {
        self.heading_scratch_bytes = self.heading_scratch_bytes.saturating_sub(bytes);
    }
}
