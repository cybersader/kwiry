// SPDX-License-Identifier: MIT OR Apache-2.0

//! Shared bounded OOXML package primitives.
//!
//! The hardened implementations originated in the DOCX extractor. Keeping this
//! narrow facade lets SpreadsheetML reuse the exact ZIP inventory, XML pull
//! parser, limits, URI canonicalization, and generated ZIP fixtures without
//! making the Word package walker a dependency of the Excel walker.

pub(crate) use super::docx::error::DocxError as PackageError;
pub(crate) use super::docx::limits;
pub(crate) use super::docx::opc::{relationships_part_uri, resolve_relationship_target};
pub(crate) use super::docx::xml::{XmlBudget, XmlElement, XmlEvent, parse_xml, parse_xml_events};
pub(crate) use super::docx::zip::ArchiveInventory;

#[cfg(test)]
pub(crate) use super::docx::test_support::{Method, TestEntry, build_zip};
