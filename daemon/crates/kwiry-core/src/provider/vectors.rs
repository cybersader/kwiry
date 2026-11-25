// SPDX-License-Identifier: MIT OR Apache-2.0
//! Per-partition vector files for the provider's `semantic` and `hybrid`
//! modes (trial only).
//!
//! Each partition owns its own SQLite file with vectors scored by sqlite-vec.
//! Nothing is shared between partitions: a nearest-neighbour search opens
//! exactly the files of the partitions it was asked about, so vectors of any
//! other partition can neither be candidates nor change an order. Distance
//! needs no corpus statistics, so the best notes of a set of partitions are
//! exactly the merge of each partition's best notes.
//!
//! A note's vectors are stored with its id and the version (SHA-256 of the
//! exact bytes) they were computed from.

use std::path::Path;

use rusqlite::{Connection, Transaction};
use zerocopy::IntoBytes;

use super::{ProviderError, ProviderResult};

fn sql(error: impl std::fmt::Display) -> ProviderError {
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
             slot INTEGER NOT NULL,
             embedding BLOB NOT NULL CHECK(length(embedding) = {bytes})
         );
         CREATE INDEX IF NOT EXISTS chunks_slot ON chunks (slot);",
        bytes = dimensions * std::mem::size_of::<f32>(),
    ))
    .map_err(sql)?;
    Ok(conn)
}

pub(super) fn begin(conn: &mut Connection) -> ProviderResult<Transaction<'_>> {
    conn.transaction().map_err(sql)
}

pub(super) fn commit(tx: Transaction<'_>) -> ProviderResult<()> {
    tx.commit().map_err(sql)
}

pub(super) fn remove_note(tx: &Transaction<'_>, id: &str) -> ProviderResult<()> {
    tx.prepare_cached("DELETE FROM chunks WHERE slot = (SELECT slot FROM notes WHERE id = ?1)")
        .and_then(|mut statement| statement.execute([id]))
        .map_err(sql)?;
    tx.prepare_cached("DELETE FROM notes WHERE id = ?1")
        .and_then(|mut statement| statement.execute([id]))
        .map_err(sql)?;
    Ok(())
}

pub(super) fn replace_note(tx: &Transaction<'_>, id: &str, version: &str) -> ProviderResult<i64> {
    remove_note(tx, id)?;
    tx.prepare_cached("INSERT INTO notes (id, version) VALUES (?1, ?2)")
        .and_then(|mut statement| statement.insert((id, version)))
        .map_err(sql)
}

pub(super) fn insert_chunks(
    tx: &Transaction<'_>,
    chunks: impl Iterator<Item = (i64, Vec<f32>)>,
) -> ProviderResult<()> {
    let mut statement = tx
        .prepare_cached("INSERT INTO chunks (slot, embedding) VALUES (?1, ?2)")
        .map_err(sql)?;
    for (slot, vector) in chunks {
        statement.execute((slot, vector.as_bytes())).map_err(sql)?;
    }
    Ok(())
}

/// One note's best (smallest) chunk distance.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct NoteHit {
    pub id: String,
    pub distance: f64,
}

