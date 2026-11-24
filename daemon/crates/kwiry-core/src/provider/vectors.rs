// SPDX-License-Identifier: MIT OR Apache-2.0
//! Per-partition vector files for the provider's `semantic` and `hybrid`
//! modes (trial only).
//!
//! Each partition owns its own SQLite file with a sqlite-vec `vec0` table.
//! Nothing is shared between partitions: a nearest-neighbour search opens
//! exactly the files of the partitions it was asked about, so vectors of any
//! other partition can neither be candidates nor change an order. Distance
//! needs no corpus statistics, so the best notes of a set of partitions are
//! exactly the merge of each partition's best notes.
//!
//! A note's vectors are stored with its id and the version (SHA-256 of the
//! exact bytes) they were computed from.

use std::collections::HashMap;
use std::path::Path;

use rusqlite::Connection;
use zerocopy::IntoBytes;

use super::{ProviderError, ProviderResult};

/// sqlite-vec's largest `k` for one KNN query.
const KNN_MAX: usize = 4096;

fn sql(error: rusqlite::Error) -> ProviderError {
    ProviderError::internal(format!("vector store: {error}"))
}

/// Opens (creating if needed) a vector file for `dimensions`-wide vectors.
pub(super) fn open(path: &Path, dimensions: usize) -> ProviderResult<Connection> {
    crate::semantic::register_sqlite_vec();
    let conn = Connection::open(path).map_err(sql)?;
    conn.execute_batch(&format!(
        "PRAGMA journal_mode = WAL;
         PRAGMA synchronous = NORMAL;
         CREATE TABLE IF NOT EXISTS notes (
             slot INTEGER PRIMARY KEY,
             id TEXT NOT NULL UNIQUE,
             version TEXT NOT NULL
         );
         CREATE TABLE IF NOT EXISTS chunks (
             rowid INTEGER PRIMARY KEY,
             slot INTEGER NOT NULL
         );
         CREATE INDEX IF NOT EXISTS chunks_slot ON chunks (slot);
         CREATE VIRTUAL TABLE IF NOT EXISTS vectors USING vec0(embedding float[{dimensions}]);"
    ))
    .map_err(sql)?;
    Ok(conn)
}

/// A note's vectors: one per chunk.
pub(super) struct NoteVectors {
    pub id: String,
    pub version: String,
    pub vectors: Vec<Vec<f32>>,
}

/// Applies one batch of changes in one transaction: every id in `remove`
/// and every id in `upsert` loses its old vectors first.
pub(super) fn apply(
    conn: &mut Connection,
    remove: &[String],
    upsert: &[NoteVectors],
) -> ProviderResult<()> {
    let tx = conn.transaction().map_err(sql)?;
    for id in remove.iter().chain(upsert.iter().map(|note| &note.id)) {
        tx.execute(
            "DELETE FROM vectors WHERE rowid IN
                 (SELECT c.rowid FROM chunks c JOIN notes n ON n.slot = c.slot WHERE n.id = ?1)",
            [id],
        )
        .map_err(sql)?;
        tx.execute(
            "DELETE FROM chunks WHERE slot IN (SELECT slot FROM notes WHERE id = ?1)",
            [id],
        )
        .map_err(sql)?;
        tx.execute("DELETE FROM notes WHERE id = ?1", [id])
            .map_err(sql)?;
    }
    for note in upsert {
        if note.vectors.is_empty() {
            continue;
        }
        tx.execute(
            "INSERT INTO notes (id, version) VALUES (?1, ?2)",
            (&note.id, &note.version),
        )
        .map_err(sql)?;
        let slot = tx.last_insert_rowid();
        for vector in &note.vectors {
            tx.execute("INSERT INTO chunks (slot) VALUES (?1)", [slot])
                .map_err(sql)?;
            let rowid = tx.last_insert_rowid();
            tx.execute(
                "INSERT INTO vectors (rowid, embedding) VALUES (?1, ?2)",
                (rowid, vector.as_bytes()),
            )
            .map_err(sql)?;
        }
    }
    tx.commit().map_err(sql)
}

