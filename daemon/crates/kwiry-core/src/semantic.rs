use std::path::Path;
use std::sync::Mutex;

use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use zerocopy::IntoBytes;

use crate::error::{Error, Result};

/// Canonical description of everything that changes embedding outputs.
/// Any fingerprint change invalidates all stored vectors.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct EmbeddingProfile {
    pub schema_version: u32,
    pub model_id: String,
    pub artifact: String,
    pub precision: String,
    pub dimensions: usize,
    pub max_tokens: usize,
    pub query_prefix: String,
    pub passage_prefix: String,
    pub pooling: String,
    pub normalized: bool,
    pub runtime: String,
}

impl EmbeddingProfile {
    pub fn bge_small_en_v15() -> Self {
        Self {
            schema_version: 1,
            model_id: "BAAI/bge-small-en-v1.5".to_owned(),
            artifact: "Xenova/bge-small-en-v1.5 onnx/model.onnx".to_owned(),
            precision: "fp32".to_owned(),
            dimensions: 384,
            max_tokens: 512,
            query_prefix: "Represent this sentence for searching relevant passages: ".to_owned(),
            passage_prefix: String::new(),
            pooling: "cls".to_owned(),
            normalized: true,
            runtime: format!("fastembed-{}", "5"),
        }
    }

    pub fn fingerprint(&self) -> String {
        let canonical = serde_json::to_string(self).expect("profile serializes");
        let digest = Sha256::digest(canonical.as_bytes());
        let mut out = String::with_capacity(64);
        for byte in digest {
            out.push_str(&format!("{byte:02x}"));
        }
        out
    }
}

/// Text an embedding is computed over: heading breadcrumb plus content, so
/// section context survives vocabulary mismatch.
pub fn embedding_text(heading_path: &[String], content: &str) -> String {
    let mut text = heading_path.join(" > ");
    if !text.is_empty() {
        text.push('\n');
    }
    text.push_str(content);
    text
}

pub trait Embedder: Send {
    fn profile(&self) -> &EmbeddingProfile;
    fn embed_passages(&mut self, texts: &[String]) -> Result<Vec<Vec<f32>>>;
    fn embed_query(&mut self, text: &str) -> Result<Vec<f32>>;
}

/// Local ONNX embedder via fastembed (bge-small-en-v1.5).
#[cfg(feature = "semantic-onnx")]
pub struct FastembedEmbedder {
    profile: EmbeddingProfile,
    model: fastembed::TextEmbedding,
}

#[cfg(feature = "semantic-onnx")]
impl FastembedEmbedder {
    /// Batch kept small: peak RSS scales with batch size (~784 MiB at 8
    /// versus ~4 GiB at 64 in the Vertical 3 benchmark).
    const BATCH: usize = 8;

    pub fn new(cache_dir: &Path) -> Result<Self> {
        use fastembed::{EmbeddingModel, InitOptions, TextEmbedding};
        let options = InitOptions::new(EmbeddingModel::BGESmallENV15)
            .with_show_download_progress(false)
            .with_cache_dir(cache_dir.to_path_buf());
        let model = TextEmbedding::try_new(options)
            .map_err(|error| Error::Semantic(format!("embedding model init failed: {error}")))?;
        Ok(Self {
            profile: EmbeddingProfile::bge_small_en_v15(),
            model,
        })
    }
}

#[cfg(feature = "semantic-onnx")]
impl Embedder for FastembedEmbedder {
    fn profile(&self) -> &EmbeddingProfile {
        &self.profile
    }

    fn embed_passages(&mut self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        let refs: Vec<&str> = texts.iter().map(String::as_str).collect();
        self.model
            .embed(refs, Some(Self::BATCH))
            .map_err(|error| Error::Semantic(format!("embedding failed: {error}")))
    }

    fn embed_query(&mut self, text: &str) -> Result<Vec<f32>> {
        let prompted = format!("{}{}", self.profile.query_prefix, text);
        let mut vectors = self
            .model
            .embed(vec![prompted.as_str()], Some(1))
            .map_err(|error| Error::Semantic(format!("query embedding failed: {error}")))?;
        vectors
            .pop()
            .ok_or_else(|| Error::Semantic("query embedding returned no vector".to_owned()))
    }
}

