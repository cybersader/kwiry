// SPDX-License-Identifier: MIT OR Apache-2.0
//! Experimental push-fed search provider store (trial only).
//!
//! A host feeds notes into named partitions and asks which notes might
//! contain some words, or which notes are nearest in meaning. Notes are
//! prepared with the same source preparation, chunking and lexical analyser
//! as the daemon, and ranked by the same lexical-v2 ladder; answers are note
//! references only.
//!
//! Storage is separate from the daemon's generation machinery on purpose:
//! each partition is one disposable Tantivy index (plus a small body-words
//! index for the lexical recall obligation), updated in place by a `delta`
//! feed and rebuilt by a `full` feed. Writes become durable at a checkpoint
//! and visible at the commit after it. When an embedding model is loaded,
//! every commit is also embedded in the background into the partition's own
//! vector file; that kind has its own state and `asOf`. Nothing here reads a
//! workspace on disk and nothing is kept across restarts.
//!
//! Desktop and non-governed: no access control, no member identity, no
//! excerpt or score on the way out. See `docs/provider-helper-trial.md`.

mod body_words;
#[cfg(feature = "semantic-onnx")]
mod pinned_model;
mod unicode_tables;
mod vectors;

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, RwLock, RwLockReadGuard, mpsc};

use rusqlite::Connection;
use sha2::{Digest, Sha256};
use tantivy::collector::{Collector, DocSetCollector, SegmentCollector};
use tantivy::columnar::Column;
use tantivy::query::{BooleanQuery, Occur, Query, TermQuery};
use tantivy::schema::{
    FAST, Field, INDEXED, IndexRecordOption, Schema, TextFieldIndexing, TextOptions, Value,
};
use tantivy::tokenizer::{PreTokenizedString, Token};
use tantivy::{
    DocId, Index, IndexReader, IndexWriter, ReloadPolicy, Score, Searcher, SegmentOrdinal,
    SegmentReader, TantivyDocument, Term,
};

use crate::api::SearchFilters;
use crate::extract::ExtractionCoverage;
use crate::format::SourceFormat;
use crate::index::{Fields, build_schema, chunk_document, register_lexical_analyzer};
use crate::model::MAX_FILE_BYTES;
use crate::query::MAX_TOTAL_CANDIDATES;
use crate::search::{AuthorizedStatistics, PinnedSearcher, search_pinned_with_quality};
use crate::semantic::{Embedder, embedding_text, rrf_fuse};
use crate::source::{
    SourceDescriptor, SourcePreparationKind, prepare_source_buffer, room_source_key,
};

#[cfg(feature = "semantic-onnx")]
pub use pinned_model::{PINNED_MODEL_FILES, PINNED_MODEL_REVISION, load_pinned_embedder};

/// Largest note body accepted: the daemon's per-file cap (`MAX_FILE_BYTES`).
pub const PROVIDER_MAX_ITEM_BYTES: u64 = MAX_FILE_BYTES;
pub const PROVIDER_MAX_PARTITION_BYTES: usize = 64;
pub const PROVIDER_MAX_ID_BYTES: usize = 256;
pub const PROVIDER_MAX_PATH_BYTES: usize = 1024;
pub const PROVIDER_MAX_LIMIT: usize = 200;
pub const PROVIDER_MAX_QUERY_PARTITIONS: usize = 64;
/// Most `terms` one query may carry (the host's query grammar allows 128).
pub const PROVIDER_MAX_TERMS: usize = 128;
/// The body-words matcher this provider honours for the lexical recall
/// obligation.
pub const PROVIDER_MATCHER: &str = body_words::MATCHER_VERSION;
/// Unicode data used for the matcher's NFD, not its pinned category/fold tables.
pub const PROVIDER_MATCHER_UNICODE_VERSION: (u8, u8, u8) = unicode_normalization::UNICODE_VERSION;
/// Exclusion reason for a note refused by the transport before its body was
/// read.
pub const PROVIDER_OVER_TRANSPORT_CAP: &str = "over_transport_cap";

const WRITER_MEMORY_BYTES: usize = 50_000_000;
const WORDS_WRITER_MEMORY_BYTES: usize = 20_000_000;
const DATA_MARKER: &str = ".provider-store";
/// How often a query re-runs when a commit lands between its search and its
/// reference lookup.
const QUERY_ATTEMPTS: usize = 4;
/// Candidates each side contributes to a hybrid fusion (the daemon's figure).
const HYBRID_CANDIDATES: usize = 100;
/// Reciprocal-rank-fusion constant (the daemon's figure).
const RRF_K: f64 = 60.0;
/// Bound both the number and total bytes of passage texts per model call
/// (one unusually large chunk can occupy a batch by itself).
const EMBED_CHUNKS_PER_BATCH: usize = 128;
const EMBED_BYTES_PER_BATCH: usize = 1 << 20;
/// Cap queued delta bookkeeping, independent of how many commits arrive.
const EMBED_MAX_PENDING_CHANGES: usize = 4096;

/// Why the store refused a call. The wire adapter maps each to its contract
/// token and HTTP status; the store knows neither.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderErrorCode {
    BadRequest,
    NotFound,
    NoOpenFeed,
    EpochMismatch,
    EpochRegressed,
    CheckpointNotIncreasing,
    VersionMismatch,
    TooLarge,
    /// The asked kind of search is not offered (no embedding model loaded).
    ModeUnavailable,
    Unavailable,
    Internal,
}

/// A refusal. `detail` is for the helper's own diagnostics and is never sent
/// on the wire.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{code:?}: {detail}")]
pub struct ProviderError {
    pub code: ProviderErrorCode,
    pub detail: String,
}

impl ProviderError {
    fn new(code: ProviderErrorCode, detail: impl Into<String>) -> Self {
        Self {
            code,
            detail: detail.into(),
        }
    }

    fn bad(detail: impl Into<String>) -> Self {
        Self::new(ProviderErrorCode::BadRequest, detail)
    }

    fn internal(error: impl std::fmt::Display) -> Self {
        Self::new(ProviderErrorCode::Internal, error.to_string())
    }

    fn no_open_feed() -> Self {
        Self::new(ProviderErrorCode::NoOpenFeed, "send begin first")
    }

    fn mode_unavailable() -> Self {
        Self::new(
            ProviderErrorCode::ModeUnavailable,
            "no embedding model is loaded",
        )
    }
}

