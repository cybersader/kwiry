use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use sha2::{Digest, Sha256};

use crate::status::{IndexFreshness, IndexFreshnessState};
use std::sync::atomic::{AtomicU8, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use arc_swap::ArcSwapOption;
use tantivy::IndexWriter;
use tantivy::index::SegmentId;
use tantivy::schema::Field;
use tantivy::{Index, IndexReader, ReloadPolicy, Term};

use crate::api::SearchFilters;
use crate::error::{Error, Result};
use crate::generation::{DataRoot, DataRootLock};
use crate::index::{
    Fields, build_schema, chunk_document, open_index_dir, partition_sources,
    register_lexical_analyzer, source_matches,
};
use crate::manifest::{EvictionReport, Manifest, ManifestFile, registration_fingerprint};
use crate::model::{
    Config, FileIngestOutcome, FileOutcomeKind, HostProfile, IndexFreshnessBasis, IngestWarning,
    LexicalSearchRequest, PreparedChunk, ResourceKey, RetrievalMetadata, SearchHit, SnapshotId,
};
use crate::partition::{GenerationLayout, PartitionEvidence, partition_index_dir};
use crate::ranking::LexicalMatchQuality;
#[cfg(feature = "internal-d5c-preview")]
use crate::ranking::RelevanceProfile;
#[cfg(feature = "internal-d5c-preview")]
use crate::ranking_eval::{
    BalancedComparisonEnvelope, BalancedPlaygroundCase, BalancedPlaygroundConfiguration,
    evaluate_balanced_playground,
};
use crate::reconcile::{
    AuditBudget, ObservationDecision, ObservationPolicy, PartitionScope, ReadReason, ReconcilePlan,
    ReconcileScope, RetentionReason, SourceSignals, plan_observation,
};
use crate::search::{
    LexicalSearchOutcome, PartitionReader, search_partitions_with_quality, search_reader,
    search_reader_with_quality,
};
#[cfg(feature = "internal-d5c-preview")]
use crate::search::{ProfileExecution, search_partitions_with_profile, search_reader_with_profile};
use crate::semantic::{
    SemanticRuntime, embedding_source_identity, embedding_text, rrf_fuse_traced,
};
use crate::walk::{EnumerationResult, discover_vault};

/// Candidate depth fetched from each leg before RRF fusion.
const HYBRID_CANDIDATES: usize = 100;
/// The contract's permitted fusion formula, standard constant.
const RRF_K: f64 = 60.0;

const WRITER_MEMORY_BYTES: usize = 50_000_000;

#[derive(Debug, Clone, PartialEq)]
pub struct GenerationSearchResult {
    /// Only resources actually opened for this request, even if none matched.
    pub snapshot_ids: BTreeMap<ResourceKey, Option<SnapshotId>>,
    pub generation: String,
    pub hits: Vec<SearchHit>,
    pub lexical_match_quality: Option<LexicalMatchQuality>,
}

impl GenerationSearchResult {
    /// Check the complete participating set, not just resources appearing in hits.
    pub fn require_snapshot_ids(&self) -> Result<()> {
        if self.snapshot_ids.is_empty() || self.snapshot_ids.values().any(Option::is_none) {
            return Err(Error::State(
                "participating resource has no reader snapshot label".into(),
            ));
        }
        Ok(())
    }
}

#[derive(Clone)]
pub struct SearchRuntime {
    active: Arc<ArcSwapOption<ActiveSearchIndex>>,
    semantic: Arc<ArcSwapOption<SemanticRuntime>>,
    freshness_basis: Arc<AtomicU8>,
    resource_freshness: Arc<Mutex<BTreeMap<ResourceKey, ResourceFreshness>>>,
    registered_resources: Arc<ArcSwapOption<BTreeSet<ResourceKey>>>,
}

impl Default for SearchRuntime {
    fn default() -> Self {
        Self::new()
    }
}

impl SearchRuntime {
    pub fn new() -> Self {
        Self {
            active: Arc::new(ArcSwapOption::empty()),
            semantic: Arc::new(ArcSwapOption::empty()),
            freshness_basis: Arc::new(AtomicU8::new(0)),
            resource_freshness: Arc::new(Mutex::new(BTreeMap::new())),
            registered_resources: Arc::new(ArcSwapOption::empty()),
        }
    }

    pub fn search(&self, request: &LexicalSearchRequest) -> Result<Vec<SearchHit>> {
        let filters = SearchFilters {
            vault_id: request.vault_id.clone(),
            ..SearchFilters::default()
        };
        self.search_filtered(&request.query, request.limit.clamp(1, 100), &filters)
    }

    /// Evaluates one bounded playground case without consulting the active vault.
    /// This method exists only in explicit internal preview builds.
    #[cfg(feature = "internal-d5c-preview")]
    pub fn internal_d5c_evaluate(
        &self,
        configuration: &BalancedPlaygroundConfiguration,
        case: &BalancedPlaygroundCase,
    ) -> BalancedComparisonEnvelope {
        evaluate_balanced_playground(configuration, case)
    }

    pub fn search_filtered(
        &self,
        query: &str,
        limit: usize,
        filters: &SearchFilters,
    ) -> Result<Vec<SearchHit>> {
        Ok(self
            .search_filtered_with_generation(query, limit, filters)?
            .hits)
    }

    #[cfg(feature = "internal-d5c-preview")]
    pub fn search_filtered_with_profile(
        &self,
        query: &str,
        limit: usize,
        filters: &SearchFilters,
        profile: &RelevanceProfile,
        query_time_epoch_seconds: u64,
    ) -> Result<Vec<SearchHit>> {
        let active = self.active.load_full().ok_or(Error::IndexBuilding)?;
        match active.as_ref() {
            ActiveSearchIndex::Desktop(index) => {
                index.search_with_profile(query, limit, filters, profile, query_time_epoch_seconds)
            }
            ActiveSearchIndex::OpenClast(_) => Err(Error::Auth(
                "openclast search requires an explicit authorized resource set".to_owned(),
            )),
        }
    }

    pub fn search_filtered_with_generation(
        &self,
        query: &str,
        limit: usize,
        filters: &SearchFilters,
    ) -> Result<GenerationSearchResult> {
        let active = self.active.load_full().ok_or(Error::IndexBuilding)?;
        match active.as_ref() {
            ActiveSearchIndex::Desktop(index) => {
                let outcome = index.search_with_quality(query, limit, filters)?;
                Ok(GenerationSearchResult {
                    snapshot_ids: BTreeMap::new(),
                    generation: index.generation.clone(),
                    hits: outcome.hits,
                    lexical_match_quality: Some(outcome.match_quality),
                })
            }
            ActiveSearchIndex::OpenClast(_) => Err(Error::Auth(
                "openclast search requires an explicit authorized resource set".to_owned(),
            )),
        }
    }

    /// Apply an accepted OpenClast registration snapshot before reconciliation.
    /// Old generation partitions are not proof that a tuple is still registered.
    pub fn set_openclast_registrations(&self, config: &Config) {
        debug_assert_eq!(config.server.profile, HostProfile::OpenClast);
        self.registered_resources.store(Some(Arc::new(
            config
                .vaults
                .iter()
                .filter_map(|vault| config.resource_key(vault))
                .collect(),
        )));
    }

    pub fn search_authorized(
        &self,
        query: &str,
        limit: usize,
        filters: &SearchFilters,
        resources: &[ResourceKey],
    ) -> Result<Vec<SearchHit>> {
        Ok(self
            .search_authorized_with_generation(query, limit, filters, resources)?
            .hits)
    }

    #[cfg(feature = "internal-d5c-preview")]
    pub fn search_authorized_with_profile(
        &self,
        query: &str,
        limit: usize,
        filters: &SearchFilters,
        resources: &[ResourceKey],
        profile: &RelevanceProfile,
        query_time_epoch_seconds: u64,
    ) -> Result<Vec<SearchHit>> {
        let (index, opened) = self.open_authorized_snapshot(filters, resources)?;
        index.search_with_profile(
            query,
            limit,
            filters,
            &opened,
            profile,
            query_time_epoch_seconds,
        )
    }

    pub fn search_authorized_with_generation(
        &self,
        query: &str,
        limit: usize,
        filters: &SearchFilters,
        resources: &[ResourceKey],
    ) -> Result<GenerationSearchResult> {
        let (index, opened) = self.open_authorized_snapshot(filters, resources)?;
        index.search_opened(&opened, query, limit, filters, resources)
    }

    /// Bind the signed resource to the acquired physical reader before ranking. Neither
    /// registration nor the active pointer is consulted again for the result's label.
    pub fn search_labeled_resource(
        &self,
        query: &str,
        limit: usize,
        resource: &ResourceKey,
    ) -> Result<GenerationSearchResult> {
        let filters = SearchFilters::default();
        let resources = std::slice::from_ref(resource);
        let (index, opened) = self.open_authorized_snapshot(&filters, resources)?;
        index.require_live_reader(&opened, resource)?;
        index.search_opened(&opened, query, limit, &filters, resources)
    }

    /// Semantic-only search: nearest chunks by embedding distance, then
    /// hydrated and filter-checked against the lexical index.
    pub fn search_semantic(
        &self,
        query: &str,
        limit: usize,
        filters: &SearchFilters,
    ) -> Result<Vec<SearchHit>> {
        Ok(self
            .search_semantic_with_generation(query, limit, filters)?
            .hits)
    }

    pub fn search_semantic_with_generation(
        &self,
        query: &str,
        limit: usize,
        filters: &SearchFilters,
    ) -> Result<GenerationSearchResult> {
        let active = self.require_desktop_index()?;
        let semantic = self.require_semantic()?;
        // Over-fetch so filter-excluded neighbors don't shrink the page.
        let neighbors = semantic.search(query, HYBRID_CANDIDATES.max(limit))?;
        let ordered: Vec<(String, f32)> = neighbors
            .into_iter()
            // Cosine distance → similarity, so larger remains better.
            .map(|hit| (hit.chunk_id, 1.0 - hit.distance as f32))
            .collect();
        let mut hits = active.hydrate(&ordered, filters, Some(query))?;
        hits.truncate(limit);
        Ok(GenerationSearchResult {
            snapshot_ids: BTreeMap::new(),
            generation: active.generation.clone(),
            hits,
            lexical_match_quality: None,
        })
    }

    /// Hybrid search: RRF fusion of the lexical and semantic rankings.
    pub fn search_hybrid(
        &self,
        query: &str,
        limit: usize,
        filters: &SearchFilters,
    ) -> Result<Vec<SearchHit>> {
        Ok(self
            .search_hybrid_with_generation(query, limit, filters)?
            .hits)
    }

    pub fn search_hybrid_with_generation(
        &self,
        query: &str,
        limit: usize,
        filters: &SearchFilters,
    ) -> Result<GenerationSearchResult> {
        self.search_hybrid_prepared_with_generation(query, query, limit, filters)
    }

    pub fn search_hybrid_prepared_with_generation(
        &self,
        lexical_query: &str,
        semantic_query: &str,
        limit: usize,
        filters: &SearchFilters,
    ) -> Result<GenerationSearchResult> {
        let active = self.require_desktop_index()?;
        let semantic = self.require_semantic()?;
        let lexical = active.search(lexical_query, HYBRID_CANDIDATES, filters)?;
        let neighbors = semantic.search(semantic_query, HYBRID_CANDIDATES)?;

        let lexical_ids: Vec<String> = lexical.iter().map(|hit| hit.chunk_id.clone()).collect();
        let semantic_ids: Vec<String> = neighbors.into_iter().map(|hit| hit.chunk_id).collect();
        let fused = rrf_fuse_traced(&lexical_ids, &semantic_ids, RRF_K);
        let ordered: Vec<(String, f32)> = fused
            .into_iter()
            .map(|trace| (trace.chunk_id, trace.fused_score as f32))
            .collect();
        let mut hits = active.hydrate(&ordered, filters, Some(semantic_query))?;
        hits.truncate(limit);
        Ok(GenerationSearchResult {
            snapshot_ids: BTreeMap::new(),
            generation: active.generation.clone(),
            hits,
            lexical_match_quality: None,
        })
    }

    pub fn semantic_profile(&self) -> Option<crate::semantic::EmbeddingProfile> {
        self.semantic
            .load_full()
            .map(|runtime| runtime.profile().clone())
    }

    pub fn semantic_ready(&self) -> bool {
        self.semantic.load_full().is_some()
    }

    pub fn freshness_basis(&self) -> IndexFreshnessBasis {
        match self.freshness_basis.load(Ordering::Acquire) {
            1 => IndexFreshnessBasis::MetadataAudit,
            2 => IndexFreshnessBasis::ProducerManifest,
            _ => IndexFreshnessBasis::StrictHash,
        }
    }

    fn set_freshness_basis(&self, basis: IndexFreshnessBasis) {
        let value = match basis {
            IndexFreshnessBasis::StrictHash => 0,
            IndexFreshnessBasis::MetadataAudit => 1,
            IndexFreshnessBasis::ProducerManifest => 2,
        };
        self.freshness_basis.store(value, Ordering::Release);
    }

    /// Watcher evidence is scoped to the vaults whose paths changed. Unknown
    /// delivery scope (overflow/config change) conservatively covers all rooms.
    pub fn mark_resources_dirty(&self, config: &Config, scope: &ReconcileScope) -> Result<()> {
        if config.server.profile != HostProfile::OpenClast {
            return Ok(());
        }
        let mut states = self
            .resource_freshness
            .lock()
            .map_err(|_| Error::State("resource freshness lock poisoned".into()))?;
        for resource in covered_resources(config, scope) {
            let state = states.entry(resource).or_default();
            state.epoch = state
                .epoch
                .checked_add(1)
                .ok_or_else(|| Error::State("resource freshness epoch exhausted".into()))?;
            state.state = IndexFreshnessState::Stale;
        }
        Ok(())
    }

    fn begin_resource_pass(
        &self,
        config: &Config,
        scope: &ReconcileScope,
    ) -> Result<BTreeMap<ResourceKey, u64>> {
        let mut states = self
            .resource_freshness
            .lock()
            .map_err(|_| Error::State("resource freshness lock poisoned".into()))?;
        let mut epochs = BTreeMap::new();
        for resource in covered_resources(config, scope) {
            let state = states.entry(resource.clone()).or_default();
            state.state = IndexFreshnessState::Reconciling;
            epochs.insert(resource, state.epoch);
        }
        Ok(epochs)
    }

    fn finish_resource_pass(
        &self,
        epochs: BTreeMap<ResourceKey, u64>,
        incomplete: &BTreeSet<ResourceKey>,
        succeeded: bool,
    ) -> Result<()> {
        let mut states = self
            .resource_freshness
            .lock()
            .map_err(|_| Error::State("resource freshness lock poisoned".into()))?;
        for resource in incomplete {
            states.entry(resource.clone()).or_default().state = IndexFreshnessState::Stale;
        }
        for (resource, epoch) in epochs {
            let state = states.entry(resource.clone()).or_default();
            state.state = if succeeded && !incomplete.contains(&resource) && state.epoch == epoch {
                IndexFreshnessState::Current
            } else {
                IndexFreshnessState::Stale
            };
        }
        Ok(())
    }

    fn record_room_walks(
        &self,
        walks: &BTreeMap<ResourceKey, RoomWalkOutcome>,
        max_stale_serve_seconds: u64,
        started: Instant,
        published: bool,
    ) -> Result<()> {
        let mut states = self
            .resource_freshness
            .lock()
            .map_err(|_| Error::State("resource freshness lock poisoned".into()))?;
        for (resource, outcome) in walks {
            let state = states.entry(resource.clone()).or_default();
            if matches!(outcome, RoomWalkOutcome::Complete) {
                if published {
                    state.walk = RoomWalkState::Ready;
                }
            } else {
                // Carry the first failure across retries. A restart must not
                // give unverified rows a new grace period. An initial partial
                // walk may serve only when it retained no unverified rows.
                state.state = IndexFreshnessState::Stale;
                let since = match state.walk {
                    RoomWalkState::Ready => Some(started),
                    RoomWalkState::Failed { since, .. } => since,
                    RoomWalkState::Unchecked => match outcome {
                        RoomWalkOutcome::Incomplete {
                            retained_unverified: false,
                        } if published => Some(started),
                        _ => None,
                    },
                };
                state.walk = RoomWalkState::Failed {
                    since,
                    grace: Duration::from_secs(max_stale_serve_seconds),
                };
            }
        }
        Ok(())
    }

    fn open_authorized_snapshot<'a>(
        &self,
        filters: &SearchFilters,
        resources: &'a [ResourceKey],
    ) -> Result<(Arc<PartitionedSearchIndex>, OpenedResources<'a>)> {
        // Reopening clears evidence under this lock before replacing the
        // snapshot. Never combine a superseded snapshot with a new proof.
        let states = self
            .resource_freshness
            .lock()
            .map_err(|_| Error::State("resource freshness lock poisoned".into()))?;
        let active = self.active.load_full().ok_or(Error::IndexBuilding)?;
        let ActiveSearchIndex::OpenClast(index) = active.as_ref() else {
            return Err(Error::Auth(
                "authorized resource search is unavailable in the desktop profile".into(),
            ));
        };
        let opened =
            self.open_authorized_resources_with_state(index, filters, resources, &states)?;
        Ok((index.clone(), opened))
    }

    fn open_authorized_resources_with_state<'a>(
        &self,
        index: &PartitionedSearchIndex,
        filters: &SearchFilters,
        resources: &'a [ResourceKey],
        states: &BTreeMap<ResourceKey, ResourceFreshness>,
    ) -> Result<OpenedResources<'a>> {
        let registered = self.registered_resources.load();
        let now = Instant::now();
        index.open_resources(filters, resources, |resource| {
            registered
                .as_ref()
                .is_none_or(|keys| keys.contains(resource))
                && states
                    .get(resource)
                    .is_some_and(|state| state.is_servable(now))
        })
    }

    /// Recheck the effective set at response time. A changed authorized revision
    /// makes an already-running search stale; forbidden publications do not.
    pub fn authorized_freshness(
        &self,
        filters: &SearchFilters,
        resources: &[ResourceKey],
        served_generation: &str,
    ) -> Result<IndexFreshness> {
        self.scoped_freshness(filters, resources, served_generation, None)
    }

    /// Recheck eligibility on the installed reader, never relabel the answering one.
    pub fn labeled_resource_freshness(
        &self,
        resource: &ResourceKey,
        served_generation: &str,
    ) -> Result<IndexFreshness> {
        self.scoped_freshness(
            &SearchFilters::default(),
            std::slice::from_ref(resource),
            served_generation,
            Some(resource),
        )
    }

    fn scoped_freshness(
        &self,
        filters: &SearchFilters,
        resources: &[ResourceKey],
        served_generation: &str,
        live_resource: Option<&ResourceKey>,
    ) -> Result<IndexFreshness> {
        // Read freshness before loading the active snapshot, under the same
        // lock that finishes a pass. Otherwise a completed publication between
        // those reads could label a superseded response current.
        let states = self
            .resource_freshness
            .lock()
            .map_err(|_| Error::State("resource freshness lock poisoned".into()))?;
        let active = self.active.load_full().ok_or(Error::IndexBuilding)?;
        let ActiveSearchIndex::OpenClast(index) = active.as_ref() else {
            return Err(Error::Auth(
                "scoped freshness requires the openclast profile".into(),
            ));
        };
        let effective = effective_resources(filters, resources);
        let opened =
            self.open_authorized_resources_with_state(index, filters, resources, &states)?;
        if let Some(resource) = live_resource {
            index.require_live_reader(&opened, resource)?;
        }
        let current_generation = index.scoped_generation(filters, resources)?;
        let mut worst = IndexFreshnessState::Current;
        for resource in effective {
            let state = states
                .get(resource)
                .map_or(IndexFreshnessState::Stale, |state| state.state);
            worst = worse_freshness(worst, state);
        }
        if current_generation != served_generation {
            worst = worse_freshness(worst, IndexFreshnessState::Stale);
        }
        Ok(IndexFreshness::new(worst, self.freshness_basis()))
    }

    pub fn generation(&self) -> Option<String> {
        self.active.load_full().map(|active| match active.as_ref() {
            ActiveSearchIndex::Desktop(index) => index.generation.clone(),
            ActiveSearchIndex::OpenClast(index) => index.generation.clone(),
        })
    }

    fn require_desktop_index(&self) -> Result<Arc<SearchIndex>> {
        let active = self.active.load_full().ok_or(Error::IndexBuilding)?;
        match active.as_ref() {
            ActiveSearchIndex::Desktop(index) => Ok(index.clone()),
            ActiveSearchIndex::OpenClast(_) => Err(Error::SemanticUnavailable(
                "semantic and hybrid search are unavailable in the openclast profile".to_owned(),
            )),
        }
    }

    fn require_semantic(&self) -> Result<Arc<SemanticRuntime>> {
        self.semantic.load_full().ok_or_else(|| {
            Error::SemanticUnavailable(
                "no embedding model is loaded; start the daemon with semantic support".to_owned(),
            )
        })
    }

    pub fn active_room_identity_format(&self) -> bool {
        matches!(self.active.load_full().as_deref(),
            Some(ActiveSearchIndex::OpenClast(index))
                if index.format == crate::manifest::INDEX_FORMAT_VERSION && crate::source::ROOM_INCLUSIVE_IDS)
    }

    fn install_desktop(&self, generation: Arc<SearchIndex>) {
        self.active
            .store(Some(Arc::new(ActiveSearchIndex::Desktop(generation))));
    }

    fn install_reopened_openclast(&self, generation: Arc<PartitionedSearchIndex>) -> Result<()> {
        let mut states = self
            .resource_freshness
            .lock()
            .map_err(|_| Error::State("resource freshness lock poisoned".into()))?;
        // An on-disk generation is not evidence that its rooms are still
        // servable. Even a caller reusing a SearchRuntime must re-prove them.
        states.clear();
        self.install_openclast(generation);
        Ok(())
    }

    fn install_openclast(&self, generation: Arc<PartitionedSearchIndex>) {
        self.active
            .store(Some(Arc::new(ActiveSearchIndex::OpenClast(generation))));
    }

    pub fn install_semantic(&self, runtime: Arc<SemanticRuntime>) {
        self.semantic.store(Some(runtime));
    }
}

#[derive(Clone)]
struct ResourceFreshness {
    state: IndexFreshnessState,
    epoch: u64,
    walk: RoomWalkState,
}

enum RoomWalkOutcome {
    Complete,
    Incomplete { retained_unverified: bool },
}

#[derive(Clone, Copy)]
enum RoomWalkState {
    Unchecked,
    Ready,
    Failed {
        since: Option<Instant>,
        grace: Duration,
    },
}

impl ResourceFreshness {
    fn is_servable(&self, now: Instant) -> bool {
        match self.walk {
            RoomWalkState::Failed { since, grace } => {
                since.is_some_and(|start| now.saturating_duration_since(start) < grace)
            }
            RoomWalkState::Ready => true,
            RoomWalkState::Unchecked => false,
        }
    }
}

impl Default for ResourceFreshness {
    fn default() -> Self {
        Self {
            state: IndexFreshnessState::Stale,
            epoch: 0,
            walk: RoomWalkState::Unchecked,
        }
    }
}

fn covered_resources(config: &Config, scope: &ReconcileScope) -> BTreeSet<ResourceKey> {
    config
        .vaults
        .iter()
        .filter_map(|vault| config.resource_key(vault))
        .filter(|resource| match scope {
            ReconcileScope::Full => true,
            ReconcileScope::Paths(paths) => paths.iter().any(|(id, _)| id == &resource.vault_id),
            ReconcileScope::RoomPaths(paths) => paths.iter().any(|(key, _)| key == resource),
        })
        .collect()
}

fn effective_resources<'a>(
    filters: &SearchFilters,
    resources: &'a [ResourceKey],
) -> BTreeSet<&'a ResourceKey> {
    resources
        .iter()
        .filter(|resource| matches_resource_filters(filters, resource))
        .collect()
}

fn matches_resource_filters(filters: &SearchFilters, resource: &ResourceKey) -> bool {
    filters
        .vault_id
        .as_deref()
        .is_none_or(|id| id == resource.vault_id)
        && filters
            .room
            .as_deref()
            .is_none_or(|room| room == resource.room_id)
}

fn worse_freshness(a: IndexFreshnessState, b: IndexFreshnessState) -> IndexFreshnessState {
    fn rank(state: IndexFreshnessState) -> u8 {
        match state {
            IndexFreshnessState::Current => 0,
            IndexFreshnessState::Reconciling => 1,
            IndexFreshnessState::Stale => 2,
            IndexFreshnessState::Unavailable => 3,
        }
    }
    if rank(a) >= rank(b) { a } else { b }
}

// All binary lengths/counts use u64-LE, never platform-sized integers.
fn digest_bytes(digest: &mut Sha256, bytes: &[u8]) {
    digest.update((bytes.len() as u64).to_le_bytes());
    digest.update(bytes);
}

/// Transitional v1 revision. Compact serde JSON has deterministic struct field
/// order and includes every persisted ManifestFile field (including optional
/// fields according to their on-disk serde rules). Rows are source_key ordered,
/// framed separately from their keys, and counted. No global timestamp/revision
/// participates. Adding a persisted field automatically adds it to this digest.
fn resource_revision(manifest: &Manifest, resource: &ResourceKey) -> Result<[u8; 32]> {
    let mut digest = Sha256::new();
    digest_bytes(&mut digest, b"kwiry-resource-revision-v1");
    for version in [
        u64::from(manifest.manifest_version),
        u64::from(manifest.index_format_version),
        manifest.chunking_version,
        u64::from(manifest.preparation_schema_version),
        u64::from(manifest.format_identity_schema_version),
    ] {
        digest.update(version.to_le_bytes());
    }
    let files: Vec<_> = manifest
        .files
        .iter()
        .filter(|(_, file)| file.resource.as_ref() == Some(resource))
        .collect();
    digest.update((files.len() as u64).to_le_bytes());
    for (key, file) in files {
        digest_bytes(&mut digest, key.as_bytes());
        let bytes = serde_json::to_vec(file)
            .map_err(|_| Error::State("resource revision serialization failed".into()))?;
        digest_bytes(&mut digest, &bytes);
    }
    Ok(digest.finalize().into())
}

enum ActiveSearchIndex {
    Desktop(Arc<SearchIndex>),
    OpenClast(Arc<PartitionedSearchIndex>),
}

struct ReaderPartition {
    index_dir: PathBuf,
    revision: [u8; 32],
    snapshot_id: Option<SnapshotId>,
}

struct PartitionedSearchIndex {
    format: u32,
    generation: String,
    partitions: BTreeMap<ResourceKey, ReaderPartition>,
    /// Lazy per-resource readers. A partition is opened only after a
    /// request's authorized resource intersection selects it, then cached
    /// for the lifetime of this immutable generation; a generation swap
    /// replaces the whole value, discarding every cached reader. The
    /// authorized-only physical baseline is preserved: unauthorized
    /// partitions are never preloaded.
    readers: std::sync::Mutex<BTreeMap<ResourceKey, Arc<SearchIndex>>>,
}

type OpenedResources<'a> = BTreeMap<&'a ResourceKey, Arc<SearchIndex>>;

fn partition_readers<'a>(opened: &'a OpenedResources<'_>) -> Vec<PartitionReader<'a>> {
    opened
        .iter()
        .map(|(resource, partition)| PartitionReader {
            index: &partition.index,
            fields: &partition.fields,
            reader: &partition.reader,
            resource,
        })
        .collect()
}

fn search_opened_with_quality(
    opened: &OpenedResources<'_>,
    query: &str,
    limit: usize,
    filters: &SearchFilters,
) -> Result<LexicalSearchOutcome> {
    search_partitions_with_quality(&partition_readers(opened), query, limit, filters)
}

impl PartitionedSearchIndex {
    fn open(active: &crate::generation::GenerationPaths) -> Result<Self> {
        Self::from_layout(active, GenerationLayout::load_generation(active)?)
    }

    fn from_layout(
        active: &crate::generation::GenerationPaths,
        layout: GenerationLayout,
    ) -> Result<Self> {
        let (manifest, _) = Manifest::load(&active.manifest_path)?.adopt();
        let mut partitions = BTreeMap::new();
        for partition in layout.partitions {
            let revision = resource_revision(&manifest, &partition.resource)?;
            let index_dir = partition_index_dir(&active.partitions_dir, &partition.resource);
            if !index_dir.join("meta.json").is_file() {
                return Err(Error::State(format!(
                    "resource partition is incomplete: {}",
                    partition.partition_id
                )));
            }
            if partitions
                .insert(
                    partition.resource.clone(),
                    ReaderPartition {
                        index_dir,
                        revision,
                        snapshot_id: partition.snapshot_id,
                    },
                )
                .is_some()
            {
                return Err(Error::State(format!(
                    "duplicate resource partition: {}/{}/{}",
                    partition.resource.tenant_id,
                    partition.resource.vault_id,
                    partition.resource.room_id
                )));
            }
        }
        Ok(Self {
            generation: active.id.clone(),
            format: manifest.index_format_version,
            partitions,
            readers: std::sync::Mutex::new(BTreeMap::new()),
        })
    }

    /// Open the complete effective set in signed order before query work.
    /// Retain the verified handles, but report all unavailable signed indexes
    /// together, whether registration, expiry or reader opening failed.
    fn open_resources<'a>(
        &self,
        filters: &SearchFilters,
        resources: &'a [ResourceKey],
        is_available: impl Fn(&ResourceKey) -> bool,
    ) -> Result<OpenedResources<'a>> {
        let mut indexes = Vec::new();
        let mut opened = BTreeMap::new();
        for (index, resource) in resources.iter().enumerate() {
            if !matches_resource_filters(filters, resource) {
                continue;
            }
            let reader = self
                .partitions
                .get(resource)
                .filter(|_| is_available(resource))
                .and_then(|p| self.authorized_reader(resource, &p.index_dir).ok());
            if let Some(reader) = reader {
                opened.insert(resource, reader);
            } else {
                indexes.push(index);
            }
        }
        if indexes.is_empty() {
            Ok(opened)
        } else if crate::source::ROOM_INCLUSIVE_IDS {
            Err(Error::UnavailableResourceIndexes(indexes))
        } else {
            Err(Error::ResourceUnavailable)
        }
    }

    fn require_live_reader(
        &self,
        opened: &OpenedResources<'_>,
        resource: &ResourceKey,
    ) -> Result<()> {
        let label = self
            .partitions
            .get(resource)
            .and_then(|p| p.snapshot_id.as_ref());
        if !opened.contains_key(resource) || !label.is_some_and(|id| id.is_live_check_label()) {
            return Err(Error::UnavailableResourceIndexes(vec![0]));
        }
        Ok(())
    }

    fn search_opened(
        &self,
        opened: &OpenedResources<'_>,
        query: &str,
        limit: usize,
        filters: &SearchFilters,
        resources: &[ResourceKey],
    ) -> Result<GenerationSearchResult> {
        let snapshot_ids = opened
            .keys()
            .map(|&key| (key.clone(), self.partitions[key].snapshot_id.clone()))
            .collect();
        let outcome = search_opened_with_quality(opened, query, limit, filters)?;
        Ok(GenerationSearchResult {
            snapshot_ids,
            generation: self.scoped_generation(filters, resources)?,
            hits: outcome.hits,
            lexical_match_quality: Some(outcome.match_quality),
        })
    }

    fn scoped_generation(
        &self,
        filters: &SearchFilters,
        resources: &[ResourceKey],
    ) -> Result<String> {
        let effective = effective_resources(filters, resources);
        let mut digest = Sha256::new();
        digest.update(b"L");
        digest.update((effective.len() as u64).to_le_bytes());
        for resource in effective {
            let partition = self
                .partitions
                .get(resource)
                .ok_or(Error::ResourceUnavailable)?;
            for field in [&resource.tenant_id, &resource.vault_id, &resource.room_id] {
                digest_bytes(&mut digest, field.as_bytes());
            }
            digest_bytes(&mut digest, &partition.revision);
        }
        Ok(format!("v1-{:x}", digest.finalize()))
    }

    fn authorized_reader(
        &self,
        resource: &ResourceKey,
        index_dir: &Path,
    ) -> Result<Arc<SearchIndex>> {
        let mut readers = self
            .readers
            .lock()
            .map_err(|_| Error::Index("partition reader cache mutex poisoned".to_owned()))?;
        // A cached reader must not make a removed partition servable.
        if !index_dir.join("meta.json").is_file() {
            readers.remove(resource);
            return Err(Error::ResourceUnavailable);
        }
        if let Some(reader) = readers.get(resource) {
            return Ok(reader.clone());
        }
        let partition = SearchIndex::open(self.generation.clone(), index_dir)?;
        partition.source_key_field()?;
        let partition = Arc::new(partition);
        readers.insert(resource.clone(), partition.clone());
        Ok(partition)
    }

    #[cfg(test)]
    fn search(
        &self,
        query: &str,
        limit: usize,
        filters: &SearchFilters,
        resources: &[ResourceKey],
    ) -> Result<Vec<SearchHit>> {
        Ok(self
            .search_with_quality(query, limit, filters, resources)?
            .hits)
    }

    #[cfg(test)]
    fn search_with_quality(
        &self,
        query: &str,
        limit: usize,
        filters: &SearchFilters,
        resources: &[ResourceKey],
    ) -> Result<LexicalSearchOutcome> {
        let opened = self.open_resources(filters, resources, |_| true)?;
        search_opened_with_quality(&opened, query, limit, filters)
    }

    #[cfg(feature = "internal-d5c-preview")]
    fn search_with_profile(
        &self,
        query: &str,
        limit: usize,
        filters: &SearchFilters,
        opened: &OpenedResources<'_>,
        profile: &RelevanceProfile,
        query_time_epoch_seconds: u64,
    ) -> Result<Vec<SearchHit>> {
        if matches!(profile, RelevanceProfile::LexicalV1) {
            return Ok(search_opened_with_quality(opened, query, limit, filters)?.hits);
        }
        let readers = partition_readers(opened);
        search_partitions_with_profile(
            &readers,
            query,
            limit,
            filters,
            ProfileExecution {
                profile,
                query_time_epoch_seconds,
            },
        )
    }
}

