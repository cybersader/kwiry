// SPDX-License-Identifier: MIT OR Apache-2.0

//! Excalidraw (`.excalidraw` JSON) extraction spike.
//!
//! # Admission status
//!
//! `excalidraw` is **not** a member of the closed source-format set frozen by
//! `CONTRACT.md` §10.1 (`markdown`, `text`, `base`, `canvas`, `docx`, `pdf`).
//! Admitting it requires an owner amendment that has not been granted, so this
//! module deliberately stops short of admission:
//!
//! * there is no `SourceFormat::Excalidraw` variant,
//! * `extract_source` has no Excalidraw arm,
//! * `SourceFormat::from_path("board.excalidraw")` still returns `None`, so
//!   discovery, `prepare_source_buffer`, the manifest, and every client mirror
//!   are untouched,
//! * `SOURCE_PREPARATION_SCHEMA_VERSION` is unchanged, because no persisted
//!   preparation can contain Excalidraw output.
//!
//! The extractor is reachable only through [`extract_excalidraw_candidate`],
//! which is compiled behind the `internal-excalidraw-extractor` feature — the
//! same admission-disabled seam the DOCX extractor used before its
//! owner-approved admission.
//!
//! # Authored schema
//!
//! A bare `.excalidraw` document is a JSON object with `type`, `version`,
//! `source`, `elements`, `appState`, and `files`. Authored text lives in three
//! places and nowhere else:
//!
//! * `text` elements carry `originalText` (the authored string) and `text` (the
//!   same string after the layout engine inserted wrap breaks). `originalText`
//!   wins so re-wrapping is identity-neutral. `originalText` postdates `text` in
//!   the element schema, so a pre-`originalText` element falls back to the
//!   layout-wrapped `text`; that fallback is **not** identity-neutral, because a
//!   container resize rewrites the wrap breaks. Losing the words entirely would
//!   be worse than re-indexing them, so the fallback stands and its limit is
//!   pinned by test rather than left implicit.
//! * `frame` / `magicframe` elements carry `name`.
//! * any element may carry `link`. A `link` is indexed even when the element's
//!   own semantics are unusable — an unsupported element type or a non-string
//!   frame name — because Excalidraw's element type union is not frozen and a
//!   type introduced by a later release must not silently strand an authored
//!   URL in the retained projection where lexical search cannot reach it.
//!
//! Shape labels are not inline: a labelled rectangle/ellipse/diamond/arrow is
//! two elements, the container holding `boundElements` and a separate `text`
//! element holding `containerId`. The label is emitted at the bound `text`
//! element's own array position, never folded into its container, so emission
//! order depends on array position alone and never on reference topology.
//!
//! Excalidraw text is plain text, not Markdown: an element reading `# Alpha` is
//! a literal hash on the canvas. Sections therefore always carry an empty
//! `heading_path` and no locator, and the heading-byte ledger is never charged.

use std::collections::{BTreeMap, BTreeSet};

use crate::extract::{
    ContentRole, ExtractedSection, ExtractedSource, ExtractionBudget, ExtractionCompleteness,
    ExtractionCoverage, ExtractionError, ExtractionNotice,
};
use crate::model::{Frontmatter, PropertyBag, PropertyValue};

use super::decode_utf8;

/// Notice cap, mirroring the Canvas extractor. This bounds the total notice
/// count, per-element and terminal notices together.
pub const MAX_EXCALIDRAW_NOTICES: usize = 32;
/// Maximum map keys plus sequence elements retained in the property projection.
pub const MAX_EXCALIDRAW_PROPERTY_ENTRIES: usize = 4_096;
/// Maximum key and scalar bytes retained in the property projection.
pub const MAX_EXCALIDRAW_PROPERTY_BYTES: usize = 1024 * 1024;

const EXCALIDRAW_DOCUMENT_TYPE: &str = "excalidraw";

/// Root keys retained verbatim in the typed projection.
const PROJECTED_ROOT_KEYS: &[&str] = &["appState", "source", "type", "version"];
/// Per-element structural keys retained in the typed projection.
const PROJECTED_ELEMENT_KEYS: &[&str] = &[
    "boundElements",
    "containerId",
    "frameId",
    "groupIds",
    "id",
    "isDeleted",
    "type",
];
/// Per-element authored keys retained in the projection for live elements only.
///
/// `customData` belongs here, not in [`PROJECTED_ELEMENT_KEYS`]: it is arbitrary
/// extension-owned JSON that routinely carries authored strings, so retaining it
/// for a soft-deleted element would leak deleted authored content into the
/// property bag.
const PROJECTED_AUTHORED_ELEMENT_KEYS: &[&str] = &["customData", "link", "name"];
/// Non-payload scalars retained from each `files` entry. `dataURL` is dropped.
const PROJECTED_FILE_DIGEST_KEYS: &[&str] = &["created", "id", "lastRetrieved", "mimeType"];