pub type ProviderResult<T> = std::result::Result<T, ProviderError>;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ProviderAsOf {
    pub epoch: u64,
    pub n: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderState {
    Ready,
    Building,
    Unavailable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderFeedMode {
    /// A resync into a fresh index; on commit every id not put since the
    /// `begin` is gone.
    Full,
    /// In-place changes to the committed index.
    Delta,
}

/// A kind of search the query asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderMode {
    Lexical,
    Semantic,
    /// Reciprocal-rank fusion of a lexical and a semantic run.
    Hybrid,
}

/// The kind of index one coverage entry describes: `lexical` or `semantic`
/// (a `hybrid` query reports both for every partition).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderKind {
    Lexical,
    Semantic,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderReference {
    pub partition: String,
    pub id: String,
    pub version: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderCoverage {
    pub partition: String,
    pub kind: ProviderKind,
    pub state: ProviderState,
    pub as_of: Option<ProviderAsOf>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderQueryOutcome {
    pub mode: ProviderMode,
    pub coverage: Vec<ProviderCoverage>,
    pub refs: Vec<ProviderReference>,
}

/// One query. `terms` are the host's matcher tokens of `text`
/// (`current-body-words-v1`), authoritative for the lexical recall
/// obligation; `None` derives them from `text` with the same matcher.
#[derive(Debug, Clone, Copy)]
pub struct ProviderQuery<'a> {
    pub partitions: &'a [String],
    pub text: &'a str,
    pub mode: ProviderMode,
    pub terms: Option<&'a [String]>,
    pub limit: usize,
}

/// State of one kind of index of a partition.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderKindStatus {
    pub state: ProviderState,
    pub as_of: Option<ProviderAsOf>,
    /// Notes committed lexically whose embedding is not finished yet.
    pub pending_notes: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderStatus {
    pub state: ProviderState,
    pub as_of: Option<ProviderAsOf>,
    /// Notes the serving index holds (fully or partly searchable).
    pub notes: usize,
    /// Notes accepted but not searchable, counted by reason.
    pub excluded: BTreeMap<String, usize>,
    /// Notes searchable only in part (for example, invalid frontmatter:
    /// the body is indexed, the properties are not), counted by reason.
    pub partial: BTreeMap<String, usize>,
    pub index_bytes: u64,
    /// The semantic kind, present only when an embedding model is loaded.
    pub semantic: Option<ProviderKindStatus>,
}

#[derive(Debug, Clone)]
enum NoteOutcome {
    Indexed,
    Partial(&'static str),
    Excluded(String),
}

#[derive(Debug, Clone)]
struct NoteRecord {
    version: String,
    /// Empty for a note excluded before its path was read.
    path: String,
    /// Body-words document of a searchable note.
    slot: Option<u64>,
    outcome: NoteOutcome,
}

impl NoteRecord {
    fn searchable(&self) -> bool {
        !matches!(self.outcome, NoteOutcome::Excluded(_))
    }
}

/// A change to the reference maps that queries resolve hits through.
#[derive(Debug, Clone)]
struct RefChange {
    id: String,
    path: String,
    slot: u64,
    reference: Option<Arc<ProviderReference>>,
}

/// The body-words index of one generation: one document per searchable
/// note, holding its `current-body-words-v1` words, so "which notes contain
/// all of these words" is exact.
struct WordsIndex {
    writer: IndexWriter,
    reader: IndexReader,
    slot: Field,
    words: Field,
    #[cfg(test)]
    fail_commit: bool,
}

impl WordsIndex {
    fn create(dir: &Path) -> ProviderResult<Self> {
        create_private_dir(dir)?;
        let mut builder = Schema::builder();
        let slot = builder.add_u64_field("slot", INDEXED | FAST);
        let words = builder.add_text_field(
            "words",
            TextOptions::default().set_indexing_options(
                TextFieldIndexing::default()
                    .set_tokenizer("raw")
                    .set_index_option(IndexRecordOption::WithFreqs),
            ),
        );
        let index = Index::create_in_dir(dir, builder.build()).map_err(ProviderError::internal)?;
        let writer = index
            .writer_with_num_threads(1, WORDS_WRITER_MEMORY_BYTES)
            .map_err(ProviderError::internal)?;
        let reader = index
            .reader_builder()
            .reload_policy(ReloadPolicy::Manual)
            .try_into()
            .map_err(ProviderError::internal)?;
        Ok(Self {
            writer,
            reader,
            slot,
            words,
            #[cfg(test)]
            fail_commit: false,
        })
    }

    fn commit(&mut self) -> ProviderResult<()> {
        #[cfg(test)]
        if std::mem::take(&mut self.fail_commit) {
            return Err(ProviderError::internal(
                "injected body-words commit failure",
            ));
        }
        self.writer.commit().map_err(ProviderError::internal)?;
        Ok(())
    }

    fn add(&mut self, slot: u64, words: &[String]) -> ProviderResult<()> {
        let tokens = words
            .iter()
            .enumerate()
            .map(|(position, word)| Token {
                offset_from: 0,
                offset_to: 0,
                position,
                text: word.clone(),
                position_length: 1,
            })
            .collect();
        let mut document = TantivyDocument::default();
        document.add_u64(self.slot, slot);
        document.add_pre_tokenized_text(
            self.words,
            PreTokenizedString {
                text: String::new(),
                tokens,
            },
        );
        self.writer
            .add_document(document)
            .map_err(ProviderError::internal)?;
        Ok(())
    }

    fn remove(&mut self, slot: u64) {
        self.writer
            .delete_term(Term::from_field_u64(self.slot, slot));
    }
}

/// One disposable index: the serving index of a partition, or the one a
/// `full` feed is building.
struct Generation {
    dir: PathBuf,
    partition: String,
    /// Owner tag stored in every chunk (SHA-256 hex of the partition name).
    /// Chunk and source identity derive from it, so identical paths in two
    /// partitions of one query stay two candidates.
    owner: String,
    epoch: u64,
    /// Last checkpoint taken on this index.
    n: u64,
    poisoned: bool,
    index: Index,
    fields: Fields,
    source_key: Field,
    writer: IndexWriter,
    reader: IndexReader,
    words: WordsIndex,
    next_slot: u64,
    notes: BTreeMap<String, NoteRecord>,
    ids_by_path: HashMap<String, String>,
    /// Undo log for writes since the last checkpoint.
    undo: Vec<(String, Option<NoteRecord>)>,
    /// Reference changes since the last checkpoint.
    pending_refs: Vec<RefChange>,
    /// Reference changes checkpointed but not yet committed.
    checkpointed_refs: Vec<RefChange>,
}

impl Generation {
    fn create(dir: PathBuf, partition: &str, epoch: u64, n: u64) -> ProviderResult<Self> {
        if dir.exists() {
            fs::remove_dir_all(&dir).map_err(ProviderError::internal)?;
        }
        create_private_dir(&dir)?;
        let schema = build_schema();
        let fields = Fields::from_schema(&schema).map_err(ProviderError::internal)?;
        let source_key = fields
            .source_key
            .ok_or_else(|| ProviderError::internal("schema has no source_key field"))?;
        let index = Index::create_in_dir(&dir, schema).map_err(ProviderError::internal)?;
        register_lexical_analyzer(&index);
        let writer = index
            .writer(WRITER_MEMORY_BYTES)
            .map_err(ProviderError::internal)?;
        let reader = index
            .reader_builder()
            .reload_policy(ReloadPolicy::Manual)
            .try_into()
            .map_err(ProviderError::internal)?;
        let words = WordsIndex::create(&dir.join("words"))?;
        Ok(Self {
            dir,
            partition: partition.to_owned(),
            owner: owner_tag(partition),
            epoch,
            n,
            poisoned: false,
            index,
            fields,
            source_key,
            writer,
            reader,
            words,
            next_slot: 0,
            notes: BTreeMap::new(),
            ids_by_path: HashMap::new(),
            undo: Vec::new(),
            pending_refs: Vec::new(),
            checkpointed_refs: Vec::new(),
        })
    }

    fn key_term(&self, path: &str) -> Term {
        Term::from_field_text(self.source_key, &room_source_key(&self.owner, None, path))
    }

    /// Removes `id` from the index and the bookkeeping; returns its record.
    fn detach(&mut self, id: &str) -> Option<NoteRecord> {
        let record = self.notes.remove(id)?;
        if record.searchable() {
            self.ids_by_path.remove(&record.path);
            self.writer.delete_term(self.key_term(&record.path));
            if let Some(slot) = record.slot {
                self.words.remove(slot);
                self.pending_refs.push(RefChange {
                    id: id.to_owned(),
                    path: record.path.clone(),
                    slot,
                    reference: None,
                });
            }
        }
        Some(record)
    }

    fn remove(&mut self, id: &str) {
        let previous = self.detach(id);
        if previous.is_some() {
            self.undo.push((id.to_owned(), previous));
        }
    }

    fn apply(
        &mut self,
        id: &str,
        version: &str,
        path: &str,
        prepared: Prepared,
    ) -> ProviderResult<()> {
        if let Some(existing) = self.notes.get(id)
            && existing.version == version
            && existing.path == path
        {
            // Idempotent on (id, version).
            return Ok(());
        }
        if prepared.searchable()
            && let Some(holder) = self.ids_by_path.get(path)
            && holder != id
        {
            // A rename arrives as delete + put; until the delete lands, the
            // later put replaces the earlier holder of the path.
            let holder = holder.clone();
            self.remove(&holder);
        }
        let previous = self.detach(id);
        self.undo.push((id.to_owned(), previous));
        for document in prepared.documents {
            self.writer
                .add_document(document)
                .map_err(ProviderError::internal)?;
        }
        let mut record = NoteRecord {
            version: version.to_owned(),
            path: path.to_owned(),
            slot: None,
            outcome: prepared.outcome,
        };
        if record.searchable() {
            let slot = self.next_slot;
            self.next_slot += 1;
            self.words.add(slot, &prepared.words)?;
            record.slot = Some(slot);
            self.ids_by_path.insert(path.to_owned(), id.to_owned());
            self.pending_refs.push(RefChange {
                id: id.to_owned(),
                path: path.to_owned(),
                slot,
                reference: Some(Arc::new(ProviderReference {
                    partition: self.partition.clone(),
                    id: id.to_owned(),
                    version: version.to_owned(),
                })),
            });
        }
        self.notes.insert(id.to_owned(), record);
        Ok(())
    }

    /// Makes the writes so far durable as checkpoint `n` (not yet visible).
    fn checkpoint(&mut self, n: u64) -> ProviderResult<()> {
        // The two durable commits cannot be made as one transaction. Even an
        // error from the first writer may mean it changed durable state.
        self.poisoned = true;
        self.writer.commit().map_err(ProviderError::internal)?;
        self.words.commit()?;
        self.poisoned = false;
        self.n = n;
        self.undo.clear();
        self.checkpointed_refs.append(&mut self.pending_refs);
        Ok(())
    }

    /// Drops every write after the last checkpoint.
    fn rollback(&mut self) -> ProviderResult<()> {
        if self.undo.is_empty() && self.pending_refs.is_empty() {
            return Ok(());
        }
        self.writer.rollback().map_err(ProviderError::internal)?;
        self.words
            .writer
            .rollback()
            .map_err(ProviderError::internal)?;
        while let Some((id, previous)) = self.undo.pop() {
            if let Some(current) = self.notes.remove(&id)
                && current.searchable()
            {
                self.ids_by_path.remove(&current.path);
            }
            if let Some(previous) = previous {
                if previous.searchable() {
                    self.ids_by_path.insert(previous.path.clone(), id.clone());
                }
                self.notes.insert(id, previous);
            }
        }
        self.pending_refs.clear();
        Ok(())
    }

    /// Reloads the readers onto the last checkpoint and returns what a query
    /// should see, plus the reference changes that came with it.
    fn publish(&mut self) -> ProviderResult<(Served, Vec<RefChange>)> {
        self.check_usable()?;
        self.poisoned = true;
        self.rollback()?;
        self.reader.reload().map_err(ProviderError::internal)?;
        self.words
            .reader
            .reload()
            .map_err(ProviderError::internal)?;
        let served = Served {
            owner: self.owner.clone(),
            index: self.index.clone(),
            fields: self.fields.clone(),
            source_key: self.source_key,
            searcher: self.reader.searcher(),
            words: self.words.reader.searcher(),
            words_field: self.words.words,
            as_of: ProviderAsOf {
                epoch: self.epoch,
                n: self.n,
            },
        };
        self.poisoned = false;
        Ok((served, std::mem::take(&mut self.checkpointed_refs)))
    }

    fn check_usable(&self) -> ProviderResult<()> {
        if self.poisoned {
            return Err(ProviderError::new(
                ProviderErrorCode::EpochMismatch,
                "index state is ambiguous; a full feed is required",
            ));
        }
        Ok(())
    }

    /// Every searchable note: `(id, version, path)`.
    fn searchable_notes(&self) -> BTreeMap<String, NoteKey> {
        self.notes
            .iter()
            .filter(|(_, record)| record.searchable())
            .map(|(id, record)| {
                (
                    id.clone(),
                    NoteKey {
                        id: id.clone(),
                        version: record.version.clone(),
                        path: record.path.clone(),
                    },
                )
            })
            .collect()
    }

    fn status_counts(&self) -> Counts {
        let mut notes = 0;
        let mut excluded = BTreeMap::new();
        let mut partial = BTreeMap::new();
        for record in self.notes.values() {
            match &record.outcome {
                NoteOutcome::Indexed => notes += 1,
                NoteOutcome::Partial(reason) => {
                    notes += 1;
                    *partial.entry((*reason).to_owned()).or_insert(0) += 1;
                }
                NoteOutcome::Excluded(reason) => {
                    *excluded.entry(reason.clone()).or_insert(0) += 1;
                }
            }
        }
        (notes, excluded, partial)
    }
}

/// What a query reads: pinned searchers at one committed `asOf`.
struct Served {
    owner: String,
    index: Index,
    fields: Fields,
    source_key: Field,
    searcher: Searcher,
    /// The body-words index at the same commit.
    words: Searcher,
    words_field: Field,
    as_of: ProviderAsOf,
}

/// Note counts of one commit: searchable notes, excluded and partial notes by
/// reason.
type Counts = (usize, BTreeMap<String, usize>, BTreeMap<String, usize>);

/// The committed view of a partition: the searchers, the reference maps and
/// the note counts for the same commit, swapped together.
#[derive(Default)]
struct View {
    served: Option<Arc<Served>>,
    by_path: HashMap<String, Arc<ProviderReference>>,
    by_slot: HashMap<u64, Arc<ProviderReference>>,
    by_id: HashMap<String, Arc<ProviderReference>>,
    counts: Counts,
}

impl View {
    fn apply(&mut self, changes: &[RefChange]) {
        for change in changes {
            match &change.reference {
                Some(reference) => {
                    self.by_path
                        .insert(change.path.clone(), Arc::clone(reference));
                    self.by_slot.insert(change.slot, Arc::clone(reference));
                    self.by_id.insert(change.id.clone(), Arc::clone(reference));
                }
                None => {
                    self.by_path.remove(&change.path);
                    self.by_slot.remove(&change.slot);
                    self.by_id.remove(&change.id);
                }
            }
        }
    }
}

struct Prepared {
    outcome: NoteOutcome,
    documents: Vec<tantivy::TantivyDocument>,
    /// `current-body-words-v1` words of the note body, in index form.
    words: Vec<String>,
}

impl Prepared {
    fn searchable(&self) -> bool {
        !matches!(self.outcome, NoteOutcome::Excluded(_))
    }
}

struct Feed {
    mode: ProviderFeedMode,
    /// The fresh index a `full` feed builds; `None` for `delta`.
    staging: Option<Generation>,
}

#[derive(Default)]
struct Writes {
    /// Highest epoch any `begin` has opened, committed or abandoned. A `full`
    /// feed may not go below it.
    highest_epoch: u64,
    /// What the last commit made visible; `None` before the first. Kept apart
    /// from `live.n`, which also moves at checkpoints that are never
    /// committed.
    committed: Option<ProviderAsOf>,
    /// A delta feed was abandoned after a checkpoint. Its writes are durable
    /// in `live` and cannot be taken back, so `live` no longer matches
    /// `committed`: it is never served again, `delta` is refused, and only a
    /// `full` feed (same epoch allowed) can replace it.
    needs_full: bool,
    live: Option<Generation>,
    feed: Option<Feed>,
}

impl Writes {
    /// Drops the open feed, if any, and everything it wrote that was not
    /// committed.
    fn abandon_feed(&mut self) -> ProviderResult<()> {
        let Some(abandoned) = self.feed.take() else {
            return Ok(());
        };
        match abandoned.staging {
            Some(staging) => {
                let dir = staging.dir.clone();
                drop(staging);
                let _ = fs::remove_dir_all(dir);
            }
            None => {
                if let Some(live) = self.live.as_mut() {
                    if live.poisoned
                        || self
                            .committed
                            .is_some_and(|committed| live.n != committed.n)
                    {
                        self.needs_full = true;
                    } else if let Err(error) = live.rollback() {
                        self.needs_full = true;
                        return Err(error);
                    }
                }
            }
        }
        Ok(())
    }

    fn target(&mut self) -> ProviderResult<&mut Generation> {
        let target = match &mut self.feed {
            Some(Feed {
                staging: Some(staging),
                ..
            }) => Some(staging),
            Some(Feed { staging: None, .. }) => self.live.as_mut(),
            None => None,
        }
        .ok_or_else(ProviderError::no_open_feed)?;
        target.check_usable()?;
        Ok(target)
    }
}

/// A read connection to one complete vector file.
struct VectorSnapshot {
    read: Mutex<Connection>,
    #[cfg(test)]
    before_scan: Mutex<Option<(mpsc::SyncSender<()>, mpsc::Receiver<()>)>>,
}

/// The semantic kind of a partition: the vector file queries read and the
/// last commit whose embeddings it holds completely.
#[derive(Default)]
struct SemanticState {
    snapshot: Option<Arc<VectorSnapshot>>,
    as_of: Option<ProviderAsOf>,
    /// Full generation of these vectors (or of the failure below).
    base: u64,
    /// Embedding failed; the kind is `unavailable` until the next full feed
    /// embeds completely.
    failed: bool,
}

/// The vector file the embedding worker writes (the one queries read too).
struct VectorWriter {
    path: PathBuf,
    conn: Connection,
}

struct Partition {
    dir: PathBuf,
    writes: Mutex<Writes>,
    view: RwLock<View>,
    semantic: RwLock<SemanticState>,
    /// Used only by the embedding worker.
    vector_writer: Mutex<Option<VectorWriter>>,
    /// Sequence number of the latest committed full feed.
    full_seq: AtomicU64,
    retired: AtomicBool,
    /// Outstanding notes in the current full generation, including the running job.
    pending: Mutex<PendingNotes>,
}

fn read<T>(lock: &RwLock<T>) -> RwLockReadGuard<'_, T> {
    lock.read()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn lock<T>(lock: &Mutex<T>) -> MutexGuard<'_, T> {
    lock.lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

impl Partition {
    fn new(dir: PathBuf) -> Self {
        Self {
            dir,
            writes: Mutex::new(Writes::default()),
            view: RwLock::new(View::default()),
            semantic: RwLock::new(SemanticState::default()),
            vector_writer: Mutex::new(None),
            full_seq: AtomicU64::new(0),
            retired: AtomicBool::new(false),
            pending: Mutex::new(PendingNotes::default()),
        }
    }

    fn lock(&self) -> MutexGuard<'_, Writes> {
        lock(&self.writes)
    }

    fn served(&self) -> Option<Arc<Served>> {
        read(&self.view).served.clone()
    }
}

/// A partial answer: coverage of one kind and its ordered references.
type KindAnswer = (Vec<ProviderCoverage>, Vec<Arc<ProviderReference>>);

struct QueryPartition<'a> {
    name: &'a str,
    partition: Option<Arc<Partition>>,
    served: Option<Arc<Served>>,
}

impl QueryPartition<'_> {
    fn matches(&self, view: &View) -> bool {
        match (&self.served, &view.served) {
            (Some(pinned), Some(current)) => Arc::ptr_eq(pinned, current),
            (None, None) => true,
            _ => false,
        }
    }
}