/// Registers sqlite-vec for all connections opened by this process.
fn register_sqlite_vec() {
    use std::sync::Once;
    static REGISTER: Once = Once::new();
    REGISTER.call_once(|| unsafe {
        type AutoExtFn = unsafe extern "C" fn(
            *mut rusqlite::ffi::sqlite3,
            *mut *mut std::ffi::c_char,
            *const rusqlite::ffi::sqlite3_api_routines,
        ) -> std::ffi::c_int;
        rusqlite::ffi::sqlite3_auto_extension(Some(std::mem::transmute::<*const (), AutoExtFn>(
            sqlite_vec::sqlite3_vec_init as *const (),
        )));
    });
}

/// Persistent vector store: one SQLite file per generation holding a
/// chunk-mapping table and a vec0 virtual table, mutated transactionally.
pub struct SemanticStore {
    conn: Connection,
    dimensions: usize,
}

impl SemanticStore {
    pub fn open(path: &Path, profile: &EmbeddingProfile) -> Result<Self> {
        register_sqlite_vec();
        let conn = Connection::open(path).map_err(|error| {
            Error::Semantic(format!("could not open {}: {error}", path.display()))
        })?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS semantic_meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
             CREATE TABLE IF NOT EXISTS chunk_map (
                 vector_rowid INTEGER PRIMARY KEY,
                 chunk_id TEXT NOT NULL UNIQUE,
                 source_key TEXT NOT NULL
             );
             CREATE INDEX IF NOT EXISTS chunk_map_source ON chunk_map (source_key);
             CREATE TABLE IF NOT EXISTS source_state (
                 source_key TEXT PRIMARY KEY,
                 content_hash TEXT NOT NULL
             );",
        )
        .map_err(semantic_error)?;

        let fingerprint = profile.fingerprint();
        let stored: Option<String> = conn
            .query_row(
                "SELECT value FROM semantic_meta WHERE key = 'embedding_fingerprint'",
                [],
                |row| row.get(0),
            )
            .map(Some)
            .or_else(|error| match error {
                rusqlite::Error::QueryReturnedNoRows => Ok(None),
                other => Err(other),
            })
            .map_err(semantic_error)?;