/// Extracts a bare `.excalidraw` JSON document.
///
/// This is a spike entry point, not an admitted format handler: nothing in the
/// registry, discovery, source-preparation, or client layers routes to it. See
/// the module documentation for the admission gate.
pub fn extract_excalidraw_candidate(bytes: &[u8]) -> Result<ExtractedSource, ExtractionError> {
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
            return Ok(quarantined(
                "excalidraw_root_not_object",
                "Excalidraw root must be a JSON object; quarantined as an invalid source preparation",
            ));
        }
        Err(_) => {
            return Ok(quarantined(
                "invalid_excalidraw_json",
                "invalid Excalidraw JSON; quarantined as an invalid source preparation",
            ));
        }
    };
    // A `type` key that is present but is not exactly the string "excalidraw"
    // is rejected whatever its JSON type. Gating on "present and a non-empty
    // string" would admit a mangled library file whose discriminator was
    // corrupted to a number, null, array, or blank string.
    match root.get("type") {
        None => {}
        Some(PropertyValue::String(declared)) if declared == EXCALIDRAW_DOCUMENT_TYPE => {}
        Some(_) => {
            return Ok(quarantined(
                "excalidraw_unexpected_document_type",
                "Excalidraw document type is not \"excalidraw\"; quarantined as an invalid source preparation",
            ));
        }
    }
    let elements = match root.get("elements") {
        None => &[][..],
        Some(PropertyValue::Sequence(elements)) => elements.as_slice(),
        Some(_) => {
            return Ok(quarantined(
                "excalidraw_elements_not_array",
                "Excalidraw elements must be a JSON array; quarantined as an invalid source preparation",
            ));
        }
    };

    let mut extractor = Extractor::default();
    for (index, element) in elements.iter().enumerate() {
        extractor.extract_element(index, element)?;
    }

    if extractor.sections.is_empty() {
        if extractor.notices.has_defects() {
            return Ok(skipped_with_notices(
                ExtractionCoverage::Quarantined,
                extractor.notices.into_notices(),
            ));
        }
        return Ok(ExtractedSource::skipped(
            ExtractionCoverage::SkippedNoExtractableText,
            ExtractionNotice::new(
                "excalidraw_no_extractable_text",
                "Excalidraw drawing contains no authored text, frame names, or links; skipped with no extractable text",
            ),
        ));
    }

    let Extractor {
        sections,
        mut notices,
        projected_elements,
        mut projection_budget,
        ..
    } = extractor;

    // The per-element half of the projection was charged against the budget as
    // it was built, so an over-budget drawing has already released its element
    // projections. Only the root half remains to be charged, and every root
    // value is cloned only once the charge has been accepted.
    let mut projection = BTreeMap::new();
    for key in PROJECTED_ROOT_KEYS {
        if let Some(value) = root.get(*key)
            && projection_budget.charge_entry_value(key, value)
        {
            projection.insert((*key).to_owned(), value.clone());
        }
    }
    let mut files_summarized = false;
    if let Some(PropertyValue::Map(files)) = root.get("files")
        && !files.is_empty()
    {
        files_summarized = true;
        let digest = PropertyValue::Map(files_digest(files));
        if projection_budget.charge_entry_value("files", &digest) {
            projection.insert("files".to_owned(), digest);
        }
    }
    // The sequence slots and element maps were already charged; only the
    // `elements` key itself is still outstanding.
    projection_budget.charge_entry();
    projection_budget.charge_bytes("elements".len());
    if !projection_budget.exceeded {
        projection.insert(
            "elements".to_owned(),
            PropertyValue::Sequence(projected_elements),
        );
    }

    let mut properties = BTreeMap::new();
    if projection_budget.exceeded {
        // Terminal notices are never evicted by per-element notices, so this
        // declaration survives even on a drawing that filled the notice buffer.
        notices.push_terminal_defect(
            "excalidraw_properties_not_retained",
            "Excalidraw property projection exceeded the retained-property budget and was dropped; extracted text is unaffected",
        );
    } else {
        properties.insert("excalidraw".to_owned(), PropertyValue::Map(projection));
        if files_summarized {
            // Only claim the summary once the projection carrying it is
            // actually retained; otherwise the notice would assert a retention
            // that the very next notice contradicts.
            notices.push_terminal_policy(
                "excalidraw_image_payloads_not_retained",
                "Excalidraw embedded image payloads were summarized to their non-payload scalars; dataURL contents are never retained",
            );
        }
    }

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

