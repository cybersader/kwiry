use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::Path;

#[cfg(feature = "internal-d5c-preview")]
use tantivy::collector::DocSetCollector;
use tantivy::collector::{Collector, Count, SegmentCollector, TopDocs};
use tantivy::fieldnorm::FieldNormReader;
#[cfg(feature = "internal-d5c-preview")]
use tantivy::query::TermSetQuery;
use tantivy::query::{
    Bm25StatisticsProvider, BooleanQuery, BoostQuery, ConstScoreQuery, DisjunctionMaxQuery, Occur,
    PhraseQuery, Query, QueryParser, RegexQuery, TermQuery,
};
use tantivy::schema::{Field, IndexRecordOption, TantivyDocument, Value};
use tantivy::snippet::SnippetGenerator;
use tantivy::store::StoreReader;
use tantivy::tokenizer::TokenStream;
use tantivy::{DocId, Index, IndexReader, Score, Searcher, SegmentOrdinal, SegmentReader, Term};

use crate::api::SearchFilters;
use crate::error::{Error, Result};
use crate::format::SourceFormat;
use crate::index::{Fields, open_index};
#[cfg(feature = "internal-d5c-preview")]
use crate::index::{
    property_date_term, property_exact_term, property_f64_term, property_i64_term,
    property_name_term, property_path_date_term, property_path_exact_term, property_path_f64_term,
    property_path_i64_term, property_path_u64_term, property_u64_term,
};
#[cfg(feature = "internal-d5c-preview")]
use crate::model::PropertyValue;
use crate::model::{LexicalSearchRequest, ResourceKey, SearchHit};
#[cfg(test)]
use crate::query::classify_query;
use crate::query::{
    LEXICAL_QUERY_PLAN_SCHEMA_VERSION, LexicalQueryPlan, QueryAssistanceEligibility,
    QueryEvidenceReport, QueryEvidenceStage, QueryEvidenceStageKind, QueryExecutionDisposition,
    QueryField, QueryFieldGroup, QueryMatchOperator, QueryMetadataField, QueryMetadataProbe,
    QueryPlanKind, QueryTermProjection, QueryTermRole, QueryTermSupportObservation,
    prepare_lexical_query,
};
#[cfg(feature = "internal-d5c-preview")]
use crate::ranking::{
    D5cRelevanceProfile, LexicalEvidenceTier, MAX_RANKING_WORK_UNITS, PropertyPredicate,
    PropertyRule, QualifiedSourceId, RERANK_INPUT_SCHEMA_VERSION, RankingScalar, RelevanceProfile,
    RerankCandidate, RerankInput, SourceSignalObservation, rerank_candidates_with_initial_work,
};
use crate::source::excel_content_role_from_chunk_id;

const MAX_RESULTS: usize = 100;
const BOOST_FILENAME: f32 = 5.0;
const BOOST_STEM: f32 = 6.0;
const BOOST_ALIAS: f32 = 6.0;
const BOOST_TITLE: f32 = 6.0;
const BOOST_HEADING: f32 = 3.0;
const BOOST_TAGS: f32 = 2.0;
const BOOST_CONTENT: f32 = 1.0;
const BOOST_EXACT_METADATA: f32 = 12.0;
const BOOST_PHRASE: f32 = 4.0;
const BOOST_CONTENT_IDENTIFIER: f32 = 5.0;
#[cfg(feature = "internal-d5c-preview")]
const DESKTOP_AUTHORIZATION_SCOPE: &str = "desktop";

pub fn search_index(data_dir: &Path, request: &LexicalSearchRequest) -> Result<Vec<SearchHit>> {
    let (index, fields) = open_index(data_dir)?;
    let reader = index
        .reader()
        .map_err(|error| Error::Index(error.to_string()))?;
    let filters = SearchFilters {
        vault_id: request.vault_id.clone(),
        ..SearchFilters::default()
    };
    search_reader(
        &index,
        &fields,
        &reader,
        &request.query,
        request.limit.clamp(1, MAX_RESULTS),
        &filters,
    )
}

pub(crate) fn search_reader(
    index: &Index,
    fields: &Fields,
    reader: &IndexReader,
    query_text: &str,
    limit: usize,
    filters: &SearchFilters,
) -> Result<Vec<SearchHit>> {
    if query_text.trim().is_empty() {
        return Err(Error::Query("query must not be empty".into()));
    }

    let searcher = reader.searcher();
    let context = NativeSearchContext {
        index,
        fields,
        searcher: &searcher,
        resource: None,
    };
    let resolved = resolve_query_plan(std::slice::from_ref(&context), query_text)?;
    let statistics = AuthorizedStatistics::new(vec![searcher.clone()]);
    execute_lexical_plan(
        std::slice::from_ref(&context),
        &resolved,
        limit.min(MAX_RESULTS),
        filters,
        &statistics,
    )
}

#[cfg(feature = "internal-d5c-preview")]
pub(crate) struct ProfileExecution<'a> {
    pub profile: &'a RelevanceProfile,
    pub query_time_epoch_seconds: u64,
}

#[cfg(feature = "internal-d5c-preview")]
pub(crate) fn search_reader_with_profile(
    index: &Index,
    fields: &Fields,
    reader: &IndexReader,
    query_text: &str,
    limit: usize,
    filters: &SearchFilters,
    execution: ProfileExecution<'_>,
) -> Result<Vec<SearchHit>> {
    if matches!(execution.profile, RelevanceProfile::LexicalV1) {
        return search_reader(index, fields, reader, query_text, limit, filters);
    }
    validate_d5c_profile(execution.profile)?;
    if query_text.trim().is_empty() {
        return Err(Error::Query("query must not be empty".into()));
    }

    let searcher = reader.searcher();
    let context = NativeSearchContext {
        index,
        fields,
        searcher: &searcher,
        resource: None,
    };
    let resolved = resolve_query_plan(std::slice::from_ref(&context), query_text)?;
    let statistics = AuthorizedStatistics::new(vec![searcher.clone()]);
    execute_d5c_profile(
        std::slice::from_ref(&context),
        &resolved,
        limit.min(MAX_RESULTS),
        filters,
        &statistics,
        execution.profile,
        execution.query_time_epoch_seconds,
    )
}

pub(crate) struct PartitionReader<'a> {
    pub index: &'a Index,
    pub fields: &'a Fields,
    pub reader: &'a IndexReader,
    pub resource: &'a ResourceKey,
}

#[derive(Clone)]
struct NativeSearchContext<'a> {
    index: &'a Index,
    fields: &'a Fields,
    searcher: &'a Searcher,
    resource: Option<&'a ResourceKey>,
}

struct ResolvedLexicalPlan {
    plan: LexicalQueryPlan,
    prefix_expansions: BTreeMap<u16, Vec<String>>,
}

/// The collection length BM25 divides by, summed from the live documents
/// themselves rather than read off a segment header.
///
/// The header value (`InvertedIndexReader::total_num_tokens`) is written once,
/// when a segment is created, and is *not* a function of the segment's live
/// content afterwards. Tantivy rewrites it on merge, and when a merged segment
/// carries deletions it cannot recover the exact figure: `merger.rs`'s
/// `estimate_total_num_tokens_in_single_segment` re-derives the total by
/// summing quantised fieldnorms over the alive documents, which
/// `fieldnorm::code` rounds down for every length above 40. `runtime`'s
/// `expunge_resident_deletions` merges exactly the segments carrying deletions,
/// so it takes that lossy branch by construction and bakes an under-count into
/// the segment it publishes. A rebuild of byte-identical content writes exact
/// headers, so the two build paths would report different collection lengths
/// for the same vault — a different `avgdl`, a different score for every
/// document, and, because each evidence stage of the ladder is a bounded
/// top-k, a different *answer set* and not merely a different order.
///
/// Summing `id_to_fieldnorm(fieldnorm_id(doc))` over the live documents closes
/// that gap exactly, and is the only formulation that can. A document's
/// fieldnorm id is derived from its own content and is copied verbatim through
/// every merge (`merger.rs::write_fieldnorms` pushes the id through
/// unchanged), so this sum depends on the set of live documents and nothing
/// else — identical for a rebuild and for any incremental history that arrives
/// at the same vault. It is also the length the scorer actually uses:
/// `Bm25Weight` divides each document's contribution by its quantised
/// fieldnorm, so an exact header made `avgdl` disagree with the very lengths
/// it normalises. Correctness here is measured against reproducibility, not
/// against the untruncated token count.
fn live_collection_length(searchers: &[Searcher], field: Field) -> tantivy::Result<u64> {
    let mut total = 0_u64;
    for searcher in searchers {
        for segment in searcher.segment_readers() {
            let Some(fieldnorms) = segment.fieldnorms_readers().get_field(field)? else {
                // A field indexed without fieldnorms has no per-document
                // length for this figure to be the mean of, and tantivy scores
                // it without BM25 length normalisation. Nothing else can be
                // derived from live content, so report the header.
                total += segment.inverted_index(field)?.total_num_tokens();
                continue;
            };
            let mut histogram = [0_u64; 256];
            for doc in segment.doc_ids_alive() {
                histogram[usize::from(fieldnorms.fieldnorm_id(doc))] += 1;
            }
            for (id, count) in histogram.iter().enumerate() {
                total += count * u64::from(FieldNormReader::id_to_fieldnorm(id as u8));
            }
        }
    }
    Ok(total)
}

/// BM25 corpus statistics summed over exactly the partitions a request is
/// authorized to read, so an unauthorized partition cannot influence a score.
///
/// Every statistic here must be drawn from the same view of the segments.
/// `doc_freq` is read off the term dictionary and so counts any document still
/// resident in the segment files; `Searcher::num_docs` counts only the live
/// ones. Mixing the two is not a smaller error than using either — it can make
/// `doc_freq` exceed the document count, and `Bm25Weight::idf` asserts against
/// exactly that in release builds. `runtime`'s publication invariant means a
/// served index holds no dead documents, so the two agree; summing `max_doc`
/// keeps them agreeing even if that invariant is ever weakened, and matches
/// how tantivy's own `Searcher` answers. The collection length is the one
/// statistic that cannot be made history-independent by removing dead
/// documents, so it is derived from live fieldnorms instead — see
/// `live_collection_length`.
///
/// The collection length is memoised per field. It is a scan of the live
/// documents, and a single request builds a `Bm25Weight` for every term in
/// every one of the seven boosted fields at every stage of the ladder, so
/// without the cache one query would rescan the index dozens of times.
struct AuthorizedStatistics {
    searchers: Vec<Searcher>,
    collection_lengths: std::cell::RefCell<HashMap<Field, u64>>,
}

impl AuthorizedStatistics {
    fn new(searchers: Vec<Searcher>) -> Self {
        Self {
            searchers,
            collection_lengths: std::cell::RefCell::new(HashMap::new()),
        }
    }
}

impl Bm25StatisticsProvider for AuthorizedStatistics {
    fn total_num_tokens(&self, field: Field) -> tantivy::Result<u64> {
        if let Some(cached) = self.collection_lengths.borrow().get(&field) {
            return Ok(*cached);
        }
        let total = live_collection_length(&self.searchers, field)?;
        self.collection_lengths.borrow_mut().insert(field, total);
        Ok(total)
    }

    fn total_num_docs(&self) -> tantivy::Result<u64> {
        Ok(self
            .searchers
            .iter()
            .flat_map(Searcher::segment_readers)
            .map(|segment| u64::from(segment.max_doc()))
            .sum())
    }

    fn doc_freq(&self, term: &Term) -> tantivy::Result<u64> {
        self.searchers.iter().try_fold(
            0_u64,
            |total, searcher| Ok(total + searcher.doc_freq(term)?),
        )
    }
}

