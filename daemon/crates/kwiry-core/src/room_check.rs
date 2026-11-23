//! One walk-and-prepare pass over a governed room, shared by the labelled
//! build and the offline `checkroom` preflight, so both decide a room's
//! publication label with the same walker, reader, parser and predicate.
use std::io::Read;
use std::path::Path;

use serde::Serialize;

use crate::chunk::ingest_file_with_room_handle;
use crate::extract::ExtractionCoverage;
use crate::format::SourceFormat;
use crate::model::{
    Config, DiscoveredFile, FileIngestOutcome, FileOutcomeKind, MAX_FILE_BYTES, VaultRegistration,
};
use crate::room_walk::{RoomRead, RoomRoots, nesting_failure};
use crate::walk::EnumerationResult;

/// What one pass over a room observed.
pub(crate) struct RoomPass {
    pub enumeration: EnumerationResult,
    /// The room may carry its configured label: its root opened, its walk was
    /// complete, and every source prepared as a clean Markdown input.
    pub clean_and_exact: bool,
    /// A registered room root appeared inside the walk. Sources already
    /// delivered must be discarded; `enumeration` is the failed walk.
    pub nesting_detected: bool,
}

/// Walk `vault` beneath its captured root and prepare each discovered source
/// in walk order, handing every outcome to `each`. This is the governed build's
/// labelling pass; `checkroom` calls it unchanged.
pub(crate) fn prepare_room(
    vault: &VaultRegistration,
    room_roots: &RoomRoots,
    mut each: impl FnMut(&DiscoveredFile, FileIngestOutcome, &RoomRead<'_>),
) -> RoomPass {
    let (mut enumeration, room) = room_roots.walk(vault);
    let mut clean = true;
    if let Some(room) = room.as_ref() {
        for file in &enumeration.files {
            if room.nesting_detected() {
                break;
            }
            let outcome = ingest_file_with_room_handle(vault, file, Some(room));
            clean &= outcome.clean_snapshot_input();
            each(file, outcome, room);
        }
    }
    let nesting_detected = room.as_ref().is_some_and(RoomRead::nesting_detected);
    if nesting_detected {
        enumeration = nesting_failure(vault);
    }
    RoomPass {
        clean_and_exact: room.is_some() && enumeration.completeness.may_infer_deletions() && clean,
        enumeration,
        nesting_detected,
    }
}

/// Why a room would not receive its label. A closed, stable set: callers may
/// match on the serialized names.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RoomCheckReason {
    /// Frontmatter opened with `---` and closed, but is not valid YAML.
    InvalidFrontmatter,
    /// Frontmatter is larger than the frontmatter byte budget and is ignored.
    FrontmatterOverSize,
    /// Source bytes are not valid UTF-8.
    NonUtf8Content,
    /// Source contains a NUL byte.
    NulByte,
    /// Source is larger than the per-file size budget.
    OverSizeBudget,
    /// Markdown yields more sections than the per-source section limit.
    SectionLimit,
    /// Heading-path bytes exceed the per-source heading-path limit.
    HeadingPathLimit,
    /// Prepared chunks (sections after long-section splitting) exceed the
    /// per-source chunk limit while sections alone do not.
    ChunkInventoryLimit,
    /// The bytes yield an extraction other than indexed-complete (partial,
    /// quarantined, unreadable or empty) for a cause not named above.
    IncompleteExtraction,
    /// The source is an admitted non-Markdown format, or Markdown not named
    /// `*.md`; only `*.md` sources may stand under a label.
    NonMarkdownFormat,
    /// The relative path is not a normalized forward-slash path (for example a
    /// name containing a backslash).
    InvalidPath,
    /// A regular file with more than one hard link.
    HardLink,
    /// An entry that is neither a regular file nor a directory.
    SpecialFile,
    /// A symbolic link, or an entry on another filesystem.
    BoundaryEscape,
    /// A directory holds an entry whose name is not valid UTF-8.
    NonUtf8Name,
    /// An entry or directory could not be opened or read.
    IoUnavailable,
    /// A source changed while it was being read.
    ChangedDuringRead,
    /// The room root itself cannot be walked: missing, not canonical, shared,
    /// overlapping another room or Kwiry state, or not on its expected mount.
    RoomBoundary,
    /// Withheld for a reason this build does not classify.
    Unclassified,
}