#[derive(Debug, Default)]
struct Extractor {
    sections: Vec<ExtractedSection>,
    budget: ExtractionBudget,
    notices: NoticeCollector,
    authored_ids: BTreeSet<String>,
    projected_elements: Vec<PropertyValue>,
    projection_budget: PropertyBudget,
}

impl Extractor {
    fn extract_element(
        &mut self,
        index: usize,
        value: &PropertyValue,
    ) -> Result<(), ExtractionError> {
        let PropertyValue::Map(element) = value else {
            self.notices.push(
                "excalidraw_element_not_object",
                format!(
                    "Excalidraw element at position {index} is not an object and was not extracted"
                ),
            );
            return Ok(());
        };
        let Some(id) = non_empty_string(element.get("id")) else {
            self.notices.push(
                "excalidraw_element_missing_id",
                format!(
                    "Excalidraw element at position {index} has no non-empty string ID and was not extracted"
                ),
            );
            return Ok(());
        };
        let Some(kind) = non_empty_string(element.get("type")) else {
            self.notices.push(
                "excalidraw_element_missing_type",
                format!(
                    "Excalidraw element at position {index} has no supported string type and was not extracted"
                ),
            );
            return Ok(());
        };

        // Soft deletion is a normal undo-buffer state, not an extraction
        // defect. Deleted elements are skipped silently and no authored string
        // of theirs reaches sections or the property projection.
        //
        // A tombstone is resolved *before* the duplicate-ID check and never
        // claims its ID: an undo-buffer record that happens to share an ID with
        // a live element must not evict the live element, which would drop
        // authored text and — when it is the only section — escalate the whole
        // drawing to quarantined.
        let deleted = match element.get("isDeleted") {
            None | Some(PropertyValue::Bool(false)) => false,
            Some(PropertyValue::Bool(true)) => true,
            Some(_) => {
                self.project_element(element, true);
                self.notices.push(
                    "excalidraw_element_invalid_deleted_flag",
                    format!(
                        "Excalidraw element at position {index} has a non-boolean isDeleted flag and was not extracted"
                    ),
                );
                return Ok(());
            }
        };
        if deleted {
            self.project_element(element, true);
            return Ok(());
        }
        if !self.authored_ids.insert(id.to_owned()) {
            self.notices.push(
                "excalidraw_duplicate_id",
                format!(
                    "Excalidraw element at position {index} has a duplicate ID and was not extracted"
                ),
            );
            return Ok(());
        }
        self.project_element(element, false);

        match kind {
            "text" => {
                let original = string(element.get("originalText"));
                let wrapped = string(element.get("text"));
                match original
                    .filter(|value| !value.trim().is_empty())
                    .or(wrapped)
                {
                    Some(content) if !content.trim().is_empty() => {
                        self.push_section(content.to_owned())?;
                    }
                    // Resolved but blank, or `originalText` is a blank string
                    // and `text` is absent: authored-but-empty, so there is
                    // nothing to index and no defect to report.
                    Some(_) => {}
                    None if original.is_some() => {}
                    None => {
                        self.notices.push(
                            "excalidraw_text_element_missing_text",
                            format!(
                                "Excalidraw text element at position {index} has no string text and was not extracted"
                            ),
                        );
                    }
                }
            }
            "frame" | "magicframe" => match element.get("name") {
                None => {}
                Some(PropertyValue::String(name)) if name.trim().is_empty() => {}
                Some(PropertyValue::String(name)) => {
                    self.push_section(name.clone())?;
                }
                Some(_) => {
                    self.notices.push(
                        "excalidraw_frame_invalid_name",
                        format!(
                            "Excalidraw frame at position {index} has a non-string name and was not extracted"
                        ),
                    );
                }
            },
            "rectangle" | "diamond" | "ellipse" | "line" | "arrow" | "freedraw" | "image"
            | "embeddable" | "iframe" | "selection" => {}
            _ => {
                self.notices.push(
                    "excalidraw_element_unsupported_type",
                    format!(
                        "Excalidraw element at position {index} has an unsupported type and was not extracted"
                    ),
                );
            }
        }

        // Every path falls through: an element whose own semantics are unusable
        // still contributes its authored `link`, so no URL is retained in the
        // projection while being unreachable from lexical search.
        self.extract_link(index, element)
    }