        let store = Self {
            conn,
            dimensions: profile.dimensions,
        };
        match stored {
            Some(existing) if existing == fingerprint => {}
            Some(_) => {
                // Fingerprint change invalidates every vector: reset in place.
                store
                    .conn
                    .execute_batch(
                        "DROP TABLE IF EXISTS chunk_vec;
                         DELETE FROM chunk_map;
                         DELETE FROM source_state;",
                    )
                    .map_err(semantic_error)?;
                store.initialize(&fingerprint)?;
            }
            None => store.initialize(&fingerprint)?,
        }
        Ok(store)
    }

    fn initialize(&self, fingerprint: &str) -> Result<()> {
        self.conn
            .execute_batch(&format!(
                "CREATE VIRTUAL TABLE IF NOT EXISTS chunk_vec USING vec0(embedding float[{}]);",
                self.dimensions
            ))
            .map_err(semantic_error)?;
        self.conn
            .execute(
                "INSERT OR REPLACE INTO semantic_meta (key, value) VALUES ('embedding_fingerprint', ?1)",
                [fingerprint],
            )
            .map_err(semantic_error)?;
        Ok(())
    }

    pub fn has_source(&self, source_key: &str) -> Result<bool> {
        let count: i64 = self
            .conn
            .query_row(
                "SELECT COUNT(*) FROM chunk_map WHERE source_key = ?1",
                [source_key],
                |row| row.get(0),
            )
            .map_err(semantic_error)?;
        Ok(count > 0)
    }

    /// The content hash the source was last embedded at, if any.
    pub fn source_hash(&self, source_key: &str) -> Result<Option<String>> {
        self.conn
            .query_row(
                "SELECT content_hash FROM source_state WHERE source_key = ?1",
                [source_key],
                |row| row.get(0),
            )
            .map(Some)
            .or_else(|error| match error {
                rusqlite::Error::QueryReturnedNoRows => Ok(None),
                other => Err(other),
            })
            .map_err(semantic_error)
    }

    /// Atomically replaces every vector for a source with the new chunk set.
    pub fn replace_source(
        &mut self,
        source_key: &str,
        content_hash: &str,
        chunks: &[(String, Vec<f32>)],
    ) -> Result<()> {
        let tx = self.conn.transaction().map_err(semantic_error)?;
        delete_source_tx(&tx, source_key)?;
        tx.execute(
            "INSERT OR REPLACE INTO source_state (source_key, content_hash) VALUES (?1, ?2)",
            (source_key, content_hash),
        )
        .map_err(semantic_error)?;
        for (chunk_id, embedding) in chunks {
            if embedding.len() != self.dimensions {
                return Err(Error::Semantic(format!(
                    "embedding for {chunk_id} has {} dimensions, expected {}",
                    embedding.len(),
                    self.dimensions
                )));
            }
            tx.execute(
                "INSERT INTO chunk_map (chunk_id, source_key) VALUES (?1, ?2)",
                (chunk_id, source_key),
            )
            .map_err(semantic_error)?;
            let rowid = tx.last_insert_rowid();
            tx.execute(
                "INSERT INTO chunk_vec (rowid, embedding) VALUES (?1, ?2)",
                (rowid, embedding.as_bytes()),
            )
            .map_err(semantic_error)?;
        }
        tx.commit().map_err(semantic_error)
    }

    pub fn delete_source(&mut self, source_key: &str) -> Result<()> {
        let tx = self.conn.transaction().map_err(semantic_error)?;
        delete_source_tx(&tx, source_key)?;
        tx.commit().map_err(semantic_error)
    }

    pub fn chunk_count(&self) -> Result<usize> {
        let count: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM chunk_map", [], |row| row.get(0))
            .map_err(semantic_error)?;
        Ok(count as usize)
    }

    pub fn search(&self, query: &[f32], k: usize) -> Result<Vec<SemanticHit>> {
        if query.len() != self.dimensions {
            return Err(Error::Semantic(format!(
                "query vector has {} dimensions, expected {}",
                query.len(),
                self.dimensions
            )));
        }
        let mut stmt = self
            .conn
            .prepare(
                "SELECT m.chunk_id, v.distance
                 FROM chunk_vec v JOIN chunk_map m ON m.vector_rowid = v.rowid
                 WHERE v.embedding MATCH ?1 AND v.k = ?2
                 ORDER BY v.distance, m.chunk_id",
            )
            .map_err(semantic_error)?;
        let hits = stmt
            .query_map((query.as_bytes(), k as i64), |row| {
                Ok(SemanticHit {
                    chunk_id: row.get(0)?,
                    distance: row.get(1)?,
                })
            })
            .map_err(semantic_error)?
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(semantic_error)?;
        Ok(hits)
    }
}

fn delete_source_tx(tx: &rusqlite::Transaction<'_>, source_key: &str) -> Result<()> {
    tx.execute(
        "DELETE FROM chunk_vec WHERE rowid IN
             (SELECT vector_rowid FROM chunk_map WHERE source_key = ?1)",
        [source_key],
    )
    .map_err(semantic_error)?;
    tx.execute("DELETE FROM chunk_map WHERE source_key = ?1", [source_key])
        .map_err(semantic_error)?;
    tx.execute(
        "DELETE FROM source_state WHERE source_key = ?1",
        [source_key],
    )
    .map_err(semantic_error)?;
    Ok(())
}

fn semantic_error(error: rusqlite::Error) -> Error {
    Error::Semantic(error.to_string())
}