pub(crate) fn search_partitions(
    partitions: &[PartitionReader<'_>],
    query_text: &str,
    limit: usize,
    filters: &SearchFilters,
) -> Result<Vec<SearchHit>> {
    if query_text.trim().is_empty() {
        return Err(Error::Query("query must not be empty".into()));
    }
    if partitions.is_empty() {
        return Ok(Vec::new());
    }

    let mut ordered_partitions: Vec<_> = partitions.iter().collect();
    ordered_partitions.sort_by(|left, right| left.resource.cmp(right.resource));
    let searchers: Vec<_> = ordered_partitions
        .iter()
        .map(|partition| partition.reader.searcher())
        .collect();
    let contexts: Vec<_> = ordered_partitions
        .iter()
        .zip(&searchers)
        .map(|(partition, searcher)| NativeSearchContext {
            index: partition.index,
            fields: partition.fields,
            searcher,
            resource: Some(partition.resource),
        })
        .collect();
    let statistics = AuthorizedStatistics::new(searchers.clone());
    let resolved = resolve_query_plan(&contexts, query_text)?;
    execute_lexical_plan(
        &contexts,
        &resolved,
        limit.min(MAX_RESULTS),
        filters,
        &statistics,
    )
}

#[cfg(feature = "internal-d5c-preview")]
pub(crate) fn search_partitions_with_profile(
    partitions: &[PartitionReader<'_>],
    query_text: &str,
    limit: usize,
    filters: &SearchFilters,
    execution: ProfileExecution<'_>,
) -> Result<Vec<SearchHit>> {
    if matches!(execution.profile, RelevanceProfile::LexicalV1) {
        return search_partitions(partitions, query_text, limit, filters);
    }
    validate_d5c_profile(execution.profile)?;
    if query_text.trim().is_empty() {
        return Err(Error::Query("query must not be empty".into()));
    }
    if partitions.is_empty() {
        return Ok(Vec::new());
    }

    let mut ordered_partitions: Vec<_> = partitions.iter().collect();
    ordered_partitions.sort_by(|left, right| left.resource.cmp(right.resource));
    let searchers: Vec<_> = ordered_partitions
        .iter()
        .map(|partition| partition.reader.searcher())
        .collect();
    let contexts: Vec<_> = ordered_partitions
        .iter()
        .zip(&searchers)
        .map(|(partition, searcher)| NativeSearchContext {
            index: partition.index,
            fields: partition.fields,
            searcher,
            resource: Some(partition.resource),
        })
        .collect();
    let statistics = AuthorizedStatistics::new(searchers.clone());
    let resolved = resolve_query_plan(&contexts, query_text)?;
    execute_d5c_profile(
        &contexts,
        &resolved,
        limit.min(MAX_RESULTS),
        filters,
        &statistics,
        execution.profile,
        execution.query_time_epoch_seconds,
    )
}

fn resolve_query_plan(
    contexts: &[NativeSearchContext<'_>],
    query: &str,
) -> Result<ResolvedLexicalPlan> {
    let plan = prepare_lexical_query(query).map_err(|error| Error::Query(error.to_string()))?;
    plan.validate()
        .map_err(|error| Error::Query(error.to_string()))?;

    if matches!(
        plan.execution,
        QueryExecutionDisposition::ExplicitBypass | QueryExecutionDisposition::EmptyNoEvidence
    ) {
        return Ok(ResolvedLexicalPlan {
            plan,
            prefix_expansions: BTreeMap::new(),
        });
    }

    let identifier_probe_matched = plan
        .metadata_probe
        .as_ref()
        .map(|probe| metadata_probe_matches(contexts, probe))
        .transpose()?;
    let mut observations = Vec::with_capacity(plan.support_probes.len());
    let mut prefix_expansions = BTreeMap::new();
    let mut prefix_terms_examined = 0_usize;

    for probe in &plan.support_probes {
        let document_frequency = support_document_frequency(contexts, &plan, probe)?;
        // Prefix assistance used to require `document_frequency == 0`. That
        // asked whether the typed stem exists *anywhere* in the vault, not
        // whether it is useful for this query: one unrelated note holding a
        // bare `vuln` made `vuln` "supported", so no `vuln…` expansions were
        // collected and a note titled `Vulnerability-Meetings` became
        // unreachable. Eligibility is a property of the term's role and
        // length, not of some other document. The stem's own token is itself
        // an expansion of the stem, so an exact match stays available as one
        // alternative rather than as a precondition.
        let eligible = plan
            .term_intents
            .get(probe.term_index as usize)
            .is_some_and(|intent| {
                intent.role == QueryTermRole::OptionalContext
                    && intent.projection == QueryTermProjection::AnalyzedText
            });
        let expansions = if eligible
            && probe.term.chars().count() >= plan.bounds.min_prefix_chars
            && prefix_terms_examined < plan.bounds.max_prefix_terms
        {
            prefix_terms_examined += 1;
            collect_prefix_expansions(
                contexts,
                &plan,
                &probe.term,
                plan.bounds.max_prefix_expansions_per_term,
                plan.bounds.max_prefix_expansion_scan,
            )?
        } else {
            Vec::new()
        };
        if !expansions.is_empty() {
            prefix_expansions.insert(probe.term_index, expansions.clone());
        }
        observations.push(QueryTermSupportObservation {
            probe_id: probe.probe_id,
            term_index: probe.term_index,
            document_frequency,
            prefix_expansions: expansions.len(),
        });
    }

    let report = QueryEvidenceReport {
        schema_version: LEXICAL_QUERY_PLAN_SCHEMA_VERSION,
        identifier_probe_matched,
        term_support: observations,
    };
    let plan = plan
        .finalize_evidence(report)
        .map_err(|error| Error::Query(error.to_string()))?;
    prefix_expansions.retain(|term_index, _| {
        plan.evidence_stages
            .iter()
            .any(|stage| stage.prefix_term_indexes.contains(term_index))
    });
    Ok(ResolvedLexicalPlan {
        plan,
        prefix_expansions,
    })
}

fn metadata_probe_matches(
    contexts: &[NativeSearchContext<'_>],
    probe: &QueryMetadataProbe,
) -> Result<bool> {
    for context in contexts {
        let mut parser =
            QueryParser::for_index(context.index, metadata_probe_fields(context.fields, probe));
        if probe.conjunction {
            parser.set_conjunction_by_default();
        }
        let query = parser
            .parse_query(&probe.query)
            .map_err(|error| Error::Query(error.to_string()))?;
        let matches = context
            .searcher
            .search(&query, &TopDocs::with_limit(1).order_by_score())
            .map_err(|error| Error::Index(error.to_string()))?;
        if !matches.is_empty() {
            return Ok(true);
        }
    }
    Ok(false)
}

fn support_document_frequency(
    contexts: &[NativeSearchContext<'_>],
    plan: &LexicalQueryPlan,
    probe: &crate::query::QueryTermSupportProbe,
) -> Result<u64> {
    let mut total = 0_u64;
    let exact_identifier_anchor = plan
        .term_intents
        .get(probe.term_index as usize)
        .is_some_and(|intent| intent.projection == QueryTermProjection::ExactIdentifier);
    for context in contexts {
        let query = if exact_identifier_anchor {
            let Some(query) = identifier_anchor_query(context.fields, plan, &probe.term)? else {
                continue;
            };
            query
        } else {
            let Some(query) = term_query_for_group(
                context.index,
                context.fields,
                plan,
                probe.field_group,
                &probe.term,
            )?
            else {
                continue;
            };
            query
        };
        let count = context
            .searcher
            .search(query.as_ref(), &Count)
            .map_err(|error| Error::Index(error.to_string()))?;
        total = total.saturating_add(count as u64);
    }
    Ok(total)
}

/// The `limit` most useful distinct terms extending `term`, selected from a
/// bounded lexicographic window over the whole authorized vocabulary.
///
/// This reads the term dictionary directly rather than counting matches, so it
/// sees every term a segment records, live or not. That is only safe because
/// `runtime` publishes no index holding deleted documents: were dead documents
/// left resident, terms that exist nowhere in the vault would sort ahead of a
/// live one and could crowd it out of the expansion set, and the typo ladder
/// would silently fail to match a note that is really there.
///
/// The scan budget is spent on *distinct terms*, not on dictionary entries
/// visited, and each segment is streamed independently up to the same bound.
/// That distinction is what makes the result a function of the vault rather
/// than of the index's segment layout. Charging every visited entry against one
/// shared counter — as this did before — makes a term that appears in three
/// segments cost three times what the same term costs in a freshly merged
/// index, so an incrementally maintained index and a rebuild of byte-identical
/// content expand the same prefix to different term sets, and the prefix stages
/// of the ladder then contribute different candidates to a bounded pool. See
/// `runtime::tests::an_incremental_index_and_a_rebuild_rank_a_vault_identically`.
///
/// Taking the first `scan` from each stream and then the first `scan` of their
/// union yields exactly the globally first `scan`: if a term is among the
/// globally first `scan`, then within any single segment fewer than `scan`
/// prefix terms can precede it, because every one of those also precedes it
/// globally. The work is bounded by `scan` entries per segment per field, and
/// `scan` is `MAX_PREFIX_EXPANSION_SCAN`.
///
/// Selection then keeps `limit` of that window. Ordering it lexicographically —
/// as this did before, with `scan` equal to `limit` — spends the whole budget on
/// an arbitrary alphabetical slice, so a stem like `adop` never reaches
/// `adoption` in a vault that also holds `adoptable`, `adopted`, `adoptee`, and
/// their plurals, and no later stage can rank a term that was never a
/// candidate. Preferring terms that occur in a metadata field, then the terms
/// closest in length to what was typed, keeps the expansions a person is
/// actually navigating toward. Both orderings are total and computed from the
/// window alone, so the result stays a pure function of the vault.
fn collect_prefix_expansions(
    contexts: &[NativeSearchContext<'_>],
    plan: &LexicalQueryPlan,
    term: &str,
    limit: usize,
    scan: usize,
) -> Result<Vec<String>> {
    if limit == 0 || scan == 0 || contexts.is_empty() {
        return Ok(Vec::new());
    }
    let fields = field_bindings(
        contexts[0].fields,
        declared_fields(plan, QueryFieldGroup::Prefix),
        QueryFieldGroup::Prefix,
    );
    let Some((analysis_field, _)) = fields.first().copied() else {
        return Err(Error::Query("prefix field group is empty".to_owned()));
    };
    let tokens = analyze_text(contexts[0].index, analysis_field, term)?;
    if tokens.len() != 1 {
        return Ok(Vec::new());
    }
    let prefix = tokens[0].as_bytes();
    // `true` once the term was seen in a prefix-metadata field. The map is
    // ordered, so both the window truncation and the selection sort below are
    // deterministic regardless of the order fields and segments are visited in.
    let mut window: BTreeMap<String, bool> = BTreeMap::new();

    for context in contexts {
        let metadata_declared = declared_fields(plan, QueryFieldGroup::PrefixMetadata);
        let metadata_fields: BTreeSet<_> =
            field_bindings(context.fields, metadata_declared, QueryFieldGroup::Prefix)
                .into_iter()
                .map(|(field, _)| field)
                .collect();
        let bindings = field_bindings(
            context.fields,
            declared_fields(plan, QueryFieldGroup::Prefix),
            QueryFieldGroup::Prefix,
        );
        for (field, _) in bindings {
            let is_metadata = metadata_fields.contains(&field);
            for segment in context.searcher.segment_readers() {
                let inverted = segment
                    .inverted_index(field)
                    .map_err(|error| Error::Index(error.to_string()))?;
                let mut stream = inverted
                    .terms()
                    .range()
                    .ge(prefix)
                    .into_stream()
                    .map_err(|error| Error::Index(error.to_string()))?;
                let mut taken = 0_usize;
                while taken < scan && stream.advance() {
                    let key = stream.key();
                    if !key.starts_with(prefix) {
                        break;
                    }
                    taken += 1;
                    if let Ok(expansion) = std::str::from_utf8(key) {
                        let entry = window.entry(expansion.to_owned()).or_insert(false);
                        *entry |= is_metadata;
                    }
                }
            }
        }
    }

    // A term that extends to nothing but itself needs no assistance: the
    // all-terms stage already requires it exactly, and expanding it would add
    // two stages that can only restate a match the ladder has already made.
    // Assistance is warranted only when some genuinely longer completion
    // exists.
    if !window
        .keys()
        .any(|expansion| expansion.as_bytes() != prefix)
    {
        return Ok(Vec::new());
    }

    let mut selected: Vec<_> = window.into_iter().take(scan).collect();
    selected.sort_by(|left, right| {
        right
            .1
            .cmp(&left.1)
            .then_with(|| left.0.len().cmp(&right.0.len()))
            .then_with(|| left.0.cmp(&right.0))
    });
    selected.truncate(limit);
    let mut expansions: Vec<_> = selected
        .into_iter()
        .map(|(expansion, _)| expansion)
        .collect();
    // The plan carries expansions as a canonical set, not as a ranked list.
    expansions.sort();
    Ok(expansions)
}

fn execute_lexical_plan(
    contexts: &[NativeSearchContext<'_>],
    resolved: &ResolvedLexicalPlan,
    limit: usize,
    filters: &SearchFilters,
    statistics: &dyn Bm25StatisticsProvider,
) -> Result<Vec<SearchHit>> {
    if limit == 0 {
        return Ok(Vec::new());
    }
    resolved
        .plan
        .validate()
        .map_err(|error| Error::Query(error.to_string()))?;

    match resolved.plan.execution {
        QueryExecutionDisposition::EmptyNoEvidence => Ok(Vec::new()),
        QueryExecutionDisposition::ExplicitBypass => {
            execute_explicit(contexts, &resolved.plan, limit, filters, statistics)
        }
        QueryExecutionDisposition::Ready => {
            execute_evidence_stages(contexts, resolved, limit, filters, statistics)
        }
        QueryExecutionDisposition::AwaitingEvidence => Err(Error::Query(
            "query plan reached execution without finalized evidence".to_owned(),
        )),
    }
}

#[derive(Debug, Clone)]
#[cfg(feature = "internal-d5c-preview")]
struct NativeRerankHit {
    source: QualifiedSourceId,
    mtime: u64,
    hit: SearchHit,
    evidence_tier: LexicalEvidenceTier,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
#[cfg(feature = "internal-d5c-preview")]
struct NativeRerankIdentity {
    source: QualifiedSourceId,
    chunk_id: String,
    path: String,
}

#[cfg(feature = "internal-d5c-preview")]
impl NativeRerankHit {
    fn identity(&self) -> NativeRerankIdentity {
        NativeRerankIdentity {
            source: self.source.clone(),
            chunk_id: self.hit.chunk_id.clone(),
            path: self.hit.path.clone(),
        }
    }

    fn rerank_candidate(&self) -> RerankCandidate {
        RerankCandidate {
            source: self.source.clone(),
            chunk_id: self.hit.chunk_id.clone(),
            path: self.hit.path.clone(),
            evidence_tier: self.evidence_tier,
            lexical_score: self.hit.score,
        }
    }
}

#[cfg(feature = "internal-d5c-preview")]
fn execute_d5c_profile(
    contexts: &[NativeSearchContext<'_>],
    resolved: &ResolvedLexicalPlan,
    limit: usize,
    filters: &SearchFilters,
    statistics: &dyn Bm25StatisticsProvider,
    profile: &RelevanceProfile,
    query_time_epoch_seconds: u64,
) -> Result<Vec<SearchHit>> {
    let RelevanceProfile::D5cPreviewV1(d5c) = profile else {
        return Err(Error::Query(
            "D5C execution requires the d5c-preview-v1 profile".to_owned(),
        ));
    };
    d5c.validate().map_err(ranking_error)?;
    if limit == 0 {
        return Ok(Vec::new());
    }

    let candidates = execute_lexical_candidates(
        contexts,
        resolved,
        crate::ranking::MAX_RERANK_CANDIDATES,
        filters,
        statistics,
    )?;
    let (source_signals, hydration_work_units) =
        hydrate_source_signals(contexts, &candidates, d5c)?;
    let input = RerankInput {
        schema_version: RERANK_INPUT_SCHEMA_VERSION,
        query_time_epoch_seconds,
        candidates: candidates
            .iter()
            .map(NativeRerankHit::rerank_candidate)
            .collect(),
        source_signals,
    };
    let ranked = rerank_candidates_with_initial_work(profile, &input, hydration_work_units)
        .map_err(ranking_error)?;

    let mut hits_by_identity = BTreeMap::new();
    for candidate in candidates {
        if hits_by_identity
            .insert(candidate.identity(), candidate.hit)
            .is_some()
        {
            return Err(Error::Index(
                "D5C candidate pool contains a duplicate qualified chunk".to_owned(),
            ));
        }
    }
    let mut hits = Vec::with_capacity(ranked.candidates().len().min(limit));
    for candidate in ranked.into_candidates().into_iter().take(limit) {
        let identity = NativeRerankIdentity {
            source: candidate.source,
            chunk_id: candidate.chunk_id,
            path: candidate.path,
        };
        let hit = hits_by_identity.remove(&identity).ok_or_else(|| {
            Error::Index("D5C reranker returned an unknown qualified chunk".to_owned())
        })?;
        hits.push(hit);
    }
    Ok(hits)
}

#[cfg(feature = "internal-d5c-preview")]
fn execute_lexical_candidates(
    contexts: &[NativeSearchContext<'_>],
    resolved: &ResolvedLexicalPlan,
    limit: usize,
    filters: &SearchFilters,
    statistics: &dyn Bm25StatisticsProvider,
) -> Result<Vec<NativeRerankHit>> {
    resolved
        .plan
        .validate()
        .map_err(|error| Error::Query(error.to_string()))?;
    match resolved.plan.execution {
        QueryExecutionDisposition::EmptyNoEvidence => Ok(Vec::new()),
        QueryExecutionDisposition::ExplicitBypass => {
            execute_explicit_candidates(contexts, &resolved.plan, limit, filters, statistics)
        }
        QueryExecutionDisposition::Ready => {
            execute_evidence_candidates(contexts, resolved, limit, filters, statistics)
        }
        QueryExecutionDisposition::AwaitingEvidence => Err(Error::Query(
            "query plan reached D5C execution without finalized evidence".to_owned(),
        )),
    }
}

#[cfg(feature = "internal-d5c-preview")]
fn execute_explicit_candidates(
    contexts: &[NativeSearchContext<'_>],
    plan: &LexicalQueryPlan,
    limit: usize,
    filters: &SearchFilters,
    statistics: &dyn Bm25StatisticsProvider,
) -> Result<Vec<NativeRerankHit>> {
    if plan.assistance != QueryAssistanceEligibility::ExplicitSyntaxBypass
        || plan.kind != QueryPlanKind::Explicit
        || plan.match_operator != QueryMatchOperator::Explicit
    {
        return Err(Error::Query(
            "explicit query plan is not an unassisted bypass".to_owned(),
        ));
    }

    let mut hits = Vec::new();
    for context in contexts {
        let parsed = if let Some(prefix) =
            simple_explicit_prefix_query(context.index, context.fields, &plan.query)?
        {
            prefix
        } else {
            let parser = lexical_parser(context.index, context.fields);
            parser
                .parse_query(&plan.query)
                .map_err(|error| Error::Query(error.to_string()))?
        };
        let query = filtered_query(parsed, filters, context.fields)?;
        let partition_hits = collect_stable_rerank_hits(
            context,
            query.as_ref(),
            limit,
            statistics,
            LexicalEvidenceTier::Explicit,
        )?;
        merge_bounded_rerank_hits(&mut hits, partition_hits, limit);
    }
    Ok(hits)
}

#[cfg(feature = "internal-d5c-preview")]
fn execute_evidence_candidates(
    contexts: &[NativeSearchContext<'_>],
    resolved: &ResolvedLexicalPlan,
    limit: usize,
    filters: &SearchFilters,
    statistics: &dyn Bm25StatisticsProvider,
) -> Result<Vec<NativeRerankHit>> {
    let plan = &resolved.plan;
    let mut hits = Vec::new();
    let mut seen = BTreeSet::new();
    let mut collected_candidates = 0_usize;

    for stage in &plan.evidence_stages {
        if hits.len() == limit || collected_candidates == plan.bounds.max_total_candidates {
            break;
        }
        let stage_limit = stage
            .max_candidates
            .min(plan.bounds.max_total_candidates - collected_candidates);
        if stage_limit == 0 {
            break;
        }

        let mut stage_hits = Vec::new();
        for context in contexts {
            let Some(stage_query) = compile_evidence_stage(
                context.index,
                context.fields,
                plan,
                stage,
                &resolved.prefix_expansions,
            )?
            else {
                continue;
            };
            let query = filtered_query(stage_query, filters, context.fields)?;
            let partition_hits = collect_stable_rerank_hits(
                context,
                query.as_ref(),
                stage_limit,
                statistics,
                stage.kind.into(),
            )?;
            merge_bounded_rerank_hits(&mut stage_hits, partition_hits, stage_limit);
        }
        collected_candidates += stage_hits.len();
        for hit in stage_hits {
            if seen.insert(hit.identity()) {
                hits.push(hit);
                if hits.len() == limit {
                    break;
                }
            }
        }
    }
    Ok(hits)
}

#[cfg(feature = "internal-d5c-preview")]
fn collect_stable_rerank_hits(
    context: &NativeSearchContext<'_>,
    query: &dyn Query,
    limit: usize,
    statistics: &dyn Bm25StatisticsProvider,
    evidence_tier: LexicalEvidenceTier,
) -> Result<Vec<NativeRerankHit>> {
    let source_key_field = context.fields.source_key.ok_or_else(|| {
        Error::Index("active generation is missing the source_key field".to_owned())
    })?;
    let collector = StableDocCollector {
        limit,
        chunk_id: context.fields.chunk_id,
        path: context.fields.path,
        source_format: context.fields.source_format,
    };
    let documents = context
        .searcher
        .search_with_statistics_provider(query, &collector, statistics)
        .map_err(|error| Error::Query(error.to_string()))?;
    let snippet_generator =
        SnippetGenerator::create(context.searcher, query, context.fields.content)
            .map_err(|error| Error::Query(error.to_string()))?;
    let mut hits = Vec::with_capacity(documents.len());
    for ranked in documents {
        validate_document_resource(&ranked.document, context.fields, context.resource)?;
        let source_key = text(&ranked.document, source_key_field)?.to_owned();
        let mtime = u64_value(&ranked.document, context.fields.mtime)?;
        let hit = hit_from_document(
            &ranked.document,
            context.fields,
            ranked.score,
            Some(&snippet_generator),
        )?;
        hits.push(NativeRerankHit {
            source: QualifiedSourceId {
                authorization_scope: authorization_scope(context.resource),
                source_key,
            },
            mtime,
            hit,
            evidence_tier,
        });
    }
    Ok(hits)
}

#[cfg(feature = "internal-d5c-preview")]
fn merge_bounded_rerank_hits(
    target: &mut Vec<NativeRerankHit>,
    incoming: Vec<NativeRerankHit>,
    limit: usize,
) {
    target.extend(incoming);
    target.sort_by(|left, right| compare_hits(&left.hit, &right.hit));
    let mut seen = BTreeSet::new();
    target.retain(|hit| seen.insert(hit.identity()));
    target.truncate(limit);
}

#[cfg(feature = "internal-d5c-preview")]
fn hydrate_source_signals(
    contexts: &[NativeSearchContext<'_>],
    candidates: &[NativeRerankHit],
    profile: &D5cRelevanceProfile,
) -> Result<(Vec<SourceSignalObservation>, usize)> {
    let mut signals = BTreeMap::<QualifiedSourceId, SourceSignalObservation>::new();
    let mut candidate_paths = BTreeMap::<QualifiedSourceId, String>::new();
    for candidate in candidates {
        if candidate_paths
            .insert(candidate.source.clone(), candidate.hit.path.clone())
            .is_some_and(|previous| previous != candidate.hit.path)
        {
            return Err(Error::Index(
                "D5C candidate source maps to inconsistent paths".to_owned(),
            ));
        }
        let signal =
            signals
                .entry(candidate.source.clone())
                .or_insert_with(|| SourceSignalObservation {
                    source: candidate.source.clone(),
                    source_mtime_epoch_seconds: Some(candidate.mtime),
                    matched_property_rule_ids: Vec::new(),
                    present_properties: Vec::new(),
                    property_values: Vec::new(),
                });
        if signal.source_mtime_epoch_seconds != Some(candidate.mtime) {
            return Err(Error::Index(
                "D5C candidate source maps to inconsistent mtimes".to_owned(),
            ));
        }
    }

    let mut hydration_work_units = 0_usize;
    for context in contexts {
        let scope = authorization_scope(context.resource);
        let source_keys: BTreeSet<_> = candidates
            .iter()
            .filter(|candidate| candidate.source.authorization_scope == scope)
            .map(|candidate| candidate.source.source_key.clone())
            .collect();
        if source_keys.is_empty() {
            continue;
        }
        hydrate_context_source_signals(
            context,
            &source_keys,
            &candidate_paths,
            profile,
            &mut signals,
            &mut hydration_work_units,
        )?;
    }

    Ok((signals.into_values().collect(), hydration_work_units))
}

#[cfg(feature = "internal-d5c-preview")]
fn hydrate_context_source_signals(
    context: &NativeSearchContext<'_>,
    source_keys: &BTreeSet<String>,
    candidate_paths: &BTreeMap<QualifiedSourceId, String>,
    profile: &D5cRelevanceProfile,
    signals: &mut BTreeMap<QualifiedSourceId, SourceSignalObservation>,
    hydration_work_units: &mut usize,
) -> Result<()> {
    let source_key_field = context.fields.source_key.ok_or_else(|| {
        Error::Index("active generation is missing the source_key field".to_owned())
    })?;
    let source_filter = || {
        Box::new(TermSetQuery::new(
            source_keys
                .iter()
                .map(|key| Term::from_field_text(source_key_field, key)),
        )) as Box<dyn Query>
    };
    let owner_filter = || {
        Box::new(TermQuery::new(
            Term::from_field_u64(context.fields.source_property_owner, 1),
            IndexRecordOption::Basic,
        )) as Box<dyn Query>
    };
    let owner_query = BooleanQuery::new(vec![
        (Occur::Must, source_filter()),
        (Occur::Must, owner_filter()),
    ]);
    let owner_addresses = context
        .searcher
        .search(&owner_query, &DocSetCollector)
        .map_err(|error| Error::Index(error.to_string()))?;
    let mut owners_seen = BTreeSet::new();
    for address in owner_addresses {
        let document = context
            .searcher
            .doc::<TantivyDocument>(address)
            .map_err(|error| Error::Index(error.to_string()))?;
        validate_document_resource(&document, context.fields, context.resource)?;
        let source_key = text(&document, source_key_field)?.to_owned();
        if !source_keys.contains(&source_key) || !owners_seen.insert(source_key.clone()) {
            return Err(Error::Index(
                "D5C source-owner hydration returned an unexpected duplicate".to_owned(),
            ));
        }
        let source = QualifiedSourceId {
            authorization_scope: authorization_scope(context.resource),
            source_key,
        };
        let expected_path = candidate_paths.get(&source).ok_or_else(|| {
            Error::Index("D5C source-owner hydration escaped the candidate set".to_owned())
        })?;
        if text(&document, context.fields.path)? != expected_path {
            return Err(Error::Index(
                "D5C source-owner path disagrees with its candidates".to_owned(),
            ));
        }
        let signal = signals.get_mut(&source).ok_or_else(|| {
            Error::Index("D5C source-owner hydration has no candidate signal".to_owned())
        })?;
        signal.source_mtime_epoch_seconds = Some(u64_value(&document, context.fields.mtime)?);
    }
    if owners_seen.len() != source_keys.len() {
        return Err(Error::Index(
            "D5C source-owner hydration is incomplete".to_owned(),
        ));
    }

    for rule in &profile.property_rules {
        let property_query = compile_property_rule_query(context, rule, hydration_work_units)?;
        let query = BooleanQuery::new(vec![
            (Occur::Must, source_filter()),
            (Occur::Must, owner_filter()),
            (Occur::Must, property_query),
        ]);
        let addresses = context
            .searcher
            .search(&query, &DocSetCollector)
            .map_err(|error| Error::Index(error.to_string()))?;
        for address in addresses {
            let document = context
                .searcher
                .doc::<TantivyDocument>(address)
                .map_err(|error| Error::Index(error.to_string()))?;
            validate_document_resource(&document, context.fields, context.resource)?;
            let source = QualifiedSourceId {
                authorization_scope: authorization_scope(context.resource),
                source_key: text(&document, source_key_field)?.to_owned(),
            };
            let signal = signals.get_mut(&source).ok_or_else(|| {
                Error::Index("D5C property hydration escaped the candidate set".to_owned())
            })?;
            signal.matched_property_rule_ids.push(rule.id.clone());
        }
    }
    for signal in signals.values_mut() {
        if signal.source.authorization_scope == authorization_scope(context.resource) {
            signal.matched_property_rule_ids.sort();
            signal.matched_property_rule_ids.dedup();
        }
    }
    Ok(())
}

#[cfg(feature = "internal-d5c-preview")]
fn compile_property_rule_query(
    context: &NativeSearchContext<'_>,
    rule: &PropertyRule,
    hydration_work_units: &mut usize,
) -> Result<Box<dyn Query>> {
    charge_native_ranking_work(hydration_work_units, 1)?;
    let fields = context.fields;
    match &rule.predicate {
        PropertyPredicate::Presence => Ok(Box::new(TermQuery::new(
            Term::from_field_text(fields.property_names, &property_name_term(&rule.property)),
            IndexRecordOption::Basic,
        ))),
        PropertyPredicate::Exact { pointer, value } => {
            let value = property_value_from_ranking_scalar(value)?;
            let (field, term) = if let Some(pointer) = pointer {
                (
                    fields.property_path_exact,
                    property_path_exact_term(&rule.property, pointer, &value),
                )
            } else {
                (
                    fields.property_exact,
                    property_exact_term(&rule.property, &value),
                )
            };
            Ok(Box::new(TermQuery::new(
                Term::from_field_text(field, &term),
                IndexRecordOption::Basic,
            )))
        }
        PropertyPredicate::I64Range { pointer, min, max } => {
            let min = min
                .as_deref()
                .map(parse_i64)
                .transpose()?
                .unwrap_or(i64::MIN);
            let max = max
                .as_deref()
                .map(parse_i64)
                .transpose()?
                .unwrap_or(i64::MAX);
            let (field, lower, upper) = if let Some(pointer) = pointer {
                (
                    fields.property_path_i64,
                    property_path_i64_term(&rule.property, pointer, min),
                    property_path_i64_term(&rule.property, pointer, max),
                )
            } else {
                (
                    fields.property_i64,
                    property_i64_term(&rule.property, min),
                    property_i64_term(&rule.property, max),
                )
            };
            bounded_string_range_query(context.searcher, field, lower, upper, hydration_work_units)
        }
        PropertyPredicate::U64Range { pointer, min, max } => {
            let min = min.as_deref().map(parse_u64).transpose()?.unwrap_or(0);
            let max = max
                .as_deref()
                .map(parse_u64)
                .transpose()?
                .unwrap_or(u64::MAX);
            let (field, lower, upper) = if let Some(pointer) = pointer {
                (
                    fields.property_path_u64,
                    property_path_u64_term(&rule.property, pointer, min),
                    property_path_u64_term(&rule.property, pointer, max),
                )
            } else {
                (
                    fields.property_u64,
                    property_u64_term(&rule.property, min),
                    property_u64_term(&rule.property, max),
                )
            };
            bounded_string_range_query(context.searcher, field, lower, upper, hydration_work_units)
        }
        PropertyPredicate::F64Range { pointer, min, max } => {
            let mut min = min
                .as_deref()
                .map(parse_f64_bits)
                .transpose()?
                .unwrap_or(-f64::MAX);
            let mut max = max
                .as_deref()
                .map(parse_f64_bits)
                .transpose()?
                .unwrap_or(f64::MAX);
            if min == 0.0 {
                min = -0.0;
            }
            if max == 0.0 {
                max = 0.0;
            }
            let (field, lower, upper) = if let Some(pointer) = pointer {
                (
                    fields.property_path_f64,
                    property_path_f64_term(&rule.property, pointer, min),
                    property_path_f64_term(&rule.property, pointer, max),
                )
            } else {
                (
                    fields.property_f64,
                    property_f64_term(&rule.property, min),
                    property_f64_term(&rule.property, max),
                )
            };
            bounded_string_range_query(context.searcher, field, lower, upper, hydration_work_units)
        }
        PropertyPredicate::DateRange { pointer, min, max } => {
            let min = min.as_deref().unwrap_or("0000-01-01");
            let max = max.as_deref().unwrap_or("9999-12-31");
            let (field, lower, upper) = if let Some(pointer) = pointer {
                (
                    fields.property_path_date,
                    property_path_date_term(&rule.property, pointer, min),
                    property_path_date_term(&rule.property, pointer, max),
                )
            } else {
                (
                    fields.property_date,
                    property_date_term(&rule.property, min),
                    property_date_term(&rule.property, max),
                )
            };
            bounded_string_range_query(context.searcher, field, lower, upper, hydration_work_units)
        }
    }
}

#[cfg(feature = "internal-d5c-preview")]
fn bounded_string_range_query(
    searcher: &Searcher,
    field: Field,
    lower: String,
    upper: String,
    hydration_work_units: &mut usize,
) -> Result<Box<dyn Query>> {
    let mut terms = BTreeSet::new();
    for segment in searcher.segment_readers() {
        let inverted = segment
            .inverted_index(field)
            .map_err(|error| Error::Index(error.to_string()))?;
        let mut stream = inverted
            .terms()
            .range()
            .ge(lower.as_bytes())
            .le(upper.as_bytes())
            .into_stream()
            .map_err(|error| Error::Index(error.to_string()))?;
        while stream.advance() {
            charge_native_ranking_work(hydration_work_units, 1)?;
            let value = std::str::from_utf8(stream.key()).map_err(|_| {
                Error::Index("D5C property range encountered a non-UTF-8 term".to_owned())
            })?;
            terms.insert(value.to_owned());
        }
    }
    Ok(Box::new(TermSetQuery::new(
        terms
            .into_iter()
            .map(|value| Term::from_field_text(field, &value)),
    )))
}

#[cfg(feature = "internal-d5c-preview")]
fn charge_native_ranking_work(work_units: &mut usize, amount: usize) -> Result<()> {
    *work_units = work_units
        .checked_add(amount)
        .filter(|work| *work <= MAX_RANKING_WORK_UNITS)
        .ok_or_else(|| {
            Error::Query(
                "ranking_work_limit_exceeded: native signal hydration exceeded its deterministic limit"
                    .to_owned(),
            )
        })?;
    Ok(())
}

#[cfg(feature = "internal-d5c-preview")]
fn property_value_from_ranking_scalar(value: &RankingScalar) -> Result<PropertyValue> {
    match value {
        RankingScalar::Null => Ok(PropertyValue::Null),
        RankingScalar::Boolean(value) => Ok(PropertyValue::Bool(*value)),
        RankingScalar::I64(value) => Ok(PropertyValue::I64(parse_i64(value)?)),
        RankingScalar::U64(value) => Ok(PropertyValue::U64(parse_u64(value)?)),
        RankingScalar::F64(value) => Ok(PropertyValue::F64(parse_f64_bits(value)?)),
        RankingScalar::String(value) => Ok(PropertyValue::String(value.clone())),
        RankingScalar::Date(value) => Ok(PropertyValue::String(value.clone())),
    }
}

#[cfg(feature = "internal-d5c-preview")]
fn parse_i64(value: &str) -> Result<i64> {
    value.parse().map_err(|_| {
        Error::Query("invalid_relevance_profile: malformed i64 ranking value".to_owned())
    })
}

#[cfg(feature = "internal-d5c-preview")]
fn parse_u64(value: &str) -> Result<u64> {
    value.parse().map_err(|_| {
        Error::Query("invalid_relevance_profile: malformed u64 ranking value".to_owned())
    })
}

#[cfg(feature = "internal-d5c-preview")]
fn parse_f64_bits(value: &str) -> Result<f64> {
    let bits = u64::from_str_radix(value, 16).map_err(|_| {
        Error::Query("invalid_relevance_profile: malformed f64 ranking value".to_owned())
    })?;
    let value = f64::from_bits(bits);
    if value.is_finite() {
        Ok(value)
    } else {
        Err(Error::Query(
            "invalid_relevance_profile: non-finite f64 ranking value".to_owned(),
        ))
    }
}

#[cfg(feature = "internal-d5c-preview")]
fn authorization_scope(resource: Option<&ResourceKey>) -> String {
    resource.map_or_else(
        || DESKTOP_AUTHORIZATION_SCOPE.to_owned(),
        |resource| format!("openclast:{}", crate::partition::partition_id(resource)),
    )
}

#[cfg(feature = "internal-d5c-preview")]
fn validate_document_resource(
    document: &TantivyDocument,
    fields: &Fields,
    resource: Option<&ResourceKey>,
) -> Result<()> {
    if let Some(resource) = resource {
        let vault_id = text(document, fields.vault_id)?;
        let room_id = text(document, fields.room)?;
        if vault_id != resource.vault_id || room_id != resource.room_id {
            return Err(Error::Index(format!(
                "document resource mismatch in partition {}/{}/{}",
                resource.tenant_id, resource.vault_id, resource.room_id
            )));
        }
    }
    Ok(())
}

#[cfg(feature = "internal-d5c-preview")]
fn validate_d5c_profile(profile: &RelevanceProfile) -> Result<()> {
    let RelevanceProfile::D5cPreviewV1(profile) = profile else {
        return Err(Error::Query(
            "D5C execution requires the d5c-preview-v1 profile".to_owned(),
        ));
    };
    profile.validate().map_err(ranking_error)
}

#[cfg(feature = "internal-d5c-preview")]
fn ranking_error(error: crate::ranking::RankingError) -> Error {
    Error::Query(format!("{}: {}", error.code, error.message))
}

fn execute_explicit(
    contexts: &[NativeSearchContext<'_>],
    plan: &LexicalQueryPlan,
    limit: usize,
    filters: &SearchFilters,
    statistics: &dyn Bm25StatisticsProvider,
) -> Result<Vec<SearchHit>> {
    if plan.assistance != QueryAssistanceEligibility::ExplicitSyntaxBypass
        || plan.kind != QueryPlanKind::Explicit
        || plan.match_operator != QueryMatchOperator::Explicit
    {
        return Err(Error::Query(
            "explicit query plan is not an unassisted bypass".to_owned(),
        ));
    }

    let mut hits = Vec::new();
    for context in contexts {
        let parsed = if let Some(prefix) =
            simple_explicit_prefix_query(context.index, context.fields, &plan.query)?
        {
            prefix
        } else {
            let parser = lexical_parser(context.index, context.fields);
            parser
                .parse_query(&plan.query)
                .map_err(|error| Error::Query(error.to_string()))?
        };
        let query = filtered_query(parsed, filters, context.fields)?;
        let partition_hits = collect_stable_hits(
            context.searcher,
            query.as_ref(),
            context.fields,
            context.resource,
            limit,
            statistics,
        )?;
        merge_bounded_hits(&mut hits, partition_hits, limit);
    }
    Ok(hits)
}

fn simple_explicit_prefix_query(
    index: &Index,
    fields: &Fields,
    query: &str,
) -> Result<Option<Box<dyn Query>>> {
    let authored = query.trim();
    let Some(authored_prefix) = authored.strip_suffix('*') else {
        return Ok(None);
    };
    let Some(prefix) = crate::lexical::normalize_raw(authored_prefix) else {
        return Ok(None);
    };
    if prefix.chars().any(char::is_whitespace)
        || prefix
            .chars()
            .any(|character| !character.is_alphanumeric() && character != '_')
        || authored[..authored.len() - 1].contains('*')
    {
        return Ok(None);
    }

    let bindings = field_bindings(
        fields,
        &[
            QueryField::Filename,
            QueryField::Stem,
            QueryField::Aliases,
            QueryField::Title,
            QueryField::Heading,
            QueryField::Tags,
            QueryField::Content,
        ],
        QueryFieldGroup::SearchableText,
    );
    let mut alternatives = Vec::new();
    for (field, boost) in bindings {
        let tokens = analyze_text(index, field, &prefix)?;
        if tokens.len() != 1 {
            continue;
        }
        let pattern = format!("{}.*", regex::escape(&tokens[0]));
        let regex = RegexQuery::from_pattern(&pattern, field)
            .map_err(|error| Error::Query(error.to_string()))?;
        alternatives.push(Box::new(BoostQuery::new(Box::new(regex), boost)) as Box<dyn Query>);
    }
    Ok((!alternatives.is_empty())
        .then(|| Box::new(DisjunctionMaxQuery::new(alternatives)) as Box<dyn Query>))
}

fn execute_evidence_stages(
    contexts: &[NativeSearchContext<'_>],
    resolved: &ResolvedLexicalPlan,
    limit: usize,
    filters: &SearchFilters,
    statistics: &dyn Bm25StatisticsProvider,
) -> Result<Vec<SearchHit>> {
    let plan = &resolved.plan;
    let mut hits = Vec::new();
    let mut seen = HashSet::new();
    let mut collected_candidates = 0_usize;

    for stage in &plan.evidence_stages {
        if hits.len() == limit || collected_candidates == plan.bounds.max_total_candidates {
            break;
        }
        let stage_limit = stage
            .max_candidates
            .min(plan.bounds.max_total_candidates - collected_candidates);
        if stage_limit == 0 {
            break;
        }

        let mut stage_hits = Vec::new();
        for context in contexts {
            let Some(stage_query) = compile_evidence_stage(
                context.index,
                context.fields,
                plan,
                stage,
                &resolved.prefix_expansions,
            )?
            else {
                continue;
            };
            let query = filtered_query(stage_query, filters, context.fields)?;
            let partition_hits = collect_stable_hits(
                context.searcher,
                query.as_ref(),
                context.fields,
                context.resource,
                stage_limit,
                statistics,
            )?;
            merge_bounded_hits(&mut stage_hits, partition_hits, stage_limit);
        }
        collected_candidates += stage_hits.len();
        for hit in stage_hits {
            if seen.insert(hit.chunk_id.clone()) {
                hits.push(hit);
                if hits.len() == limit {
                    break;
                }
            }
        }
    }
    Ok(hits)
}

fn compile_evidence_stage(
    index: &Index,
    fields: &Fields,
    plan: &LexicalQueryPlan,
    stage: &QueryEvidenceStage,
    prefix_expansions: &BTreeMap<u16, Vec<String>>,
) -> Result<Option<Box<dyn Query>>> {
    match stage.kind {
        QueryEvidenceStageKind::ExactMetadata => exact_query(fields, plan)
            .map(|query| with_exact_identifier_anchors(fields, plan, query))
            .transpose()
            .and_then(|query| {
                query
                    .map(Some)
                    .ok_or_else(|| Error::Query("exact metadata stage has no intent".to_owned()))
            }),
        QueryEvidenceStageKind::ExactPhrase => phrase_query(index, fields, plan)?.map_or_else(
            || Ok(None),
            |query| with_exact_identifier_anchors(fields, plan, query).map(Some),
        ),
        QueryEvidenceStageKind::AllTerms | QueryEvidenceStageKind::PartialCoverage => {
            required_terms_query(index, fields, plan, stage)
        }
        QueryEvidenceStageKind::PrefixMetadata | QueryEvidenceStageKind::Prefix => {
            prefix_stage_query(index, fields, plan, stage, prefix_expansions)
        }
    }
}

fn required_terms_query(
    index: &Index,
    fields: &Fields,
    plan: &LexicalQueryPlan,
    stage: &QueryEvidenceStage,
) -> Result<Option<Box<dyn Query>>> {
    let mut clauses = Vec::with_capacity(stage.required_term_indexes.len());
    for term_index in &stage.required_term_indexes {
        let intent = plan
            .term_intents
            .get(*term_index as usize)
            .ok_or_else(|| Error::Query("evidence stage references an unknown term".to_owned()))?;
        let query = if intent.projection == QueryTermProjection::ExactIdentifier {
            let Some(query) = identifier_anchor_query(fields, plan, &intent.text)? else {
                return Ok(None);
            };
            query
        } else {
            let Some(query) =
                term_query_for_group(index, fields, plan, stage.field_group, &intent.text)?
            else {
                return Ok(None);
            };
            query
        };
        clauses.push((Occur::Must, query));
    }
    Ok(Some(Box::new(BooleanQuery::new(clauses))))
}

fn prefix_stage_query(
    index: &Index,
    fields: &Fields,
    plan: &LexicalQueryPlan,
    stage: &QueryEvidenceStage,
    prefix_expansions: &BTreeMap<u16, Vec<String>>,
) -> Result<Option<Box<dyn Query>>> {
    let mut clauses = Vec::new();
    for term_index in &stage.required_term_indexes {
        let intent = plan
            .term_intents
            .get(*term_index as usize)
            .ok_or_else(|| Error::Query("prefix stage references an unknown term".to_owned()))?;
        let query = if intent.projection == QueryTermProjection::ExactIdentifier {
            let Some(query) = identifier_anchor_query(fields, plan, &intent.text)? else {
                return Ok(None);
            };
            query
        } else {
            let Some(query) = term_query_for_group(
                index,
                fields,
                plan,
                QueryFieldGroup::SearchableText,
                &intent.text,
            )?
            else {
                return Ok(None);
            };
            query
        };
        clauses.push((Occur::Must, query));
    }
    for term_index in &stage.prefix_term_indexes {
        let expansions = prefix_expansions.get(term_index).ok_or_else(|| {
            Error::Query("prefix stage is missing its bounded expansions".to_owned())
        })?;
        // The metadata-scoped stage and the searchable-text stage share one
        // expansion set and differ only in the fields they may match.
        let bindings = field_bindings(
            fields,
            declared_fields(plan, stage.field_group),
            QueryFieldGroup::Prefix,
        );
        let mut alternatives = Vec::new();
        for expansion in expansions {
            alternatives.extend(term_alternatives(&bindings, expansion));
        }
        if alternatives.is_empty() {
            return Ok(None);
        }
        clauses.push((
            Occur::Must,
            Box::new(DisjunctionMaxQuery::new(alternatives)) as Box<dyn Query>,
        ));
    }
    Ok(Some(Box::new(BooleanQuery::new(clauses))))
}

fn identifier_anchor_query(
    fields: &Fields,
    plan: &LexicalQueryPlan,
    text: &str,
) -> Result<Option<Box<dyn Query>>> {
    if !declared_fields(plan, QueryFieldGroup::Exact).contains(&QueryField::ContentIdentifiers) {
        return Err(Error::Query(
            "exact identifier projection is not declared".to_owned(),
        ));
    }
    Ok(Some(Box::new(BoostQuery::new(
        Box::new(TermQuery::new(
            Term::from_field_text(fields.content_identifiers, text),
            IndexRecordOption::Basic,
        )),
        BOOST_CONTENT_IDENTIFIER,
    ))))
}

fn with_exact_identifier_anchors(
    fields: &Fields,
    plan: &LexicalQueryPlan,
    query: Box<dyn Query>,
) -> Result<Box<dyn Query>> {
    let mut clauses = vec![(Occur::Must, query)];
    for intent in plan
        .term_intents
        .iter()
        .filter(|intent| intent.projection == QueryTermProjection::ExactIdentifier)
    {
        let anchor = identifier_anchor_query(fields, plan, &intent.text)?
            .ok_or_else(|| Error::Query("exact identifier anchor has no query".to_owned()))?;
        clauses.push((Occur::Must, anchor));
    }
    if clauses.len() == 1 {
        Ok(clauses.pop().expect("base query exists").1)
    } else {
        Ok(Box::new(BooleanQuery::new(clauses)))
    }
}

fn term_query_for_group(
    index: &Index,
    fields: &Fields,
    plan: &LexicalQueryPlan,
    group: QueryFieldGroup,
    text: &str,
) -> Result<Option<Box<dyn Query>>> {
    let bindings = field_bindings(fields, declared_fields(plan, group), group);
    let Some((analysis_field, _)) = bindings.first().copied() else {
        return Ok(None);
    };
    let tokens = analyze_text(index, analysis_field, text)?;
    if tokens.is_empty() {
        return Ok(None);
    }
    let mut token_clauses = Vec::with_capacity(tokens.len());
    for token in tokens {
        let alternatives = term_alternatives(&bindings, &token);
        if alternatives.is_empty() {
            return Ok(None);
        }
        token_clauses.push((
            Occur::Must,
            Box::new(DisjunctionMaxQuery::new(alternatives)) as Box<dyn Query>,
        ));
    }
    if token_clauses.len() == 1 {
        Ok(Some(token_clauses.pop().expect("one token clause").1))
    } else {
        Ok(Some(Box::new(BooleanQuery::new(token_clauses))))
    }
}

fn term_alternatives(bindings: &[(Field, f32)], token: &str) -> Vec<Box<dyn Query>> {
    bindings
        .iter()
        .map(|(field, boost)| {
            let query: Box<dyn Query> = Box::new(TermQuery::new(
                Term::from_field_text(*field, token),
                IndexRecordOption::WithFreqs,
            ));
            if (*boost - 1.0).abs() < f32::EPSILON {
                query
            } else {
                Box::new(BoostQuery::new(query, *boost))
            }
        })
        .collect()
}

fn phrase_query(
    index: &Index,
    fields: &Fields,
    plan: &LexicalQueryPlan,
) -> Result<Option<Box<dyn Query>>> {
    let intent = plan
        .phrase_intent
        .as_ref()
        .ok_or_else(|| Error::Query("phrase stage has no intent".to_owned()))?;
    let bindings = field_bindings(
        fields,
        declared_fields(plan, intent.field_group),
        intent.field_group,
    );
    let mut alternatives = Vec::new();
    for (field, boost) in bindings {
        let mut tokens = Vec::new();
        for term in &intent.terms {
            tokens.extend(analyze_text(index, field, term)?);
        }
        if tokens.len() < 2 {
            continue;
        }
        let terms = tokens
            .into_iter()
            .map(|token| Term::from_field_text(field, &token))
            .collect();
        let query: Box<dyn Query> = Box::new(PhraseQuery::new(terms));
        alternatives.push(Box::new(BoostQuery::new(query, boost * BOOST_PHRASE)) as Box<dyn Query>);
    }
    if alternatives.is_empty() {
        return Ok(None);
    }
    Ok(Some(Box::new(DisjunctionMaxQuery::new(alternatives))))
}

fn exact_query(fields: &Fields, plan: &LexicalQueryPlan) -> Option<Box<dyn Query>> {
    let intent = plan.exact_intent.as_ref()?;
    let bindings = field_bindings(
        fields,
        declared_fields(plan, intent.field_group),
        intent.field_group,
    );
    let queries = bindings
        .into_iter()
        .map(|(field, boost)| {
            Box::new(BoostQuery::new(
                Box::new(TermQuery::new(
                    Term::from_field_text(field, &intent.normalized),
                    IndexRecordOption::Basic,
                )),
                boost,
            )) as Box<dyn Query>
        })
        .collect();
    Some(Box::new(DisjunctionMaxQuery::new(queries)))
}

fn analyze_text(index: &Index, field: Field, text: &str) -> Result<Vec<String>> {
    let mut analyzer = index
        .tokenizer_for_field(field)
        .map_err(|error| Error::Query(error.to_string()))?;
    let mut stream = analyzer.token_stream(text);
    let mut tokens = Vec::new();
    while stream.advance() {
        tokens.push(stream.token().text.clone());
    }
    Ok(tokens)
}

fn declared_fields(plan: &LexicalQueryPlan, group: QueryFieldGroup) -> &[QueryField] {
    match group {
        QueryFieldGroup::SearchableText => &plan.field_groups.searchable_text,
        QueryFieldGroup::Metadata => &plan.field_groups.metadata,
        QueryFieldGroup::Exact => &plan.field_groups.exact,
        QueryFieldGroup::Phrase => &plan.field_groups.phrase,
        QueryFieldGroup::Prefix => &plan.field_groups.prefix,
        QueryFieldGroup::PrefixMetadata => &plan.field_groups.prefix_metadata,
    }
}

fn field_bindings(
    fields: &Fields,
    declared: &[QueryField],
    group: QueryFieldGroup,
) -> Vec<(Field, f32)> {
    declared
        .iter()
        .map(|field| match (group, field) {
            (QueryFieldGroup::Exact, QueryField::Filename) => {
                (fields.filename_raw, BOOST_EXACT_METADATA)
            }
            (QueryFieldGroup::Exact, QueryField::Stem) => (fields.stem_raw, BOOST_EXACT_METADATA),
            (QueryFieldGroup::Exact, QueryField::Aliases) => {
                (fields.aliases_raw, BOOST_EXACT_METADATA)
            }
            (QueryFieldGroup::Exact, QueryField::Title) => (fields.title_raw, BOOST_EXACT_METADATA),
            (QueryFieldGroup::Exact, QueryField::Heading) => (fields.heading_raw, BOOST_HEADING),
            (QueryFieldGroup::Exact, QueryField::ContentIdentifiers) => {
                (fields.content_identifiers, BOOST_CONTENT_IDENTIFIER)
            }
            (_, QueryField::Filename) => (fields.filename, BOOST_FILENAME),
            (_, QueryField::Stem) => (fields.stem, BOOST_STEM),
            (_, QueryField::Aliases) => (fields.aliases, BOOST_ALIAS),
            (_, QueryField::Title) => (fields.title, BOOST_TITLE),
            (_, QueryField::Heading) => (fields.heading_text, BOOST_HEADING),
            (_, QueryField::Tags) => (fields.tags_text, BOOST_TAGS),
            (_, QueryField::Content) => (fields.content, BOOST_CONTENT),
            (_, QueryField::ContentIdentifiers) => {
                (fields.content_identifiers, BOOST_CONTENT_IDENTIFIER)
            }
        })
        .collect()
}

fn metadata_probe_fields(fields: &Fields, probe: &QueryMetadataProbe) -> Vec<Field> {
    probe
        .fields
        .iter()
        .map(|field| match field {
            QueryMetadataField::Filename => fields.filename,
            QueryMetadataField::Stem => fields.stem,
            QueryMetadataField::Aliases => fields.aliases,
            QueryMetadataField::Title => fields.title,
            QueryMetadataField::Heading => fields.heading_text,
            QueryMetadataField::Tags => fields.tags_text,
        })
        .collect()
}

fn lexical_parser(index: &Index, fields: &Fields) -> QueryParser {
    let mut parser = QueryParser::for_index(
        index,
        vec![
            fields.filename,
            fields.stem,
            fields.aliases,
            fields.title,
            fields.heading_text,
            fields.tags_text,
            fields.content,
        ],
    );
    parser.set_field_boost(fields.filename, BOOST_FILENAME);
    parser.set_field_boost(fields.stem, BOOST_STEM);
    parser.set_field_boost(fields.aliases, BOOST_ALIAS);
    parser.set_field_boost(fields.title, BOOST_TITLE);
    parser.set_field_boost(fields.heading_text, BOOST_HEADING);
    parser.set_field_boost(fields.tags_text, BOOST_TAGS);
    parser.set_field_boost(fields.content, BOOST_CONTENT);
    parser
}

fn filtered_query(
    parsed: Box<dyn Query>,
    filters: &SearchFilters,
    fields: &Fields,
) -> Result<Box<dyn Query>> {
    let mut clauses: Vec<(Occur, Box<dyn Query>)> = vec![(Occur::Must, parsed)];
    if let Some(vault_id) = filters.vault_id.as_deref() {
        clauses.push(exact_filter(fields.vault_id, vault_id));
    }
    if let Some(room) = filters.room.as_deref() {
        clauses.push(exact_filter(fields.room, room));
    }
    if let Some(prefix) = filters.path_prefix.as_deref() {
        let pattern = format!("{}.*", regex::escape(prefix));
        let query = RegexQuery::from_pattern(&pattern, fields.path)
            .map_err(|error| Error::Query(format!("invalid path prefix: {error}")))?;
        clauses.push((
            Occur::Must,
            Box::new(ConstScoreQuery::new(Box::new(query), 0.0)),
        ));
    }
    for tag in &filters.tags {
        clauses.push(exact_filter(fields.tags, tag));
    }
    add_frontmatter_filters(&mut clauses, &filters.frontmatter_equals, fields)?;
    Ok(Box::new(BooleanQuery::new(clauses)))
}

fn add_frontmatter_filters(
    clauses: &mut Vec<(Occur, Box<dyn Query>)>,
    filters: &BTreeMap<String, String>,
    fields: &Fields,
) -> Result<()> {
    for (name, value) in filters {
        let field = match name.as_str() {
            "title" => fields.title_exact,
            "description" => fields.description_exact,
            "status" => fields.status_exact,
            "date" => fields.date_exact,
            _ => {
                return Err(Error::Query(format!(
                    "unsupported frontmatter filter: {name}"
                )));
            }
        }
        .ok_or_else(|| {
            Error::Query(format!(
                "frontmatter filter {name} requires a Vertical 2 index rebuild"
            ))
        })?;
        clauses.push(exact_filter(field, value));
    }
    Ok(())
}

fn exact_filter(field: Field, value: &str) -> (Occur, Box<dyn Query>) {
    let query = TermQuery::new(
        Term::from_field_text(field, value),
        IndexRecordOption::Basic,
    );
    (
        Occur::Must,
        Box::new(ConstScoreQuery::new(Box::new(query), 0.0)),
    )
}

struct StableDocCollector {
    limit: usize,
    chunk_id: Field,
    path: Field,
    source_format: Field,
}

struct StableSegmentCollector {
    limit: usize,
    chunk_id: Field,
    path: Field,
    source_format: Field,
    store: StoreReader,
    documents: Vec<RankedDocument>,
    error: Option<String>,
}

struct StableCollectorFruit {
    documents: Vec<RankedDocument>,
    error: Option<String>,
}

struct RankedDocument {
    score: Score,
    chunk_id: String,
    path: String,
    document: TantivyDocument,
}

impl Collector for StableDocCollector {
    type Fruit = Vec<RankedDocument>;
    type Child = StableSegmentCollector;

    fn for_segment(
        &self,
        _segment_local_id: SegmentOrdinal,
        segment: &SegmentReader,
    ) -> tantivy::Result<Self::Child> {
        Ok(StableSegmentCollector {
            limit: self.limit,
            chunk_id: self.chunk_id,
            path: self.path,
            source_format: self.source_format,
            store: segment.get_store_reader(10)?,
            documents: Vec::new(),
            error: None,
        })
    }

    fn requires_scoring(&self) -> bool {
        true
    }

    fn merge_fruits(
        &self,
        fruits: Vec<<Self::Child as SegmentCollector>::Fruit>,
    ) -> tantivy::Result<Self::Fruit> {
        let mut documents = Vec::new();
        for fruit in fruits {
            if let Some(error) = fruit.error {
                return Err(tantivy::TantivyError::InvalidArgument(error));
            }
            for document in fruit.documents {
                insert_bounded_document(&mut documents, document, self.limit);
            }
        }
        Ok(documents)
    }
}

impl SegmentCollector for StableSegmentCollector {
    type Fruit = StableCollectorFruit;

    fn collect(&mut self, doc: DocId, score: Score) {
        if self.error.is_some() || self.limit == 0 {
            return;
        }
        let document = match self.store.get::<TantivyDocument>(doc) {
            Ok(document) => document,
            Err(error) => {
                self.error = Some(format!("could not load scored document: {error}"));
                return;
            }
        };
        let Some(chunk_id) = document
            .get_first(self.chunk_id)
            .and_then(|value| value.as_str())
            .map(str::to_owned)
        else {
            self.error = Some("scored document is missing its chunk ID".to_owned());
            return;
        };
        let Some(path) = document
            .get_first(self.path)
            .and_then(|value| value.as_str())
            .map(str::to_owned)
        else {
            self.error = Some("scored document is missing its path".to_owned());
            return;
        };
        let Some(source_format) = document
            .get_first(self.source_format)
            .and_then(|value| value.as_str())
        else {
            self.error = Some("scored document is missing its source format".to_owned());
            return;
        };
        let source_format = match serde_json::from_str::<SourceFormat>(source_format) {
            Ok(source_format) => source_format,
            Err(error) => {
                self.error = Some(format!(
                    "scored document has an invalid source format: {error}"
                ));
                return;
            }
        };
        if source_format == SourceFormat::Excel
            && excel_content_role_from_chunk_id(&chunk_id).is_none()
        {
            self.error = Some("scored Excel document has an invalid content-role tag".to_owned());
            return;
        }
        // The role never touches the score (§10.5: identical text-evidence
        // rules for every format). Its tie-break is the chunk ID itself: the
        // role byte leads the ID, so at equal score the existing ascending
        // chunk-ID comparison orders primary before supporting before latent.
        insert_bounded_document(
            &mut self.documents,
            RankedDocument {
                score,
                chunk_id,
                path,
                document,
            },
            self.limit,
        );
    }

    fn harvest(self) -> Self::Fruit {
        StableCollectorFruit {
            documents: self.documents,
            error: self.error,
        }
    }
}

fn insert_bounded_document(
    documents: &mut Vec<RankedDocument>,
    document: RankedDocument,
    limit: usize,
) {
    if limit == 0 {
        return;
    }
    if documents.len() == limit
        && compare_ranked_documents(
            &document,
            documents.last().expect("bounded list is nonempty"),
        ) != Ordering::Less
    {
        return;
    }
    documents.push(document);
    documents.sort_by(compare_ranked_documents);
    documents.truncate(limit);
}

fn compare_ranked_documents(left: &RankedDocument, right: &RankedDocument) -> Ordering {
    right
        .score
        .total_cmp(&left.score)
        .then_with(|| left.chunk_id.cmp(&right.chunk_id))
        .then_with(|| left.path.cmp(&right.path))
}

fn collect_stable_hits(
    searcher: &Searcher,
    query: &dyn Query,
    fields: &Fields,
    resource: Option<&ResourceKey>,
    limit: usize,
    statistics: &dyn Bm25StatisticsProvider,
) -> Result<Vec<SearchHit>> {
    let collector = StableDocCollector {
        limit,
        chunk_id: fields.chunk_id,
        path: fields.path,
        source_format: fields.source_format,
    };
    let documents = searcher
        .search_with_statistics_provider(query, &collector, statistics)
        .map_err(|error| Error::Query(error.to_string()))?;
    let snippet_generator = SnippetGenerator::create(searcher, query, fields.content)
        .map_err(|error| Error::Query(error.to_string()))?;
    let mut hits = Vec::with_capacity(documents.len());
    for ranked in documents {
        if let Some(resource) = resource {
            let vault_id = text(&ranked.document, fields.vault_id)?;
            let room_id = text(&ranked.document, fields.room)?;
            if vault_id != resource.vault_id || room_id != resource.room_id {
                return Err(Error::Index(format!(
                    "document resource mismatch in partition {}/{}/{}",
                    resource.tenant_id, resource.vault_id, resource.room_id
                )));
            }
        }
        hits.push(hit_from_document(
            &ranked.document,
            fields,
            ranked.score,
            Some(&snippet_generator),
        )?);
    }
    Ok(hits)
}

fn merge_bounded_hits(target: &mut Vec<SearchHit>, incoming: Vec<SearchHit>, limit: usize) {
    target.extend(incoming);
    target.sort_by(compare_hits);
    let mut seen = HashSet::new();
    target.retain(|hit| seen.insert(hit.chunk_id.clone()));
    target.truncate(limit);
}

fn compare_hits(left: &SearchHit, right: &SearchHit) -> Ordering {
    right
        .score
        .total_cmp(&left.score)
        .then_with(|| left.chunk_id.cmp(&right.chunk_id))
        .then_with(|| left.path.cmp(&right.path))
}

fn hit_from_document(
    document: &TantivyDocument,
    fields: &Fields,
    score: f32,
    snippets: Option<&SnippetGenerator>,
) -> Result<SearchHit> {
    let content = text(document, fields.content)?;
    let excerpt = match snippets {
        Some(generator) => {
            let snippet = generator.snippet_from_doc(document);
            if snippet.fragment().trim().is_empty() {
                fallback_excerpt(content)
            } else {
                snippet.to_html()
            }
        }
        None => fallback_excerpt(content),
    };
    Ok(SearchHit {
        chunk_id: text(document, fields.chunk_id)?.to_owned(),
        vault_id: text(document, fields.vault_id)?.to_owned(),
        path: text(document, fields.path)?.to_owned(),
        heading_path: decode_json(text(document, fields.heading_path)?, "heading_path")?,
        format: decode_json(text(document, fields.source_format)?, "source_format")?,
        coverage: decode_json(
            text(document, fields.extraction_coverage)?,
            "extraction_coverage",
        )?,
        locator: decode_json(text(document, fields.source_locator)?, "source_locator")?,
        score,
        excerpt,
        frontmatter: decode_json(text(document, fields.frontmatter)?, "frontmatter")?,
    })
}

/// Fetches full hits for an externally ranked chunk-ID list (semantic or
/// fused ordering), applying the same filters as lexical search. IDs the
/// filters exclude are dropped; the input order and scores are preserved.
pub(crate) fn hydrate_ordered(
    index: &Index,
    fields: &Fields,
    reader: &IndexReader,
    ordered: &[(String, f32)],
    filters: &SearchFilters,
    snippet_source: Option<&str>,
) -> Result<Vec<SearchHit>> {
    if ordered.is_empty() {
        return Ok(Vec::new());
    }
    let searcher = reader.searcher();
    let id_clauses: Vec<(Occur, Box<dyn Query>)> = ordered
        .iter()
        .map(|(chunk_id, _)| {
            (
                Occur::Should,
                Box::new(TermQuery::new(
                    Term::from_field_text(fields.chunk_id, chunk_id),
                    IndexRecordOption::Basic,
                )) as Box<dyn Query>,
            )
        })
        .collect();
    let base: Box<dyn Query> = Box::new(BooleanQuery::new(id_clauses));
    let query = filtered_query(base, filters, fields)?;
    let top_docs = searcher
        .search(&query, &TopDocs::with_limit(ordered.len()).order_by_score())
        .map_err(|error| Error::Query(error.to_string()))?;

    let snippets = snippet_source.and_then(|source| {
        let parser = lexical_parser(index, fields);
        let parsed = parser.parse_query(source).ok()?;
        SnippetGenerator::create(&searcher, parsed.as_ref(), fields.content).ok()
    });

    let mut by_id: HashMap<String, SearchHit> = HashMap::with_capacity(top_docs.len());
    for (_, address) in top_docs {
        let document = searcher
            .doc::<TantivyDocument>(address)
            .map_err(|error| Error::Index(error.to_string()))?;
        let hit = hit_from_document(&document, fields, 0.0, snippets.as_ref())?;
        by_id.insert(hit.chunk_id.clone(), hit);
    }

    let mut hits = Vec::with_capacity(ordered.len().min(by_id.len()));
    for (chunk_id, score) in ordered {
        if let Some(mut hit) = by_id.remove(chunk_id) {
            hit.score = *score;
            hits.push(hit);
        }
    }
    Ok(hits)
}

fn text(document: &TantivyDocument, field: Field) -> Result<&str> {
    document
        .get_first(field)
        .and_then(|value| value.as_str())
        .ok_or_else(|| Error::Index(format!("stored field {field:?} is missing or not text")))
}

#[cfg(feature = "internal-d5c-preview")]
fn u64_value(document: &TantivyDocument, field: Field) -> Result<u64> {
    document
        .get_first(field)
        .and_then(|value| value.as_u64())
        .ok_or_else(|| Error::Index(format!("stored field {field:?} is missing or not u64")))
}

fn decode_json<T: serde::de::DeserializeOwned>(source: &str, field: &str) -> Result<T> {
    serde_json::from_str(source)
        .map_err(|error| Error::Index(format!("invalid stored {field}: {error}")))
}

fn fallback_excerpt(content: &str) -> String {
    let mut excerpt: String = content.chars().take(240).collect();
    if content.chars().count() > 240 {
        excerpt.push('…');
    }
    excerpt
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;
    use std::fs;
    use std::path::PathBuf;

    #[cfg(feature = "internal-d5c-preview")]
    use filetime::{FileTime, set_file_mtime};
    use serde::Deserialize;
    use tempfile::tempdir;

    use super::*;
    use crate::index::build_index;
    use crate::model::{Config, VaultRegistration};

    fn matching_paths_for_field(
        index: &Index,
        fields: &Fields,
        reader: &IndexReader,
        field: Field,
        query: &str,
    ) -> HashSet<String> {
        let mut parser = QueryParser::for_index(index, vec![field]);
        parser.set_conjunction_by_default();
        let query = parser.parse_query(query).unwrap();
        let searcher = reader.searcher();
        searcher
            .search(&query, &TopDocs::with_limit(100).order_by_score())
            .unwrap()
            .into_iter()
            .map(|(_, address)| {
                let document = searcher.doc::<TantivyDocument>(address).unwrap();
                text(&document, fields.path).unwrap().to_owned()
            })
            .collect()
    }

    fn technical_fixture_config() -> Config {
        let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../../fixtures/retrieval/technical-terms");
        Config {
            vaults: vec![
                VaultRegistration {
                    id: "technical-a".into(),
                    path: fixture.join("vault-a"),
                    room: None,
                },
                VaultRegistration {
                    id: "technical-b".into(),
                    path: fixture.join("vault-b"),
                    room: None,
                },
            ],
            ..Config::default()
        }
    }

    fn search(data: &Path, query: &str, limit: usize) -> Vec<SearchHit> {
        search_index(
            data,
            &LexicalSearchRequest {
                query: query.to_owned(),
                limit,
                vault_id: None,
            },
        )
        .unwrap()
    }

    #[cfg(feature = "internal-d5c-preview")]
    fn d5c_search(
        data: &Path,
        query: &str,
        limit: usize,
        profile: D5cRelevanceProfile,
        query_time_epoch_seconds: u64,
        filters: &SearchFilters,
    ) -> Vec<SearchHit> {
        let (index, fields) = open_index(data).unwrap();
        let reader = index.reader().unwrap();
        let profile = RelevanceProfile::D5cPreviewV1(profile);
        search_reader_with_profile(
            &index,
            &fields,
            &reader,
            query,
            limit,
            filters,
            ProfileExecution {
                profile: &profile,
                query_time_epoch_seconds,
            },
        )
        .unwrap()
    }

    #[cfg(feature = "internal-d5c-preview")]
    fn property_boost(id: &str, property: &str, value: RankingScalar) -> PropertyRule {
        PropertyRule {
            id: id.to_owned(),
            property: property.to_owned(),
            predicate: PropertyPredicate::Exact {
                pointer: Some(String::new()),
                value,
            },
            effect: crate::ranking::RuleEffect::Boost,
            strength: crate::ranking::RuleStrength::High,
        }
    }

    #[test]
    fn query_classifier_is_conservative_and_preserves_explicit_syntax() {
        assert_eq!(classify_query("IIA 2 line"), QueryPlanKind::Identifier);
        assert_eq!(classify_query("iia 2 line"), QueryPlanKind::Ordinary);
        assert_eq!(
            classify_query("RFC 9110 caching"),
            QueryPlanKind::Identifier
        );
        assert_eq!(classify_query("CVE-2026-1234"), QueryPlanKind::Identifier);
        assert_eq!(
            classify_query("dungeons and dragons"),
            QueryPlanKind::Ordinary
        );
        assert_eq!(classify_query("top 10 books"), QueryPlanKind::Ordinary);
        assert_eq!(classify_query("\"IIA 2 line\""), QueryPlanKind::Explicit);
        assert_eq!(classify_query("IIA OR line"), QueryPlanKind::Explicit);
        assert_eq!(classify_query("title:IIA"), QueryPlanKind::Explicit);
        assert_eq!(classify_query("CVE-*"), QueryPlanKind::Explicit);
    }

    #[test]
    fn explicit_field_boolean_phrase_wildcard_group_and_range_queries_bypass_assistance() {
        let temporary = tempdir().unwrap();
        let vault = temporary.path().join("vault");
        let data = temporary.path().join("data");
        fs::create_dir(&vault).unwrap();
        fs::write(
            vault.join("alpha.md"),
            "---\ntitle: Alpha Beta\n---\n\n# Alpha\nalpha beta gamma",
        )
        .unwrap();
        let config = Config {
            vaults: vec![VaultRegistration {
                id: "fixture".into(),
                path: vault,
                room: None,
            }],
            ..Config::default()
        };
        build_index(&config, &data).unwrap();
        let (index, fields) = open_index(&data).unwrap();
        let reader = index.reader().unwrap();
        let searcher = reader.searcher();
        let context = NativeSearchContext {
            index: &index,
            fields: &fields,
            searcher: &searcher,
            resource: None,
        };

        for (query, expected_hits) in [
            ("title:Alpha", 1),
            ("alpha AND beta", 1),
            ("\"alpha beta\"", 1),
            ("alph*", 1),
            ("alph?", 0),
            ("(alpha OR missing)", 1),
            ("mtime:[0 TO 9999999999999]", 1),
        ] {
            let resolved = resolve_query_plan(std::slice::from_ref(&context), query).unwrap();
            assert_eq!(
                resolved.plan.execution,
                QueryExecutionDisposition::ExplicitBypass,
                "{query}"
            );
            assert!(resolved.plan.evidence_stages.is_empty(), "{query}");
            assert!(resolved.plan.support_probes.is_empty(), "{query}");
            assert_eq!(search(&data, query, 20).len(), expected_hits, "{query}");
        }
    }

    #[test]
    fn frontmatter_tags_are_searchable_without_outranking_stronger_text() {
        let temporary = tempdir().unwrap();
        let vault = temporary.path().join("vault");
        let data = temporary.path().join("data");
        fs::create_dir(&vault).unwrap();
        fs::write(
            vault.join("tagged.md"),
            "---\ntags: [tagbeacon]\n---\n# Tag witness\nSynthetic tag field.",
        )
        .unwrap();
        fs::write(
            vault.join("titled.md"),
            "---\ntitle: tagbeacon\n---\n# Title witness\nSynthetic title field.",
        )
        .unwrap();
        fs::write(vault.join("plain.md"), "# Plain\nNo beacon term here.").unwrap();
        build_index(
            &Config {
                vaults: vec![VaultRegistration {
                    id: "fixture".into(),
                    path: vault,
                    room: None,
                }],
                ..Config::default()
            },
            &data,
        )
        .unwrap();

        let paths: Vec<_> = search(&data, "tagbeacon", 20)
            .into_iter()
            .map(|hit| hit.path)
            .collect();
        assert!(paths.contains(&"tagged.md".to_owned()));
        assert!(paths.contains(&"titled.md".to_owned()));
        assert!(!paths.contains(&"plain.md".to_owned()));
        // Title evidence carries a higher boost than tag evidence, so the
        // title match must not fall below the tag-only match.
        let title_rank = paths.iter().position(|path| path == "titled.md").unwrap();
        let tag_rank = paths.iter().position(|path| path == "tagged.md").unwrap();
        assert!(title_rank < tag_rank);
    }

    #[test]
    fn excel_formula_text_ranks_by_the_same_text_evidence_rules_as_every_format() {
        let temporary = tempdir().unwrap();
        let vault = temporary.path().join("vault");
        let data = temporary.path().join("data");
        fs::create_dir(&vault).unwrap();
        fs::write(
            vault.join("roles.xlsx"),
            crate::formats::excel::tests::ranking_fixture(),
        )
        .unwrap();
        build_index(
            &Config {
                vaults: vec![VaultRegistration {
                    id: "fixture".into(),
                    path: vault,
                    room: None,
                }],
                ..Config::default()
            },
            &data,
        )
        .unwrap();

        let hits = search(&data, "rankbeacon", 40);
        let cells = hits
            .iter()
            .filter_map(|hit| match hit.locator.as_ref() {
                Some(crate::extract::SourceLocator::ExcelCell { cell, .. }) => {
                    Some((cell.as_str(), hit.score))
                }
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(cells.len(), 33, "formula text must remain searchable");
        // The cached value is never hidden by its weaker class.
        assert!(cells.iter().any(|(cell, _)| *cell == "B1"));
        // Each latent formula cell is a short exact match; B1's cached value
        // sits inside the sheet's long primary section. Under the identical
        // text-evidence rules of contract 10.5 the tighter matches score
        // higher, so B1 must NOT lead. The retired class bands forced B1
        // first by construction, letting the weakest text evidence in the
        // result set outrank the strongest; this assertion fails if any
        // banding comes back.
        assert_ne!(cells[0].0, "B1");
        // Latent matches with equal evidence tie exactly and order
        // deterministically; nothing about the class perturbs the score.
        let latent: Vec<f32> = cells
            .iter()
            .filter(|(cell, _)| *cell != "B1")
            .map(|(_, score)| *score)
            .collect();
        assert!(latent.windows(2).all(|pair| pair[0] == pair[1]));
    }

    #[test]
    fn spaced_and_unknown_field_syntax_never_broadens_into_ordinary_search() {
        let temporary = tempdir().unwrap();
        let vault = temporary.path().join("vault");
        let data = temporary.path().join("data");
        fs::create_dir(&vault).unwrap();
        fs::write(
            vault.join("title.md"),
            "---\ntitle: Alpha\n---\n\nbody without the token",
        )
        .unwrap();
        fs::write(vault.join("body.md"), "title alpha").unwrap();
        build_index(
            &Config {
                vaults: vec![VaultRegistration {
                    id: "fixture".into(),
                    path: vault,
                    room: None,
                }],
                ..Config::default()
            },
            &data,
        )
        .unwrap();

        let paths: Vec<_> = search(&data, "title : Alpha", 20)
            .into_iter()
            .map(|hit| hit.path)
            .collect();
        assert_eq!(paths, ["title.md"]);

        let error = search_index(
            &data,
            &LexicalSearchRequest {
                query: "bogus:Alpha".to_owned(),
                limit: 20,
                vault_id: None,
            },
        )
        .unwrap_err();
        assert!(error.to_string().contains("Field does not exist"));
    }

    #[test]
    fn native_resolution_consumes_finalized_shared_intent() {
        let temporary = tempdir().unwrap();
        let vault = temporary.path().join("vault");
        let data = temporary.path().join("data");
        fs::create_dir(&vault).unwrap();
        fs::write(vault.join("note.md"), "alpha beta").unwrap();
        let config = Config {
            vaults: vec![VaultRegistration {
                id: "fixture".into(),
                path: vault,
                room: None,
            }],
            ..Config::default()
        };
        build_index(&config, &data).unwrap();
        let (index, fields) = open_index(&data).unwrap();
        let reader = index.reader().unwrap();
        let searcher = reader.searcher();
        let context = NativeSearchContext {
            index: &index,
            fields: &fields,
            searcher: &searcher,
            resource: None,
        };

        let resolved = resolve_query_plan(std::slice::from_ref(&context), "alpha beta").unwrap();
        assert_eq!(resolved.plan.execution, QueryExecutionDisposition::Ready);
        assert_eq!(
            resolved
                .plan
                .evidence_stages
                .iter()
                .map(|stage| stage.kind)
                .collect::<Vec<_>>(),
            [
                QueryEvidenceStageKind::ExactMetadata,
                QueryEvidenceStageKind::ExactPhrase,
                QueryEvidenceStageKind::AllTerms,
            ]
        );
        assert!(
            resolved
                .plan
                .term_intents
                .iter()
                .all(|intent| intent.support == crate::query::QueryTermSupport::Useful)
        );
    }

    #[test]
    fn identifier_plan_recalls_metadata_and_rejects_missing_components() {
        let temporary = tempdir().unwrap();
        let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../../fixtures/retrieval/technical-terms");
        let config = Config {
            vaults: vec![
                VaultRegistration {
                    id: "technical-a".into(),
                    path: fixture.join("vault-a"),
                    room: None,
                },
                VaultRegistration {
                    id: "technical-b".into(),
                    path: fixture.join("vault-b"),
                    room: None,
                },
            ],
            ..Config::default()
        };
        build_index(&config, temporary.path()).unwrap();
        let (index, fields) = open_index(temporary.path()).unwrap();
        let reader = index.reader().unwrap();
        let filename_matches =
            matching_paths_for_field(&index, &fields, &reader, fields.filename, "IIA 2 line");
        let alias_matches =
            matching_paths_for_field(&index, &fields, &reader, fields.aliases, "IIA 2 line");
        assert!(filename_matches.contains("IIA-2-line.md"));
        assert!(alias_matches.contains("alias-guidance.md"));
        let searcher = reader.searcher();
        let context = NativeSearchContext {
            index: &index,
            fields: &fields,
            searcher: &searcher,
            resource: None,
        };
        let lowercase = resolve_query_plan(std::slice::from_ref(&context), "iia 2 line").unwrap();
        assert_eq!(lowercase.plan.kind, QueryPlanKind::Identifier);

        let hits = search_index(
            temporary.path(),
            &LexicalSearchRequest {
                query: "IIA 2 line".into(),
                limit: 100,
                vault_id: None,
            },
        )
        .unwrap();
        let paths: HashSet<_> = hits.iter().map(|hit| hit.path.as_str()).collect();

        for expected in [
            "IIA-2-line.md",
            "alias-guidance.md",
            "title-guidance.md",
            "heading-guidance.md",
            "body-guidance.md",
            "multi-section.md",
        ] {
            assert!(paths.contains(expected), "missing expected path {expected}");
        }
        for rejected in [
            "line-frequency.md",
            "missing-iia-2.md",
            "missing-iia-line.md",
            "missing-2-line.md",
            "partial-tokens.md",
        ] {
            assert!(
                !paths.contains(rejected),
                "unexpected hard negative {rejected}"
            );
        }
        let lowercase_hits = search_index(
            temporary.path(),
            &LexicalSearchRequest {
                query: "iia 2 line".into(),
                limit: 100,
                vault_id: None,
            },
        )
        .unwrap();
        let lowercase_paths: HashSet<_> =
            lowercase_hits.iter().map(|hit| hit.path.as_str()).collect();
        for expected in [
            "IIA-2-line.md",
            "alias-guidance.md",
            "title-guidance.md",
            "heading-guidance.md",
            "body-guidance.md",
            "multi-section.md",
        ] {
            assert!(
                lowercase_paths.contains(expected),
                "lowercase query is missing expected path {expected}"
            );
        }
        for rejected in [
            "line-frequency.md",
            "missing-iia-2.md",
            "missing-iia-line.md",
            "missing-2-line.md",
            "partial-tokens.md",
        ] {
            assert!(
                !lowercase_paths.contains(rejected),
                "lowercase query admitted hard negative {rejected}"
            );
        }

        let body_rank = hits
            .iter()
            .position(|hit| hit.path == "body-guidance.md")
            .unwrap();
        for exact_path in [
            "IIA-2-line.md",
            "alias-guidance.md",
            "title-guidance.md",
            "multi-section.md",
        ] {
            let exact_rank = hits.iter().position(|hit| hit.path == exact_path).unwrap();
            assert!(
                exact_rank < body_rank,
                "{exact_path} must outrank body phrase evidence"
            );
        }
    }

    #[test]
    fn native_tiers_order_exact_then_phrase_then_all_terms() {
        let temporary = tempdir().unwrap();
        build_index(&technical_fixture_config(), temporary.path()).unwrap();

        let hits = search(temporary.path(), "lexical ladder exact", 20);
        let exact_rank = hits
            .iter()
            .position(|hit| hit.path == "lexical-ladder-exact.md")
            .unwrap();
        let phrase_rank = hits
            .iter()
            .position(|hit| hit.path == "lexical-ladder-phrase.md")
            .unwrap();
        let all_terms_rank = hits
            .iter()
            .position(|hit| hit.path == "lexical-ladder-all-terms.md")
            .unwrap();
        assert!(exact_rank < phrase_rank);
        assert!(phrase_rank < all_terms_rank);
    }

    #[test]
    fn rfc_and_cve_anchors_survive_only_with_every_anchor_component() {
        let temporary = tempdir().unwrap();
        build_index(&technical_fixture_config(), temporary.path()).unwrap();

        let rfc = search(temporary.path(), "RFC 9110 unfindablecontext", 20);
        assert!(rfc.iter().any(|hit| hit.path == "rfc-9110.md"));
        assert!(search(temporary.path(), "RFC 9999 unfindablecontext", 20).is_empty());

        let cve = search(temporary.path(), "CVE-2026-1234 unfindablecontext", 20);
        assert!(cve.iter().any(|hit| hit.path == "cve-2026-1234.md"));
        assert!(search(temporary.path(), "CVE-2026-9999 unfindablecontext", 20).is_empty());
    }

    #[test]
    fn relaxable_backend_tokenless_terms_do_not_abort_native_search() {
        let temporary = tempdir().unwrap();
        let vault = temporary.path().join("vault");
        let data = temporary.path().join("data");
        fs::create_dir(&vault).unwrap();
        fs::write(vault.join("cache.md"), "cache").unwrap();
        build_index(
            &Config {
                vaults: vec![VaultRegistration {
                    id: "fixture".into(),
                    path: vault,
                    room: None,
                }],
                ..Config::default()
            },
            &data,
        )
        .unwrap();

        let query = format!("cache {}", "z".repeat(41));
        let hits = search(&data, &query, 20);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].path, "cache.md");
    }

    #[test]
    fn technical_identifier_anchor_requires_exact_identifier_evidence() {
        let temporary = tempdir().unwrap();
        let vault = temporary.path().join("vault");
        let data = temporary.path().join("data");
        fs::create_dir(&vault).unwrap();
        fs::write(
            vault.join("separated.md"),
            "CVE mitigation in 2026 addressed item 1234",
        )
        .unwrap();
        fs::write(vault.join("exact.md"), "CVE-2026-1234 mitigation").unwrap();
        fs::write(vault.join("spaced.md"), "CVE 2026 1234 mitigation").unwrap();
        fs::write(vault.join("rfc-exact.md"), "RFC 9110 caching").unwrap();
        fs::write(
            vault.join("rfc-separated.md"),
            "RFC caching guidance eventually mentions 9110",
        )
        .unwrap();
        build_index(
            &Config {
                vaults: vec![VaultRegistration {
                    id: "fixture".into(),
                    path: vault,
                    room: None,
                }],
                ..Config::default()
            },
            &data,
        )
        .unwrap();

        let paths: Vec<_> = search(&data, "CVE-2026-1234", 20)
            .into_iter()
            .map(|hit| hit.path)
            .collect();
        assert_eq!(paths, ["exact.md"]);

        let spaced_paths: Vec<_> = search(&data, "CVE 2026 1234", 20)
            .into_iter()
            .map(|hit| hit.path)
            .collect();
        assert_eq!(spaced_paths, ["spaced.md"]);

        let rfc_paths: Vec<_> = search(&data, "RFC 9110", 20)
            .into_iter()
            .map(|hit| hit.path)
            .collect();
        assert_eq!(rfc_paths, ["rfc-exact.md"]);
    }

    #[test]
    fn exact_metadata_does_not_collide_after_the_old_256_scalar_prefix() {
        let temporary = tempdir().unwrap();
        let vault = temporary.path().join("vault");
        let data = temporary.path().join("data");
        fs::create_dir(&vault).unwrap();
        let prefix = format!("marker {}", "a".repeat(3_000));
        fs::write(
            vault.join("long.md"),
            format!("---\ntitle: {prefix}x\n---\nbody"),
        )
        .unwrap();
        build_index(
            &Config {
                vaults: vec![VaultRegistration {
                    id: "fixture".into(),
                    path: vault,
                    room: None,
                }],
                ..Config::default()
            },
            &data,
        )
        .unwrap();
        let (index, fields) = open_index(&data).unwrap();
        let reader = index.reader().unwrap();
        let searcher = reader.searcher();
        let context = NativeSearchContext {
            index: &index,
            fields: &fields,
            searcher: &searcher,
            resource: None,
        };
        let resolved =
            resolve_query_plan(std::slice::from_ref(&context), &format!("{prefix}y")).unwrap();
        let exact_stage = resolved
            .plan
            .evidence_stages
            .iter()
            .find(|stage| stage.kind == QueryEvidenceStageKind::ExactMetadata)
            .unwrap();
        let exact = compile_evidence_stage(
            &index,
            &fields,
            &resolved.plan,
            exact_stage,
            &resolved.prefix_expansions,
        )
        .unwrap()
        .unwrap();
        let hits =
            collect_stable_hits(&searcher, exact.as_ref(), &fields, None, 20, &searcher).unwrap();
        assert!(hits.is_empty());
    }

    #[test]
    fn ordinary_exact_support_and_prefix_search_are_accent_insensitive() {
        let temporary = tempdir().unwrap();
        let vault = temporary.path().join("vault");
        let data = temporary.path().join("data");
        fs::create_dir(&vault).unwrap();
        fs::write(
            vault.join("accented.md"),
            "---\ntitle: Résumé Cache\n---\n# Café\nnaïve e\u{301}lan",
        )
        .unwrap();
        build_index(
            &Config {
                vaults: vec![VaultRegistration {
                    id: "fixture".into(),
                    path: vault,
                    room: None,
                }],
                ..Config::default()
            },
            &data,
        )
        .unwrap();

        for query in [
            "Résumé Cache",
            "Resume Cache",
            "Re\u{301}sume\u{301} Cache",
            "naive elan",
            "NAÏVE ÉLAN",
            "cafe*",
            "café*",
            "resu",
            "résu",
        ] {
            let paths: Vec<_> = search(&data, query, 20)
                .into_iter()
                .map(|hit| hit.path)
                .collect();
            assert_eq!(paths, ["accented.md"], "{query}");
        }
        assert!(search(&data, "cafeteria*", 20).is_empty());
    }

    #[test]
    fn bounded_prefix_evidence_is_last_and_uses_capped_expansions() {
        let temporary = tempdir().unwrap();
        build_index(&technical_fixture_config(), temporary.path()).unwrap();
        let (index, fields) = open_index(temporary.path()).unwrap();
        let reader = index.reader().unwrap();
        let searcher = reader.searcher();
        let context = NativeSearchContext {
            index: &index,
            fields: &fields,
            searcher: &searcher,
            resource: None,
        };

        let resolved = resolve_query_plan(std::slice::from_ref(&context), "prefixab").unwrap();
        assert_eq!(
            resolved.plan.evidence_stages.last().map(|stage| stage.kind),
            Some(QueryEvidenceStageKind::Prefix)
        );
        assert!(
            resolved
                .prefix_expansions
                .values()
                .all(|expansions| expansions.len()
                    <= resolved.plan.bounds.max_prefix_expansions_per_term)
        );
        let hits = search(temporary.path(), "prefixab", 20);
        assert!(hits.iter().any(|hit| hit.path == "prefix-evidence.md"));
    }

    #[test]
    fn prefix_specific_hit_precedes_a_partial_coverage_result_window() {
        let temporary = tempdir().unwrap();
        let vault = temporary.path().join("vault");
        let data = temporary.path().join("data");
        fs::create_dir(&vault).unwrap();
        fs::write(
            vault.join("orchard-adoption.md"),
            "---\ntitle: Orchard Adoption\n---\nSynthetic target.",
        )
        .unwrap();
        for index in 0..24 {
            fs::write(
                vault.join(format!("orchard-decoy-{index:02}.md")),
                format!("---\ntitle: Orchard\n---\nSynthetic decoy {index:02}."),
            )
            .unwrap();
        }
        build_index(
            &Config {
                vaults: vec![VaultRegistration {
                    id: "fixture".into(),
                    path: vault,
                    room: None,
                }],
                ..Config::default()
            },
            &data,
        )
        .unwrap();

        let (index, fields) = open_index(&data).unwrap();
        let reader = index.reader().unwrap();
        let searcher = reader.searcher();
        let context = NativeSearchContext {
            index: &index,
            fields: &fields,
            searcher: &searcher,
            resource: None,
        };
        let resolved = resolve_query_plan(std::slice::from_ref(&context), "orchard adop").unwrap();
        assert_eq!(
            resolved
                .plan
                .evidence_stages
                .iter()
                .map(|stage| stage.kind)
                .collect::<Vec<_>>(),
            [
                QueryEvidenceStageKind::ExactMetadata,
                QueryEvidenceStageKind::ExactPhrase,
                QueryEvidenceStageKind::PrefixMetadata,
                QueryEvidenceStageKind::AllTerms,
                QueryEvidenceStageKind::Prefix,
                QueryEvidenceStageKind::PartialCoverage,
            ]
        );

        let hits = search(&data, "orchard adop", 8);
        assert_eq!(hits.len(), 8);
        assert_eq!(hits[0].path, "orchard-adoption.md");
        assert!(
            hits[1..]
                .iter()
                .all(|hit| hit.path.starts_with("orchard-decoy-"))
        );
    }

    /// A note named for the words beats a note that merely mentions the
    /// letters that were typed.
    ///
    /// Stages fill the visible window in order, so with the all-terms tier
    /// first a passing sentence carrying both literal stems outranked the
    /// filename and title carrying the words those stems abbreviate. That is
    /// the reported `vuln meet` behaviour, reduced to invented words.
    #[test]
    fn a_named_note_outranks_a_body_mention_of_the_typed_letters() {
        let temporary = tempdir().unwrap();
        let vault = temporary.path().join("vault");
        let data = temporary.path().join("data");
        fs::create_dir(&vault).unwrap();
        fs::write(
            vault.join("Zorbification-Quarries.md"),
            "---\ntitle: Zorbification Quarries\n---\nNotes from the recurring session.",
        )
        .unwrap();
        // Body prose carrying both typed stems literally but not adjacently,
        // which is the realistic shape: an exact phrase match is separately
        // strong evidence and is not what this test is about.
        fs::write(
            vault.join("random-ops.md"),
            "---\ntitle: Random Ops Note\n---\nDiscuss zorb topics, then quar planning next week.",
        )
        .unwrap();
        build_index(
            &Config {
                vaults: vec![VaultRegistration {
                    id: "fixture".into(),
                    path: vault,
                    room: None,
                }],
                ..Config::default()
            },
            &data,
        )
        .unwrap();

        let (index, fields) = open_index(&data).unwrap();
        let reader = index.reader().unwrap();
        let searcher = reader.searcher();
        let context = NativeSearchContext {
            index: &index,
            fields: &fields,
            searcher: &searcher,
            resource: None,
        };
        let resolved = resolve_query_plan(std::slice::from_ref(&context), "zorb quar").unwrap();
        assert_eq!(
            resolved
                .plan
                .evidence_stages
                .iter()
                .map(|stage| stage.kind)
                .collect::<Vec<_>>(),
            [
                QueryEvidenceStageKind::ExactMetadata,
                QueryEvidenceStageKind::ExactPhrase,
                QueryEvidenceStageKind::PrefixMetadata,
                QueryEvidenceStageKind::AllTerms,
                QueryEvidenceStageKind::Prefix,
            ]
        );

        let hits = search(&data, "zorb quar", 8);
        assert_eq!(hits[0].path, "Zorbification-Quarries.md");
        assert_eq!(hits[1].path, "random-ops.md");
    }

    /// An abbreviation stays expandable even once its exact form exists.
    ///
    /// One unrelated note holding a bare `zorb` used to make the stem
    /// "supported", which suppressed prefix assistance for the whole query and
    /// left a note whose title is the long form unreachable. The stem existing
    /// somewhere else says nothing about what the reader meant here.
    #[test]
    fn an_abbreviation_stays_expandable_after_its_exact_form_appears_elsewhere() {
        fn resolve(
            vault: &std::path::Path,
            data: &std::path::Path,
            query: &str,
        ) -> ResolvedLexicalPlan {
            let (index, fields) = open_index(data).unwrap();
            let reader = index.reader().unwrap();
            let searcher = reader.searcher();
            let context = NativeSearchContext {
                index: &index,
                fields: &fields,
                searcher: &searcher,
                resource: None,
            };
            let _ = vault;
            resolve_query_plan(std::slice::from_ref(&context), query).unwrap()
        }

        let temporary = tempdir().unwrap();
        let vault = temporary.path().join("vault");
        let data = temporary.path().join("data");
        fs::create_dir(&vault).unwrap();
        fs::write(
            vault.join("quarry-target.md"),
            "---\ntitle: Zorbification Meetings\n---\nSynthetic abbreviation target.",
        )
        .unwrap();
        let config = Config {
            vaults: vec![VaultRegistration {
                id: "fixture".into(),
                path: vault.clone(),
                room: None,
            }],
            ..Config::default()
        };
        build_index(&config, &data).unwrap();

        let alone = resolve(&vault, &data, "zorb meetings");
        assert_eq!(
            alone
                .plan
                .evidence_stages
                .iter()
                .map(|stage| stage.kind)
                .collect::<Vec<_>>(),
            [
                QueryEvidenceStageKind::ExactMetadata,
                QueryEvidenceStageKind::ExactPhrase,
                QueryEvidenceStageKind::PrefixMetadata,
                QueryEvidenceStageKind::AllTerms,
                QueryEvidenceStageKind::Prefix,
                QueryEvidenceStageKind::PartialCoverage,
            ]
        );
        assert_eq!(
            search(&data, "zorb meetings", 8)[0].path,
            "quarry-target.md"
        );

        // One unrelated note now carries the bare stem as ordinary prose.
        fs::write(
            vault.join("quarry-unrelated.md"),
            "---\ntitle: Quarry Notes\n---\nzorb appears here as shorthand.",
        )
        .unwrap();
        let restated = temporary.path().join("data-restated");
        build_index(&config, &restated).unwrap();

        let together = resolve(&vault, &restated, "zorb meetings");
        let stem = together
            .plan
            .term_intents
            .iter()
            .find(|intent| intent.text == "zorb")
            .expect("the stem is a planned term");
        // The stem is genuinely supported now, and that must not matter.
        assert_eq!(stem.support, crate::query::QueryTermSupport::Useful);
        assert!(
            together
                .plan
                .evidence_stages
                .iter()
                .any(|stage| stage.kind == QueryEvidenceStageKind::PrefixMetadata),
            "prefix assistance must survive the stem existing elsewhere"
        );
        let expansions = together
            .prefix_expansions
            .values()
            .flatten()
            .collect::<Vec<_>>();
        // The stem expands to itself as well as to the long form, so an exact
        // match stays reachable rather than becoming a precondition.
        assert!(expansions.iter().any(|term| *term == "zorb"));
        assert!(expansions.iter().any(|term| *term == "zorbification"));

        let hits = search(&restated, "zorb meetings", 8);
        assert_eq!(hits[0].path, "quarry-target.md");
    }

    /// A typed stem whose useful expansion sorts alphabetically behind more
    /// than a full expansion budget of body-only neighbours. Selecting the
    /// budget lexicographically drops the title term before any stage runs, so
    /// this fails on candidate selection rather than on ranking.
    #[test]
    fn a_titled_expansion_survives_a_full_budget_of_alphabetically_earlier_neighbours() {
        let temporary = tempdir().unwrap();
        let vault = temporary.path().join("vault");
        let data = temporary.path().join("data");
        fs::create_dir(&vault).unwrap();
        fs::write(
            vault.join("pinnacle-target.md"),
            "---\ntitle: Quarry Zorbification\n---\nSynthetic target.",
        )
        .unwrap();
        // Every decoy term shares the typed stem and sorts before the title
        // term, and there are more of them than the kept expansion budget.
        let neighbours = [
            "zorba",
            "zorbable",
            "zorbaceous",
            "zorbadic",
            "zorbage",
            "zorbal",
            "zorbamic",
            "zorbane",
            "zorbant",
            "zorbara",
            "zorbate",
            "zorbatic",
            "zorbature",
            "zorbec",
            "zorbedly",
            "zorbelic",
            "zorbenic",
            "zorbeous",
            "zorberly",
            "zorbetic",
        ];
        assert!(neighbours.len() > crate::MAX_PREFIX_EXPANSIONS_PER_TERM);
        assert!(
            neighbours
                .iter()
                .all(|neighbour| *neighbour < "zorbification")
        );
        for (index, neighbour) in neighbours.iter().enumerate() {
            fs::write(
                vault.join(format!("pinnacle-decoy-{index:02}.md")),
                format!("---\ntitle: Quarry\n---\nSynthetic decoy {neighbour}."),
            )
            .unwrap();
        }
        build_index(
            &Config {
                vaults: vec![VaultRegistration {
                    id: "fixture".into(),
                    path: vault,
                    room: None,
                }],
                ..Config::default()
            },
            &data,
        )
        .unwrap();

        let (index, fields) = open_index(&data).unwrap();
        let reader = index.reader().unwrap();
        let searcher = reader.searcher();
        let context = NativeSearchContext {
            index: &index,
            fields: &fields,
            searcher: &searcher,
            resource: None,
        };
        let resolved = resolve_query_plan(std::slice::from_ref(&context), "quarry zorb").unwrap();
        // Every eligible term is expanded now, so select the stem's own set
        // rather than whichever entry happens to sort first.
        let stem_index = resolved
            .plan
            .term_intents
            .iter()
            .find(|intent| intent.text == "zorb")
            .expect("the stem is a planned term")
            .index;
        let expansions = resolved.prefix_expansions.get(&stem_index).unwrap();
        assert!(expansions.len() <= crate::MAX_PREFIX_EXPANSIONS_PER_TERM);
        assert!(expansions.iter().any(|term| term == "zorbification"));

        let hits = search(&data, "quarry zorb", 8);
        assert_eq!(hits.len(), 8);
        assert_eq!(hits[0].path, "pinnacle-target.md");
    }

    #[test]
    fn high_frequency_prefix_dictionary_walk_stops_at_the_declared_cap() {
        let temporary = tempdir().unwrap();
        let vault = temporary.path().join("vault");
        let data = temporary.path().join("data");
        fs::create_dir(&vault).unwrap();
        for index in 0..40 {
            fs::write(
                vault.join(format!("crowded-{index:02}.md")),
                format!("crowdedprefix{index:02}"),
            )
            .unwrap();
        }
        let config = Config {
            vaults: vec![VaultRegistration {
                id: "prefixes".into(),
                path: vault,
                room: None,
            }],
            ..Config::default()
        };
        build_index(&config, &data).unwrap();
        let (index, fields) = open_index(&data).unwrap();
        let reader = index.reader().unwrap();
        let searcher = reader.searcher();
        let context = NativeSearchContext {
            index: &index,
            fields: &fields,
            searcher: &searcher,
            resource: None,
        };

        let resolved = resolve_query_plan(std::slice::from_ref(&context), "crowdedpre").unwrap();
        let expansions = resolved.prefix_expansions.values().next().unwrap();
        assert_eq!(
            expansions.len(),
            resolved.plan.bounds.max_prefix_expansions_per_term
        );
        assert!(search(&data, "crowdedpre", 100).len() <= expansions.len());
    }

    /// The prefix ladder must expand a term the same way for the same vault,
    /// however the index arrived at it.
    ///
    /// The budget used to be charged per dictionary entry visited, against one
    /// counter shared by every segment and every prefix field. A term recorded
    /// in three segments therefore cost three times what the same term costs
    /// once the segments are merged, so an incrementally maintained index
    /// reached the cap after fewer *distinct* terms than a rebuild of identical
    /// content and expanded the same query to a strictly smaller set. The
    /// prefix stage then contributed different candidates to a bounded pool,
    /// which is a difference in the answer, not in its order.
    ///
    /// Charging the budget per distinct term, per segment, removes the
    /// dependence: `limit` entries are taken from each stream, and the first
    /// `limit` of their union is exactly the globally first `limit`, because a
    /// term among the globally first `limit` can be preceded within any one
    /// segment only by terms that also precede it globally.
    ///
    /// Mutation check: restore the shared `examined_terms` counter and this
    /// fails. Both indexes still reach the cap, but on different terms — the
    /// rebuild returns `crowdedprefix00` and then `03`..`17`, having spent two
    /// of its sixteen visits re-reading `01` and `02` out of a second prefix
    /// field, while the incremental index returns `00`..`15`. Neither is the
    /// vault's answer to the query; they are two readings of its segment
    /// layout.
    #[test]
    fn prefix_expansion_depends_on_the_vocabulary_not_on_the_segment_layout() {
        let temporary = tempdir().unwrap();
        let vault = temporary.path().join("vault");
        let incremental = temporary.path().join("incremental");
        let rebuilt = temporary.path().join("rebuilt");
        fs::create_dir(&vault).unwrap();
        let write = |count: usize| {
            for index in 0..count {
                fs::write(
                    vault.join(format!("crowded-{index:02}.md")),
                    format!("crowdedprefix{index:02}"),
                )
                .unwrap();
            }
        };
        write(10);
        let config = Config {
            vaults: vec![VaultRegistration {
                id: "prefixes".into(),
                path: vault.clone(),
                room: None,
            }],
            ..Config::default()
        };

        // Four publications, so the live vocabulary ends up spread over
        // several segments instead of arriving in one.
        build_index(&config, &incremental).unwrap();
        let runtime = crate::runtime::SearchRuntime::new();
        let mut manager =
            crate::runtime::IndexManager::open(config.clone(), &incremental, runtime.clone())
                .unwrap();
        for count in [20, 30, 40] {
            write(count);
            manager.reconcile(config.clone()).unwrap();
        }
        manager.shutdown().unwrap();

        build_index(&config, &rebuilt).unwrap();

        let expansions_for = |data: &Path| {
            let (index, fields) = open_index(data).unwrap();
            let reader = index.reader().unwrap();
            let searcher = reader.searcher();
            let context = NativeSearchContext {
                index: &index,
                fields: &fields,
                searcher: &searcher,
                resource: None,
            };
            let segments = searcher.segment_readers().len();
            let resolved =
                resolve_query_plan(std::slice::from_ref(&context), "crowdedpre").unwrap();
            (
                segments,
                resolved.prefix_expansions.values().next().unwrap().clone(),
            )
        };
        let (incremental_segments, incremental_expansions) = expansions_for(&incremental);
        let (rebuilt_segments, rebuilt_expansions) = expansions_for(&rebuilt);

        assert!(
            incremental_segments > rebuilt_segments,
            "the fixture must actually produce two different segment layouts, \
             or this asserts nothing (incremental {incremental_segments}, \
             rebuilt {rebuilt_segments})"
        );
        assert_eq!(
            rebuilt_expansions.len(),
            crate::query::MAX_PREFIX_EXPANSIONS_PER_TERM,
            "the corpus must be crowded enough to reach the cap"
        );
        assert_eq!(
            incremental_expansions, rebuilt_expansions,
            "the same vocabulary must expand to the same terms"
        );
        assert_eq!(
            search(&incremental, "crowdedpre", 100)
                .into_iter()
                .map(|hit| hit.path)
                .collect::<Vec<_>>(),
            search(&rebuilt, "crowdedpre", 100)
                .into_iter()
                .map(|hit| hit.path)
                .collect::<Vec<_>>(),
        );
    }

    #[test]
    fn stable_ties_survive_candidate_cutoff_and_rebuilds() {
        let temporary = tempdir().unwrap();
        let vault = temporary.path().join("vault");
        let first_data = temporary.path().join("first-data");
        let second_data = temporary.path().join("second-data");
        fs::create_dir(&vault).unwrap();
        for index in 0..300 {
            fs::write(vault.join(format!("tie-{index:03}.md")), "stabletie").unwrap();
        }
        let config = Config {
            vaults: vec![VaultRegistration {
                id: "ties".into(),
                path: vault,
                room: None,
            }],
            ..Config::default()
        };
        build_index(&config, &first_data).unwrap();
        build_index(&config, &second_data).unwrap();

        let first = search(&first_data, "stabletie", 100);
        let repeated = search(&first_data, "stabletie", 100);
        let rebuilt = search(&second_data, "stabletie", 100);
        let identity = |hits: &[SearchHit]| {
            hits.iter()
                .map(|hit| (hit.chunk_id.clone(), hit.path.clone()))
                .collect::<Vec<_>>()
        };
        assert_eq!(identity(&first), identity(&repeated));
        assert_eq!(identity(&first), identity(&rebuilt));
        assert_eq!(first.len(), 100);
        let mut independently_sorted = identity(&first);
        independently_sorted.sort();
        assert_eq!(identity(&first), independently_sorted);
        assert!(
            first
                .windows(2)
                .all(|pair| { compare_hits(&pair[0], &pair[1]) != Ordering::Greater })
        );
    }

    #[test]
    fn ordinary_terms_require_all_supported_evidence_and_relax_only_unknown_filler() {
        let temporary = tempdir().unwrap();
        let vault = temporary.path().join("vault");
        let data = temporary.path().join("data");
        fs::create_dir(&vault).unwrap();
        fs::write(
            vault.join("top-10-books.md"),
            "# Reading list\ncurated titles",
        )
        .unwrap();
        fs::write(vault.join("dungeons.md"), "underground dungeons").unwrap();
        fs::write(vault.join("dragons.md"), "flying dragons").unwrap();
        let config = Config {
            vaults: vec![VaultRegistration {
                id: "technical-a".into(),
                path: vault,
                room: None,
            }],
            ..Config::default()
        };
        build_index(&config, &data).unwrap();
        let (index, fields) = open_index(&data).unwrap();
        let reader = index.reader().unwrap();
        let searcher = reader.searcher();
        let context = NativeSearchContext {
            index: &index,
            fields: &fields,
            searcher: &searcher,
            resource: None,
        };
        let list = resolve_query_plan(std::slice::from_ref(&context), "top 10 books").unwrap();
        assert_eq!(list.plan.kind, QueryPlanKind::Ordinary);

        let all_supported = search_index(
            &data,
            &LexicalSearchRequest {
                query: "dungeons dragons".into(),
                limit: 20,
                vault_id: None,
            },
        )
        .unwrap();
        assert!(all_supported.is_empty());

        let relaxed = search_index(
            &data,
            &LexicalSearchRequest {
                query: "dungeons unfindablefiller".into(),
                limit: 20,
                vault_id: None,
            },
        )
        .unwrap();
        assert_eq!(relaxed.len(), 1);
        assert_eq!(relaxed[0].path, "dungeons.md");

        let no_evidence = search_index(
            &data,
            &LexicalSearchRequest {
                query: "nowhereword unfindablefiller".into(),
                limit: 20,
                vault_id: None,
            },
        )
        .unwrap();
        assert!(no_evidence.is_empty());
        assert!(search(&data, "...", 20).is_empty());

        let explicit_or = search_index(
            &data,
            &LexicalSearchRequest {
                query: "dungeons OR dragons".into(),
                limit: 20,
                vault_id: None,
            },
        )
        .unwrap();
        assert_eq!(explicit_or.len(), 2);
        let explicit_and = search_index(
            &data,
            &LexicalSearchRequest {
                query: "dungeons AND dragons".into(),
                limit: 20,
                vault_id: None,
            },
        )
        .unwrap();
        assert!(explicit_and.is_empty());
    }

    #[test]
    #[cfg(feature = "internal-d5c-preview")]
    fn native_lexical_v1_profile_is_exact_and_invalid_d5c_never_falls_back() {
        let temporary = tempdir().unwrap();
        let vault = temporary.path().join("vault");
        let data = temporary.path().join("data");
        fs::create_dir(&vault).unwrap();
        fs::write(vault.join("note.md"), "profilecompat profilecompat").unwrap();
        build_index(
            &Config {
                vaults: vec![VaultRegistration {
                    id: "fixture".into(),
                    path: vault,
                    room: None,
                }],
                ..Config::default()
            },
            &data,
        )
        .unwrap();
        let (index, fields) = open_index(&data).unwrap();
        let reader = index.reader().unwrap();
        let legacy = search_reader(
            &index,
            &fields,
            &reader,
            "profilecompat",
            20,
            &SearchFilters::default(),
        )
        .unwrap();
        let profiled = search_reader_with_profile(
            &index,
            &fields,
            &reader,
            "profilecompat",
            20,
            &SearchFilters::default(),
            ProfileExecution {
                profile: &RelevanceProfile::LexicalV1,
                query_time_epoch_seconds: 2_000_000_000,
            },
        )
        .unwrap();
        assert_eq!(
            serde_json::to_vec(&profiled).unwrap(),
            serde_json::to_vec(&legacy).unwrap()
        );

        let mut invalid = D5cRelevanceProfile::preview();
        invalid.profile_id = "malformed-preview".into();
        let error = search_reader_with_profile(
            &index,
            &fields,
            &reader,
            "profilecompat",
            20,
            &SearchFilters::default(),
            ProfileExecution {
                profile: &RelevanceProfile::D5cPreviewV1(invalid),
                query_time_epoch_seconds: 2_000_000_000,
            },
        )
        .unwrap_err();
        assert!(error.to_string().contains("invalid_relevance_profile"));
    }

    #[test]
    #[cfg(feature = "internal-d5c-preview")]
    fn native_d5c_recency_reorders_only_within_lexical_evidence_bands() {
        let temporary = tempdir().unwrap();
        let vault = temporary.path().join("vault");
        let data = temporary.path().join("data");
        fs::create_dir(&vault).unwrap();
        let old_exact = vault.join("old-exact.md");
        let recent_phrase = vault.join("recent-phrase.md");
        let old_same_tier = vault.join("old-same-tier.md");
        let recent_same_tier = vault.join("recent-same-tier.md");
        fs::write(
            &old_exact,
            "---\ntitle: Native Ranking\n---\ncanonical material",
        )
        .unwrap();
        fs::write(&recent_phrase, "native ranking in a recent body").unwrap();
        fs::write(&old_same_tier, "same intervening words recency").unwrap();
        fs::write(&recent_same_tier, "same different words recency").unwrap();
        let old_time = FileTime::from_unix_time(1_600_000_000, 0);
        let recent_time = FileTime::from_unix_time(1_999_999_900, 0);
        set_file_mtime(&old_exact, old_time).unwrap();
        set_file_mtime(&old_same_tier, old_time).unwrap();
        set_file_mtime(&recent_phrase, recent_time).unwrap();
        set_file_mtime(&recent_same_tier, recent_time).unwrap();
        build_index(
            &Config {
                vaults: vec![VaultRegistration {
                    id: "fixture".into(),
                    path: vault,
                    room: None,
                }],
                ..Config::default()
            },
            &data,
        )
        .unwrap();

        let mut profile = D5cRelevanceProfile::preview();
        profile.recency = Some(crate::ranking::RecencyRule {
            id: "recent".into(),
            clock: crate::ranking::RecencyClock::SourceMtime,
            horizon: crate::ranking::RecencyHorizon::Week,
            strength: crate::ranking::RuleStrength::High,
        });
        let lexical = search(&data, "native ranking", 20);
        let reranked = d5c_search(
            &data,
            "native ranking",
            20,
            profile.clone(),
            2_000_000_000,
            &SearchFilters::default(),
        );
        assert_eq!(reranked[0].path, "old-exact.md");
        assert_eq!(reranked[1].path, "recent-phrase.md");
        let lexical_scores: BTreeMap<_, _> = lexical
            .iter()
            .map(|hit| (hit.chunk_id.as_str(), hit.score))
            .collect();
        assert!(
            reranked.iter().all(|hit| {
                lexical_scores.get(hit.chunk_id.as_str()).copied() == Some(hit.score)
            })
        );

        let same_tier = d5c_search(
            &data,
            "same recency",
            20,
            profile,
            2_000_000_000,
            &SearchFilters::default(),
        );
        assert_eq!(same_tier[0].path, "recent-same-tier.md");
        assert_eq!(same_tier[1].path, "old-same-tier.md");
    }

    #[test]
    #[cfg(feature = "internal-d5c-preview")]
    fn native_d5c_typed_property_fans_out_to_a_non_owner_chunk() {
        let temporary = tempdir().unwrap();
        let vault = temporary.path().join("vault");
        let data = temporary.path().join("data");
        fs::create_dir(&vault).unwrap();
        fs::write(
            vault.join("typed.md"),
            format!(
                "---\npriority: 7\n---\n# Intro\n{}\n# Target\nfanoutneedle",
                "filler ".repeat(2_000)
            ),
        )
        .unwrap();
        fs::write(
            vault.join("string.md"),
            "---\npriority: \"7\"\n---\nfanoutneedle fanoutneedle fanoutneedle",
        )
        .unwrap();
        fs::write(vault.join("missing.md"), "fanoutneedle fanoutneedle").unwrap();
        build_index(
            &Config {
                vaults: vec![VaultRegistration {
                    id: "fixture".into(),
                    path: vault,
                    room: None,
                }],
                ..Config::default()
            },
            &data,
        )
        .unwrap();

        let lexical = search(&data, "fanoutneedle", 20);
        assert_ne!(lexical[0].path, "typed.md");
        let neutral_order: Vec<_> = lexical
            .iter()
            .filter(|hit| matches!(hit.path.as_str(), "string.md" | "missing.md"))
            .map(|hit| hit.path.clone())
            .collect();
        let mut profile = D5cRelevanceProfile::preview();
        profile.property_rules = vec![property_boost(
            "priority-i64",
            "priority",
            RankingScalar::i64(7),
        )];
        let reranked = d5c_search(
            &data,
            "fanoutneedle",
            20,
            profile,
            2_000_000_000,
            &SearchFilters::default(),
        );
        assert_eq!(reranked[0].path, "typed.md");
        assert_eq!(reranked[0].heading_path, ["Target"]);
        let reranked_neutral_order: Vec<_> = reranked
            .iter()
            .filter(|hit| matches!(hit.path.as_str(), "string.md" | "missing.md"))
            .map(|hit| hit.path.clone())
            .collect();
        assert_eq!(reranked_neutral_order, neutral_order);
    }

    #[test]
    #[cfg(feature = "internal-d5c-preview")]
    fn native_d5c_property_ranges_preserve_mixed_scalar_types() {
        let temporary = tempdir().unwrap();
        let vault = temporary.path().join("vault");
        let data = temporary.path().join("data");
        fs::create_dir(&vault).unwrap();
        fs::write(
            vault.join("typed.md"),
            "---\nscore: 7\nlimit: 18446744073709551615\nratio: 1.5\nreviewed: 2026-07-31\n---\ntypedrangeneedle",
        )
        .unwrap();
        fs::write(
            vault.join("strings.md"),
            "---\nscore: \"7\"\nlimit: \"18446744073709551615\"\nratio: \"1.5\"\nreviewed: not-a-date\n---\ntypedrangeneedle typedrangeneedle",
        )
        .unwrap();
        fs::write(vault.join("missing.md"), "typedrangeneedle").unwrap();
        build_index(
            &Config {
                vaults: vec![VaultRegistration {
                    id: "fixture".into(),
                    path: vault,
                    room: None,
                }],
                ..Config::default()
            },
            &data,
        )
        .unwrap();

        let mut profile = D5cRelevanceProfile::preview();
        profile.property_rules = vec![
            PropertyRule {
                id: "00-score".into(),
                property: "score".into(),
                predicate: PropertyPredicate::I64Range {
                    pointer: Some(String::new()),
                    min: Some("7".into()),
                    max: Some("7".into()),
                },
                effect: crate::ranking::RuleEffect::Boost,
                strength: crate::ranking::RuleStrength::Low,
            },
            PropertyRule {
                id: "01-limit".into(),
                property: "limit".into(),
                predicate: PropertyPredicate::U64Range {
                    pointer: Some(String::new()),
                    min: Some(u64::MAX.to_string()),
                    max: Some(u64::MAX.to_string()),
                },
                effect: crate::ranking::RuleEffect::Boost,
                strength: crate::ranking::RuleStrength::Low,
            },
            PropertyRule {
                id: "02-ratio".into(),
                property: "ratio".into(),
                predicate: PropertyPredicate::F64Range {
                    pointer: Some(String::new()),
                    min: Some(match RankingScalar::f64(1.5) {
                        RankingScalar::F64(value) => value,
                        _ => unreachable!(),
                    }),
                    max: Some(match RankingScalar::f64(1.5) {
                        RankingScalar::F64(value) => value,
                        _ => unreachable!(),
                    }),
                },
                effect: crate::ranking::RuleEffect::Boost,
                strength: crate::ranking::RuleStrength::Low,
            },
            PropertyRule {
                id: "03-reviewed".into(),
                property: "reviewed".into(),
                predicate: PropertyPredicate::DateRange {
                    pointer: Some(String::new()),
                    min: Some("2026-07-01".into()),
                    max: Some("2026-07-31".into()),
                },
                effect: crate::ranking::RuleEffect::Boost,
                strength: crate::ranking::RuleStrength::Low,
            },
        ];
        let hits = d5c_search(
            &data,
            "typedrangeneedle",
            20,
            profile,
            2_000_000_000,
            &SearchFilters::default(),
        );
        assert_eq!(hits[0].path, "typed.md");
        let neutral: Vec<_> = hits
            .iter()
            .filter(|hit| matches!(hit.path.as_str(), "strings.md" | "missing.md"))
            .map(|hit| hit.path.clone())
            .collect();
        let lexical: Vec<_> = search(&data, "typedrangeneedle", 20)
            .into_iter()
            .filter(|hit| matches!(hit.path.as_str(), "strings.md" | "missing.md"))
            .map(|hit| hit.path)
            .collect();
        assert_eq!(neutral, lexical);
    }

    #[test]
    #[cfg(feature = "internal-d5c-preview")]
    fn native_d5c_range_term_expansion_is_work_bounded() {
        let mut schema_builder = tantivy::schema::Schema::builder();
        let field = schema_builder.add_text_field("range", tantivy::schema::STRING);
        let index = Index::create_in_ram(schema_builder.build());
        let mut writer = index.writer(50_000_000).unwrap();
        let mut document = TantivyDocument::default();
        for ordinal in 0..=MAX_RANKING_WORK_UNITS {
            document.add_text(field, format!("value-{ordinal:05}"));
        }
        writer.add_document(document).unwrap();
        writer.commit().unwrap();
        let reader = index.reader().unwrap();
        let searcher = reader.searcher();
        let mut work_units = 0;
        let result = bounded_string_range_query(
            &searcher,
            field,
            "value-00000".to_owned(),
            "value-99999".to_owned(),
            &mut work_units,
        );
        assert!(matches!(
            result,
            Err(Error::Query(message)) if message.starts_with("ranking_work_limit_exceeded:")
        ));
    }

    #[test]
    #[cfg(feature = "internal-d5c-preview")]
    fn native_d5c_f64_ranges_treat_signed_zero_numerically() {
        let temporary = tempdir().unwrap();
        let vault = temporary.path().join("vault");
        let data = temporary.path().join("data");
        fs::create_dir(&vault).unwrap();
        fs::write(
            vault.join("negative-zero.md"),
            "---\nratio: -0.0\n---\nzerorangeneedle",
        )
        .unwrap();
        fs::write(
            vault.join("positive-zero.md"),
            "---\nratio: 0.0\n---\nzerorangeneedle",
        )
        .unwrap();
        fs::write(
            vault.join("missing.md"),
            "zerorangeneedle zerorangeneedle zerorangeneedle",
        )
        .unwrap();
        build_index(
            &Config {
                vaults: vec![VaultRegistration {
                    id: "fixture".into(),
                    path: vault,
                    room: None,
                }],
                ..Config::default()
            },
            &data,
        )
        .unwrap();

        let zero = match RankingScalar::f64(0.0) {
            RankingScalar::F64(value) => value,
            _ => unreachable!(),
        };
        let mut profile = D5cRelevanceProfile::preview();
        profile.property_rules = vec![PropertyRule {
            id: "zero-range".into(),
            property: "ratio".into(),
            predicate: PropertyPredicate::F64Range {
                pointer: Some(String::new()),
                min: Some(zero.clone()),
                max: Some(zero),
            },
            effect: crate::ranking::RuleEffect::Boost,
            strength: crate::ranking::RuleStrength::High,
        }];
        let hits = d5c_search(
            &data,
            "zerorangeneedle",
            20,
            profile,
            2_000_000_000,
            &SearchFilters::default(),
        );
        assert_eq!(hits.last().unwrap().path, "missing.md");
        assert_eq!(
            hits[..2]
                .iter()
                .map(|hit| hit.path.as_str())
                .collect::<BTreeSet<_>>(),
            BTreeSet::from(["negative-zero.md", "positive-zero.md"])
        );
    }

    #[test]
    #[cfg(feature = "internal-d5c-preview")]
    fn native_d5c_folder_rules_are_component_bounded() {
        let temporary = tempdir().unwrap();
        let vault = temporary.path().join("vault");
        let data = temporary.path().join("data");
        for folder in ["authority", "authority-old", "archive", "archive-old"] {
            fs::create_dir_all(vault.join(folder)).unwrap();
            fs::write(vault.join(folder).join("note.md"), "hierarchyneedle").unwrap();
        }
        build_index(
            &Config {
                vaults: vec![VaultRegistration {
                    id: "fixture".into(),
                    path: vault,
                    room: None,
                }],
                ..Config::default()
            },
            &data,
        )
        .unwrap();

        let mut profile = D5cRelevanceProfile::preview();
        profile.hierarchy.authority_folders = vec![crate::ranking::FolderRule {
            id: "authority".into(),
            prefix: "authority".into(),
            strength: crate::ranking::RuleStrength::Standard,
        }];
        profile.hierarchy.archive_folders = vec![crate::ranking::FolderRule {
            id: "archive".into(),
            prefix: "archive".into(),
            strength: crate::ranking::RuleStrength::High,
        }];
        let hits = d5c_search(
            &data,
            "hierarchyneedle",
            20,
            profile,
            2_000_000_000,
            &SearchFilters::default(),
        );
        assert_eq!(hits.first().unwrap().path, "authority/note.md");
        assert_eq!(hits.last().unwrap().path, "archive/note.md");
        assert!(
            hits.iter()
                .position(|hit| hit.path == "authority-old/note.md")
                .unwrap()
                < hits
                    .iter()
                    .position(|hit| hit.path == "archive/note.md")
                    .unwrap()
        );
        assert!(
            hits.iter()
                .position(|hit| hit.path == "archive-old/note.md")
                .unwrap()
                < hits
                    .iter()
                    .position(|hit| hit.path == "archive/note.md")
                    .unwrap()
        );
    }

    #[test]
    #[cfg(feature = "internal-d5c-preview")]
    fn balanced_source_shaped_markdown_fixture_runs_through_native_tantivy() {
        let corpus: serde_json::Value = serde_json::from_str(include_str!(
            "../../../../fixtures/retrieval/d5c-balanced/corpus.json"
        ))
        .unwrap();
        let fixture_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../../fixtures/retrieval/d5c-balanced");
        let temporary = tempdir().unwrap();
        let vault = temporary.path().join("vault");
        let data = temporary.path().join("data");
        fs::create_dir(&vault).unwrap();
        for source in corpus["sources"].as_array().unwrap() {
            if source["provider"]["kind"] != "markdown" {
                continue;
            }
            let relative_path = source["path"].as_str().unwrap();
            let destination = vault.join(relative_path);
            fs::create_dir_all(destination.parent().unwrap()).unwrap();
            fs::copy(
                fixture_root.join(source["provider"]["fixture_path"].as_str().unwrap()),
                &destination,
            )
            .unwrap();
            let source_key = source["source"]["source_key"].as_str().unwrap();
            let mtime = match source_key {
                "md-recent" | "md-recent-plain" => 1_999_999_900,
                "md-old" | "md-old-authority" | "md-strong" => 1_600_000_000,
                _ => 1_700_000_000,
            };
            set_file_mtime(&destination, FileTime::from_unix_time(mtime, 0)).unwrap();
        }
        build_index(
            &Config {
                vaults: vec![VaultRegistration {
                    id: "balanced-fixture".into(),
                    path: vault,
                    room: None,
                }],
                ..Config::default()
            },
            &data,
        )
        .unwrap();

        let mut profile = D5cRelevanceProfile::preview();
        profile.recency = Some(crate::ranking::RecencyRule {
            id: "00-balanced-recency".into(),
            clock: crate::ranking::RecencyClock::SourceMtime,
            horizon: crate::ranking::RecencyHorizon::Quarter,
            strength: crate::ranking::RuleStrength::Low,
        });
        profile.hierarchy.authority_folders = vec![crate::ranking::FolderRule {
            id: "10-authority".into(),
            prefix: "reference".into(),
            strength: crate::ranking::RuleStrength::Standard,
        }];
        profile.hierarchy.archive_folders = vec![crate::ranking::FolderRule {
            id: "20-archive".into(),
            prefix: "archive".into(),
            strength: crate::ranking::RuleStrength::Standard,
        }];
        profile.property_rules = vec![PropertyRule {
            id: "30-approved".into(),
            property: "approved".into(),
            predicate: PropertyPredicate::Exact {
                pointer: Some(String::new()),
                value: RankingScalar::Boolean(true),
            },
            effect: crate::ranking::RuleEffect::Boost,
            strength: crate::ranking::RuleStrength::Low,
        }];

        let recency = d5c_search(
            &data,
            "same tier recency evidence",
            20,
            profile.clone(),
            2_000_000_000,
            &SearchFilters::default(),
        );
        assert_eq!(recency[0].path, "notes/recent.md");
        assert_eq!(recency[1].path, "notes/old.md");

        let authority = d5c_search(
            &data,
            "authority note evidence",
            20,
            profile.clone(),
            2_000_000_000,
            &SearchFilters::default(),
        );
        assert_eq!(authority[0].path, "reference/old-authority.md");
        assert_eq!(authority[1].path, "notes/recent-plain.md");

        let hierarchy = d5c_search(
            &data,
            "hierarchy lookalike evidence",
            20,
            profile.clone(),
            2_000_000_000,
            &SearchFilters::default(),
        );
        assert_eq!(hierarchy[0].path, "reference/canonical.md");
        assert_eq!(hierarchy.last().unwrap().path, "archive/real.md");
        assert!(
            hierarchy
                .iter()
                .position(|hit| hit.path == "archive-old/lookalike.md")
                .unwrap()
                < hierarchy
                    .iter()
                    .position(|hit| hit.path == "archive/real.md")
                    .unwrap()
        );

        let fanout = d5c_search(
            &data,
            "fanout balanced evidence",
            20,
            profile,
            2_000_000_000,
            &SearchFilters::default(),
        );
        assert_eq!(
            fanout
                .iter()
                .filter(|hit| hit.path == "notes/multi-chunk.md")
                .count(),
            2
        );
        assert!(
            fanout
                .iter()
                .take(2)
                .all(|hit| hit.path == "notes/multi-chunk.md")
        );
    }

    #[test]
    #[cfg(feature = "internal-d5c-preview")]
    fn native_d5c_filters_before_a_deeper_bounded_pool_and_rebuilds_stably() {
        let temporary = tempdir().unwrap();
        let vault = temporary.path().join("vault");
        let first_data = temporary.path().join("first-data");
        let second_data = temporary.path().join("second-data");
        fs::create_dir_all(vault.join("allowed")).unwrap();
        fs::create_dir_all(vault.join("excluded")).unwrap();
        for index in 0..300 {
            fs::write(
                vault.join(format!("allowed/note-{index:03}.md")),
                format!("---\npriority: {index}\n---\ncutoffneedle"),
            )
            .unwrap();
        }
        fs::write(
            vault.join("excluded/special.md"),
            "---\npriority: 999\n---\ncutoffneedle",
        )
        .unwrap();
        let config = Config {
            vaults: vec![VaultRegistration {
                id: "fixture".into(),
                path: vault,
                room: None,
            }],
            ..Config::default()
        };
        build_index(&config, &first_data).unwrap();
        build_index(&config, &second_data).unwrap();

        let (index, fields) = open_index(&first_data).unwrap();
        let reader = index.reader().unwrap();
        let searcher = reader.searcher();
        let context = NativeSearchContext {
            index: &index,
            fields: &fields,
            searcher: &searcher,
            resource: None,
        };
        let resolved = resolve_query_plan(std::slice::from_ref(&context), "cutoffneedle").unwrap();
        let pool = execute_lexical_candidates(
            std::slice::from_ref(&context),
            &resolved,
            crate::ranking::MAX_RERANK_CANDIDATES,
            &SearchFilters::default(),
            &searcher,
        )
        .unwrap();
        assert_eq!(pool.len(), crate::query::MAX_CANDIDATES_PER_STAGE);
        let promoted = pool[150].hit.path.clone();
        let promoted_value = promoted
            .strip_prefix("allowed/note-")
            .and_then(|value| value.strip_suffix(".md"))
            .unwrap()
            .parse::<i64>()
            .unwrap();
        let mut profile = D5cRelevanceProfile::preview();
        profile.property_rules = vec![property_boost(
            "promote-deep-candidate",
            "priority",
            RankingScalar::i64(promoted_value),
        )];
        let filters = SearchFilters {
            path_prefix: Some("allowed/".into()),
            ..SearchFilters::default()
        };
        let first = d5c_search(
            &first_data,
            "cutoffneedle",
            5,
            profile.clone(),
            2_000_000_000,
            &filters,
        );
        assert_eq!(first[0].path, promoted);
        assert!(first.iter().all(|hit| hit.path.starts_with("allowed/")));
        assert_eq!(first.len(), 5);

        let rebuilt = d5c_search(
            &second_data,
            "cutoffneedle",
            5,
            profile,
            2_000_000_000,
            &filters,
        );
        assert_eq!(
            first
                .iter()
                .map(|hit| (&hit.chunk_id, &hit.path, hit.score))
                .collect::<Vec<_>>(),
            rebuilt
                .iter()
                .map(|hit| (&hit.chunk_id, &hit.path, hit.score))
                .collect::<Vec<_>>()
        );

        let outside_pool = (0..300)
            .map(|index| format!("allowed/note-{index:03}.md"))
            .find(|path| !pool.iter().any(|candidate| candidate.hit.path == *path))
            .unwrap();
        let outside_value = outside_pool
            .strip_prefix("allowed/note-")
            .and_then(|value| value.strip_suffix(".md"))
            .unwrap()
            .parse::<i64>()
            .unwrap();
        let mut outside_profile = D5cRelevanceProfile::preview();
        outside_profile.property_rules = vec![property_boost(
            "outside-cutoff",
            "priority",
            RankingScalar::i64(outside_value),
        )];
        let cutoff = d5c_search(
            &first_data,
            "cutoffneedle",
            100,
            outside_profile,
            2_000_000_000,
            &filters,
        );
        assert!(cutoff.iter().all(|hit| hit.path != outside_pool));
    }

    #[derive(Deserialize)]
    struct ConformanceCorpus {
        schema_version: u32,
        profile_id: String,
        documents: Vec<ConformanceDocument>,
        generated_documents: Vec<GeneratedConformanceDocuments>,
        cases: Vec<ConformanceCase>,
        bounds: ConformanceBounds,
    }

    #[derive(Deserialize)]
    struct ConformanceDocument {
        scope: String,
        path: String,
        markdown: String,
    }

    #[derive(Deserialize)]
    struct GeneratedConformanceDocuments {
        scope: String,
        path_prefix: String,
        count: usize,
        width: usize,
        markdown: String,
    }

    #[derive(Deserialize)]
    struct ConformanceCase {
        id: String,
        query: String,
        scope: String,
        limit: usize,
        assistance: String,
        execution: String,
        stages: Vec<String>,
        anchors: Vec<String>,
        #[serde(default)]
        expected_paths: Vec<ExpectedConformancePath>,
        #[serde(default)]
        excluded_paths: Vec<String>,
        prefix_expansions: Option<usize>,
        #[serde(default)]
        stable_ties: bool,
        #[serde(default)]
        combined_scope_changes_evidence: bool,
    }

    #[derive(Deserialize)]
    struct ExpectedConformancePath {
        path: String,
        tier: String,
    }

    #[derive(Deserialize)]
    struct ConformanceBounds {
        maximum_terms: usize,
        over_limit_terms: usize,
        maximum_query_bytes: usize,
        over_limit_unicode: String,
        maximum_prefix_terms: usize,
        maximum_prefix_expansions_per_term: usize,
        maximum_candidates_per_stage: usize,
        maximum_total_candidates: usize,
    }

    fn enum_name<T: serde::Serialize>(value: T) -> String {
        serde_json::to_value(value)
            .unwrap()
            .as_str()
            .unwrap()
            .to_owned()
    }

    #[test]
    fn shared_lexical_conformance_corpus_matches_native_tantivy() {
        let corpus: ConformanceCorpus = serde_json::from_str(include_str!(
            "../../../../fixtures/retrieval/lexical-conformance/cases.json"
        ))
        .unwrap();
        assert_eq!(corpus.schema_version, 1);
        assert_eq!(corpus.profile_id, "lexical-v1");

        let temporary = tempdir().unwrap();
        let allowed_vault = temporary.path().join("allowed");
        let data = temporary.path().join("data");
        fs::create_dir(&allowed_vault).unwrap();
        for document in corpus
            .documents
            .iter()
            .filter(|document| document.scope == "allowed")
        {
            fs::write(allowed_vault.join(&document.path), &document.markdown).unwrap();
        }
        for generated in corpus
            .generated_documents
            .iter()
            .filter(|generated| generated.scope == "allowed")
        {
            for index in 0..generated.count {
                let path = format!(
                    "{}{:0width$}.md",
                    generated.path_prefix,
                    index,
                    width = generated.width
                );
                fs::write(allowed_vault.join(path), &generated.markdown).unwrap();
            }
        }
        let config = Config {
            vaults: vec![VaultRegistration {
                id: "allowed".into(),
                path: allowed_vault,
                room: None,
            }],
            ..Config::default()
        };
        build_index(&config, &data).unwrap();
        let (index, fields) = open_index(&data).unwrap();
        let reader = index.reader().unwrap();
        let searcher = reader.searcher();
        let context = NativeSearchContext {
            index: &index,
            fields: &fields,
            searcher: &searcher,
            resource: None,
        };

        for case in &corpus.cases {
            if case.scope != "allowed" {
                continue;
            }
            let resolved = resolve_query_plan(std::slice::from_ref(&context), &case.query)
                .unwrap_or_else(|error| panic!("{} failed to resolve: {error}", case.id));
            assert_eq!(
                enum_name(resolved.plan.assistance),
                case.assistance,
                "{} assistance",
                case.id
            );
            assert_eq!(
                enum_name(resolved.plan.execution),
                case.execution,
                "{} execution",
                case.id
            );
            assert_eq!(
                resolved
                    .plan
                    .evidence_stages
                    .iter()
                    .map(|stage| enum_name(stage.kind))
                    .collect::<Vec<_>>(),
                case.stages,
                "{} stages",
                case.id
            );
            assert_eq!(
                resolved
                    .plan
                    .term_intents
                    .iter()
                    .filter(|intent| {
                        intent.role == crate::query::QueryTermRole::RequiredIdentifierAnchor
                    })
                    .map(|intent| intent.text.clone())
                    .collect::<Vec<_>>(),
                case.anchors,
                "{} anchors",
                case.id
            );
            if let Some(expected) = case.prefix_expansions {
                assert_eq!(
                    resolved
                        .prefix_expansions
                        .values()
                        .map(Vec::len)
                        .sum::<usize>(),
                    expected,
                    "{} prefix expansions",
                    case.id
                );
            }

            let hits = execute_lexical_plan(
                std::slice::from_ref(&context),
                &resolved,
                case.limit,
                &SearchFilters::default(),
                &searcher,
            )
            .unwrap_or_else(|error| panic!("{} failed to execute: {error}", case.id));
            let hit_paths: Vec<_> = hits.iter().map(|hit| hit.path.clone()).collect();
            for excluded in &case.excluded_paths {
                assert!(
                    !hit_paths.contains(excluded),
                    "{} leaked {excluded}",
                    case.id
                );
            }
            for expected in &case.expected_paths {
                assert!(
                    hit_paths.contains(&expected.path),
                    "{} missed {} in {:?}",
                    case.id,
                    expected.path,
                    hit_paths
                );
                let first_tier = if resolved.plan.execution
                    == QueryExecutionDisposition::ExplicitBypass
                {
                    "explicit".to_owned()
                } else {
                    resolved
                        .plan
                        .evidence_stages
                        .iter()
                        .find_map(|stage| {
                            let stage_query = compile_evidence_stage(
                                &index,
                                &fields,
                                &resolved.plan,
                                stage,
                                &resolved.prefix_expansions,
                            )
                            .unwrap()?;
                            let stage_hits = collect_stable_hits(
                                &searcher,
                                stage_query.as_ref(),
                                &fields,
                                None,
                                stage.max_candidates,
                                &searcher,
                            )
                            .unwrap();
                            stage_hits
                                .iter()
                                .any(|hit| hit.path == expected.path)
                                .then(|| enum_name(stage.kind))
                        })
                        .unwrap_or_else(|| panic!("{} had no tier for {}", case.id, expected.path))
                };
                assert_eq!(first_tier, expected.tier, "{} tier", case.id);
            }
            if case.id == "tier-dominance" {
                let expected_order: Vec<_> = case
                    .expected_paths
                    .iter()
                    .map(|expected| expected.path.clone())
                    .collect();
                let actual_order: Vec<_> = hit_paths
                    .into_iter()
                    .filter(|path| expected_order.contains(path))
                    .collect();
                assert_eq!(actual_order, expected_order);
            }
            if case.stable_ties {
                let repeated = execute_lexical_plan(
                    std::slice::from_ref(&context),
                    &resolved,
                    case.limit,
                    &SearchFilters::default(),
                    &searcher,
                )
                .unwrap();
                let identity = |values: &[SearchHit]| {
                    values
                        .iter()
                        .map(|hit| (hit.chunk_id.clone(), hit.path.clone()))
                        .collect::<Vec<_>>()
                };
                assert_eq!(identity(&hits), identity(&repeated));
                let mut independently_sorted = identity(&hits);
                independently_sorted.sort();
                assert_eq!(identity(&hits), independently_sorted);
            }
            if case.combined_scope_changes_evidence {
                assert_eq!(case.id, "authorized-scope-noninterference");
            }
        }

        assert_eq!(corpus.bounds.maximum_terms, crate::query::MAX_QUERY_TERMS);
        assert_eq!(
            corpus.bounds.maximum_query_bytes,
            crate::query::MAX_QUERY_BYTES
        );
        assert_eq!(
            corpus.bounds.maximum_prefix_terms,
            crate::query::MAX_PREFIX_TERMS
        );
        assert_eq!(
            corpus.bounds.maximum_prefix_expansions_per_term,
            crate::query::MAX_PREFIX_EXPANSIONS_PER_TERM
        );
        assert_eq!(
            corpus.bounds.maximum_candidates_per_stage,
            crate::query::MAX_CANDIDATES_PER_STAGE
        );
        assert_eq!(
            corpus.bounds.maximum_total_candidates,
            crate::query::MAX_TOTAL_CANDIDATES
        );
        let maximum_terms = (0..corpus.bounds.maximum_terms)
            .map(|index| format!("boundterm{index}"))
            .collect::<Vec<_>>()
            .join(" ");
        assert_eq!(
            prepare_lexical_query(&maximum_terms)
                .unwrap()
                .support_probes
                .len(),
            corpus.bounds.maximum_terms
        );
        let duplicate_terms = std::iter::repeat_n("boundterm", corpus.bounds.maximum_terms)
            .collect::<Vec<_>>()
            .join(" ");
        assert_eq!(
            prepare_lexical_query(&duplicate_terms)
                .unwrap()
                .support_probes
                .len(),
            1
        );
        let over_limit_terms = std::iter::repeat_n("boundterm", corpus.bounds.over_limit_terms)
            .collect::<Vec<_>>()
            .join(" ");
        assert!(prepare_lexical_query(&over_limit_terms).is_err());
        let over_limit_bytes = corpus
            .bounds
            .over_limit_unicode
            .repeat(corpus.bounds.maximum_query_bytes / 2 + 1);
        assert!(prepare_lexical_query(&over_limit_bytes).is_err());
    }
}
