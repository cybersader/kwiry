use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use arc_swap::ArcSwapOption;
use tantivy::IndexWriter;
use tantivy::collector::DocSetCollector;
use tantivy::index::SegmentId;
use tantivy::query::AllQuery;
use tantivy::schema::{Field, Value};
use tantivy::{Index, IndexReader, ReloadPolicy, TantivyDocument, Term};

use crate::api::SearchFilters;
use crate::chunk::ingest_file;
use crate::error::{Error, Result};
use crate::generation::{DataRoot, DataRootLock};
use crate::index::{
    Fields, build_schema, chunk_document, open_index_dir, register_lexical_analyzer,
};
use crate::manifest::{
    EvictionReport, Manifest, ManifestFile, ManifestFileOutcome, registration_fingerprint,
    source_key,
};
use crate::model::{
    Config, FileIngestOutcome, FileOutcomeKind, HostProfile, IndexFreshnessBasis, IngestWarning,
    LexicalSearchRequest, PreparedChunk, ResourceKey, RetrievalMetadata, SearchHit,
};
use crate::partition::{GenerationLayout, partition_index_dir};
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
use crate::search::{PartitionReader, search_partitions, search_reader};
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
    pub generation: String,
    pub hits: Vec<SearchHit>,
}

#[derive(Clone)]
pub struct SearchRuntime {
    active: Arc<ArcSwapOption<ActiveSearchIndex>>,
    semantic: Arc<ArcSwapOption<SemanticRuntime>>,
    freshness_basis: Arc<AtomicU8>,
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
            ActiveSearchIndex::Desktop(index) => Ok(GenerationSearchResult {
                generation: index.generation.clone(),
                hits: index.search(query, limit, filters)?,
            }),
            ActiveSearchIndex::OpenClast(_) => Err(Error::Auth(
                "openclast search requires an explicit authorized resource set".to_owned(),
            )),
        }
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
        let active = self.active.load_full().ok_or(Error::IndexBuilding)?;
        match active.as_ref() {
            ActiveSearchIndex::Desktop(_) => Err(Error::Auth(
                "authorized resource search is unavailable in the desktop profile".to_owned(),
            )),
            ActiveSearchIndex::OpenClast(index) => index.search_with_profile(
                query,
                limit,
                filters,
                resources,
                profile,
                query_time_epoch_seconds,
            ),
        }
    }

    pub fn search_authorized_with_generation(
        &self,
        query: &str,
        limit: usize,
        filters: &SearchFilters,
        resources: &[ResourceKey],
    ) -> Result<GenerationSearchResult> {
        let active = self.active.load_full().ok_or(Error::IndexBuilding)?;
        match active.as_ref() {
            ActiveSearchIndex::Desktop(_) => Err(Error::Auth(
                "authorized resource search is unavailable in the desktop profile".to_owned(),
            )),
            ActiveSearchIndex::OpenClast(index) => Ok(GenerationSearchResult {
                generation: index.generation.clone(),
                hits: index.search(query, limit, filters, resources)?,
            }),
        }
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
            generation: active.generation.clone(),
            hits,
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
        let active = self.require_desktop_index()?;
        let semantic = self.require_semantic()?;
        let lexical = active.search(query, HYBRID_CANDIDATES, filters)?;
        let neighbors = semantic.search(query, HYBRID_CANDIDATES)?;

        let lexical_ids: Vec<String> = lexical.iter().map(|hit| hit.chunk_id.clone()).collect();
        let semantic_ids: Vec<String> = neighbors.into_iter().map(|hit| hit.chunk_id).collect();
        let fused = rrf_fuse_traced(&lexical_ids, &semantic_ids, RRF_K);
        let ordered: Vec<(String, f32)> = fused
            .into_iter()
            .map(|trace| (trace.chunk_id, trace.fused_score as f32))
            .collect();
        let mut hits = active.hydrate(&ordered, filters, Some(query))?;
        hits.truncate(limit);
        Ok(GenerationSearchResult {
            generation: active.generation.clone(),
            hits,
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

    fn install_desktop(&self, generation: Arc<SearchIndex>) {
        self.active
            .store(Some(Arc::new(ActiveSearchIndex::Desktop(generation))));
    }

    fn install_openclast(&self, generation: Arc<PartitionedSearchIndex>) {
        self.active
            .store(Some(Arc::new(ActiveSearchIndex::OpenClast(generation))));
    }

    pub fn install_semantic(&self, runtime: Arc<SemanticRuntime>) {
        self.semantic.store(Some(runtime));
    }
}

enum ActiveSearchIndex {
    Desktop(Arc<SearchIndex>),
    OpenClast(Arc<PartitionedSearchIndex>),
}

struct PartitionedSearchIndex {
    generation: String,
    partition_dirs: BTreeMap<ResourceKey, PathBuf>,
    /// Lazy per-resource readers. A partition is opened only after a
    /// request's authorized resource intersection selects it, then cached
    /// for the lifetime of this immutable generation; a generation swap
    /// replaces the whole value, discarding every cached reader. The
    /// authorized-only physical baseline is preserved: unauthorized
    /// partitions are never preloaded.
    readers: std::sync::Mutex<BTreeMap<ResourceKey, Arc<SearchIndex>>>,
}

impl PartitionedSearchIndex {
    fn open(active: &crate::generation::GenerationPaths) -> Result<Self> {
        let layout = GenerationLayout::load(&active.layout_path)?;
        let mut partition_dirs = BTreeMap::new();
        for partition in layout.partitions {
            let index_dir = partition_index_dir(&active.partitions_dir, &partition.resource);
            if !index_dir.join("meta.json").is_file() {
                return Err(Error::State(format!(
                    "resource partition is incomplete: {}",
                    partition.partition_id
                )));
            }
            if partition_dirs
                .insert(partition.resource.clone(), index_dir)
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
            partition_dirs,
            readers: std::sync::Mutex::new(BTreeMap::new()),
        })
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
        if let Some(reader) = readers.get(resource) {
            return Ok(reader.clone());
        }
        let partition = match SearchIndex::open(self.generation.clone(), index_dir) {
            Ok(partition) => partition,
            // The generation directory was pruned while this stale reader
            // was still installed: a typed retriable state, not an
            // internal error. The next generation swap resolves it.
            Err(_) if !index_dir.join("meta.json").is_file() => {
                return Err(Error::IndexBuilding);
            }
            Err(error) => return Err(error),
        };
        partition.source_key_field()?;
        let partition = Arc::new(partition);
        readers.insert(resource.clone(), partition.clone());
        Ok(partition)
    }

    fn search(
        &self,
        query: &str,
        limit: usize,
        filters: &SearchFilters,
        resources: &[ResourceKey],
    ) -> Result<Vec<SearchHit>> {
        let mut selected = Vec::new();
        let mut unique = BTreeSet::new();
        for resource in resources {
            if !unique.insert(resource) {
                continue;
            }
            if filters
                .vault_id
                .as_deref()
                .is_some_and(|vault_id| vault_id != resource.vault_id)
                || filters
                    .room
                    .as_deref()
                    .is_some_and(|room| room != resource.room_id)
            {
                continue;
            }
            let Some(index_dir) = self.partition_dirs.get(resource) else {
                continue;
            };
            selected.push((resource, index_dir));
        }

        let opened = selected
            .into_iter()
            .map(|(resource, index_dir)| {
                Ok((resource, self.authorized_reader(resource, index_dir)?))
            })
            .collect::<Result<Vec<_>>>()?;
        let readers = opened
            .iter()
            .map(|(resource, partition)| PartitionReader {
                index: &partition.index,
                fields: &partition.fields,
                reader: &partition.reader,
                resource,
            })
            .collect::<Vec<_>>();
        search_partitions(&readers, query, limit, filters)
    }

    #[cfg(feature = "internal-d5c-preview")]
    fn search_with_profile(
        &self,
        query: &str,
        limit: usize,
        filters: &SearchFilters,
        resources: &[ResourceKey],
        profile: &RelevanceProfile,
        query_time_epoch_seconds: u64,
    ) -> Result<Vec<SearchHit>> {
        if matches!(profile, RelevanceProfile::LexicalV1) {
            return self.search(query, limit, filters, resources);
        }

        let mut selected = Vec::new();
        let mut unique = BTreeSet::new();
        for resource in resources {
            if !unique.insert(resource) {
                continue;
            }
            if filters
                .vault_id
                .as_deref()
                .is_some_and(|vault_id| vault_id != resource.vault_id)
                || filters
                    .room
                    .as_deref()
                    .is_some_and(|room| room != resource.room_id)
            {
                continue;
            }
            let Some(index_dir) = self.partition_dirs.get(resource) else {
                continue;
            };
            selected.push((resource, index_dir));
        }

        let opened = selected
            .into_iter()
            .map(|(resource, index_dir)| {
                Ok((resource, self.authorized_reader(resource, index_dir)?))
            })
            .collect::<Result<Vec<_>>>()?;
        let readers = opened
            .iter()
            .map(|(resource, partition)| PartitionReader {
                index: &partition.index,
                fields: &partition.fields,
                reader: &partition.reader,
                resource,
            })
            .collect::<Vec<_>>();
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
}

fn observe_vault(
    vault: &crate::model::VaultRegistration,
    previous: &Manifest,
    context: &VaultObservationContext<'_>,
    audit: &mut AuditBudget,
) -> VaultObservations {
    let enumeration = discover_vault(vault);
    let mut decisions = Vec::with_capacity(enumeration.files.len());
    let mut audit_candidates = Vec::new();

    for file in &enumeration.files {
        let key = source_key(&vault.id, &file.relative_path);
        let previous_file = previous.files.get(&key);
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
    let mut reused_keys = BTreeSet::new();
    let mut source_files_read = 0;
    let mut source_bytes_read = 0;
    for (file, key, decision) in decisions {
        let read_reason = match decision {
            ObservationDecision::ReadHash(reason) => Some(reason),
            ObservationDecision::ReuseMetadata if audited.contains(&key) => Some(ReadReason::Audit),
            ObservationDecision::ReuseMetadata => None,
        };
        if read_reason.is_some() {
            let outcome = ingest_file(vault, file);
            source_files_read += 1;
            if outcome.content_hash.is_some() {
                source_bytes_read += outcome.byte_length;
            }
            outcomes.push(outcome);
        } else {
            reused_keys.insert(key);
        }
    }

    VaultObservations {
        outcomes,
        reused_keys,
        enumeration,
        source_files_read,
        source_bytes_read,
        audited_sources: audited.len(),
        audit_pending: audit_candidates.len() > audited.len(),
    }
}

fn system_time_nanos() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_nanos())
}

fn vault_read_scope(scope: &ReconcileScope, vault_id: &str) -> Option<BTreeSet<String>> {
    match scope {
        ReconcileScope::Full => None,
        ReconcileScope::Paths(paths) => Some(
            paths
                .iter()
                .filter(|(scoped_vault, _)| scoped_vault == vault_id)
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
            ReconcileScope::Paths(_) => AuditBudget::new(self.audit_cursor, 0, 0),
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
            let vault_read_scope = vault_read_scope(scope, &vault.id);
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
                },
                &mut audit,
            );
            let discovery_incomplete = !observed.enumeration.completeness.may_infer_deletions();
            warnings.extend(observed.enumeration.warnings);
            source_files_read += observed.source_files_read;
            source_bytes_read += observed.source_bytes_read;
            audited_sources += observed.audited_sources;
            audit_pending |= observed.audit_pending;
            let mut seen_keys = observed.reused_keys;

            for outcome in observed.outcomes {
                let key = source_key(&outcome.vault_id, &outcome.path);
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
    layout: GenerationLayout,
    runtime: SearchRuntime,
    manifest: Manifest,
    config: Config,
    audit_cursor: usize,
    evictions: EvictionReport,
}

impl OpenClastIndexManager {
    fn open(config: Config, data_dir: &Path, runtime: SearchRuntime) -> Result<Self> {
        let data_root = DataRoot::new(data_dir);
        let lock = data_root.acquire_writer_lock()?;
        let mut active = data_root.active()?.ok_or_else(|| {
            Error::State("IG-1 generation is missing; run `kwiry index` first".to_owned())
        })?;
        // Same mandatory open-time eviction as the desktop profile, addressed
        // per partition. `GenerationPaths::validate` has already refused any
        // OpenClast row without a resource, so every evicted key is addressable.
        let (mut manifest, evictions) = Manifest::load(&active.manifest_path)?.adopt();
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
                manifest.save(&candidate.manifest_path())?;
                Ok(())
            })();
            if let Err(error) = update_result {
                let _ = fs::remove_dir_all(staging_dir);
                return Err(error);
            }
            active = data_root.publish(candidate)?;
        } else {
            // The same repair the desktop profile applies, per partition: this
            // profile addresses deletions per resource, so an inherited
            // generation can hold them in any subset of its partitions.
            let mut inherited: Vec<ResourceKey> = Vec::new();
            for partition in GenerationLayout::load(&active.layout_path)?.partitions {
                let index_dir = partition_index_dir(&active.partitions_dir, &partition.resource);
                if index_dir.join("meta.json").is_file() && holds_resident_deletions(&index_dir)? {
                    inherited.push(partition.resource);
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
                    manifest.save(&candidate.manifest_path())?;
                    Ok(())
                })();
                if let Err(error) = repair_result {
                    let _ = fs::remove_dir_all(staging_dir);
                    return Err(error);
                }
                active = data_root.publish(candidate)?;
            }
        }
        let audit_cursor = manifest.state_revision as usize;
        let layout = GenerationLayout::load(&active.layout_path)?;
        let search = Arc::new(PartitionedSearchIndex::open(&active)?);
        runtime.set_freshness_basis(config.indexing.basis);
        runtime.install_openclast(search);
        Ok(Self {
            _lock: lock,
            data_root,
            active,
            layout,
            runtime,
            manifest,
            config,
            audit_cursor,
            evictions,
        })
    }

    fn manifest(&self) -> &Manifest {
        &self.manifest
    }

    fn config(&self) -> &Config {
        &self.config
    }

    fn reconcile(&mut self, config: Config, scope: &ReconcileScope) -> Result<ReconcileReport> {
        let mut plan = ReconcilePlan::new(&self.manifest);
        let mut next_resources: BTreeSet<_> = self
            .layout
            .partitions
            .iter()
            .map(|partition| partition.resource.clone())
            .collect();
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
            ReconcileScope::Paths(_) => AuditBudget::new(self.audit_cursor, 0, 0),
        };
        self.runtime.set_freshness_basis(config.indexing.basis);
        let configured_ids: HashSet<_> = config
            .vaults
            .iter()
            .map(|vault| vault.id.as_str())
            .collect();
        let previous_resources = resources_by_vault(&self.layout)?;
        let current_fingerprints: BTreeMap<_, _> = config
            .vaults
            .iter()
            .map(|vault| (vault.id.clone(), registration_fingerprint(vault)))
            .collect();

        let removed_keys: Vec<_> = self
            .manifest
            .files
            .iter()
            .filter(|(_, file)| !configured_ids.contains(file.vault_id.as_str()))
            .map(|(key, _)| key.clone())
            .collect();
        for key in removed_keys {
            let previous_file = &self.manifest.files[&key];
            plan.remove_manifest_source(&key);
            if let Some(resource) = previous_file.resource.clone() {
                plan.remove_index_source(key.clone(), PartitionScope::Resource(resource));
            }
            if let Some(resource) = previous_resources.get(&previous_file.vault_id).cloned() {
                plan.remove_index_source(key.clone(), PartitionScope::Resource(resource));
            }
        }
        next_resources.retain(|resource| configured_ids.contains(resource.vault_id.as_str()));

        for vault in &config.vaults {
            let resource = config.resource_key(vault).ok_or_else(|| {
                Error::State(format!(
                    "openclast vault {} is missing an exact resource classification",
                    vault.id
                ))
            })?;
            let previous_resource = previous_resources.get(&vault.id).cloned();
            let fingerprint = &current_fingerprints[&vault.id];
            let resource_reclassified = previous_resource
                .as_ref()
                .is_some_and(|previous| previous != &resource);
            let registration_reclassified = self
                .manifest
                .files
                .values()
                .filter(|file| file.vault_id == vault.id)
                .any(|file| {
                    file.registration_fingerprint != fingerprint.as_str()
                        || file.resource.as_ref() != Some(&resource)
                });
            let vault_reclassified = resource_reclassified || registration_reclassified;
            if vault_reclassified && let Some(previous) = previous_resource.as_ref() {
                next_resources.remove(previous);
            }
            if !vault.path.is_dir() {
                unavailable_vaults.push(vault.id.clone());
                for (key, file) in &self.manifest.files {
                    if file.vault_id == vault.id {
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
            next_resources.insert(resource.clone());
            let previous_keys: Vec<_> = self
                .manifest
                .files
                .iter()
                .filter(|(_, file)| file.vault_id == vault.id)
                .map(|(key, _)| key.clone())
                .collect();
            let vault_read_scope = vault_read_scope(scope, &vault.id);
            let observed = observe_vault(
                vault,
                &self.manifest,
                &VaultObservationContext {
                    registration_fingerprint: fingerprint,
                    scope: &PartitionScope::Resource(resource.clone()),
                    previous_resource: previous_resource.as_ref(),
                    policy: observation_policy,
                    semantic: None,
                    read_scope: vault_read_scope.as_ref(),
                },
                &mut audit,
            );
            let discovery_incomplete = !observed.enumeration.completeness.may_infer_deletions();
            warnings.extend(observed.enumeration.warnings);
            source_files_read += observed.source_files_read;
            source_bytes_read += observed.source_bytes_read;
            audited_sources += observed.audited_sources;
            audit_pending |= observed.audit_pending;
            let mut seen_keys = observed.reused_keys;

            for outcome in observed.outcomes {
                let key = source_key(&outcome.vault_id, &outcome.path);
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
                let Some((_, next_file)) =
                    ManifestFile::from_outcome(&outcome, fingerprint, Some(&resource))
                else {
                    continue;
                };
                seen_keys.insert(key.clone());
                let previous_file = self.manifest.files.get(&key);
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
                if discovery_incomplete {
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
        }

        let next_layout = GenerationLayout::openclast(next_resources)?;
        let layout_changed = next_layout != self.layout;
        let mut expected_source_keys = BTreeMap::<ResourceKey, BTreeSet<String>>::new();
        for (key, file) in &plan.next_manifest.files {
            let Some(resource) = file.resource.as_ref() else {
                continue;
            };
            if file.outcome == ManifestFileOutcome::Indexed
                && current_fingerprints.get(&file.vault_id) == Some(&file.registration_fingerprint)
            {
                expected_source_keys
                    .entry(resource.clone())
                    .or_default()
                    .insert(key.clone());
            }
        }
        for partition in &next_layout.partitions {
            let index_dir = partition_index_dir(&self.active.partitions_dir, &partition.resource);
            if !index_dir.join("meta.json").is_file() {
                continue;
            }
            let (index, fields) = open_index_dir(&index_dir)?;
            let source_key_field = fields.source_key.ok_or_else(|| {
                Error::Index("resource partition is missing the source_key field".to_owned())
            })?;
            let expected = expected_source_keys
                .get(&partition.resource)
                .cloned()
                .unwrap_or_default();
            for key in partition_source_keys(&index, source_key_field)? {
                if !expected.contains(&key) {
                    plan.remove_index_source(
                        key,
                        PartitionScope::Resource(partition.resource.clone()),
                    );
                }
            }
        }
        let added_chunks = plan.added_chunk_count();
        let mut resources_to_update: BTreeSet<_> = plan
            .update_scopes()
            .into_iter()
            .filter_map(|scope| match scope {
                PartitionScope::Resource(resource) => Some(resource),
                PartitionScope::Whole => None,
            })
            .collect();
        for partition in &next_layout.partitions {
            let index_dir = partition_index_dir(&self.active.partitions_dir, &partition.resource);
            if !index_dir.join("meta.json").is_file() {
                resources_to_update.insert(partition.resource.clone());
            }
        }
        plan.validate_retention()?;
        let manifest_changed = plan.next_manifest != self.manifest;
        let changed_sources = plan.changed_source_count();
        let publish_generation =
            manifest_changed || layout_changed || changed_sources > 0 || added_chunks > 0;
        if publish_generation {
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
                .map(|partition| partition.partition_id.as_str())
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

                next_layout.save(&candidate.layout_path())?;
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
            self.layout = next_layout;
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

fn resources_by_vault(layout: &GenerationLayout) -> Result<BTreeMap<String, ResourceKey>> {
    let mut resources = BTreeMap::new();
    for partition in &layout.partitions {
        let vault_id = partition.resource.vault_id.clone();
        if resources
            .insert(vault_id.clone(), partition.resource.clone())
            .is_some()
        {
            return Err(Error::State(format!(
                "generation layout contains multiple resources for vault {vault_id}"
            )));
        }
    }
    Ok(resources)
}

fn partition_source_keys(index: &Index, source_key_field: Field) -> Result<BTreeSet<String>> {
    let reader = index
        .reader()
        .map_err(|error| Error::Index(error.to_string()))?;
    let searcher = reader.searcher();
    let addresses = searcher
        .search(&AllQuery, &DocSetCollector)
        .map_err(|error| Error::Index(error.to_string()))?;
    let mut source_keys = BTreeSet::new();
    for address in addresses {
        let document = searcher
            .doc::<TantivyDocument>(address)
            .map_err(|error| Error::Index(error.to_string()))?;
        let key = document
            .get_first(source_key_field)
            .and_then(|value| value.as_str())
            .ok_or_else(|| {
                Error::Index("resource partition document is missing source_key".to_owned())
            })?;
        source_keys.insert(key.to_owned());
    }
    Ok(source_keys)
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

    #[test]
    fn a_vanished_partition_returns_a_typed_retriable_state() {
        let temporary = tempdir().unwrap();
        let vault_path = temporary.path().join("vault");
        let data = temporary.path().join("data");
        fs::create_dir(&vault_path).unwrap();
        fs::write(vault_path.join("note.md"), "vanishterm").unwrap();
        let config = openclast_config(vec![VaultRegistration {
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
        assert!(matches!(error, Error::IndexBuilding));
    }

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

    #[test]
    fn semantic_unavailable_without_model_while_lexical_serves() {
        let temporary = tempdir().unwrap();
        let vault_path = temporary.path().join("vault");
        let data_root = temporary.path().join("data");
        fs::create_dir(&vault_path).unwrap();
        fs::write(vault_path.join("note.md"), "phosphorescent").unwrap();
        let config = Config {
            vaults: vec![VaultRegistration {
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
        });
        config.vaults = vaults;
        config
    }

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
            id: "x".into(),
            path: x.clone(),
            room: Some("room-x".into()),
        };
        let config = openclast_config(vec![
            x_registration.clone(),
            VaultRegistration {
                id: "y".into(),
                path: y,
                room: Some("room-y".into()),
            },
        ]);
        build_index(&config, &data).unwrap();
        let runtime = SearchRuntime::new();
        let manager = IndexManager::open(config.clone(), &data, runtime.clone()).unwrap();
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
            id: "allowed".into(),
            path: allowed.clone(),
            room: Some("room-allowed".into()),
        };
        let config = openclast_config(vec![
            allowed_registration.clone(),
            VaultRegistration {
                id: "forbidden".into(),
                path: forbidden,
                room: Some("room-forbidden".into()),
            },
        ]);
        build_index(&config, &data).unwrap();
        let runtime = SearchRuntime::new();
        let manager = IndexManager::open(config.clone(), &data, runtime.clone()).unwrap();
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
            id: "allowed".into(),
            path: allowed.clone(),
            room: Some("room-allowed".into()),
        };
        let config = openclast_config(vec![
            allowed_registration.clone(),
            VaultRegistration {
                id: "forbidden".into(),
                path: forbidden,
                room: Some("room-forbidden".into()),
            },
        ]);
        build_index(&config, &data).unwrap();
        let runtime = SearchRuntime::new();
        let manager = IndexManager::open(config.clone(), &data, runtime.clone()).unwrap();
        let allowed_resource = config.resource_key(&config.vaults[0]).unwrap();

        let baseline = Config {
            vaults: vec![VaultRegistration {
                room: None,
                ..allowed_registration
            }],
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

    #[test]
    fn openclast_opens_only_request_authorized_partitions() {
        let temporary = tempdir().unwrap();
        let x = temporary.path().join("x");
        let y = temporary.path().join("y");
        let data = temporary.path().join("data");
        fs::create_dir(&x).unwrap();
        fs::create_dir(&y).unwrap();
        fs::write(x.join("allowed.md"), "authorized needle").unwrap();
        fs::write(y.join("forbidden.md"), "forbidden needle").unwrap();

        let config = openclast_config(vec![
            VaultRegistration {
                id: "x".into(),
                path: x,
                room: Some("room-x".into()),
            },
            VaultRegistration {
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
        assert!(
            partitions
                .search("needle", 20, &SearchFilters::default(), &[resource_y],)
                .is_err()
        );
    }

    #[cfg(unix)]
    #[test]
    fn authorized_partition_readers_are_cached_for_the_generation() {
        let temporary = tempdir().unwrap();
        let vault_path = temporary.path().join("vault");
        let data = temporary.path().join("data");
        fs::create_dir(&vault_path).unwrap();
        fs::write(vault_path.join("note.md"), "cachedneedle").unwrap();
        let config = openclast_config(vec![VaultRegistration {
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

        // Removing the on-disk partition proves reuse: a cached reader keeps
        // serving through its open handles, while a per-request reopen would
        // fail on the missing meta.json.
        fs::remove_dir_all(partition_index_dir(&active.partitions_dir, &resource)).unwrap();
        let second = partitions
            .search(
                "cachedneedle",
                20,
                &SearchFilters::default(),
                std::slice::from_ref(&resource),
            )
            .unwrap();
        assert_eq!(second, first);
    }

    #[test]
    fn openclast_reconcile_edits_renames_and_deletes_within_one_partition() {
        let temporary = tempdir().unwrap();
        let vault_path = temporary.path().join("vault");
        let data_root = temporary.path().join("data");
        fs::create_dir(&vault_path).unwrap();
        let note = vault_path.join("note.md");
        fs::write(&note, "# One\noldterm").unwrap();
        let config = openclast_config(vec![VaultRegistration {
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

    #[test]
    fn openclast_restart_reclassifies_vault_from_persisted_layout() {
        let temporary = tempdir().unwrap();
        let vault_path = temporary.path().join("vault");
        let data_root = temporary.path().join("data");
        fs::create_dir(&vault_path).unwrap();
        fs::write(vault_path.join("note.md"), "reclassifiedterm").unwrap();
        let old_config = openclast_config(vec![VaultRegistration {
            id: "fixture".into(),
            path: vault_path.clone(),
            room: Some("room-a".into()),
        }]);
        build_index(&old_config, &data_root).unwrap();
        let old_resource = old_config.resource_key(&old_config.vaults[0]).unwrap();

        let new_config = openclast_config(vec![VaultRegistration {
            id: "fixture".into(),
            path: vault_path,
            room: Some("room-b".into()),
        }]);
        let new_resource = new_config.resource_key(&new_config.vaults[0]).unwrap();
        let runtime = SearchRuntime::new();
        let mut manager =
            IndexManager::open(new_config.clone(), &data_root, runtime.clone()).unwrap();
        manager.reconcile(new_config).unwrap();

        assert!(
            runtime
                .search_authorized(
                    "reclassifiedterm",
                    20,
                    &SearchFilters::default(),
                    &[old_resource],
                )
                .unwrap()
                .is_empty()
        );
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

    #[test]
    fn openclast_restart_reclassifies_tenant_from_persisted_layout() {
        let temporary = tempdir().unwrap();
        let vault_path = temporary.path().join("vault");
        let data_root = temporary.path().join("data");
        fs::create_dir(&vault_path).unwrap();
        fs::write(vault_path.join("note.md"), "tenantterm").unwrap();
        let old_config = openclast_config(vec![VaultRegistration {
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

        assert!(
            runtime
                .search_authorized("tenantterm", 20, &SearchFilters::default(), &[old_resource],)
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            runtime
                .search_authorized("tenantterm", 20, &SearchFilters::default(), &[new_resource],)
                .unwrap()
                .len(),
            1
        );
        manager.shutdown().unwrap();
    }

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
        assert!(
            runtime
                .search_authorized(
                    "retryterm",
                    20,
                    &SearchFilters::default(),
                    std::slice::from_ref(&new_resource),
                )
                .unwrap()
                .is_empty()
        );
        for query in ["duplicateterm", "deletedterm"] {
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

        manager.reconcile(new_config.clone()).unwrap();
        for query in ["retryterm", "duplicateterm", "deletedterm"] {
            assert!(
                runtime
                    .search_authorized(
                        query,
                        20,
                        &SearchFilters::default(),
                        std::slice::from_ref(&old_resource),
                    )
                    .unwrap()
                    .is_empty()
            );
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
        assert!(
            runtime
                .search_authorized(
                    "deletedterm",
                    20,
                    &SearchFilters::default(),
                    std::slice::from_ref(&new_resource),
                )
                .unwrap()
                .is_empty()
        );
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
        let manager = IndexManager::open(new_config, &data_root, runtime.clone()).unwrap();
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
        assert!(
            runtime
                .search_authorized(
                    "deletedterm",
                    20,
                    &SearchFilters::default(),
                    &[new_resource],
                )
                .unwrap()
                .is_empty()
        );
        manager.shutdown().unwrap();
    }

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
        assert_eq!(
            runtime
                .search_authorized(
                    "unpublishedterm",
                    20,
                    &SearchFilters::default(),
                    std::slice::from_ref(&resource),
                )
                .unwrap()
                .len(),
            1
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
        let manager = IndexManager::open(new_config, &data_root, runtime.clone()).unwrap();
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

    #[test]
    fn openclast_reclassification_withholds_unavailable_content_until_reindexed() {
        let temporary = tempdir().unwrap();
        let vault_path = temporary.path().join("vault");
        let offline_path = temporary.path().join("vault-offline");
        let data_root = temporary.path().join("data");
        fs::create_dir(&vault_path).unwrap();
        fs::write(vault_path.join("note.md"), "withheldterm").unwrap();
        let old_config = openclast_config(vec![VaultRegistration {
            id: "fixture".into(),
            path: vault_path.clone(),
            room: Some("room-a".into()),
        }]);
        build_index(&old_config, &data_root).unwrap();
        let old_resource = old_config.resource_key(&old_config.vaults[0]).unwrap();

        let new_config = openclast_config(vec![VaultRegistration {
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
        assert_eq!(
            runtime
                .search_authorized(
                    "withheldterm",
                    20,
                    &SearchFilters::default(),
                    std::slice::from_ref(&new_resource),
                )
                .unwrap()
                .len(),
            1
        );
        let report = manager.reconcile(new_config.clone()).unwrap();

        assert_eq!(report.unavailable_vaults, ["fixture"]);
        assert!(
            runtime
                .search_authorized(
                    "withheldterm",
                    20,
                    &SearchFilters::default(),
                    &[old_resource],
                )
                .unwrap()
                .is_empty()
        );
        assert!(
            runtime
                .search_authorized(
                    "withheldterm",
                    20,
                    &SearchFilters::default(),
                    std::slice::from_ref(&new_resource),
                )
                .unwrap()
                .is_empty()
        );

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

    #[test]
    fn openclast_path_change_withholds_unavailable_previous_content() {
        let temporary = tempdir().unwrap();
        let old_path = temporary.path().join("old-vault");
        let new_path = temporary.path().join("new-vault");
        let data_root = temporary.path().join("data");
        fs::create_dir(&old_path).unwrap();
        fs::write(old_path.join("note.md"), "oldpathterm").unwrap();
        let old_config = openclast_config(vec![VaultRegistration {
            id: "fixture".into(),
            path: old_path,
            room: Some("room-a".into()),
        }]);
        build_index(&old_config, &data_root).unwrap();
        let resource = old_config.resource_key(&old_config.vaults[0]).unwrap();

        let new_config = openclast_config(vec![VaultRegistration {
            id: "fixture".into(),
            path: new_path.clone(),
            room: Some("room-a".into()),
        }]);
        let runtime = SearchRuntime::new();
        let mut manager =
            IndexManager::open(new_config.clone(), &data_root, runtime.clone()).unwrap();
        let report = manager.reconcile(new_config.clone()).unwrap();
        assert_eq!(report.unavailable_vaults, ["fixture"]);
        assert!(
            runtime
                .search_authorized(
                    "oldpathterm",
                    20,
                    &SearchFilters::default(),
                    std::slice::from_ref(&resource),
                )
                .unwrap()
                .is_empty()
        );
        manager.shutdown().unwrap();

        let runtime = SearchRuntime::new();
        let mut manager =
            IndexManager::open(new_config.clone(), &data_root, runtime.clone()).unwrap();
        assert!(
            runtime
                .search_authorized(
                    "oldpathterm",
                    20,
                    &SearchFilters::default(),
                    std::slice::from_ref(&resource),
                )
                .unwrap()
                .is_empty()
        );
        fs::create_dir(&new_path).unwrap();
        fs::write(new_path.join("note.md"), "newpathterm").unwrap();
        manager.reconcile(new_config).unwrap();
        assert!(
            runtime
                .search_authorized(
                    "oldpathterm",
                    20,
                    &SearchFilters::default(),
                    std::slice::from_ref(&resource),
                )
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            runtime
                .search_authorized("newpathterm", 20, &SearchFilters::default(), &[resource],)
                .unwrap()
                .len(),
            1
        );
        manager.shutdown().unwrap();
    }

    #[test]
    fn openclast_unregisters_a_vault_after_its_content_was_withheld() {
        let temporary = tempdir().unwrap();
        let vault_path = temporary.path().join("vault");
        let offline_path = temporary.path().join("vault-offline");
        let data_root = temporary.path().join("data");
        fs::create_dir(&vault_path).unwrap();
        fs::write(vault_path.join("note.md"), "removedwithheldterm").unwrap();
        let old_config = openclast_config(vec![VaultRegistration {
            id: "fixture".into(),
            path: vault_path.clone(),
            room: Some("room-a".into()),
        }]);
        build_index(&old_config, &data_root).unwrap();
        let old_resource = old_config.resource_key(&old_config.vaults[0]).unwrap();
        fs::rename(&vault_path, &offline_path).unwrap();

        let reclassified_config = openclast_config(vec![VaultRegistration {
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
            assert!(
                runtime
                    .search_authorized(
                        "removedwithheldterm",
                        20,
                        &SearchFilters::default(),
                        std::slice::from_ref(resource),
                    )
                    .unwrap()
                    .is_empty()
            );
        }
        manager.shutdown().unwrap();

        let runtime = SearchRuntime::new();
        let mut manager =
            IndexManager::open(removed_config.clone(), &data_root, runtime.clone()).unwrap();
        manager.reconcile(removed_config).unwrap();
        assert!(manager.manifest().files.is_empty());
        for resource in [old_resource, new_resource] {
            assert!(
                runtime
                    .search_authorized(
                        "removedwithheldterm",
                        20,
                        &SearchFilters::default(),
                        &[resource],
                    )
                    .unwrap()
                    .is_empty()
            );
        }
        manager.shutdown().unwrap();
    }

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
                id: "x".into(),
                path: x.clone(),
                room: Some("room-x".into()),
            },
            VaultRegistration {
                id: "y".into(),
                path: y,
                room: Some("room-y".into()),
            },
        ]);
        build_index(&old_config, &data_root).unwrap();
        let removed_resource = old_config.resource_key(&old_config.vaults[1]).unwrap();

        let new_config = openclast_config(vec![VaultRegistration {
            id: "x".into(),
            path: x,
            room: Some("room-x".into()),
        }]);
        let runtime = SearchRuntime::new();
        let mut manager =
            IndexManager::open(new_config.clone(), &data_root, runtime.clone()).unwrap();
        manager.reconcile(new_config).unwrap();

        assert!(
            runtime
                .search_authorized(
                    "removedterm",
                    20,
                    &SearchFilters::default(),
                    &[removed_resource],
                )
                .unwrap()
                .is_empty()
        );
        manager.shutdown().unwrap();
    }

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
                id: "fixture".into(),
                path: vault_path.clone(),
                room: None,
            }],
            indexing: crate::model::IndexingConfig {
                basis: IndexFreshnessBasis::MetadataAudit,
                audit_sources_per_pass: 0,
                audit_bytes_per_pass: 0,
                racy_window_millis: 0,
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
    #[test]
    fn openclast_evicts_a_stale_format_from_its_partition_before_serving() {
        let temporary = tempdir().unwrap();
        let vault_path = temporary.path().join("vault");
        let data_root = temporary.path().join("data");
        fs::create_dir(&vault_path).unwrap();
        fs::write(vault_path.join("note.md"), "# Note\nmarkdownterm").unwrap();
        fs::write(vault_path.join("log.txt"), "plainterm").unwrap();
        let config = openclast_config(vec![VaultRegistration {
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
        let search = |query: &str| {
            runtime
                .search_authorized(
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

        let key = source_key("fixture", "log.txt");
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
}