#[derive(Debug, Clone, PartialEq)]
pub struct SemanticHit {
    pub chunk_id: String,
    pub distance: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct RrfTrace {
    pub chunk_id: String,
    pub lexical_rank: Option<usize>,
    pub semantic_rank: Option<usize>,
    pub lexical_contribution: f64,
    pub semantic_contribution: f64,
    pub fused_score: f64,
}

/// Reciprocal-rank fusion over two ranked chunk-ID lists (the contract's one
/// permitted formula). Deterministic: ties break on chunk_id.
pub fn rrf_fuse(lexical: &[String], semantic: &[String], k: f64) -> Vec<(String, f64)> {
    rrf_fuse_traced(lexical, semantic, k)
        .into_iter()
        .map(|trace| (trace.chunk_id, trace.fused_score))
        .collect()
}

pub(crate) fn rrf_fuse_traced(lexical: &[String], semantic: &[String], k: f64) -> Vec<RrfTrace> {
    use std::collections::BTreeMap;

    let mut traces: BTreeMap<&str, RrfTrace> = BTreeMap::new();
    for (rank, chunk_id) in lexical.iter().enumerate() {
        let contribution = 1.0 / (k + rank as f64 + 1.0);
        let trace = traces.entry(chunk_id).or_insert_with(|| RrfTrace {
            chunk_id: chunk_id.clone(),
            lexical_rank: None,
            semantic_rank: None,
            lexical_contribution: 0.0,
            semantic_contribution: 0.0,
            fused_score: 0.0,
        });
        trace.lexical_rank = Some(rank + 1);
        trace.lexical_contribution = contribution;
        trace.fused_score += contribution;
    }
    for (rank, chunk_id) in semantic.iter().enumerate() {
        let contribution = 1.0 / (k + rank as f64 + 1.0);
        let trace = traces.entry(chunk_id).or_insert_with(|| RrfTrace {
            chunk_id: chunk_id.clone(),
            lexical_rank: None,
            semantic_rank: None,
            lexical_contribution: 0.0,
            semantic_contribution: 0.0,
            fused_score: 0.0,
        });
        trace.semantic_rank = Some(rank + 1);
        trace.semantic_contribution = contribution;
        trace.fused_score += contribution;
    }
    let mut fused: Vec<_> = traces.into_values().collect();
    fused.sort_by(|left, right| {
        right
            .fused_score
            .total_cmp(&left.fused_score)
            .then_with(|| left.chunk_id.cmp(&right.chunk_id))
    });
    fused
}

/// Shared semantic runtime: one embedder instance (models are hundreds of
/// MB resident) and one store connection, each mutex-serialized. Indexing
/// and query paths share it; a long passage batch delays queries rather
/// than doubling model RAM.
pub struct SemanticRuntime {
    profile: EmbeddingProfile,
    store: Mutex<SemanticStore>,
    embedder: Mutex<Box<dyn Embedder>>,
}

impl SemanticRuntime {
    pub fn open(store_path: &Path, embedder: Box<dyn Embedder>) -> Result<Self> {
        if let Some(parent) = store_path.parent() {
            std::fs::create_dir_all(parent).map_err(|error| {
                Error::Semantic(format!("could not create {}: {error}", parent.display()))
            })?;
        }
        let profile = embedder.profile().clone();
        let store = SemanticStore::open(store_path, &profile)?;
        Ok(Self {
            profile,
            store: Mutex::new(store),
            embedder: Mutex::new(embedder),
        })
    }

    pub fn profile(&self) -> &EmbeddingProfile {
        &self.profile
    }

    pub fn chunk_count(&self) -> Result<usize> {
        self.lock_store()?.chunk_count()
    }

    pub fn has_source(&self, source_key: &str) -> Result<bool> {
        self.lock_store()?.has_source(source_key)
    }

    /// The content hash the source was last embedded at, if any.
    pub fn source_hash(&self, source_key: &str) -> Result<Option<String>> {
        self.lock_store()?.source_hash(source_key)
    }

    pub fn delete_source(&self, source_key: &str) -> Result<()> {
        self.lock_store()?.delete_source(source_key)
    }

    /// Embeds the chunk texts and atomically replaces the source's vectors.
    /// Skips the embedding work when the stored content hash already matches.
    pub fn embed_and_replace_source(
        &self,
        source_key: &str,
        content_hash: &str,
        chunks: &[(String, String)],
    ) -> Result<bool> {
        if self.lock_store()?.source_hash(source_key)?.as_deref() == Some(content_hash) {
            return Ok(false);
        }
        let texts: Vec<String> = chunks.iter().map(|(_, text)| text.clone()).collect();
        let vectors = self.lock_embedder()?.embed_passages(&texts)?;
        let rows: Vec<(String, Vec<f32>)> = chunks
            .iter()
            .map(|(chunk_id, _)| chunk_id.clone())
            .zip(vectors)
            .collect();
        self.lock_store()?
            .replace_source(source_key, content_hash, &rows)?;
        Ok(true)
    }