/// `(id, version, path)` of one note.
#[derive(Debug, Clone)]
struct NoteKey {
    id: String,
    version: String,
    path: String,
}

enum EmbedWork {
    Full(BTreeMap<String, NoteKey>),
    Delta(BTreeMap<String, Option<NoteKey>>),
}

impl EmbedWork {
    fn visit_ids(&self, mut visit: impl FnMut(&str)) {
        match self {
            Self::Full(notes) => notes.keys().for_each(|id| visit(id)),
            Self::Delta(changes) => changes.keys().for_each(|id| visit(id)),
        }
    }
}

/// Latest lexical snapshot and net work needed to reach it.
struct EmbedJob {
    partition: Arc<Partition>,
    served: Arc<Served>,
    base: u64,
    work: EmbedWork,
}

#[derive(Default)]
struct PendingNotes {
    base: u64,
    /// One entry per affected id; a running job and a queued job can overlap.
    by_id: HashMap<String, usize>,
}

impl PendingNotes {
    fn add(&mut self, work: &EmbedWork) {
        work.visit_ids(|id| *self.by_id.entry(id.to_owned()).or_default() += 1);
    }

    fn remove(&mut self, work: &EmbedWork) {
        work.visit_ids(|id| {
            let count = self.by_id.get_mut(id).expect("settling a pending id");
            *count -= 1;
            if *count == 0 {
                self.by_id.remove(id);
            }
        });
    }
}

impl EmbedJob {
    /// Called under the semantic write lock on publish, or on failure/cancel.
    fn settle(&self) {
        let mut pending = lock(&self.partition.pending);
        if pending.base == self.base {
            pending.remove(&self.work);
        }
    }

    fn moot(&self) -> bool {
        self.partition.retired.load(Ordering::Acquire)
            || self.base != self.partition.full_seq.load(Ordering::Acquire)
    }
}

#[derive(Default)]
struct EmbedQueue {
    work: Mutex<VecDeque<EmbedJob>>,
    /// Terminal failure of the only worker. Set while holding `work`, so
    /// enqueue cannot leave work behind after failure cleanup.
    failed: AtomicBool,
}

impl EmbedQueue {
    fn fail(&self, queue: &mut VecDeque<EmbedJob>, running: Option<&EmbedJob>) {
        self.failed.store(true, Ordering::Release);
        if let Some(job) = running {
            lock(&job.partition.pending).by_id.clear();
        }
        for job in queue.drain(..) {
            lock(&job.partition.pending).by_id.clear();
        }
    }
}

/// The loaded embedding model: one instance answers queries, another (owned
/// by the background worker) embeds commits, so a long feed never delays a
/// query behind a passage batch.
struct SemanticShared {
    dimensions: usize,
    query_embedder: Mutex<Box<dyn Embedder>>,
    jobs: Arc<EmbedQueue>,
    /// A single wakeup is enough: the worker drains the queue before sleeping.
    wake: mpsc::SyncSender<()>,
}

