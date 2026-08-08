// SPDX-License-Identifier: MIT OR Apache-2.0

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;
use std::str::CharIndices;

use sha2::{Digest, Sha256};
use tantivy::Index;
use tantivy::schema::{
    FAST, Field, INDEXED, IndexRecordOption, JsonObjectOptions, OwnedValue, STORED, STRING, Schema,
    TEXT, TantivyDocument, TextFieldIndexing,
};
use tantivy::tokenizer::{
    RemoveLongFilter, SimpleTokenizer, TextAnalyzer, Token, TokenStream, Tokenizer,
};
use unicode_normalization::char::is_combining_mark;

use crate::chunk::ingest_vault_files;
use crate::error::{Error, Result};
use crate::format::SourceFormat;
use crate::generation::DataRoot;
use crate::lexical::{fold_lexical, normalize_raw};
use crate::manifest::{Manifest, registration_fingerprint, source_key};
use crate::model::{
    Config, HostProfile, IndexStats, PreparedChunk, PropertyBag, PropertyValue, ResourceKey,
    RetrievalMetadata, VaultRegistration,
};
use crate::partition::{GenerationLayout, partition_index_dir};

const WRITER_MEMORY_BYTES: usize = 50_000_000;
const MAX_RAW_PROPERTY_TOKEN_BYTES: usize = 39;

#[derive(Clone, Default)]
struct LexicalTokenizer {
    token: Token,
}

struct LexicalTokenStream<'a> {
    text: &'a str,
    chars: CharIndices<'a>,
    token: &'a mut Token,
}

impl Tokenizer for LexicalTokenizer {
    type TokenStream<'a> = LexicalTokenStream<'a>;

    fn token_stream<'a>(&'a mut self, text: &'a str) -> Self::TokenStream<'a> {
        self.token.reset();
        LexicalTokenStream {
            text,
            chars: text.char_indices(),
            token: &mut self.token,
        }
    }
}

impl LexicalTokenStream<'_> {
    fn search_token_end(&mut self) -> usize {
        self.chars
            .find(|(_, character)| !character.is_alphanumeric() && !is_combining_mark(*character))
            .map_or(self.text.len(), |(offset, _)| offset)
    }
}

impl TokenStream for LexicalTokenStream<'_> {
    fn advance(&mut self) -> bool {
        self.token.text.clear();
        self.token.position = self.token.position.wrapping_add(1);
        while let Some((offset_from, character)) = self.chars.next() {
            if character.is_alphanumeric() {
                let offset_to = self.search_token_end();
                self.token.offset_from = offset_from;
                self.token.offset_to = offset_to;
                self.token.text = fold_lexical(&self.text[offset_from..offset_to]);
                return true;
            }
        }
        false
    }

    fn token(&self) -> &Token {
        self.token
    }

    fn token_mut(&mut self) -> &mut Token {
        self.token
    }
}

pub(crate) fn register_lexical_analyzer(index: &Index) {
    let analyzer = TextAnalyzer::builder(LexicalTokenizer::default())
        .filter(RemoveLongFilter::limit(40))
        .build();
    index.tokenizers().register("default", analyzer);
}

#[derive(Debug, Clone)]
pub(crate) struct Fields {
    pub source_key: Option<Field>,
    pub source_property_owner: Field,
    pub chunk_id: Field,
    pub vault_id: Field,
    pub room: Field,
    pub path: Field,
    pub source_format: Field,
    pub source_locator: Field,
    pub extraction_coverage: Field,
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
    pub property_names: Field,
    pub property_paths: Field,
    pub property_exact: Field,
    pub property_path_exact: Field,
    pub property_i64: Field,
    pub property_u64: Field,
    pub property_f64: Field,
    pub property_path_i64: Field,
    pub property_path_u64: Field,
    pub property_path_f64: Field,
    pub property_date: Field,
    pub property_path_date: Field,
    pub property_text_string: Field,
    pub property_text_integer: Field,
    pub property_text_real: Field,
    pub property_text_boolean: Field,
    pub property_text_date: Field,
    pub tags: Field,
    pub tags_text: Field,
    pub links_out: Field,
    pub mtime: Field,
    pub content_hash: Field,
    pub chunking_version: Field,
}

