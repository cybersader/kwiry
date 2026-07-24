use std::cmp::Ordering;
use std::collections::{BTreeMap, HashMap};
use std::path::Path;

use tantivy::Index;
use tantivy::Term;
use tantivy::collector::TopDocs;
use tantivy::query::{
    Bm25StatisticsProvider, BooleanQuery, BoostQuery, ConstScoreQuery, DisjunctionMaxQuery, Occur,
    Query, QueryParser, RegexQuery, TermQuery,
};
use tantivy::schema::{Field, IndexRecordOption, TantivyDocument, Value};
use tantivy::snippet::SnippetGenerator;
use tantivy::{IndexReader, Searcher};

use crate::api::SearchFilters;
use crate::error::{Error, Result};
use crate::index::{Fields, open_index};
use crate::model::{ResourceKey, SearchHit, SearchRequest};
#[cfg(test)]
use crate::query::classify_query;
use crate::query::{
    LexicalQueryPlan, QueryMetadataField, QueryMetadataProbe, QueryPlanKind, prepare_lexical_query,
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

pub fn search_index(data_dir: &Path, request: &SearchRequest) -> Result<Vec<SearchHit>> {
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
    let (_, ranked) = build_lexical_query(index, fields, &searcher, query_text)?;
    let query = filtered_query(ranked, filters, fields)?;
    collect_hits(&searcher, query, fields, limit)
}

pub(crate) struct PartitionReader<'a> {
    pub index: &'a Index,
    pub fields: &'a Fields,
    pub reader: &'a IndexReader,
    pub resource: &'a ResourceKey,
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

    let searchers: Vec<_> = partitions
        .iter()
        .map(|partition| partition.reader.searcher())
        .collect();
    let statistics = AuthorizedStatistics {
        searchers: searchers.clone(),
    };
    let plan = resolve_partitioned_query_plan(partitions, &searchers, query_text)?;
    let mut hits = Vec::new();
    for (partition, searcher) in partitions.iter().zip(&searchers) {
        let ranked = build_lexical_query_from_plan(partition.index, partition.fields, &plan)?;
        let query = filtered_query(ranked, filters, partition.fields)?;
        hits.extend(collect_partition_hits(
            searcher,
            query,
            partition.fields,
            partition.resource,
            limit,
            &statistics,
        )?);
    }
    hits.sort_by(|left, right| {
        right
            .score
            .partial_cmp(&left.score)
            .unwrap_or(Ordering::Equal)
            .then_with(|| left.chunk_id.cmp(&right.chunk_id))
            .then_with(|| left.path.cmp(&right.path))
    });
    hits.truncate(limit);
    Ok(hits)
}

