use std::cmp::Ordering;
use std::collections::{BTreeMap, HashMap};
use std::path::Path;

use tantivy::Index;
use tantivy::Term;
use tantivy::collector::TopDocs;
use tantivy::query::{
    BooleanQuery, ConstScoreQuery, Occur, Query, QueryParser, RegexQuery, TermQuery,
};
use tantivy::schema::{Field, IndexRecordOption, TantivyDocument, Value};
use tantivy::snippet::SnippetGenerator;
use tantivy::{IndexReader, Searcher};

use crate::api::SearchFilters;
use crate::error::{Error, Result};
use crate::index::{Fields, open_index};
use crate::model::{SearchHit, SearchRequest};

const MAX_RESULTS: usize = 100;

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
    let parser = QueryParser::for_index(
        index,
        vec![fields.title, fields.heading_text, fields.content],
    );
    let parsed = parser
        .parse_query(query_text)
        .map_err(|error| Error::Query(error.to_string()))?;
    let query = filtered_query(parsed, filters, fields)?;
    collect_hits(&searcher, query, fields, limit)
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
        let parser = QueryParser::for_index(
            index,
            vec![fields.title, fields.heading_text, fields.content],
        );
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