impl RoomCheckReason {
    pub const ALL: [Self; 19] = [
        Self::InvalidFrontmatter,
        Self::FrontmatterOverSize,
        Self::NonUtf8Content,
        Self::NulByte,
        Self::OverSizeBudget,
        Self::SectionLimit,
        Self::HeadingPathLimit,
        Self::ChunkInventoryLimit,
        Self::IncompleteExtraction,
        Self::NonMarkdownFormat,
        Self::InvalidPath,
        Self::HardLink,
        Self::SpecialFile,
        Self::BoundaryEscape,
        Self::NonUtf8Name,
        Self::IoUnavailable,
        Self::ChangedDuringRead,
        Self::RoomBoundary,
        Self::Unclassified,
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::InvalidFrontmatter => "invalid_frontmatter",
            Self::FrontmatterOverSize => "frontmatter_over_size",
            Self::NonUtf8Content => "non_utf8_content",
            Self::NulByte => "nul_byte",
            Self::OverSizeBudget => "over_size_budget",
            Self::SectionLimit => "section_limit",
            Self::HeadingPathLimit => "heading_path_limit",
            Self::ChunkInventoryLimit => "chunk_inventory_limit",
            Self::IncompleteExtraction => "incomplete_extraction",
            Self::NonMarkdownFormat => "non_markdown_format",
            Self::InvalidPath => "invalid_path",
            Self::HardLink => "hard_link",
            Self::SpecialFile => "special_file",
            Self::BoundaryEscape => "boundary_escape",
            Self::NonUtf8Name => "non_utf8_name",
            Self::IoUnavailable => "io_unavailable",
            Self::ChangedDuringRead => "changed_during_read",
            Self::RoomBoundary => "room_boundary",
            Self::Unclassified => "unclassified",
        }
    }

    /// Fixed explanatory text. Never contains file content, names or labels.
    pub const fn detail(self) -> &'static str {
        match self {
            Self::InvalidFrontmatter => "frontmatter is not valid YAML",
            Self::FrontmatterOverSize => "frontmatter exceeds the frontmatter byte budget",
            Self::NonUtf8Content => "file content is not valid UTF-8",
            Self::NulByte => "file content contains a NUL byte",
            Self::OverSizeBudget => "file exceeds the per-file size budget",
            Self::SectionLimit => "file has more sections than the per-source section limit",
            Self::HeadingPathLimit => "heading paths exceed the per-source heading-path byte limit",
            Self::ChunkInventoryLimit => "prepared chunks exceed the per-source chunk limit",
            Self::IncompleteExtraction => "file content does not extract completely",
            Self::NonMarkdownFormat => {
                "only .md Markdown sources may be present in a labelled room"
            }
            Self::InvalidPath => "relative path is not a normalized forward-slash path",
            Self::HardLink => "regular file has more than one hard link",
            Self::SpecialFile => "entry is not a regular file or directory",
            Self::BoundaryEscape => "entry is a symbolic link or on another filesystem",
            Self::NonUtf8Name => "directory holds an entry whose name is not valid UTF-8",
            Self::IoUnavailable => "entry could not be opened or read",
            Self::ChangedDuringRead => "file changed while it was being read",
            Self::RoomBoundary => "room root cannot be walked within the room boundary",
            Self::Unclassified => "withheld for an unclassified reason",
        }
    }

    /// Whether the cause is in the file's bytes, in a read that may not
    /// repeat, or in the room's structure.
    pub const fn class(self) -> RoomCheckClass {
        match self {
            Self::InvalidFrontmatter
            | Self::FrontmatterOverSize
            | Self::NonUtf8Content
            | Self::NulByte
            | Self::OverSizeBudget
            | Self::SectionLimit
            | Self::HeadingPathLimit
            | Self::ChunkInventoryLimit
            | Self::IncompleteExtraction => RoomCheckClass::Content,
            Self::IoUnavailable | Self::ChangedDuringRead => RoomCheckClass::Transient,
            Self::NonMarkdownFormat
            | Self::InvalidPath
            | Self::HardLink
            | Self::SpecialFile
            | Self::BoundaryEscape
            | Self::NonUtf8Name
            | Self::RoomBoundary
            | Self::Unclassified => RoomCheckClass::Structural,
        }
    }
}

