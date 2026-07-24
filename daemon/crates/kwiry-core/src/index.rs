use std::fs;
use std::path::Path;

use tantivy::Index;
use tantivy::schema::{FAST, Field, INDEXED, STORED, STRING, Schema, TEXT, TantivyDocument};

use crate::chunk::ingest_vault_files;
use crate::error::{Error, Result};
use crate::generation::DataRoot;
use crate::lexical::normalize_raw;
use crate::manifest::{Manifest, registration_fingerprint, source_key};
use crate::model::{
    Config, FileOutcomeKind, HostProfile, IndexStats, PreparedChunk, ResourceKey,
    RetrievalMetadata, VaultRegistration,
};
use crate::partition::{GenerationLayout, partition_index_dir};

const WRITER_MEMORY_BYTES: usize = 50_000_000;

#[derive(Debug, Clone)]
pub(crate) struct Fields {
    pub source_key: Option<Field>,
    pub chunk_id: Field,
    pub vault_id: Field,
    pub room: Field,
    pub path: Field,
    pub filename: Field,
    pub stem: Field,
    pub aliases: Field,
    pub filename_raw: Field,
    pub stem_raw: Field,
    pub aliases_raw: Field,
    pub heading_path: Field,
    pub heading_text: Field,
    pub heading_raw: Field,
    pub title: Field,
    pub title_raw: Field,
    pub title_exact: Option<Field>,
    pub description_exact: Option<Field>,
    pub status_exact: Option<Field>,
    pub date_exact: Option<Field>,
    pub content: Field,
    pub content_identifiers: Field,
    pub frontmatter: Field,
    pub tags: Field,
    pub links_out: Field,
    pub mtime: Field,
    pub content_hash: Field,
    pub chunking_version: Field,
}

impl Fields {
    pub fn from_schema(schema: &Schema) -> Result<Self> {
        Ok(Self {
            source_key: schema.get_field("source_key").ok(),
            chunk_id: field(schema, "chunk_id")?,
            vault_id: field(schema, "vault_id")?,
            room: field(schema, "room")?,
            path: field(schema, "path")?,
            filename: field(schema, "filename")?,
            stem: field(schema, "stem")?,
            aliases: field(schema, "aliases")?,
            filename_raw: field(schema, "filename_raw")?,
            stem_raw: field(schema, "stem_raw")?,
            aliases_raw: field(schema, "aliases_raw")?,
            heading_path: field(schema, "heading_path")?,
            heading_text: field(schema, "heading_text")?,
            heading_raw: field(schema, "heading_raw")?,
            title: field(schema, "title")?,
            title_raw: field(schema, "title_raw")?,
            title_exact: schema.get_field("title_exact").ok(),
            description_exact: schema.get_field("description_exact").ok(),
            status_exact: schema.get_field("status_exact").ok(),
            date_exact: schema.get_field("date_exact").ok(),
            content: field(schema, "content")?,
            content_identifiers: field(schema, "content_identifiers")?,
            frontmatter: field(schema, "frontmatter")?,
            tags: field(schema, "tags")?,
            links_out: field(schema, "links_out")?,
            mtime: field(schema, "mtime")?,
            content_hash: field(schema, "content_hash")?,
            chunking_version: field(schema, "chunking_version")?,
        })
    }
}

pub fn build_index(config: &Config, data_dir: &Path) -> Result<IndexStats> {
    if config.vaults.is_empty() {
        return Err(Error::NoVaults);
    }

    let data_root = DataRoot::new(data_dir);
    let _lock = data_root.acquire_writer_lock()?;
    let candidate = data_root.create_candidate()?;
    let result = match config.server.profile {
        HostProfile::Desktop => {
            build_desktop_candidate(config, &candidate.index_dir, &candidate.manifest_path())
        }
        HostProfile::OpenClast => build_openclast_candidate(
            config,
            &candidate.partitions_dir,
            &candidate.manifest_path(),
            &candidate.layout_path(),
        ),
    };
    let stats = match result {
        Ok(stats) => stats,
        Err(error) => {
            let _ = fs::remove_dir_all(&candidate.staging_dir);
            return Err(error);
        }
    };
    data_root.publish(candidate)?;
    Ok(stats)
}