/// Exact scan of this partition: for each eligible note, score all of its
/// chunks and consider only its minimum distance. Cut after the note-level
/// comparison, including id, with no chunk window to hide notes or ties.
pub(super) fn nearest_notes(
    conn: &Connection,
    query: &[f32],
    want: usize,
    keep: impl Fn(&str, &str) -> bool,
) -> ProviderResult<Vec<NoteHit>> {
    let mut best: Vec<NoteHit> = Vec::with_capacity(want);
    if want == 0 {
        return Ok(best);
    }
    let mut notes = conn
        .prepare_cached("SELECT slot, id, version FROM notes")
        .map_err(sql)?;
    let mut distance = conn
        .prepare_cached("SELECT MIN(vec_distance_L2(embedding, ?1)) FROM chunks WHERE slot = ?2")
        .map_err(sql)?;
    let mut rows = notes.query([]).map_err(sql)?;
    while let Some(row) = rows.next().map_err(sql)? {
        let id = row.get_ref(1).map_err(sql)?.as_str().map_err(sql)?;
        let version = row.get_ref(2).map_err(sql)?.as_str().map_err(sql)?;
        if !keep(id, version) {
            continue;
        }
        let slot: i64 = row.get(0).map_err(sql)?;
        let Some(distance) = distance
            .query_row((query.as_bytes(), slot), |row| row.get::<_, Option<f64>>(0))
            .map_err(sql)?
        else {
            continue;
        };
        let position = best.partition_point(|hit| {
            hit.distance
                .total_cmp(&distance)
                .then_with(|| hit.id.as_bytes().cmp(id.as_bytes()))
                .is_lt()
        });
        if position < want {
            if best.len() == want {
                best.pop();
            }
            best.insert(
                position,
                NoteHit {
                    id: id.into(),
                    distance,
                },
            );
        }
    }
    Ok(best)
}

#[cfg(test)]
mod tests {
    use super::*;

    struct NoteVectors {
        id: String,
        version: String,
        vectors: Vec<Vec<f32>>,
    }

    fn apply(
        conn: &mut Connection,
        remove: &[String],
        upsert: &[NoteVectors],
    ) -> ProviderResult<()> {
        let tx = begin(conn)?;
        for id in remove {
            remove_note(&tx, id)?;
        }
        for note in upsert {
            let slot = replace_note(&tx, &note.id, &note.version)?;
            insert_chunks(&tx, note.vectors.iter().cloned().map(|v| (slot, v)))?;
        }
        commit(tx)
    }

    fn unit(values: &[f32]) -> Vec<f32> {
        let norm = values.iter().map(|v| v * v).sum::<f32>().sqrt();
        values.iter().map(|v| v / norm).collect()
    }

    #[test]
    fn nearest_notes_breaks_ties_before_limiting() {
        let mut ids = ["z", "y", "x", "w", "a"];
        // Put the winning id at every insertion position, independent of
        // which chunk SQLite happens to prefer in an equal-distance window.
        for _ in 0..ids.len() {
            let conn = &mut open(Path::new(":memory:"), 2).unwrap();
            let notes = ids.map(|id| NoteVectors {
                id: id.into(),
                version: "1".into(),
                vectors: vec![vec![1.0, 0.0]],
            });
            apply(conn, &[], &notes).unwrap();
            let hits = nearest_notes(conn, &[1.0, 0.0], 1, |_, _| true).unwrap();
            assert_eq!(hits[0].id, "a", "insertion order: {ids:?}");
            ids.rotate_left(1);
        }
    }

    #[test]
    fn nearest_notes_reaches_current_notes_beyond_the_chunk_cap() {
        let conn = &mut open(Path::new(":memory:"), 2).unwrap();
        let notes = [
            ("stale", "old", 4096),
            ("close", "1", 4096),
            ("far", "1", 1),
        ]
        .map(|(id, version, chunks)| NoteVectors {
            id: id.into(),
            version: version.into(),
            vectors: vec![
                if id == "far" {
                    vec![0.0, 1.0]
                } else {
                    vec![1.0, 0.0]
                };
                chunks
            ],
        });
        apply(conn, &[], &notes).unwrap();
        let hits = nearest_notes(conn, &[1.0, 0.0], 2, |_, version| version == "1").unwrap();
        assert_eq!(
            hits.iter().map(|hit| hit.id.as_str()).collect::<Vec<_>>(),
            ["close", "far"]
        );
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
        assert_eq!(hits[0].id, "b");
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM chunks", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 1);
        assert!(
            nearest_notes(&conn, &unit(&[1.0, 0.0]), 5, |_, version| version == "1")
                .unwrap()
                .is_empty()
        );
    }
}