/// The push-fed store: every partition the host has begun and not retired.
pub struct ProviderStore {
    partitions_dir: PathBuf,
    partitions: Mutex<BTreeMap<String, Arc<Partition>>>,
    semantic: Option<SemanticShared>,
}

impl ProviderStore {
    /// Opens a lexical-only store in `data_dir`, discarding anything a
    /// previous run left there. Refuses a non-empty directory this store did
    /// not create, so a mistyped path cannot wipe unrelated data.
    ///
    /// Directories it creates are private to the user (0700 on Unix); the
    /// files inside follow the process umask, which the helper sets to 0077.
    pub fn open(data_dir: &Path) -> ProviderResult<Self> {
        let partitions_dir = prepare_data_dir(data_dir)?;
        Ok(Self {
            partitions_dir,
            partitions: Mutex::new(BTreeMap::new()),
            semantic: None,
        })
    }

    /// Opens a store that also offers `semantic` and `hybrid`. Both
    /// embedders must load the same model: `query_embedder` answers queries,
    /// `index_embedder` is moved to a background thread that embeds commits.
    pub fn open_with_semantic(
        data_dir: &Path,
        query_embedder: Box<dyn Embedder>,
        index_embedder: Box<dyn Embedder>,
    ) -> ProviderResult<Self> {
        if query_embedder.profile() != index_embedder.profile() {
            return Err(ProviderError::bad(
                "query and index embedders load different models",
            ));
        }
        let dimensions = query_embedder.profile().dimensions;
        let partitions_dir = prepare_data_dir(data_dir)?;
        let jobs = Arc::new(EmbedQueue::default());
        let worker_jobs = Arc::clone(&jobs);
        let (wake, receiver) = mpsc::sync_channel(1);
        std::thread::Builder::new()
            .name("provider-embed".to_owned())
            .spawn(move || embed_worker(&worker_jobs, receiver, index_embedder, dimensions))
            .map_err(ProviderError::internal)?;
        Ok(Self {
            partitions_dir,
            partitions: Mutex::new(BTreeMap::new()),
            semantic: Some(SemanticShared {
                dimensions,
                query_embedder: Mutex::new(query_embedder),
                jobs,
                wake,
            }),
        })
    }

    /// Whether `semantic` and `hybrid` are offered (an embedding model is
    /// loaded).
    pub fn semantic_enabled(&self) -> bool {
        self.semantic.is_some()
    }