struct SearchIndex {
    generation: String,
    index: Index,
    fields: Fields,
    reader: IndexReader,
}

impl SearchIndex {
    fn open(generation: String, index_dir: &Path) -> Result<Self> {
        let (index, fields) = open_index_dir(index_dir)?;
        let reader = index
            .reader_builder()
            .reload_policy(ReloadPolicy::Manual)
            .try_into()
            .map_err(|error| Error::Index(format!("could not open index reader: {error}")))?;
        Ok(Self {
            generation,
            index,
            fields,
            reader,
        })
    }

    fn search(&self, query: &str, limit: usize, filters: &SearchFilters) -> Result<Vec<SearchHit>> {
        search_reader(
            &self.index,
            &self.fields,
            &self.reader,
            query,
            limit,
            filters,
        )
    }

    fn search_with_quality(
        &self,
        query: &str,
        limit: usize,
        filters: &SearchFilters,
    ) -> Result<LexicalSearchOutcome> {
        search_reader_with_quality(
            &self.index,
            &self.fields,
            &self.reader,
            query,
            limit,
            filters,
        )
    }

    #[cfg(feature = "internal-d5c-preview")]
    fn search_with_profile(
        &self,
        query: &str,
        limit: usize,
        filters: &SearchFilters,
        profile: &RelevanceProfile,
        query_time_epoch_seconds: u64,
    ) -> Result<Vec<SearchHit>> {
        search_reader_with_profile(
            &self.index,
            &self.fields,
            &self.reader,
            query,
            limit,
            filters,
            ProfileExecution {
                profile,
                query_time_epoch_seconds,
            },
        )
    }

    fn hydrate(
        &self,
        ordered: &[(String, f32)],
        filters: &SearchFilters,
        snippet_source: Option<&str>,
    ) -> Result<Vec<SearchHit>> {
        crate::search::hydrate_ordered(
            &self.index,
            &self.fields,
            &self.reader,
            ordered,
            filters,
            snippet_source,
        )
    }

    fn source_key_field(&self) -> Result<Field> {
        self.fields.source_key.ok_or_else(|| {
            Error::Index("active generation is missing the source_key field".to_owned())
        })
    }
}

struct VaultObservations {
    outcomes: Vec<FileIngestOutcome>,
    whole_walk_failed: bool,
    reused_keys: BTreeSet<String>,
    enumeration: EnumerationResult,
    source_files_read: usize,
    source_bytes_read: u64,
    audited_sources: usize,
    audit_pending: bool,
}

struct VaultObservationContext<'a> {
    registration_fingerprint: &'a str,
    scope: &'a PartitionScope,
    previous_resource: Option<&'a ResourceKey>,
    policy: ObservationPolicy,
    semantic: Option<&'a SemanticRuntime>,
    /// Watcher-evidence relative paths for this vault. Forces byte reads
    /// for matching sources; never restricts enumeration or deletion.
    read_scope: Option<&'a BTreeSet<String>>,
    room_roots: Option<&'a crate::room_walk::RoomRoots>,
}

impl VaultObservationContext<'_> {
    /// Manifest history can outlive a withheld partition. Only compare against
    /// history whose chunks actually exist in the partition carried forward.
    fn reusable<'a>(&self, previous: &'a Manifest, key: &str) -> Option<&'a ManifestFile> {
        previous.files.get(key).filter(|_| {
            matches!(self.scope, PartitionScope::Whole) || self.previous_resource.is_some()
        })
    }
}

fn observe_vault(
    vault: &crate::model::VaultRegistration,
    previous: &Manifest,
    context: &VaultObservationContext<'_>,
    audit: &mut AuditBudget,
    needs_full_read: impl FnOnce(usize, &[FileIngestOutcome]) -> bool,
) -> VaultObservations {
    let (mut enumeration, secure_read) = match context.room_roots {
        Some(roots) => roots.walk(vault),
        None => (discover_vault(vault), None),
    };
    let mut decisions = Vec::with_capacity(enumeration.files.len());
    let mut audit_candidates = Vec::new();

    for file in &enumeration.files {
        let key =
            crate::source::room_source_key(&vault.id, vault.room.as_deref(), &file.relative_path);
        let previous_file = context.reusable(previous, &key);
        let previous_scope = previous_file.map(|file| match context.scope {
            PartitionScope::Whole => PartitionScope::Whole,
            PartitionScope::Resource(current) => PartitionScope::Resource(
                file.resource
                    .clone()
                    .or_else(|| context.previous_resource.cloned())
                    .unwrap_or_else(|| current.clone()),
            ),
        });
        // A source whose chunks are missing from the semantic store (boot
        // backfill, semantic newly enabled) needs its bytes even when the
        // lexical metadata is reusable.
        let semantic_backfill = context.policy.basis != IndexFreshnessBasis::StrictHash
            && match (context.semantic, previous_file) {
                (Some(runtime), Some(previous_file)) if previous_file.chunk_count > 0 => {
                    // Compared against the same value the store records, which
                    // binds the format identity as well as the bytes — so a
                    // format whose extractor moved is backfilled rather than
                    // left with vectors of text that is no longer produced.
                    runtime.source_hash(&key).ok().flatten().as_deref()
                        != Some(
                            embedding_source_identity(
                                &previous_file.content_hash,
                                previous_file.format,
                            )
                            .as_str(),
                        )
                }
                _ => false,
            };
        let forced_read = context
            .read_scope
            .is_some_and(|paths| paths.contains(&file.relative_path));
        let decision = plan_observation(
            previous_file,
            file,
            context.registration_fingerprint,
            context.scope,
            previous_scope.as_ref(),
            context.policy,
            SourceSignals {
                forced_read,
                semantic_backfill,
            },
        );
        if decision == ObservationDecision::ReuseMetadata {
            audit_candidates.push((key.clone(), file.byte_length));
        }
        decisions.push((file, key, decision));
    }

    let audited = audit.select(&audit_candidates);
    let mut outcomes = Vec::new();
    let mut deferred = Vec::new();
    let mut source_files_read = 0;
    let mut source_bytes_read = 0;
    let nesting = || {
        secure_read
            .as_ref()
            .is_some_and(crate::room_walk::RoomRead::nesting_detected)
    };
    let mut read = |file| {
        let outcome = crate::chunk::ingest_file_with_room_handle(vault, file, secure_read.as_ref());
        source_files_read += 1;
        if outcome.content_hash.is_some() {
            source_bytes_read += outcome.byte_length;
        }
        outcome
    };
    for (file, key, decision) in decisions {
        let read_reason = match decision {
            ObservationDecision::ReadHash(reason) => Some(reason),
            ObservationDecision::ReuseMetadata if audited.contains(&key) => Some(ReadReason::Audit),
            ObservationDecision::ReuseMetadata => None,
        };
        if read_reason.is_some() {
            outcomes.push(read(file));
            if nesting() {
                break;
            }
        } else {
            deferred.push((file, key));
        }
    }
    // Finish this same pinned walk if the normal observation found a row change.
    // Already-read sources (including watcher hints and audits) are not read twice.
    let full = needs_full_read(enumeration.files.len(), &outcomes);
    let mut reused_keys = BTreeSet::new();
    for (file, key) in deferred {
        if full {
            if nesting() {
                break;
            }
            outcomes.push(read(file));
        } else {
            reused_keys.insert(key);
        }
    }
    let nesting_detected = nesting();
    if nesting_detected {
        enumeration = crate::room_walk::nesting_failure(vault);
        outcomes.clear();
        reused_keys.clear();
    }
    VaultObservations {
        outcomes,
        whole_walk_failed: nesting_detected
            || (context.room_roots.is_some() && secure_read.is_none()),
        reused_keys,
        enumeration,
        source_files_read,
        source_bytes_read,
        audited_sources: audited.len(),
        audit_pending: !full && audit_candidates.len() > audited.len(),
    }
}

fn system_time_nanos() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_nanos())
}

fn vault_read_scope(
    scope: &ReconcileScope,
    vault_id: &str,
    resource: Option<&ResourceKey>,
) -> Option<BTreeSet<String>> {
    match scope {
        ReconcileScope::Full => None,
        ReconcileScope::Paths(paths) => Some(
            paths
                .iter()
                .filter(|(id, _)| id == vault_id)
                .map(|(_, path)| path.clone())
                .collect(),
        ),
        ReconcileScope::RoomPaths(paths) => Some(
            paths
                .iter()
                .filter(|(key, _)| Some(key) == resource)
                .map(|(_, path)| path.clone())
                .collect(),
        ),
    }
}

struct DesktopIndexManager {
    _lock: DataRootLock,
    data_root: DataRoot,
    active: crate::generation::GenerationPaths,
    search: Arc<SearchIndex>,
    runtime: SearchRuntime,
    manifest: Manifest,
    config: Config,
    audit_cursor: usize,
    evictions: EvictionReport,
}

impl DesktopIndexManager {
    fn open(config: Config, data_dir: &Path, runtime: SearchRuntime) -> Result<Self> {
        let data_root = DataRoot::new(data_dir);
        let lock = data_root.acquire_writer_lock()?;
        let mut active = data_root.active()?.ok_or_else(|| {
            Error::State("Vertical 2 generation is missing; run `kwiry index` first".to_owned())
        })?;
        // Eviction is a mandatory open-time transformation, never a read-time
        // filter: every downstream stage — planning, retention, publication,
        // search — then holds by construction instead of by remembering to
        // check. It runs before the index is made searchable, so a row whose
        // format identity moved is never served in the interim.
        let (mut manifest, evictions) = Manifest::load(&active.manifest_path)?.adopt();
        if !evictions.is_empty() {
            // Crash-safe order: delete the index rows, write the evicted
            // manifest, then publish. A crash before publication leaves the
            // previous generation intact and the next open evicts again —
            // idempotent. The reverse order is unrecoverable: postings the
            // manifest no longer names can never be discovered again.
            manifest.mark_synced()?;
            let candidate = data_root.create_candidate_from(&active)?;
            let staging_dir = candidate.staging_dir.clone();
            let evicted_keys: BTreeSet<String> = evictions.evicted.keys().cloned().collect();
            let update_result = (|| -> Result<()> {
                let candidate_search =
                    SearchIndex::open(candidate.id.clone(), &candidate.index_dir)?;
                apply_index_updates(
                    &candidate_search.index,
                    &candidate_search.fields,
                    Some(&evicted_keys),
                    None,
                    None,
                )?;
                manifest.save(&candidate.manifest_path())?;
                Ok(())
            })();
            if let Err(error) = update_result {
                let _ = fs::remove_dir_all(staging_dir);
                return Err(error);
            }
            active = data_root.publish(candidate)?;
        } else if holds_resident_deletions(&active.index_dir)? {
            // Inherited from a build that did not finish its removals. Repair
            // before serving; see `repair_inherited_generation`.
            manifest.mark_synced()?;
            let candidate = data_root.create_candidate_from(&active)?;
            let staging_dir = candidate.staging_dir.clone();
            let repair_result = (|| -> Result<()> {
                repair_inherited_generation(&candidate.index_dir)?;
                manifest.save(&candidate.manifest_path())?;
                Ok(())
            })();
            if let Err(error) = repair_result {
                let _ = fs::remove_dir_all(staging_dir);
                return Err(error);
            }
            active = data_root.publish(candidate)?;
        }
        let audit_cursor = manifest.state_revision as usize;
        let search = Arc::new(SearchIndex::open(active.id.clone(), &active.index_dir)?);
        search.source_key_field()?;
        runtime.set_freshness_basis(config.indexing.basis);
        runtime.install_desktop(search.clone());
        Ok(Self {
            _lock: lock,
            data_root,
            active,
            search,
            runtime,
            manifest,
            config,
            audit_cursor,
            evictions,
        })
    }

    pub fn manifest(&self) -> &Manifest {
        &self.manifest
    }

    pub fn config(&self) -> &Config {
        &self.config
    }

    pub fn reconcile(&mut self, config: Config, scope: &ReconcileScope) -> Result<ReconcileReport> {
        let mut plan = ReconcilePlan::new(&self.manifest);
        // Semantic state reconciles by its own stored hashes, so every
        // discovered source is offered; unchanged ones short-circuit.
        let mut semantic_sources =
            std::collections::BTreeMap::<String, (String, Vec<(String, String)>)>::new();
        let mut warnings = Vec::new();
        let mut unavailable_vaults = Vec::new();
        let mut source_files_read = 0;
        let mut source_bytes_read = 0;
        let mut audited_sources = 0;
        let mut audit_pending = false;
        let observation_policy = ObservationPolicy {
            basis: config.indexing.basis,
            now_nanos: system_time_nanos(),
            racy_window_nanos: u128::from(config.indexing.racy_window_millis) * 1_000_000,
        };
        // The rolling audit belongs to full passes only: a scoped candidate
        // set would rotate the cursor over a shrunken list and quietly stop
        // covering the vault.
        let mut audit = match scope {
            ReconcileScope::Full => AuditBudget::new(
                self.audit_cursor,
                config.indexing.audit_sources_per_pass,
                config.indexing.audit_bytes_per_pass,
            ),
            ReconcileScope::Paths(_) | ReconcileScope::RoomPaths(_) => {
                AuditBudget::new(self.audit_cursor, 0, 0)
            }
        };
        self.runtime.set_freshness_basis(config.indexing.basis);
        let configured_ids: HashSet<_> = config
            .vaults
            .iter()
            .map(|vault| vault.id.as_str())
            .collect();

        let removed_keys: Vec<_> = self
            .manifest
            .files
            .iter()
            .filter(|(_, file)| !configured_ids.contains(file.vault_id.as_str()))
            .map(|(key, _)| key.clone())
            .collect();
        for key in removed_keys {
            plan.remove_source(key, PartitionScope::Whole);
        }

        for vault in &config.vaults {
            if !vault.path.is_dir() {
                unavailable_vaults.push(vault.id.clone());
                for (key, file) in &self.manifest.files {
                    if file.vault_id == vault.id {
                        plan.retain_source(key, file, RetentionReason::VaultUnavailable);
                    }
                }
                warnings.push(IngestWarning {
                    path: vault.path.clone(),
                    message: "vault root is unavailable; retained last committed content"
                        .to_owned(),
                });
                continue;
            }

            let fingerprint = registration_fingerprint(vault);
            let previous_keys: Vec<_> = self
                .manifest
                .files
                .iter()
                .filter(|(_, file)| file.vault_id == vault.id)
                .map(|(key, _)| key.clone())
                .collect();
            let semantic_runtime = self.runtime.semantic.load_full();
            let vault_read_scope = vault_read_scope(scope, &vault.id, None);
            let observed = observe_vault(
                vault,
                &self.manifest,
                &VaultObservationContext {
                    registration_fingerprint: &fingerprint,
                    scope: &PartitionScope::Whole,
                    previous_resource: None,
                    policy: observation_policy,
                    semantic: semantic_runtime.as_deref(),
                    read_scope: vault_read_scope.as_ref(),
                    room_roots: None,
                },
                &mut audit,
                |_, _| false,
            );
            let discovery_incomplete = !observed.enumeration.completeness.may_infer_deletions();
            warnings.extend(observed.enumeration.warnings);
            source_files_read += observed.source_files_read;
            source_bytes_read += observed.source_bytes_read;
            audited_sources += observed.audited_sources;
            audit_pending |= observed.audit_pending;
            let mut seen_keys = observed.reused_keys;
            for outcome in observed.outcomes {
                let key = crate::source::room_source_key(
                    &outcome.vault_id,
                    outcome.room.as_deref(),
                    &outcome.path,
                );
                if let Some(warning) = outcome.warning.clone() {
                    warnings.push(warning);
                }
                if outcome.kind == FileOutcomeKind::TransientError {
                    if let Some(previous) = self.manifest.files.get(&key) {
                        plan.retain_source(&key, previous, RetentionReason::TransientReadError);
                    }
                    seen_keys.insert(key);
                    continue;
                }
                let Some((_, next_file)) = ManifestFile::from_outcome(&outcome, &fingerprint, None)
                else {
                    continue;
                };
                seen_keys.insert(key.clone());
                if !outcome.chunks.is_empty() {
                    semantic_sources.insert(
                        key.clone(),
                        (
                            // Not the raw content hash: an evicted row is
                            // re-ingested from byte-identical input, so that
                            // value cannot notice that the extractor which
                            // produced the embedded text is gone.
                            embedding_source_identity(&next_file.content_hash, next_file.format),
                            outcome
                                .chunks
                                .iter()
                                .map(|chunk| {
                                    (
                                        chunk.chunk_id.clone(),
                                        embedding_text(&chunk.heading_path, &chunk.content),
                                    )
                                })
                                .collect(),
                        ),
                    );
                }
                let previous = self.manifest.files.get(&key);
                let retrieval = outcome.retrieval;
                let chunks = outcome
                    .chunks
                    .into_iter()
                    .map(|chunk| (chunk, retrieval.clone()))
                    .collect();
                plan.reconcile_source(
                    key,
                    previous,
                    next_file,
                    PartitionScope::Whole,
                    None,
                    chunks,
                );
            }

            for key in previous_keys {
                if seen_keys.contains(&key) {
                    continue;
                }
                if discovery_incomplete {
                    let previous = &self.manifest.files[&key];
                    plan.retain_source(&key, previous, RetentionReason::IncompleteEnumeration);
                } else {
                    plan.remove_source(key, PartitionScope::Whole);
                }
            }
        }

        plan.validate_retention()?;
        let manifest_changed = plan.next_manifest != self.manifest;
        let changed_sources = plan.changed_source_count();
        let added_chunks = plan.added_chunk_count();
        let publish_generation = manifest_changed || changed_sources > 0 || added_chunks > 0;
        if publish_generation {
            plan.next_manifest.mark_synced()?;
            let candidate = self.data_root.create_candidate_from(&self.active)?;
            let staging_dir = candidate.staging_dir.clone();
            let update_result = (|| -> Result<()> {
                let candidate_search =
                    SearchIndex::open(candidate.id.clone(), &candidate.index_dir)?;
                apply_index_updates(
                    &candidate_search.index,
                    &candidate_search.fields,
                    plan.deletes(&PartitionScope::Whole),
                    plan.additions(&PartitionScope::Whole),
                    None,
                )?;
                plan.next_manifest.save(&candidate.manifest_path())?;
                Ok(())
            })();
            if let Err(error) = update_result {
                let _ = fs::remove_dir_all(staging_dir);
                return Err(error);
            }
            let active = self.data_root.publish(candidate)?;
            let search = Arc::new(SearchIndex::open(active.id.clone(), &active.index_dir)?);
            search.source_key_field()?;
            self.runtime.install_desktop(search.clone());
            self.active = active;
            self.search = search;
        }

        // Semantic updates follow the committed lexical state and reconcile
        // by their own stored hashes (covers boot backfill and offline
        // changes). Failures are warnings: lexical search must never degrade
        // because embedding or vector-store work failed.
        if let Some(semantic) = self.runtime.semantic.load_full() {
            for (key, (hash, chunks)) in &semantic_sources {
                if let Err(error) = semantic.embed_and_replace_source(key, hash, chunks) {
                    warnings.push(IngestWarning {
                        path: PathBuf::from(key.clone()),
                        message: format!("semantic update failed; lexical unaffected: {error}"),
                    });
                }
            }
            if let Some(delete_keys) = plan.deletes(&PartitionScope::Whole) {
                for key in delete_keys {
                    if semantic_sources.contains_key(key) {
                        continue;
                    }
                    if let Err(error) = semantic.delete_source(key) {
                        warnings.push(IngestWarning {
                            path: PathBuf::from(key.clone()),
                            message: format!("semantic delete failed; lexical unaffected: {error}"),
                        });
                    }
                }
            }
            let keep: BTreeSet<String> = plan.next_manifest.files.keys().cloned().collect();
            if let Err(error) = semantic.retain_sources(&keep) {
                warnings.push(IngestWarning {
                    path: PathBuf::from("semantic"),
                    message: format!("semantic cleanup failed; lexical unaffected: {error}"),
                });
            }
        }

        if publish_generation {
            self.manifest = plan.next_manifest;
        }
        if matches!(scope, ReconcileScope::Full) {
            self.audit_cursor = audit.cursor();
        }
        let freshness_basis = config.indexing.basis;
        self.config = config;

        Ok(ReconcileReport {
            changed_sources,
            added_chunks,
            source_files_read,
            source_bytes_read,
            audited_sources,
            audit_pending,
            freshness_basis,
            documents: self.manifest.document_count(),
            chunks: self.manifest.chunk_count(),
            last_sync: self.manifest.last_sync.clone(),
            warnings,
            unavailable_vaults,
            generation: self.runtime.generation(),
            manifest: self.manifest.clone(),
        })
    }

    pub fn shutdown(self) -> Result<()> {
        Ok(())
    }
}

struct OpenClastIndexManager {
    _lock: DataRootLock,
    data_root: DataRoot,
    active: crate::generation::GenerationPaths,
    runtime: SearchRuntime,
    manifest: Manifest,
    config: Config,
    audit_cursor: usize,
    evictions: EvictionReport,
    room_retries: BTreeSet<(ResourceKey, String)>,
    pending_storage_warnings: Vec<IngestWarning>,
}

impl OpenClastIndexManager {
    fn open(config: Config, data_dir: &Path, runtime: SearchRuntime) -> Result<Self> {
        #[cfg(not(target_os = "linux"))]
        crate::room_walk::probe_openclast_ingestion(data_dir, &config.vaults)?;
        crate::room_boundary::validate_ingestion_roots(&config, &[])?;
        crate::room_boundary::validate_data_root(&config, data_dir)?;
        let data_root = DataRoot::new(data_dir);
        let lock = data_root.acquire_writer_lock()?;
        let (mut active, layout) = data_root.active_with_layout()?.ok_or_else(|| {
            Error::State("IG-1 generation is missing; run `kwiry index` first".to_owned())
        })?;
        // Same mandatory open-time eviction as the desktop profile, addressed
        // per partition. `GenerationPaths::validate` has already refused any
        // OpenClast row without a resource, so every evicted key is addressable.
        let (mut manifest, evictions) = Manifest::load(&active.manifest_path)?.adopt();
        let mut layout = layout
            .ok_or_else(|| Error::State("expected partitioned generation".into()))?
            .with_generation_labels(&active);
        let mut pending_storage_warnings = Vec::new();
        if !evictions.is_empty() {
            let mut evicted_by_resource = BTreeMap::<ResourceKey, BTreeSet<String>>::new();
            for (key, evicted) in &evictions.evicted {
                let Some(resource) = evicted.resource.clone() else {
                    return Err(Error::State(format!(
                        "OpenClast manifest entry {} is missing its resource; run `kwiry index` to rebuild the disposable index",
                        evicted.path
                    )));
                };
                evicted_by_resource
                    .entry(resource)
                    .or_default()
                    .insert(key.clone());
            }
            manifest.mark_synced()?;
            let candidate = data_root.create_candidate_from(&active)?;
            let staging_dir = candidate.staging_dir.clone();
            let update_result = (|| -> Result<()> {
                for (resource, keys) in &evicted_by_resource {
                    let index_dir = partition_index_dir(&candidate.partitions_dir, resource);
                    // A resource whose partition is absent is retained state
                    // whose content is already withheld: nothing to delete.
                    if !index_dir.join("meta.json").is_file() {
                        continue;
                    }
                    let (index, fields) = open_index_dir(&index_dir)?;
                    apply_index_updates(&index, &fields, Some(keys), None, Some(resource))?;
                }
                for p in &mut layout.partitions {
                    p.snapshot_id = if evicted_by_resource.contains_key(&p.resource) {
                        PartitionEvidence::NoWalk
                    } else {
                        PartitionEvidence::UnchangedLiveRows(p.snapshot_id.as_ref())
                    }
                    .publication_label();
                }
                layout.save_candidate(&candidate, &mut pending_storage_warnings)?;
                manifest.save(&candidate.manifest_path())?;
                Ok(())
            })();
            if let Err(error) = update_result {
                let _ = fs::remove_dir_all(staging_dir);
                return Err(error);
            }
            active = data_root.publish(candidate)?;
            layout = GenerationLayout::load_generation(&active)?;
        } else {
            // The same repair the desktop profile applies, per partition: this
            // profile addresses deletions per resource, so an inherited
            // generation can hold them in any subset of its partitions.
            let mut inherited: Vec<ResourceKey> = Vec::new();
            for partition in &layout.partitions {
                let index_dir = partition_index_dir(&active.partitions_dir, &partition.resource);
                if index_dir.join("meta.json").is_file() && holds_resident_deletions(&index_dir)? {
                    inherited.push(partition.resource.clone());
                }
            }
            if !inherited.is_empty() {
                manifest.mark_synced()?;
                let candidate = data_root.create_candidate_from(&active)?;
                let staging_dir = candidate.staging_dir.clone();
                let repair_result = (|| -> Result<()> {
                    for resource in &inherited {
                        let index_dir = partition_index_dir(&candidate.partitions_dir, resource);
                        repair_inherited_generation(&index_dir)?;
                    }
                    // Expunging deleted documents neither adds nor removes live chunks.
                    for p in &mut layout.partitions {
                        p.snapshot_id =
                            PartitionEvidence::UnchangedLiveRows(p.snapshot_id.as_ref())
                                .publication_label();
                    }
                    layout.save_candidate(&candidate, &mut pending_storage_warnings)?;
                    manifest.save(&candidate.manifest_path())?;
                    Ok(())
                })();
                if let Err(error) = repair_result {
                    let _ = fs::remove_dir_all(staging_dir);
                    return Err(error);
                }
                active = data_root.publish(candidate)?;
                layout = GenerationLayout::load_generation(&active)?;
            }
        }
        let audit_cursor = manifest.state_revision as usize;
        let search = Arc::new(PartitionedSearchIndex::from_layout(&active, layout)?);
        runtime.set_freshness_basis(config.indexing.basis);
        runtime.install_reopened_openclast(search)?;
        Ok(Self {
            _lock: lock,
            data_root,
            active,
            runtime,
            manifest,
            config,
            audit_cursor,
            evictions,
            room_retries: BTreeSet::new(),
            pending_storage_warnings,
        })
    }

    fn manifest(&self) -> &Manifest {
        &self.manifest
    }

    fn config(&self) -> &Config {
        &self.config
    }

    fn reconcile(&mut self, config: Config, scope: &ReconcileScope) -> Result<ReconcileReport> {
        crate::room_boundary::validate_ingestion_roots(&config, &self.config.vaults)?;
        let epochs = self.runtime.begin_resource_pass(&config, scope)?;
        let mut incomplete = BTreeSet::new();
        let max_stale_serve_seconds = config.indexing.max_stale_serve_seconds;
        let started = Instant::now();
        let mut walks = BTreeMap::new();
        let result = self.reconcile_inner(config, scope, &mut incomplete, &mut walks);
        self.runtime
            .record_room_walks(&walks, max_stale_serve_seconds, started, result.is_ok())?;
        self.runtime
            .finish_resource_pass(epochs, &incomplete, result.is_ok())?;
        result
    }

