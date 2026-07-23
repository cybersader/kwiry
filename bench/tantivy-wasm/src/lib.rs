// SPDX-License-Identifier: MIT OR Apache-2.0

use serde_json::{Value as JsonValue, json};
use tantivy::collector::TopDocs;
use tantivy::directory::RamDirectory;
use tantivy::query::QueryParser;
use tantivy::schema::{Field, STORED, STRING, Schema, TEXT, TantivyDocument, Value};
use tantivy::store::Compressor;
use tantivy::{Index, IndexSettings, ReloadPolicy, Term, doc};
use wasm_bindgen::prelude::*;

const MEMORY_BUDGET: usize = 15_000_000;
const TANTIVY_VERSION: &str = "0.26.1";

type ProbeResult<T> = Result<T, String>;

fn at<T, E: std::fmt::Display>(stage: &str, result: Result<T, E>) -> ProbeResult<T> {
    result.map_err(|error| format!("{stage}: {error}"))
}

fn schema() -> (Schema, Field, Field) {
    let mut builder = Schema::builder();
    let id = builder.add_text_field("id", STRING | STORED);
    let body = builder.add_text_field("body", TEXT | STORED);
    (builder.build(), id, body)
}

fn settings() -> IndexSettings {
    IndexSettings {
        docstore_compression: Compressor::None,
        docstore_compress_dedicated_thread: false,
        ..IndexSettings::default()
    }
}

fn documents(id: Field, body: Field) -> [TantivyDocument; 3] {
    [
        doc!(id => "exact", body => "wasm search search search"),
        doc!(id => "partial", body => "wasm search rust code"),
        doc!(id => "other", body => "rust indexing only"),
    ]
}

fn query_ids(index: &Index, id: Field, body: Field, query_text: &str) -> ProbeResult<Vec<String>> {
    let reader = at(
        "reader:create_manual",
        index
            .reader_builder()
            .reload_policy(ReloadPolicy::Manual)
            .try_into(),
    )?;
    at("reader:reload", reader.reload())?;

    let parser = QueryParser::for_index(index, vec![body]);
    let query = at("query:parse", parser.parse_query(query_text))?;
    let searcher = reader.searcher();
    let hits = at(
        "query:top_docs",
        searcher.search(&query, &TopDocs::with_limit(10).order_by_score()),
    )?;

    hits.into_iter()
        .map(|(_score, address)| {
            let document: TantivyDocument = at("query:load_document", searcher.doc(address))?;
            let stored_id = document
                .get_first(id)
                .ok_or_else(|| "query:load_document: missing stored id".to_owned())?;
            stored_id
                .as_str()
                .map(ToOwned::to_owned)
                .ok_or_else(|| "query:load_document: stored id is not text".to_owned())
        })
        .collect()
}

fn evidence(probe: &str, writer: &str, fields: &[(&str, JsonValue)]) -> ProbeResult<String> {
    let mut value = json!({
        "probe": probe,
        "status": "pass",
        "tantivy": TANTIVY_VERSION,
        "writer": writer,
        "manual_reader": true,
        "docstore_compression": "none",
        "dedicated_compression_thread": false,
    });
    let object = value
        .as_object_mut()
        .ok_or_else(|| "evidence: expected JSON object".to_owned())?;
    for (key, field_value) in fields {
        object.insert((*key).to_owned(), field_value.clone());
    }
    at("evidence:serialize", serde_json::to_string(&value))
}

fn run_single_segment() -> ProbeResult<String> {
    let (schema, id, body) = schema();
    let directory = RamDirectory::create();
    let builder = Index::builder().schema(schema).settings(settings());
    let mut writer = at(
        "single_segment:create_writer",
        builder.single_segment_index_writer::<TantivyDocument>(directory.clone(), MEMORY_BUDGET),
    )?;

    for document in documents(id, body) {
        at("single_segment:add_document", writer.add_document(document))?;
    }

    let finalized = at("single_segment:finalize", writer.finalize())?;
    drop(finalized);
    let reopened = at("single_segment:reopen", Index::open(directory.clone()))?;
    let ordered_ids = query_ids(&reopened, id, body, "search")?;
    if ordered_ids != ["exact", "partial"] {
        return Err(format!(
            "single_segment:ranking: expected [\"exact\", \"partial\"], got {ordered_ids:?}"
        ));
    }

    evidence(
        "single_segment",
        "SingleSegmentIndexWriter",
        &[
            ("reopened_ram_directory", json!(true)),
            ("documents", json!(3)),
            ("ordered_ids", json!(ordered_ids)),
            ("ram_bytes", json!(directory.total_mem_usage())),
        ],
    )
}

fn run_index_writer() -> ProbeResult<String> {
    let (schema, id, body) = schema();
    let directory = RamDirectory::create();
    let index = at(
        "index_writer:create_index",
        Index::builder()
            .schema(schema)
            .settings(settings())
            .open_or_create(directory.clone()),
    )?;

    let mut writer = at(
        "index_writer:create_writer",
        index.writer_with_num_threads::<TantivyDocument>(1, MEMORY_BUDGET),
    )?;

    for document in documents(id, body) {
        at("index_writer:add_document", writer.add_document(document))?;
    }
    at("index_writer:commit_adds", writer.commit())?;

    let reopened = at(
        "index_writer:reopen_after_adds",
        Index::open(directory.clone()),
    )?;
    let before_delete = query_ids(&reopened, id, body, "search")?;
    if before_delete != ["exact", "partial"] {
        return Err(format!(
            "index_writer:ranking_before_delete: expected [\"exact\", \"partial\"], got {before_delete:?}"
        ));
    }

    writer.delete_term(Term::from_field_text(id, "exact"));
    at("index_writer:commit_delete", writer.commit())?;

    let reopened = at(
        "index_writer:reopen_after_delete",
        Index::open(directory.clone()),
    )?;
    let after_delete = query_ids(&reopened, id, body, "search")?;
    if after_delete != ["partial"] {
        return Err(format!(
            "index_writer:delete_verification: expected [\"partial\"], got {after_delete:?}"
        ));
    }

    at(
        "index_writer:wait_merging_threads",
        writer.wait_merging_threads(),
    )?;

    evidence(
        "index_writer",
        "IndexWriter",
        &[
            ("initial_documents", json!(3)),
            ("documents_after_delete", json!(2)),
            ("deleted_id_absent", json!(true)),
            ("ordered_ids_after_delete", json!(after_delete)),
        ],
    )
}

fn to_js(result: ProbeResult<String>) -> Result<String, JsValue> {
    result.map_err(|error| JsValue::from_str(&error))
}

#[wasm_bindgen]
pub fn probe_single_segment() -> Result<String, JsValue> {
    to_js(run_single_segment())
}

#[wasm_bindgen]
pub fn probe_index_writer() -> Result<String, JsValue> {
    to_js(run_index_writer())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn single_segment_native_control() {
        let output = run_single_segment().expect("single-segment control should pass");
        assert!(output.contains("\"status\":\"pass\""));
    }

    #[test]
    fn index_writer_native_control() {
        let output = run_index_writer().expect("index-writer control should pass");
        assert!(output.contains("\"deleted_id_absent\":true"));
    }
}