    fn map(&self) -> MutexGuard<'_, BTreeMap<String, Arc<Partition>>> {
        lock(&self.partitions)
    }

    fn known(&self, name: &str) -> ProviderResult<Option<Arc<Partition>>> {
        validate_partition(name)?;
        Ok(self.map().get(name).cloned())
    }

    fn feeding(&self, name: &str) -> ProviderResult<Arc<Partition>> {
        self.known(name)?.ok_or_else(ProviderError::no_open_feed)
    }

    /// Opens a feed. `full` needs `epoch` ≥ every epoch begun so far and
    /// builds a fresh index while the committed one keeps serving; `delta`
    /// needs `epoch` = the committed epoch and changes the committed index in
    /// place. Every `begin`, even a refused one, first abandons an open,
    /// uncommitted feed: the host has lost its place in it. A `delta`
    /// abandoned after a checkpoint leaves writes that cannot be taken back,
    /// so the partition then refuses `delta` with `epoch_mismatch` until a
    /// `full` feed commits (contract §2.2 rule 2: the host resends, or
    /// resyncs with the same epoch).
    pub fn begin(&self, name: &str, epoch: u64, mode: ProviderFeedMode) -> ProviderResult<()> {
        validate_partition(name)?;
        let partition = {
            let mut map = self.map();
            Arc::clone(map.entry(name.to_owned()).or_insert_with(|| {
                Arc::new(Partition::new(self.partitions_dir.join(owner_tag(name))))
            }))
        };
        let mut writes = partition.lock();
        writes.abandon_feed()?;
        let committed = writes.committed;
        match mode {
            ProviderFeedMode::Delta => {
                if writes.needs_full || committed.map(|c| c.epoch) != Some(epoch) {
                    return Err(ProviderError::new(
                        ProviderErrorCode::EpochMismatch,
                        "delta needs the committed epoch and no pending resync",
                    ));
                }
            }
            ProviderFeedMode::Full => {
                if epoch < writes.highest_epoch {
                    return Err(ProviderError::new(
                        ProviderErrorCode::EpochRegressed,
                        "full needs an epoch at least the highest begun",
                    ));
                }
            }
        }
        let staging = match mode {
            ProviderFeedMode::Delta => None,
            ProviderFeedMode::Full => {
                // Checkpoint numbers keep increasing within an epoch, counted
                // from the last commit, never from an abandoned checkpoint.
                let floor = committed.filter(|c| c.epoch == epoch).map_or(0, |c| c.n);
                let dir = partition
                    .dir
                    .join(format!("epoch-{epoch}-{}", unique_suffix()));
                Some(Generation::create(dir, name, epoch, floor)?)
            }
        };
        writes.highest_epoch = writes.highest_epoch.max(epoch);
        writes.feed = Some(Feed { mode, staging });
        Ok(())
    }

    pub fn put(
        &self,
        name: &str,
        id: &str,
        version: &str,
        path: &str,
        bytes: &[u8],
    ) -> ProviderResult<()> {
        validate_id(id)?;
        validate_version(version)?;
        validate_path(path)?;
        let partition = self.feeding(name)?;
        if bytes.len() as u64 > PROVIDER_MAX_ITEM_BYTES {
            self.exclude_over_transport_cap(name, id)?;
            return Err(ProviderError::new(
                ProviderErrorCode::TooLarge,
                "body over the item cap",
            ));
        }
        if hex_sha256(bytes) != version {
            return Err(ProviderError::new(
                ProviderErrorCode::VersionMismatch,
                "version is not the SHA-256 of the body",
            ));
        }
        // CPU-bound preparation runs before the partition lock is taken.
        let prepared = prepare(&owner_tag(name), path, bytes)?;
        let mut writes = partition.lock();
        writes.target()?.apply(id, version, path, prepared)
    }

    /// Records `id` as excluded because the transport refused its body
    /// before reading it. Never silently dropped: `status` counts it.
    pub fn exclude_over_transport_cap(&self, name: &str, id: &str) -> ProviderResult<()> {
        validate_id(id)?;
        let partition = self.feeding(name)?;
        let mut writes = partition.lock();
        let target = writes.target()?;
        let previous = target.detach(id);
        target.undo.push((id.to_owned(), previous));
        target.notes.insert(
            id.to_owned(),
            NoteRecord {
                version: String::new(),
                path: String::new(),
                slot: None,
                outcome: NoteOutcome::Excluded(PROVIDER_OVER_TRANSPORT_CAP.to_owned()),
            },
        );
        Ok(())
    }

    pub fn delete(&self, name: &str, id: &str) -> ProviderResult<()> {
        validate_id(id)?;
        let partition = self.feeding(name)?;
        let mut writes = partition.lock();
        writes.target()?.remove(id);
        Ok(())
    }

    /// Makes every write so far durable as checkpoint `n`. Visible at the
    /// next commit.
    pub fn checkpoint(&self, name: &str, n: u64) -> ProviderResult<()> {
        let partition = self.feeding(name)?;
        let mut writes = partition.lock();
        let target = writes.target()?;
        if n <= target.n {
            return Err(ProviderError::new(
                ProviderErrorCode::CheckpointNotIncreasing,
                format!("checkpoint must exceed {}", target.n),
            ));
        }
        target.checkpoint(n)
    }

    /// Closes the open feed and makes everything up to its last checkpoint
    /// visible; writes after that checkpoint are dropped. With a model
    /// loaded, the commit is then embedded in the background: the semantic
    /// kind stays at its previous `asOf` (state `building`) until done.
    pub fn commit(&self, name: &str) -> ProviderResult<ProviderAsOf> {
        let partition = self.feeding(name)?;
        let mut writes = partition.lock();
        let feed = writes.feed.take().ok_or_else(ProviderError::no_open_feed)?;
        let full = feed.mode == ProviderFeedMode::Full;
        let ((served, refs), previous) = if full {
            let mut staging = feed
                .staging
                .ok_or_else(|| ProviderError::internal("full feed without an index"))?;
            let result = match staging.publish() {
                Ok(result) => result,
                Err(error) => {
                    let dir = staging.dir.clone();
                    drop(staging);
                    let _ = fs::remove_dir_all(dir);
                    return Err(error);
                }
            };
            // Keep the previous generation's files until after swapping the view.
            let previous = writes.live.replace(staging);
            writes.needs_full = false;
            (result, previous)
        } else {
            let live = writes
                .live
                .as_mut()
                .ok_or_else(ProviderError::no_open_feed)?;
            match live.publish() {
                Ok(result) => (result, None),
                Err(error) => {
                    writes.needs_full = true;
                    return Err(error);
                }
            }
        };
        let as_of = served.as_of;
        let served = Arc::new(served);
        let live = writes
            .live
            .as_ref()
            .expect("a successful publish has a live index");
        let counts = live.status_counts();
        {
            let mut view = write_lock(&partition.view);
            if full {
                *view = View::default();
            }
            view.apply(&refs);
            view.served = Some(Arc::clone(&served));
            view.counts = counts;
            if let Some(semantic) = &self.semantic {
                let base = if full {
                    partition.full_seq.fetch_add(1, Ordering::AcqRel) + 1
                } else {
                    partition.full_seq.load(Ordering::Acquire)
                };
                let work = if full {
                    EmbedWork::Full(live.searchable_notes())
                } else {
                    delta_work(refs)
                };
                semantic.enqueue(
                    EmbedJob {
                        partition: Arc::clone(&partition),
                        served,
                        base,
                        work,
                    },
                    live,
                );
            }
        }
        writes.committed = Some(as_of);
        if let Some(previous) = previous {
            let dir = previous.dir.clone();
            drop(previous);
            let _ = fs::remove_dir_all(dir);
        }
        Ok(as_of)
    }

    /// Forgets the partition and deletes its data. Idempotent.
    pub fn retire(&self, name: &str) -> ProviderResult<()> {
        validate_partition(name)?;
        // The map stays locked until the directory is gone, so a `begin` of
        // the same name cannot create its index inside a directory that is
        // being removed.
        let mut map = self.map();
        let Some(partition) = map.remove(name) else {
            return Ok(());
        };
        partition.retired.store(true, Ordering::Release);
        let mut writes = partition.lock();
        {
            let mut view = write_lock(&partition.view);
            *view = View::default();
            *write_lock(&partition.semantic) = SemanticState::default();
            if let Some(semantic) = &self.semantic {
                lock(&semantic.jobs.work).retain(|job| !Arc::ptr_eq(&job.partition, &partition));
            }
            let mut pending = lock(&partition.pending);
            pending.base = u64::MAX;
            pending.by_id.clear();
        }
        *writes = Writes::default();
        let _ = fs::remove_dir_all(&partition.dir);
        Ok(())
    }

    /// Lexical query with terms derived from `text` (see [`Self::ask`]).
    pub fn query(
        &self,
        names: &[String],
        text: &str,
        limit: usize,
    ) -> ProviderResult<ProviderQueryOutcome> {
        self.ask(&ProviderQuery {
            partitions: names,
            text,
            mode: ProviderMode::Lexical,
            terms: None,
            limit,
        })
    }

    /// Answers one query over exactly the asked partitions.
    ///
    /// - `lexical`: Kwiry's lexical-v2 ranking over the asked partitions
    ///   (BM25 statistics summed over exactly that set), under the recall
    ///   obligation: every note containing all `terms` as body words is
    ///   returned when they fit in `limit`, and when they do not, every
    ///   returned note is one of them. Kwiry's order applies within each tier.
    /// - `semantic`: nearest notes by best chunk, read from each asked
    ///   partition's own vector file and merged.
    /// - `hybrid`: reciprocal-rank fusion of the two.
    ///
    /// Equal ranks are ordered by id bytes ascending.
    pub fn ask(&self, question: &ProviderQuery<'_>) -> ProviderResult<ProviderQueryOutcome> {
        let names = question.partitions;
        if names.is_empty() || names.len() > PROVIDER_MAX_QUERY_PARTITIONS {
            return Err(ProviderError::bad("partitions must name 1..=64"));
        }
        for (index, name) in names.iter().enumerate() {
            validate_partition(name).map_err(|_| ProviderError::bad("invalid partition name"))?;
            if names[..index].contains(name) {
                return Err(ProviderError::bad("a partition is named twice"));
            }
        }
        let limit = question.limit;
        if limit == 0 || limit > PROVIDER_MAX_LIMIT {
            return Err(ProviderError::bad("limit must be 1..=200"));
        }
        if question.mode != ProviderMode::Lexical && self.semantic.is_none() {
            return Err(ProviderError::mode_unavailable());
        }
        let terms = match question.terms {
            Some(terms) if terms.len() > PROVIDER_MAX_TERMS => {
                return Err(ProviderError::bad("at most 128 terms"));
            }
            Some(terms) => {
                let mut seen = HashSet::new();
                terms
                    .iter()
                    .map(|term| body_words::index_word(&body_words::fold_for_match(term)))
                    .filter(|term| !term.is_empty() && seen.insert(term.clone()))
                    .collect()
            }
            None => body_words::query_words(question.text),
        };
        let mode = question.mode;
        let depth = if mode == ProviderMode::Hybrid {
            limit.max(HYBRID_CANDIDATES)
        } else {
            limit
        };
        let mut query = None;
        for _ in 0..QUERY_ATTEMPTS {
            let pinned = self.pin(names);
            let lexical = if mode != ProviderMode::Semantic {
                let Some(answer) = Self::lexical_once(&pinned, question.text, &terms, depth)?
                else {
                    continue;
                };
                Some(answer)
            } else {
                None
            };
            let semantic = if mode != ProviderMode::Lexical {
                if query.is_none() {
                    query = Some(self.embed_query(question.text)?);
                }
                let Some(answer) =
                    self.semantic_once(&pinned, query.as_ref().and_then(|q| q.as_deref()), depth)?
                else {
                    continue;
                };
                Some(answer)
            } else {
                None
            };
            let (coverage, refs) = match (lexical, semantic) {
                (Some((lexical_coverage, lexical)), Some((semantic_coverage, semantic))) => {
                    let coverage = lexical_coverage
                        .into_iter()
                        .zip(semantic_coverage)
                        .flat_map(|(lexical, semantic)| [lexical, semantic])
                        .collect();
                    (coverage, fuse(lexical, semantic, limit))
                }
                (Some(answer), None) | (None, Some(answer)) => answer,
                (None, None) => unreachable!("every query has a retrieval leg"),
            };
            return Ok(ProviderQueryOutcome {
                mode,
                coverage,
                refs: refs.into_iter().map(|r| (*r).clone()).collect(),
            });
        }
        Err(ProviderError::new(
            ProviderErrorCode::Unavailable,
            "commits kept landing during the query",
        ))
    }

    fn pin<'a>(&self, names: &'a [String]) -> Vec<QueryPartition<'a>> {
        let partitions: Vec<_> = {
            let map = self.map();
            names.iter().map(|name| map.get(name).cloned()).collect()
        };
        names
            .iter()
            .zip(partitions)
            .map(|(name, partition)| {
                let served = partition.as_ref().and_then(|p| p.served());
                QueryPartition {
                    name,
                    partition,
                    served,
                }
            })
            .collect()
    }

    /// One attempt; `None` when a commit landed between the search and its
    /// reference lookup, so the hits cannot be paired with one `asOf`.
    fn lexical_once(
        partitions: &[QueryPartition<'_>],
        text: &str,
        terms: &[String],
        limit: usize,
    ) -> ProviderResult<Option<KindAnswer>> {
        let mut coverage = Vec::with_capacity(partitions.len());
        let mut pinned = Vec::new();
        for pin in partitions {
            let state = match (&pin.partition, &pin.served) {
                (_, Some(_)) => ProviderState::Ready,
                (Some(_), None) => ProviderState::Building,
                (None, None) => ProviderState::Unavailable,
            };
            coverage.push(ProviderCoverage {
                partition: pin.name.to_owned(),
                kind: ProviderKind::Lexical,
                state,
                as_of: pin.served.as_ref().map(|s| s.as_of),
            });
            if let (Some(partition), Some(served)) = (&pin.partition, &pin.served) {
                pinned.push((Arc::clone(partition), Arc::clone(served)));
            }
        }
        // Keep acquisition order independent of the caller's partition order.
        pinned.sort_unstable_by(|(_, left), (_, right)| left.owner.cmp(&right.owner));
        if pinned.is_empty() {
            return Ok(Some((coverage, Vec::new())));
        }
        // Blank ranking text supplies no extra candidates, but never skips
        // the host's authoritative terms.
        let hits = if text.trim().is_empty() {
            Vec::new()
        } else {
            let targets: Vec<_> = pinned
                .iter()
                .map(|(_, served)| PinnedSearcher {
                    index: &served.index,
                    fields: &served.fields,
                    searcher: &served.searcher,
                })
                .collect();
            // Rank the plan's whole candidate window, then keep the best chunk of
            // each note, so `limit` notes survive the per-note collapse.
            search_pinned_with_quality(
                &targets,
                text,
                MAX_TOTAL_CANDIDATES,
                &SearchFilters::default(),
            )
            .map_err(|error| match error {
                crate::Error::Query(message) => ProviderError::bad(message),
                other => ProviderError::internal(other),
            })?
            .hits
        };
        let matches = all_terms_matches(&pinned, terms)?;
        let views: Vec<_> = pinned
            .iter()
            .map(|(partition, _)| read(&partition.view))
            .collect();
        for ((_, served), view) in pinned.iter().zip(&views) {
            if !view
                .served
                .as_ref()
                .is_some_and(|current| Arc::ptr_eq(current, served))
            {
                return Ok(None);
            }
        }
        // Kwiry's order, one entry per note (its best chunk).
        let mut seen = HashSet::new();
        let mut ranked: Vec<(Arc<ProviderReference>, Score)> = Vec::new();
        for hit in hits {
            if !seen.insert((hit.vault_id.clone(), hit.path.clone())) {
                continue;
            }
            let reference = pinned
                .iter()
                .zip(&views)
                .find(|((_, served), _)| served.owner == hit.vault_id)
                .and_then(|(_, view)| view.by_path.get(&hit.path))
                .ok_or_else(|| ProviderError::internal("a hit has no reference at its commit"))?;
            ranked.push((Arc::clone(reference), hit.score));
        }
        // Notes containing every term, with a body-words BM25 score.
        let mut all_terms: Vec<(Arc<ProviderReference>, Score)> = Vec::with_capacity(matches.len());
        for (index, score, slot) in matches {
            let reference = views[index]
                .by_slot
                .get(&slot)
                .ok_or_else(|| ProviderError::internal("a body-words hit has no reference"))?;
            all_terms.push((Arc::clone(reference), score));
        }
        drop(views);
        Ok(Some((coverage, superset_order(ranked, all_terms, limit))))
    }

    fn embed_query(&self, text: &str) -> ProviderResult<Option<Vec<f32>>> {
        let semantic = self
            .semantic
            .as_ref()
            .ok_or_else(ProviderError::mode_unavailable)?;
        if text.trim().is_empty() || semantic.jobs.failed.load(Ordering::Acquire) {
            return Ok(None);
        }
        let vector = lock(&semantic.query_embedder)
            .embed_query(text)
            .map_err(ProviderError::internal)?;
        if vector.len() != semantic.dimensions {
            return Err(ProviderError::internal("query vector has the wrong width"));
        }
        Ok(Some(vector))
    }

    fn semantic_once(
        &self,
        partitions: &[QueryPartition<'_>],
        query: Option<&[f32]>,
        limit: usize,
    ) -> ProviderResult<Option<KindAnswer>> {
        let semantic = self
            .semantic
            .as_ref()
            .ok_or_else(ProviderError::mode_unavailable)?;
        let mut coverage = Vec::with_capacity(partitions.len());
        let mut hits: Vec<(vectors::NoteHit, Arc<ProviderReference>)> = Vec::new();
        for pin in partitions {
            let Some(partition) = &pin.partition else {
                coverage.push(ProviderCoverage {
                    partition: pin.name.to_owned(),
                    kind: ProviderKind::Semantic,
                    state: ProviderState::Unavailable,
                    as_of: None,
                });
                continue;
            };
            let lexical = pin.served.as_ref().map(|s| s.as_of);
            let (snapshot, (kind_state, as_of)) = {
                let view = read(&partition.view);
                if !pin.matches(&view) {
                    return Ok(None);
                }
                let state = read(&partition.semantic);
                (
                    state.snapshot.clone(),
                    semantic.coverage(partition, lexical, &state),
                )
            };
            coverage.push(ProviderCoverage {
                partition: pin.name.to_owned(),
                kind: ProviderKind::Semantic,
                state: kind_state,
                as_of,
            });
            let (Some(query), Some(snapshot), Some(_)) = (query, snapshot, as_of) else {
                continue;
            };
            // Neither publication lock is held while queuing for SQLite or
            // computing distances. Check each id under a short view read.
            let nearest = {
                let conn = lock(&snapshot.read);
                #[cfg(test)]
                if let Some((entered, resume)) = lock(&snapshot.before_scan).take() {
                    entered.send(()).unwrap();
                    resume.recv().unwrap();
                }
                vectors::nearest_notes(&conn, query, limit, |id, version| {
                    let view = read(&partition.view);
                    pin.matches(&view) && view.by_id.get(id).is_some_and(|r| r.version == version)
                })
            };
            // A lexical commit or a vector publication since we pinned the
            // coverage can invalidate the scan (including a retired file).
            // Only resolve ids or propagate SQLite errors from a valid scan.
            let view = read(&partition.view);
            if !pin.matches(&view)
                || semantic.coverage(partition, lexical, &read(&partition.semantic))
                    != (kind_state, as_of)
            {
                return Ok(None);
            }
            for hit in nearest? {
                if let Some(reference) = view.by_id.get(&hit.id) {
                    hits.push((hit, Arc::clone(reference)));
                }
            }
        }
        // Distance needs no corpus statistics: the best `limit` notes of the
        // set are the best `limit` of the merged per-partition bests.
        hits.sort_by(|(left, left_ref), (right, right_ref)| {
            left.distance
                .total_cmp(&right.distance)
                .then_with(|| left.id.as_bytes().cmp(right.id.as_bytes()))
                .then_with(|| left_ref.partition.cmp(&right_ref.partition))
        });
        hits.truncate(limit);
        Ok(Some((
            coverage,
            hits.into_iter().map(|(_, reference)| reference).collect(),
        )))
    }

    pub fn status(&self, name: &str) -> ProviderResult<ProviderStatus> {
        let Some(partition) = self.known(name)? else {
            return Ok(ProviderStatus {
                state: ProviderState::Unavailable,
                as_of: None,
                notes: 0,
                excluded: BTreeMap::new(),
                partial: BTreeMap::new(),
                index_bytes: 0,
                semantic: self.semantic.as_ref().map(|_| ProviderKindStatus {
                    state: ProviderState::Unavailable,
                    as_of: None,
                    pending_notes: 0,
                }),
            });
        };
        let view = read(&partition.view);
        let as_of = view.served.as_ref().map(|s| s.as_of);
        let (notes, excluded, partial) = view.counts.clone();
        let semantic = self.semantic.as_ref().map(|semantic| {
            let vectors = read(&partition.semantic);
            let pending = lock(&partition.pending);
            let base = partition.full_seq.load(Ordering::Acquire);
            let failed = semantic.jobs.failed.load(Ordering::Acquire);
            let (state, kind_as_of) = semantic_state(as_of, base, &vectors, failed);
            ProviderKindStatus {
                state,
                as_of: kind_as_of,
                pending_notes: if failed { 0 } else { pending.by_id.len() },
            }
        });
        drop(view);
        Ok(ProviderStatus {
            state: if as_of.is_some() {
                ProviderState::Ready
            } else {
                ProviderState::Building
            },
            as_of,
            notes,
            excluded,
            partial,
            index_bytes: dir_bytes(&partition.dir),
            semantic,
        })
    }
}

fn write_lock<T>(lock: &RwLock<T>) -> std::sync::RwLockWriteGuard<'_, T> {
    lock.write()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn prepare_data_dir(data_dir: &Path) -> ProviderResult<PathBuf> {
    create_private_dir(data_dir)?;
    let marker = data_dir.join(DATA_MARKER);
    let empty = fs::read_dir(data_dir)
        .map_err(ProviderError::internal)?
        .next()
        .is_none();
    if !empty && !marker.is_file() {
        return Err(ProviderError::bad(
            "data directory is not empty and was not created by this mode",
        ));
    }
    restrict_to_owner(data_dir, 0o700)?;
    write_private_file(&marker, b"disposable provider store; safe to delete\n")?;
    let partitions_dir = data_dir.join("partitions");
    if partitions_dir.exists() {
        fs::remove_dir_all(&partitions_dir).map_err(ProviderError::internal)?;
    }
    create_private_dir(&partitions_dir)?;
    Ok(partitions_dir)
}

/// Classify coverage against the lexical snapshot under the view lock.
/// A previous full build (in particular a prior epoch) is not valid coverage.
fn semantic_state(
    lexical: Option<ProviderAsOf>,
    base: u64,
    state: &SemanticState,
    worker_failed: bool,
) -> (ProviderState, Option<ProviderAsOf>) {
    if worker_failed {
        (ProviderState::Unavailable, None)
    } else if state.base != base || lexical.is_none() {
        (ProviderState::Building, None)
    } else if state.failed {
        (ProviderState::Unavailable, None)
    } else if state.as_of == lexical {
        (ProviderState::Ready, state.as_of)
    } else {
        (ProviderState::Building, state.as_of)
    }
}

/// The net effect of one delta commit, per id (the last change wins).
fn delta_work(changes: Vec<RefChange>) -> EmbedWork {
    EmbedWork::Delta(
        changes
            .into_iter()
            .map(|change| {
                let key = change.reference.map(|reference| NoteKey {
                    id: change.id.clone(),
                    version: reference.version.clone(),
                    path: change.path,
                });
                (change.id, key)
            })
            .collect(),
    )
}

impl SemanticShared {
    fn coverage(
        &self,
        partition: &Partition,
        lexical: Option<ProviderAsOf>,
        state: &SemanticState,
    ) -> (ProviderState, Option<ProviderAsOf>) {
        semantic_state(
            lexical,
            partition.full_seq.load(Ordering::Acquire),
            state,
            self.jobs.failed.load(Ordering::Acquire),
        )
    }

    /// Never waits for passage embedding or a vector transaction. The view
    /// lock pairs pending accounting with the lexical commit that queued it.
    fn enqueue(&self, mut job: EmbedJob, live: &Generation) {
        let partition = Arc::clone(&job.partition);
        let mut queue = lock(&self.jobs.work);
        if self.jobs.failed.load(Ordering::Acquire) {
            return;
        }
        let queued = queue
            .iter()
            .position(|old| Arc::ptr_eq(&old.partition, &partition));
        let mut pending = lock(&partition.pending);
        if pending.base != job.base {
            *pending = PendingNotes {
                base: job.base,
                by_id: HashMap::new(),
            };
        }
        if let Some(index) = queued
            && let Some(previous) = queue.remove(index)
            && previous.base == job.base
        {
            pending.remove(&previous.work);
            if let EmbedWork::Delta(changes) = job.work {
                job.work = match previous.work {
                    EmbedWork::Full(mut notes) => {
                        for (id, change) in changes {
                            if let Some(note) = change {
                                notes.insert(id, note);
                            } else {
                                notes.remove(&id);
                            }
                        }
                        EmbedWork::Full(notes)
                    }
                    EmbedWork::Delta(mut earlier) => {
                        earlier.extend(changes);
                        EmbedWork::Delta(earlier)
                    }
                };
            }
        }
        if matches!(&job.work, EmbedWork::Delta(changes) if changes.len() > EMBED_MAX_PENDING_CHANGES)
        {
            job.work = EmbedWork::Full(live.searchable_notes());
        }
        pending.add(&job.work);
        let index = queued.unwrap_or(queue.len());
        queue.insert(index, job);
        drop(pending);
        // Full is fine: the worker already has a wakeup. If it disconnected,
        // no partition's backlog can be processed.
        if self.wake.try_send(()) == Err(mpsc::TrySendError::Disconnected(())) {
            self.jobs.fail(&mut queue, None);
        }
    }
}

/// Lexical answer under the recall obligation.
///
/// Tier 1 is every note that contains all terms: first those Kwiry ranked,
/// in Kwiry's order, then the rest by body-words BM25 (statistics over the
/// asked partitions only). Tier 2 is every other note Kwiry ranked, in
/// Kwiry's order. Tier 1 always comes first, so when it holds more than
/// `limit` notes the answer is made of tier-1 notes only, and when it holds
/// `limit` or fewer all of them are in the answer. Equal scores are ordered
/// by id bytes ascending.
fn superset_order(
    ranked: Vec<(Arc<ProviderReference>, Score)>,
    all_terms: Vec<(Arc<ProviderReference>, Score)>,
    limit: usize,
) -> Vec<Arc<ProviderReference>> {
    let key = |reference: &ProviderReference| (reference.partition.clone(), reference.id.clone());
    let matching: HashSet<_> = all_terms.iter().map(|(r, _)| key(r)).collect();
    let (mut tier_one, mut tier_two): (Vec<_>, Vec<_>) = ranked
        .into_iter()
        .partition(|(reference, _)| matching.contains(&key(reference)));
    tie_break_equal_runs(&mut tier_one);
    tie_break_equal_runs(&mut tier_two);
    let ranked_keys: HashSet<_> = tier_one.iter().map(|(r, _)| key(r)).collect();
    let mut rest: Vec<_> = all_terms
        .into_iter()
        .filter(|(reference, _)| !ranked_keys.contains(&key(reference)))
        .collect();
    let by_score = |left: &(Arc<ProviderReference>, Score),
                    right: &(Arc<ProviderReference>, Score)| {
        right
            .1
            .total_cmp(&left.1)
            .then_with(|| left.0.id.as_bytes().cmp(right.0.id.as_bytes()))
            .then_with(|| left.0.partition.cmp(&right.0.partition))
    };
    let need = limit.saturating_sub(tier_one.len());
    if rest.len() > need {
        if need == 0 {
            rest.clear();
        } else {
            rest.select_nth_unstable_by(need - 1, by_score);
            rest.truncate(need);
        }
    }
    rest.sort_by(by_score);
    tier_one
        .into_iter()
        .chain(rest)
        .chain(tier_two)
        .take(limit)
        .map(|(reference, _)| reference)
        .collect()
}

/// Orders each run of exactly equal scores by id bytes (then partition),
/// leaving the order between different scores as it was.
fn tie_break_equal_runs(entries: &mut [(Arc<ProviderReference>, Score)]) {
    let mut start = 0;
    while start < entries.len() {
        let mut end = start + 1;
        while end < entries.len() && entries[end].1.to_bits() == entries[start].1.to_bits() {
            end += 1;
        }
        entries[start..end].sort_by(|left, right| {
            left.0
                .id
                .as_bytes()
                .cmp(right.0.id.as_bytes())
                .then_with(|| left.0.partition.cmp(&right.0.partition))
        });
        start = end;
    }
}

/// Note-level reciprocal-rank fusion of a lexical and a semantic answer,
/// with the daemon's formula (`rrf_fuse`, k = 60). Equal fused scores are
/// ordered by id bytes ascending.
fn fuse(
    lexical: Vec<Arc<ProviderReference>>,
    semantic: Vec<Arc<ProviderReference>>,
    limit: usize,
) -> Vec<Arc<ProviderReference>> {
    let key =
        |reference: &ProviderReference| format!("{}\u{1f}{}", reference.partition, reference.id);
    let mut by_key: HashMap<String, Arc<ProviderReference>> = HashMap::new();
    // Both legs resolve against the same lexical commit, so overlap has the
    // same version in both lists.
    for reference in semantic.iter().chain(lexical.iter()) {
        by_key.insert(key(reference), Arc::clone(reference));
    }
    let lexical_keys: Vec<_> = lexical.iter().map(|r| key(r)).collect();
    let semantic_keys: Vec<_> = semantic.iter().map(|r| key(r)).collect();
    let mut fused: Vec<(Arc<ProviderReference>, f64)> =
        rrf_fuse(&lexical_keys, &semantic_keys, RRF_K)
            .into_iter()
            .filter_map(|(key, score)| by_key.get(&key).map(|r| (Arc::clone(r), score)))
            .collect();
    fused.sort_by(|left, right| {
        right
            .1
            .total_cmp(&left.1)
            .then_with(|| left.0.id.as_bytes().cmp(right.0.id.as_bytes()))
            .then_with(|| left.0.partition.cmp(&right.0.partition))
    });
    fused.truncate(limit);
    fused.into_iter().map(|(reference, _)| reference).collect()
}

/// Every note of the pinned partitions whose body words contain all
/// `terms`: `(index into pinned, BM25 score, slot)`. Statistics are summed
/// over exactly the pinned partitions.
fn all_terms_matches(
    pinned: &[(Arc<Partition>, Arc<Served>)],
    terms: &[String],
) -> ProviderResult<Vec<(usize, Score, u64)>> {
    if terms.is_empty() || pinned.is_empty() {
        return Ok(Vec::new());
    }
    let statistics = AuthorizedStatistics::new(
        pinned
            .iter()
            .map(|(_, served)| served.words.clone())
            .collect(),
    );
    let mut matches = Vec::new();
    for (index, (_, served)) in pinned.iter().enumerate() {
        let clauses: Vec<(Occur, Box<dyn Query>)> = terms
            .iter()
            .map(|term| {
                let query: Box<dyn Query> = Box::new(TermQuery::new(
                    Term::from_field_text(served.words_field, term),
                    IndexRecordOption::WithFreqs,
                ));
                (Occur::Must, query)
            })
            .collect();
        let hits = served
            .words
            .search_with_statistics_provider(
                &BooleanQuery::new(clauses),
                &SlotCollector,
                &statistics,
            )
            .map_err(ProviderError::internal)?;
        matches.extend(hits.into_iter().map(|(score, slot)| (index, score, slot)));
    }
    Ok(matches)
}

/// Collects `(score, slot)` of every matching body-words document.
struct SlotCollector;

struct SlotSegmentCollector {
    slots: Column<u64>,
    hits: Vec<(Score, u64)>,
}

impl Collector for SlotCollector {
    type Fruit = Vec<(Score, u64)>;
    type Child = SlotSegmentCollector;

    fn for_segment(
        &self,
        _segment_local_id: SegmentOrdinal,
        segment: &SegmentReader,
    ) -> tantivy::Result<Self::Child> {
        Ok(SlotSegmentCollector {
            slots: segment.fast_fields().u64("slot")?,
            hits: Vec::new(),
        })
    }

    fn requires_scoring(&self) -> bool {
        true
    }

    fn merge_fruits(&self, fruits: Vec<Vec<(Score, u64)>>) -> tantivy::Result<Self::Fruit> {
        Ok(fruits.into_iter().flatten().collect())
    }
}

impl SegmentCollector for SlotSegmentCollector {
    type Fruit = Vec<(Score, u64)>;

    fn collect(&mut self, doc: DocId, score: Score) {
        if let Some(slot) = self.slots.first(doc) {
            self.hits.push((score, slot));
        }
    }

    fn harvest(self) -> Self::Fruit {
        self.hits
    }
}

/// One worker, draining one coalesced job per partition in queue order.
fn embed_worker(
    jobs: &EmbedQueue,
    wake: mpsc::Receiver<()>,
    mut embedder: Box<dyn Embedder>,
    dimensions: usize,
) {
    while wake.recv().is_ok() {
        loop {
            let Some(job) = lock(&jobs.work).pop_front() else {
                break;
            };
            // Catch a panic only to retire the lane and account for the lost
            // work. Never reuse the embedder or its writer after unwinding.
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                // `true` means the job already settled while publishing coverage.
                let result = run_job(&job, embedder.as_mut(), dimensions);
                if !matches!(result, Ok(true)) {
                    let mut state = write_lock(&job.partition.semantic);
                    if let Err(error) = result
                        && !job.moot()
                    {
                        eprintln!("provider helper: embedding failed: {}", error.detail);
                        state.base = job.base;
                        state.failed = true;
                    }
                    job.settle();
                }
            }));
            if result.is_err() {
                jobs.fail(&mut lock(&jobs.work), Some(&job));
                return;
            }
        }
    }
}