    fn reconcile_inner(
        &mut self,
        config: Config,
        scope: &ReconcileScope,
        incomplete: &mut BTreeSet<ResourceKey>,
        walks: &mut BTreeMap<ResourceKey, RoomWalkOutcome>,
    ) -> Result<ReconcileReport> {
        let mut plan = ReconcilePlan::new(&self.manifest);
        let mut next_room_retries = BTreeSet::new();
        let installed = self
            .runtime
            .active
            .load_full()
            .ok_or(Error::IndexBuilding)?;
        let ActiveSearchIndex::OpenClast(reader) = installed.as_ref() else {
            return Err(Error::IndexBuilding);
        };
        let serving: BTreeMap<_, _> = reader
            .partitions
            .iter()
            .map(|(key, p)| (key.clone(), p.snapshot_id.clone()))
            .collect();
        let mut next_resources = serving.clone();
        let mut warnings = Vec::new();
        let mut unavailable_vaults = Vec::new();
        let mut source_files_read = 0;
        let mut source_bytes_read = 0;
        let mut audited_sources = 0;
        let mut audit_pending = false;
        let observation_policy = ObservationPolicy {
            basis: config.indexing.basis,
            now_nanos: system_time_nanos(),
            racy_window_nanos: u128::from(config.indexing.racy_window_millis) * 1_000_000,
        };
        // The rolling audit belongs to full passes only: a scoped candidate
        // set would rotate the cursor over a shrunken list and quietly stop
        // covering the vault.
        let mut audit = match scope {
            ReconcileScope::Full => AuditBudget::new(
                self.audit_cursor,
                config.indexing.audit_sources_per_pass,
                config.indexing.audit_bytes_per_pass,
            ),
            ReconcileScope::Paths(_) | ReconcileScope::RoomPaths(_) => {
                AuditBudget::new(self.audit_cursor, 0, 0)
            }
        };
        self.runtime.set_freshness_basis(config.indexing.basis);
        let current_fingerprints: BTreeMap<_, _> = config
            .vaults
            .iter()
            .map(|vault| {
                config
                    .resource_key(vault)
                    .map(|resource| (resource, registration_fingerprint(vault)))
                    .ok_or_else(|| {
                        Error::State("registered OpenClast vault has no exact room".to_owned())
                    })
            })
            .collect::<Result<_>>()?;
        // Removed registrations never retain content, including the other room
        // of a repeated vault. Removing a layout entry drops the full partition.
        let removed_keys: Vec<_> = self
            .manifest
            .files
            .iter()
            .filter(|(_, file)| {
                file.resource
                    .as_ref()
                    .is_none_or(|resource| !current_fingerprints.contains_key(resource))
            })
            .map(|(key, _)| key.clone())
            .collect();
        for key in removed_keys {
            plan.remove_manifest_source(&key);
        }
        next_resources.retain(|resource, _| current_fingerprints.contains_key(resource));

        let mut fresh_indexes = BTreeSet::new();
        let mut failed_walks = BTreeMap::new();
        for vault in &config.vaults {
            let resource = config.resource_key(vault).ok_or_else(|| {
                Error::State(format!(
                    "openclast vault {} is missing an exact resource classification",
                    vault.id
                ))
            })?;
            let previous_resource = next_resources
                .get_key_value(&resource)
                .map(|(key, _)| key.clone());
            let previous_label = next_resources.get(&resource).cloned().flatten();
            let label_changed = previous_label != vault.snapshot_id;
            let fingerprint = &current_fingerprints[&resource];
            let vault_reclassified = self
                .manifest
                .files
                .values()
                .filter(|file| file.resource.as_ref() == Some(&resource))
                .any(|file| file.registration_fingerprint != *fingerprint);
            let vault_reclassified = vault_reclassified
                || self.config.vaults.iter().any(|old| {
                    old.id == vault.id && old.room == vault.room && old.path != vault.path
                });
            if vault_reclassified {
                next_resources.remove(&resource);
            }
            let room_roots =
                crate::room_walk::RoomRoots::capture(&config, Some(self.data_root.path()));
            let index_dir = partition_index_dir(&self.active.partitions_dir, &resource);
            let has_partition = index_dir.join("meta.json").is_file();
            if !vault.path.is_dir() {
                failed_walks.insert(resource.clone(), label_changed);
                walks.insert(
                    resource.clone(),
                    RoomWalkOutcome::Incomplete {
                        retained_unverified: true,
                    },
                );
                incomplete.insert(resource.clone());
                unavailable_vaults.push(vault.id.clone());
                for (key, file) in &self.manifest.files {
                    if file.resource.as_ref() == Some(&resource) {
                        plan.retain_source(key, file, RetentionReason::VaultUnavailable);
                    }
                }
                warnings.push(IngestWarning {
                    path: vault.path.clone(),
                    message: if vault_reclassified {
                        "vault root is unavailable after registration change; withheld last committed content until it can be reindexed"
                    } else {
                        "vault root is unavailable; retained last committed content"
                    }
                    .to_owned(),
                });
                continue;
            }
            let previous_keys: Vec<_> = self
                .manifest
                .files
                .iter()
                .filter(|(_, file)| file.resource.as_ref() == Some(&resource))
                .map(|(key, _)| key.clone())
                .collect();
            let mut vault_read_scope = vault_read_scope(scope, &vault.id, Some(&resource));
            if let Some(paths) = vault_read_scope.as_mut() {
                paths.extend(
                    self.room_retries
                        .iter()
                        .filter_map(|(key, path)| (key == &resource).then_some(path.clone())),
                );
            }
            // A layout may have survived without its manifest rows after a partial
            // restore. Segment metadata suffices to detect an orphan-only index.
            let fresh = !has_partition
                || previous_resource.is_none()
                || vault_reclassified
                || (previous_keys.is_empty()
                    && open_index_dir(&index_dir)?
                        .0
                        .searchable_segment_metas()
                        .map_err(|e| Error::Index(e.to_string()))?
                        .iter()
                        .any(|m| m.num_docs() > 0));
            let scope = PartitionScope::Resource(resource.clone());
            let mut context = VaultObservationContext {
                registration_fingerprint: fingerprint,
                scope: &scope,
                previous_resource: previous_resource.as_ref().filter(|_| !fresh),
                policy: observation_policy,
                semantic: None,
                read_scope: vault_read_scope.as_ref(),
                room_roots: Some(&room_roots),
            };
            let mut certify = vault.snapshot_id.is_some() && (label_changed || fresh);
            if certify {
                context.policy.basis = IndexFreshnessBasis::StrictHash;
            }
            let observed = observe_vault(
                vault,
                &self.manifest,
                &context,
                &mut audit,
                |files, outcomes| {
                    if !certify && vault.snapshot_id.is_some() {
                        certify = outcomes.iter().any(|o| !o.clean_snapshot_input())
                            || files != previous_keys.len()
                            || outcomes.iter().any(|o| {
                                ManifestFile::from_outcome(o, fingerprint, Some(&resource))
                                    .is_none_or(|(k, f)| self.manifest.files.get(&k) != Some(&f))
                            });
                    }
                    certify
                },
            );
            let opened_root = !observed.whole_walk_failed;
            let clean_inputs = observed.reused_keys.is_empty()
                && observed
                    .outcomes
                    .iter()
                    .all(FileIngestOutcome::clean_snapshot_input);
            let discovery_incomplete = !observed.enumeration.completeness.may_infer_deletions();
            if discovery_incomplete {
                incomplete.insert(resource.clone());
            }
            // A rolling audit backlog is not dirty state under metadata_audit:
            // the configured evidence basis already describes metadata reuse.
            // Its shared budget must not couple one room's freshness to another.
            let excluded_paths = observed.enumeration.excluded;
            let directory_evidence = observed.enumeration.directories;
            let mut retained_unverified = observed.whole_walk_failed;
            warnings.extend(observed.enumeration.warnings);
            source_files_read += observed.source_files_read;
            source_bytes_read += observed.source_bytes_read;
            audited_sources += observed.audited_sources;
            audit_pending |= observed.audit_pending;
            let mut seen_keys = observed.reused_keys;

            let mut fresh = fresh;
            let mut physical = None;
            if certify && clean_inputs && !discovery_incomplete && opened_root && !fresh {
                match partition_sources(&index_dir) {
                    Ok(rows) => physical = Some(rows),
                    Err(error) => {
                        // An unaddressable chunk cannot be deleted by key. Replace only
                        // this candidate partition from the complete walk; retry its proof.
                        warnings.push(IngestWarning {
                            path: index_dir.clone(),
                            message: format!("incomplete partition: {error}"),
                        });
                        incomplete.insert(resource.clone());
                        fresh = true;
                    }
                }
            }
            if fresh && opened_root {
                fresh_indexes.insert(resource.clone());
            }

            for outcome in observed.outcomes {
                let key = crate::source::room_source_key(
                    &outcome.vault_id,
                    outcome.room.as_deref(),
                    &outcome.path,
                );
                if let Some(warning) = outcome.warning.clone() {
                    warnings.push(warning);
                }
                if outcome.kind == FileOutcomeKind::TransientError {
                    incomplete.insert(resource.clone());
                    next_room_retries.insert((resource.clone(), outcome.path.clone()));
                    if self.manifest.files.contains_key(&key) {
                        plan.remove_source(key.clone(), PartitionScope::Resource(resource.clone()));
                    }
                    seen_keys.insert(key);
                    continue;
                }
                let Some((_, next_file)) =
                    ManifestFile::from_outcome(&outcome, fingerprint, Some(&resource))
                else {
                    continue;
                };
                seen_keys.insert(key.clone());
                // A successful delete-by-key plus insertion of these freshly prepared
                // chunks establishes the post-commit state for every mismatching key.
                let exact = physical
                    .as_mut()
                    .is_none_or(|rows| source_matches(rows.remove(&key), &next_file));
                let previous_file = context
                    .reusable(&self.manifest, &key)
                    .filter(|_| exact && !fresh);
                let previous_scope = previous_file.map(|file| {
                    PartitionScope::Resource(
                        file.resource
                            .clone()
                            .or_else(|| previous_resource.clone())
                            .unwrap_or_else(|| resource.clone()),
                    )
                });
                let retrieval = outcome.retrieval;
                let chunks = outcome
                    .chunks
                    .into_iter()
                    .map(|chunk| (chunk, retrieval.clone()))
                    .collect();
                plan.reconcile_source(
                    key,
                    previous_file,
                    next_file,
                    PartitionScope::Resource(resource.clone()),
                    previous_scope,
                    chunks,
                );
            }

            for key in previous_keys {
                if seen_keys.contains(&key) {
                    continue;
                }
                let previous_file = &self.manifest.files[&key];
                let excluded = excluded_paths.iter().any(|path| {
                    path.is_empty() || Path::new(&previous_file.path).starts_with(path)
                });
                if !excluded && !directory_evidence.proves_absent(&previous_file.path) {
                    retained_unverified = true;
                    incomplete.insert(resource.clone());
                    plan.retain_source(&key, previous_file, RetentionReason::IncompleteEnumeration);
                    continue;
                }
                let delete_resource = previous_file
                    .resource
                    .clone()
                    .or_else(|| previous_resource.clone())
                    .unwrap_or_else(|| resource.clone());
                plan.remove_source(
                    key.clone(),
                    PartitionScope::Resource(delete_resource.clone()),
                );
                if delete_resource != resource {
                    plan.remove_index_source(key, PartitionScope::Resource(resource.clone()));
                }
            }
            if let Some(extra) = physical {
                for key in extra.into_keys() {
                    plan.remove_index_source(key, scope.clone());
                }
            }
            let outcome = if incomplete.contains(&resource) {
                RoomWalkOutcome::Incomplete {
                    retained_unverified,
                }
            } else {
                RoomWalkOutcome::Complete
            };
            if opened_root {
                let label = if !certify && !label_changed && !incomplete.contains(&resource) {
                    PartitionEvidence::UnchangedLiveRows(previous_label.as_ref())
                } else {
                    PartitionEvidence::Walk {
                        configured: vault.snapshot_id.as_ref(),
                        clean_and_exact: certify && clean_inputs && !incomplete.contains(&resource),
                    }
                }
                .publication_label();
                next_resources.insert(resource.clone(), label);
            } else {
                failed_walks.insert(resource.clone(), label_changed);
            }
            walks.insert(resource, outcome);
        }

        let layout_changed = !next_resources.keys().eq(serving.keys());
        let labels_changed = next_resources != serving;
        let added_chunks = plan.added_chunk_count();
        let mut resources_to_update: BTreeSet<_> = plan
            .update_scopes()
            .into_iter()
            .filter_map(|scope| match scope {
                PartitionScope::Resource(resource) => Some(resource),
                PartitionScope::Whole => None,
            })
            .collect();
        resources_to_update.extend(fresh_indexes.iter().cloned());
        // Changing a failed room's label alone must not replace the serving
        // reader. If we do publish, only carry proof for untouched live rows
        // whose serving label still matches the registration.
        for (resource, label_changed) in failed_walks {
            if (label_changed || resources_to_update.contains(&resource))
                && let Some(label) = next_resources.get_mut(&resource)
            {
                *label = PartitionEvidence::NoWalk.publication_label();
            }
        }
        let mut next_layout = GenerationLayout::with_snapshots(next_resources)?;
        plan.validate_retention()?;
        let manifest_changed = plan.next_manifest != self.manifest;
        let changed_sources = plan.changed_source_count();
        let publish_generation = manifest_changed
            || layout_changed
            || !resources_to_update.is_empty()
            || changed_sources > 0
            || added_chunks > 0
            || (labels_changed && next_layout.labels_fit()?);
        if publish_generation {
            crate::room_boundary::validate_data_root(&config, self.data_root.path())?;
            plan.next_manifest.mark_synced()?;
            let candidate = self.data_root.create_candidate_from(&self.active)?;
            let staging_dir = candidate.staging_dir.clone();
            let next_resource_set: BTreeSet<_> = next_layout
                .partitions
                .iter()
                .map(|partition| partition.resource.clone())
                .collect();
            let next_partition_ids: BTreeSet<_> = next_layout
                .partitions
                .iter()
                .map(|partition| partition.partition_id.clone())
                .collect();
            let update_result = (|| -> Result<()> {
                for entry in fs::read_dir(&candidate.partitions_dir)
                    .map_err(|error| crate::error::io_error(&candidate.partitions_dir, error))?
                {
                    let entry = entry.map_err(|error| {
                        crate::error::io_error(&candidate.partitions_dir, error)
                    })?;
                    if !next_partition_ids.contains(entry.file_name().to_string_lossy().as_ref()) {
                        let path = entry.path();
                        if entry
                            .file_type()
                            .map_err(|error| crate::error::io_error(&path, error))?
                            .is_dir()
                        {
                            fs::remove_dir_all(&path)
                                .map_err(|error| crate::error::io_error(&path, error))?;
                        } else {
                            fs::remove_file(&path)
                                .map_err(|error| crate::error::io_error(&path, error))?;
                        }
                    }
                }

                for resource in &resources_to_update {
                    if !next_resource_set.contains(resource) {
                        continue;
                    }
                    let index_dir = partition_index_dir(&candidate.partitions_dir, resource);
                    if fresh_indexes.contains(resource) && index_dir.exists() {
                        fs::remove_dir_all(&index_dir)
                            .map_err(|e| crate::error::io_error(&index_dir, e))?;
                    }
                    let (index, fields) = ensure_partition_index(&index_dir)?;
                    let scope = PartitionScope::Resource(resource.clone());
                    apply_index_updates(
                        &index,
                        &fields,
                        plan.deletes(&scope),
                        plan.additions(&scope),
                        Some(resource),
                    )?;
                }

                // New/reclassified rooms did not scan the partition being discarded.
                // Check the replacement once, after its writer has committed.
                for p in &mut next_layout.partitions {
                    if p.snapshot_id.is_some() && fresh_indexes.contains(&p.resource) {
                        let dir = partition_index_dir(&candidate.partitions_dir, &p.resource);
                        let exact = crate::index::fresh_partition_matches(
                            &dir,
                            &plan.next_manifest,
                            &p.resource,
                        );
                        if !matches!(exact, Ok(true)) {
                            warnings.push(IngestWarning {
                                path: dir,
                                message: format!(
                                    "incomplete partition: replacement verification: {exact:?}"
                                ),
                            });
                            incomplete.insert(p.resource.clone());
                            walks.insert(
                                p.resource.clone(),
                                RoomWalkOutcome::Incomplete {
                                    retained_unverified: true,
                                },
                            );
                            p.snapshot_id = PartitionEvidence::NoWalk.publication_label();
                        }
                    }
                }
                next_layout.save_candidate(&candidate, &mut warnings)?;
                plan.next_manifest.save(&candidate.manifest_path())?;
                Ok(())
            })();
            if let Err(error) = update_result {
                let _ = fs::remove_dir_all(staging_dir);
                return Err(error);
            }
            let active = self.data_root.publish(candidate)?;
            let search = Arc::new(PartitionedSearchIndex::open(&active)?);
            self.runtime.install_openclast(search);
            self.active = active;
            self.manifest = plan.next_manifest;
        }
        if matches!(scope, ReconcileScope::Full) {
            self.audit_cursor = audit.cursor();
        }
        let freshness_basis = config.indexing.basis;
        self.config = config;
        self.room_retries = next_room_retries;
        warnings.append(&mut self.pending_storage_warnings);

        Ok(ReconcileReport {
            changed_sources,
            added_chunks,
            source_files_read,
            source_bytes_read,
            audited_sources,
            audit_pending,
            freshness_basis,
            documents: self.manifest.document_count(),
            chunks: self.manifest.chunk_count(),
            last_sync: self.manifest.last_sync.clone(),
            warnings,
            unavailable_vaults,
            generation: self.runtime.generation(),
            manifest: self.manifest.clone(),
        })
    }

    fn shutdown(self) -> Result<()> {
        Ok(())
    }
}

fn apply_index_updates(
    index: &Index,
    fields: &Fields,
    deletes: Option<&BTreeSet<String>>,
    additions: Option<&[(PreparedChunk, RetrievalMetadata)]>,
    expected_resource: Option<&ResourceKey>,
) -> Result<()> {
    let deletes_empty = deletes.map(BTreeSet::is_empty).unwrap_or(true);
    let additions_empty = additions.map(<[_]>::is_empty).unwrap_or(true);
    if deletes_empty && additions_empty {
        return Ok(());
    }

    let source_key_field = fields
        .source_key
        .ok_or_else(|| Error::Index("index is missing the source_key field".to_owned()))?;
    let mut writer = index
        .writer(WRITER_MEMORY_BYTES)
        .map_err(|error| Error::Index(error.to_string()))?;
    if let Some(keys) = deletes {
        for key in keys {
            writer.delete_term(Term::from_field_text(source_key_field, key));
        }
    }
    if let Some(chunks) = additions {
        for (chunk, retrieval) in chunks {
            if let Some(resource) = expected_resource
                && (chunk.vault_id != resource.vault_id
                    || chunk.room.as_deref() != Some(resource.room_id.as_str()))
            {
                return Err(Error::State(format!(
                    "chunk {} does not match its resource partition",
                    chunk.chunk_id
                )));
            }
            writer
                .add_document(chunk_document(fields, chunk, retrieval)?)
                .map_err(|error| Error::Index(error.to_string()))?;
        }
    }
    writer
        .commit()
        .map_err(|error| Error::Index(error.to_string()))?;
    writer
        .wait_merging_threads()
        .map_err(|error| Error::Index(error.to_string()))?;
    // A commit does not remove what this pass deleted; it only marks it dead.
    // Publishing an index that still holds dead documents would make ranking a
    // function of the index's history, so the removal is finished here.
    expunge_resident_deletions(index)
}

/// Rewrites every segment still holding documents this generation deleted, so
/// that a published index contains only what the vault currently holds.
///
/// `IndexWriter::delete_term` does not remove a document. It records an
/// opstamp, and at commit the affected segments gain an alive-bitset entry;
/// the document's postings, its term-dictionary entries and its token counts
/// all stay resident until that segment is rewritten. Retrieval is filtered by
/// the alive bitset, but the BM25 corpus statistics are not: `total_num_docs`
/// sums each segment's `max_doc` (alive *and* dead), `doc_freq` reads the term
/// dictionary, `total_num_tokens` is an inverted-index header written at index
/// time, and the average fieldnorm is the ratio of the last two. An index that
/// keeps its dead documents therefore scores live ones against a corpus that
/// no longer exists.
///
/// That is not a cosmetic difference in magnitude. Kwiry's plans are boolean
/// and disjunction-max structures over several terms across seven boosted
/// fields, so competing candidates routinely draw their evidence from terms
/// with unequal exposure to a deletion, and two candidates can swap places
/// outright — see
/// `tests::incremental_publication_orders_results_exactly_as_a_rebuild_does`,
/// which inverts the top two results over byte-identical live content. A term
/// surviving only in dead documents also remains streamable from the term
/// dictionary, where it can consume the bounded prefix-expansion budget in
/// `search::collect_prefix_expansions` and hide a live term behind it. A full
/// rebuild writes only live documents into an empty directory and so has none
/// of this by construction.
///
/// Removing the dead documents is necessary but **not** sufficient for the two
/// build paths to agree, and an earlier revision of this comment claimed
/// otherwise. Merging a segment that carries deletions is the one path on
/// which tantivy cannot preserve a field's exact token total: `merger.rs`'s
/// `estimate_total_num_tokens_in_single_segment` re-derives it by summing
/// quantised fieldnorms over the alive documents, which round down above 40
/// tokens. This function always takes that branch, by construction, so the
/// segment it publishes carries a token total a few percent below the one a
/// rebuild writes. The remaining half of the invariant is therefore enforced
/// on the read side, by `search::live_collection_length`, which derives the
/// collection length from live fieldnorms so that neither build path consults
/// a segment header at all. Neither half alone makes an incremental index rank
/// like a rebuild; `tests::an_incremental_index_and_a_rebuild_rank_a_vault_identically`
/// holds both to it over documents long enough for the quantiser to be lossy.
///
/// Tantivy will not do this on its own. `LogMergePolicy`'s
/// `del_docs_ratio_before_merge` defaults to 1.0 and the comparison is
/// strictly greater-than, so no number of deletions makes a segment a merge
/// candidate; the only remaining trigger is the eight-segments-per-level rule,
/// which a delete-only publication — open-time format eviction, the largest
/// single deletion this system issues — can never reach, because it creates no
/// segment at all.
///
/// Cost is paid only when there is something to expunge: the segment metadata
/// carries the deleted count, so the ordinary publication that deletes nothing
/// reads a handful of integers and returns, and
/// `tests::a_publication_that_deletes_nothing_does_not_rewrite_existing_segments`
/// holds it to that. When deletions are present this rewrites exactly the
/// segments carrying them, so an edit to one note rewrites the one segment
/// that held it rather than the index. Measured on an index this build wrote
/// over 10,000 notes — 60,000 chunks in 75 MiB across six segments, which
/// `kwiry index` rebuilds from source in 11.4 s: expunging one edited note's
/// chunks costs 342 ms and leaves the other five segments untouched, and the
/// worst case, a deletion spread across every segment, costs 1.0–1.2 s. On a
/// 12,000-chunk index the same two figures are 227 ms and 289 ms. Against the
/// surrounding pass — a reconciliation of that vault takes 3.3 s before any of
/// this — correctness costs roughly a tenth of a publication that deletes one
/// note and under a third of one that deletes thousands.
///
/// It runs after `wait_merging_threads` has drained the policy's own merges
/// and while the data-root writer lock is held on a candidate no reader can
/// see, so no segment named here is already in a merge.
fn expunge_resident_deletions(index: &Index) -> Result<()> {
    let resident: Vec<SegmentId> = index
        .searchable_segment_metas()
        .map_err(|error| Error::Index(error.to_string()))?
        .into_iter()
        .filter(|meta| meta.num_deleted_docs() > 0)
        .map(|meta| meta.id())
        .collect();
    if resident.is_empty() {
        return Ok(());
    }
    let mut writer: IndexWriter = index
        .writer(WRITER_MEMORY_BYTES)
        .map_err(|error| Error::Index(error.to_string()))?;
    writer
        .merge(&resident)
        .wait()
        .map_err(|error| Error::Index(error.to_string()))?;
    writer
        .wait_merging_threads()
        .map_err(|error| Error::Index(error.to_string()))?;
    Ok(())
}

/// Whether an index directory still holds documents a previous build deleted.
///
/// Reads the committed segment metadata only, which already carries each
/// segment's deleted count, so an index that is clean costs a `meta.json`
/// parse and no segment I/O at all.
fn holds_resident_deletions(index_dir: &Path) -> Result<bool> {
    let (index, _) = open_index_dir(index_dir)?;
    Ok(index
        .searchable_segment_metas()
        .map_err(|error| Error::Index(error.to_string()))?
        .iter()
        .any(|meta| meta.num_deleted_docs() > 0))
}

/// Finishes a removal that an *earlier* build left half-done, before the
/// inherited generation is served.
///
/// `apply_index_updates` guarantees the no-dead-documents invariant only for
/// generations this build publishes, and it is not reached at all by a pass
/// with nothing to add and nothing to delete. A generation written by a build
/// that predates `expunge_resident_deletions` therefore survives an upgrade
/// intact: opened, reconciled against an unchanged vault, and served — with
/// its dead documents counted in every query's corpus size and their
/// vocabulary still streamable from the term dictionary — for as long as the
/// vault stays still. That is precisely the state
/// `search::AuthorizedStatistics` and `search::collect_prefix_expansions`
/// document themselves as relying on the invariant to exclude.
///
/// So the invariant is re-established where the index is adopted rather than
/// only where it is written: an inherited generation is repaired into a fresh
/// one before any reader can see it. The repair is a publication like any
/// other — a candidate built from the active generation, expunged, then
/// published — so a crash midway leaves the previous generation active and the
/// next open repeats it. It costs one segment-metadata read on every open that
/// finds nothing to do, which is every open after the first.
fn repair_inherited_generation(index_dir: &Path) -> Result<()> {
    let (index, _) = open_index_dir(index_dir)?;
    expunge_resident_deletions(&index)
}

fn ensure_partition_index(index_dir: &Path) -> Result<(Index, Fields)> {
    if index_dir.join("meta.json").is_file() {
        return open_index_dir(index_dir);
    }
    fs::create_dir_all(index_dir).map_err(|error| crate::error::io_error(index_dir, error))?;
    let schema = build_schema();
    let fields = Fields::from_schema(&schema)?;
    let index =
        Index::create_in_dir(index_dir, schema).map_err(|error| Error::Index(error.to_string()))?;
    register_lexical_analyzer(&index);
    Ok((index, fields))
}

pub struct IndexManager {
    inner: IndexManagerInner,
}

enum IndexManagerInner {
    Desktop(DesktopIndexManager),
    OpenClast(OpenClastIndexManager),
}

impl IndexManager {
    pub fn open(config: Config, data_dir: &Path, runtime: SearchRuntime) -> Result<Self> {
        let inner = match config.server.profile {
            HostProfile::Desktop => {
                IndexManagerInner::Desktop(DesktopIndexManager::open(config, data_dir, runtime)?)
            }
            HostProfile::OpenClast => IndexManagerInner::OpenClast(OpenClastIndexManager::open(
                config, data_dir, runtime,
            )?),
        };
        Ok(Self { inner })
    }

    pub fn manifest(&self) -> &Manifest {
        match &self.inner {
            IndexManagerInner::Desktop(manager) => manager.manifest(),
            IndexManagerInner::OpenClast(manager) => manager.manifest(),
        }
    }

    /// What open-time eviction removed, if anything. Empty on the ordinary
    /// path; non-empty exactly when a format's identity moved under a
    /// reusable index, which must be reported rather than narrowed silently.
    pub fn open_evictions(&self) -> &EvictionReport {
        match &self.inner {
            IndexManagerInner::Desktop(manager) => &manager.evictions,
            IndexManagerInner::OpenClast(manager) => &manager.evictions,
        }
    }

    pub fn config(&self) -> &Config {
        match &self.inner {
            IndexManagerInner::Desktop(manager) => manager.config(),
            IndexManagerInner::OpenClast(manager) => manager.config(),
        }
    }

    pub fn reconcile(&mut self, config: Config) -> Result<ReconcileReport> {
        self.reconcile_scoped(config, &ReconcileScope::Full)
    }

    pub fn reconcile_scoped(
        &mut self,
        config: Config,
        scope: &ReconcileScope,
    ) -> Result<ReconcileReport> {
        if self.config().requires_restart_for(&config) {
            return Err(Error::State(
                "startup configuration changed; restart the daemon to apply it".to_owned(),
            ));
        }
        match &mut self.inner {
            IndexManagerInner::Desktop(manager) => manager.reconcile(config, scope),
            IndexManagerInner::OpenClast(manager) => manager.reconcile(config, scope),
        }
    }