    fn extract_link(
        &mut self,
        index: usize,
        element: &BTreeMap<String, PropertyValue>,
    ) -> Result<(), ExtractionError> {
        match element.get("link") {
            None | Some(PropertyValue::Null) => {}
            Some(PropertyValue::String(link)) if link.trim().is_empty() => {}
            Some(PropertyValue::String(link)) => {
                self.push_section(link.clone())?;
            }
            Some(_) => {
                self.notices.push(
                    "excalidraw_element_invalid_link",
                    format!(
                        "Excalidraw element at position {index} has a non-string link that was not extracted"
                    ),
                );
            }
        }
        Ok(())
    }

    /// Retains a bounded typed projection of one element. Geometry, style,
    /// `seed`, `version`, `versionNonce`, `index`, `updated`, `points`,
    /// `fileId`, `scale`, and `crop` are never retained: the drawing JSON is
    /// large and machine-generated, and `files[*].dataURL` is a base64 image
    /// payload that must not cross the source ABI.
    ///
    /// The projection budget is charged **before** anything is cloned, and the
    /// already-retained projections are released the moment it is exceeded.
    /// Measuring only the finished projection would let a drawing at the
    /// `MAX_FILE_BYTES` ceiling allocate tens of times its own size before the
    /// budget it was declared to obey could reject it — and §2.7's stated
    /// motivation is exactly that a large drawing is mostly points, seeds, and
    /// nonces.
    fn project_element(&mut self, element: &BTreeMap<String, PropertyValue>, deleted: bool) {
        if self.projection_budget.exceeded {
            return;
        }
        // One sequence slot for this element, then its retained keys.
        self.projection_budget.charge_entry();
        charge_projected_keys(&mut self.projection_budget, element, PROJECTED_ELEMENT_KEYS);
        if !deleted {
            charge_projected_keys(
                &mut self.projection_budget,
                element,
                PROJECTED_AUTHORED_ELEMENT_KEYS,
            );
        }
        if self.projection_budget.exceeded {
            // The projection is dropped whole, so release what was retained
            // instead of carrying it to the end of the document.
            self.projected_elements = Vec::new();
            return;
        }

        let mut projected = BTreeMap::new();
        for key in PROJECTED_ELEMENT_KEYS {
            if let Some(value) = element.get(*key) {
                projected.insert((*key).to_owned(), value.clone());
            }
        }
        if !deleted {
            for key in PROJECTED_AUTHORED_ELEMENT_KEYS {
                if let Some(value) = element.get(*key) {
                    projected.insert((*key).to_owned(), value.clone());
                }
            }
        }
        self.projected_elements.push(PropertyValue::Map(projected));
    }

    fn push_section(&mut self, content: String) -> Result<(), ExtractionError> {
        // Excalidraw text is plain, so the heading path is always empty and the
        // heading-byte ledger is never charged.
        self.budget.reserve_section(&[])?;
        self.sections.push(ExtractedSection {
            heading_path: Vec::new(),
            content,
            role: ContentRole::Primary,
            locator: None,
        });
        Ok(())
    }
}

fn files_digest(files: &BTreeMap<String, PropertyValue>) -> BTreeMap<String, PropertyValue> {
    let mut digest = BTreeMap::new();
    for (file_id, entry) in files {
        let mut scalars = BTreeMap::new();
        if let PropertyValue::Map(entry) = entry {
            for key in PROJECTED_FILE_DIGEST_KEYS {
                if let Some(value) = entry.get(*key) {
                    scalars.insert((*key).to_owned(), value.clone());
                }
            }
        }
        digest.insert(file_id.clone(), PropertyValue::Map(scalars));
    }
    digest
}

/// Charges the retained subset of one element's keys without cloning them.
fn charge_projected_keys(
    budget: &mut PropertyBudget,
    element: &BTreeMap<String, PropertyValue>,
    keys: &[&str],
) {
    for key in keys {
        if budget.exceeded {
            return;
        }
        if let Some(value) = element.get(*key) {
            budget.charge_entry_value(key, value);
        }
    }
}

/// Running cost of the retained property projection.
///
/// The counters are charged as the projection is built, so exceeding the budget
/// stops the work instead of merely rejecting its result. Recursion is bounded
/// by `MAX_PROPERTY_NESTING_DEPTH`, which the deserializer enforces before any
/// value reaches here.
#[derive(Debug, Default)]
struct PropertyBudget {
    entries: usize,
    bytes: usize,
    exceeded: bool,
}