fn build_desktop_candidate(
    config: &Config,
    index_dir: &Path,
    manifest_path: &Path,
) -> Result<IndexStats> {
    let schema = build_schema();
    let fields = Fields::from_schema(&schema)?;
    let index =
        Index::create_in_dir(index_dir, schema).map_err(|error| Error::Index(error.to_string()))?;
    let mut writer = index
        .writer(WRITER_MEMORY_BYTES)
        .map_err(|error| Error::Index(error.to_string()))?;
    let mut manifest = Manifest::default();
    let mut stats = IndexStats::default();

    for vault in &config.vaults {
        if !vault.path.is_dir() {
            return Err(Error::InvalidVaultPath(vault.path.clone()));
        }
        let fingerprint = registration_fingerprint(vault);
        let (outcomes, discovery_warnings) = ingest_vault_files(vault);
        stats.warnings.extend(discovery_warnings);
        for outcome in outcomes {
            if let Some(warning) = outcome.warning.clone() {
                stats.warnings.push(warning);
            }
            if outcome.kind == FileOutcomeKind::Indexed {
                stats.documents += 1;
            }
            for chunk in &outcome.chunks {
                writer
                    .add_document(chunk_document(&fields, chunk, &outcome.retrieval)?)
                    .map_err(|error| Error::Index(error.to_string()))?;
                stats.chunks += 1;
            }
            manifest.insert_outcome(&outcome, &fingerprint);
        }
    }

    writer
        .commit()
        .map_err(|error| Error::Index(error.to_string()))?;
    writer
        .wait_merging_threads()
        .map_err(|error| Error::Index(error.to_string()))?;
    manifest.mark_synced()?;
    manifest.save(manifest_path)?;

    let reader = index
        .reader()
        .map_err(|error| Error::Index(error.to_string()))?;
    let indexed_chunks = reader.searcher().num_docs() as usize;
    if indexed_chunks != stats.chunks || manifest.chunk_count() != stats.chunks {
        return Err(Error::State(format!(
            "candidate count mismatch: stats={}, manifest={}, index={indexed_chunks}",
            stats.chunks,
            manifest.chunk_count()
        )));
    }
    if manifest.document_count() != stats.documents {
        return Err(Error::State(format!(
            "candidate document mismatch: stats={}, manifest={}",
            stats.documents,
            manifest.document_count()
        )));
    }
    Ok(stats)
}

fn build_openclast_candidate(
    config: &Config,
    partitions_dir: &Path,
    manifest_path: &Path,
    layout_path: &Path,
) -> Result<IndexStats> {
    let resources: Vec<_> = config
        .vaults
        .iter()
        .map(|vault| {
            config.resource_key(vault).ok_or_else(|| {
                Error::State(format!(
                    "openclast vault {} is missing an exact resource classification",
                    vault.id
                ))
            })
        })
        .collect::<Result<_>>()?;
    let layout = GenerationLayout::openclast(resources)?;
    let mut manifest = Manifest::default();
    let mut stats = IndexStats::default();

    for vault in &config.vaults {
        let resource = config.resource_key(vault).ok_or_else(|| {
            Error::State(format!(
                "openclast vault {} is missing an exact resource classification",
                vault.id
            ))
        })?;
        let index_dir = partition_index_dir(partitions_dir, &resource);
        fs::create_dir_all(&index_dir)
            .map_err(|error| crate::error::io_error(&index_dir, error))?;
        build_partition(vault, &resource, &index_dir, &mut manifest, &mut stats)?;
    }

    manifest.mark_synced()?;
    manifest.save(manifest_path)?;
    layout.save(layout_path)?;

    let indexed_chunks = layout
        .partitions
        .iter()
        .try_fold(0_u64, |total, partition| {
            let index_dir = partition_index_dir(partitions_dir, &partition.resource);
            let (index, _) = open_index_dir(&index_dir)?;
            let reader = index
                .reader()
                .map_err(|error| Error::Index(error.to_string()))?;
            Ok::<_, Error>(total + reader.searcher().num_docs())
        })? as usize;
    if indexed_chunks != stats.chunks || manifest.chunk_count() != stats.chunks {
        return Err(Error::State(format!(
            "candidate count mismatch: stats={}, manifest={}, partitions={indexed_chunks}",
            stats.chunks,
            manifest.chunk_count()
        )));
    }
    if manifest.document_count() != stats.documents {
        return Err(Error::State(format!(
            "candidate document mismatch: stats={}, manifest={}",
            stats.documents,
            manifest.document_count()
        )));
    }
    Ok(stats)
}