/// Unpublished full builds can fail or be superseded at any point.
struct StagedVectors(PathBuf);

impl Drop for StagedVectors {
    fn drop(&mut self) {
        if !self.0.as_os_str().is_empty() {
            remove_vector_file(&self.0);
        }
    }
}

fn run_job(job: &EmbedJob, embedder: &mut dyn Embedder, dimensions: usize) -> ProviderResult<bool> {
    if job.moot() {
        return Ok(false);
    }
    {
        let state = read(&job.partition.semantic);
        // Only a new full feed (a different base) can recover a failure.
        if state.base == job.base && state.failed {
            return Ok(false);
        }
    }
    match &job.work {
        EmbedWork::Full(notes) => {
            let mut staged = StagedVectors(job.partition.dir.join(format!(
                "vectors-{}-{}.sqlite",
                job.base,
                unique_suffix()
            )));
            let mut conn = vectors::open(&staged.0, dimensions)?;
            let tx = vectors::begin(&mut conn)?;
            if !embed_notes(job, &tx, notes.values(), embedder, dimensions)? {
                return Ok(false);
            }
            vectors::commit(tx)?;
            let reader = vectors::open(&staged.0, dimensions)?;
            let mut writer = lock(&job.partition.vector_writer);
            {
                let mut state = write_lock(&job.partition.semantic);
                if job.moot() {
                    return Ok(false);
                }
                job.settle();
                *state = SemanticState {
                    snapshot: Some(Arc::new(VectorSnapshot {
                        read: Mutex::new(reader),
                        #[cfg(test)]
                        before_scan: Mutex::new(None),
                    })),
                    as_of: Some(job.served.as_of),
                    base: job.base,
                    failed: false,
                };
            }
            let path = std::mem::take(&mut staged.0);
            if let Some(previous) = writer.replace(VectorWriter { path, conn }) {
                let old = previous.path.clone();
                drop(previous);
                remove_vector_file(&old);
            }
            Ok(true)
        }
        EmbedWork::Delta(changes) => {
            let mut writer = lock(&job.partition.vector_writer);
            let writer = writer
                .as_mut()
                .ok_or_else(|| ProviderError::internal("a delta arrived before any full feed"))?;
            let tx = vectors::begin(&mut writer.conn)?;
            for (id, change) in changes {
                if change.is_none() {
                    vectors::remove_note(&tx, id)?;
                }
            }
            if !embed_notes(
                job,
                &tx,
                changes.values().filter_map(Option::as_ref),
                embedder,
                dimensions,
            )? {
                return Ok(false);
            }
            // While passages are embedded, WAL readers still see the last
            // transaction. Hold the semantic lock only for publication.
            let mut state = write_lock(&job.partition.semantic);
            if job.moot() {
                return Ok(false);
            }
            vectors::commit(tx)?;
            job.settle();
            state.as_of = Some(job.served.as_of);
            Ok(true)
        }
    }
}

