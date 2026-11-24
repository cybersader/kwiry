// SPDX-License-Identifier: MIT OR Apache-2.0
//! Experimental push-fed search provider store (trial only).
//!
//! A host feeds notes into named partitions and asks which notes might
//! contain some words. Notes are prepared with the same source preparation,
//! chunking and lexical analyser as the daemon, and ranked by the same
//! lexical-v2 ladder; answers are note references only.
//!
//! Storage is separate from the daemon's generation machinery on purpose:
//! each partition is one disposable Tantivy index, updated in place by a
//! `delta` feed and rebuilt into a fresh index by a `full` feed. Writes become
//! durable at a checkpoint and visible at the commit after it. Nothing here
//! reads a workspace on disk and nothing is kept across restarts.
//!
//! Desktop and non-governed: no access control, no member identity, no
//! excerpt or score on the way out. See `docs/provider-helper-trial.md`.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, RwLock};

use sha2::{Digest, Sha256};
use tantivy::schema::Field;
use tantivy::{Index, IndexReader, IndexWriter, ReloadPolicy, Searcher, Term};

use crate::api::SearchFilters;
use crate::extract::ExtractionCoverage;
use crate::format::SourceFormat;
use crate::index::{Fields, build_schema, chunk_document, register_lexical_analyzer};
use crate::model::MAX_FILE_BYTES;
use crate::query::MAX_TOTAL_CANDIDATES;
use crate::search::{PinnedSearcher, search_pinned_with_quality};
use crate::source::{
    SourceDescriptor, SourcePreparationKind, prepare_source_buffer, room_source_key,
};

/// Largest note body accepted: the daemon's per-file cap (`MAX_FILE_BYTES`).
pub const PROVIDER_MAX_ITEM_BYTES: u64 = MAX_FILE_BYTES;
pub const PROVIDER_MAX_PARTITION_BYTES: usize = 64;
pub const PROVIDER_MAX_ID_BYTES: usize = 256;
pub const PROVIDER_MAX_PATH_BYTES: usize = 1024;
pub const PROVIDER_MAX_LIMIT: usize = 200;
pub const PROVIDER_MAX_QUERY_PARTITIONS: usize = 64;
/// Exclusion reason for a note refused by the transport before its body was
/// read.
pub const PROVIDER_OVER_TRANSPORT_CAP: &str = "over_transport_cap";