fn resolve_partitioned_query_plan(
    partitions: &[PartitionReader<'_>],
    searchers: &[Searcher],
    query: &str,
) -> Result<LexicalQueryPlan> {
    let plan = prepare_lexical_query(query).map_err(|error| Error::Query(error.to_string()))?;
    let Some(probe) = plan.metadata_probe.as_ref() else {
        return Ok(plan);
    };

    for (partition, searcher) in partitions.iter().zip(searchers) {
        let mut parser = QueryParser::for_index(
            partition.index,
            metadata_probe_fields(partition.fields, probe),
        );
        if probe.conjunction {
            parser.set_conjunction_by_default();
        }
        let metadata_query = parser
            .parse_query(&probe.query)
            .map_err(|error| Error::Query(error.to_string()))?;
        let matches = searcher
            .search(&metadata_query, &TopDocs::with_limit(1).order_by_score())
            .map_err(|error| Error::Index(error.to_string()))?;
        if !matches.is_empty() {
            return Ok(plan.finalize_metadata_probe(true));
        }
    }
    Ok(plan.finalize_metadata_probe(false))
}

fn build_lexical_query(
    index: &Index,
    fields: &Fields,
    searcher: &Searcher,
    query_text: &str,
) -> Result<(QueryPlanKind, Box<dyn Query>)> {
    let plan = resolve_query_plan(index, fields, searcher, query_text)?;
    let kind = plan.kind;
    Ok((kind, build_lexical_query_from_plan(index, fields, &plan)?))
}

fn build_lexical_query_from_plan(
    index: &Index,
    fields: &Fields,
    plan: &LexicalQueryPlan,
) -> Result<Box<dyn Query>> {
    let parser = lexical_parser(index, fields);
    let query = match plan.kind {
        QueryPlanKind::Explicit => parser
            .parse_query(&plan.query)
            .map_err(|error| Error::Query(error.to_string()))?,
        QueryPlanKind::Ordinary => {
            let parsed = parser
                .parse_query(&plan.query)
                .map_err(|error| Error::Query(error.to_string()))?;
            let mut clauses = vec![(Occur::Should, parsed)];
            add_ordering_boosters(&mut clauses, &parser, fields, plan)?;
            Box::new(BooleanQuery::new(clauses))
        }
        QueryPlanKind::Identifier => {
            let mut clauses = Vec::with_capacity(plan.identifier_terms.len() + 2);
            if plan.identifier_terms.len() == 1 {
                let parsed = parser
                    .parse_query(&plan.query)
                    .map_err(|error| Error::Query(error.to_string()))?;
                let mut alternatives = vec![parsed];
                if let Some(exact) = exact_query(fields, plan.normalized_exact.as_deref()) {
                    alternatives.push(exact);
                }
                clauses.push((
                    Occur::Must,
                    Box::new(DisjunctionMaxQuery::new(alternatives)) as Box<dyn Query>,
                ));
            } else {
                for term in &plan.identifier_terms {
                    let parsed = parser
                        .parse_query(term)
                        .map_err(|error| Error::Query(error.to_string()))?;
                    clauses.push((Occur::Must, parsed));
                }
            }
            add_ordering_boosters(&mut clauses, &parser, fields, plan)?;
            Box::new(BooleanQuery::new(clauses))
        }
    };
    Ok(query)
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

fn resolve_query_plan(
    index: &Index,
    fields: &Fields,
    searcher: &Searcher,
    query: &str,
) -> Result<LexicalQueryPlan> {
    let plan = prepare_lexical_query(query).map_err(|error| Error::Query(error.to_string()))?;
    let Some(probe) = plan.metadata_probe.as_ref() else {
        return Ok(plan);
    };

    let mut parser = QueryParser::for_index(index, metadata_probe_fields(fields, probe));
    if probe.conjunction {
        parser.set_conjunction_by_default();
    }
    let metadata_query = parser
        .parse_query(&probe.query)
        .map_err(|error| Error::Query(error.to_string()))?;
    let matches = searcher
        .search(&metadata_query, &TopDocs::with_limit(1).order_by_score())
        .map_err(|error| Error::Index(error.to_string()))?;
    Ok(plan.finalize_metadata_probe(!matches.is_empty()))
}

fn add_ordering_boosters(
    clauses: &mut Vec<(Occur, Box<dyn Query>)>,
    parser: &QueryParser,
    fields: &Fields,
    plan: &LexicalQueryPlan,
) -> Result<()> {
    if let Some(exact) = exact_query(fields, plan.normalized_exact.as_deref()) {
        clauses.push((Occur::Should, exact));
    }
    if plan.phrase_boost {
        let escaped = plan.query.replace('\\', "\\\\").replace('"', "\\\"");
        let phrase = parser
            .parse_query(&format!("\"{escaped}\""))
            .map_err(|error| Error::Query(error.to_string()))?;
        clauses.push((
            Occur::Should,
            Box::new(BoostQuery::new(phrase, BOOST_PHRASE)),
        ));
    }
    Ok(())
}

fn exact_query(fields: &Fields, normalized: Option<&str>) -> Option<Box<dyn Query>> {
    let normalized = normalized?;
    let high_fields = [
        fields.filename_raw,
        fields.stem_raw,
        fields.aliases_raw,
        fields.title_raw,
    ];
    let mut queries: Vec<Box<dyn Query>> = high_fields
        .into_iter()
        .map(|field| {
            Box::new(BoostQuery::new(
                Box::new(TermQuery::new(
                    Term::from_field_text(field, normalized),
                    IndexRecordOption::Basic,
                )),
                BOOST_EXACT_METADATA,
            )) as Box<dyn Query>
        })
        .collect();
    queries.push(Box::new(BoostQuery::new(
        Box::new(TermQuery::new(
            Term::from_field_text(fields.heading_raw, normalized),
            IndexRecordOption::Basic,
        )),
        BOOST_HEADING,
    )));
    queries.push(Box::new(BoostQuery::new(
        Box::new(TermQuery::new(
            Term::from_field_text(fields.content_identifiers, normalized),
            IndexRecordOption::Basic,
        )),
        BOOST_CONTENT_IDENTIFIER,
    )));
    Some(Box::new(DisjunctionMaxQuery::new(queries)))
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

fn collect_partition_hits(
    searcher: &Searcher,
    query: Box<dyn Query>,
    fields: &Fields,
    resource: &ResourceKey,
    limit: usize,
    statistics: &dyn Bm25StatisticsProvider,
) -> Result<Vec<SearchHit>> {
    let top_docs = searcher
        .search_with_statistics_provider(
            query.as_ref(),
            &TopDocs::with_limit(limit).order_by_score(),
            statistics,
        )
        .map_err(|error| Error::Query(error.to_string()))?;
    let snippet_generator = SnippetGenerator::create(searcher, query.as_ref(), fields.content)
        .map_err(|error| Error::Query(error.to_string()))?;

    let mut hits = Vec::with_capacity(top_docs.len());
    for (score, address) in top_docs {
        let document = searcher
            .doc::<TantivyDocument>(address)
            .map_err(|error| Error::Index(error.to_string()))?;
        let vault_id = text(&document, fields.vault_id)?;
        let room_id = text(&document, fields.room)?;
        if vault_id != resource.vault_id || room_id != resource.room_id {
            return Err(Error::Index(format!(
                "document resource mismatch in partition {}/{}/{}",
                resource.tenant_id, resource.vault_id, resource.room_id
            )));
        }
        hits.push(hit_from_document(
            &document,
            fields,
            score,
            Some(&snippet_generator),
        )?);
    }
    hits.sort_by(|left, right| {
        right
            .score
            .partial_cmp(&left.score)
            .unwrap_or(Ordering::Equal)
            .then_with(|| left.chunk_id.cmp(&right.chunk_id))
            .then_with(|| left.path.cmp(&right.path))
    });
    Ok(hits)
}

fn collect_hits(
    searcher: &Searcher,
    query: Box<dyn Query>,
    fields: &Fields,
    limit: usize,
) -> Result<Vec<SearchHit>> {
    let top_docs = searcher
        .search(&query, &TopDocs::with_limit(limit).order_by_score())
        .map_err(|error| Error::Query(error.to_string()))?;
    let snippet_generator = SnippetGenerator::create(searcher, query.as_ref(), fields.content)
        .map_err(|error| Error::Query(error.to_string()))?;

    let mut hits = Vec::with_capacity(top_docs.len());
    for (score, address) in top_docs {
        let document = searcher
            .doc::<TantivyDocument>(address)
            .map_err(|error| Error::Index(error.to_string()))?;
        hits.push(hit_from_document(
            &document,
            fields,
            score,
            Some(&snippet_generator),
        )?);
    }

    hits.sort_by(|left, right| {
        right
            .score
            .partial_cmp(&left.score)
            .unwrap_or(Ordering::Equal)
            .then_with(|| left.chunk_id.cmp(&right.chunk_id))
            .then_with(|| left.path.cmp(&right.path))
    });
    Ok(hits)
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
    fn native_query_construction_consumes_prepared_identifier_terms() {
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
        let mut plan = prepare_lexical_query("not-present").unwrap();
        plan.kind = QueryPlanKind::Identifier;
        plan.identifier_terms = vec!["alpha".into(), "beta".into()];
        plan.normalized_exact = None;
        plan.phrase_boost = false;

        let query = build_lexical_query_from_plan(&index, &fields, &plan).unwrap();
        let hits = searcher
            .search(&query, &TopDocs::with_limit(10).order_by_score())
            .unwrap();
        assert_eq!(hits.len(), 1);
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
        let (lowercase_kind, _) =
            build_lexical_query(&index, &fields, &searcher, "iia 2 line").unwrap();
        assert_eq!(lowercase_kind, QueryPlanKind::Identifier);

        let hits = search_index(
            temporary.path(),
            &SearchRequest {
                query: "IIA 2 line".into(),
                limit: 100,
                vault_id: None,
            },
        )
        .unwrap();
        let paths: HashSet<_> = hits.iter().map(|hit| hit.path.as_str()).collect();

        assert_eq!(hits[0].path, "IIA-2-line.md");
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
            &SearchRequest {
                query: "iia 2 line".into(),
                limit: 100,
                vault_id: None,
            },
        )
        .unwrap();
        let lowercase_paths: HashSet<_> =
            lowercase_hits.iter().map(|hit| hit.path.as_str()).collect();
        assert_eq!(lowercase_hits[0].path, "IIA-2-line.md");
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

        let alias_rank = hits
            .iter()
            .position(|hit| hit.path == "alias-guidance.md")
            .unwrap();
        let body_rank = hits
            .iter()
            .position(|hit| hit.path == "body-guidance.md")
            .unwrap();
        assert!(alias_rank < body_rank);
    }

    #[test]
    fn ordinary_plan_preserves_or_eligibility() {
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
        let (list_kind, _) =
            build_lexical_query(&index, &fields, &searcher, "top 10 books").unwrap();
        assert_eq!(list_kind, QueryPlanKind::Ordinary);

        let hits = search_index(
            &data,
            &SearchRequest {
                query: "dungeons and dragons".into(),
                limit: 20,
                vault_id: None,
            },
        )
        .unwrap();
        let paths: HashSet<_> = hits.iter().map(|hit| hit.path.as_str()).collect();

        assert!(paths.contains("dungeons.md"));
        assert!(paths.contains("dragons.md"));

        let explicit_or = search_index(
            &data,
            &SearchRequest {
                query: "dungeons OR dragons".into(),
                limit: 20,
                vault_id: None,
            },
        )
        .unwrap();
        assert_eq!(explicit_or.len(), 2);
        let explicit_and = search_index(
            &data,
            &SearchRequest {
                query: "dungeons AND dragons".into(),
                limit: 20,
                vault_id: None,
            },
        )
        .unwrap();
        assert!(explicit_and.is_empty());
    }
}
