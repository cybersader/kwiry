use std::fs;
use std::path::Path;
use std::time::UNIX_EPOCH;

use ignore::{DirEntry, WalkBuilder};

use crate::format::SourceFormat;
use crate::model::{DiscoveredFile, IngestWarning, MAX_FILE_BYTES, VaultRegistration};
use crate::reconcile::EnumerationCompleteness;

const SKIPPED_DIRECTORIES: &[&str] = &[
    ".git",
    ".hg",
    ".svn",
    ".obsidian",
    ".trash",
    "node_modules",
    "target",
];

#[derive(Debug)]
pub(crate) struct EnumerationResult {
    pub files: Vec<DiscoveredFile>,
    pub warnings: Vec<IngestWarning>,
    pub completeness: EnumerationCompleteness,
}

pub(crate) fn discover_vault(vault: &VaultRegistration) -> EnumerationResult {
    let mut files = Vec::new();
    let mut warnings = Vec::new();
    let root = vault.path.clone();

    let walker = WalkBuilder::new(&root)
        .hidden(true)
        .follow_links(false)
        .git_ignore(true)
        .git_exclude(true)
        .git_global(true)
        .filter_entry(keep_entry)
        .build();

    for result in walker {
        let entry = match result {
            Ok(entry) => entry,
            Err(error) => {
                warnings.push(IngestWarning {
                    path: root.clone(),
                    message: error.to_string(),
                });
                continue;
            }
        };

        let Some(file_type) = entry.file_type() else {
            continue;
        };
        if !file_type.is_file() || file_type.is_symlink() {
            continue;
        }

        let path = entry.into_path();
        let Some(extension) = normalized_extension(&path) else {
            continue;
        };

        let metadata = match fs::metadata(&path) {
            Ok(metadata) => metadata,
            Err(error) => {
                warnings.push(IngestWarning {
                    path: path.clone(),
                    message: error.to_string(),
                });
                continue;
            }
        };

        if metadata.len() > MAX_FILE_BYTES {
            warnings.push(IngestWarning {
                path: path.clone(),
                message: format!(
                    "skipped file larger than {} bytes ({})",
                    MAX_FILE_BYTES,
                    metadata.len()
                ),
            });
            continue;
        }

        let relative_path = match path.strip_prefix(&root) {
            Ok(relative) => {
                let components: Option<Vec<_>> = relative
                    .components()
                    .map(|component| component.as_os_str().to_str())
                    .collect();
                let Some(components) = components else {
                    warnings.push(IngestWarning {
                        path: path.clone(),
                        message: "skipped path that is not valid UTF-8".to_owned(),
                    });
                    continue;
                };
                components.join("/")
            }
            Err(error) => {
                warnings.push(IngestWarning {
                    path: path.clone(),
                    message: error.to_string(),
                });
                continue;
            }
        };
        let modified = metadata
            .modified()
            .ok()
            .and_then(|modified| modified.duration_since(UNIX_EPOCH).ok());
        let mtime = modified.as_ref().map_or(0, std::time::Duration::as_secs);
        let mtime_nanos = modified.map_or(0, |duration| duration.as_nanos());

        files.push(DiscoveredFile {
            absolute_path: path,
            relative_path,
            extension,
            byte_length: metadata.len(),
            mtime,
            mtime_nanos,
        });
    }

    files.sort_by(|left, right| left.relative_path.cmp(&right.relative_path));
    let completeness = if warnings.is_empty() {
        EnumerationCompleteness::Complete
    } else {
        EnumerationCompleteness::Defective
    };
    EnumerationResult {
        files,
        warnings,
        completeness,
    }
}

fn keep_entry(entry: &DirEntry) -> bool {
    if entry.depth() == 0 {
        return true;
    }

    if !entry.file_type().is_some_and(|kind| kind.is_dir()) {
        return true;
    }

    let name = entry.file_name().to_string_lossy();
    !SKIPPED_DIRECTORIES.contains(&name.as_ref())
}

fn normalized_extension(path: &Path) -> Option<String> {
    SourceFormat::from_extractable_path(path)?;
    Some(path.extension()?.to_str()?.to_ascii_lowercase())
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::tempdir;

    use super::*;

    #[test]
    fn discovery_is_sorted_and_skips_hidden_and_unsupported_files() {
        let temporary = tempdir().unwrap();
        fs::write(temporary.path().join("z.md"), "z").unwrap();
        fs::write(temporary.path().join("a.TXT"), "a").unwrap();
        fs::write(temporary.path().join("dashboard.base"), "views: []").unwrap();
        fs::write(temporary.path().join("board.canvas"), "{}").unwrap();
        fs::write(temporary.path().join("report.docx"), "docx").unwrap();
        fs::write(temporary.path().join("paper.PDF"), "pdf").unwrap();
        fs::write(temporary.path().join("image.png"), "png").unwrap();
        fs::create_dir(temporary.path().join(".hidden")).unwrap();
        fs::write(temporary.path().join(".hidden/secret.md"), "secret").unwrap();
        let vault = VaultRegistration {
            id: "fixture".into(),
            path: temporary.path().to_path_buf(),
            room: None,
        };

        let result = discover_vault(&vault);
        let paths: Vec<_> = result
            .files
            .iter()
            .map(|file| file.relative_path.as_str())
            .collect();
        assert_eq!(
            paths,
            [
                "a.TXT",
                "board.canvas",
                "dashboard.base",
                // PDF is admitted, so an uppercase extension is discovered the
                // same way `a.TXT` is; only `image.png` stays unsupported.
                "paper.PDF",
                "report.docx",
                "z.md"
            ]
        );
        assert!(result.warnings.is_empty());
        assert_eq!(result.completeness, EnumerationCompleteness::Complete);
    }

    #[test]
    fn warning_marks_enumeration_defective() {
        let temporary = tempdir().unwrap();
        fs::File::create(temporary.path().join("oversized.md"))
            .unwrap()
            .set_len(MAX_FILE_BYTES + 1)
            .unwrap();
        let vault = VaultRegistration {
            id: "fixture".into(),
            path: temporary.path().to_path_buf(),
            room: None,
        };

        let result = discover_vault(&vault);

        assert!(result.files.is_empty());
        assert_eq!(result.warnings.len(), 1);
        assert_eq!(result.completeness, EnumerationCompleteness::Defective);
    }
}