pub(super) fn chunk_count(conn: &Connection) -> ProviderResult<usize> {
    conn.query_row("SELECT COUNT(*) FROM chunks", [], |row| {
        row.get::<_, i64>(0)
    })
    .map(|count| count as usize)
    .map_err(sql)
}

/// One note's best (smallest) chunk distance.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct NoteHit {
    pub id: String,
    pub version: String,
    pub distance: f64,
}

/// The `want` nearest notes of this one file that `keep` accepts, by best
/// chunk distance, ties by id bytes. Widens the chunk window until it has
/// `want` notes or has seen every chunk (or reached sqlite-vec's `k` limit).
pub(super) fn nearest_notes(
    conn: &Connection,
    query: &[f32],
    want: usize,
    keep: impl Fn(&str, &str) -> bool,
) -> ProviderResult<Vec<NoteHit>> {
    let total = chunk_count(conn)?;
    if total == 0 || want == 0 {
        return Ok(Vec::new());
    }
    let mut k = (want * 4).clamp(1, KNN_MAX).min(total);
    let mut statement = conn
        .prepare_cached(
            "SELECT n.id, n.version, v.distance
             FROM vectors v
             JOIN chunks c ON c.rowid = v.rowid
             JOIN notes n ON n.slot = c.slot
             WHERE v.embedding MATCH ?1 AND v.k = ?2",
        )
        .map_err(sql)?;
    loop {
        let mut best: HashMap<String, NoteHit> = HashMap::new();
        let rows = statement
            .query_map((query.as_bytes(), k as i64), |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, f64>(2)?,
                ))
            })
            .map_err(sql)?;
        for row in rows {
            let (id, version, distance) = row.map_err(sql)?;
            if !keep(&id, &version) {
                continue;
            }
            best.entry(id.clone())
                .and_modify(|hit| hit.distance = hit.distance.min(distance))
                .or_insert(NoteHit {
                    id,
                    version,
                    distance,
                });
        }
        if best.len() >= want || k >= total || k >= KNN_MAX {
            let mut hits: Vec<_> = best.into_values().collect();
            sort_hits(&mut hits);
            hits.truncate(want);
            return Ok(hits);
        }
        k = (k * 4).min(KNN_MAX).min(total);
    }
}

pub(super) fn sort_hits(hits: &mut [NoteHit]) {
    hits.sort_by(|left, right| {
        left.distance
            .total_cmp(&right.distance)
            .then_with(|| left.id.as_bytes().cmp(right.id.as_bytes()))
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unit(values: &[f32]) -> Vec<f32> {
        let norm = values.iter().map(|v| v * v).sum::<f32>().sqrt();
        values.iter().map(|v| v / norm).collect()
    }

    #[test]
    fn nearest_notes_collapse_chunks_and_replace_atomically() {
        let dir = tempfile::tempdir().unwrap();
        let mut conn = open(&dir.path().join("v.sqlite"), 2).unwrap();
        apply(
            &mut conn,
            &[],
            &[
                NoteVectors {
                    id: "a".into(),
                    version: "1".into(),
                    vectors: vec![unit(&[1.0, 0.0]), unit(&[1.0, 0.1])],
                },
                NoteVectors {
                    id: "b".into(),
                    version: "1".into(),
                    vectors: vec![unit(&[0.0, 1.0])],
                },
            ],
        )
        .unwrap();
        let hits = nearest_notes(&conn, &unit(&[1.0, 0.0]), 5, |_, _| true).unwrap();
        assert_eq!(
            hits.iter().map(|h| h.id.as_str()).collect::<Vec<_>>(),
            ["a", "b"]
        );
        apply(
            &mut conn,
            &["a".into()],
            &[NoteVectors {
                id: "b".into(),
                version: "2".into(),
                vectors: vec![unit(&[1.0, 0.0])],
            }],
        )
        .unwrap();
        let hits = nearest_notes(&conn, &unit(&[1.0, 0.0]), 5, |_, _| true).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!((hits[0].id.as_str(), hits[0].version.as_str()), ("b", "2"));
        assert_eq!(chunk_count(&conn).unwrap(), 1);
        assert!(
            nearest_notes(&conn, &unit(&[1.0, 0.0]), 5, |_, version| version == "1")
                .unwrap()
                .is_empty()
        );
    }
}