/// `content`: decided by the file's bytes alone; the same bytes always give
/// the same finding. `transient`: a read failed or raced; checking again may
/// differ. `structural`: the room's shape (names, formats, links, entry types,
/// root) withholds the label.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RoomCheckClass {
    Content,
    Transient,
    Structural,
}

/// Section accounting for a limit refusal. See the README for definitions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct SectionUsage {
    pub sections: usize,
    pub heading_path_bytes: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RoomCheckFinding {
    /// Room-relative path; empty for the room root.
    pub path: String,
    pub reason: RoomCheckReason,
    pub class: RoomCheckClass,
    pub detail: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub usage: Option<SectionUsage>,
}

impl RoomCheckFinding {
    fn new(path: String, reason: RoomCheckReason) -> Self {
        Self {
            path,
            reason,
            class: reason.class(),
            detail: reason.detail(),
            usage: None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct RoomCheckLimits {
    pub max_file_bytes: u64,
    pub max_sections: usize,
    pub max_heading_path_bytes: usize,
    pub max_chunks: usize,
}

pub const ROOM_CHECK_LIMITS: RoomCheckLimits = RoomCheckLimits {
    max_file_bytes: MAX_FILE_BYTES,
    max_sections: crate::extract::MAX_EXTRACTED_SECTIONS_PER_SOURCE,
    max_heading_path_bytes: crate::extract::MAX_EXTRACTED_HEADING_BYTES_PER_SOURCE,
    max_chunks: crate::source::MAX_PREPARED_CHUNKS_PER_SOURCE,
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RoomCheckReport {
    /// The room would receive its configured publication label.
    pub would_label: bool,
    /// Sources the walk admitted and the preparation pass read.
    pub files_checked: usize,
    /// Named files whose removal from the room clears their finding.
    pub offenders: Vec<RoomCheckFinding>,
    /// Walk failures that no single named file explains.
    pub walk_defects: Vec<RoomCheckFinding>,
}

/// Run the labelling pass for `config.vaults[vault]` without an index, a data
/// directory, a lock or any write. `data_root`, when given, is only compared
/// against the room for overlap, exactly as the daemon compares it.
pub fn check_room(
    config: &Config,
    vault: usize,
    data_root: Option<&Path>,
) -> crate::Result<RoomCheckReport> {
    let vault = config
        .vaults
        .get(vault)
        .ok_or_else(|| crate::Error::State("room registration is not configured".into()))?;
    let room_roots = RoomRoots::capture(config, data_root);
    let mut files_checked = 0;
    let mut offenders = Vec::new();
    let pass = prepare_room(vault, &room_roots, |file, outcome, room| {
        files_checked += 1;
        if !outcome.clean_snapshot_input() {
            offenders.push(classify_outcome(file, &outcome, room));
        }
    });
    if pass.nesting_detected {
        offenders.clear();
    }
    let mut walk_defects = Vec::new();
    for warning in &pass.enumeration.warnings {
        let relative = warning
            .path
            .strip_prefix(&vault.path)
            .map(|path| path.to_string_lossy().into_owned())
            .unwrap_or_default();
        let reason = walk_reason(&warning.message, relative.is_empty());
        let finding = RoomCheckFinding::new(relative.clone(), reason);
        if !relative.is_empty() && pass.enumeration.excluded.contains(&relative) {
            offenders.push(finding);
        } else {
            walk_defects.push(finding);
        }
    }
    offenders.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(RoomCheckReport {
        would_label: pass.clean_and_exact,
        files_checked,
        offenders,
        walk_defects,
    })
}

fn walk_reason(message: &str, root: bool) -> RoomCheckReason {
    use RoomCheckReason as R;
    match message {
        "entry excluded by room boundary" => R::BoundaryEscape,
        "entry is not a regular file or directory" => R::SpecialFile,
        "hard-linked file excluded" => R::HardLink,
        "file exceeds source size budget" => R::OverSizeBudget,
        "directory entry is not valid UTF-8" => R::NonUtf8Name,
        _ if root => R::RoomBoundary,
        "directory could not be opened within the room"
        | "directory could not be enumerated safely"
        | "entry could not be opened safely"
        | "entry metadata could not be read safely" => R::IoUnavailable,
        _ => R::Unclassified,
    }
}

fn classify_outcome(
    file: &DiscoveredFile,
    outcome: &FileIngestOutcome,
    room: &RoomRead<'_>,
) -> RoomCheckFinding {
    use RoomCheckReason as R;
    let path = file.relative_path.clone();
    let markdown_md = outcome.format == SourceFormat::Markdown
        && path
            .rsplit_once('.')
            .is_some_and(|(_, ext)| ext.eq_ignore_ascii_case("md"));
    if !markdown_md {
        return RoomCheckFinding::new(path, R::NonMarkdownFormat);
    }
    let message = outcome
        .warning
        .as_ref()
        .map_or("", |warning| warning.message.as_str());
    let reason = match message {
        "host_read: source changed during bounded read" => R::ChangedDuringRead,
        "host_read: source is unavailable"
        | "host_read: oversized source could not be recorded" => R::IoUnavailable,
        "skipped binary file containing NUL bytes" => R::NulByte,
        "invalid YAML frontmatter" => R::InvalidFrontmatter,
        _ if message.starts_with("frontmatter exceeds ") => R::FrontmatterOverSize,
        "path must be a normalized vault-relative forward-slash file path" => R::InvalidPath,
        "prepared source exceeds the heading-path memory limit" => R::HeadingPathLimit,
        "prepared source exceeds the chunk inventory limit" => R::ChunkInventoryLimit,
        _ if message.starts_with("skipped non-UTF-8 file") => R::NonUtf8Content,
        _ if message.starts_with("skipped file larger than ") => R::OverSizeBudget,
        _ if outcome.kind == FileOutcomeKind::TransientError => R::IoUnavailable,
        _ if outcome.coverage != ExtractionCoverage::IndexedComplete => R::IncompleteExtraction,
        _ => R::Unclassified,
    };
    let mut finding = RoomCheckFinding::new(path, reason);
    if matches!(reason, R::HeadingPathLimit | R::ChunkInventoryLimit) {
        finding.usage = section_usage(room, &file.relative_path);
        // Extraction and chunk preparation refuse with the same message; the
        // section count tells which limit was reached.
        if reason == R::ChunkInventoryLimit
            && finding
                .usage
                .is_some_and(|usage| usage.sections > ROOM_CHECK_LIMITS.max_sections)
        {
            finding = RoomCheckFinding {
                usage: finding.usage,
                ..RoomCheckFinding::new(finding.path, R::SectionLimit)
            };
        }
    }
    finding
}

/// Re-read a refused source through the same room handle and count it with
/// the extractor's own section accounting.
fn section_usage(room: &RoomRead<'_>, relative: &str) -> Option<SectionUsage> {
    let mut bytes = Vec::new();
    room.open(relative)
        .ok()?
        .take(MAX_FILE_BYTES + 1)
        .read_to_end(&mut bytes)
        .ok()?;
    let (sections, heading_path_bytes) = crate::formats::section_usage(&bytes)?;
    Some(SectionUsage {
        sections,
        heading_path_bytes,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reason_names_are_unique_and_snake_case() {
        let names: std::collections::BTreeSet<_> =
            RoomCheckReason::ALL.iter().map(|r| r.as_str()).collect();
        assert_eq!(names.len(), RoomCheckReason::ALL.len());
        for reason in RoomCheckReason::ALL {
            assert_eq!(
                serde_json::to_value(reason).unwrap(),
                serde_json::Value::String(reason.as_str().into())
            );
        }
    }
}