    /// Removes sources absent from `keep` — reconciliation for offline
    /// deletions discovered at boot.
    pub fn retain_sources(&self, keep: &std::collections::BTreeSet<String>) -> Result<usize> {
        let store = self.lock_store()?;
        let stale: Vec<String> = {
            let mut stmt = store
                .conn
                .prepare("SELECT source_key FROM source_state")
                .map_err(semantic_error)?;
            let keys = stmt
                .query_map([], |row| row.get::<_, String>(0))
                .map_err(semantic_error)?
                .collect::<std::result::Result<Vec<_>, _>>()
                .map_err(semantic_error)?;
            keys.into_iter().filter(|key| !keep.contains(key)).collect()
        };
        drop(store);
        for key in &stale {
            self.delete_source(key)?;
        }
        Ok(stale.len())
    }

    pub fn search(&self, query: &str, k: usize) -> Result<Vec<SemanticHit>> {
        let vector = self.lock_embedder()?.embed_query(query)?;
        self.lock_store()?.search(&vector, k)
    }

    fn lock_store(&self) -> Result<std::sync::MutexGuard<'_, SemanticStore>> {
        self.store
            .lock()
            .map_err(|_| Error::Semantic("semantic store mutex poisoned".to_owned()))
    }

    fn lock_embedder(&self) -> Result<std::sync::MutexGuard<'_, Box<dyn Embedder>>> {
        self.embedder
            .lock()
            .map_err(|_| Error::Semantic("embedder mutex poisoned".to_owned()))
    }
}

#[cfg(test)]
pub(crate) mod test_support {
    use super::*;

    /// Deterministic fake embedder: token-hash bag-of-words vectors so
    /// shared vocabulary produces nearby embeddings without ONNX.
    pub struct HashEmbedder {
        profile: EmbeddingProfile,
    }

    impl HashEmbedder {
        pub const DIMENSIONS: usize = 32;

        pub fn new() -> Self {
            let mut profile = EmbeddingProfile::bge_small_en_v15();
            profile.model_id = "test/hash-embedder".to_owned();
            profile.artifact = "none".to_owned();
            profile.dimensions = Self::DIMENSIONS;
            profile.runtime = "test".to_owned();
            Self { profile }
        }

        fn vector(text: &str) -> Vec<f32> {
            let mut vector = vec![0.0f32; Self::DIMENSIONS];
            for token in text
                .to_lowercase()
                .split(|c: char| !c.is_alphanumeric())
                .filter(|token| !token.is_empty())
            {
                let digest = Sha256::digest(token.as_bytes());
                let slot = (digest[0] as usize) % Self::DIMENSIONS;
                vector[slot] += 1.0;
            }
            let norm = vector.iter().map(|v| v * v).sum::<f32>().sqrt();
            if norm > 0.0 {
                for value in &mut vector {
                    *value /= norm;
                }
            }
            vector
        }
    }

    impl Embedder for HashEmbedder {
        fn profile(&self) -> &EmbeddingProfile {
            &self.profile
        }

        fn embed_passages(&mut self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
            Ok(texts.iter().map(|text| Self::vector(text)).collect())
        }

        fn embed_query(&mut self, text: &str) -> Result<Vec<f32>> {
            Ok(Self::vector(text))
        }
    }
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use super::test_support::HashEmbedder;
    use super::*;

    fn store(dir: &Path, embedder: &HashEmbedder) -> SemanticStore {
        SemanticStore::open(&dir.join("semantic.db"), embedder.profile()).unwrap()
    }

    #[test]
    fn replace_delete_and_search_round_trip() {
        let dir = tempdir().unwrap();
        let mut embedder = HashEmbedder::new();
        let mut store = store(dir.path(), &embedder);

        let texts = vec![
            "phosphorescent deterministic indexing".to_owned(),
            "watering the garden tomatoes".to_owned(),
        ];
        let vectors = embedder.embed_passages(&texts).unwrap();
        store
            .replace_source(
                "source-a",
                "hash-1",
                &[
                    ("chunk-1".to_owned(), vectors[0].clone()),
                    ("chunk-2".to_owned(), vectors[1].clone()),
                ],
            )
            .unwrap();
        assert_eq!(
            store.source_hash("source-a").unwrap().as_deref(),
            Some("hash-1")
        );
        assert_eq!(store.chunk_count().unwrap(), 2);
        assert!(store.has_source("source-a").unwrap());

        let query = embedder.embed_query("phosphorescent indexing").unwrap();
        let hits = store.search(&query, 2).unwrap();
        assert_eq!(hits[0].chunk_id, "chunk-1");

        store.delete_source("source-a").unwrap();
        assert_eq!(store.chunk_count().unwrap(), 0);
        assert!(!store.has_source("source-a").unwrap());
        assert!(store.source_hash("source-a").unwrap().is_none());
    }

