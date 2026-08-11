// SPDX-License-Identifier: MIT OR Apache-2.0

pub(crate) const MAX_CENTRAL_DIRECTORY_ENTRIES: usize = 4_096;
pub(crate) const MAX_CANONICAL_PART_URI_BYTES: usize = 1_024;
pub(crate) const MAX_RELATIONSHIP_TARGET_BYTES: usize = 2_048;
pub(crate) const MAX_ZIP_METADATA_BYTES: usize = 2 * 1024 * 1024;
pub(crate) const MAX_SELECTED_XML_PARTS: usize = 256;
pub(crate) const MAX_RELATIONSHIPS_PER_PART: usize = 4_096;
pub(crate) const MAX_RELATIONSHIPS_TOTAL: usize = 16_384;
pub(crate) const MAX_DECLARED_ENTRY_BYTES: u64 = 512 * 1024 * 1024;
pub(crate) const MAX_DECLARED_PACKAGE_BYTES: u64 = 2 * 1024 * 1024 * 1024;
pub(crate) const MAX_SELECTED_XML_PART_BYTES: u64 = 16 * 1024 * 1024;
pub(crate) const MAX_SELECTED_XML_TOTAL_BYTES: u64 = 32 * 1024 * 1024;
pub(crate) const MAX_EXTRACTED_TEXT_BYTES: usize = 10 * 1024 * 1024;
pub(crate) const MAX_EXPANSION_RATIO: u64 = 200;
pub(crate) const EXPANSION_RATIO_ALLOWANCE_BYTES: u64 = 1024 * 1024;
pub(crate) const MAX_XML_DEPTH: usize = 256;
pub(crate) const MAX_XML_EVENTS: usize = 2_000_000;
pub(crate) const MAX_ATTRIBUTES_PER_ELEMENT: usize = 256;
pub(crate) const MAX_XML_ATTRIBUTES_TOTAL: usize = 1_000_000;
pub(crate) const MAX_ATTRIBUTE_BYTES_PER_ELEMENT: usize = 64 * 1024;
pub(crate) const MAX_ATTRIBUTE_BYTES_TOTAL: usize = 8 * 1024 * 1024;
pub(crate) const MAX_NAMESPACE_DECLARATIONS_PER_ELEMENT: usize = 64;
pub(crate) const MAX_NAMESPACE_DECLARATIONS_TOTAL: usize = 65_536;
pub(crate) const MAX_QNAME_BYTES: usize = 1_024;
pub(crate) const MAX_SINGLE_TEXT_EVENT_BYTES: usize = 1024 * 1024;
pub(crate) const MAX_XML_TEXT_BYTES: usize = 10 * 1024 * 1024;
/// `w:outlineLvl` is defined over 0-8 for heading levels; 9 means body text.
/// Clamping to the schema range bounds heading-stack depth, and with it the
/// heading path cloned into every emitted section.
pub(crate) const MAX_OUTLINE_LEVEL: usize = 8;
pub(crate) const MAX_PROPERTY_ENTRIES: usize = 4_096;
pub(crate) const MAX_PROPERTY_BYTES: usize = 1024 * 1024;
pub(crate) const MAX_NOTICES: usize = 32;
pub(crate) const DECOMPRESSION_BUFFER_BYTES: usize = 8 * 1024;