fn build_partition(
    vault: &VaultRegistration,
    resource: &ResourceKey,
    index_dir: &Path,
    manifest: &mut Manifest,
    stats: &mut IndexStats,
) -> Result<()> {
    if !vault.path.is_dir() {
        return Err(Error::InvalidVaultPath(vault.path.clone()));
    }
    if vault.id != resource.vault_id || vault.room.as_deref() != Some(resource.room_id.as_str()) {
        return Err(Error::State(format!(
            "vault {} does not match its resource partition",
            vault.id
        )));
    }

    let schema = build_schema();
    let fields = Fields::from_schema(&schema)?;
    let index =
        Index::create_in_dir(index_dir, schema).map_err(|error| Error::Index(error.to_string()))?;
    let mut writer = index
        .writer(WRITER_MEMORY_BYTES)
        .map_err(|error| Error::Index(error.to_string()))?;
    let fingerprint = registration_fingerprint(vault);
    let (outcomes, discovery_warnings) = ingest_vault_files(vault);
    stats.warnings.extend(discovery_warnings);
    for outcome in outcomes {
        if let Some(warning) = outcome.warning.clone() {
            stats.warnings.push(warning);
        }
        if outcome.kind == FileOutcomeKind::Indexed {
            stats.documents += 1;
        }
        for chunk in &outcome.chunks {
            if chunk.vault_id != resource.vault_id
                || chunk.room.as_deref() != Some(resource.room_id.as_str())
            {
                return Err(Error::State(format!(
                    "chunk {} does not match its resource partition",
                    chunk.chunk_id
                )));
            }
            writer
                .add_document(chunk_document(&fields, chunk, &outcome.retrieval)?)
                .map_err(|error| Error::Index(error.to_string()))?;
            stats.chunks += 1;
        }
        manifest.insert_outcome_for_resource(&outcome, &fingerprint, Some(resource));
    }
    writer
        .commit()
        .map_err(|error| Error::Index(error.to_string()))?;
    writer
        .wait_merging_threads()
        .map_err(|error| Error::Index(error.to_string()))?;
    Ok(())
}

pub(crate) fn open_index(data_dir: &Path) -> Result<(Index, Fields)> {
    let index_dir = DataRoot::new(data_dir).active_or_legacy_index()?;
    open_index_dir(&index_dir)
}

pub(crate) fn open_index_dir(index_dir: &Path) -> Result<(Index, Fields)> {
    if !index_dir.join("meta.json").is_file() {
        return Err(Error::Index(format!(
            "no index found at {}; run `kwiry index` first",
            index_dir.display()
        )));
    }
    let index = Index::open_in_dir(index_dir).map_err(|error| Error::Index(error.to_string()))?;
    let fields = Fields::from_schema(&index.schema())?;
    Ok((index, fields))
}

pub(crate) fn build_schema() -> Schema {
    let mut builder = Schema::builder();
    builder.add_text_field("source_key", STRING | STORED);
    builder.add_text_field("chunk_id", STRING | STORED);
    builder.add_text_field("vault_id", STRING | STORED);
    builder.add_text_field("room", STRING | STORED);
    builder.add_text_field("path", STRING | STORED);
    builder.add_text_field("filename", TEXT);
    builder.add_text_field("stem", TEXT);
    builder.add_text_field("aliases", TEXT);
    builder.add_text_field("filename_raw", STRING);
    builder.add_text_field("stem_raw", STRING);
    builder.add_text_field("aliases_raw", STRING);
    builder.add_text_field("heading_path", STORED);
    builder.add_text_field("heading_text", TEXT | STORED);
    builder.add_text_field("heading_raw", STRING);
    builder.add_text_field("title", TEXT | STORED);
    builder.add_text_field("title_raw", STRING);
    builder.add_text_field("title_exact", STRING | STORED);
    builder.add_text_field("description_exact", STRING | STORED);
    builder.add_text_field("status_exact", STRING | STORED);
    builder.add_text_field("date_exact", STRING | STORED);
    builder.add_text_field("content", TEXT | STORED);
    builder.add_text_field("content_identifiers", STRING);
    builder.add_text_field("frontmatter", STORED);
    builder.add_text_field("tags", STRING | STORED);
    builder.add_text_field("links_out", STORED);
    builder.add_u64_field("mtime", INDEXED | STORED | FAST);
    builder.add_text_field("content_hash", STRING | STORED);
    builder.add_u64_field("chunking_version", INDEXED | STORED | FAST);
    builder.build()
}