    #[test]
    fn replace_is_atomic_per_source() {
        let dir = tempdir().unwrap();
        let mut embedder = HashEmbedder::new();
        let mut store = store(dir.path(), &embedder);
        let vectors = embedder
            .embed_passages(&["one".to_owned(), "two".to_owned()])
            .unwrap();
        store
            .replace_source("s", "h1", &[("c1".to_owned(), vectors[0].clone())])
            .unwrap();
        store
            .replace_source("s", "h2", &[("c2".to_owned(), vectors[1].clone())])
            .unwrap();
        assert_eq!(store.chunk_count().unwrap(), 1);
        let query = embedder.embed_query("two").unwrap();
        assert_eq!(store.search(&query, 1).unwrap()[0].chunk_id, "c2");
    }

    #[test]
    fn fingerprint_change_resets_the_store() {
        let dir = tempdir().unwrap();
        let mut embedder = HashEmbedder::new();
        {
            let mut store = store(dir.path(), &embedder);
            let vectors = embedder.embed_passages(&["hello".to_owned()]).unwrap();
            store
                .replace_source("s", "h1", &[("c1".to_owned(), vectors[0].clone())])
                .unwrap();
            assert_eq!(store.chunk_count().unwrap(), 1);
        }
        let mut changed = EmbeddingProfile::bge_small_en_v15();
        changed.model_id = "test/hash-embedder".to_owned();
        changed.artifact = "none".to_owned();
        changed.dimensions = HashEmbedder::DIMENSIONS;
        changed.runtime = "test".to_owned();
        changed.precision = "int8".to_owned();
        let reopened = SemanticStore::open(&dir.path().join("semantic.db"), &changed).unwrap();
        assert_eq!(reopened.chunk_count().unwrap(), 0);
        assert!(reopened.source_hash("s").unwrap().is_none());
    }

    #[test]
    fn dimension_mismatch_is_rejected() {
        let dir = tempdir().unwrap();
        let embedder = HashEmbedder::new();
        let mut store = store(dir.path(), &embedder);
        let error = store
            .replace_source("s", "h1", &[("c1".to_owned(), vec![1.0, 2.0])])
            .unwrap_err();
        assert!(error.to_string().contains("dimensions"));
        let error = store.search(&[1.0], 5).unwrap_err();
        assert!(error.to_string().contains("dimensions"));
    }

    #[test]
    fn rrf_fusion_is_deterministic_and_rank_weighted() {
        let lexical = vec!["a".to_owned(), "b".to_owned(), "c".to_owned()];
        let semantic = vec!["b".to_owned(), "d".to_owned()];
        let fused = rrf_fuse(&lexical, &semantic, 60.0);
        // "b" appears in both lists → highest fused score.
        assert_eq!(fused[0].0, "b");
        // "a" (lexical rank 1) beats "d" (semantic rank 2) and "c".
        assert_eq!(fused[1].0, "a");
        let again = rrf_fuse(&lexical, &semantic, 60.0);
        assert_eq!(fused, again);

        let traced = rrf_fuse_traced(&lexical, &semantic, 60.0);
        assert_eq!(traced[0].chunk_id, "b");
        assert_eq!(traced[0].lexical_rank, Some(2));
        assert_eq!(traced[0].semantic_rank, Some(1));
        assert!(traced[0].lexical_contribution > 0.0);
        assert!(traced[0].semantic_contribution > 0.0);
        assert_eq!(
            traced[0].fused_score,
            traced[0].lexical_contribution + traced[0].semantic_contribution
        );
    }

    #[test]
    fn rrf_ties_break_on_chunk_id() {
        let fused = rrf_fuse(&["b".to_owned()], &["a".to_owned()], 60.0);
        assert_eq!(fused[0].0, "a");
        assert_eq!(fused[1].0, "b");
    }
}