    pub fn shutdown(self) -> Result<()> {
        match self.inner {
            IndexManagerInner::Desktop(manager) => manager.shutdown(),
            IndexManagerInner::OpenClast(manager) => manager.shutdown(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReconcileReport {
    pub changed_sources: usize,
    pub added_chunks: usize,
    pub source_files_read: usize,
    pub source_bytes_read: u64,
    pub audited_sources: usize,
    pub audit_pending: bool,
    pub freshness_basis: IndexFreshnessBasis,
    pub documents: usize,
    pub chunks: usize,
    pub last_sync: Option<String>,
    pub warnings: Vec<IngestWarning>,
    pub unavailable_vaults: Vec<String>,
    pub generation: Option<String>,
    pub manifest: Manifest,
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::fs;
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;

    use filetime::{FileTime, set_file_mtime};
    use tempfile::tempdir;

    use super::*;
    use crate::chunk::ingest_vault_files;
    use crate::format::SourceFormat;
    use crate::index::build_index;
    use crate::model::VaultRegistration;

    #[cfg(target_os = "linux")]
    mod snapshots;

    fn request(query: &str) -> LexicalSearchRequest {
        LexicalSearchRequest {
            query: query.into(),
            limit: 20,
            vault_id: None,
        }
    }

    fn snapshot_tree(root: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
        fn visit(root: &Path, path: &Path, snapshot: &mut BTreeMap<PathBuf, Vec<u8>>) {
            let mut entries: Vec<_> = fs::read_dir(path)
                .unwrap()
                .map(|entry| entry.unwrap())
                .collect();
            entries.sort_by_key(|entry| entry.file_name());
            for entry in entries {
                let path = entry.path();
                if entry.file_type().unwrap().is_dir() {
                    visit(root, &path, snapshot);
                } else {
                    snapshot.insert(
                        path.strip_prefix(root).unwrap().to_path_buf(),
                        fs::read(path).unwrap(),
                    );
                }
            }
        }

        let mut snapshot = BTreeMap::new();
        visit(root, root, &mut snapshot);
        snapshot
    }

    #[test]
    fn reconcile_edits_deletes_and_renames_without_orphans() {
        let temporary = tempdir().unwrap();
        let vault_path = temporary.path().join("vault");
        let data_root = temporary.path().join("data");
        fs::create_dir(&vault_path).unwrap();
        let note = vault_path.join("note.md");
        fs::write(&note, "# One\noldterm").unwrap();
        let config = Config {
            vaults: vec![VaultRegistration {
                snapshot_id: None,
                id: "fixture".into(),
                path: vault_path.clone(),
                room: None,
            }],
            ..Config::default()
        };
        build_index(&config, &data_root).unwrap();
        let runtime = SearchRuntime::new();
        let mut manager = IndexManager::open(config.clone(), &data_root, runtime.clone()).unwrap();
        let old_id = runtime.search(&request("oldterm")).unwrap()[0]
            .chunk_id
            .clone();
        let previous_generation = DataRoot::new(&data_root).active().unwrap().unwrap();
        let previous_snapshot = snapshot_tree(&previous_generation.root);

        fs::write(&note, "# One\nnewterm").unwrap();
        manager.reconcile(config.clone()).unwrap();
        let current_generation = DataRoot::new(&data_root).active().unwrap().unwrap();
        assert_ne!(current_generation.id, previous_generation.id);
        assert_eq!(snapshot_tree(&previous_generation.root), previous_snapshot);
        assert!(runtime.search(&request("oldterm")).unwrap().is_empty());
        assert_eq!(runtime.search(&request("newterm")).unwrap().len(), 1);

        let moved = vault_path.join("moved.md");
        fs::rename(&note, &moved).unwrap();
        manager.reconcile(config.clone()).unwrap();
        let moved_hit = &runtime.search(&request("newterm")).unwrap()[0];
        assert_eq!(moved_hit.path, "moved.md");
        assert_ne!(moved_hit.chunk_id, old_id);

        fs::remove_file(moved).unwrap();
        manager.reconcile(config).unwrap();
        assert!(runtime.search(&request("newterm")).unwrap().is_empty());
        manager.shutdown().unwrap();
    }

    #[test]
    fn reconcile_detects_same_size_same_mtime_content_changes() {
        let temporary = tempdir().unwrap();
        let vault_path = temporary.path().join("vault");
        let data_root = temporary.path().join("data");
        fs::create_dir(&vault_path).unwrap();
        let note = vault_path.join("note.md");
        let fixed_mtime = FileTime::from_unix_time(1_700_000_000, 123_456_789);
        fs::write(&note, "oldterm").unwrap();
        set_file_mtime(&note, fixed_mtime).unwrap();
        let config = Config {
            vaults: vec![VaultRegistration {
                snapshot_id: None,
                id: "fixture".into(),
                path: vault_path,
                room: None,
            }],
            ..Config::default()
        };
        build_index(&config, &data_root).unwrap();
        let runtime = SearchRuntime::new();
        let mut manager = IndexManager::open(config.clone(), &data_root, runtime.clone()).unwrap();

        fs::write(&note, "newterm").unwrap();
        set_file_mtime(&note, fixed_mtime).unwrap();
        manager.reconcile(config).unwrap();

        assert!(runtime.search(&request("oldterm")).unwrap().is_empty());
        assert_eq!(runtime.search(&request("newterm")).unwrap().len(), 1);
        manager.shutdown().unwrap();
    }

    #[test]
    fn reconcile_detects_content_changes_when_mtime_moves_backwards() {
        let temporary = tempdir().unwrap();
        let vault_path = temporary.path().join("vault");
        let data_root = temporary.path().join("data");
        fs::create_dir(&vault_path).unwrap();
        let note = vault_path.join("note.md");
        fs::write(&note, "forward").unwrap();
        set_file_mtime(&note, FileTime::from_unix_time(1_700_000_100, 0)).unwrap();
        let config = Config {
            vaults: vec![VaultRegistration {
                snapshot_id: None,
                id: "fixture".into(),
                path: vault_path,
                room: None,
            }],
            ..Config::default()
        };
        build_index(&config, &data_root).unwrap();
        let runtime = SearchRuntime::new();
        let mut manager = IndexManager::open(config.clone(), &data_root, runtime.clone()).unwrap();

        fs::write(&note, "backward").unwrap();
        set_file_mtime(&note, FileTime::from_unix_time(1_700_000_000, 0)).unwrap();
        manager.reconcile(config).unwrap();

        assert!(runtime.search(&request("forward")).unwrap().is_empty());
        assert_eq!(runtime.search(&request("backward")).unwrap().len(), 1);
        manager.shutdown().unwrap();
    }

    #[test]
    fn oversized_warning_suppresses_unrelated_deletion_inference() {
        let temporary = tempdir().unwrap();
        let vault_path = temporary.path().join("vault");
        let data_root = temporary.path().join("data");
        fs::create_dir(&vault_path).unwrap();
        let retained = vault_path.join("retained.md");
        fs::write(&retained, "retainedterm").unwrap();
        let config = Config {
            vaults: vec![VaultRegistration {
                snapshot_id: None,
                id: "fixture".into(),
                path: vault_path.clone(),
                room: None,
            }],
            ..Config::default()
        };
        build_index(&config, &data_root).unwrap();
        let runtime = SearchRuntime::new();
        let mut manager = IndexManager::open(config.clone(), &data_root, runtime.clone()).unwrap();

        fs::remove_file(retained).unwrap();
        fs::write(
            vault_path.join("oversized.md"),
            vec![b'x'; crate::model::MAX_FILE_BYTES as usize + 1],
        )
        .unwrap();
        let report = manager.reconcile(config).unwrap();

        assert!(
            report
                .warnings
                .iter()
                .any(|warning| warning.message.contains("skipped file larger"))
        );
        assert_eq!(runtime.search(&request("retainedterm")).unwrap().len(), 1);
        manager.shutdown().unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn transient_read_error_retains_previous_manifest_and_chunks() {
        let temporary = tempdir().unwrap();
        let vault_path = temporary.path().join("vault");
        let data_root = temporary.path().join("data");
        fs::create_dir(&vault_path).unwrap();
        let note = vault_path.join("note.md");
        fs::write(&note, "transientterm").unwrap();
        let config = Config {
            vaults: vec![VaultRegistration {
                snapshot_id: None,
                id: "fixture".into(),
                path: vault_path,
                room: None,
            }],
            ..Config::default()
        };
        build_index(&config, &data_root).unwrap();
        let runtime = SearchRuntime::new();
        let mut manager = IndexManager::open(config.clone(), &data_root, runtime.clone()).unwrap();
        let previous_manifest = manager.manifest().clone();

        let mut permissions = fs::metadata(&note).unwrap().permissions();
        permissions.set_mode(0o000);
        fs::set_permissions(&note, permissions).unwrap();
        let report = manager.reconcile(config).unwrap();

        let mut permissions = fs::metadata(&note).unwrap().permissions();
        permissions.set_mode(0o600);
        fs::set_permissions(&note, permissions).unwrap();
        assert!(!report.warnings.is_empty());
        assert_eq!(manager.manifest(), &previous_manifest);
        assert_eq!(runtime.search(&request("transientterm")).unwrap().len(), 1);
        manager.shutdown().unwrap();
    }

    #[test]
    fn metadata_audit_reuses_settled_unchanged_sources_without_reading_bytes() {
        let temporary = tempdir().unwrap();
        let vault_path = temporary.path().join("vault");
        let data_root = temporary.path().join("data");
        fs::create_dir(&vault_path).unwrap();
        let settled = FileTime::from_unix_time(1_700_000_000, 0);
        for index in 0..3 {
            let note = vault_path.join(format!("note-{index}.md"));
            fs::write(&note, format!("settledterm {index}")).unwrap();
            set_file_mtime(&note, settled).unwrap();
        }
        let mut config = Config {
            vaults: vec![VaultRegistration {
                snapshot_id: None,
                id: "fixture".into(),
                path: vault_path,
                room: None,
            }],
            ..Config::default()
        };
        build_index(&config, &data_root).unwrap();
        config.indexing.basis = IndexFreshnessBasis::MetadataAudit;
        config.indexing.audit_sources_per_pass = 1;
        let runtime = SearchRuntime::new();
        let mut manager = IndexManager::open(config.clone(), &data_root, runtime.clone()).unwrap();
        let generation_before = runtime.generation();

        let report = manager.reconcile(config).unwrap();

        assert_eq!(report.freshness_basis, IndexFreshnessBasis::MetadataAudit);
        assert_eq!(report.changed_sources, 0);
        assert_eq!(report.source_files_read, 1);
        assert_eq!(report.audited_sources, 1);
        assert!(report.audit_pending);
        assert_eq!(runtime.generation(), generation_before);
        assert_eq!(runtime.search(&request("settledterm")).unwrap().len(), 3);
        assert_eq!(
            runtime.freshness_basis(),
            IndexFreshnessBasis::MetadataAudit
        );
        manager.shutdown().unwrap();
    }

    #[test]
    fn metadata_audit_rolling_audit_catches_metadata_equal_content_change() {
        let temporary = tempdir().unwrap();
        let vault_path = temporary.path().join("vault");
        let data_root = temporary.path().join("data");
        fs::create_dir(&vault_path).unwrap();
        let settled = FileTime::from_unix_time(1_700_000_000, 0);
        let note = vault_path.join("note.md");
        fs::write(&note, "oldterm").unwrap();
        set_file_mtime(&note, settled).unwrap();
        let mut config = Config {
            vaults: vec![VaultRegistration {
                snapshot_id: None,
                id: "fixture".into(),
                path: vault_path,
                room: None,
            }],
            ..Config::default()
        };
        build_index(&config, &data_root).unwrap();
        config.indexing.basis = IndexFreshnessBasis::MetadataAudit;
        let runtime = SearchRuntime::new();
        let mut manager = IndexManager::open(config.clone(), &data_root, runtime.clone()).unwrap();

        // Same byte length and restored mtime: only the rolling audit can
        // observe this change without a watcher hint.
        fs::write(&note, "newterm").unwrap();
        set_file_mtime(&note, settled).unwrap();
        let report = manager.reconcile(config).unwrap();

        assert_eq!(report.audited_sources, 1);
        assert!(!report.audit_pending);
        assert_eq!(report.changed_sources, 1);
        assert!(runtime.search(&request("oldterm")).unwrap().is_empty());
        assert_eq!(runtime.search(&request("newterm")).unwrap().len(), 1);
        manager.shutdown().unwrap();
    }

    #[test]
    fn metadata_audit_reads_size_changed_sources_outside_the_audit_budget() {
        let temporary = tempdir().unwrap();
        let vault_path = temporary.path().join("vault");
        let data_root = temporary.path().join("data");
        fs::create_dir(&vault_path).unwrap();
        let settled = FileTime::from_unix_time(1_700_000_000, 0);
        let changed = vault_path.join("changed.md");
        let untouched = vault_path.join("untouched.md");
        fs::write(&changed, "before").unwrap();
        fs::write(&untouched, "untouchedterm").unwrap();
        set_file_mtime(&changed, settled).unwrap();
        set_file_mtime(&untouched, settled).unwrap();
        let mut config = Config {
            vaults: vec![VaultRegistration {
                snapshot_id: None,
                id: "fixture".into(),
                path: vault_path,
                room: None,
            }],
            ..Config::default()
        };
        build_index(&config, &data_root).unwrap();
        config.indexing.basis = IndexFreshnessBasis::MetadataAudit;
        config.indexing.audit_sources_per_pass = 1;
        let runtime = SearchRuntime::new();
        let mut manager = IndexManager::open(config.clone(), &data_root, runtime.clone()).unwrap();

        fs::write(&changed, "after grew larger").unwrap();
        set_file_mtime(&changed, settled).unwrap();
        let report = manager.reconcile(config).unwrap();

        // The changed source is read because its size changed; the audit
        // budget is spent on the remaining metadata-equal candidate.
        assert_eq!(report.source_files_read, 2);
        assert_eq!(report.audited_sources, 1);
        assert_eq!(report.changed_sources, 1);
        assert_eq!(runtime.search(&request("larger")).unwrap().len(), 1);
        assert_eq!(runtime.search(&request("untouchedterm")).unwrap().len(), 1);
        manager.shutdown().unwrap();
    }

    #[test]
    fn watch_event_forces_a_read_for_a_same_size_same_mtime_edit() {
        let temporary = tempdir().unwrap();
        let vault_path = temporary.path().join("vault");
        let data_root = temporary.path().join("data");
        fs::create_dir(&vault_path).unwrap();
        let settled = FileTime::from_unix_time(1_700_000_000, 0);
        let edited = vault_path.join("edited.md");
        let untouched = vault_path.join("untouched.md");
        fs::write(&edited, "oldterm").unwrap();
        fs::write(&untouched, "untouchedterm").unwrap();
        set_file_mtime(&edited, settled).unwrap();
        set_file_mtime(&untouched, settled).unwrap();
        let mut config = Config {
            vaults: vec![VaultRegistration {
                snapshot_id: None,
                id: "fixture".into(),
                path: vault_path,
                room: None,
            }],
            ..Config::default()
        };
        build_index(&config, &data_root).unwrap();
        config.indexing.basis = IndexFreshnessBasis::MetadataAudit;
        let runtime = SearchRuntime::new();
        let mut manager = IndexManager::open(config.clone(), &data_root, runtime.clone()).unwrap();

        // Same byte length, restored mtime: without the watcher evidence a
        // scoped metadata pass could not see this change.
        fs::write(&edited, "newterm").unwrap();
        set_file_mtime(&edited, settled).unwrap();
        let scope = ReconcileScope::Paths(BTreeSet::from([(
            "fixture".to_owned(),
            "edited.md".to_owned(),
        )]));
        let report = manager.reconcile_scoped(config, &scope).unwrap();

        // Exactly one read (the forced event path); the audit is parked on
        // scoped passes and the untouched source is reused.
        assert_eq!(report.source_files_read, 1);
        assert_eq!(report.audited_sources, 0);
        assert_eq!(report.changed_sources, 1);
        assert!(runtime.search(&request("oldterm")).unwrap().is_empty());
        assert_eq!(runtime.search(&request("newterm")).unwrap().len(), 1);
        assert_eq!(runtime.search(&request("untouchedterm")).unwrap().len(), 1);
        manager.shutdown().unwrap();
    }

    #[test]
    fn scoped_pass_still_detects_deletions_from_complete_enumeration() {
        let temporary = tempdir().unwrap();
        let vault_path = temporary.path().join("vault");
        let data_root = temporary.path().join("data");
        fs::create_dir(&vault_path).unwrap();
        let settled = FileTime::from_unix_time(1_700_000_000, 0);
        let doomed = vault_path.join("doomed.md");
        fs::write(&doomed, "doomedterm").unwrap();
        set_file_mtime(&doomed, settled).unwrap();
        let mut config = Config {
            vaults: vec![VaultRegistration {
                snapshot_id: None,
                id: "fixture".into(),
                path: vault_path,
                room: None,
            }],
            ..Config::default()
        };
        build_index(&config, &data_root).unwrap();
        config.indexing.basis = IndexFreshnessBasis::MetadataAudit;
        let runtime = SearchRuntime::new();
        let mut manager = IndexManager::open(config.clone(), &data_root, runtime.clone()).unwrap();

        // The deletion is NOT in the scope set: enumeration completeness,
        // not the watcher evidence, is what authorizes removal.
        fs::remove_file(&doomed).unwrap();
        let scope = ReconcileScope::Paths(BTreeSet::from([(
            "fixture".to_owned(),
            "unrelated.md".to_owned(),
        )]));
        manager.reconcile_scoped(config, &scope).unwrap();

        assert!(runtime.search(&request("doomedterm")).unwrap().is_empty());
        manager.shutdown().unwrap();
    }

    #[test]
    fn scoped_reconciliation_converges_to_a_full_rebuild() {
        let temporary = tempdir().unwrap();
        let vault_path = temporary.path().join("vault");
        let data_root = temporary.path().join("data");
        let fresh_root = temporary.path().join("fresh");
        fs::create_dir(&vault_path).unwrap();
        let settled = FileTime::from_unix_time(1_700_000_000, 0);
        for index in 0..4 {
            let note = vault_path.join(format!("note-{index}.md"));
            fs::write(&note, format!("# Note {index}\nseedterm {index}")).unwrap();
            set_file_mtime(&note, settled).unwrap();
        }
        let mut config = Config {
            vaults: vec![VaultRegistration {
                snapshot_id: None,
                id: "fixture".into(),
                path: vault_path.clone(),
                room: None,
            }],
            ..Config::default()
        };
        build_index(&config, &data_root).unwrap();
        config.indexing.basis = IndexFreshnessBasis::MetadataAudit;
        let runtime = SearchRuntime::new();
        let mut manager = IndexManager::open(config.clone(), &data_root, runtime.clone()).unwrap();
        let scoped = |paths: &[&str]| {
            ReconcileScope::Paths(
                paths
                    .iter()
                    .map(|path| ("fixture".to_owned(), (*path).to_owned()))
                    .collect(),
            )
        };

        // Edit, rename, delete, and create, each through a scoped pass.
        fs::write(vault_path.join("note-0.md"), "# Note 0\neditedterm").unwrap();
        manager
            .reconcile_scoped(config.clone(), &scoped(&["note-0.md"]))
            .unwrap();
        fs::rename(vault_path.join("note-1.md"), vault_path.join("moved.md")).unwrap();
        manager
            .reconcile_scoped(config.clone(), &scoped(&["note-1.md", "moved.md"]))
            .unwrap();
        fs::remove_file(vault_path.join("note-2.md")).unwrap();
        manager
            .reconcile_scoped(config.clone(), &scoped(&["note-2.md"]))
            .unwrap();
        fs::write(vault_path.join("created.md"), "# Created\ncreatedterm").unwrap();
        manager
            .reconcile_scoped(config.clone(), &scoped(&["created.md"]))
            .unwrap();
        let final_report = manager.reconcile(config.clone()).unwrap();

        // A fresh authoritative rebuild over the same bytes must agree on
        // the manifest key set and every query.
        let mut fresh_config = config.clone();
        fresh_config.indexing.basis = IndexFreshnessBasis::StrictHash;
        build_index(&fresh_config, &fresh_root).unwrap();
        let fresh_runtime = SearchRuntime::new();
        let fresh_manager =
            IndexManager::open(fresh_config, &fresh_root, fresh_runtime.clone()).unwrap();
        let fresh_keys: BTreeSet<_> = fresh_manager.manifest().files.keys().cloned().collect();
        let scoped_keys: BTreeSet<_> = final_report.manifest.files.keys().cloned().collect();
        assert_eq!(scoped_keys, fresh_keys);
        for query in ["seedterm", "editedterm", "createdterm", "moved", "note"] {
            let scoped_hits: Vec<_> = runtime
                .search(&request(query))
                .unwrap()
                .into_iter()
                .map(|hit| (hit.chunk_id, hit.path))
                .collect();
            let fresh_hits: Vec<_> = fresh_runtime
                .search(&request(query))
                .unwrap()
                .into_iter()
                .map(|hit| (hit.chunk_id, hit.path))
                .collect();
            assert_eq!(scoped_hits, fresh_hits, "query {query} diverged");
        }
        manager.shutdown().unwrap();
        fresh_manager.shutdown().unwrap();
    }

    #[cfg_attr(
        not(target_os = "linux"),
        ignore = "secure OpenClast ingestion is Linux-only pending platform guidance"
    )]
    #[test]
    fn a_vanished_partition_returns_a_typed_retriable_state() {
        let temporary = tempdir().unwrap();
        let vault_path = temporary.path().join("vault");
        let data = temporary.path().join("data");
        fs::create_dir(&vault_path).unwrap();
        fs::write(vault_path.join("note.md"), "vanishterm").unwrap();
        let config = openclast_config(vec![VaultRegistration {
            snapshot_id: None,
            id: "vault-a".into(),
            path: vault_path,
            room: Some("room-a".into()),
        }]);
        build_index(&config, &data).unwrap();
        let active = DataRoot::new(&data).active().unwrap().unwrap();
        let resource = config.resource_key(&config.vaults[0]).unwrap();
        let partitions = PartitionedSearchIndex::open(&active).unwrap();

        // Simulate the generation being pruned while this stale reader is
        // still installed, before any request opened the partition.
        fs::remove_dir_all(partition_index_dir(&active.partitions_dir, &resource)).unwrap();
        let error = partitions
            .search(
                "vanishterm",
                20,
                &SearchFilters::default(),
                std::slice::from_ref(&resource),
            )
            .unwrap_err();
        assert!(matches!(error, Error::UnavailableResourceIndexes(indexes) if indexes == [0]));
    }

    #[cfg_attr(
        not(target_os = "linux"),
        ignore = "secure OpenClast ingestion is Linux-only pending platform guidance"
    )]
    #[test]
    fn openclast_metadata_audit_reuses_unchanged_partition_sources() {
        let temporary = tempdir().unwrap();
        let vault_path = temporary.path().join("vault");
        let data_root = temporary.path().join("data");
        fs::create_dir(&vault_path).unwrap();
        let settled = FileTime::from_unix_time(1_700_000_000, 0);
        let note = vault_path.join("note.md");
        fs::write(&note, "partitionterm").unwrap();
        set_file_mtime(&note, settled).unwrap();
        let mut config = openclast_config(vec![VaultRegistration {
            snapshot_id: None,
            id: "vault-a".into(),
            path: vault_path,
            room: Some("room-a".into()),
        }]);
        build_index(&config, &data_root).unwrap();
        config.indexing.basis = IndexFreshnessBasis::MetadataAudit;
        config.indexing.audit_sources_per_pass = 1;
        let runtime = SearchRuntime::new();
        let mut manager = IndexManager::open(config.clone(), &data_root, runtime.clone()).unwrap();
        let generation_before = runtime.generation();

        let report = manager.reconcile(config.clone()).unwrap();

        // The single source is metadata-equal, so the only read is its audit.
        assert_eq!(report.changed_sources, 0);
        assert_eq!(report.source_files_read, 1);
        assert_eq!(report.audited_sources, 1);
        assert!(!report.audit_pending);
        assert_eq!(runtime.generation(), generation_before);
        let resource = config.resource_key(&config.vaults[0]).unwrap();
        assert_eq!(
            runtime
                .search_authorized("partitionterm", 20, &SearchFilters::default(), &[resource],)
                .unwrap()
                .len(),
            1
        );
        manager.shutdown().unwrap();
    }

    #[test]
    fn reconcile_refreshes_alias_and_filename_evidence() {
        let temporary = tempdir().unwrap();
        let vault_path = temporary.path().join("vault");
        let data_root = temporary.path().join("data");
        fs::create_dir(&vault_path).unwrap();
        let note = vault_path.join("note.md");
        fs::write(
            &note,
            "---\naliases: [OLD 2 line]\n---\n# Governance\nspecialist oversight",
        )
        .unwrap();
        let config = Config {
            vaults: vec![VaultRegistration {
                snapshot_id: None,
                id: "fixture".into(),
                path: vault_path.clone(),
                room: None,
            }],
            ..Config::default()
        };
        build_index(&config, &data_root).unwrap();
        let runtime = SearchRuntime::new();
        let mut manager = IndexManager::open(config.clone(), &data_root, runtime.clone()).unwrap();

        assert_eq!(runtime.search(&request("OLD 2 line")).unwrap().len(), 1);
        fs::write(
            &note,
            "---\naliases: [NEW 2 line]\n---\n# Governance\nspecialist oversight",
        )
        .unwrap();
        manager.reconcile(config.clone()).unwrap();
        assert!(runtime.search(&request("OLD 2 line")).unwrap().is_empty());
        assert_eq!(runtime.search(&request("NEW 2 line")).unwrap().len(), 1);

        let renamed = vault_path.join("RENAMED-2-line.md");
        fs::rename(note, &renamed).unwrap();
        manager.reconcile(config).unwrap();
        let hits = runtime.search(&request("RENAMED 2 line")).unwrap();
        assert!(!hits.is_empty());
        assert!(hits.iter().all(|hit| hit.path == "RENAMED-2-line.md"));
        manager.shutdown().unwrap();
    }

    #[test]
    fn lexical_filters_are_combined_with_and_semantics() {
        let temporary = tempdir().unwrap();
        let vault_path = temporary.path().join("vault");
        let data_root = temporary.path().join("data");
        fs::create_dir_all(vault_path.join("docs")).unwrap();
        fs::write(
            vault_path.join("docs/note.md"),
            "---\ntitle: Filter Note\ndescription: Exact description\ntags: [alpha, beta]\nstatus: active\ndate: 2026-07-19\n---\n# Search\nfilterterm",
        )
        .unwrap();
        let config = Config {
            vaults: vec![VaultRegistration {
                snapshot_id: None,
                id: "fixture".into(),
                path: vault_path,
                room: Some("room-a".into()),
            }],
            ..Config::default()
        };
        build_index(&config, &data_root).unwrap();
        let runtime = SearchRuntime::new();
        let manager = IndexManager::open(config, &data_root, runtime.clone()).unwrap();
        let filters = SearchFilters {
            vault_id: Some("fixture".into()),
            room: Some("room-a".into()),
            path_prefix: Some("docs/".into()),
            tags: vec!["alpha".into(), "beta".into()],
            frontmatter_equals: BTreeMap::from([
                ("title".into(), "Filter Note".into()),
                ("status".into(), "active".into()),
            ]),
        };
        assert_eq!(
            runtime
                .search_filtered("filterterm", 20, &filters)
                .unwrap()
                .len(),
            1
        );
        let mut wrong = filters;
        wrong.tags.push("missing".into());
        assert!(
            runtime
                .search_filtered("filterterm", 20, &wrong)
                .unwrap()
                .is_empty()
        );
        manager.shutdown().unwrap();
    }

    #[test]
    fn semantic_and_hybrid_search_follow_reconciliation() {
        use crate::semantic::test_support::HashEmbedder;

        let temporary = tempdir().unwrap();
        let vault_path = temporary.path().join("vault");
        let data_root = temporary.path().join("data");
        fs::create_dir(&vault_path).unwrap();
        fs::write(
            vault_path.join("garden.md"),
            "# Garden\nwatering tomatoes in the greenhouse",
        )
        .unwrap();
        fs::write(
            vault_path.join("search.md"),
            "# Search\nphosphorescent deterministic indexing",
        )
        .unwrap();
        let config = Config {
            vaults: vec![VaultRegistration {
                snapshot_id: None,
                id: "fixture".into(),
                path: vault_path.clone(),
                room: None,
            }],
            ..Config::default()
        };
        build_index(&config, &data_root).unwrap();
        let runtime = SearchRuntime::new();
        let embedder = Box::new(HashEmbedder::new());
        let semantic =
            SemanticRuntime::open(&temporary.path().join("semantic.db"), embedder).unwrap();
        runtime.install_semantic(Arc::new(semantic));
        let mut manager = IndexManager::open(config.clone(), &data_root, runtime.clone()).unwrap();
        manager.reconcile(config.clone()).unwrap();

        let filters = SearchFilters::default();
        // Semantic search finds vocabulary overlap, hydrated with real paths.
        let semantic_hits = runtime
            .search_semantic("tomatoes greenhouse", 5, &filters)
            .unwrap();
        assert!(!semantic_hits.is_empty());
        assert_eq!(semantic_hits[0].path, "garden.md");

        // Hybrid fuses both legs and stays deterministic.
        let hybrid_one = runtime
            .search_hybrid("phosphorescent indexing", 5, &filters)
            .unwrap();
        let hybrid_two = runtime
            .search_hybrid("phosphorescent indexing", 5, &filters)
            .unwrap();
        assert_eq!(hybrid_one[0].path, "search.md");
        let ids_one: Vec<_> = hybrid_one.iter().map(|hit| &hit.chunk_id).collect();
        let ids_two: Vec<_> = hybrid_two.iter().map(|hit| &hit.chunk_id).collect();
        assert_eq!(ids_one, ids_two);

        // An edit flows through reconcile into the semantic leg.
        fs::write(
            vault_path.join("garden.md"),
            "# Garden\nrepotting orchids in the conservatory",
        )
        .unwrap();
        manager.reconcile(config.clone()).unwrap();
        let after_edit = runtime
            .search_semantic("orchids conservatory", 5, &filters)
            .unwrap();
        assert_eq!(after_edit[0].path, "garden.md");

        // A delete removes the source from the semantic leg entirely.
        fs::remove_file(vault_path.join("garden.md")).unwrap();
        manager.reconcile(config).unwrap();
        let after_delete = runtime
            .search_semantic("orchids conservatory", 5, &filters)
            .unwrap();
        assert!(after_delete.iter().all(|hit| hit.path != "garden.md"));
        manager.shutdown().unwrap();
    }

    #[cfg_attr(
        not(target_os = "linux"),
        ignore = "secure OpenClast ingestion is Linux-only pending platform guidance"
    )]
    #[test]
    fn semantic_unavailable_without_model_while_lexical_serves() {
        let temporary = tempdir().unwrap();
        let vault_path = temporary.path().join("vault");
        let data_root = temporary.path().join("data");
        fs::create_dir(&vault_path).unwrap();
        fs::write(vault_path.join("note.md"), "phosphorescent").unwrap();
        let config = Config {
            vaults: vec![VaultRegistration {
                snapshot_id: None,
                id: "fixture".into(),
                path: vault_path,
                room: None,
            }],
            ..Config::default()
        };
        build_index(&config, &data_root).unwrap();
        let runtime = SearchRuntime::new();
        let manager = IndexManager::open(config, &data_root, runtime.clone()).unwrap();

        assert_eq!(runtime.search(&request("phosphorescent")).unwrap().len(), 1);
        assert!(!runtime.semantic_ready());
        let filters = SearchFilters::default();
        let error = runtime
            .search_semantic("phosphorescent", 5, &filters)
            .unwrap_err();
        assert!(matches!(error, Error::SemanticUnavailable(_)));
        let error = runtime
            .search_hybrid("phosphorescent", 5, &filters)
            .unwrap_err();
        assert!(matches!(error, Error::SemanticUnavailable(_)));
        manager.shutdown().unwrap();
    }

    fn openclast_config(vaults: Vec<VaultRegistration>) -> Config {
        let mut config = Config::default();
        config.server.profile = HostProfile::OpenClast;
        config.auth.openclast = Some(crate::model::OpenClastAuthConfig {
            tenant_id: "tenant-a".into(),
            issuer: "openclast-search".into(),
            audience: "kwiry-search".into(),
            jwks_file: PathBuf::from("/run/secrets/search-jwks.json"),
            max_token_ttl_seconds: 60,
            workload_subject_allowlist: Vec::new(),
            legacy_identity_accept_until: None,
        });
        config.vaults = vaults;
        config
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn fr9_reopen_does_not_restore_stale_rows() {
        let fixture = tempdir().unwrap();
        let room = fixture.path().join("room");
        let data = fixture.path().join("data");
        fs::create_dir(&room).unwrap();
        fs::write(room.join("removed.md"), "synthetic expired").unwrap();
        fs::write(room.join("kept.md"), "synthetic remaining").unwrap();
        let mut config = openclast_config(vec![VaultRegistration {
            snapshot_id: None,
            id: "fixture".into(),
            path: room.clone(),
            room: Some("room".into()),
        }]);
        config.indexing.max_stale_serve_seconds = 0;
        build_index(&config, &data).unwrap();
        let resources = [config.resource_key(&config.vaults[0]).unwrap()];
        let filters = SearchFilters::default();
        let runtime = SearchRuntime::new();
        let mut manager = IndexManager::open(config.clone(), &data, runtime.clone()).unwrap();
        manager.reconcile(config.clone()).unwrap();
        let initial = runtime
            .search_authorized_with_generation("expired", 10, &filters, &resources)
            .unwrap();
        assert_eq!(initial.hits.len(), 1);
        fs::remove_file(room.join("removed.md")).unwrap();
        let partial = |manager: &mut IndexManager, config: &Config| {
            let report = crate::room_walk::with_interrupted_listing(room.clone(), || {
                manager.reconcile(config.clone())
            })
            .unwrap();
            assert!(
                report
                    .manifest
                    .files
                    .values()
                    .any(|row| row.path == "removed.md")
            );
        };
        let require_refusal = |runtime: &SearchRuntime| {
            assert!(
                matches!(runtime.search_authorized("expired", 10, &filters, &resources), Err(Error::UnavailableResourceIndexes(indexes)) if indexes == [0])
            );
            assert!(
                matches!(runtime.authorized_freshness(&filters, &resources, &initial.generation), Err(Error::UnavailableResourceIndexes(indexes)) if indexes == [0])
            );
        };
        partial(&mut manager, &config);
        require_refusal(&runtime);
        manager.shutdown().unwrap();

        // A new runtime and a positive cap cannot revive unverified rows.
        config.indexing.max_stale_serve_seconds = 60;
        let reopened = SearchRuntime::new();
        let mut manager = IndexManager::open(config.clone(), &data, reopened.clone()).unwrap();
        require_refusal(&reopened);
        partial(&mut manager, &config);
        require_refusal(&reopened);
        manager.reconcile(config.clone()).unwrap();
        assert!(
            reopened
                .search_authorized("expired", 10, &filters, &resources)
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            reopened
                .search_authorized("remaining", 10, &filters, &resources)
                .unwrap()
                .len(),
            1
        );
        manager.shutdown().unwrap();

        // Nor does reusing the runtime preserve a previous manager's proof.
        let mut manager = IndexManager::open(config.clone(), &data, reopened.clone()).unwrap();
        require_refusal(&reopened);
        manager.reconcile(config).unwrap();
        assert_eq!(
            reopened
                .search_authorized("remaining", 10, &filters, &resources)
                .unwrap()
                .len(),
            1
        );
        manager.shutdown().unwrap();
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn fr11_rejects_noncanonical_roots_at_public_boundaries() {
        let fixture = tempdir().unwrap();
        let base = fs::canonicalize(fixture.path()).unwrap();
        let b = base.join("B");
        let sub = b.join("sub");
        let symlink = base.join("alias");
        fs::create_dir_all(&sub).unwrap();
        std::os::unix::fs::symlink(&sub, &symlink).unwrap();
        fs::write(sub.join("note.md"), "synthetic fixture").unwrap();
        let config = Config {
            vaults: vec![VaultRegistration {
                snapshot_id: None,
                id: "b".into(),
                path: b.clone(),
                room: Some("b".into()),
            }],
            ..openclast_config(Vec::new())
        };
        let mut config = config;
        config.indexing.max_stale_serve_seconds = 0;
        config.auth.openclast.as_mut().unwrap().tenant_id = "tenant_AAAAAAAAAAAAAAAAAAAA".into();
        let settings = base.join("settings/config.toml");
        let data = base.join("data");
        let bad_data = base.join("unused-data");
        crate::save_config(&settings, &config).unwrap();
        let paths = crate::Paths::resolve(Some(settings.clone()), Some(data.clone())).unwrap();
        let saved = fs::read(&settings).unwrap();
        build_index(&config, &data).unwrap();
        let runtime = SearchRuntime::new();
        let mut manager = IndexManager::open(config.clone(), &data, runtime.clone()).unwrap();
        let original_generation = DataRoot::new(&data).active().unwrap().unwrap().id;
        let invalid = |error: Error| {
            assert!(
                error
                    .to_string()
                    .contains("room root path is not canonical"),
                "{error}"
            )
        };
        for bad in [
            base.join("B/../B/sub"),
            base.join("B/./sub"),
            base.join("B//sub"),
            base.join("B/sub/"),
            PathBuf::from("B/sub"),
            symlink,
        ] {
            let mut attempted = config.clone();
            invalid(
                crate::add_vault(&mut attempted, "a".into(), bad.clone(), Some("a".into()))
                    .unwrap_err(),
            );
            invalid(
                crate::ensure_vault_registration(
                    &mut attempted,
                    "a".into(),
                    bad.clone(),
                    Some("a".into()),
                )
                .unwrap_err(),
            );
            invalid(
                crate::register_vault(&paths, "a".into(), bad.clone(), Some("a".into()))
                    .unwrap_err(),
            );
            assert_eq!(attempted, config);
            assert_eq!(fs::read(&settings).unwrap(), saved);
            attempted.vaults.push(VaultRegistration {
                snapshot_id: None,
                id: "a".into(),
                path: bad,
                room: Some("a".into()),
            });
            invalid(crate::save_config(&settings, &attempted).unwrap_err());
            invalid(build_index(&attempted, &bad_data).unwrap_err());
            invalid(crate::probe_openclast_ingestion(&bad_data, &attempted.vaults).unwrap_err());
            let Err(error) = IndexManager::open(attempted.clone(), &data, SearchRuntime::new())
            else {
                panic!("bad room path must refuse open")
            };
            invalid(error);
            invalid(manager.reconcile(attempted.clone()).unwrap_err());
            assert!(!bad_data.exists());
            fs::write(&settings, toml::to_string(&attempted).unwrap()).unwrap();
            invalid(crate::load_config_for_paths(&paths).unwrap_err());
            fs::write(&settings, &saved).unwrap();
        }
        assert_eq!(
            DataRoot::new(&data).active().unwrap().unwrap().id,
            original_generation
        );
        // A filesystem change at an unchanged registration is a failed
        // walk, not a rejected config proposal that leaves its proof current.
        manager.reconcile(config.clone()).unwrap();
        let resources = [config.resource_key(&config.vaults[0]).unwrap()];
        let filters = SearchFilters::default();
        assert_eq!(
            runtime
                .search_authorized("fixture", 10, &filters, &resources)
                .unwrap()
                .len(),
            1
        );
        let moved = base.join("moved");
        fs::rename(&b, &moved).unwrap();
        std::os::unix::fs::symlink(&moved, &b).unwrap();
        let rejected = manager.reconcile(config).unwrap();
        assert!(
            rejected
                .warnings
                .iter()
                .any(|warning| warning.message == "room root path is not canonical")
        );
        assert!(
            matches!(runtime.search_authorized("fixture", 10, &filters, &resources), Err(Error::UnavailableResourceIndexes(indexes)) if indexes == [0])
        );
        manager.shutdown().unwrap();
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn fr9_partial_directory_keeps_only_uncertain_rows_stale() {
        for cap in [0, 60] {
            let fixture = tempdir().unwrap();
            let a = fixture.path().join("a");
            let b = fixture.path().join("b");
            let listed = a.join("listed");
            let linked = a.join("linked.md");
            let hidden = a.join(".linked.md");
            let unread = a.join("unread");
            let data = fixture.path().join("data");
            fs::create_dir_all(&listed).unwrap();
            fs::create_dir_all(&unread).unwrap();
            fs::create_dir(&b).unwrap();
            fs::write(listed.join("moved.md"), "synthetic departed").unwrap();
            fs::write(unread.join("removed.md"), "synthetic uncertain").unwrap();
            fs::write(unread.join("kept.md"), "synthetic remaining").unwrap();
            let registration = |name: &str, path| VaultRegistration {
                snapshot_id: None,
                id: name.into(),
                path,
                room: Some(name.into()),
            };
            let mut config =
                openclast_config(vec![registration("a", a), registration("b", b.clone())]);
            config.indexing.max_stale_serve_seconds = cap;
            build_index(&config, &data).unwrap();
            let runtime = SearchRuntime::new();
            let mut manager = IndexManager::open(config.clone(), &data, runtime.clone()).unwrap();
            manager.reconcile(config.clone()).unwrap();
            let a_key = config.resource_key(&config.vaults[0]).unwrap();
            let a_only = std::slice::from_ref(&a_key);
            let filters = SearchFilters::default();
            let search = |q| runtime.search_authorized_with_generation(q, 10, &filters, a_only);
            assert_eq!(search("departed").unwrap().hits.len(), 1);
            fs::rename(listed.join("moved.md"), b.join("moved.md")).unwrap();
            fs::remove_file(unread.join("removed.md")).unwrap();
            fs::write(&hidden, "synthetic link fixture").unwrap();
            fs::hard_link(&hidden, &linked).unwrap();
            let partial = crate::room_walk::with_interrupted_listing(unread.clone(), || {
                manager.reconcile(config.clone())
            })
            .unwrap();
            assert!(partial.warnings.iter().any(|warning| warning.path == unread
                && warning.message == "directory could not be enumerated safely"));
            assert!(
                !partial
                    .manifest
                    .files
                    .values()
                    .any(|row| row.resource.as_ref() == Some(&a_key)
                        && row.path == "listed/moved.md")
            );
            assert!(
                partial
                    .manifest
                    .files
                    .values()
                    .any(|row| row.path == "unread/removed.md")
            );
            if cap == 0 {
                assert!(
                    matches!(search("uncertain"), Err(Error::UnavailableResourceIndexes(indexes)) if indexes == [0])
                );
            } else {
                assert!(search("departed").unwrap().hits.is_empty());
                let retained = search("uncertain").unwrap();
                assert_eq!(retained.hits.len(), 1);
                assert_eq!(
                    runtime
                        .authorized_freshness(&filters, a_only, &retained.generation)
                        .unwrap()
                        .state,
                    IndexFreshnessState::Stale
                );
            }
            assert!(
                partial.warnings.iter().any(|warning| warning.path == linked
                    && warning.message == "hard-linked file excluded")
            );
            fs::remove_file(linked).unwrap();
            let full = manager.reconcile(config).unwrap();
            assert!(full.warnings.is_empty());
            let deleted = search("uncertain").unwrap();
            assert!(deleted.hits.is_empty());
            assert_eq!(
                runtime
                    .authorized_freshness(&filters, a_only, &deleted.generation)
                    .unwrap()
                    .state,
                IndexFreshnessState::Current
            );
            manager.shutdown().unwrap();
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn fr12_aggregates_disappeared_cold_and_cached_partitions() {
        let fixture = tempdir().unwrap();
        let data = fixture.path().join("data");
        let config = openclast_config(
            ["a", "b", "c"]
                .into_iter()
                .map(|id| {
                    let path = fixture.path().join(id);
                    fs::create_dir(&path).unwrap();
                    fs::write(path.join("note.md"), "synthetic needle").unwrap();
                    VaultRegistration {
                        snapshot_id: None,
                        id: id.into(),
                        path,
                        room: Some(id.into()),
                    }
                })
                .collect(),
        );
        build_index(&config, &data).unwrap();
        let active = DataRoot::new(&data).active().unwrap().unwrap();
        let index = PartitionedSearchIndex::open(&active).unwrap();
        let signed: Vec<_> = config
            .vaults
            .iter()
            .map(|v| config.resource_key(v).unwrap())
            .collect();
        let filters = SearchFilters::default();
        assert_eq!(
            index
                .search("needle", 10, &filters, &signed[..1])
                .unwrap()
                .len(),
            1
        );
        for i in [0, 2] {
            fs::remove_dir_all(partition_index_dir(&active.partitions_dir, &signed[i])).unwrap();
        }
        assert!(
            matches!(index.search("needle", 10, &filters, &signed), Err(Error::UnavailableResourceIndexes(indexes)) if indexes == [0, 2])
        );
        assert!(index.readers.lock().unwrap().contains_key(&signed[1]));
        assert_eq!(
            index
                .search("needle", 10, &filters, &signed[1..2])
                .unwrap()
                .len(),
            1
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn fr11_public_index_build_and_reconcile_use_the_room_walker() {
        let fixture = tempdir().unwrap();
        let data = fixture.path().join("data");
        let path = fixture.path().join("room");
        fs::create_dir(&path).unwrap();
        let config = openclast_config(vec![VaultRegistration {
            snapshot_id: None,
            id: "vault".into(),
            path: path.clone(),
            room: Some("room".into()),
        }]);
        fs::write(path.join("ordinary.md"), "ordinary fixture").unwrap();
        fs::write(path.join("linked.md"), "synthetic private fixture").unwrap();
        fs::hard_link(path.join("linked.md"), path.join("alias.md")).unwrap();
        let stats = build_index(&config, &data).unwrap();
        assert_eq!(stats.documents, 1);
        let resources = [config.resource_key(&config.vaults[0]).unwrap()];
        let runtime = SearchRuntime::new();
        let mut manager = IndexManager::open(config.clone(), &data, runtime.clone()).unwrap();
        let counters = |report: ReconcileReport| {
            report
                .warnings
                .iter()
                .filter(|w| w.message == "hard-linked file excluded")
                .count()
        };
        assert_eq!(counters(manager.reconcile(config.clone()).unwrap()), 2);
        assert_eq!(
            counters(
                manager
                    .reconcile_scoped(config, &ReconcileScope::Full)
                    .unwrap()
            ),
            2
        );
        let filters = SearchFilters::default();
        assert!(
            runtime
                .search_authorized("private", 10, &filters, &resources)
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            runtime
                .search_authorized("ordinary", 10, &filters, &resources)
                .unwrap()
                .len(),
            1
        );
        manager.shutdown().unwrap();
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn fr7_expired_room_does_not_hide_healthy_room_deletions() {
        let fixture = tempdir().unwrap();
        let a = fixture.path().join("a");
        let b = fixture.path().join("b");
        let data = fixture.path().join("data");
        let register = |name: &str, root: &Path| {
            fs::create_dir(root).unwrap();
            fs::write(root.join("index.md"), "synthetic pinecone").unwrap();
            VaultRegistration {
                snapshot_id: None,
                id: name.into(),
                path: root.into(),
                room: Some(name.into()),
            }
        };
        let config = openclast_config(vec![register("a", &a), register("b", &b)]);
        assert_eq!(config.indexing.max_stale_serve_seconds, 3600);
        build_index(&config, &data).unwrap();
        let runtime = SearchRuntime::new();
        let mut manager = IndexManager::open(config.clone(), &data, runtime.clone()).unwrap();
        manager.reconcile(config.clone()).unwrap();
        let a_key = config.resource_key(&config.vaults[0]).unwrap();
        let b_key = config.resource_key(&config.vaults[1]).unwrap();
        fs::rename(&a, fixture.path().join("offline")).unwrap();
        manager.reconcile(config.clone()).unwrap();
        let filters = SearchFilters::default();
        let search = |resources: &[ResourceKey]| {
            runtime.search_authorized("pinecone", 20, &filters, resources)
        };
        assert_eq!(search(&[a_key.clone(), b_key.clone()]).unwrap().len(), 2);
        {
            let mut states = runtime.resource_freshness.lock().unwrap();
            let RoomWalkState::Failed { since, .. } = &mut states.get_mut(&a_key).unwrap().walk
            else {
                panic!("room walk must fail")
            };
            *since = Some(Instant::now() - Duration::from_secs(3600));
        }
        fs::remove_file(b.join("index.md")).unwrap();
        manager.reconcile(config).unwrap();
        assert!(search(std::slice::from_ref(&b_key)).unwrap().is_empty());
        assert!(
            matches!(search(&[b_key, a_key]), Err(Error::UnavailableResourceIndexes(indexes)) if indexes == [1])
        );
        manager.shutdown().unwrap();
    }

    #[cfg_attr(
        not(target_os = "linux"),
        ignore = "secure OpenClast ingestion is Linux-only pending platform guidance"
    )]
    #[test]
    fn authorized_partitions_match_a_physical_baseline() {
        let temporary = tempdir().unwrap();
        let x = temporary.path().join("x");
        let y = temporary.path().join("y");
        let data = temporary.path().join("enterprise-data");
        let baseline_data = temporary.path().join("baseline-data");
        fs::create_dir(&x).unwrap();
        fs::create_dir(&y).unwrap();
        fs::write(x.join("allowed.md"), "# Allowed\nneedle governed evidence").unwrap();
        for index in 0..12 {
            fs::write(
                y.join(format!("forbidden-{index}.md")),
                format!("# Forbidden {index}\nneedle needle secret corpus {index}"),
            )
            .unwrap();
        }

        let x_registration = VaultRegistration {
            snapshot_id: None,
            id: "x".into(),
            path: x.clone(),
            room: Some("room-x".into()),
        };
        let config = openclast_config(vec![
            x_registration.clone(),
            VaultRegistration {
                snapshot_id: None,
                id: "y".into(),
                path: y,
                room: Some("room-y".into()),
            },
        ]);
        build_index(&config, &data).unwrap();
        let runtime = SearchRuntime::new();
        let mut manager = IndexManager::open(config.clone(), &data, runtime.clone()).unwrap();
        manager.reconcile(config.clone()).unwrap();
        let resource_x = config.resource_key(&config.vaults[0]).unwrap();
        let resource_y = config.resource_key(&config.vaults[1]).unwrap();
        let x_hits = runtime
            .search_authorized(
                "needle",
                20,
                &SearchFilters::default(),
                std::slice::from_ref(&resource_x),
            )
            .unwrap();
        assert_eq!(x_hits.len(), 1);
        assert_eq!(x_hits[0].vault_id, "x");

        let baseline = Config {
            vaults: vec![x_registration],
            ..Config::default()
        };
        build_index(&baseline, &baseline_data).unwrap();
        let baseline_runtime = SearchRuntime::new();
        let baseline_manager =
            IndexManager::open(baseline, &baseline_data, baseline_runtime.clone()).unwrap();
        let baseline_hits = baseline_runtime.search(&request("needle")).unwrap();
        assert_eq!(x_hits.len(), baseline_hits.len());
        assert_eq!(x_hits[0].chunk_id, baseline_hits[0].chunk_id);
        assert_eq!(x_hits[0].score, baseline_hits[0].score);

        let combined = runtime
            .search_authorized(
                "needle",
                20,
                &SearchFilters::default(),
                &[resource_x.clone(), resource_y],
            )
            .unwrap();
        assert!(combined.iter().any(|hit| hit.vault_id == "x"));
        assert!(combined.iter().any(|hit| hit.vault_id == "y"));

        let foreign_filter = SearchFilters {
            room: Some("room-y".into()),
            ..SearchFilters::default()
        };
        assert!(
            runtime
                .search_authorized("needle", 20, &foreign_filter, &[resource_x])
                .unwrap()
                .is_empty()
        );
        baseline_manager.shutdown().unwrap();
        manager.shutdown().unwrap();
    }

    #[cfg_attr(
        not(target_os = "linux"),
        ignore = "secure OpenClast ingestion is Linux-only pending platform guidance"
    )]
    #[test]
    fn unauthorized_partition_cannot_change_support_prefixes_or_order() {
        let temporary = tempdir().unwrap();
        let allowed = temporary.path().join("allowed");
        let forbidden = temporary.path().join("forbidden");
        let data = temporary.path().join("enterprise-data");
        let baseline_data = temporary.path().join("baseline-data");
        fs::create_dir(&allowed).unwrap();
        fs::create_dir(&forbidden).unwrap();
        let corpus: serde_json::Value = serde_json::from_str(include_str!(
            "../../../../fixtures/retrieval/lexical-conformance/cases.json"
        ))
        .unwrap();
        for document in corpus["documents"].as_array().unwrap() {
            let scope = document["scope"].as_str().unwrap();
            if !matches!(scope, "allowed" | "forbidden") {
                continue;
            }
            let root = if scope == "allowed" {
                &allowed
            } else {
                &forbidden
            };
            fs::write(
                root.join(document["path"].as_str().unwrap()),
                document["markdown"].as_str().unwrap(),
            )
            .unwrap();
        }

        let allowed_registration = VaultRegistration {
            snapshot_id: None,
            id: "allowed".into(),
            path: allowed.clone(),
            room: Some("room-allowed".into()),
        };
        let config = openclast_config(vec![
            allowed_registration.clone(),
            VaultRegistration {
                snapshot_id: None,
                id: "forbidden".into(),
                path: forbidden,
                room: Some("room-forbidden".into()),
            },
        ]);
        build_index(&config, &data).unwrap();
        let runtime = SearchRuntime::new();
        let mut manager = IndexManager::open(config.clone(), &data, runtime.clone()).unwrap();
        manager.reconcile(config.clone()).unwrap();
        let allowed_resource = config.resource_key(&config.vaults[0]).unwrap();
        let forbidden_resource = config.resource_key(&config.vaults[1]).unwrap();

        let authorized = runtime
            .search_authorized(
                "scopeanchor forbiddenterm",
                20,
                &SearchFilters::default(),
                std::slice::from_ref(&allowed_resource),
            )
            .unwrap();
        assert_eq!(authorized.len(), 1);
        assert_eq!(authorized[0].path, "scope-allowed.md");
        assert!(
            runtime
                .search_authorized(
                    "secretpre",
                    20,
                    &SearchFilters::default(),
                    std::slice::from_ref(&allowed_resource),
                )
                .unwrap()
                .is_empty()
        );

        let combined = runtime
            .search_authorized(
                "secretpre",
                20,
                &SearchFilters::default(),
                &[allowed_resource.clone(), forbidden_resource.clone()],
            )
            .unwrap();
        assert!(!combined.is_empty());
        assert!(combined.iter().all(|hit| hit.vault_id == "forbidden"));
        let reversed = runtime
            .search_authorized(
                "secretpre",
                20,
                &SearchFilters::default(),
                &[forbidden_resource, allowed_resource.clone()],
            )
            .unwrap();
        assert_eq!(
            combined
                .iter()
                .map(|hit| (&hit.chunk_id, hit.score, &hit.path))
                .collect::<Vec<_>>(),
            reversed
                .iter()
                .map(|hit| (&hit.chunk_id, hit.score, &hit.path))
                .collect::<Vec<_>>()
        );

        let baseline = Config {
            vaults: vec![allowed_registration],
            ..Config::default()
        };
        build_index(&baseline, &baseline_data).unwrap();
        let baseline_runtime = SearchRuntime::new();
        let baseline_manager =
            IndexManager::open(baseline, &baseline_data, baseline_runtime.clone()).unwrap();
        let baseline_hits = baseline_runtime
            .search(&request("scopeanchor forbiddenterm"))
            .unwrap();
        assert_eq!(
            authorized
                .iter()
                .map(|hit| (&hit.chunk_id, hit.score, &hit.path))
                .collect::<Vec<_>>(),
            baseline_hits
                .iter()
                .map(|hit| (&hit.chunk_id, hit.score, &hit.path))
                .collect::<Vec<_>>()
        );

        baseline_manager.shutdown().unwrap();
        manager.shutdown().unwrap();
    }

    #[cfg(feature = "internal-d5c-preview")]
    #[cfg_attr(
        not(target_os = "linux"),
        ignore = "secure OpenClast ingestion is Linux-only pending platform guidance"
    )]
    #[test]
    fn d5c_openclast_hydrates_and_reranks_only_authorized_partitions() {
        let temporary = tempdir().unwrap();
        let allowed = temporary.path().join("allowed");
        let forbidden = temporary.path().join("forbidden");
        let data = temporary.path().join("enterprise-data");
        let baseline_data = temporary.path().join("baseline-data");
        fs::create_dir(&allowed).unwrap();
        fs::create_dir(&forbidden).unwrap();
        fs::write(
            allowed.join("preferred.md"),
            "---\npriority: 7\n---\nauthorizedrank",
        )
        .unwrap();
        fs::write(allowed.join("neutral.md"), "authorizedrank authorizedrank").unwrap();
        for index in 0..12 {
            fs::write(
                forbidden.join(format!("forbidden-{index:02}.md")),
                "---\npriority: 7\n---\nauthorizedrank authorizedrank authorizedrank",
            )
            .unwrap();
        }

        let allowed_registration = VaultRegistration {
            snapshot_id: None,
            id: "allowed".into(),
            path: allowed.clone(),
            room: Some("room-allowed".into()),
        };
        let config = openclast_config(vec![
            allowed_registration.clone(),
            VaultRegistration {
                snapshot_id: None,
                id: "forbidden".into(),
                path: forbidden,
                room: Some("room-forbidden".into()),
            },
        ]);
        build_index(&config, &data).unwrap();
        let runtime = SearchRuntime::new();
        let mut manager = IndexManager::open(config.clone(), &data, runtime.clone()).unwrap();
        manager.reconcile(config.clone()).unwrap();
        let allowed_resource = config.resource_key(&config.vaults[0]).unwrap();

        let baseline = Config {
            // Preserve the room identity while removing the forbidden corpus:
            // the room is part of the chunk ID in both storage profiles.
            vaults: vec![allowed_registration],
            ..Config::default()
        };
        build_index(&baseline, &baseline_data).unwrap();
        let baseline_runtime = SearchRuntime::new();
        let baseline_manager =
            IndexManager::open(baseline, &baseline_data, baseline_runtime.clone()).unwrap();

        let mut profile = crate::ranking::D5cRelevanceProfile::preview();
        profile.property_rules = vec![crate::ranking::PropertyRule {
            id: "priority".into(),
            property: "priority".into(),
            predicate: crate::ranking::PropertyPredicate::Exact {
                pointer: Some(String::new()),
                value: crate::ranking::RankingScalar::i64(7),
            },
            effect: crate::ranking::RuleEffect::Boost,
            strength: crate::ranking::RuleStrength::High,
        }];
        let profile = RelevanceProfile::D5cPreviewV1(profile);
        let authorized = runtime
            .search_authorized_with_profile(
                "authorizedrank",
                20,
                &SearchFilters::default(),
                std::slice::from_ref(&allowed_resource),
                &profile,
                2_000_000_000,
            )
            .unwrap();
        let physical_baseline = baseline_runtime
            .search_filtered_with_profile(
                "authorizedrank",
                20,
                &SearchFilters::default(),
                &profile,
                2_000_000_000,
            )
            .unwrap();
        assert_eq!(authorized[0].path, "preferred.md");
        assert_eq!(
            authorized
                .iter()
                .map(|hit| (&hit.chunk_id, &hit.path, hit.score))
                .collect::<Vec<_>>(),
            physical_baseline
                .iter()
                .map(|hit| (&hit.chunk_id, &hit.path, hit.score))
                .collect::<Vec<_>>()
        );
        assert!(authorized.iter().all(|hit| hit.vault_id == "allowed"));

        baseline_manager.shutdown().unwrap();
        manager.shutdown().unwrap();
    }

    #[cfg(feature = "internal-d5c-preview")]
    #[test]
    fn balanced_playground_evaluates_fixture_cases_without_consulting_the_active_index() {
        let corpus: serde_json::Value = serde_json::from_str(include_str!(
            "../../../../fixtures/retrieval/d5c-balanced/corpus.json"
        ))
        .unwrap();
        let evaluations = corpus["evaluations"].as_array().unwrap();
        let runtime = SearchRuntime::new();
        assert!(runtime.generation().is_none());

        for (id, expected_kind) in [
            ("same-tier-recency-native", "strict_balanced"),
            (
                "future-untrusted-clock-native",
                "neutralized_counterfactual",
            ),
            ("candidate-cardinality-over-limit", "fatal"),
        ] {
            let request = &evaluations
                .iter()
                .find(|evaluation| evaluation["id"] == id)
                .unwrap()["request"];
            let configuration: BalancedPlaygroundConfiguration =
                serde_json::from_value(request["configuration"].clone()).unwrap();
            let case: BalancedPlaygroundCase =
                serde_json::from_value(request["case"].clone()).unwrap();
            let envelope = runtime.internal_d5c_evaluate(&configuration, &case);
            assert_eq!(
                serde_json::to_value(&envelope).unwrap()["disposition"]["kind"],
                expected_kind,
                "{id}"
            );
        }
        assert!(runtime.generation().is_none());
    }

    #[cfg_attr(
        not(target_os = "linux"),
        ignore = "secure OpenClast ingestion is Linux-only pending platform guidance"
    )]
    #[test]
    fn openclast_opens_only_request_authorized_partitions() {
        let temporary = tempdir().unwrap();
        let x = temporary.path().join("x");
        let y = temporary.path().join("y");
        let data = temporary.path().join("data");
        fs::create_dir(&x).unwrap();
        fs::create_dir(&y).unwrap();
        fs::write(
            x.join("allowed.md"),
            "authorized needle amberstone birchwood dunefield",
        )
        .unwrap();
        fs::write(x.join("support.md"), "cedarleaf").unwrap();
        fs::write(
            y.join("forbidden.md"),
            "forbidden needle amberstone birchwood cedarleaf dunefield",
        )
        .unwrap();

        let config = openclast_config(vec![
            VaultRegistration {
                snapshot_id: None,
                id: "x".into(),
                path: x,
                room: Some("room-x".into()),
            },
            VaultRegistration {
                snapshot_id: None,
                id: "y".into(),
                path: y,
                room: Some("room-y".into()),
            },
        ]);
        build_index(&config, &data).unwrap();
        let active = DataRoot::new(&data).active().unwrap().unwrap();
        let resource_x = config.resource_key(&config.vaults[0]).unwrap();
        let resource_y = config.resource_key(&config.vaults[1]).unwrap();
        let forbidden_meta =
            partition_index_dir(&active.partitions_dir, &resource_y).join("meta.json");
        fs::write(forbidden_meta, "not valid tantivy metadata").unwrap();

        let partitions = PartitionedSearchIndex::open(&active).unwrap();
        let hits = partitions
            .search(
                "needle",
                20,
                &SearchFilters::default(),
                std::slice::from_ref(&resource_x),
            )
            .unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].vault_id, "x");

        let fallback = partitions
            .search_with_quality(
                "amberstone birchwood cedarleaf dunefield",
                20,
                &SearchFilters::default(),
                std::slice::from_ref(&resource_x),
            )
            .unwrap();
        assert_eq!(fallback.match_quality, LexicalMatchQuality::PartialOnly);
        // The bounded exploratory partial pass surfaces every authorized
        // candidate that carries at least one optional term, not only a
        // preselected subset: allowed.md (three of the four terms) and
        // support.md (one term) both come from the authorized resource_x
        // partition, while forbidden.md's matching terms in resource_y
        // never surface even though it satisfies more of the query.
        let fallback_paths: Vec<_> = fallback.hits.iter().map(|hit| hit.path.as_str()).collect();
        assert_eq!(fallback_paths, vec!["allowed.md", "support.md"]);
        assert!(fallback.hits.iter().all(|hit| hit.vault_id == "x"));

        assert!(
            partitions
                .search("needle", 20, &SearchFilters::default(), &[resource_y],)
                .is_err()
        );
    }

    /// Previously indexed content is withheld when the search either finds
    /// nothing or refuses because the resource has no partition. Any other
    /// error is a test failure, never evidence of withholding.
    fn withheld(result: Result<Vec<SearchHit>>) -> bool {
        match result {
            Ok(hits) => hits.is_empty(),
            Err(Error::ResourceUnavailable | Error::UnavailableResourceIndexes(_)) => true,
            Err(error) => panic!("unexpected search error: {error:?}"),
        }
    }

    #[cfg_attr(
        not(target_os = "linux"),
        ignore = "secure OpenClast ingestion is Linux-only pending platform guidance"
    )]
    #[test]
    fn openclast_refuses_a_signed_resource_without_a_partition() {
        let temporary = tempdir().unwrap();
        let x = temporary.path().join("x");
        let data = temporary.path().join("data");
        fs::create_dir(&x).unwrap();
        fs::write(x.join("allowed.md"), "authorized needle").unwrap();
        let config = openclast_config(vec![VaultRegistration {
            snapshot_id: None,
            id: "x".into(),
            path: x,
            room: Some("room-x".into()),
        }]);
        build_index(&config, &data).unwrap();
        let active = DataRoot::new(&data).active().unwrap().unwrap();
        let resource_x = config.resource_key(&config.vaults[0]).unwrap();
        let unindexed = ResourceKey {
            room_id: "room-not-indexed".into(),
            ..resource_x.clone()
        };
        let partitions = PartitionedSearchIndex::open(&active).unwrap();

        // Signed but unindexed: refuse the whole request, never answer from
        // the remaining resources as if coverage were complete.
        for resources in [
            vec![unindexed.clone()],
            vec![resource_x.clone(), unindexed.clone()],
        ] {
            let Err(error) =
                partitions.search_with_quality("needle", 20, &SearchFilters::default(), &resources)
            else {
                panic!("an unindexed signed resource must be refused");
            };
            assert!(
                matches!(
                    error,
                    Error::ResourceUnavailable | Error::UnavailableResourceIndexes(_)
                ),
                "{error:?}"
            );
            assert!(
                !error.to_string().contains("room-not-indexed"),
                "refusal must not name the resource"
            );
        }

        // A caller filter that narrows away every signed resource is a
        // deliberate narrowing, not missing coverage: still an empty success.
        let narrowed = partitions
            .search_with_quality(
                "needle",
                20,
                &SearchFilters {
                    room: Some("room-elsewhere".into()),
                    ..SearchFilters::default()
                },
                std::slice::from_ref(&resource_x),
            )
            .unwrap();
        assert!(narrowed.hits.is_empty());

        // The available resource alone still answers normally.
        let hits = partitions
            .search("needle", 20, &SearchFilters::default(), &[resource_x])
            .unwrap();
        assert_eq!(hits.len(), 1);
    }

    #[cfg(unix)]
    #[cfg_attr(
        not(target_os = "linux"),
        ignore = "secure OpenClast ingestion is Linux-only pending platform guidance"
    )]
    #[test]
    fn authorized_partition_readers_are_cached_for_the_generation() {
        let temporary = tempdir().unwrap();
        let vault_path = temporary.path().join("vault");
        let data = temporary.path().join("data");
        fs::create_dir(&vault_path).unwrap();
        fs::write(vault_path.join("note.md"), "cachedneedle").unwrap();
        let config = openclast_config(vec![VaultRegistration {
            snapshot_id: None,
            id: "vault-a".into(),
            path: vault_path,
            room: Some("room-a".into()),
        }]);
        build_index(&config, &data).unwrap();
        let active = DataRoot::new(&data).active().unwrap().unwrap();
        let resource = config.resource_key(&config.vaults[0]).unwrap();
        let partitions = PartitionedSearchIndex::open(&active).unwrap();

        let first = partitions
            .search(
                "cachedneedle",
                20,
                &SearchFilters::default(),
                std::slice::from_ref(&resource),
            )
            .unwrap();
        assert_eq!(first.len(), 1);

        // Verify reuse without requiring a removed partition to stay servable.
        let cached = partitions.readers.lock().unwrap()[&resource].clone();
        let second = partitions
            .search(
                "cachedneedle",
                20,
                &SearchFilters::default(),
                std::slice::from_ref(&resource),
            )
            .unwrap();
        assert_eq!(second, first);
        assert!(Arc::ptr_eq(
            &cached,
            &partitions.readers.lock().unwrap()[&resource]
        ));
    }

    #[cfg_attr(
        not(target_os = "linux"),
        ignore = "secure OpenClast ingestion is Linux-only pending platform guidance"
    )]
    #[test]
    fn openclast_reconcile_edits_renames_and_deletes_within_one_partition() {
        let temporary = tempdir().unwrap();
        let vault_path = temporary.path().join("vault");
        let data_root = temporary.path().join("data");
        fs::create_dir(&vault_path).unwrap();
        let note = vault_path.join("note.md");
        fs::write(&note, "# One\noldterm").unwrap();
        let config = openclast_config(vec![VaultRegistration {
            snapshot_id: None,
            id: "fixture".into(),
            path: vault_path.clone(),
            room: Some("room-a".into()),
        }]);
        build_index(&config, &data_root).unwrap();
        let runtime = SearchRuntime::new();
        let mut manager = IndexManager::open(config.clone(), &data_root, runtime.clone()).unwrap();
        let resource = config.resource_key(&config.vaults[0]).unwrap();
        let previous_generation = DataRoot::new(&data_root).active().unwrap().unwrap();
        let previous_snapshot = snapshot_tree(&previous_generation.root);

        fs::write(&note, "# One\nnewterm").unwrap();
        manager.reconcile(config.clone()).unwrap();
        let current_generation = DataRoot::new(&data_root).active().unwrap().unwrap();
        assert_ne!(current_generation.id, previous_generation.id);
        assert_eq!(snapshot_tree(&previous_generation.root), previous_snapshot);
        assert!(
            runtime
                .search_authorized(
                    "oldterm",
                    20,
                    &SearchFilters::default(),
                    std::slice::from_ref(&resource),
                )
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            runtime
                .search_authorized(
                    "newterm",
                    20,
                    &SearchFilters::default(),
                    std::slice::from_ref(&resource),
                )
                .unwrap()
                .len(),
            1
        );

        let moved = vault_path.join("moved.md");
        fs::rename(&note, &moved).unwrap();
        manager.reconcile(config.clone()).unwrap();
        assert_eq!(
            runtime
                .search_authorized(
                    "newterm",
                    20,
                    &SearchFilters::default(),
                    std::slice::from_ref(&resource),
                )
                .unwrap()[0]
                .path,
            "moved.md"
        );
        fs::remove_file(moved).unwrap();
        manager.reconcile(config).unwrap();
        assert!(
            runtime
                .search_authorized("newterm", 20, &SearchFilters::default(), &[resource],)
                .unwrap()
                .is_empty()
        );
        manager.shutdown().unwrap();
    }

    #[cfg_attr(
        not(target_os = "linux"),
        ignore = "secure OpenClast ingestion is Linux-only pending platform guidance"
    )]
    #[test]
    fn openclast_restart_reclassifies_vault_from_persisted_layout() {
        let temporary = tempdir().unwrap();
        let vault_path = temporary.path().join("vault");
        let data_root = temporary.path().join("data");
        fs::create_dir(&vault_path).unwrap();
        fs::write(vault_path.join("note.md"), "reclassifiedterm").unwrap();
        let old_config = openclast_config(vec![VaultRegistration {
            snapshot_id: None,
            id: "fixture".into(),
            path: vault_path.clone(),
            room: Some("room-a".into()),
        }]);
        build_index(&old_config, &data_root).unwrap();
        let old_resource = old_config.resource_key(&old_config.vaults[0]).unwrap();

        let new_config = openclast_config(vec![VaultRegistration {
            snapshot_id: None,
            id: "fixture".into(),
            path: vault_path,
            room: Some("room-b".into()),
        }]);
        let new_resource = new_config.resource_key(&new_config.vaults[0]).unwrap();
        let runtime = SearchRuntime::new();
        let mut manager =
            IndexManager::open(new_config.clone(), &data_root, runtime.clone()).unwrap();
        manager.reconcile(new_config).unwrap();

        assert!(withheld(runtime.search_authorized(
            "reclassifiedterm",
            20,
            &SearchFilters::default(),
            &[old_resource],
        )));
        assert_eq!(
            runtime
                .search_authorized(
                    "reclassifiedterm",
                    20,
                    &SearchFilters::default(),
                    &[new_resource],
                )
                .unwrap()
                .len(),
            1
        );
        manager.shutdown().unwrap();
    }

    #[cfg_attr(
        not(target_os = "linux"),
        ignore = "secure OpenClast ingestion is Linux-only pending platform guidance"
    )]
    #[test]
    fn openclast_restart_reclassifies_tenant_from_persisted_layout() {
        let temporary = tempdir().unwrap();
        let vault_path = temporary.path().join("vault");
        let data_root = temporary.path().join("data");
        fs::create_dir(&vault_path).unwrap();
        fs::write(vault_path.join("note.md"), "tenantterm").unwrap();
        let old_config = openclast_config(vec![VaultRegistration {
            snapshot_id: None,
            id: "fixture".into(),
            path: vault_path,
            room: Some("room-a".into()),
        }]);
        build_index(&old_config, &data_root).unwrap();
        let old_resource = old_config.resource_key(&old_config.vaults[0]).unwrap();

        let mut new_config = old_config.clone();
        new_config.auth.openclast.as_mut().unwrap().tenant_id = "tenant-b".into();
        let new_resource = new_config.resource_key(&new_config.vaults[0]).unwrap();
        let runtime = SearchRuntime::new();
        let mut manager =
            IndexManager::open(new_config.clone(), &data_root, runtime.clone()).unwrap();
        manager.reconcile(new_config).unwrap();

        assert!(withheld(runtime.search_authorized(
            "tenantterm",
            20,
            &SearchFilters::default(),
            &[old_resource],
        )));
        assert_eq!(
            runtime
                .search_authorized("tenantterm", 20, &SearchFilters::default(), &[new_resource],)
                .unwrap()
                .len(),
            1
        );
        manager.shutdown().unwrap();
    }

    #[cfg_attr(
        not(target_os = "linux"),
        ignore = "secure OpenClast ingestion is Linux-only pending platform guidance"
    )]
    #[test]
    fn openclast_reconcile_recovers_a_partial_cross_resource_commit() {
        let temporary = tempdir().unwrap();
        let vault_path = temporary.path().join("vault");
        let data_root = temporary.path().join("data");
        fs::create_dir(&vault_path).unwrap();
        fs::write(vault_path.join("retry.md"), "retryterm").unwrap();
        fs::write(vault_path.join("duplicate.md"), "duplicateterm").unwrap();
        fs::write(vault_path.join("deleted.md"), "deletedterm").unwrap();
        let old_config = openclast_config(vec![VaultRegistration {
            snapshot_id: None,
            id: "fixture".into(),
            path: vault_path,
            room: Some("room-a".into()),
        }]);
        build_index(&old_config, &data_root).unwrap();
        let old_resource = old_config.resource_key(&old_config.vaults[0]).unwrap();

        let mut new_config = old_config.clone();
        new_config.auth.openclast.as_mut().unwrap().tenant_id = "tenant-b".into();
        let new_resource = new_config.resource_key(&new_config.vaults[0]).unwrap();
        let active = DataRoot::new(&data_root).active().unwrap().unwrap();
        let new_index_dir = partition_index_dir(&active.partitions_dir, &new_resource);
        let (new_index, fields) = ensure_partition_index(&new_index_dir).unwrap();
        let mut writer: IndexWriter<tantivy::TantivyDocument> =
            new_index.writer(WRITER_MEMORY_BYTES).unwrap();
        let (outcomes, enumeration) = ingest_vault_files(&new_config.vaults[0]);
        assert!(enumeration.warnings.is_empty());
        for outcome in outcomes {
            if matches!(outcome.path.as_str(), "duplicate.md" | "deleted.md") {
                for chunk in &outcome.chunks {
                    writer
                        .add_document(chunk_document(&fields, chunk, &outcome.retrieval).unwrap())
                        .unwrap();
                }
            }
        }
        writer.commit().unwrap();
        writer.wait_merging_threads().unwrap();
        GenerationLayout::openclast([new_resource.clone()])
            .unwrap()
            .save(&active.layout_path)
            .unwrap();
        fs::remove_file(new_config.vaults[0].path.join("deleted.md")).unwrap();

        // This models a target partition commit followed by a crash before the
        // old manifest could be replaced: one retained file was never inserted,
        // one was already inserted, and one disappeared before retry.
        let runtime = SearchRuntime::new();
        let mut manager =
            IndexManager::open(new_config.clone(), &data_root, runtime.clone()).unwrap();
        assert!(withheld(runtime.search_authorized(
            "retryterm",
            20,
            &SearchFilters::default(),
            std::slice::from_ref(&new_resource),
        )));
        assert!(
            matches!(runtime.search_authorized("duplicateterm", 20, &SearchFilters::default(), std::slice::from_ref(&new_resource)), Err(Error::UnavailableResourceIndexes(indexes)) if indexes == [0])
        );
        assert!(
            matches!(runtime.search_authorized("deletedterm", 20, &SearchFilters::default(), std::slice::from_ref(&new_resource)), Err(Error::UnavailableResourceIndexes(indexes)) if indexes == [0])
        );
        manager.reconcile(new_config.clone()).unwrap();
        for query in ["retryterm", "duplicateterm", "deletedterm"] {
            assert!(withheld(runtime.search_authorized(
                query,
                20,
                &SearchFilters::default(),
                std::slice::from_ref(&old_resource),
            )));
        }
        for query in ["retryterm", "duplicateterm"] {
            assert_eq!(
                runtime
                    .search_authorized(
                        query,
                        20,
                        &SearchFilters::default(),
                        std::slice::from_ref(&new_resource),
                    )
                    .unwrap()
                    .len(),
                1
            );
        }
        assert!(withheld(runtime.search_authorized(
            "deletedterm",
            20,
            &SearchFilters::default(),
            std::slice::from_ref(&new_resource),
        )));
        assert_eq!(manager.manifest().files.len(), 2);
        assert!(
            manager
                .manifest()
                .files
                .values()
                .all(|file| file.resource.as_ref() == Some(&new_resource))
        );
        manager.shutdown().unwrap();

        let runtime = SearchRuntime::new();
        let mut manager =
            IndexManager::open(new_config.clone(), &data_root, runtime.clone()).unwrap();
        manager.reconcile(new_config).unwrap();
        for query in ["retryterm", "duplicateterm"] {
            assert_eq!(
                runtime
                    .search_authorized(
                        query,
                        20,
                        &SearchFilters::default(),
                        std::slice::from_ref(&new_resource),
                    )
                    .unwrap()
                    .len(),
                1
            );
        }
        assert!(withheld(runtime.search_authorized(
            "deletedterm",
            20,
            &SearchFilters::default(),
            &[new_resource],
        )));
        manager.shutdown().unwrap();
    }

    #[cfg_attr(
        not(target_os = "linux"),
        ignore = "secure OpenClast ingestion is Linux-only pending platform guidance"
    )]
    #[test]
    fn openclast_reconcile_removes_orphans_from_an_unpublished_new_resource() {
        let temporary = tempdir().unwrap();
        let existing_path = temporary.path().join("existing");
        let vault_path = temporary.path().join("vault");
        let data_root = temporary.path().join("data");
        fs::create_dir(&existing_path).unwrap();
        fs::create_dir(&vault_path).unwrap();
        fs::write(existing_path.join("existing.md"), "existingterm").unwrap();
        fs::write(vault_path.join("deleted.md"), "unpublishedterm").unwrap();
        let existing = VaultRegistration {
            snapshot_id: None,
            id: "existing".into(),
            path: existing_path,
            room: Some("room-existing".into()),
        };
        let initial_config = openclast_config(vec![existing.clone()]);
        build_index(&initial_config, &data_root).unwrap();
        let existing_resource = initial_config
            .resource_key(&initial_config.vaults[0])
            .unwrap();

        let new_config = openclast_config(vec![
            existing,
            VaultRegistration {
                snapshot_id: None,
                id: "fixture".into(),
                path: vault_path.clone(),
                room: Some("room-a".into()),
            },
        ]);
        let resource = new_config.resource_key(&new_config.vaults[1]).unwrap();
        let active = DataRoot::new(&data_root).active().unwrap().unwrap();
        let index_dir = partition_index_dir(&active.partitions_dir, &resource);
        let (index, fields) = ensure_partition_index(&index_dir).unwrap();
        let mut writer = index.writer(WRITER_MEMORY_BYTES).unwrap();
        let (outcomes, enumeration) = ingest_vault_files(&new_config.vaults[1]);
        assert!(enumeration.warnings.is_empty());
        for outcome in outcomes {
            for chunk in &outcome.chunks {
                writer
                    .add_document(chunk_document(&fields, chunk, &outcome.retrieval).unwrap())
                    .unwrap();
            }
        }
        writer.commit().unwrap();
        writer.wait_merging_threads().unwrap();
        GenerationLayout::openclast([existing_resource, resource.clone()])
            .unwrap()
            .save(&active.layout_path)
            .unwrap();
        fs::remove_file(vault_path.join("deleted.md")).unwrap();

        // The target partition and layout were committed, but the old empty
        // manifest survived. Reconciliation must not trust the orphan index.
        let runtime = SearchRuntime::new();
        let mut manager =
            IndexManager::open(new_config.clone(), &data_root, runtime.clone()).unwrap();
        assert!(
            matches!(runtime.search_authorized("unpublishedterm", 20, &SearchFilters::default(), std::slice::from_ref(&resource)), Err(Error::UnavailableResourceIndexes(indexes)) if indexes == [0])
        );
        manager.reconcile(new_config.clone()).unwrap();
        assert!(
            runtime
                .search_authorized(
                    "unpublishedterm",
                    20,
                    &SearchFilters::default(),
                    std::slice::from_ref(&resource),
                )
                .unwrap()
                .is_empty()
        );
        assert!(
            manager
                .manifest()
                .files
                .values()
                .all(|file| file.vault_id != "fixture")
        );
        manager.shutdown().unwrap();

        let runtime = SearchRuntime::new();
        let mut manager =
            IndexManager::open(new_config.clone(), &data_root, runtime.clone()).unwrap();
        manager.reconcile(new_config).unwrap();
        assert!(
            runtime
                .search_authorized(
                    "unpublishedterm",
                    20,
                    &SearchFilters::default(),
                    &[resource],
                )
                .unwrap()
                .is_empty()
        );
        manager.shutdown().unwrap();
    }

    #[cfg_attr(
        not(target_os = "linux"),
        ignore = "secure OpenClast ingestion is Linux-only pending platform guidance"
    )]
    #[test]
    fn openclast_reclassification_withholds_unavailable_content_until_reindexed() {
        let temporary = tempdir().unwrap();
        let vault_path = temporary.path().join("vault");
        let offline_path = temporary.path().join("vault-offline");
        let data_root = temporary.path().join("data");
        fs::create_dir(&vault_path).unwrap();
        fs::write(vault_path.join("note.md"), "withheldterm").unwrap();
        let old_config = openclast_config(vec![VaultRegistration {
            snapshot_id: None,
            id: "fixture".into(),
            path: vault_path.clone(),
            room: Some("room-a".into()),
        }]);
        build_index(&old_config, &data_root).unwrap();
        let old_resource = old_config.resource_key(&old_config.vaults[0]).unwrap();

        let new_config = openclast_config(vec![VaultRegistration {
            snapshot_id: None,
            id: "fixture".into(),
            path: vault_path.clone(),
            room: Some("room-b".into()),
        }]);
        let new_resource = new_config.resource_key(&new_config.vaults[0]).unwrap();
        let active = DataRoot::new(&data_root).active().unwrap().unwrap();
        let index_dir = partition_index_dir(&active.partitions_dir, &new_resource);
        let (index, fields) = ensure_partition_index(&index_dir).unwrap();
        let mut writer = index.writer(WRITER_MEMORY_BYTES).unwrap();
        let (outcomes, enumeration) = ingest_vault_files(&new_config.vaults[0]);
        assert!(enumeration.warnings.is_empty());
        for outcome in outcomes {
            for chunk in &outcome.chunks {
                writer
                    .add_document(chunk_document(&fields, chunk, &outcome.retrieval).unwrap())
                    .unwrap();
            }
        }
        writer.commit().unwrap();
        writer.wait_merging_threads().unwrap();
        GenerationLayout::openclast([new_resource.clone()])
            .unwrap()
            .save(&active.layout_path)
            .unwrap();
        fs::rename(&vault_path, &offline_path).unwrap();

        let runtime = SearchRuntime::new();
        let mut manager =
            IndexManager::open(new_config.clone(), &data_root, runtime.clone()).unwrap();
        assert!(
            matches!(runtime.search_authorized("withheldterm", 20, &SearchFilters::default(), std::slice::from_ref(&new_resource)), Err(Error::UnavailableResourceIndexes(indexes)) if indexes == [0])
        );
        let report = manager.reconcile(new_config.clone()).unwrap();

        assert_eq!(report.unavailable_vaults, ["fixture"]);
        assert!(withheld(runtime.search_authorized(
            "withheldterm",
            20,
            &SearchFilters::default(),
            &[old_resource],
        )));
        assert!(withheld(runtime.search_authorized(
            "withheldterm",
            20,
            &SearchFilters::default(),
            std::slice::from_ref(&new_resource),
        )));

        fs::rename(&offline_path, &vault_path).unwrap();
        manager.reconcile(new_config).unwrap();
        assert_eq!(
            runtime
                .search_authorized(
                    "withheldterm",
                    20,
                    &SearchFilters::default(),
                    &[new_resource],
                )
                .unwrap()
                .len(),
            1
        );
        manager.shutdown().unwrap();
    }

    #[cfg_attr(
        not(target_os = "linux"),
        ignore = "secure OpenClast ingestion is Linux-only pending platform guidance"
    )]
    #[test]
    fn openclast_path_change_withholds_unavailable_previous_content() {
        let temporary = tempdir().unwrap();
        let old_path = temporary.path().join("old-vault");
        let new_path = temporary.path().join("new-vault");
        let data_root = temporary.path().join("data");
        fs::create_dir(&old_path).unwrap();
        fs::write(old_path.join("note.md"), "oldpathterm").unwrap();
        let old_config = openclast_config(vec![VaultRegistration {
            snapshot_id: None,
            id: "fixture".into(),
            path: old_path,
            room: Some("room-a".into()),
        }]);
        build_index(&old_config, &data_root).unwrap();
        let resource = old_config.resource_key(&old_config.vaults[0]).unwrap();

        let new_config = openclast_config(vec![VaultRegistration {
            snapshot_id: None,
            id: "fixture".into(),
            path: new_path.clone(),
            room: Some("room-a".into()),
        }]);
        let runtime = SearchRuntime::new();
        let mut manager =
            IndexManager::open(new_config.clone(), &data_root, runtime.clone()).unwrap();
        let report = manager.reconcile(new_config.clone()).unwrap();
        assert_eq!(report.unavailable_vaults, ["fixture"]);
        assert!(withheld(runtime.search_authorized(
            "oldpathterm",
            20,
            &SearchFilters::default(),
            std::slice::from_ref(&resource),
        )));
        manager.shutdown().unwrap();

        let runtime = SearchRuntime::new();
        let mut manager =
            IndexManager::open(new_config.clone(), &data_root, runtime.clone()).unwrap();
        assert!(withheld(runtime.search_authorized(
            "oldpathterm",
            20,
            &SearchFilters::default(),
            std::slice::from_ref(&resource),
        )));
        fs::create_dir(&new_path).unwrap();
        fs::write(new_path.join("note.md"), "newpathterm").unwrap();
        manager.reconcile(new_config).unwrap();
        assert!(withheld(runtime.search_authorized(
            "oldpathterm",
            20,
            &SearchFilters::default(),
            std::slice::from_ref(&resource),
        )));
        assert_eq!(
            runtime
                .search_authorized("newpathterm", 20, &SearchFilters::default(), &[resource],)
                .unwrap()
                .len(),
            1
        );
        manager.shutdown().unwrap();
    }

    #[cfg_attr(
        not(target_os = "linux"),
        ignore = "secure OpenClast ingestion is Linux-only pending platform guidance"
    )]
    #[test]
    fn openclast_unregisters_a_vault_after_its_content_was_withheld() {
        let temporary = tempdir().unwrap();
        let vault_path = temporary.path().join("vault");
        let offline_path = temporary.path().join("vault-offline");
        let data_root = temporary.path().join("data");
        fs::create_dir(&vault_path).unwrap();
        fs::write(vault_path.join("note.md"), "removedwithheldterm").unwrap();
        let old_config = openclast_config(vec![VaultRegistration {
            snapshot_id: None,
            id: "fixture".into(),
            path: vault_path.clone(),
            room: Some("room-a".into()),
        }]);
        build_index(&old_config, &data_root).unwrap();
        let old_resource = old_config.resource_key(&old_config.vaults[0]).unwrap();
        fs::rename(&vault_path, &offline_path).unwrap();

        let reclassified_config = openclast_config(vec![VaultRegistration {
            snapshot_id: None,
            id: "fixture".into(),
            path: vault_path,
            room: Some("room-b".into()),
        }]);
        let new_resource = reclassified_config
            .resource_key(&reclassified_config.vaults[0])
            .unwrap();
        let runtime = SearchRuntime::new();
        let mut manager =
            IndexManager::open(reclassified_config.clone(), &data_root, runtime.clone()).unwrap();
        manager.reconcile(reclassified_config).unwrap();

        let removed_config = openclast_config(Vec::new());
        manager.reconcile(removed_config.clone()).unwrap();
        assert!(manager.manifest().files.is_empty());
        for resource in [&old_resource, &new_resource] {
            assert!(withheld(runtime.search_authorized(
                "removedwithheldterm",
                20,
                &SearchFilters::default(),
                std::slice::from_ref(resource),
            )));
        }
        manager.shutdown().unwrap();

        let runtime = SearchRuntime::new();
        let mut manager =
            IndexManager::open(removed_config.clone(), &data_root, runtime.clone()).unwrap();
        manager.reconcile(removed_config).unwrap();
        assert!(manager.manifest().files.is_empty());
        for resource in [old_resource, new_resource] {
            assert!(withheld(runtime.search_authorized(
                "removedwithheldterm",
                20,
                &SearchFilters::default(),
                &[resource],
            )));
        }
        manager.shutdown().unwrap();
    }

    #[cfg_attr(
        not(target_os = "linux"),
        ignore = "secure OpenClast ingestion is Linux-only pending platform guidance"
    )]
    #[test]
    fn openclast_restart_removes_unregistered_vault_from_persisted_layout() {
        let temporary = tempdir().unwrap();
        let x = temporary.path().join("x");
        let y = temporary.path().join("y");
        let data_root = temporary.path().join("data");
        fs::create_dir(&x).unwrap();
        fs::create_dir(&y).unwrap();
        fs::write(x.join("allowed.md"), "allowedterm").unwrap();
        fs::write(y.join("removed.md"), "removedterm").unwrap();
        let old_config = openclast_config(vec![
            VaultRegistration {
                snapshot_id: None,
                id: "x".into(),
                path: x.clone(),
                room: Some("room-x".into()),
            },
            VaultRegistration {
                snapshot_id: None,
                id: "y".into(),
                path: y,
                room: Some("room-y".into()),
            },
        ]);
        build_index(&old_config, &data_root).unwrap();
        let removed_resource = old_config.resource_key(&old_config.vaults[1]).unwrap();

        let new_config = openclast_config(vec![VaultRegistration {
            snapshot_id: None,
            id: "x".into(),
            path: x,
            room: Some("room-x".into()),
        }]);
        let runtime = SearchRuntime::new();
        let mut manager =
            IndexManager::open(new_config.clone(), &data_root, runtime.clone()).unwrap();
        manager.reconcile(new_config).unwrap();

        assert!(withheld(runtime.search_authorized(
            "removedterm",
            20,
            &SearchFilters::default(),
            &[removed_resource],
        )));
        manager.shutdown().unwrap();
    }

    #[cfg_attr(
        not(target_os = "linux"),
        ignore = "secure OpenClast ingestion is Linux-only pending platform guidance"
    )]
    #[test]
    fn openclast_reconcile_initializes_an_empty_resource_partition() {
        let temporary = tempdir().unwrap();
        let populated = temporary.path().join("populated");
        let empty = temporary.path().join("empty");
        let data_root = temporary.path().join("data");
        fs::create_dir(&populated).unwrap();
        fs::create_dir(&empty).unwrap();
        fs::write(populated.join("note.md"), "existingterm").unwrap();
        let initial = openclast_config(vec![VaultRegistration {
            snapshot_id: None,
            id: "populated".into(),
            path: populated.clone(),
            room: Some("room-a".into()),
        }]);
        build_index(&initial, &data_root).unwrap();
        let runtime = SearchRuntime::new();
        let mut manager = IndexManager::open(initial.clone(), &data_root, runtime.clone()).unwrap();

        let next = openclast_config(vec![
            initial.vaults[0].clone(),
            VaultRegistration {
                snapshot_id: None,
                id: "empty".into(),
                path: empty,
                room: Some("room-empty".into()),
            },
        ]);
        let empty_resource = next.resource_key(&next.vaults[1]).unwrap();
        manager.reconcile(next).unwrap();

        assert!(
            runtime
                .search_authorized("anything", 20, &SearchFilters::default(), &[empty_resource],)
                .unwrap()
                .is_empty()
        );
        manager.shutdown().unwrap();
    }

    #[test]
    fn manager_rejects_startup_configuration_changes() {
        let temporary = tempdir().unwrap();
        let vault_path = temporary.path().join("vault");
        let data_root = temporary.path().join("data");
        fs::create_dir(&vault_path).unwrap();
        fs::write(vault_path.join("note.md"), "startupterm").unwrap();
        let config = Config {
            vaults: vec![VaultRegistration {
                snapshot_id: None,
                id: "fixture".into(),
                path: vault_path,
                room: None,
            }],
            ..Config::default()
        };
        build_index(&config, &data_root).unwrap();
        let runtime = SearchRuntime::new();
        let mut manager = IndexManager::open(config.clone(), &data_root, runtime).unwrap();
        let mut changed = config;
        changed.server.bind = "127.0.0.1:40000".into();

        let error = manager.reconcile(changed).unwrap_err();
        assert!(error.to_string().contains("restart the daemon"));
        manager.shutdown().unwrap();
    }

    #[test]
    fn unavailable_vault_keeps_last_committed_results() {
        let temporary = tempdir().unwrap();
        let vault_path = temporary.path().join("vault");
        let data_root = temporary.path().join("data");
        fs::create_dir(&vault_path).unwrap();
        fs::write(vault_path.join("note.md"), "retainedterm").unwrap();
        let config = Config {
            vaults: vec![VaultRegistration {
                snapshot_id: None,
                id: "fixture".into(),
                path: vault_path.clone(),
                room: None,
            }],
            ..Config::default()
        };
        build_index(&config, &data_root).unwrap();
        let runtime = SearchRuntime::new();
        let mut manager = IndexManager::open(config.clone(), &data_root, runtime.clone()).unwrap();
        fs::remove_dir_all(&vault_path).unwrap();

        let report = manager.reconcile(config).unwrap();
        assert_eq!(report.unavailable_vaults, ["fixture"]);
        assert_eq!(runtime.search(&request("retainedterm")).unwrap().len(), 1);
        manager.shutdown().unwrap();
    }

    // ------------------------------------------------- split-identity wave

    /// Rewrites one format's recorded identity in the active generation's
    /// manifest and returns how many rows were rewritten.
    fn stamp_identity(
        data_root: &Path,
        format: crate::format::SourceFormat,
        identity: &str,
    ) -> usize {
        let active = DataRoot::new(data_root).active().unwrap().unwrap();
        let mut document: serde_json::Value =
            serde_json::from_slice(&fs::read(&active.manifest_path).unwrap()).unwrap();
        let mut stamped = 0;
        for (_, file) in document["files"].as_object_mut().unwrap() {
            if file["format"] == serde_json::json!(format.as_str()) {
                file["format_identity"] = serde_json::json!(identity);
                stamped += 1;
            }
        }
        fs::write(
            &active.manifest_path,
            serde_json::to_vec_pretty(&document).unwrap(),
        )
        .unwrap();
        stamped
    }

    /// Stamps an identity **no build compiles at all** — not a bumped version,
    /// not another format's, not another tier's.
    ///
    /// The doc comment this replaces claimed `"f" * 64` was "exactly what a
    /// build with a bumped `extractor_version_for` would look like". It was
    /// not: a real bump moves the digest through
    /// `policy::identity_for_material`, and a literal proves nothing about
    /// whether the version reaches that derivation. The literal is still the
    /// right fixture for the *pipeline* tests below — it cannot drift with the
    /// predicate — but the claim belongs to
    /// `stamp_next_extractor_version`, which derives its value.
    fn forge_foreign_identity(data_root: &Path, format: crate::format::SourceFormat) -> usize {
        stamp_identity(data_root, format, &"f".repeat(64))
    }

    /// Stamps the identity a build whose `extractor_version_for(format)` is one
    /// higher would produce, deriving it from the shipped material.
    ///
    /// This is the fact the split-identity wave rests on and the one nothing
    /// asserted end to end: that a *version* difference — same format, same
    /// tier, same schema, same bytes on disk — is what drives eviction and
    /// re-ingest through the real open path. A build compiles exactly one
    /// version, so the second build is expressed as the digest it would stamp
    /// rather than as a second binary; the predicate is string equality, so the
    /// direction of the difference is immaterial.
    fn stamp_next_extractor_version(
        data_root: &Path,
        format: crate::format::SourceFormat,
    ) -> usize {
        let bumped = crate::policy::identity_at_extractor_version(
            format,
            crate::policy::extractor_version_for(format) + 1,
        );
        assert_ne!(
            bumped,
            crate::policy::format_identity_fingerprint(format),
            "a bumped extractor version must not claim this build's identity"
        );
        stamp_identity(data_root, format, &bumped)
    }

    fn two_format_vault() -> (tempfile::TempDir, PathBuf, PathBuf, Config) {
        let temporary = tempdir().unwrap();
        let vault_path = temporary.path().join("vault");
        let data_root = temporary.path().join("data");
        fs::create_dir(&vault_path).unwrap();
        fs::write(vault_path.join("note.md"), "# Note\nmarkdownterm").unwrap();
        fs::write(vault_path.join("log.txt"), "plainterm").unwrap();
        let config = Config {
            vaults: vec![VaultRegistration {
                snapshot_id: None,
                id: "fixture".into(),
                path: vault_path.clone(),
                room: None,
            }],
            indexing: crate::model::IndexingConfig {
                basis: IndexFreshnessBasis::MetadataAudit,
                audit_sources_per_pass: 0,
                audit_bytes_per_pass: 0,
                racy_window_millis: 0,
                ..Default::default()
            },
            ..Config::default()
        };
        (temporary, vault_path, data_root, config)
    }

    /// A real extractor version bump, end to end, on the real open path.
    ///
    /// The generation is written by this build, then one format's rows are
    /// re-stamped with the identity the *next* version of that format's
    /// extractor would produce — derived from `policy::identity_for_material`,
    /// so the only difference between the recorded rows and what this build
    /// would write is the number in `extractor_version_for`. Opening must
    /// evict exactly that format, leave every other format searchable
    /// throughout, and re-ingest exactly the bumped format's sources.
    ///
    /// This is the assertion the wave was missing. Every other test on this
    /// path stamps `"f" * 64`, which proves the pipeline reacts to a foreign
    /// identity but never that a version bump produces one.
    ///
    /// Mutation check: delete the `extractor=` component from
    /// `policy::identity_for_material` and `stamp_next_extractor_version`'s own
    /// `assert_ne!` fires before the fixture is even written.
    #[test]
    fn a_bumped_extractor_version_evicts_and_reingests_only_that_format() {
        let (_temporary, _vault_path, data_root, config) = two_format_vault();
        build_index(&config, &data_root).unwrap();
        let before = DataRoot::new(&data_root).active().unwrap().unwrap().id;
        assert_eq!(
            stamp_next_extractor_version(&data_root, SourceFormat::Text),
            1
        );

        let runtime = SearchRuntime::new();
        let mut manager = IndexManager::open(config.clone(), &data_root, runtime.clone()).unwrap();

        // Narrow, and immediate: the bumped format is unsearchable from the
        // first query after open, and the untouched format never blinks.
        assert_eq!(
            manager.open_evictions().by_format,
            BTreeMap::from([(SourceFormat::Text, 1)])
        );
        assert!(runtime.search(&request("plainterm")).unwrap().is_empty());
        assert_eq!(runtime.search(&request("markdownterm")).unwrap().len(), 1);
        assert_ne!(
            DataRoot::new(&data_root).active().unwrap().unwrap().id,
            before,
            "eviction publishes a new generation"
        );

        // Only the bumped format's bytes are read again.
        let report = manager.reconcile(config).unwrap();
        assert_eq!(
            report.source_files_read, 1,
            "a bump may re-read only the bumped format's sources"
        );
        assert_eq!(report.changed_sources, 1);
        assert_eq!(report.documents, 2);
        assert_eq!(runtime.search(&request("plainterm")).unwrap().len(), 1);
        assert_eq!(runtime.search(&request("markdownterm")).unwrap().len(), 1);

        // And every rebuilt row now carries this build's identity, which is
        // what makes the next open free.
        for file in report.manifest.files.values() {
            assert!(file.identity_matches());
            assert_eq!(
                file.format_identity.as_deref(),
                Some(crate::policy::format_identity_fingerprint(file.format))
            );
        }
        manager.shutdown().unwrap();
    }

    /// The property this whole wave exists to preserve: an evicted row's
    /// postings are gone from the *first* search after open, before any
    /// reconciliation pass has run. Eviction is an open-time transformation,
    /// not a read-time filter.
    #[test]
    fn an_evicted_format_is_unsearchable_from_the_first_query_after_open() {
        let (_temporary, _vault_path, data_root, config) = two_format_vault();
        build_index(&config, &data_root).unwrap();
        assert_eq!(forge_foreign_identity(&data_root, SourceFormat::Text), 1);

        let runtime = SearchRuntime::new();
        let manager = IndexManager::open(config, &data_root, runtime.clone()).unwrap();

        // Narrow: only the Text row was evicted.
        let evictions = manager.open_evictions();
        assert_eq!(evictions.total(), 1);
        assert_eq!(
            evictions.by_format,
            BTreeMap::from([(SourceFormat::Text, 1)])
        );
        // Removed, never orphaned: gone from the manifest *and* from the index.
        assert!(runtime.search(&request("plainterm")).unwrap().is_empty());
        assert_eq!(runtime.search(&request("markdownterm")).unwrap().len(), 1);
        assert_eq!(manager.manifest().files.len(), 1);
        assert_eq!(manager.manifest().document_count(), 1);
        manager.manifest().validate().unwrap();
        manager.shutdown().unwrap();
    }

    /// The eviction is durable and idempotent: reopening finds nothing left to
    /// evict, and the published manifest is the evicted one.
    #[test]
    fn eviction_is_published_atomically_and_is_idempotent() {
        let (_temporary, _vault_path, data_root, config) = two_format_vault();
        build_index(&config, &data_root).unwrap();
        let before = DataRoot::new(&data_root).active().unwrap().unwrap().id;
        forge_foreign_identity(&data_root, SourceFormat::Text);

        let first = IndexManager::open(config.clone(), &data_root, SearchRuntime::new()).unwrap();
        assert_eq!(first.open_evictions().total(), 1);
        let after = DataRoot::new(&data_root).active().unwrap().unwrap().id;
        assert_ne!(after, before, "eviction publishes a new generation");
        first.shutdown().unwrap();

        let runtime = SearchRuntime::new();
        let second = IndexManager::open(config, &data_root, runtime.clone()).unwrap();
        assert!(second.open_evictions().is_empty());
        assert!(runtime.search(&request("plainterm")).unwrap().is_empty());
        assert_eq!(runtime.search(&request("markdownterm")).unwrap().len(), 1);
        second.shutdown().unwrap();
    }

    /// After eviction only the evicted format's sources are read again; every
    /// other format's row is reused without touching its bytes. This is the
    /// saving the narrowing buys.
    #[test]
    fn reingest_after_eviction_reads_only_the_evicted_formats_sources() {
        let (_temporary, _vault_path, data_root, config) = two_format_vault();
        build_index(&config, &data_root).unwrap();
        forge_foreign_identity(&data_root, SourceFormat::Text);

        let runtime = SearchRuntime::new();
        let mut manager = IndexManager::open(config.clone(), &data_root, runtime.clone()).unwrap();
        let report = manager.reconcile(config).unwrap();

        assert_eq!(
            report.source_files_read, 1,
            "only the evicted format's source may be re-read"
        );
        assert_eq!(report.changed_sources, 1);
        assert_eq!(report.documents, 2);
        assert_eq!(runtime.search(&request("plainterm")).unwrap().len(), 1);
        assert_eq!(runtime.search(&request("markdownterm")).unwrap().len(), 1);
        // And the rebuilt row now carries the running identity.
        for file in report.manifest.files.values() {
            assert!(file.identity_matches());
        }
        manager.shutdown().unwrap();
    }

    /// Eviction before retention. An offline vault must not resurrect a stale
    /// row, and an identity change must not delete a healthy one: the evicted
    /// format goes, every other format is retained, and nothing is re-read.
    #[test]
    fn an_offline_vault_retains_surviving_formats_while_the_evicted_one_stays_gone() {
        let (_temporary, vault_path, data_root, config) = two_format_vault();
        build_index(&config, &data_root).unwrap();
        forge_foreign_identity(&data_root, SourceFormat::Text);
        fs::remove_dir_all(&vault_path).unwrap();

        let runtime = SearchRuntime::new();
        let mut manager = IndexManager::open(config.clone(), &data_root, runtime.clone()).unwrap();
        assert_eq!(manager.open_evictions().total(), 1);
        assert!(runtime.search(&request("plainterm")).unwrap().is_empty());

        let report = manager.reconcile(config).unwrap();
        assert_eq!(report.unavailable_vaults, ["fixture"]);
        assert_eq!(report.source_files_read, 0);
        assert_eq!(
            runtime.search(&request("markdownterm")).unwrap().len(),
            1,
            "a surviving format must be retained across an offline pass"
        );
        assert!(runtime.search(&request("plainterm")).unwrap().is_empty());
        assert_eq!(report.manifest.files.len(), 1);
        manager.shutdown().unwrap();
    }

    /// A run whose identities all match must not publish anything: the
    /// ordinary path pays nothing for the eviction machinery.
    #[test]
    fn an_unchanged_identity_set_publishes_no_generation_at_open() {
        let (_temporary, _vault_path, data_root, config) = two_format_vault();
        build_index(&config, &data_root).unwrap();
        let before = DataRoot::new(&data_root).active().unwrap().unwrap();
        let snapshot = snapshot_tree(&before.root);

        let manager = IndexManager::open(config, &data_root, SearchRuntime::new()).unwrap();
        assert!(manager.open_evictions().is_empty());
        let after = DataRoot::new(&data_root).active().unwrap().unwrap();
        assert_eq!(after.id, before.id);
        assert_eq!(snapshot_tree(&after.root), snapshot);
        manager.shutdown().unwrap();
    }

    /// A core mismatch is refused whole, with the rebuild instruction — never
    /// narrowed into a partial eviction.
    #[test]
    fn a_core_mismatch_refuses_the_open_instead_of_evicting() {
        let (_temporary, _vault_path, data_root, config) = two_format_vault();
        build_index(&config, &data_root).unwrap();
        let active = DataRoot::new(&data_root).active().unwrap().unwrap();
        let mut document: serde_json::Value =
            serde_json::from_slice(&fs::read(&active.manifest_path).unwrap()).unwrap();
        document["format_identity_schema_version"] =
            serde_json::json!(crate::policy::FORMAT_IDENTITY_SCHEMA_VERSION + 1);
        fs::write(
            &active.manifest_path,
            serde_json::to_vec_pretty(&document).unwrap(),
        )
        .unwrap();

        let Err(error) = IndexManager::open(config, &data_root, SearchRuntime::new()) else {
            panic!("a core mismatch must refuse the open");
        };
        assert!(error.to_string().contains("run `kwiry index`"));
    }

    /// The OpenClast profile evicts per partition, and an evicted row's
    /// postings are gone from the authorized partition before the first search.
    #[cfg_attr(
        not(target_os = "linux"),
        ignore = "secure OpenClast ingestion is Linux-only pending platform guidance"
    )]
    #[test]
    fn openclast_evicts_a_stale_format_from_its_partition_before_serving() {
        let temporary = tempdir().unwrap();
        let vault_path = temporary.path().join("vault");
        let data_root = temporary.path().join("data");
        fs::create_dir(&vault_path).unwrap();
        fs::write(vault_path.join("note.md"), "# Note\nmarkdownterm").unwrap();
        fs::write(vault_path.join("log.txt"), "plainterm").unwrap();
        let config = openclast_config(vec![VaultRegistration {
            snapshot_id: None,
            id: "fixture".into(),
            path: vault_path,
            room: Some("room-a".into()),
        }]);
        build_index(&config, &data_root).unwrap();
        assert_eq!(forge_foreign_identity(&data_root, SourceFormat::Text), 1);
        let resource = config.resource_key(&config.vaults[0]).unwrap();

        let runtime = SearchRuntime::new();
        let manager = IndexManager::open(config, &data_root, runtime.clone()).unwrap();

        assert_eq!(manager.open_evictions().total(), 1);
        assert!(
            matches!(runtime.search_authorized("plainterm", 20, &SearchFilters::default(), std::slice::from_ref(&resource)), Err(Error::UnavailableResourceIndexes(indexes)) if indexes == [0])
        );
        // Inspect the actual postings before any reconcile can reingest the
        // file with the current extractor. Public search remains blocked.
        let active = DataRoot::new(&data_root).active().unwrap().unwrap();
        let partitions = PartitionedSearchIndex::open(&active).unwrap();
        let search = |query: &str| {
            partitions
                .search(
                    query,
                    20,
                    &SearchFilters::default(),
                    std::slice::from_ref(&resource),
                )
                .unwrap()
        };
        assert!(search("plainterm").is_empty());
        assert_eq!(search("markdownterm").len(), 1);
        assert_eq!(manager.manifest().files.len(), 1);
        manager.shutdown().unwrap();
    }

    // ------------------------------------- the read-only door (`kwiry search`)

    /// Rewrites one format's recorded identity to a *literal* that no build
    /// derives. Deliberately not `format_identity_fingerprint(other_format)`
    /// and deliberately not built from the constant under test: a fixture
    /// derived from the guarded constant moves with a widened predicate and
    /// keeps passing while the guard stops guarding.
    fn forge_absent_identity(data_root: &Path, format: crate::format::SourceFormat) -> usize {
        let active = DataRoot::new(data_root).active().unwrap().unwrap();
        let mut document: serde_json::Value =
            serde_json::from_slice(&fs::read(&active.manifest_path).unwrap()).unwrap();
        let mut forged = 0;
        for (_, file) in document["files"].as_object_mut().unwrap() {
            if file["format"] == serde_json::json!(format.as_str()) {
                file.as_object_mut().unwrap().remove("format_identity");
                forged += 1;
            }
        }
        fs::write(
            &active.manifest_path,
            serde_json::to_vec_pretty(&document).unwrap(),
        )
        .unwrap();
        forged
    }

    /// `kwiry search` opens the index directly and holds no writer lock, so it
    /// cannot evict. Its only honest alternative is to refuse — and until this
    /// gate existed it did neither: it served every row the daemon would have
    /// evicted, under an identity that row was not built under, while the
    /// per-row rule was enforced on no CLI path at all.
    ///
    /// Filtering the stale rows out of the result set instead would not fix it:
    /// the postings would still be there for the next reader, which is the
    /// read-time filter this design rules out in favour of an open-time
    /// transformation.
    #[test]
    fn the_read_only_search_path_refuses_an_identity_stale_generation() {
        let (_temporary, _vault_path, data_root, config) = two_format_vault();
        build_index(&config, &data_root).unwrap();
        // Reads fine while every row is one this build could have produced.
        assert_eq!(
            crate::search::search_index(&data_root, &request("plainterm"))
                .unwrap()
                .len(),
            1
        );
        assert_eq!(forge_foreign_identity(&data_root, SourceFormat::Text), 1);

        let error = crate::search::search_index(&data_root, &request("plainterm"))
            .unwrap_err()
            .to_string();
        assert!(error.contains("log.txt"), "{error}");
        assert!(error.contains(&"f".repeat(64)), "{error}");
        assert!(error.contains("kwiry index"), "{error}");
        // The whole read is refused, including the format whose identity is
        // intact: answering from the survivors is the read-time filter.
        assert!(crate::search::search_index(&data_root, &request("markdownterm")).is_err());
    }

    /// The same for a row carrying no identity at all — the state a record
    /// written before the field existed presents, and the one a `None`-accepts
    /// widening of the predicate would wave through.
    #[test]
    fn the_read_only_search_path_refuses_a_row_with_no_recorded_identity() {
        let (_temporary, _vault_path, data_root, config) = two_format_vault();
        build_index(&config, &data_root).unwrap();
        assert_eq!(forge_absent_identity(&data_root, SourceFormat::Markdown), 1);

        let error = crate::search::search_index(&data_root, &request("markdownterm"))
            .unwrap_err()
            .to_string();
        assert!(error.contains("note.md"), "{error}");
        assert!(error.contains("<absent>"), "{error}");
        assert!(error.contains("kwiry index"), "{error}");
    }

    /// And a row whose recorded tier contradicts the compiled one, which the
    /// per-row predicate refuses even though its digest string matches. This is
    /// the exact state the pre-wave whole-artifact gate caught on this path,
    /// so it is the regression's own boundary.
    #[test]
    fn the_read_only_search_path_refuses_a_row_whose_profile_contradicts_this_build() {
        let (_temporary, _vault_path, data_root, config) = two_format_vault();
        build_index(&config, &data_root).unwrap();
        let active = DataRoot::new(&data_root).active().unwrap().unwrap();
        let mut document: serde_json::Value =
            serde_json::from_slice(&fs::read(&active.manifest_path).unwrap()).unwrap();
        for (_, file) in document["files"].as_object_mut().unwrap() {
            if file["format"] == serde_json::json!("markdown") {
                file["extraction_profile"] = serde_json::json!("enhanced");
            }
        }
        fs::write(
            &active.manifest_path,
            serde_json::to_vec_pretty(&document).unwrap(),
        )
        .unwrap();

        let error = crate::search::search_index(&data_root, &request("markdownterm"))
            .unwrap_err()
            .to_string();
        assert!(error.contains("note.md"), "{error}");
        assert!(error.contains("enhanced"), "{error}");
        assert!(error.contains("kwiry index"), "{error}");
    }

    /// The daemon still evicts rather than refuses: the reader's gate must not
    /// leak into the writer's path, or one format's identity change would cost
    /// the whole index again — the exact conflation this wave undoes.
    #[test]
    fn the_reader_gate_does_not_stop_the_daemon_from_evicting() {
        let (_temporary, _vault_path, data_root, config) = two_format_vault();
        build_index(&config, &data_root).unwrap();
        forge_foreign_identity(&data_root, SourceFormat::Text);

        let runtime = SearchRuntime::new();
        let manager = IndexManager::open(config, &data_root, runtime.clone()).unwrap();
        assert_eq!(manager.open_evictions().total(), 1);
        assert_eq!(runtime.search(&request("markdownterm")).unwrap().len(), 1);
        manager.shutdown().unwrap();

        // And once the daemon has evicted, the reader is satisfied again.
        assert_eq!(
            crate::search::search_index(&data_root, &request("markdownterm"))
                .unwrap()
                .len(),
            1
        );
        assert!(
            crate::search::search_index(&data_root, &request("plainterm"))
                .unwrap()
                .is_empty()
        );
    }

    // ------------------------------------------------ semantic reuse key

    /// Vectors may only be reused on the identity of the *text* they were
    /// computed from, never on the source bytes alone.
    ///
    /// Eviction re-ingests from byte-identical input, so `content_hash` cannot
    /// move across an extractor change — which means a store keyed on it hands
    /// `semantic` and `hybrid` an embedding of text the running build no longer
    /// emits, silently, because the hit bodies are hydrated from the current
    /// lexical index. The legacy state is constructed here directly, which is
    /// also exactly what an existing store looks like on first upgrade.
    #[test]
    fn semantic_vectors_are_keyed_by_extraction_identity_not_by_source_bytes() {
        use crate::semantic::{embedding_source_identity, test_support::HashEmbedder};

        let (temporary, _vault_path, data_root, config) = two_format_vault();
        build_index(&config, &data_root).unwrap();
        let runtime = SearchRuntime::new();
        let semantic = Arc::new(
            SemanticRuntime::open(
                &temporary.path().join("semantic.db"),
                Box::new(HashEmbedder::new()),
            )
            .unwrap(),
        );
        runtime.install_semantic(semantic.clone());
        let mut manager = IndexManager::open(config.clone(), &data_root, runtime.clone()).unwrap();
        manager.reconcile(config.clone()).unwrap();

        let key = crate::source::source_key("fixture", "log.txt");
        let content_hash = manager.manifest().files[&key].content_hash.clone();
        let bound = embedding_source_identity(&content_hash, SourceFormat::Text);
        assert_ne!(
            bound, content_hash,
            "the reuse key must not be the source-bytes digest"
        );
        assert_eq!(
            semantic.source_hash(&key).unwrap().as_deref(),
            Some(&*bound)
        );

        // Force the store into the state a build keyed on bytes alone leaves
        // behind, with a vector for a chunk the current extractor never
        // produced. Nothing about the source changed, so every lexical signal
        // says "settled".
        semantic
            .embed_and_replace_source(
                &key,
                &content_hash,
                &[(
                    "stale-chunk".to_owned(),
                    "text no extractor emits".to_owned(),
                )],
            )
            .unwrap();
        assert_eq!(
            semantic.source_hash(&key).unwrap().as_deref(),
            Some(&*content_hash)
        );

        // The next pass must read that source again and replace its vectors.
        let report = manager.reconcile(config).unwrap();
        assert!(
            report.source_files_read >= 1,
            "a source whose embedded text cannot be vouched for must be re-read"
        );
        assert_eq!(
            semantic.source_hash(&key).unwrap().as_deref(),
            Some(&*bound)
        );
        // The vector that named a chunk no extractor produces is gone. It never
        // surfaced as a wrong *hit* — `hydrate_ordered` resolves against the
        // lexical index and silently drops an unknown chunk id — which is the
        // point: the loss was invisible, and only the reuse key could see it.
        let hits = runtime
            .search_semantic("plainterm", 5, &SearchFilters::default())
            .unwrap();
        assert!(
            hits.iter().any(|hit| hit.path == "log.txt"),
            "the re-embedded source must be retrievable again"
        );
        assert!(hits.iter().all(|hit| hit.chunk_id != "stale-chunk"));
        manager.shutdown().unwrap();
    }

    /// The bound key must actually depend on the format, or binding it in buys
    /// nothing: two formats' rows sharing a content hash would share vectors.
    /// Asserted against literals rather than against the derivation.
    #[test]
    fn the_semantic_reuse_key_separates_formats_sharing_a_content_hash() {
        use crate::semantic::embedding_source_identity;

        let hash = "9".repeat(64);
        let markdown = embedding_source_identity(&hash, SourceFormat::Markdown);
        let text = embedding_source_identity(&hash, SourceFormat::Text);
        assert_ne!(markdown, text);
        assert_ne!(markdown, hash);
        assert_eq!(markdown.len(), 64);
        assert!(markdown.chars().all(|c| c.is_ascii_hexdigit()));
    }

    // ------------------------------------------ publication without history

    /// Sum of documents deleted but still resident in the committed segments
    /// of a generation's index.
    fn resident_deleted_docs(index_dir: &Path) -> u32 {
        let (index, _) = open_index_dir(index_dir).unwrap();
        index
            .searchable_segment_metas()
            .unwrap()
            .iter()
            .map(|meta| meta.num_deleted_docs())
            .sum()
    }

    fn committed_segment_ids(index_dir: &Path) -> BTreeSet<String> {
        let (index, _) = open_index_dir(index_dir).unwrap();
        index
            .searchable_segment_metas()
            .unwrap()
            .iter()
            .map(|meta| meta.id().uuid_string())
            .collect()
    }

    /// Every term the content field's dictionaries still record.
    fn resident_content_terms(index_dir: &Path) -> BTreeSet<String> {
        let (index, fields) = open_index_dir(index_dir).unwrap();
        let searcher = index.reader().unwrap().searcher();
        let mut terms = BTreeSet::new();
        for segment in searcher.segment_readers() {
            let inverted = segment.inverted_index(fields.content).unwrap();
            let mut stream = inverted.terms().range().into_stream().unwrap();
            while stream.advance() {
                terms.insert(String::from_utf8(stream.key().to_vec()).unwrap());
            }
        }
        terms
    }

    /// Writes the corpus that separates a tombstone-carrying index from a
    /// freshly built one.
    ///
    /// Both candidates carry both query terms, so every stage of the lexical
    /// ladder retrieves both and the two builds can only disagree about their
    /// *order*. `gamma` is common among the notes that get removed and rare
    /// among the survivors, so a build that still counts the removed notes
    /// treats a `gamma` match as weak evidence and prefers the `alpha`-heavy
    /// candidate; a build that counts only what the vault holds treats it as
    /// strong evidence and prefers the `gamma`-heavy one.
    fn write_drift_corpus(vault_path: &Path) {
        fs::create_dir_all(vault_path).unwrap();
        for index in 0..40 {
            fs::write(
                vault_path.join(format!("noise-{index:02}.md")),
                "gamma gamma gamma gamma\n",
            )
            .unwrap();
        }
        for index in 0..6 {
            fs::write(
                vault_path.join(format!("filler-{index:02}.md")),
                "alpha alpha alpha alpha alpha alpha\n",
            )
            .unwrap();
        }
        fs::write(
            vault_path.join("first-note.md"),
            "gamma delta alpha alpha alpha alpha alpha alpha alpha alpha\n",
        )
        .unwrap();
        fs::write(
            vault_path.join("second-note.md"),
            "alpha delta gamma gamma gamma gamma gamma gamma gamma gamma\n",
        )
        .unwrap();
    }

    fn remove_drift_noise(vault_path: &Path) {
        for index in 0..40 {
            fs::remove_file(vault_path.join(format!("noise-{index:02}.md"))).unwrap();
        }
    }

    fn drift_ordering(data_root: &Path, config: &Config) -> Vec<String> {
        let runtime = SearchRuntime::new();
        let manager = IndexManager::open(config.clone(), data_root, runtime.clone()).unwrap();
        let ordering = runtime
            .search(&request("alpha gamma"))
            .unwrap()
            .into_iter()
            .map(|hit| hit.path)
            .collect();
        manager.shutdown().unwrap();
        ordering
    }

    /// Ranking must be a function of the vault's current content, not of the
    /// history that produced the index. Tantivy keeps a deleted document's
    /// postings, term-dictionary entries and token counts resident in its
    /// segment until that segment is rewritten, and every BM25 corpus
    /// statistic — `total_num_docs`, `doc_freq`, `total_num_tokens` and the
    /// average fieldnorm derived from them — is read straight off those
    /// segments. Left alone, an incrementally maintained index therefore
    /// scores the same live notes against a corpus that no longer exists,
    /// and the resulting order can differ from a rebuild outright rather
    /// than differing only in magnitude.
    ///
    /// Scope, deliberately narrow: `write_drift_corpus` writes nothing longer
    /// than ten tokens, and `fieldnorm::code` is lossless below 41, so this
    /// exercises only the dead-document half of the property. It passes
    /// unchanged against a build whose collection length still comes from the
    /// segment header — verified, not assumed — which is why the
    /// quantiser-sensitive half has a corpus of its own in
    /// `an_incremental_index_and_a_rebuild_rank_a_vault_identically`. Keep
    /// this one short: a general claim asserted over one corpus shape is worth
    /// less than two corpora that each falsify a different mechanism.
    #[test]
    fn incremental_publication_orders_results_exactly_as_a_rebuild_does() {
        let temporary = tempdir().unwrap();
        let vault_path = temporary.path().join("vault");
        let incremental_root = temporary.path().join("incremental");
        let rebuilt_root = temporary.path().join("rebuilt");
        write_drift_corpus(&vault_path);
        let config = Config {
            vaults: vec![VaultRegistration {
                snapshot_id: None,
                id: "fixture".into(),
                path: vault_path.clone(),
                room: None,
            }],
            ..Config::default()
        };

        build_index(&config, &incremental_root).unwrap();
        let runtime = SearchRuntime::new();
        let mut manager =
            IndexManager::open(config.clone(), &incremental_root, runtime.clone()).unwrap();
        remove_drift_noise(&vault_path);
        manager.reconcile(config.clone()).unwrap();
        manager.shutdown().unwrap();

        // Same content, built from nothing.
        build_index(&config, &rebuilt_root).unwrap();

        let active = DataRoot::new(&incremental_root).active().unwrap().unwrap();
        assert_eq!(
            drift_ordering(&incremental_root, &config),
            drift_ordering(&rebuilt_root, &config),
            "the same vault must rank the same way however its index was built"
        );
        assert_eq!(
            resident_deleted_docs(&active.index_dir),
            0,
            "a published generation must not carry documents the vault no longer has"
        );
    }

    /// The eviction path publishes deletions with no accompanying additions,
    /// so it creates no new segment and can never trip a segment-count merge
    /// heuristic. It is also the largest single deletion the system issues —
    /// a format-identity change evicts every row of that format at once.
    #[test]
    fn open_time_eviction_leaves_no_resident_deleted_documents() {
        let temporary = tempdir().unwrap();
        let vault_path = temporary.path().join("vault");
        let data_root = temporary.path().join("data");
        fs::create_dir(&vault_path).unwrap();
        for index in 0..24 {
            fs::write(
                vault_path.join(format!("note-{index:02}.md")),
                "alpha beta gamma\n",
            )
            .unwrap();
        }
        fs::write(vault_path.join("log.txt"), "alpha beta gamma\n").unwrap();
        let config = Config {
            vaults: vec![VaultRegistration {
                snapshot_id: None,
                id: "fixture".into(),
                path: vault_path,
                room: None,
            }],
            ..Config::default()
        };
        build_index(&config, &data_root).unwrap();
        let active = DataRoot::new(&data_root).active().unwrap().unwrap();

        // Move the markdown format's identity so the next open evicts every
        // markdown row before the index is made searchable.
        assert_eq!(
            forge_foreign_identity(&data_root, SourceFormat::Markdown),
            24
        );

        let runtime = SearchRuntime::new();
        let manager = IndexManager::open(config.clone(), &data_root, runtime.clone()).unwrap();
        let evicted = DataRoot::new(&data_root).active().unwrap().unwrap();
        assert_ne!(evicted.id, active.id, "eviction must publish a generation");
        assert_eq!(
            resident_deleted_docs(&evicted.index_dir),
            0,
            "a delete-only publication must not leave its deletions resident"
        );
        assert_eq!(
            runtime.search(&request("alpha")).unwrap().len(),
            1,
            "only the unevicted text source survives"
        );
        manager.shutdown().unwrap();
    }

    /// A removed note's vocabulary must leave the index with it. The prefix
    /// ladder streams the term dictionary directly and spends a fixed budget
    /// in dictionary order, so a term that outlived its only document does not
    /// merely waste space: it can crowd a live term out of the expansion set
    /// and make a note that exists unfindable by prefix.
    #[test]
    fn a_removed_notes_vocabulary_leaves_the_term_dictionary() {
        let temporary = tempdir().unwrap();
        let vault_path = temporary.path().join("vault");
        let data_root = temporary.path().join("data");
        fs::create_dir(&vault_path).unwrap();
        // Five removed terms sort ahead of the surviving one, which is more
        // than a single note needs to exhaust a bounded expansion budget.
        fs::write(
            vault_path.join("removed.md"),
            "projalpha projbeta projdelta projepsilon projgamma\n",
        )
        .unwrap();
        fs::write(vault_path.join("kept.md"), "projzeta\n").unwrap();
        let config = Config {
            vaults: vec![VaultRegistration {
                snapshot_id: None,
                id: "fixture".into(),
                path: vault_path.clone(),
                room: None,
            }],
            ..Config::default()
        };
        build_index(&config, &data_root).unwrap();
        let runtime = SearchRuntime::new();
        let mut manager = IndexManager::open(config.clone(), &data_root, runtime.clone()).unwrap();

        fs::remove_file(vault_path.join("removed.md")).unwrap();
        manager.reconcile(config).unwrap();
        manager.shutdown().unwrap();

        let active = DataRoot::new(&data_root).active().unwrap().unwrap();
        let terms = resident_content_terms(&active.index_dir);
        assert!(
            terms.contains("projzeta"),
            "the surviving note's term must still be recorded"
        );
        for departed in [
            "projalpha",
            "projbeta",
            "projdelta",
            "projepsilon",
            "projgamma",
        ] {
            assert!(
                !terms.contains(departed),
                "{departed} outlived the only note that contained it"
            );
        }
    }

    /// The removal is paid for only when there is something to remove. A pass
    /// that adds a note and deletes nothing must leave the existing segments
    /// exactly as they were, so ordinary editing does not start rewriting the
    /// whole index.
    #[test]
    fn a_publication_that_deletes_nothing_does_not_rewrite_existing_segments() {
        let temporary = tempdir().unwrap();
        let vault_path = temporary.path().join("vault");
        let data_root = temporary.path().join("data");
        fs::create_dir(&vault_path).unwrap();
        for index in 0..8 {
            fs::write(
                vault_path.join(format!("note-{index:02}.md")),
                format!("alpha term{index:02}\n"),
            )
            .unwrap();
        }
        let config = Config {
            vaults: vec![VaultRegistration {
                snapshot_id: None,
                id: "fixture".into(),
                path: vault_path.clone(),
                room: None,
            }],
            ..Config::default()
        };
        build_index(&config, &data_root).unwrap();
        let before = DataRoot::new(&data_root).active().unwrap().unwrap();
        let original_segments = committed_segment_ids(&before.index_dir);
        assert!(!original_segments.is_empty());

        let runtime = SearchRuntime::new();
        let mut manager = IndexManager::open(config.clone(), &data_root, runtime.clone()).unwrap();
        fs::write(vault_path.join("added.md"), "alpha addedterm\n").unwrap();
        let report = manager.reconcile(config).unwrap();
        manager.shutdown().unwrap();
        assert_eq!(report.changed_sources, 1);

        let after = DataRoot::new(&data_root).active().unwrap().unwrap();
        assert_ne!(after.id, before.id, "the addition must publish");
        assert_eq!(
            resident_deleted_docs(&after.index_dir),
            0,
            "an addition deletes nothing"
        );
        let current_segments = committed_segment_ids(&after.index_dir);
        assert!(
            original_segments.is_subset(&current_segments),
            "a pass with nothing to remove must not rewrite the segments it inherited"
        );
        assert_eq!(runtime.search(&request("addedterm")).unwrap().len(), 1);
    }

    /// The OpenClast profile addresses deletions per partition, so the same
    /// invariant has to hold for each partition index it publishes.
    #[cfg_attr(
        not(target_os = "linux"),
        ignore = "secure OpenClast ingestion is Linux-only pending platform guidance"
    )]
    #[test]
    fn openclast_eviction_leaves_no_resident_deleted_documents_in_its_partition() {
        let temporary = tempdir().unwrap();
        let vault_path = temporary.path().join("vault");
        let data_root = temporary.path().join("data");
        fs::create_dir(&vault_path).unwrap();
        for index in 0..8 {
            fs::write(
                vault_path.join(format!("note-{index:02}.md")),
                "markdownterm\n",
            )
            .unwrap();
        }
        fs::write(vault_path.join("log.txt"), "plainterm\n").unwrap();
        let config = openclast_config(vec![VaultRegistration {
            snapshot_id: None,
            id: "fixture".into(),
            path: vault_path,
            room: Some("room-a".into()),
        }]);
        build_index(&config, &data_root).unwrap();
        assert_eq!(
            forge_foreign_identity(&data_root, SourceFormat::Markdown),
            8
        );
        let resource = config.resource_key(&config.vaults[0]).unwrap();

        let runtime = SearchRuntime::new();
        let manager = IndexManager::open(config, &data_root, runtime.clone()).unwrap();
        assert_eq!(manager.open_evictions().total(), 8);

        let active = DataRoot::new(&data_root).active().unwrap().unwrap();
        let partition = partition_index_dir(&active.partitions_dir, &resource);
        assert_eq!(
            resident_deleted_docs(&partition),
            0,
            "a partition must not carry rows its manifest no longer names"
        );
        assert!(
            !resident_content_terms(&partition).contains("markdownterm"),
            "the evicted rows' vocabulary must leave the partition with them"
        );
        manager.shutdown().unwrap();
    }

    // ------------------------------------- publication without history: BM25

    /// The `content` collection length as the two sources disagree about it:
    /// the figure recorded in each segment's inverted-index header, and the
    /// figure `search::live_collection_length` derives from the live documents'
    /// fieldnorms.
    ///
    /// A freshly written segment records an exact token count. A segment
    /// tantivy merged while it carried deletions records the sum of quantised
    /// fieldnorms instead, because that is all
    /// `merger.rs::estimate_total_num_tokens_in_single_segment` can recover.
    /// The two are equal only when every document is short enough for
    /// `fieldnorm::code` to be lossless, which is at most 40 tokens.
    fn content_collection_lengths(index_dir: &Path) -> (u64, u64) {
        let (index, fields) = open_index_dir(index_dir).unwrap();
        let searcher = index.reader().unwrap().searcher();
        let mut recorded = 0_u64;
        let mut live = 0_u64;
        for segment in searcher.segment_readers() {
            recorded += segment
                .inverted_index(fields.content)
                .unwrap()
                .total_num_tokens();
            let fieldnorms = segment
                .fieldnorms_readers()
                .get_field(fields.content)
                .unwrap()
                .expect("content is indexed with fieldnorms");
            for doc in segment.doc_ids_alive() {
                live += u64::from(tantivy::fieldnorm::FieldNormReader::id_to_fieldnorm(
                    fieldnorms.fieldnorm_id(doc),
                ));
            }
        }
        (recorded, live)
    }

    /// A deterministic note of `length` tokens drawn from a small vocabulary,
    /// so that terms recur across the corpus with unequal frequencies and the
    /// idf of a query term is a real number rather than a constant.
    fn drift_note(seed: u64, length: usize) -> String {
        const VOCABULARY: [&str; 24] = [
            "alpha", "beta", "gamma", "delta", "epsilon", "zeta", "eta", "theta", "iota", "kappa",
            "lambda", "mu", "nu", "xi", "omicron", "pi", "rho", "sigma", "tau", "upsilon", "phi",
            "chi", "psi", "omega",
        ];
        let mut state = seed.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
        let mut words = Vec::with_capacity(length);
        for _ in 0..length {
            state = state
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            words.push(VOCABULARY[((state >> 33) % 24) as usize]);
        }
        format!("{}\n", words.join(" "))
    }

    /// Writes the long-document corpus. Lengths run from 40 to 400 tokens, so
    /// all but the shortest are past the point where the fieldnorm quantiser
    /// starts rounding down — which is the whole reason this corpus exists.
    fn write_long_document_corpus(vault_path: &Path, revision: u64, edited: &[u64]) {
        fs::create_dir_all(vault_path).unwrap();
        for index in 0..200_u64 {
            let length = 40 + (index as usize % 7) * 60;
            let seed = if edited.contains(&index) {
                index + revision * 10_000
            } else {
                index
            };
            fs::write(
                vault_path.join(format!("note-{index:03}.md")),
                drift_note(seed, length),
            )
            .unwrap();
        }
    }

    fn ranked_paths(data_root: &Path, config: &Config, query: &str) -> Vec<(String, f32)> {
        let runtime = SearchRuntime::new();
        let manager = IndexManager::open(config.clone(), data_root, runtime.clone()).unwrap();
        let mut probe = request(query);
        probe.limit = 60;
        let ranking = runtime
            .search(&probe)
            .unwrap()
            .into_iter()
            .map(|hit| (hit.path, hit.score))
            .collect();
        manager.shutdown().unwrap();
        ranking
    }

    /// The whole invariant, over a corpus that can actually falsify it: a vault
    /// edited into its current state and the same vault built from nothing must
    /// return the same documents in the same order.
    ///
    /// `incremental_publication_orders_results_exactly_as_a_rebuild_does`
    /// asserts this property too, but every document in its corpus is under 40
    /// tokens, and that is exactly the region where `fieldnorm::code` is
    /// lossless. It can therefore only see the dead-document half of the
    /// defect. Removing the dead documents leaves a second, independent half
    /// standing: `expunge_resident_deletions` merges segments that carry
    /// deletions, which is the one path on which tantivy re-derives a field's
    /// token total from quantised fieldnorms, so the published segment records
    /// a total a few percent under what a rebuild writes. Measured on this
    /// corpus, 41,720 against 43,640 — a 4.4% shift in `avgdl`, in the same
    /// direction for every query. Because each stage of the evidence ladder is
    /// a bounded top-k, that perturbs the answer *set* and not merely its
    /// order.
    ///
    /// The first assertion is what keeps the rest honest. If this corpus ever
    /// drifts back into the quantiser's exact region — shorter notes, fewer of
    /// them, a different chunker — the recorded and live totals converge, the
    /// ranking assertions hold vacuously, and the test silently stops testing
    /// anything. So it fails loudly instead.
    ///
    /// Mutation checks, each confirmed to fail this test:
    /// - drop `expunge_resident_deletions` from `apply_index_updates`;
    /// - restore `search_reader`'s `&searcher` in place of
    ///   `AuthorizedStatistics`, so the collection length comes from the
    ///   segment header again.
    ///
    /// Scores are compared with a tolerance rather than bit-for-bit. Two
    /// independent rebuilds of this corpus already differ by about one f32 ulp
    /// on a three-term query — segment assignment is not deterministic across
    /// builds — so bit equality is not a property this system has on any path,
    /// and asserting it would make the test flaky rather than strict. Order and
    /// membership are exact.
    #[test]
    fn an_incremental_index_and_a_rebuild_rank_a_vault_identically() {
        let temporary = tempdir().unwrap();
        let vault_path = temporary.path().join("vault");
        let incremental_root = temporary.path().join("incremental");
        let rebuilt_root = temporary.path().join("rebuilt");
        write_long_document_corpus(&vault_path, 0, &[]);
        let config = Config {
            vaults: vec![VaultRegistration {
                snapshot_id: None,
                id: "fixture".into(),
                path: vault_path.clone(),
                room: None,
            }],
            ..Config::default()
        };

        build_index(&config, &incremental_root).unwrap();
        let runtime = SearchRuntime::new();
        let mut manager =
            IndexManager::open(config.clone(), &incremental_root, runtime.clone()).unwrap();
        let edited: Vec<u64> = (0..20).map(|note| note * 7 % 200).collect();
        write_long_document_corpus(&vault_path, 1, &edited);
        let report = manager.reconcile(config.clone()).unwrap();
        assert_eq!(report.changed_sources, edited.len());
        manager.shutdown().unwrap();

        // Byte-identical content, built from nothing.
        build_index(&config, &rebuilt_root).unwrap();

        let incremental = DataRoot::new(&incremental_root).active().unwrap().unwrap();
        let rebuilt = DataRoot::new(&rebuilt_root).active().unwrap().unwrap();
        let (incremental_recorded, incremental_live) =
            content_collection_lengths(&incremental.index_dir);
        let (rebuilt_recorded, rebuilt_live) = content_collection_lengths(&rebuilt.index_dir);

        assert_ne!(
            incremental_recorded, rebuilt_recorded,
            "this corpus must reach the lossy region of the fieldnorm quantiser, or every \
             assertion below holds for the wrong reason"
        );
        assert_eq!(
            incremental_live, rebuilt_live,
            "the collection length the scorer uses must depend on the live documents alone"
        );
        assert_eq!(resident_deleted_docs(&incremental.index_dir), 0);
        assert_eq!(resident_deleted_docs(&rebuilt.index_dir), 0);

        for query in ["alpha", "alpha beta", "gamma delta", "sigma tau upsilon"] {
            let incremental_ranking = ranked_paths(&incremental_root, &config, query);
            let rebuilt_ranking = ranked_paths(&rebuilt_root, &config, query);
            assert!(!incremental_ranking.is_empty(), "{query} retrieved nothing");
            assert_eq!(
                incremental_ranking
                    .iter()
                    .map(|(path, _)| path)
                    .collect::<Vec<_>>(),
                rebuilt_ranking
                    .iter()
                    .map(|(path, _)| path)
                    .collect::<Vec<_>>(),
                "{query}: the same vault must rank the same way however its index was built"
            );
            for ((path, incremental_score), (_, rebuilt_score)) in
                incremental_ranking.iter().zip(&rebuilt_ranking)
            {
                assert!(
                    (incremental_score - rebuilt_score).abs() <= 1e-6,
                    "{query}: {path} scored {incremental_score} incrementally and \
                     {rebuilt_score} rebuilt"
                );
            }
        }
    }

    /// The invariant has to hold for an index this build did not write.
    ///
    /// `apply_index_updates` establishes it only for generations this build
    /// publishes, and it is not reached at all by a pass with nothing to add
    /// and nothing to delete. Before `repair_inherited_generation`, a
    /// generation written by an earlier build survived the upgrade intact: its
    /// dead documents were counted in every query's corpus size, and their
    /// vocabulary stayed streamable from the term dictionary, for as long as
    /// the vault did not change. That is the exact state the doc comments on
    /// `search::AuthorizedStatistics` and `search::collect_prefix_expansions`
    /// declare they rely on being impossible.
    ///
    /// The fixture is the real thing rather than an approximation of it: the
    /// active generation's manifest drops a source and its rows are deleted and
    /// committed without being expunged, which is byte-for-byte what a build
    /// without `expunge_resident_deletions` left on disk.
    ///
    /// Mutation check: delete the `holds_resident_deletions` branch from
    /// `DesktopIndexManager::open` and this fails on both assertions.
    #[cfg_attr(
        not(target_os = "linux"),
        ignore = "secure OpenClast ingestion is Linux-only pending platform guidance"
    )]
    #[test]
    fn an_inherited_generation_is_repaired_before_it_is_served() {
        let temporary = tempdir().unwrap();
        let vault_path = temporary.path().join("vault");
        let data_root = temporary.path().join("data");
        fs::create_dir(&vault_path).unwrap();
        for index in 0..12 {
            fs::write(
                vault_path.join(format!("note-{index:02}.md")),
                format!("alpha inheritedterm{index:02}\n"),
            )
            .unwrap();
        }
        let config = Config {
            vaults: vec![VaultRegistration {
                snapshot_id: None,
                id: "fixture".into(),
                path: vault_path.clone(),
                room: None,
            }],
            ..Config::default()
        };
        build_index(&config, &data_root).unwrap();
        let active = DataRoot::new(&data_root).active().unwrap().unwrap();

        let (mut manifest, _) = Manifest::load(&active.manifest_path).unwrap().adopt();
        let departed = manifest
            .files
            .iter()
            .find(|(_, file)| file.path.contains("note-00"))
            .map(|(key, _)| key.clone())
            .unwrap();
        manifest.files.remove(&departed);
        manifest.mark_synced().unwrap();
        manifest.save(&active.manifest_path).unwrap();
        fs::remove_file(vault_path.join("note-00.md")).unwrap();
        {
            let (index, fields) = open_index_dir(&active.index_dir).unwrap();
            let mut writer: IndexWriter = index.writer(WRITER_MEMORY_BYTES).unwrap();
            // Collapse to one segment before deleting. `build_index` spreads
            // documents over as many segments as the writer has indexing
            // threads, so on a busy machine the departed note can be the only
            // occupant of its segment — and a segment with no live documents
            // is dropped at commit rather than left carrying a tombstone,
            // which is not the state this test is about.
            let segments: Vec<SegmentId> = index
                .searchable_segment_metas()
                .unwrap()
                .iter()
                .map(|meta| meta.id())
                .collect();
            if segments.len() > 1 {
                writer.merge(&segments).wait().unwrap();
            }
            writer.delete_term(Term::from_field_text(fields.source_key.unwrap(), &departed));
            writer.commit().unwrap();
            writer.wait_merging_threads().unwrap();
        }
        assert_eq!(
            resident_deleted_docs(&active.index_dir),
            1,
            "the fixture must reproduce what the earlier build published"
        );

        // The vault is now consistent with the inherited manifest, so nothing
        // in the reconciliation pass has any reason to publish.
        let runtime = SearchRuntime::new();
        let mut manager = IndexManager::open(config.clone(), &data_root, runtime.clone()).unwrap();
        let report = manager.reconcile(config).unwrap();
        assert_eq!(report.changed_sources, 0);
        assert_eq!(runtime.search(&request("alpha")).unwrap().len(), 11);
        manager.shutdown().unwrap();

        let served = DataRoot::new(&data_root).active().unwrap().unwrap();
        assert_eq!(
            resident_deleted_docs(&served.index_dir),
            0,
            "an inherited generation must be repaired before it is served"
        );
        assert!(
            !resident_content_terms(&served.index_dir).contains("inheritedterm00"),
            "the departed note's vocabulary must leave the term dictionary with it"
        );
    }

    fn governed_header_fixture() -> (
        tempfile::TempDir,
        Config,
        PathBuf,
        SearchRuntime,
        IndexManager,
    ) {
        let temp = tempdir().unwrap();
        let mut vaults = Vec::new();
        for id in ["a", "b"] {
            let path = temp.path().join(id);
            fs::create_dir(&path).unwrap();
            fs::write(path.join("note.md"), "# Note\nneedle evidence").unwrap();
            vaults.push(VaultRegistration {
                snapshot_id: None,
                id: id.into(),
                path,
                room: Some(format!("room-{id}")),
            });
        }
        let config = openclast_config(vaults);
        let data = temp.path().join("data");
        build_index(&config, &data).unwrap();
        let runtime = SearchRuntime::new();
        let mut manager = IndexManager::open(config.clone(), &data, runtime.clone()).unwrap();
        manager.reconcile(config.clone()).unwrap();
        (temp, config, data, runtime, manager)
    }

    #[cfg_attr(
        not(target_os = "linux"),
        ignore = "secure OpenClast ingestion is Linux-only pending platform guidance"
    )]
    #[test]
    fn governed_headers_ignore_forbidden_publish_reconcile_and_watcher_evidence() {
        let (_temp, config, data, runtime, mut manager) = governed_header_fixture();
        let resources = [config.resource_key(&config.vaults[0]).unwrap()];
        let filters = SearchFilters::default();
        let baseline = runtime
            .search_authorized_with_generation("needle", 20, &filters, &resources)
            .unwrap();
        assert!(baseline.generation.starts_with("v1-"));
        assert_eq!(baseline.generation.len(), 67);
        let freshness = || {
            runtime
                .authorized_freshness(&filters, &resources, &baseline.generation)
                .unwrap()
        };
        assert_eq!(freshness().state, IndexFreshnessState::Current);
        let forbidden_scope =
            ReconcileScope::Paths(BTreeSet::from([("b".into(), "note.md".into())]));
        runtime
            .mark_resources_dirty(&config, &forbidden_scope)
            .unwrap();
        assert_eq!(freshness().state, IndexFreshnessState::Current);
        let epochs = runtime
            .begin_resource_pass(&config, &forbidden_scope)
            .unwrap();
        assert_eq!(freshness().state, IndexFreshnessState::Current);
        runtime
            .finish_resource_pass(epochs, &BTreeSet::new(), false)
            .unwrap();
        assert_eq!(freshness().state, IndexFreshnessState::Current);
        let old_global = runtime.generation();
        fs::write(
            config.vaults[1].path.join("note.md"),
            "# Forbidden\nchanged changed needle",
        )
        .unwrap();
        manager
            .reconcile_scoped(config.clone(), &forbidden_scope)
            .unwrap();
        assert_ne!(old_global, runtime.generation());
        assert_eq!(
            runtime
                .search_authorized_with_generation("needle", 20, &filters, &resources)
                .unwrap(),
            baseline
        );
        assert_eq!(freshness().state, IndexFreshnessState::Current);
        // Full publication (including its timestamp) also leaves A's header alone
        // once the covering pass completes.
        manager.reconcile(config.clone()).unwrap();
        assert_eq!(
            runtime
                .search_authorized_with_generation("needle", 20, &filters, &resources)
                .unwrap(),
            baseline
        );
        assert_eq!(freshness().state, IndexFreshnessState::Current);
        // Removing and re-adding a forbidden partition must not identify a new
        // authorized view either, even though both operations publish layouts.
        let mut without_forbidden = config.clone();
        without_forbidden.vaults.pop();
        for next in [without_forbidden, config.clone()] {
            manager.reconcile(next).unwrap();
            assert_eq!(
                runtime
                    .search_authorized_with_generation("needle", 20, &filters, &resources)
                    .unwrap(),
                baseline
            );
            assert_eq!(freshness().state, IndexFreshnessState::Current);
        }
        manager.shutdown().unwrap();
        // An external full rebuild, not just incremental reconciliation.
        build_index(&config, &data).unwrap();
        let mut reopened = IndexManager::open(config.clone(), &data, runtime.clone()).unwrap();
        reopened.reconcile(config).unwrap();
        assert_eq!(
            runtime
                .search_authorized_with_generation("needle", 20, &filters, &resources)
                .unwrap(),
            baseline
        );
        assert_eq!(freshness().state, IndexFreshnessState::Current);
        reopened.shutdown().unwrap();
    }

    #[cfg_attr(
        not(target_os = "linux"),
        ignore = "secure OpenClast ingestion is Linux-only pending platform guidance"
    )]
    #[test]
    fn governed_metadata_audit_backlog_cannot_leak_forbidden_work() {
        let (temp, mut config, _data, runtime, mut manager) = governed_header_fixture();
        let resource = [config.resource_key(&config.vaults[0]).unwrap()];
        // Settled mtimes select the metadata fast path deterministically.
        for vault in &config.vaults {
            fs::File::options()
                .write(true)
                .open(vault.path.join("note.md"))
                .unwrap()
                .set_modified(UNIX_EPOCH + std::time::Duration::from_secs(1_600_000_000))
                .unwrap();
        }
        manager.reconcile(config.clone()).unwrap();
        config.indexing.basis = IndexFreshnessBasis::MetadataAudit;
        config.indexing.audit_sources_per_pass = 1;
        // The forbidden vault consumes the only audit slot before A is visited.
        config.vaults.reverse();
        let report = manager.reconcile(config.clone()).unwrap();
        assert!(report.audit_pending);
        let filters = SearchFilters::default();
        let actual = runtime
            .search_authorized_with_generation("needle", 20, &filters, &resource)
            .unwrap();
        let freshness = runtime
            .authorized_freshness(&filters, &resource, &actual.generation)
            .unwrap();
        assert_eq!(freshness.state, IndexFreshnessState::Current);
        assert_eq!(freshness.basis, IndexFreshnessBasis::MetadataAudit);

        config.vaults.retain(|vault| vault.id == "a");
        let baseline_data = temp.path().join("authorized-only");
        build_index(&config, &baseline_data).unwrap();
        let baseline_runtime = SearchRuntime::new();
        let mut baseline_manager =
            IndexManager::open(config.clone(), &baseline_data, baseline_runtime.clone()).unwrap();
        let report = baseline_manager.reconcile(config.clone()).unwrap();
        assert!(!report.audit_pending);
        let baseline = baseline_runtime
            .search_authorized_with_generation("needle", 20, &filters, &resource)
            .unwrap();
        assert_eq!(actual, baseline);
        assert_eq!(
            freshness,
            baseline_runtime
                .authorized_freshness(&filters, &resource, &baseline.generation)
                .unwrap()
        );
        let scope = ReconcileScope::Paths(BTreeSet::from([("a".into(), "note.md".into())]));
        runtime.mark_resources_dirty(&config, &scope).unwrap();
        manager.reconcile_scoped(config, &scope).unwrap();
        assert_eq!(
            runtime
                .authorized_freshness(&filters, &resource, &actual.generation)
                .unwrap(),
            freshness
        );
        baseline_manager.shutdown().unwrap();
        manager.shutdown().unwrap();
    }

    #[cfg_attr(
        not(target_os = "linux"),
        ignore = "secure OpenClast ingestion is Linux-only pending platform guidance"
    )]
    #[test]
    fn governed_freshness_transitions_and_effective_set_are_resource_scoped() {
        let (_temp, config, _data, runtime, mut manager) = governed_header_fixture();
        let a = config.resource_key(&config.vaults[0]).unwrap();
        let b = config.resource_key(&config.vaults[1]).unwrap();
        let resources = [a.clone(), b.clone()];
        let filters = SearchFilters::default();
        let generation = runtime
            .search_authorized_with_generation("needle", 20, &filters, &resources)
            .unwrap()
            .generation;
        let state = || {
            runtime
                .authorized_freshness(&filters, &resources, &generation)
                .unwrap()
                .state
        };
        let scope = ReconcileScope::Paths(BTreeSet::from([("a".into(), "note.md".into())]));
        let epochs = runtime.begin_resource_pass(&config, &scope).unwrap();
        assert_eq!(state(), IndexFreshnessState::Reconciling);
        runtime
            .finish_resource_pass(epochs, &BTreeSet::from([a.clone()]), true)
            .unwrap();
        assert_eq!(state(), IndexFreshnessState::Stale);
        let epochs = runtime.begin_resource_pass(&config, &scope).unwrap();
        assert_eq!(state(), IndexFreshnessState::Reconciling);
        runtime.mark_resources_dirty(&config, &scope).unwrap();
        runtime
            .finish_resource_pass(epochs, &BTreeSet::new(), true)
            .unwrap();
        assert_eq!(
            state(),
            IndexFreshnessState::Stale,
            "a concurrent event must survive completion"
        );
        manager.reconcile_scoped(config.clone(), &scope).unwrap();
        assert_eq!(state(), IndexFreshnessState::Current);
        runtime.mark_resources_dirty(&config, &scope).unwrap();
        assert_eq!(state(), IndexFreshnessState::Stale);
        let only_b = SearchFilters {
            room: Some(b.room_id.clone()),
            ..SearchFilters::default()
        };
        let from_both = runtime
            .search_authorized_with_generation("needle", 20, &only_b, &resources)
            .unwrap();
        let from_b = runtime
            .search_authorized_with_generation("needle", 20, &only_b, std::slice::from_ref(&b))
            .unwrap();
        assert_eq!(from_both, from_b);
        assert_eq!(
            runtime
                .authorized_freshness(&only_b, &resources, &from_b.generation)
                .unwrap()
                .state,
            IndexFreshnessState::Current
        );
        // Sorting is bytewise tuple order, independent of signed ordering.
        let reversed = runtime
            .search_authorized_with_generation("needle", 20, &filters, &[b, a])
            .unwrap();
        assert_eq!(generation, reversed.generation);
        let empty_filters = SearchFilters {
            room: Some("no-such-room".into()),
            ..SearchFilters::default()
        };
        let empty = runtime
            .search_authorized_with_generation("needle", 20, &empty_filters, &resources)
            .unwrap();
        assert!(empty.hits.is_empty());
        assert_eq!(
            runtime
                .authorized_freshness(&empty_filters, &resources, &empty.generation)
                .unwrap()
                .state,
            IndexFreshnessState::Current
        );
        fs::write(
            config.vaults[0].path.join("note.md"),
            "# Changed\nneedle new evidence",
        )
        .unwrap();
        manager.reconcile_scoped(config.clone(), &scope).unwrap();
        assert_eq!(
            state(),
            IndexFreshnessState::Stale,
            "superseded authorized revision"
        );
        let mut unmapped = config.clone();
        unmapped.vaults.remove(0);
        manager.reconcile(unmapped).unwrap();
        assert!(matches!(
            runtime.authorized_freshness(&filters, &resources, &generation),
            Err(Error::ResourceUnavailable | Error::UnavailableResourceIndexes(_))
        ));
        manager.shutdown().unwrap();
    }

    #[cfg_attr(
        not(target_os = "linux"),
        ignore = "secure OpenClast ingestion is Linux-only pending platform guidance"
    )]
    #[test]
    fn governed_mtime_only_rebuild_changes_range_results_and_identity_together() {
        let (_temp, config, data, runtime, manager) = governed_header_fixture();
        let file = config.vaults[0].path.join("note.md");
        let resource = [config.resource_key(&config.vaults[0]).unwrap()];
        let source_key = crate::source::room_source_key(
            &resource[0].vault_id,
            Some(&resource[0].room_id),
            "note.md",
        );
        let filters = SearchFilters::default();
        let set_time = |seconds| {
            fs::File::options()
                .write(true)
                .open(&file)
                .unwrap()
                .set_modified(UNIX_EPOCH + std::time::Duration::from_secs(seconds))
                .unwrap()
        };
        manager.shutdown().unwrap();
        set_time(1_600_000_000);
        build_index(&config, &data).unwrap();
        let mut manager = IndexManager::open(config.clone(), &data, runtime.clone()).unwrap();
        manager.reconcile(config.clone()).unwrap();
        let query = "mtime:[1599999999 TO 1600000001]";
        let before = runtime
            .search_authorized_with_generation(query, 20, &filters, &resource)
            .unwrap();
        assert_eq!(before.hits.len(), 1);
        let hash = manager.manifest().files[&source_key].content_hash.clone();
        // Reconciliation is also a rebuild of the mtime-bearing index document.
        set_time(1_700_000_000);
        manager.reconcile(config.clone()).unwrap();
        let after_reconcile = runtime
            .search_authorized_with_generation(query, 20, &filters, &resource)
            .unwrap();
        assert!(after_reconcile.hits.is_empty());
        assert_ne!(before.generation, after_reconcile.generation);
        assert_eq!(hash, manager.manifest().files[&source_key].content_hash);
        assert_eq!(
            runtime
                .search_authorized("mtime:[1699999999 TO 1700000001]", 20, &filters, &resource)
                .unwrap()
                .len(),
            1
        );
        manager.shutdown().unwrap();
        // A full rebuild with unchanged bytes has exactly the same revision.
        build_index(&config, &data).unwrap();
        let mut manager = IndexManager::open(config.clone(), &data, runtime.clone()).unwrap();
        manager.reconcile(config).unwrap();
        let after_rebuild = runtime
            .search_authorized_with_generation(query, 20, &filters, &resource)
            .unwrap();
        assert_eq!(after_rebuild, after_reconcile);
        manager.shutdown().unwrap();
    }

    #[cfg_attr(
        not(target_os = "linux"),
        ignore = "secure OpenClast ingestion is Linux-only pending platform guidance"
    )]
    #[test]
    fn governed_revision_covers_every_persisted_file_field_and_manifest_version() {
        let (_temp, config, _data, _runtime, manager) = governed_header_fixture();
        let resource = config.resource_key(&config.vaults[0]).unwrap();
        let manifest = manager.manifest();
        let baseline = resource_revision(manifest, &resource).unwrap();
        let key = manifest
            .files
            .iter()
            .find(|(_, f)| f.resource.as_ref() == Some(&resource))
            .unwrap()
            .0
            .clone();
        let original = serde_json::to_value(&manifest.files[&key]).unwrap();
        // Mutate every serialized field, including nested resource/coverage/profile
        // values. This test automatically grows with persisted ManifestFile fields.
        for (field, value) in original.as_object().unwrap() {
            let mut row = original.clone();
            row[field] = match value {
                serde_json::Value::String(value) => {
                    serde_json::Value::String(format!("{value}-changed"))
                }
                serde_json::Value::Number(value) => serde_json::json!(value.as_u64().unwrap() + 1),
                serde_json::Value::Null => serde_json::json!("changed"),
                serde_json::Value::Object(_) => {
                    let mut v = value.clone();
                    v["room_id"] = serde_json::json!("changed-room");
                    v
                }
                _ => panic!("add a field mutation for {field}"),
            };
            // Closed enums cannot deserialize arbitrary names: use real alternates.
            match field.as_str() {
                "format" => row[field] = serde_json::json!("text"),
                "extraction_profile" => row[field] = serde_json::json!("none"),
                "coverage" => row[field] = serde_json::json!("unreadable"),
                "outcome" => row[field] = serde_json::json!("skipped"),
                _ => {}
            }
            let mut changed = manifest.clone();
            changed
                .files
                .insert(key.clone(), serde_json::from_value(row).unwrap());
            assert_ne!(
                baseline,
                resource_revision(&changed, &resource).unwrap(),
                "field {field}"
            );
        }
        for version in 0..5 {
            let mut changed = manifest.clone();
            match version {
                0 => changed.manifest_version += 1,
                1 => changed.index_format_version += 1,
                2 => changed.chunking_version += 1,
                3 => changed.preparation_schema_version += 1,
                _ => changed.format_identity_schema_version += 1,
            }
            assert_ne!(baseline, resource_revision(&changed, &resource).unwrap());
        }
        let mut global_only = manifest.clone();
        global_only.state_revision += 1;
        global_only.last_sync = Some("unrelated-global-time".into());
        assert_eq!(
            baseline,
            resource_revision(&global_only, &resource).unwrap()
        );
        // u64-LE framing is architecture independent, including lengths >255.
        let mut framed = Sha256::new();
        digest_bytes(&mut framed, &[7; 300]);
        let mut expected = Sha256::new();
        expected.update(300_u64.to_le_bytes());
        expected.update([7; 300]);
        assert_eq!(framed.finalize(), expected.finalize());
        manager.shutdown().unwrap();
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn same_vault_room_identity_and_scoped_lexical_vectors() {
        let temporary = tempdir().unwrap();
        let root = temporary.path();
        let data = root.join("data");
        let vault = |name: &str, room: &str, text: &str| {
            let path = root.join(name);
            fs::create_dir(&path).unwrap();
            fs::write(path.join("index.md"), text).unwrap();
            VaultRegistration {
                snapshot_id: None,
                id: "VaultOne_0123456789abc".into(),
                path,
                room: Some(room.into()),
            }
        };
        let a = vault(
            "a",
            "RoomAaaa_0123456789abc",
            "---\nstatus: alpha\n---\nalpha shared",
        );
        let b = vault(
            "b",
            "RoomBbbb_0123456789abc",
            "---\nstatus: beta\n---\nbeta shared",
        );
        let mut config = openclast_config(vec![a.clone(), b.clone()]);
        let ra = config.resource_key(&a).unwrap();
        let rb = config.resource_key(&b).unwrap();
        let resources = [ra.clone(), rb.clone()];
        let runtime = SearchRuntime::new();
        build_index(&config, &data).unwrap();
        let mut manager = IndexManager::open(config.clone(), &data, runtime.clone()).unwrap();
        manager.reconcile(config.clone()).unwrap();
        assert!(runtime.active_room_identity_format());
        let filters = SearchFilters::default();
        let search = |filters: &SearchFilters, resources: &[ResourceKey], limit| {
            runtime
                .search_authorized_with_generation("shared", limit, filters, resources)
                .unwrap()
        };
        let a_only = search(&filters, std::slice::from_ref(&ra), 20);
        let b_only = search(&filters, std::slice::from_ref(&rb), 20);
        assert_eq!(a_only.hits.len(), 1); // P1
        let baseline = search(&filters, &resources, 20);
        assert_eq!(baseline.hits.len(), 2); // P2
        assert_ne!(baseline.hits[0].chunk_id, baseline.hits[1].chunk_id);
        assert_eq!(
            baseline
                .hits
                .iter()
                .map(|hit| hit.path.as_str())
                .collect::<Vec<_>>(),
            ["index.md", "index.md"]
        );
        for hit in &baseline.hits {
            let expected = if hit.room_id.as_deref() == Some(ra.room_id.as_str()) {
                "alpha"
            } else {
                assert_eq!(hit.room_id.as_deref(), Some(rb.room_id.as_str()));
                "beta"
            };
            assert_eq!(hit.vault_id, ra.vault_id);
            assert_eq!(hit.frontmatter.status.as_deref(), Some(expected)); // P13
            assert!(hit.excerpt.contains(expected));
        }
        let revision_a = a_only.generation.clone();
        let freshness_a = runtime
            .authorized_freshness(&filters, std::slice::from_ref(&ra), &revision_a)
            .unwrap();
        let room = |id: &str| SearchFilters {
            room: Some(id.into()),
            ..SearchFilters::default()
        };
        assert_eq!(
            search(&room(&rb.room_id), &resources, 20).generation,
            b_only.generation
        ); // P6
        let outside = search(&room("RoomZzzz_0123456789abc"), &resources, 20);
        assert!(outside.hits.is_empty());
        assert_eq!(outside, search(&filters, &[], 20)); // P7 empty-set digest
        assert_eq!(
            outside,
            search(
                &SearchFilters {
                    room: Some(ra.room_id.clone()),
                    vault_id: Some("VaultTwo_0123456789abc".into()),
                    ..SearchFilters::default()
                },
                &resources,
                20
            )
        ); // P14
        assert_eq!(search(&filters, &resources, 1).hits.len(), 1); // P10

        let mut d = vault("d", "RoomDddd_0123456789abc", "delta shared");
        d.id = "VaultTwo_0123456789abc".into();
        let rd = config.resource_key(&d).unwrap();
        config.vaults.push(d);
        manager.reconcile(config.clone()).unwrap();
        assert_eq!(
            search(&filters, &[ra.clone(), rb.clone(), rd.clone()], 20)
                .hits
                .len(),
            3
        ); // P3 core
        let wrong_vault = ResourceKey {
            vault_id: ra.vault_id.clone(),
            ..rd
        };
        let absent = ResourceKey {
            room_id: "RoomZzzz_0123456789abc".into(),
            ..wrong_vault.clone()
        };
        for missing in [wrong_vault.clone(), absent.clone()] {
            assert!(
                matches!(runtime.search_authorized_with_generation("shared", 20, &filters, &[missing]), Err(Error::UnavailableResourceIndexes(indexes)) if indexes == [0])
            ); // N14
        }
        assert!(
            matches!(runtime.search_authorized_with_generation("shared", 20, &filters, &[wrong_vault, ra.clone(), absent]), Err(Error::UnavailableResourceIndexes(indexes)) if indexes == [0, 2])
        ); // no partial hits; evaluate every tuple
        config.vaults.pop();
        manager.reconcile(config.clone()).unwrap();

        let c = vault("c", "RoomCccc_0123456789abc", "gamma alone");
        config.vaults.push(c.clone());
        manager.reconcile(config.clone()).unwrap();
        assert_eq!(
            outside,
            search(&room(c.room.as_deref().unwrap()), &resources, 20)
        ); // P7 existence invariance
        let bytes = |result: &GenerationSearchResult| serde_json::to_vec(&result.hits).unwrap();
        assert_eq!(bytes(&baseline), bytes(&search(&filters, &resources, 20))); // P8 add
        fs::write(c.path.join("index.md"), "shared gamma").unwrap();
        manager.reconcile(config.clone()).unwrap();
        assert_eq!(baseline, search(&filters, &resources, 20)); // P8 mutate
        config.vaults.pop();
        manager.reconcile(config.clone()).unwrap();
        assert_eq!(bytes(&baseline), bytes(&search(&filters, &resources, 20))); // P8 remove

        fs::write(b.path.join("index.md"), "beta shared changed").unwrap();
        manager
            .reconcile_scoped(
                config.clone(),
                &ReconcileScope::RoomPaths(BTreeSet::from([(rb.clone(), "index.md".into())])),
            )
            .unwrap();
        assert_eq!(a_only, search(&filters, std::slice::from_ref(&ra), 20)); // P11
        assert_eq!(
            freshness_a,
            runtime
                .authorized_freshness(&filters, std::slice::from_ref(&ra), &revision_a)
                .unwrap()
        );
        config.vaults.remove(0);
        let report = manager.reconcile(config).unwrap();
        let b_key = crate::source::room_source_key(&rb.vault_id, Some(&rb.room_id), "index.md");
        assert!(report.manifest.files.contains_key(&b_key)); // P12
        assert_eq!(search(&filters, &[rb], 20).hits[0].path, "index.md");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn room_ingestion_n20_n25_never_retains_hard_linked_content() {
        use std::os::unix::fs::symlink;
        let temporary = tempdir().unwrap();
        let a = temporary.path().join("a");
        let b = temporary.path().join("b");
        let data = temporary.path().join("data");
        fs::create_dir(&a).unwrap();
        fs::create_dir(&b).unwrap();
        fs::write(b.join("index.md"), "beta shared").unwrap();
        symlink(b.join("index.md"), a.join("link.md")).unwrap();
        let config = openclast_config(vec![
            VaultRegistration {
                snapshot_id: None,
                id: "vault".into(),
                path: a.clone(),
                room: Some("room-a".into()),
            },
            VaultRegistration {
                snapshot_id: None,
                id: "vault".into(),
                path: b.clone(),
                room: Some("room-b".into()),
            },
        ]);
        let resources: Vec<_> = config
            .vaults
            .iter()
            .map(|vault| config.resource_key(vault).unwrap())
            .collect();
        let runtime = SearchRuntime::new();
        build_index(&config, &data).unwrap();
        let mut manager = IndexManager::open(config.clone(), &data, runtime.clone()).unwrap();
        manager.reconcile(config.clone()).unwrap();
        let filters = SearchFilters::default();
        let search = |resource| {
            runtime
                .search_authorized_with_generation(
                    "beta",
                    20,
                    &filters,
                    std::slice::from_ref(resource),
                )
                .unwrap()
        };
        assert!(search(&resources[0]).hits.is_empty()); // N20
        assert_eq!(search(&resources[1]).hits.len(), 1);
        fs::hard_link(b.join("index.md"), a.join("hard.md")).unwrap();
        let report = manager.reconcile(config).unwrap();
        assert_eq!(
            report
                .warnings
                .iter()
                .filter(|warning| warning.message == "hard-linked file excluded")
                .count(),
            2
        );
        for resource in &resources {
            let after = search(resource);
            assert!(after.hits.is_empty(), "N25");
            assert_eq!(
                runtime
                    .authorized_freshness(
                        &filters,
                        std::slice::from_ref(resource),
                        &after.generation
                    )
                    .unwrap()
                    .state,
                IndexFreshnessState::Stale
            );
        }
    }
}