pub(crate) fn chunk_document(
    fields: &Fields,
    prepared: &PreparedChunk,
    retrieval: &RetrievalMetadata,
) -> Result<TantivyDocument> {
    let chunk = &prepared.chunk;
    let mut document = TantivyDocument::default();
    document.add_text(
        required_optional_field(fields.source_key, "source_key")?,
        source_key(&chunk.vault_id, &chunk.path),
    );
    document.add_text(fields.chunk_id, &chunk.chunk_id);
    document.add_text(fields.vault_id, &chunk.vault_id);
    document.add_text(fields.room, chunk.room.as_deref().unwrap_or_default());
    document.add_text(fields.path, &chunk.path);
    document.add_text(fields.filename, &retrieval.filename);
    document.add_text(fields.stem, &retrieval.stem);
    add_raw(&mut document, fields.filename_raw, &retrieval.filename);
    add_raw(&mut document, fields.stem_raw, &retrieval.stem);
    for alias in &retrieval.aliases {
        document.add_text(fields.aliases, alias);
        add_raw(&mut document, fields.aliases_raw, alias);
    }
    document.add_text(
        fields.heading_path,
        serde_json::to_string(&chunk.heading_path)
            .map_err(|error| Error::Index(error.to_string()))?,
    );
    document.add_text(fields.heading_text, &prepared.heading_text);
    add_raw(&mut document, fields.heading_raw, &prepared.heading_text);
    let title = chunk.frontmatter.title.as_deref().unwrap_or_default();
    document.add_text(fields.title, title);
    add_raw(&mut document, fields.title_raw, title);
    document.add_text(
        required_optional_field(fields.title_exact, "title_exact")?,
        chunk.frontmatter.title.as_deref().unwrap_or_default(),
    );
    document.add_text(
        required_optional_field(fields.description_exact, "description_exact")?,
        chunk.frontmatter.description.as_deref().unwrap_or_default(),
    );
    document.add_text(
        required_optional_field(fields.status_exact, "status_exact")?,
        chunk.frontmatter.status.as_deref().unwrap_or_default(),
    );
    document.add_text(
        required_optional_field(fields.date_exact, "date_exact")?,
        chunk.frontmatter.date.as_deref().unwrap_or_default(),
    );
    document.add_text(fields.content, &chunk.content);
    for identifier in &prepared.technical_identifiers {
        document.add_text(fields.content_identifiers, identifier);
    }
    document.add_text(
        fields.frontmatter,
        serde_json::to_string(&chunk.frontmatter)
            .map_err(|error| Error::Index(error.to_string()))?,
    );
    for tag in &chunk.frontmatter.tags {
        document.add_text(fields.tags, tag);
    }
    document.add_text(
        fields.links_out,
        serde_json::to_string(&chunk.links_out).map_err(|error| Error::Index(error.to_string()))?,
    );
    document.add_u64(fields.mtime, chunk.mtime);
    document.add_text(fields.content_hash, &chunk.content_hash);
    document.add_u64(fields.chunking_version, chunk.chunking_version);
    Ok(document)
}

fn add_raw(document: &mut TantivyDocument, field: Field, value: &str) {
    if let Some(value) = normalize_raw(value) {
        document.add_text(field, value);
    }
}

fn required_optional_field(field: Option<Field>, name: &str) -> Result<Field> {
    field.ok_or_else(|| Error::Index(format!("missing schema field {name}")))
}