fn embed_notes<'a>(
    job: &EmbedJob,
    tx: &rusqlite::Transaction<'_>,
    notes: impl Iterator<Item = &'a NoteKey>,
    embedder: &mut dyn Embedder,
    dimensions: usize,
) -> ProviderResult<bool> {
    let mut texts = Vec::with_capacity(EMBED_CHUNKS_PER_BATCH);
    let mut slots = Vec::with_capacity(EMBED_CHUNKS_PER_BATCH);
    let mut bytes = 0;
    for note in notes {
        if job.moot() {
            return Ok(false);
        }
        let slot = vectors::replace_note(tx, &note.id, &note.version)?;
        for address in chunk_addresses(&job.served, &note.path)? {
            let text = chunk_text(&job.served, address)?;
            if !texts.is_empty()
                && (texts.len() == EMBED_CHUNKS_PER_BATCH
                    || bytes + text.len() > EMBED_BYTES_PER_BATCH)
            {
                if job.moot() {
                    return Ok(false);
                }
                embed_batch(tx, embedder, dimensions, &mut texts, &mut slots)?;
                bytes = 0;
            }
            bytes += text.len();
            texts.push(text);
            slots.push(slot);
        }
    }
    if job.moot() {
        return Ok(false);
    }
    embed_batch(tx, embedder, dimensions, &mut texts, &mut slots)?;
    Ok(true)
}