const WRITER_MEMORY_BYTES: usize = 50_000_000;
const DATA_MARKER: &str = ".provider-store";
/// How often a query re-runs when a commit lands between its search and its
/// reference lookup.
const QUERY_ATTEMPTS: usize = 4;

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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderReference {
    pub partition: String,
    pub id: String,
    pub version: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderCoverage {
    pub partition: String,
    pub state: ProviderState,
    pub as_of: Option<ProviderAsOf>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderQueryOutcome {
    pub coverage: Vec<ProviderCoverage>,
    pub refs: Vec<ProviderReference>,
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
    outcome: NoteOutcome,
}

impl NoteRecord {
    fn searchable(&self) -> bool {
        !matches!(self.outcome, NoteOutcome::Excluded(_))
    }
}

/// A change to the path → reference map that queries resolve hits through.
type RefChange = (String, Option<ProviderReference>);

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
    index: Index,
    fields: Fields,
    source_key: Field,
    writer: IndexWriter,
    reader: IndexReader,
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
        Ok(Self {
            dir,
            partition: partition.to_owned(),
            owner: owner_tag(partition),
            epoch,
            n,
            index,
            fields,
            source_key,
            writer,
            reader,
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
            self.pending_refs.push((record.path.clone(), None));
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
        let record = NoteRecord {
            version: version.to_owned(),
            path: path.to_owned(),
            outcome: prepared.outcome,
        };
        if record.searchable() {
            self.ids_by_path.insert(path.to_owned(), id.to_owned());
            self.pending_refs.push((
                path.to_owned(),
                Some(ProviderReference {
                    partition: self.partition.clone(),
                    id: id.to_owned(),
                    version: version.to_owned(),
                }),
            ));
        }
        self.notes.insert(id.to_owned(), record);
        Ok(())
    }

    /// Makes the writes so far durable as checkpoint `n` (not yet visible).
    fn checkpoint(&mut self, n: u64) -> ProviderResult<()> {
        self.writer.commit().map_err(ProviderError::internal)?;
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

    /// Reloads the reader onto the last checkpoint and returns what a query
    /// should see, plus the reference changes that came with it.
    fn publish(&mut self) -> ProviderResult<(Served, Vec<RefChange>)> {
        self.rollback()?;
        self.reader.reload().map_err(ProviderError::internal)?;
        let served = Served {
            owner: self.owner.clone(),
            index: self.index.clone(),
            fields: self.fields.clone(),
            searcher: self.reader.searcher(),
            as_of: ProviderAsOf {
                epoch: self.epoch,
                n: self.n,
            },
        };
        Ok((served, std::mem::take(&mut self.checkpointed_refs)))
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

/// What a query reads: one pinned searcher at one committed `asOf`.
struct Served {
    owner: String,
    index: Index,
    fields: Fields,
    searcher: Searcher,
    as_of: ProviderAsOf,
}

/// Note counts of one commit: searchable notes, excluded and partial notes by
/// reason.
type Counts = (usize, BTreeMap<String, usize>, BTreeMap<String, usize>);

/// The committed view of a partition: the searcher, the path → reference map
/// and the note counts for the same commit, swapped together.
#[derive(Default)]
struct View {
    served: Option<Arc<Served>>,
    refs: HashMap<String, ProviderReference>,
    counts: Counts,
}

struct Prepared {
    outcome: NoteOutcome,
    documents: Vec<tantivy::TantivyDocument>,
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
                    if self
                        .committed
                        .is_some_and(|committed| live.n != committed.n)
                    {
                        self.needs_full = true;
                    }
                    live.rollback()?;
                }
            }
        }
        Ok(())
    }

    fn target(&mut self) -> ProviderResult<&mut Generation> {
        match &mut self.feed {
            Some(Feed {
                staging: Some(staging),
                ..
            }) => Ok(staging),
            Some(Feed { staging: None, .. }) => {
                self.live.as_mut().ok_or_else(ProviderError::no_open_feed)
            }
            None => Err(ProviderError::no_open_feed()),
        }
    }
}

struct Partition {
    dir: PathBuf,
    writes: Mutex<Writes>,
    view: RwLock<View>,
}

impl Partition {
    fn lock(&self) -> MutexGuard<'_, Writes> {
        self.writes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn served(&self) -> Option<Arc<Served>> {
        self.view
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .served
            .clone()
    }
}

/// The push-fed store: every partition the host has begun and not retired.
pub struct ProviderStore {
    partitions_dir: PathBuf,
    partitions: Mutex<BTreeMap<String, Arc<Partition>>>,
}

impl ProviderStore {
    /// Opens a store in `data_dir`, discarding anything a previous run left
    /// there. Refuses a non-empty directory this store did not create, so a
    /// mistyped path cannot wipe unrelated data.
    ///
    /// Directories it creates are private to the user (0700 on Unix); the
    /// files inside follow the process umask, which the helper sets to 0077.
    pub fn open(data_dir: &Path) -> ProviderResult<Self> {
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
        Ok(Self {
            partitions_dir,
            partitions: Mutex::new(BTreeMap::new()),
        })
    }

    fn map(&self) -> MutexGuard<'_, BTreeMap<String, Arc<Partition>>> {
        self.partitions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
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
                Arc::new(Partition {
                    dir: self.partitions_dir.join(owner_tag(name)),
                    writes: Mutex::new(Writes::default()),
                    view: RwLock::new(View::default()),
                })
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
    /// visible; writes after that checkpoint are dropped.
    pub fn commit(&self, name: &str) -> ProviderResult<ProviderAsOf> {
        let partition = self.feeding(name)?;
        let mut writes = partition.lock();
        let feed = writes.feed.take().ok_or_else(ProviderError::no_open_feed)?;
        let as_of = match feed.mode {
            ProviderFeedMode::Delta => {
                let live = writes
                    .live
                    .as_mut()
                    .ok_or_else(ProviderError::no_open_feed)?;
                let (served, changes) = live.publish()?;
                let as_of = served.as_of;
                let counts = live.status_counts();
                let mut view = partition
                    .view
                    .write()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                apply_ref_changes(&mut view.refs, changes);
                view.served = Some(Arc::new(served));
                view.counts = counts;
                as_of
            }
            ProviderFeedMode::Full => {
                let mut staging = feed
                    .staging
                    .ok_or_else(|| ProviderError::internal("full feed without an index"))?;
                let (served, changes) = staging.publish()?;
                let as_of = served.as_of;
                let mut refs = HashMap::with_capacity(staging.ids_by_path.len());
                apply_ref_changes(&mut refs, changes);
                {
                    let mut view = partition
                        .view
                        .write()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    *view = View {
                        served: Some(Arc::new(served)),
                        refs,
                        counts: staging.status_counts(),
                    };
                }
                if let Some(previous) = writes.live.replace(staging) {
                    let dir = previous.dir.clone();
                    drop(previous);
                    let _ = fs::remove_dir_all(dir);
                }
                writes.needs_full = false;
                as_of
            }
        };
        writes.committed = Some(as_of);
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
        let mut writes = partition.lock();
        *partition
            .view
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = View::default();
        *writes = Writes::default();
        let _ = fs::remove_dir_all(&partition.dir);
        Ok(())
    }

    /// Ranks `text` over the requested partitions together: BM25 statistics
    /// are summed over exactly the partitions that are ready, and each
    /// partition reports the commit it was read at.
    pub fn query(
        &self,
        names: &[String],
        text: &str,
        limit: usize,
    ) -> ProviderResult<ProviderQueryOutcome> {
        if names.is_empty() || names.len() > PROVIDER_MAX_QUERY_PARTITIONS {
            return Err(ProviderError::bad("partitions must name 1..=64"));
        }
        for (index, name) in names.iter().enumerate() {
            validate_partition(name).map_err(|_| ProviderError::bad("invalid partition name"))?;
            if names[..index].contains(name) {
                return Err(ProviderError::bad("a partition is named twice"));
            }
        }
        if limit == 0 || limit > PROVIDER_MAX_LIMIT {
            return Err(ProviderError::bad("limit must be 1..=200"));
        }
        for _ in 0..QUERY_ATTEMPTS {
            if let Some(outcome) = self.query_once(names, text, limit)? {
                return Ok(outcome);
            }
        }
        Err(ProviderError::new(
            ProviderErrorCode::Unavailable,
            "commits kept landing during the query",
        ))
    }

    /// One attempt; `None` when a commit landed between the search and the
    /// reference lookup, so the hits cannot be paired with one `asOf`.
    fn query_once(
        &self,
        names: &[String],
        text: &str,
        limit: usize,
    ) -> ProviderResult<Option<ProviderQueryOutcome>> {
        let mut coverage = Vec::with_capacity(names.len());
        let mut pinned = Vec::new();
        for name in names {
            let partition = self.map().get(name).cloned();
            let served = partition.as_ref().and_then(|p| p.served());
            let state = match (&partition, &served) {
                (_, Some(_)) => ProviderState::Ready,
                (Some(_), None) => ProviderState::Building,
                (None, None) => ProviderState::Unavailable,
            };
            coverage.push(ProviderCoverage {
                partition: name.clone(),
                state,
                as_of: served.as_ref().map(|s| s.as_of),
            });
            if let (Some(partition), Some(served)) = (partition, served) {
                pinned.push((partition, served));
            }
        }
        if text.trim().is_empty() || pinned.is_empty() {
            return Ok(Some(ProviderQueryOutcome {
                coverage,
                refs: Vec::new(),
            }));
        }
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
        let outcome = search_pinned_with_quality(
            &targets,
            text,
            MAX_TOTAL_CANDIDATES,
            &SearchFilters::default(),
        )
        .map_err(|error| match error {
            crate::Error::Query(message) => ProviderError::bad(message),
            other => ProviderError::internal(other),
        })?;
        let views: Vec<_> = pinned
            .iter()
            .map(|(partition, _)| {
                partition
                    .view
                    .read()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
            })
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
        let mut seen = HashSet::new();
        let mut refs = Vec::with_capacity(limit);
        for hit in outcome.hits {
            if refs.len() == limit {
                break;
            }
            if !seen.insert((hit.vault_id.clone(), hit.path.clone())) {
                continue;
            }
            let reference = pinned
                .iter()
                .zip(&views)
                .find(|((_, served), _)| served.owner == hit.vault_id)
                .and_then(|(_, view)| view.refs.get(&hit.path))
                .ok_or_else(|| ProviderError::internal("a hit has no reference at its commit"))?;
            refs.push(reference.clone());
        }
        Ok(Some(ProviderQueryOutcome { coverage, refs }))
    }

    /// What the last commit made visible. Writes of an open or abandoned
    /// feed are not counted.
    pub fn status(&self, name: &str) -> ProviderResult<ProviderStatus> {
        let Some(partition) = self.known(name)? else {
            return Ok(ProviderStatus {
                state: ProviderState::Unavailable,
                as_of: None,
                notes: 0,
                excluded: BTreeMap::new(),
                partial: BTreeMap::new(),
                index_bytes: 0,
            });
        };
        let (served, (notes, excluded, partial)) = {
            let view = partition
                .view
                .read()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            (view.served.clone(), view.counts.clone())
        };
        Ok(ProviderStatus {
            state: if served.is_some() {
                ProviderState::Ready
            } else {
                ProviderState::Building
            },
            as_of: served.map(|s| s.as_of),
            notes,
            excluded,
            partial,
            index_bytes: dir_bytes(&partition.dir),
        })
    }
}

fn apply_ref_changes(refs: &mut HashMap<String, ProviderReference>, changes: Vec<RefChange>) {
    for (path, reference) in changes {
        match reference {
            Some(reference) => {
                refs.insert(path, reference);
            }
            None => {
                refs.remove(&path);
            }
        }
    }
}

fn prepare(owner: &str, path: &str, bytes: &[u8]) -> ProviderResult<Prepared> {
    let excluded = |reason: &str| Prepared {
        outcome: NoteOutcome::Excluded(reason.to_owned()),
        documents: Vec::new(),
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
    Ok(Prepared { outcome, documents })
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
mod tests {
    use super::*;

    fn put(store: &ProviderStore, p: &str, id: &str, path: &str, body: &str) {
        store
            .put(p, id, &hex_sha256(body.as_bytes()), path, body.as_bytes())
            .unwrap();
    }

    fn ids(store: &ProviderStore, partitions: &[&str], q: &str) -> Vec<String> {
        let names: Vec<_> = partitions.iter().map(|p| (*p).to_owned()).collect();
        let mut ids: Vec<_> = store
            .query(&names, q, 10)
            .unwrap()
            .refs
            .into_iter()
            .map(|r| format!("{}/{}", r.partition, r.id))
            .collect();
        ids.sort_unstable();
        ids
    }

    fn state(store: &ProviderStore, p: &str) -> (ProviderState, Option<ProviderAsOf>) {
        let coverage = &store.query(&[p.to_owned()], "x", 5).unwrap().coverage[0];
        (coverage.state, coverage.as_of)
    }

    #[test]
    fn provider_feeds_become_visible_only_at_commit() {
        let dir = tempfile::tempdir().unwrap();
        let store = ProviderStore::open(dir.path()).unwrap();
        assert_eq!(state(&store, "p"), (ProviderState::Unavailable, None));
        store.begin("p", 1, ProviderFeedMode::Full).unwrap();
        assert_eq!(state(&store, "p"), (ProviderState::Building, None));
        put(
            &store,
            "p",
            "a",
            "Areas/alpha.md",
            "# Alpha\n\nquokka habitat\n",
        );
        put(&store, "p", "b", "beta.md", "# Beta\n\nunrelated\n");
        put(
            &store,
            "p",
            "c",
            "broken.md",
            "---\n: : [\n---\nbody quokka\n",
        );
        put(&store, "p", "d", "blob.bin", "quokka");
        store.checkpoint("p", 1).unwrap();
        put(
            &store,
            "p",
            "late",
            "late.md",
            "quokka after the checkpoint\n",
        );
        assert_eq!(store.commit("p").unwrap(), ProviderAsOf { epoch: 1, n: 1 });
        assert_eq!(ids(&store, &["p"], "quokka"), ["p/a", "p/c"]);

        store.begin("p", 1, ProviderFeedMode::Delta).unwrap();
        put(&store, "p", "b", "beta.md", "# Beta\n\nquokka quokka\n");
        store.delete("p", "a").unwrap();
        store.checkpoint("p", 2).unwrap();
        assert_eq!(
            ids(&store, &["p"], "quokka"),
            ["p/a", "p/c"],
            "not before commit"
        );
        assert_eq!(store.commit("p").unwrap(), ProviderAsOf { epoch: 1, n: 2 });
        assert_eq!(ids(&store, &["p"], "quokka"), ["p/b", "p/c"]);

        store.begin("p", 1, ProviderFeedMode::Delta).unwrap();
        let refused = store.checkpoint("p", 2).unwrap_err();
        assert_eq!(refused.code, ProviderErrorCode::CheckpointNotIncreasing);
        let mismatch = store.put("p", "e", &hex_sha256(b"other"), "e.md", b"body");
        assert_eq!(
            mismatch.unwrap_err().code,
            ProviderErrorCode::VersionMismatch
        );
        store.exclude_over_transport_cap("p", "huge").unwrap();
        store.checkpoint("p", 3).unwrap();
        store.commit("p").unwrap();

        let status = store.status("p").unwrap();
        assert_eq!(status.notes, 2);
        assert_eq!(status.excluded.get("unsupported_format"), Some(&1));
        assert_eq!(status.excluded.get(PROVIDER_OVER_TRANSPORT_CAP), Some(&1));
        assert_eq!(status.partial.get("frontmatter_not_extracted"), Some(&1));

        assert_eq!(
            store
                .begin("p", 2, ProviderFeedMode::Delta)
                .unwrap_err()
                .code,
            ProviderErrorCode::EpochMismatch
        );
        // A full resync keeps serving epoch 1 until it commits; ids it did
        // not receive are gone afterwards.
        store.begin("p", 2, ProviderFeedMode::Full).unwrap();
        put(&store, "p", "z", "zeta.md", "quokka\n");
        store.checkpoint("p", 1).unwrap();
        assert_eq!(state(&store, "p").1, Some(ProviderAsOf { epoch: 1, n: 3 }));
        store.commit("p").unwrap();
        assert_eq!(ids(&store, &["p"], "quokka"), ["p/z"]);
        assert_eq!(
            store
                .begin("p", 1, ProviderFeedMode::Full)
                .unwrap_err()
                .code,
            ProviderErrorCode::EpochRegressed
        );
        assert_eq!(
            store.delete("p", "z").unwrap_err().code,
            ProviderErrorCode::NoOpenFeed
        );

        store.retire("p").unwrap();
        assert_eq!(state(&store, "p").0, ProviderState::Unavailable);
    }

    #[test]
    fn provider_keeps_same_paths_in_two_partitions_apart() {
        let dir = tempfile::tempdir().unwrap();
        let store = ProviderStore::open(dir.path()).unwrap();
        for name in ["p", "q"] {
            store.begin(name, 1, ProviderFeedMode::Full).unwrap();
            put(
                &store,
                name,
                "same",
                "shared.md",
                "# Shared\n\nwombat burrow\n",
            );
            store.checkpoint(name, 1).unwrap();
            store.commit(name).unwrap();
        }
        assert_eq!(
            ids(&store, &["p", "q", "r"], "wombat"),
            ["p/same", "q/same"]
        );
        assert_eq!(ids(&store, &["q"], "wombat"), ["q/same"]);
        let names = ["p".to_owned(), "p".to_owned()];
        assert_eq!(
            store.query(&names, "wombat", 5).unwrap_err().code,
            ProviderErrorCode::BadRequest
        );
    }

    #[test]
    fn provider_never_serves_an_abandoned_delta_checkpoint() {
        let dir = tempfile::tempdir().unwrap();
        let store = ProviderStore::open(dir.path()).unwrap();
        store.begin("p", 1, ProviderFeedMode::Full).unwrap();
        put(&store, "p", "a", "alpha.md", "# Alpha\n\nquokka\n");
        store.checkpoint("p", 1).unwrap();
        assert_eq!(store.commit("p").unwrap(), ProviderAsOf { epoch: 1, n: 1 });

        // A delta that checkpointed and was then abandoned by a new begin.
        store.begin("p", 1, ProviderFeedMode::Delta).unwrap();
        put(&store, "p", "ghost", "ghost.md", "# Ghost\n\nquokka\n");
        store.checkpoint("p", 2).unwrap();
        let code = |result: ProviderResult<()>| result.unwrap_err().code;
        assert_eq!(
            code(store.begin("p", 1, ProviderFeedMode::Delta)),
            ProviderErrorCode::EpochMismatch
        );
        assert_eq!(
            code(store.checkpoint("p", 2)),
            ProviderErrorCode::NoOpenFeed
        );
        assert_eq!(
            store.commit("p").unwrap_err().code,
            ProviderErrorCode::NoOpenFeed
        );
        assert_eq!(ids(&store, &["p"], "quokka"), ["p/a"]);
        assert_eq!(state(&store, "p").1, Some(ProviderAsOf { epoch: 1, n: 1 }));
        let status = store.status("p").unwrap();
        assert_eq!(status.notes, 1, "status counts the commit, not the ghost");

        // A full resync with the same epoch counts checkpoints from the
        // commit, so the host's resend of 2 is accepted.
        store.begin("p", 1, ProviderFeedMode::Full).unwrap();
        put(&store, "p", "a", "alpha.md", "# Alpha\n\nquokka\n");
        store.checkpoint("p", 2).unwrap();
        assert_eq!(store.commit("p").unwrap(), ProviderAsOf { epoch: 1, n: 2 });
        assert_eq!(ids(&store, &["p"], "quokka"), ["p/a"]);
        store.begin("p", 1, ProviderFeedMode::Delta).unwrap();
        store.checkpoint("p", 3).unwrap();
        assert_eq!(store.commit("p").unwrap(), ProviderAsOf { epoch: 1, n: 3 });

        // A delta abandoned before any checkpoint leaves nothing behind.
        store.begin("p", 1, ProviderFeedMode::Delta).unwrap();
        put(&store, "p", "b", "beta.md", "# Beta\n\nquokka\n");
        store.begin("p", 1, ProviderFeedMode::Delta).unwrap();
        store.checkpoint("p", 4).unwrap();
        assert_eq!(store.commit("p").unwrap(), ProviderAsOf { epoch: 1, n: 4 });
        assert_eq!(ids(&store, &["p"], "quokka"), ["p/a"]);
    }

    #[test]
    fn provider_remembers_an_abandoned_higher_epoch() {
        let dir = tempfile::tempdir().unwrap();
        let store = ProviderStore::open(dir.path()).unwrap();
        store.begin("p", 1, ProviderFeedMode::Full).unwrap();
        store.checkpoint("p", 1).unwrap();
        store.commit("p").unwrap();
        // A full at epoch 3 is abandoned by a delta at the committed epoch.
        store.begin("p", 3, ProviderFeedMode::Full).unwrap();
        store.begin("p", 1, ProviderFeedMode::Delta).unwrap();
        store.checkpoint("p", 2).unwrap();
        assert_eq!(store.commit("p").unwrap(), ProviderAsOf { epoch: 1, n: 2 });
        assert_eq!(
            store
                .begin("p", 2, ProviderFeedMode::Full)
                .unwrap_err()
                .code,
            ProviderErrorCode::EpochRegressed,
            "the delta must not lower the epoch record"
        );
        store.begin("p", 3, ProviderFeedMode::Full).unwrap();
        store.checkpoint("p", 1).unwrap();
        assert_eq!(store.commit("p").unwrap(), ProviderAsOf { epoch: 3, n: 1 });
    }

    #[cfg(unix)]
    #[test]
    fn provider_data_dir_is_private() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let data = dir.path().join("data");
        let _store = ProviderStore::open(&data).unwrap();
        let mode = |path: &Path| fs::metadata(path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&data), 0o700);
        assert_eq!(mode(&data.join("partitions")), 0o700);
        assert_eq!(mode(&data.join(DATA_MARKER)), 0o600);
    }

    #[test]
    fn provider_refuses_a_foreign_data_dir() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("keep.txt"), b"x").unwrap();
        assert!(ProviderStore::open(dir.path()).is_err());
        assert!(dir.path().join("keep.txt").is_file());
    }
}