fn field(schema: &Schema, name: &str) -> Result<Field> {
    schema
        .get_field(name)
        .map_err(|error| Error::Index(format!("missing schema field {name}: {error}")))
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::tempdir;

    use super::*;
    use crate::model::VaultRegistration;
    use crate::{SearchRequest, search_index};

    #[test]
    fn rebuild_produces_identical_lexical_results() {
        let temporary = tempdir().unwrap();
        let vault_path = temporary.path().join("vault");
        let index_path = temporary.path().join("index");
        fs::create_dir(&vault_path).unwrap();
        fs::write(
            vault_path.join("note.md"),
            "---\ntitle: Search Note\ntags: [fixture]\n---\n# Retrieval\nDeterministic phosphorescent indexing.",
        )
        .unwrap();
        let config = Config {
            vaults: vec![VaultRegistration {
                id: "fixture".into(),
                path: vault_path,
                room: None,
            }],
            ..Config::default()
        };
        let request = SearchRequest {
            query: "phosphorescent".into(),
            limit: 20,
            vault_id: None,
        };

        let first_stats = build_index(&config, &index_path).unwrap();
        let first = search_index(&index_path, &request).unwrap();
        fs::remove_dir_all(&index_path).unwrap();
        let second_stats = build_index(&config, &index_path).unwrap();
        let second = search_index(&index_path, &request).unwrap();

        assert_eq!(first_stats.documents, 1);
        assert_eq!(first_stats, second_stats);
        assert_eq!(first, second);
        assert_eq!(first.len(), 1);
        assert_eq!(first[0].path, "note.md");
    }

    #[test]
    fn explicit_rebuild_replaces_an_incompatible_generation_pointer() {
        let temporary = tempdir().unwrap();
        let vault_path = temporary.path().join("vault");
        let data_root = temporary.path().join("data");
        fs::create_dir(&vault_path).unwrap();
        fs::create_dir(&data_root).unwrap();
        fs::write(vault_path.join("note.md"), "# Retrieval\nrebuildterm").unwrap();
        fs::write(
            data_root.join("current.json"),
            r#"{"layout_version":1,"index_format_version":2,"generation":"old"}"#,
        )
        .unwrap();
        let config = Config {
            vaults: vec![VaultRegistration {
                id: "fixture".into(),
                path: vault_path,
                room: None,
            }],
            ..Config::default()
        };

        build_index(&config, &data_root).unwrap();
        let current: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(data_root.join("current.json")).unwrap())
                .unwrap();
        assert_eq!(
            current["index_format_version"],
            crate::manifest::INDEX_FORMAT_VERSION
        );
        assert_eq!(
            search_index(
                &data_root,
                &SearchRequest {
                    query: "rebuildterm".into(),
                    limit: 20,
                    vault_id: None,
                }
            )
            .unwrap()
            .len(),
            1
        );
    }

    #[test]
    fn failed_rebuild_preserves_current_generation() {
        let temporary = tempdir().unwrap();
        let vault_path = temporary.path().join("vault");
        let data_root = temporary.path().join("data");
        fs::create_dir(&vault_path).unwrap();
        fs::write(vault_path.join("note.md"), "stable phosphorescent").unwrap();
        let config = Config {
            vaults: vec![VaultRegistration {
                id: "fixture".into(),
                path: vault_path.clone(),
                room: None,
            }],
            ..Config::default()
        };
        build_index(&config, &data_root).unwrap();
        let before = search_index(
            &data_root,
            &SearchRequest {
                query: "phosphorescent".into(),
                limit: 20,
                vault_id: None,
            },
        )
        .unwrap();

        fs::remove_dir_all(&vault_path).unwrap();
        let broken = Config {
            vaults: vec![VaultRegistration {
                id: "fixture".into(),
                path: vault_path,
                room: None,
            }],
            ..Config::default()
        };
        assert!(build_index(&broken, &data_root).is_err());
        let after = search_index(
            &data_root,
            &SearchRequest {
                query: "phosphorescent".into(),
                limit: 20,
                vault_id: None,
            },
        )
        .unwrap();
        assert_eq!(before, after);
    }
}