fn embed_batch(
    tx: &rusqlite::Transaction<'_>,
    embedder: &mut dyn Embedder,
    dimensions: usize,
    texts: &mut Vec<String>,
    slots: &mut Vec<i64>,
) -> ProviderResult<()> {
    let embedded = embed_by_length(embedder, std::mem::take(texts))?;
    if embedded.iter().any(|v| v.len() != dimensions) {
        return Err(ProviderError::internal(
            "the embedder returned the wrong width of vectors",
        ));
    }
    vectors::insert_chunks(tx, slots.drain(..).zip(embedded))
}

/// Embeds `texts` in order of length and returns the vectors in the original
/// order. The model pads each batch to its longest text, so grouping texts of
/// similar length saves most of the padding work.
fn embed_by_length(
    embedder: &mut dyn Embedder,
    mut texts: Vec<String>,
) -> ProviderResult<Vec<Vec<f32>>> {
    if texts.is_empty() {
        return Ok(Vec::new());
    }
    let mut order: Vec<usize> = (0..texts.len()).collect();
    order.sort_by_key(|&index| texts[index].len());
    let sorted: Vec<String> = order
        .iter()
        .map(|&index| std::mem::take(&mut texts[index]))
        .collect();
    let vectors = embedder
        .embed_passages(&sorted)
        .map_err(ProviderError::internal)?;
    if vectors.len() != texts.len() {
        return Err(ProviderError::internal(
            "the embedder returned the wrong number of vectors",
        ));
    }
    let mut out = vec![Vec::new(); texts.len()];
    for (vector, index) in vectors.into_iter().zip(order) {
        out[index] = vector;
    }
    Ok(out)
}

fn chunk_addresses(served: &Served, path: &str) -> ProviderResult<Vec<tantivy::DocAddress>> {
    let query = TermQuery::new(
        Term::from_field_text(
            served.source_key,
            &room_source_key(&served.owner, None, path),
        ),
        IndexRecordOption::Basic,
    );
    let mut addresses: Vec<_> = served
        .searcher
        .search(&query, &DocSetCollector)
        .map_err(ProviderError::internal)?
        .into_iter()
        .collect();
    addresses.sort();
    Ok(addresses)
}

fn chunk_text(served: &Served, address: tantivy::DocAddress) -> ProviderResult<String> {
    let document: TantivyDocument = served
        .searcher
        .doc(address)
        .map_err(ProviderError::internal)?;
    let heading: Vec<String> = document
        .get_first(served.fields.heading_path)
        .and_then(|value| value.as_str())
        .and_then(|value| serde_json::from_str(value).ok())
        .unwrap_or_default();
    let content = document
        .get_first(served.fields.content)
        .and_then(|value| value.as_str())
        .unwrap_or_default();
    Ok(embedding_text(&heading, content))
}

fn remove_vector_file(path: &Path) {
    let _ = fs::remove_file(path);
    for suffix in ["-wal", "-shm"] {
        let mut name = path.as_os_str().to_owned();
        name.push(suffix);
        let _ = fs::remove_file(PathBuf::from(name));
    }
}

fn prepare(owner: &str, path: &str, bytes: &[u8]) -> ProviderResult<Prepared> {
    let excluded = |reason: &str| Prepared {
        outcome: NoteOutcome::Excluded(reason.to_owned()),
        documents: Vec::new(),
        words: Vec::new(),
    };
    let Some(format) = SourceFormat::from_path(path) else {
        return Ok(excluded("unsupported_format"));
    };
    let descriptor = SourceDescriptor {
        vault_id: owner.to_owned(),
        room: None,
        path: path.to_owned(),
        format,
        byte_length: bytes.len() as u64,
        mtime: 0,
        mtime_nanos: 0,
    };
    let Ok(preparation) = prepare_source_buffer(&descriptor, bytes) else {
        return Ok(excluded("invalid_source"));
    };
    if preparation.kind == SourcePreparationKind::Skipped || preparation.chunks.is_empty() {
        return Ok(excluded(match preparation.coverage {
            ExtractionCoverage::Unreadable => "unreadable",
            ExtractionCoverage::Quarantined => "quarantined",
            _ => "no_extractable_text",
        }));
    }
    static FIELDS: OnceLock<Option<Fields>> = OnceLock::new();
    let fields = FIELDS
        .get_or_init(|| Fields::from_schema(&build_schema()).ok())
        .as_ref()
        .ok_or_else(|| ProviderError::internal("schema fields are unavailable"))?;
    let documents = preparation
        .chunks
        .iter()
        .map(|chunk| chunk_document(fields, chunk, &preparation.retrieval))
        .collect::<crate::Result<Vec<_>>>()
        .map_err(ProviderError::internal)?;
    let outcome = if preparation.coverage == ExtractionCoverage::IndexedComplete {
        NoteOutcome::Indexed
    } else if preparation
        .warning
        .as_deref()
        .is_some_and(|warning| warning.contains("frontmatter"))
    {
        NoteOutcome::Partial("frontmatter_not_extracted")
    } else {
        NoteOutcome::Partial("partial_extraction")
    };
    Ok(Prepared {
        outcome,
        documents,
        // Not UTF-8: the host's matcher cannot read it either, so it owes
        // no body words.
        words: body_words::note_words(bytes).unwrap_or_default(),
    })
}

fn validate_partition(name: &str) -> ProviderResult<()> {
    if name.is_empty()
        || name.len() > PROVIDER_MAX_PARTITION_BYTES
        || !name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-')
    {
        return Err(ProviderError::new(
            ProviderErrorCode::NotFound,
            "partition must match [A-Za-z0-9_-]{1,64}",
        ));
    }
    Ok(())
}

fn validate_id(id: &str) -> ProviderResult<()> {
    if id.is_empty() || id.len() > PROVIDER_MAX_ID_BYTES || id.chars().any(char::is_control) {
        return Err(ProviderError::bad("id must be 1..=256 bytes"));
    }
    Ok(())
}

fn validate_version(version: &str) -> ProviderResult<()> {
    if version.len() != 64
        || !version
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(ProviderError::bad("version must be 64 lowercase hex"));
    }
    Ok(())
}

fn validate_path(path: &str) -> ProviderResult<()> {
    if path.is_empty()
        || path.len() > PROVIDER_MAX_PATH_BYTES
        || path.chars().any(char::is_control)
        || path.starts_with('/')
        || path.ends_with('/')
        || path.contains('\\')
        || path
            .split('/')
            .any(|component| component.is_empty() || matches!(component, "." | ".."))
    {
        return Err(ProviderError::bad(
            "path must be a normalized relative forward-slash path",
        ));
    }
    Ok(())
}

/// Creates `dir` (and missing parents) private to the owner (0700 on Unix).
fn create_private_dir(dir: &Path) -> ProviderResult<()> {
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
    builder.create(dir).map_err(ProviderError::internal)
}

/// Sets `path`'s permission bits on Unix; a directory that already existed
/// keeps its old mode through `create_private_dir`.
fn restrict_to_owner(path: &Path, mode: u32) -> ProviderResult<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(mode))
            .map_err(ProviderError::internal)?;
    }
    #[cfg(not(unix))]
    let _ = (path, mode);
    Ok(())
}

fn write_private_file(path: &Path, bytes: &[u8]) -> ProviderResult<()> {
    fs::write(path, bytes).map_err(ProviderError::internal)?;
    restrict_to_owner(path, 0o600)
}

pub fn hex_sha256(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn owner_tag(partition: &str) -> String {
    hex_sha256(partition.as_bytes())
}

fn unique_suffix() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_nanos())
}

fn dir_bytes(dir: &Path) -> u64 {
    let Ok(entries) = fs::read_dir(dir) else {
        return 0;
    };
    entries
        .flatten()
        .map(|entry| match entry.file_type() {
            Ok(kind) if kind.is_dir() => dir_bytes(&entry.path()),
            Ok(_) => entry.metadata().map_or(0, |metadata| metadata.len()),
            Err(_) => 0,
        })
        .sum()
}

#[cfg(test)]
mod tests;