impl PropertyBudget {
    fn charge_entry(&mut self) {
        self.entries = self.entries.saturating_add(1);
        if self.entries > MAX_EXCALIDRAW_PROPERTY_ENTRIES {
            self.exceeded = true;
        }
    }

    fn charge_bytes(&mut self, bytes: usize) {
        self.bytes = self.bytes.saturating_add(bytes);
        if self.bytes > MAX_EXCALIDRAW_PROPERTY_BYTES {
            self.exceeded = true;
        }
    }

    /// Charges one map entry — its key plus its whole value — and reports
    /// whether the projection is still within budget afterwards.
    fn charge_entry_value(&mut self, key: &str, value: &PropertyValue) -> bool {
        if self.exceeded {
            return false;
        }
        self.charge_entry();
        self.charge_bytes(key.len());
        if self.exceeded {
            return false;
        }
        self.charge_value(value);
        !self.exceeded
    }

    fn charge_value(&mut self, value: &PropertyValue) {
        match value {
            PropertyValue::Null | PropertyValue::Bool(_) => self.charge_bytes(1),
            PropertyValue::I64(_) | PropertyValue::U64(_) | PropertyValue::F64(_) => {
                self.charge_bytes(8);
            }
            PropertyValue::String(value) => self.charge_bytes(value.len()),
            PropertyValue::Sequence(values) => {
                for value in values {
                    if self.exceeded {
                        return;
                    }
                    self.charge_entry();
                    if self.exceeded {
                        return;
                    }
                    self.charge_value(value);
                }
            }
            PropertyValue::Map(map) => {
                for (key, value) in map {
                    if !self.charge_entry_value(key, value) {
                        return;
                    }
                }
            }
        }
    }
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

fn quarantined(code: &str, message: &str) -> ExtractedSource {
    ExtractedSource::skipped(
        ExtractionCoverage::Quarantined,
        ExtractionNotice::new(code, message),
    )
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

/// Two-lane notice buffer.
///
/// Per-element notices are unbounded in principle — one hostile drawing can
/// carry a defect per element — so they are capped and their elision is
/// declared. Terminal notices are the whole-document declarations emitted after
/// the element pass; there are at most a handful of them and losing one would
/// make a declared elision silent, so they live in their own lane and are never
/// evicted. The combined output still honours [`MAX_EXCALIDRAW_NOTICES`]:
/// per-element notices are trimmed to make room, and that trim is itself
/// declared by the truncation notice.
#[derive(Debug, Default)]
struct NoticeCollector {
    notices: Vec<ExtractionNotice>,
    terminal: Vec<ExtractionNotice>,
    defects: usize,
    truncated: bool,
}

impl NoticeCollector {
    /// Records a per-element defect: the source is at best `indexed-partial`.
    fn push(&mut self, code: impl Into<String>, message: impl Into<String>) {
        self.defects += 1;
        if self.notices.len() < MAX_EXCALIDRAW_NOTICES {
            self.notices.push(ExtractionNotice::new(code, message));
        } else if !self.truncated {
            self.notices.pop();
            self.notices.push(truncation_notice());
            self.truncated = true;
        }
    }

    /// Records a whole-document policy elision. Policy notices are reported but
    /// are not defects, so a drawing whose image payloads were summarized is
    /// still `indexed-complete`.
    fn push_terminal_policy(&mut self, code: impl Into<String>, message: impl Into<String>) {
        self.terminal.push(ExtractionNotice::new(code, message));
    }

    /// Records a whole-document defect, such as a dropped property projection.
    fn push_terminal_defect(&mut self, code: impl Into<String>, message: impl Into<String>) {
        self.defects += 1;
        self.terminal.push(ExtractionNotice::new(code, message));
    }

    fn has_defects(&self) -> bool {
        self.defects != 0
    }

    fn into_notices(mut self) -> Vec<ExtractionNotice> {
        let room = MAX_EXCALIDRAW_NOTICES.saturating_sub(self.terminal.len());
        if self.notices.len() > room {
            self.notices.truncate(room);
            if let Some(last) = self.notices.last_mut() {
                *last = truncation_notice();
            }
        }
        self.notices.append(&mut self.terminal);
        self.notices
    }
}

fn truncation_notice() -> ExtractionNotice {
    ExtractionNotice::new(
        "excalidraw_notices_truncated",
        "additional malformed or unsupported Excalidraw entries were not reported",
    )
}