impl Fields {
    pub fn from_schema(schema: &Schema) -> Result<Self> {
        Ok(Self {
            source_key: schema.get_field("source_key").ok(),
            source_property_owner: field(schema, "source_property_owner")?,
            chunk_id: field(schema, "chunk_id")?,
            vault_id: field(schema, "vault_id")?,
            room: field(schema, "room")?,
            path: field(schema, "path")?,
            source_format: field(schema, "source_format")?,
            source_locator: field(schema, "source_locator")?,
            extraction_coverage: field(schema, "extraction_coverage")?,
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
            property_names: field(schema, "property_names")?,
            property_paths: field(schema, "property_paths")?,
            property_exact: field(schema, "property_exact")?,
            property_path_exact: field(schema, "property_path_exact")?,
            property_i64: field(schema, "property_i64")?,
            property_u64: field(schema, "property_u64")?,
            property_f64: field(schema, "property_f64")?,
            property_path_i64: field(schema, "property_path_i64")?,
            property_path_u64: field(schema, "property_path_u64")?,
            property_path_f64: field(schema, "property_path_f64")?,
            property_date: field(schema, "property_date")?,
            property_path_date: field(schema, "property_path_date")?,
            property_text_string: field(schema, "property_text_string")?,
            property_text_integer: field(schema, "property_text_integer")?,
            property_text_real: field(schema, "property_text_real")?,
            property_text_boolean: field(schema, "property_text_boolean")?,
            property_text_date: field(schema, "property_text_date")?,
            tags: field(schema, "tags")?,
            tags_text: field(schema, "tags_text")?,
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
    register_lexical_analyzer(&index);
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
        let (outcomes, enumeration) = ingest_vault_files(vault);
        stats.warnings.extend(enumeration.warnings);
        for outcome in outcomes {
            if let Some(warning) = outcome.warning.clone() {
                stats.warnings.push(warning);
            }
            stats.record_outcome(&outcome);
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
    let manifest_counts = manifest.source_format_counts();
    if manifest_counts != stats.source_format_counts
        || stats.source_format_counts.indexed_documents() != stats.documents
    {
        return Err(Error::State(
            "candidate per-format coverage counts do not match indexed documents or manifest"
                .to_owned(),
        ));
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
    let manifest_counts = manifest.source_format_counts();
    if manifest_counts != stats.source_format_counts
        || stats.source_format_counts.indexed_documents() != stats.documents
    {
        return Err(Error::State(
            "candidate per-format coverage counts do not match indexed documents or manifest"
                .to_owned(),
        ));
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
    register_lexical_analyzer(&index);
    let mut writer = index
        .writer(WRITER_MEMORY_BYTES)
        .map_err(|error| Error::Index(error.to_string()))?;
    let fingerprint = registration_fingerprint(vault);
    let (outcomes, enumeration) = ingest_vault_files(vault);
    stats.warnings.extend(enumeration.warnings);
    for outcome in outcomes {
        if let Some(warning) = outcome.warning.clone() {
            stats.warnings.push(warning);
        }
        stats.record_outcome(&outcome);
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
    register_lexical_analyzer(&index);
    let fields = Fields::from_schema(&index.schema())?;
    Ok((index, fields))
}

pub(crate) fn build_schema() -> Schema {
    let property_text_options = JsonObjectOptions::default().set_indexing_options(
        TextFieldIndexing::default()
            .set_tokenizer("raw")
            .set_index_option(IndexRecordOption::WithFreqsAndPositions),
    );
    let mut builder = Schema::builder();
    builder.add_text_field("source_key", STRING | STORED);
    // Required in the finalized unshipped format-v6 shape. Interim v6 indexes that duplicated the
    // complete property bag per chunk fail schema opening instead of being silently reused.
    builder.add_u64_field("source_property_owner", INDEXED | STORED | FAST);
    builder.add_text_field("chunk_id", STRING | STORED);
    builder.add_text_field("vault_id", STRING | STORED);
    builder.add_text_field("room", STRING | STORED);
    builder.add_text_field("path", STRING | STORED);
    // Extraction metadata is returned after ranking but is never indexed, queried, boosted, or
    // included in BM25 statistics.
    builder.add_text_field("source_format", STORED);
    builder.add_text_field("source_locator", STORED);
    builder.add_text_field("extraction_coverage", STORED);
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
    builder.add_text_field("property_names", STRING);
    builder.add_text_field("property_paths", STRING);
    builder.add_text_field("property_exact", STRING);
    builder.add_text_field("property_path_exact", STRING);
    builder.add_text_field("property_i64", STRING);
    builder.add_text_field("property_u64", STRING);
    builder.add_text_field("property_f64", STRING);
    builder.add_text_field("property_path_i64", STRING);
    builder.add_text_field("property_path_u64", STRING);
    builder.add_text_field("property_path_f64", STRING);
    builder.add_text_field("property_date", STRING);
    builder.add_text_field("property_path_date", STRING);
    builder.add_json_field("property_text_string", property_text_options.clone());
    builder.add_json_field("property_text_integer", property_text_options.clone());
    builder.add_json_field("property_text_real", property_text_options.clone());
    builder.add_json_field("property_text_boolean", property_text_options.clone());
    builder.add_json_field("property_text_date", property_text_options);
    builder.add_text_field("tags", STRING | STORED);
    builder.add_text_field("tags_text", TEXT);
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
    let owns_source_properties = owns_source_properties(prepared)?;
    document.add_u64(
        fields.source_property_owner,
        u64::from(owns_source_properties),
    );
    document.add_text(fields.chunk_id, &chunk.chunk_id);
    document.add_text(fields.vault_id, &chunk.vault_id);
    document.add_text(fields.room, chunk.room.as_deref().unwrap_or_default());
    document.add_text(fields.path, &chunk.path);
    let source_format = prepared.source_format.ok_or_else(|| {
        Error::Index(format!(
            "prepared chunk {} is missing its source format",
            chunk.chunk_id
        ))
    })?;
    let extraction_coverage = prepared.extraction_coverage.ok_or_else(|| {
        Error::Index(format!(
            "prepared chunk {} is missing its extraction coverage",
            chunk.chunk_id
        ))
    })?;
    document.add_text(
        fields.source_format,
        serde_json::to_string(&source_format).map_err(|error| Error::Index(error.to_string()))?,
    );
    document.add_text(
        fields.source_locator,
        serde_json::to_string(&prepared.source_locator)
            .map_err(|error| Error::Index(error.to_string()))?,
    );
    document.add_text(
        fields.extraction_coverage,
        serde_json::to_string(&extraction_coverage)
            .map_err(|error| Error::Index(error.to_string()))?,
    );
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
    let frontmatter = prepared.source_frontmatter.as_ref();
    let title = frontmatter.title().unwrap_or_default();
    document.add_text(fields.title, title);
    add_raw(&mut document, fields.title_raw, title);
    document.add_text(
        required_optional_field(fields.title_exact, "title_exact")?,
        frontmatter.title().unwrap_or_default(),
    );
    document.add_text(
        required_optional_field(fields.description_exact, "description_exact")?,
        frontmatter.description().unwrap_or_default(),
    );
    document.add_text(
        required_optional_field(fields.status_exact, "status_exact")?,
        frontmatter.status().unwrap_or_default(),
    );
    document.add_text(
        required_optional_field(fields.date_exact, "date_exact")?,
        frontmatter.date().unwrap_or_default(),
    );
    document.add_text(fields.content, &chunk.content);
    for identifier in &prepared.technical_identifiers {
        document.add_text(fields.content_identifiers, identifier);
    }
    document.add_text(
        fields.frontmatter,
        serde_json::to_string(frontmatter).map_err(|error| Error::Index(error.to_string()))?,
    );
    if owns_source_properties && source_format != SourceFormat::Canvas {
        add_property_projection(&mut document, fields, &prepared.source_properties);
    }
    for tag in frontmatter.tags() {
        document.add_text(fields.tags, tag);
        document.add_text(fields.tags_text, tag);
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

#[derive(Debug, Default, PartialEq, Eq)]
struct PropertyProjection {
    names: BTreeSet<String>,
    paths: BTreeSet<String>,
    exact: BTreeSet<String>,
    path_exact: BTreeSet<String>,
    i64_values: BTreeSet<String>,
    u64_values: BTreeSet<String>,
    f64_values: BTreeSet<String>,
    path_i64_values: BTreeSet<String>,
    path_u64_values: BTreeSet<String>,
    path_f64_values: BTreeSet<String>,
    date_values: BTreeSet<String>,
    path_date_values: BTreeSet<String>,
    text_string: BTreeMap<String, BTreeSet<String>>,
    text_integer: BTreeMap<String, BTreeSet<String>>,
    text_real: BTreeMap<String, BTreeSet<String>>,
    text_boolean: BTreeMap<String, BTreeSet<String>>,
    text_date: BTreeMap<String, BTreeSet<String>>,
}

fn add_property_projection(
    document: &mut TantivyDocument,
    fields: &Fields,
    properties: &PropertyBag,
) {
    let projection = project_properties(properties);
    add_terms(document, fields.property_names, projection.names);
    add_terms(document, fields.property_paths, projection.paths);
    add_terms(document, fields.property_exact, projection.exact);
    add_terms(document, fields.property_path_exact, projection.path_exact);
    add_terms(document, fields.property_i64, projection.i64_values);
    add_terms(document, fields.property_u64, projection.u64_values);
    add_terms(document, fields.property_f64, projection.f64_values);
    add_terms(
        document,
        fields.property_path_i64,
        projection.path_i64_values,
    );
    add_terms(
        document,
        fields.property_path_u64,
        projection.path_u64_values,
    );
    add_terms(
        document,
        fields.property_path_f64,
        projection.path_f64_values,
    );
    add_terms(document, fields.property_date, projection.date_values);
    add_terms(
        document,
        fields.property_path_date,
        projection.path_date_values,
    );
    add_property_text(
        document,
        fields.property_text_string,
        projection.text_string,
    );
    add_property_text(
        document,
        fields.property_text_integer,
        projection.text_integer,
    );
    add_property_text(document, fields.property_text_real, projection.text_real);
    add_property_text(
        document,
        fields.property_text_boolean,
        projection.text_boolean,
    );
    add_property_text(document, fields.property_text_date, projection.text_date);
}

fn add_property_text(
    document: &mut TantivyDocument,
    field: Field,
    values_by_property: BTreeMap<String, BTreeSet<String>>,
) {
    if values_by_property.is_empty() {
        return;
    }
    let object = values_by_property
        .into_iter()
        .map(|(property, values)| {
            let values = values.into_iter().map(OwnedValue::Str).collect();
            (property, OwnedValue::Array(values))
        })
        .collect();
    document.add_object(field, object);
}

fn add_terms(document: &mut TantivyDocument, field: Field, terms: BTreeSet<String>) {
    for term in terms {
        document.add_text(field, term);
    }
}

fn project_properties(properties: &PropertyBag) -> PropertyProjection {
    let mut projection = PropertyProjection::default();
    for (name, value) in properties.iter() {
        let property = property_name_term(name);
        projection.names.insert(property.clone());
        project_property_value(name, &property, "", value, &mut projection);
    }
    projection
}

fn project_property_value(
    name: &str,
    property: &str,
    path: &str,
    value: &PropertyValue,
    projection: &mut PropertyProjection,
) {
    match value {
        PropertyValue::Sequence(values) => {
            for (index, value) in values.iter().enumerate() {
                project_property_value(
                    name,
                    property,
                    &format!("{path}/{index}"),
                    value,
                    projection,
                );
            }
        }
        PropertyValue::Map(values) => {
            for (component, value) in values {
                project_property_value(
                    name,
                    property,
                    &format!("{path}/{}", escape_json_pointer_component(component)),
                    value,
                    projection,
                );
            }
        }
        scalar => project_property_scalar(name, property, path, scalar, projection),
    }
}

fn project_property_scalar(
    name: &str,
    property: &str,
    path: &str,
    value: &PropertyValue,
    projection: &mut PropertyProjection,
) {
    let path_key = property_path_term(name, path);
    projection.paths.insert(path_key.clone());
    projection.exact.insert(property_exact_term(name, value));
    projection
        .path_exact
        .insert(property_path_exact_term(name, path, value));

    match value {
        PropertyValue::Bool(value) => {
            insert_property_text(&mut projection.text_boolean, property, value.to_string());
        }
        PropertyValue::I64(value) => {
            projection
                .i64_values
                .insert(property_i64_term(name, *value));
            projection
                .path_i64_values
                .insert(property_path_i64_term(name, path, *value));
            insert_property_text(&mut projection.text_integer, property, value.to_string());
        }
        PropertyValue::U64(value) => {
            projection
                .u64_values
                .insert(property_u64_term(name, *value));
            projection
                .path_u64_values
                .insert(property_path_u64_term(name, path, *value));
            insert_property_text(&mut projection.text_integer, property, value.to_string());
        }
        PropertyValue::F64(value) if value.is_finite() => {
            projection
                .f64_values
                .insert(property_f64_term(name, *value));
            projection
                .path_f64_values
                .insert(property_path_f64_term(name, path, *value));
            insert_property_text(&mut projection.text_real, property, value.to_string());
        }
        PropertyValue::String(value) if is_iso_calendar_date(value) => {
            projection
                .date_values
                .insert(property_date_term(name, value));
            projection
                .path_date_values
                .insert(property_path_date_term(name, path, value));
            insert_property_text(&mut projection.text_date, property, value.clone());
        }
        PropertyValue::String(value) => {
            insert_property_text(&mut projection.text_string, property, value.clone());
        }
        PropertyValue::Null
        | PropertyValue::F64(_)
        | PropertyValue::Sequence(_)
        | PropertyValue::Map(_) => {}
    }
}

fn insert_property_text(
    projection: &mut BTreeMap<String, BTreeSet<String>>,
    property: &str,
    value: String,
) {
    let values = projection.entry(property.to_owned()).or_default();
    let mut tokenizer = SimpleTokenizer::default();
    let mut stream = tokenizer.token_stream(&value);
    while stream.advance() {
        values.insert(property_text_term(&stream.token().text));
    }
}

fn property_text_term(value: &str) -> String {
    let normalized = value.to_lowercase();
    if normalized.len() <= MAX_RAW_PROPERTY_TOKEN_BYTES {
        normalized
    } else {
        format!("kwiry-long-{:x}", Sha256::digest(normalized.as_bytes()))
    }
}

pub(crate) fn property_name_term(name: &str) -> String {
    hash_components(b"kwiry-property-name-v1\0", &[name.as_bytes()])
}

pub(crate) fn property_path_term(name: &str, path: &str) -> String {
    let property = property_name_term(name);
    hash_components(
        b"kwiry-property-path-v1\0",
        &[property.as_bytes(), path.as_bytes()],
    )
}

pub(crate) fn property_exact_term(name: &str, value: &PropertyValue) -> String {
    property_exact_term_for_key(&property_name_term(name), value)
}

pub(crate) fn property_path_exact_term(name: &str, path: &str, value: &PropertyValue) -> String {
    property_exact_term_for_key(&property_path_term(name, path), value)
}

fn property_exact_term_for_key(key: &str, value: &PropertyValue) -> String {
    let (kind, bytes) = scalar_identity(value);
    hash_components(
        b"kwiry-property-exact-v1\0",
        &[key.as_bytes(), kind, &bytes],
    )
}

pub(crate) fn property_i64_term(name: &str, value: i64) -> String {
    property_i64_term_for_key(&property_name_term(name), value)
}

pub(crate) fn property_path_i64_term(name: &str, path: &str, value: i64) -> String {
    property_i64_term_for_key(&property_path_term(name, path), value)
}

fn property_i64_term_for_key(key: &str, value: i64) -> String {
    format!("{key}:{:016x}", (value as u64) ^ (1_u64 << 63))
}

pub(crate) fn property_u64_term(name: &str, value: u64) -> String {
    property_u64_term_for_key(&property_name_term(name), value)
}

pub(crate) fn property_path_u64_term(name: &str, path: &str, value: u64) -> String {
    property_u64_term_for_key(&property_path_term(name, path), value)
}

fn property_u64_term_for_key(key: &str, value: u64) -> String {
    format!("{key}:{value:016x}")
}

pub(crate) fn property_f64_term(name: &str, value: f64) -> String {
    property_f64_term_for_key(&property_name_term(name), value)
}

pub(crate) fn property_path_f64_term(name: &str, path: &str, value: f64) -> String {
    property_f64_term_for_key(&property_path_term(name, path), value)
}

fn property_f64_term_for_key(key: &str, value: f64) -> String {
    let bits = value.to_bits();
    let sortable = if bits & (1_u64 << 63) == 0 {
        bits ^ (1_u64 << 63)
    } else {
        !bits
    };
    format!("{key}:{sortable:016x}")
}

pub(crate) fn property_date_term(name: &str, value: &str) -> String {
    property_date_term_for_key(&property_name_term(name), value)
}

pub(crate) fn property_path_date_term(name: &str, path: &str, value: &str) -> String {
    property_date_term_for_key(&property_path_term(name, path), value)
}

fn property_date_term_for_key(key: &str, value: &str) -> String {
    format!("{key}:{value}")
}

fn scalar_identity(value: &PropertyValue) -> (&'static [u8], Vec<u8>) {
    match value {
        PropertyValue::Null => (b"null", Vec::new()),
        PropertyValue::Bool(value) => (b"bool", vec![u8::from(*value)]),
        PropertyValue::I64(value) => (b"i64", value.to_le_bytes().to_vec()),
        PropertyValue::U64(value) => (b"u64", value.to_le_bytes().to_vec()),
        PropertyValue::F64(value) => (b"real", value.to_bits().to_le_bytes().to_vec()),
        PropertyValue::String(value) if is_iso_calendar_date(value) => {
            (b"date", value.as_bytes().to_vec())
        }
        PropertyValue::String(value) => (b"string", value.as_bytes().to_vec()),
        PropertyValue::Sequence(_) | PropertyValue::Map(_) => {
            unreachable!("recursive property values are flattened before scalar projection")
        }
    }
}

fn is_iso_calendar_date(value: &str) -> bool {
    let bytes = value.as_bytes();
    if bytes.len() != 10
        || bytes[4] != b'-'
        || bytes[7] != b'-'
        || !bytes
            .iter()
            .enumerate()
            .all(|(index, byte)| matches!(index, 4 | 7) || byte.is_ascii_digit())
    {
        return false;
    }
    let year = decimal_component(&bytes[0..4]);
    let month = decimal_component(&bytes[5..7]);
    let day = decimal_component(&bytes[8..10]);
    if !(1..=12).contains(&month) || day == 0 {
        return false;
    }
    let leap = year.is_multiple_of(4) && (!year.is_multiple_of(100) || year.is_multiple_of(400));
    let days = [
        31,
        if leap { 29 } else { 28 },
        31,
        30,
        31,
        30,
        31,
        31,
        30,
        31,
        30,
        31,
    ];
    day <= days[(month - 1) as usize]
}

fn decimal_component(bytes: &[u8]) -> u32 {
    bytes
        .iter()
        .fold(0, |value, byte| value * 10 + u32::from(byte - b'0'))
}

fn hash_components(domain: &[u8], components: &[&[u8]]) -> String {
    let mut digest = Sha256::new();
    digest.update(domain);
    for component in components {
        update_digest_component(&mut digest, component);
    }
    format!("{:x}", digest.finalize())
}

fn escape_json_pointer_component(component: &str) -> String {
    component.replace('~', "~0").replace('/', "~1")
}

// Source-level property signals belong to chunk zero only. Reusing the stable chunk-id contract
// avoids repeating note-authored metadata across every chunk while keeping incremental writers,
// which receive chunks one at a time, on the same projection rule as full rebuilds.
fn owns_source_properties(prepared: &PreparedChunk) -> Result<bool> {
    let chunk = &prepared.chunk;
    let heading_json =
        serde_json::to_vec(&chunk.heading_path).map_err(|error| Error::Index(error.to_string()))?;
    let mut digest = Sha256::new();
    digest.update(b"kwiry-chunk-v1\0");
    update_digest_component(&mut digest, chunk.vault_id.as_bytes());
    update_digest_component(&mut digest, chunk.path.as_bytes());
    update_digest_component(&mut digest, &heading_json);
    digest.update(0_u64.to_le_bytes());
    Ok(chunk.chunk_id == format!("{:x}", digest.finalize()))
}

fn update_digest_component(digest: &mut Sha256, bytes: &[u8]) {
    digest.update((bytes.len() as u64).to_le_bytes());
    digest.update(bytes);
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
    use std::collections::BTreeMap;
    use std::fs;

    use tantivy::Term;
    use tantivy::collector::{Count, DocSetCollector};
    use tantivy::query::{AllQuery, QueryParser, TermQuery};
    use tantivy::schema::Value;
    use tempfile::tempdir;

    use super::*;
    use crate::api::SearchFilters;
    use crate::extract::{ExtractionCoverage, SourceLocator};
    use crate::format::SourceFormat;
    use crate::model::VaultRegistration;
    use crate::search::search_reader;
    use crate::source::{SourceDescriptor, prepare_source_buffer};
    use crate::{LexicalSearchRequest, search_index};

    #[test]
    fn property_projection_preserves_types_paths_and_source_level_deduplication() {
        let frontmatter = PropertyBag::from_properties(BTreeMap::from([
            (
                "mixed".to_owned(),
                PropertyValue::Sequence(vec![
                    PropertyValue::I64(7),
                    PropertyValue::String("7".to_owned()),
                    PropertyValue::I64(7),
                    PropertyValue::Map(BTreeMap::from([(
                        "a/b~".to_owned(),
                        PropertyValue::Bool(true),
                    )])),
                ]),
            ),
            ("nothing".to_owned(), PropertyValue::Null),
            (
                "reviewed".to_owned(),
                PropertyValue::String("2026-07-29".to_owned()),
            ),
        ]));

        let projection = project_properties(&frontmatter);
        let mixed = property_name_term("mixed");

        assert_eq!(projection.names.len(), 3);
        assert!(projection.names.contains(&mixed));
        assert_eq!(
            projection.i64_values,
            [property_i64_term("mixed", 7)].into()
        );
        assert!(
            projection
                .exact
                .contains(&property_exact_term("mixed", &PropertyValue::I64(7)))
        );
        assert!(projection.exact.contains(&property_exact_term(
            "mixed",
            &PropertyValue::String("7".to_owned())
        )));
        assert_ne!(
            property_exact_term("mixed", &PropertyValue::I64(7)),
            property_exact_term("mixed", &PropertyValue::String("7".to_owned()))
        );
        assert!(
            projection
                .exact
                .contains(&property_exact_term("mixed", &PropertyValue::Bool(true)))
        );
        assert!(
            projection
                .paths
                .contains(&property_path_term("mixed", "/3/a~1b~0"))
        );
        assert_eq!(
            projection.text_string.get(&mixed),
            Some(&BTreeSet::from(["7".to_owned()]))
        );
        assert_eq!(
            projection.text_integer.get(&mixed),
            Some(&BTreeSet::from(["7".to_owned()]))
        );
        assert!(
            projection
                .date_values
                .contains(&property_date_term("reviewed", "2026-07-29"))
        );
        assert!(
            projection
                .paths
                .contains(&property_path_term("reviewed", ""))
        );
        assert!(projection.path_exact.contains(&property_path_exact_term(
            "reviewed",
            "",
            &PropertyValue::String("2026-07-29".to_owned())
        )));
        assert!(
            !projection
                .text_string
                .contains_key(&property_name_term("reviewed"))
        );
        assert_eq!(
            projection.text_date.get(&property_name_term("reviewed")),
            Some(&BTreeSet::from([
                "07".to_owned(),
                "2026".to_owned(),
                "29".to_owned()
            ]))
        );
    }

    #[test]
    fn property_projection_does_not_invent_cardinality_or_value_limits() {
        let mut properties = (0..1_000)
            .map(|index| {
                (
                    format!("property_{index}"),
                    PropertyValue::String(format!("value_{index}")),
                )
            })
            .collect::<BTreeMap<_, _>>();
        properties.insert(
            "items".to_owned(),
            PropertyValue::Sequence(
                (0..1_200)
                    .map(|index| PropertyValue::I64(index.into()))
                    .collect(),
            ),
        );
        properties.insert(
            "payload".to_owned(),
            PropertyValue::String("x".repeat(128 * 1_024)),
        );

        let projection = project_properties(&PropertyBag::from_properties(properties));

        assert_eq!(projection.names.len(), 1_002);
        assert_eq!(projection.i64_values.len(), 1_200);
        assert_eq!(
            projection.text_string.get(&property_name_term("payload")),
            Some(&BTreeSet::from([property_text_term(
                &"x".repeat(128 * 1_024)
            )]))
        );
    }

    #[test]
    fn calendar_dates_match_the_sqlite_projection_boundary() {
        assert!(is_iso_calendar_date("2024-02-29"));
        assert!(is_iso_calendar_date("2026-07-29"));
        assert!(!is_iso_calendar_date("2025-02-29"));
        assert!(!is_iso_calendar_date("2026-13-01"));
        assert!(!is_iso_calendar_date("2026-7-29"));
        assert_eq!(
            scalar_identity(&PropertyValue::String("2026-07-29".to_owned())).0,
            b"date"
        );
        assert_eq!(
            scalar_identity(&PropertyValue::String("not-a-date".to_owned())).0,
            b"string"
        );
    }

    #[test]
    fn sortable_numeric_terms_preserve_each_native_type_order() {
        assert!(property_i64_term("score", -2) < property_i64_term("score", 0));
        assert!(property_i64_term("score", 0) < property_i64_term("score", 9));
        assert!(property_u64_term("score", 2) < property_u64_term("score", 10));
        assert!(property_f64_term("score", -2.5) < property_f64_term("score", -0.0));
        assert!(property_f64_term("score", -0.0) < property_f64_term("score", 0.0));
        assert!(property_f64_term("score", 0.0) < property_f64_term("score", 9.5));
    }

    #[test]
    fn frontmatter_only_source_indexes_its_property_fields() {
        let body = "---\npriority: 7\n---\n";
        let descriptor = SourceDescriptor {
            vault_id: "fixture".to_owned(),
            room: None,
            path: "properties-only.md".to_owned(),
            format: SourceFormat::Markdown,
            byte_length: body.len() as u64,
            mtime: 1,
            mtime_nanos: 1,
        };
        let preparation = prepare_source_buffer(&descriptor, body.as_bytes()).unwrap();
        assert_eq!(preparation.chunks.len(), 1);
        assert!(preparation.chunks[0].content.is_empty());

        let schema = build_schema();
        let fields = Fields::from_schema(&schema).unwrap();
        let index = Index::create_in_ram(schema);
        register_lexical_analyzer(&index);
        let mut writer = index.writer(WRITER_MEMORY_BYTES).unwrap();
        writer
            .add_document(
                chunk_document(&fields, &preparation.chunks[0], &preparation.retrieval).unwrap(),
            )
            .unwrap();
        writer.commit().unwrap();
        let searcher = index.reader().unwrap().searcher();
        let presence = TermQuery::new(
            Term::from_field_text(fields.property_names, &property_name_term("priority")),
            IndexRecordOption::Basic,
        );

        assert_eq!(searcher.search(&presence, &Count).unwrap(), 1);
    }

    #[test]
    fn indexed_property_fields_match_once_for_a_multi_chunk_source() {
        const LONG_TOKEN: &str = "abcdefghijklmnopqrstuvwxyzabcdefghijklmnopqrstuv";
        let body = format!(
            "---\npriority: 7\nsummary: Needle phrase\nlong_token: {LONG_TOKEN}\n---\n# Body\n{}",
            "word ".repeat(2_000)
        );
        let descriptor = SourceDescriptor {
            vault_id: "fixture".to_owned(),
            room: None,
            path: "note.md".to_owned(),
            format: SourceFormat::Markdown,
            byte_length: body.len() as u64,
            mtime: 1,
            mtime_nanos: 1,
        };
        let preparation = prepare_source_buffer(&descriptor, body.as_bytes()).unwrap();
        assert!(preparation.chunks.len() > 1);
        assert!(owns_source_properties(&preparation.chunks[0]).unwrap());
        assert!(
            preparation.chunks[1..]
                .iter()
                .all(|chunk| !owns_source_properties(chunk).unwrap())
        );

        let schema = build_schema();
        let fields = Fields::from_schema(&schema).unwrap();
        let index = Index::create_in_ram(schema);
        register_lexical_analyzer(&index);
        let mut writer = index.writer(WRITER_MEMORY_BYTES).unwrap();
        for chunk in &preparation.chunks {
            writer
                .add_document(chunk_document(&fields, chunk, &preparation.retrieval).unwrap())
                .unwrap();
        }
        writer.commit().unwrap();
        let reader = index.reader().unwrap();
        let searcher = reader.searcher();

        let presence = TermQuery::new(
            Term::from_field_text(fields.property_names, &property_name_term("priority")),
            IndexRecordOption::Basic,
        );
        let exact = TermQuery::new(
            Term::from_field_text(
                fields.property_exact,
                &property_exact_term("priority", &PropertyValue::I64(7)),
            ),
            IndexRecordOption::Basic,
        );
        let numeric = TermQuery::new(
            Term::from_field_text(fields.property_i64, &property_i64_term("priority", 7)),
            IndexRecordOption::Basic,
        );
        assert_eq!(searcher.search(&presence, &Count).unwrap(), 1);
        assert_eq!(searcher.search(&exact, &Count).unwrap(), 1);
        assert_eq!(searcher.search(&numeric, &Count).unwrap(), 1);

        let parser = QueryParser::for_index(&index, vec![fields.property_text_string]);
        let text = parser
            .parse_query(&format!(
                "property_text_string.{}:needle",
                property_name_term("summary")
            ))
            .unwrap();
        assert_eq!(searcher.search(&text, &Count).unwrap(), 1);

        let parser = QueryParser::for_index(&index, vec![fields.property_text_integer]);
        let numeric_text = parser
            .parse_query(&format!(
                "property_text_integer.{}:7",
                property_name_term("priority")
            ))
            .unwrap();
        assert_eq!(searcher.search(&numeric_text, &Count).unwrap(), 1);

        let parser = QueryParser::for_index(&index, vec![fields.property_text_string]);
        let long_text = parser
            .parse_query(&format!(
                "property_text_string.{}:{}",
                property_name_term("long_token"),
                property_text_term(LONG_TOKEN)
            ))
            .unwrap();
        assert_eq!(searcher.search(&long_text, &Count).unwrap(), 1);
    }

    #[test]
    fn native_documents_store_only_compact_frontmatter_and_one_property_projection() {
        let payload = "tantivy-property-payload-".repeat(11_000);
        let body = "bodyword ".repeat(120_000);
        let source = format!("---\npayload: {payload}\n---\n{body}");
        let descriptor = SourceDescriptor {
            vault_id: "fixture".to_owned(),
            room: None,
            path: "amplification.md".to_owned(),
            format: SourceFormat::Markdown,
            byte_length: source.len() as u64,
            mtime: 1,
            mtime_nanos: 1,
        };
        let preparation = prepare_source_buffer(&descriptor, source.as_bytes()).unwrap();
        assert!(preparation.chunks.len() > 250);

        let schema = build_schema();
        let fields = Fields::from_schema(&schema).unwrap();
        let index = Index::create_in_ram(schema);
        register_lexical_analyzer(&index);
        let mut writer = index.writer(WRITER_MEMORY_BYTES).unwrap();
        for chunk in &preparation.chunks {
            writer
                .add_document(chunk_document(&fields, chunk, &preparation.retrieval).unwrap())
                .unwrap();
        }
        writer.commit().unwrap();
        let searcher = index.reader().unwrap().searcher();

        let presence = TermQuery::new(
            Term::from_field_text(fields.property_names, &property_name_term("payload")),
            IndexRecordOption::Basic,
        );
        assert_eq!(searcher.search(&presence, &Count).unwrap(), 1);
        let owners = TermQuery::new(
            Term::from_field_u64(fields.source_property_owner, 1),
            IndexRecordOption::Basic,
        );
        assert_eq!(searcher.search(&owners, &Count).unwrap(), 1);

        let addresses = searcher.search(&AllQuery, &DocSetCollector).unwrap();
        assert_eq!(addresses.len(), preparation.chunks.len());
        for address in addresses {
            let document = searcher.doc::<TantivyDocument>(address).unwrap();
            let stored = document
                .get_first(fields.frontmatter)
                .and_then(|value| value.as_str())
                .expect("stored compact frontmatter");
            assert_eq!(stored, "{}");
            assert!(!stored.contains(&payload));
        }
    }

    #[test]
    fn base_fixture_projects_through_tantivy_with_stored_locator_and_excerpt() {
        let temporary = tempdir().unwrap();
        let vault_path = temporary.path().join("vault");
        let index_path = temporary.path().join("index");
        fs::create_dir(&vault_path).unwrap();
        fs::write(
            vault_path.join("project-dashboard.base"),
            include_bytes!("../tests/fixtures/base/well-formed.base"),
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

        let stats = build_index(&config, &index_path).unwrap();
        assert_eq!(stats.documents, 1);
        assert_eq!(stats.chunks, 4);

        let gallery = search_index(
            &index_path,
            &LexicalSearchRequest {
                query: "Gallery".into(),
                limit: 20,
                vault_id: None,
            },
        )
        .unwrap();
        assert_eq!(gallery.len(), 1);
        assert_eq!(gallery[0].format, SourceFormat::Base);
        assert_eq!(gallery[0].coverage, ExtractionCoverage::IndexedComplete);
        assert_eq!(
            gallery[0].locator,
            Some(SourceLocator::BaseView {
                view: "Gallery".to_owned()
            })
        );
        assert!(gallery[0].excerpt.chars().count() <= 241);
        assert!(gallery[0].excerpt.contains("Gallery") || gallery[0].excerpt.contains("cover"));

        let configuration = search_index(
            &index_path,
            &LexicalSearchRequest {
                query: "file.inFolder".into(),
                limit: 20,
                vault_id: None,
            },
        )
        .unwrap();
        assert_eq!(configuration.len(), 1);
        assert_eq!(configuration[0].format, SourceFormat::Base);
        assert_eq!(configuration[0].locator, None);
    }

    #[test]
    fn canvas_fixture_projects_authored_text_through_tantivy_without_structural_signals() {
        let temporary = tempdir().unwrap();
        let vault_path = temporary.path().join("vault");
        let index_path = temporary.path().join("index");
        fs::create_dir(&vault_path).unwrap();
        fs::write(
            vault_path.join("research.canvas"),
            include_bytes!("../tests/fixtures/canvas/well-formed.canvas"),
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

        let stats = build_index(&config, &index_path).unwrap();
        assert_eq!(stats.documents, 1);
        assert_eq!(stats.chunks, 9);

        for query in [
            "Alpha body",
            "Research Cluster",
            "example canvas source",
            "References target",
            "Only Authored Subpath",
            "supports source",
            "resolves into",
        ] {
            let hits = search_index(
                &index_path,
                &LexicalSearchRequest {
                    query: query.to_owned(),
                    limit: 20,
                    vault_id: None,
                },
            )
            .unwrap();
            assert!(
                !hits.is_empty(),
                "authored Canvas query must match: {query}"
            );
            assert!(hits.iter().all(|hit| {
                hit.path == "research.canvas"
                    && hit.format == SourceFormat::Canvas
                    && hit.coverage == ExtractionCoverage::IndexedComplete
                    && hit.locator.is_none()
            }));
        }

        for structural_query in [
            "1111111111111111",
            "aaaaaaaaaaaaaaaa",
            "geometrysentinelqzx",
            "18446744073709551615",
        ] {
            assert!(
                search_index(
                    &index_path,
                    &LexicalSearchRequest {
                        query: structural_query.to_owned(),
                        limit: 20,
                        vault_id: None,
                    },
                )
                .unwrap()
                .is_empty(),
                "Canvas structural value must not be lexical evidence: {structural_query}"
            );
        }
    }

    #[test]
    fn canvas_properties_are_not_projected_into_native_property_fields() {
        let source = include_bytes!("../tests/fixtures/canvas/well-formed.canvas");
        let preparation = prepare_source_buffer(
            &SourceDescriptor {
                vault_id: "fixture".to_owned(),
                room: None,
                path: "research.canvas".to_owned(),
                format: SourceFormat::Canvas,
                byte_length: source.len() as u64,
                mtime: 1,
                mtime_nanos: 1,
            },
            source,
        )
        .unwrap();
        let schema = build_schema();
        let fields = Fields::from_schema(&schema).unwrap();
        let index = Index::create_in_ram(schema);
        register_lexical_analyzer(&index);
        let mut writer = index.writer(WRITER_MEMORY_BYTES).unwrap();
        for chunk in &preparation.chunks {
            writer
                .add_document(chunk_document(&fields, chunk, &preparation.retrieval).unwrap())
                .unwrap();
        }
        writer.commit().unwrap();
        let searcher = index.reader().unwrap().searcher();

        let property_name = TermQuery::new(
            Term::from_field_text(fields.property_names, &property_name_term("canvas")),
            IndexRecordOption::Basic,
        );
        let structural_id = TermQuery::new(
            Term::from_field_text(
                fields.property_exact,
                &property_exact_term(
                    "canvas",
                    &PropertyValue::String("1111111111111111".to_owned()),
                ),
            ),
            IndexRecordOption::Basic,
        );
        let owners = TermQuery::new(
            Term::from_field_u64(fields.source_property_owner, 1),
            IndexRecordOption::Basic,
        );
        assert_eq!(searcher.search(&property_name, &Count).unwrap(), 0);
        assert_eq!(searcher.search(&structural_id, &Count).unwrap(), 0);
        assert_eq!(searcher.search(&owners, &Count).unwrap(), 1);
    }

    #[test]
    fn native_index_stats_account_for_every_persisted_format_coverage() {
        let temporary = tempdir().unwrap();
        let vault_path = temporary.path().join("vault");
        let data_path = temporary.path().join("data");
        fs::create_dir(&vault_path).unwrap();
        fs::write(vault_path.join("complete.md"), "# Complete\nbody").unwrap();
        fs::write(vault_path.join("unreadable.md"), [0xff]).unwrap();
        fs::write(
            vault_path.join("partial.base"),
            "filters: status == \"active\"\nviews:\n  - type: table\n  - not-a-view\n",
        )
        .unwrap();
        fs::write(vault_path.join("empty.base"), "{}").unwrap();
        fs::write(vault_path.join("broken.base"), "not: [valid").unwrap();
        fs::write(
            vault_path.join("complete.canvas"),
            include_bytes!("../tests/fixtures/canvas/well-formed.canvas"),
        )
        .unwrap();
        fs::write(
            vault_path.join("partial.canvas"),
            include_bytes!("../tests/fixtures/canvas/partial.canvas"),
        )
        .unwrap();
        fs::write(
            vault_path.join("empty.canvas"),
            include_bytes!("../tests/fixtures/canvas/empty.canvas"),
        )
        .unwrap();
        fs::write(
            vault_path.join("broken.canvas"),
            include_bytes!("../tests/fixtures/canvas/malformed.canvas"),
        )
        .unwrap();
        fs::write(vault_path.join("unreadable.canvas"), [0xff]).unwrap();
        let config = Config {
            vaults: vec![VaultRegistration {
                id: "fixture".into(),
                path: vault_path,
                room: None,
            }],
            ..Config::default()
        };

        let stats = build_index(&config, &data_path).unwrap();

        assert_eq!(stats.documents, 4);
        assert_eq!(stats.source_format_counts.indexed_documents(), 4);
        assert_eq!(stats.source_format_counts.total_sources(), 10);
        assert_eq!(stats.source_format_counts.markdown.indexed_complete, 1);
        assert_eq!(stats.source_format_counts.markdown.unreadable, 1);
        assert_eq!(stats.source_format_counts.base.indexed_partial, 1);
        assert_eq!(stats.source_format_counts.base.quarantined, 1);
        assert_eq!(
            stats.source_format_counts.base.skipped_no_extractable_text,
            1
        );
        assert_eq!(stats.source_format_counts.canvas.indexed_complete, 1);
        assert_eq!(stats.source_format_counts.canvas.indexed_partial, 1);
        assert_eq!(stats.source_format_counts.canvas.unreadable, 1);
        assert_eq!(stats.source_format_counts.canvas.quarantined, 1);
        assert_eq!(
            stats
                .source_format_counts
                .canvas
                .skipped_no_extractable_text,
            1
        );
    }

    #[test]
    fn stored_source_format_is_ranking_neutral_under_mutation() {
        fn preparation(path: &str, format: SourceFormat) -> crate::source::SourcePreparation {
            let body = b"neutral evidence";
            prepare_source_buffer(
                &SourceDescriptor {
                    vault_id: "fixture".to_owned(),
                    room: None,
                    path: path.to_owned(),
                    format,
                    byte_length: body.len() as u64,
                    mtime: 1,
                    mtime_nanos: 1,
                },
                body,
            )
            .unwrap()
        }

        fn ranked(
            first_format: SourceFormat,
            second_format: SourceFormat,
        ) -> Vec<crate::model::SearchHit> {
            let mut first = preparation("alpha.md", SourceFormat::Markdown);
            let mut second = preparation("beta.txt", SourceFormat::Text);
            first.chunks[0].source_format = Some(first_format);
            second.chunks[0].source_format = Some(second_format);

            let schema = build_schema();
            let fields = Fields::from_schema(&schema).unwrap();
            let index = Index::create_in_ram(schema);
            register_lexical_analyzer(&index);
            let mut writer = index.writer(WRITER_MEMORY_BYTES).unwrap();
            for source in [&first, &second] {
                writer
                    .add_document(
                        chunk_document(&fields, &source.chunks[0], &source.retrieval).unwrap(),
                    )
                    .unwrap();
            }
            writer.commit().unwrap();
            let reader = index.reader().unwrap();
            search_reader(
                &index,
                &fields,
                &reader,
                "content:neutral",
                20,
                &SearchFilters::default(),
            )
            .unwrap()
        }

        let baseline = ranked(SourceFormat::Markdown, SourceFormat::Text);
        assert_eq!(baseline.len(), 2);
        assert_eq!(baseline[0].score, baseline[1].score);

        let mutated = ranked(SourceFormat::Canvas, SourceFormat::Markdown);
        assert_eq!(
            baseline
                .iter()
                .map(|hit| (&hit.chunk_id, hit.score))
                .collect::<Vec<_>>(),
            mutated
                .iter()
                .map(|hit| (&hit.chunk_id, hit.score))
                .collect::<Vec<_>>()
        );
        let baseline_formats = baseline
            .iter()
            .map(|hit| (hit.path.as_str(), hit.format))
            .collect::<BTreeMap<_, _>>();
        let mutated_formats = mutated
            .iter()
            .map(|hit| (hit.path.as_str(), hit.format))
            .collect::<BTreeMap<_, _>>();
        assert_eq!(baseline_formats["alpha.md"], SourceFormat::Markdown);
        assert_eq!(baseline_formats["beta.txt"], SourceFormat::Text);
        assert_eq!(mutated_formats["alpha.md"], SourceFormat::Canvas);
        assert_eq!(mutated_formats["beta.txt"], SourceFormat::Markdown);
    }

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
        let request = LexicalSearchRequest {
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
            r#"{"layout_version":1,"index_format_version":10,"generation":"old"}"#,
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
                &LexicalSearchRequest {
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
            &LexicalSearchRequest {
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
            &LexicalSearchRequest {
                query: "phosphorescent".into(),
                limit: 20,
                vault_id: None,
            },
        )
        .unwrap();
        assert_eq!(before, after);
    }
}
