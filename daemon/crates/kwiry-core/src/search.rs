use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::Path;

use tantivy::collector::{Collector, Count, SegmentCollector, TopDocs};
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
use crate::index::{Fields, open_index};
use crate::model::{LexicalSearchRequest, ResourceKey, SearchHit};
#[cfg(test)]
use crate::query::classify_query;
use crate::query::{
    LEXICAL_QUERY_PLAN_SCHEMA_VERSION, LexicalQueryPlan, QueryAssistanceEligibility,
    QueryEvidenceReport, QueryEvidenceStage, QueryEvidenceStageKind, QueryExecutionDisposition,
    QueryField, QueryFieldGroup, QueryMatchOperator, QueryMetadataField, QueryMetadataProbe,
    QueryPlanKind, QueryTermSupportObservation, prepare_lexical_query,
};

const MAX_RESULTS: usize = 100;
const BOOST_FILENAME: f32 = 5.0;
const BOOST_STEM: f32 = 6.0;
const BOOST_ALIAS: f32 = 6.0;
const BOOST_TITLE: f32 = 6.0;
const BOOST_HEADING: f32 = 3.0;
const BOOST_CONTENT: f32 = 1.0;
const BOOST_EXACT_METADATA: f32 = 12.0;
const BOOST_PHRASE: f32 = 4.0;
const BOOST_CONTENT_IDENTIFIER: f32 = 5.0;

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
    execute_lexical_plan(
        std::slice::from_ref(&context),
        &resolved,
        limit.min(MAX_RESULTS),
        filters,
        &searcher,
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

struct AuthorizedStatistics {
    searchers: Vec<Searcher>,
}

impl Bm25StatisticsProvider for AuthorizedStatistics {
    fn total_num_tokens(&self, field: Field) -> tantivy::Result<u64> {
        self.searchers.iter().try_fold(0_u64, |total, searcher| {
            Ok(total + searcher.total_num_tokens(field)?)
        })
    }

    fn total_num_docs(&self) -> tantivy::Result<u64> {
        Ok(self.searchers.iter().map(Searcher::num_docs).sum())
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
    let statistics = AuthorizedStatistics {
        searchers: searchers.clone(),
    };
    let resolved = resolve_query_plan(&contexts, query_text)?;
    execute_lexical_plan(
        &contexts,
        &resolved,
        limit.min(MAX_RESULTS),
        filters,
        &statistics,
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
        let expansions = if document_frequency == 0
            && probe.term.chars().count() >= plan.bounds.min_prefix_chars
            && prefix_terms_examined < plan.bounds.max_prefix_terms
        {
            prefix_terms_examined += 1;
            collect_prefix_expansions(
                contexts,
                &plan,
                &probe.term,
                plan.bounds.max_prefix_expansions_per_term,
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
    for context in contexts {
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
        let count = context
            .searcher
            .search(query.as_ref(), &Count)
            .map_err(|error| Error::Index(error.to_string()))?;
        total = total.saturating_add(count as u64);
    }
    Ok(total)
}

fn collect_prefix_expansions(
    contexts: &[NativeSearchContext<'_>],
    plan: &LexicalQueryPlan,
    term: &str,
    limit: usize,
) -> Result<Vec<String>> {
    if limit == 0 || contexts.is_empty() {
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
    let mut expansions = BTreeSet::new();
    let mut examined_terms = 0_usize;

    for context in contexts {
        let bindings = field_bindings(
            context.fields,
            declared_fields(plan, QueryFieldGroup::Prefix),
            QueryFieldGroup::Prefix,
        );
        for (field, _) in bindings {
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
                while stream.advance() {
                    let key = stream.key();
                    if !key.starts_with(prefix) {
                        break;
                    }
                    examined_terms += 1;
                    if let Ok(expansion) = std::str::from_utf8(key) {
                        expansions.insert(expansion.to_owned());
                    }
                    if expansions.len() == limit || examined_terms == limit {
                        return Ok(expansions.into_iter().collect());
                    }
                }
            }
        }
    }
    Ok(expansions.into_iter().collect())
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
        let parser = lexical_parser(context.index, context.fields);
        let parsed = parser
            .parse_query(&plan.query)
            .map_err(|error| Error::Query(error.to_string()))?;
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
            let stage_query = compile_evidence_stage(
                context.index,
                context.fields,
                plan,
                stage,
                &resolved.prefix_expansions,
            )?;
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
) -> Result<Box<dyn Query>> {
    match stage.kind {
        QueryEvidenceStageKind::ExactMetadata => exact_query(fields, plan)
            .ok_or_else(|| Error::Query("exact metadata stage has no intent".to_owned())),
        QueryEvidenceStageKind::ExactPhrase => phrase_query(index, fields, plan),
        QueryEvidenceStageKind::AllTerms | QueryEvidenceStageKind::PartialCoverage => {
            required_terms_query(index, fields, plan, stage)
        }
        QueryEvidenceStageKind::Prefix => {
            prefix_stage_query(index, fields, plan, stage, prefix_expansions)
        }
    }
}

fn required_terms_query(
    index: &Index,
    fields: &Fields,
    plan: &LexicalQueryPlan,
    stage: &QueryEvidenceStage,
) -> Result<Box<dyn Query>> {
    let mut clauses = Vec::with_capacity(stage.required_term_indexes.len());
    for term_index in &stage.required_term_indexes {
        let intent = plan
            .term_intents
            .get(*term_index as usize)
            .ok_or_else(|| Error::Query("evidence stage references an unknown term".to_owned()))?;
        let query = term_query_for_group(index, fields, plan, stage.field_group, &intent.text)?
            .ok_or_else(|| Error::Query("evidence term produced no backend tokens".to_owned()))?;
        clauses.push((Occur::Must, query));
    }
    Ok(Box::new(BooleanQuery::new(clauses)))
}

fn prefix_stage_query(
    index: &Index,
    fields: &Fields,
    plan: &LexicalQueryPlan,
    stage: &QueryEvidenceStage,
    prefix_expansions: &BTreeMap<u16, Vec<String>>,
) -> Result<Box<dyn Query>> {
    let mut clauses = Vec::new();
    for term_index in &stage.required_term_indexes {
        let intent = plan
            .term_intents
            .get(*term_index as usize)
            .ok_or_else(|| Error::Query("prefix stage references an unknown term".to_owned()))?;
        let query = term_query_for_group(
            index,
            fields,
            plan,
            QueryFieldGroup::SearchableText,
            &intent.text,
        )?
        .ok_or_else(|| Error::Query("required prefix context produced no tokens".to_owned()))?;
        clauses.push((Occur::Must, query));
    }
    for term_index in &stage.prefix_term_indexes {
        let expansions = prefix_expansions.get(term_index).ok_or_else(|| {
            Error::Query("prefix stage is missing its bounded expansions".to_owned())
        })?;
        let bindings = field_bindings(
            fields,
            declared_fields(plan, QueryFieldGroup::Prefix),
            QueryFieldGroup::Prefix,
        );
        let mut alternatives = Vec::new();
        for expansion in expansions {
            alternatives.extend(term_alternatives(&bindings, expansion));
        }
        if alternatives.is_empty() {
            return Err(Error::Query(
                "prefix stage has no executable expansion".to_owned(),
            ));
        }
        clauses.push((
            Occur::Must,
            Box::new(DisjunctionMaxQuery::new(alternatives)) as Box<dyn Query>,
        ));
    }
    Ok(Box::new(BooleanQuery::new(clauses)))
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

fn phrase_query(index: &Index, fields: &Fields, plan: &LexicalQueryPlan) -> Result<Box<dyn Query>> {
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
        return Err(Error::Query(
            "phrase intent produced no backend tokens".to_owned(),
        ));
    }
    Ok(Box::new(DisjunctionMaxQuery::new(alternatives)))
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
            fields.content,
        ],
    );
    parser.set_field_boost(fields.filename, BOOST_FILENAME);
    parser.set_field_boost(fields.stem, BOOST_STEM);
    parser.set_field_boost(fields.aliases, BOOST_ALIAS);
    parser.set_field_boost(fields.title, BOOST_TITLE);
    parser.set_field_boost(fields.heading_text, BOOST_HEADING);
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
}

struct StableSegmentCollector {
    limit: usize,
    chunk_id: Field,
    path: Field,
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
        if self.documents.len() == self.limit
            && score.total_cmp(
                &self
                    .documents
                    .last()
                    .expect("bounded segment list is nonempty")
                    .score,
            ) == Ordering::Less
        {
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
            ("alph*", 0),
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
                let first_tier = resolved
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
                        .unwrap();
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
                    .unwrap_or_else(|| panic!("{} had no tier for {}", case.id, expected.path));
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
        let maximum_terms = std::iter::repeat_n("boundterm", corpus.bounds.maximum_terms)
            .collect::<Vec<_>>()
            .join(" ");
        assert_eq!(
            prepare_lexical_query(&maximum_terms)
                .unwrap()
                .support_probes
                .len(),
            corpus.bounds.maximum_terms
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
