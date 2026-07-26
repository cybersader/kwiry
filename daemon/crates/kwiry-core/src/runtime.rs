use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use arc_swap::ArcSwapOption;
#[cfg(test)]
use tantivy::IndexWriter;
use tantivy::collector::DocSetCollector;
use tantivy::query::AllQuery;
use tantivy::schema::{Field, Value};
use tantivy::{Index, IndexReader, ReloadPolicy, TantivyDocument, Term};

use crate::api::SearchFilters;
use crate::chunk::ingest_file;
use crate::error::{Error, Result};
use crate::generation::{DataRoot, DataRootLock};
use crate::index::{Fields, build_schema, chunk_document, open_index_dir};
use crate::manifest::{
    Manifest, ManifestFile, ManifestFileOutcome, registration_fingerprint, source_key,
};
use crate::model::{
    Config, FileIngestOutcome, FileOutcomeKind, HostProfile, IndexFreshnessBasis, IngestWarning,
    LexicalSearchRequest, PreparedChunk, ResourceKey, RetrievalMetadata, SearchHit,
};
use crate::partition::{GenerationLayout, partition_index_dir};
use crate::reconcile::{
    AuditBudget, ObservationDecision, ObservationPolicy, PartitionScope, ReadReason, ReconcilePlan,
    ReconcileScope, RetentionReason, SourceSignals, plan_observation,
};
use crate::search::{PartitionReader, search_partitions, search_reader};
use crate::semantic::{SemanticRuntime, embedding_text, rrf_fuse_traced};
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
                    runtime.source_hash(&key).ok().flatten().as_deref()
                        != Some(previous_file.content_hash.as_str())
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
}

impl DesktopIndexManager {
    fn open(config: Config, data_dir: &Path, runtime: SearchRuntime) -> Result<Self> {
        let data_root = DataRoot::new(data_dir);
        let lock = data_root.acquire_writer_lock()?;
        let active = data_root.active()?.ok_or_else(|| {
            Error::State("Vertical 2 generation is missing; run `kwiry index` first".to_owned())
        })?;
        let manifest = Manifest::load(&active.manifest_path)?;
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
                            next_file.content_hash.clone(),
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
}

impl OpenClastIndexManager {
    fn open(config: Config, data_dir: &Path, runtime: SearchRuntime) -> Result<Self> {
        let data_root = DataRoot::new(data_dir);
        let lock = data_root.acquire_writer_lock()?;
        let active = data_root.active()?.ok_or_else(|| {
            Error::State("IG-1 generation is missing; run `kwiry index` first".to_owned())
        })?;
        let manifest = Manifest::load(&active.manifest_path)?;
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
    Ok(())
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
}
