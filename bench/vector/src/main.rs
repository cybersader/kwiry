//! sqlite-vec acceptance gate for kwir Vertical 3.
//!
//! Proves: (1) sqlite-vec registers against current rusqlite, (2) delete +
//! reinsert works transactionally alongside a chunk-mapping table, and
//! (3) exact-KNN latency over 384-dim vectors at vault-realistic sizes.
//!
//! Usage: kwir-vector-bench [sizes...]   (default: 25000 50000 100000)

use std::time::Instant;

use anyhow::{ensure, Context, Result};
use rusqlite::{ffi::sqlite3_auto_extension, Connection};
use serde::Serialize;
use sqlite_vec::sqlite3_vec_init;
use zerocopy::IntoBytes;

const DIM: usize = 384;
const QUERIES: usize = 20;
const TOP_K: usize = 40;

#[derive(Serialize)]
struct SizeReport {
    chunks: usize,
    insert_ms: u128,
    knn_p50_ms: f64,
    knn_p95_ms: f64,
    delete_reinsert_1pct_ms: u128,
    db_size_mib: u64,
}

/// Deterministic pseudo-random unit-ish vector (xorshift; no rand dep).
fn vector_for(seed: u64) -> Vec<f32> {
    let mut state = seed.wrapping_mul(0x9E3779B97F4A7C15).max(1);
    (0..DIM)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            ((state as f64 / u64::MAX as f64) as f32) - 0.5
        })
        .collect()
}

fn bench_size(count: usize) -> Result<SizeReport> {
    let dir = std::env::temp_dir().join(format!("kwir-vec-bench-{count}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir)?;
    let db_path = dir.join("vectors.db");
    let db = Connection::open(&db_path)?;

    db.execute_batch(
        "CREATE TABLE chunk_map (
             vector_rowid INTEGER PRIMARY KEY,
             chunk_id TEXT NOT NULL UNIQUE
         );
         CREATE VIRTUAL TABLE chunk_vec USING vec0(embedding float[384]);",
    )?;

    // Bulk insert, one transaction per 1000 rows (watcher-batch shaped).
    let insert_start = Instant::now();
    let mut inserted = 0usize;
    while inserted < count {
        let tx = db.unchecked_transaction()?;
        let end = (inserted + 1000).min(count);
        for i in inserted..end {
            tx.execute(
                "INSERT INTO chunk_map (vector_rowid, chunk_id) VALUES (?1, ?2)",
                (i as i64 + 1, format!("chunk-{i:08x}")),
            )?;
            tx.execute(
                "INSERT INTO chunk_vec (rowid, embedding) VALUES (?1, ?2)",
                (i as i64 + 1, vector_for(i as u64).as_bytes()),
            )?;
        }
        tx.commit()?;
        inserted = end;
    }
    let insert_ms = insert_start.elapsed().as_millis();

    // Exact KNN latency: sqlite-vec vec0 KNN needs `MATCH ... ORDER BY distance`.
    let mut knn_ms = Vec::with_capacity(QUERIES);
    let mut stmt = db.prepare(
        "SELECT m.chunk_id, v.distance
         FROM chunk_vec v JOIN chunk_map m ON m.vector_rowid = v.rowid
         WHERE v.embedding MATCH ?1 AND v.k = ?2
         ORDER BY v.distance",
    )?;
    for q in 0..QUERIES {
        let query = vector_for(0xC0FFEE + q as u64);
        let start = Instant::now();
        let rows: Vec<(String, f64)> = stmt
            .query_map((query.as_bytes(), TOP_K as i64), |row| {
                Ok((row.get(0)?, row.get(1)?))
            })?
            .collect::<std::result::Result<_, _>>()?;
        knn_ms.push(start.elapsed().as_secs_f64() * 1000.0);
        ensure!(rows.len() == TOP_K, "expected {TOP_K} rows, got {}", rows.len());
    }
    knn_ms.sort_by(|a, b| a.total_cmp(b));
    let p50 = knn_ms[QUERIES / 2];
    let p95 = knn_ms[(QUERIES as f64 * 0.95) as usize - 1];

    // Delete + reinsert 1% of chunks in one transaction (content-change shape).
    let churn = (count / 100).max(1);
    let churn_start = Instant::now();
    {
        let tx = db.unchecked_transaction()?;
        for i in 0..churn {
            let rowid = i as i64 + 1;
            tx.execute("DELETE FROM chunk_vec WHERE rowid = ?1", (rowid,))?;
            tx.execute("DELETE FROM chunk_map WHERE vector_rowid = ?1", (rowid,))?;
            tx.execute(
                "INSERT INTO chunk_map (vector_rowid, chunk_id) VALUES (?1, ?2)",
                (rowid, format!("chunk-{i:08x}-v2")),
            )?;
            tx.execute(
                "INSERT INTO chunk_vec (rowid, embedding) VALUES (?1, ?2)",
                (rowid, vector_for(0xDEAD_0000 + i as u64).as_bytes()),
            )?;
        }
        tx.commit()?;
    }
    let delete_reinsert_1pct_ms = churn_start.elapsed().as_millis();

    let db_size_mib = std::fs::metadata(&db_path)?.len() / (1024 * 1024);
    drop(stmt);
    drop(db);
    let _ = std::fs::remove_dir_all(&dir);

    Ok(SizeReport {
        chunks: count,
        insert_ms,
        knn_p50_ms: p50,
        knn_p95_ms: p95,
        delete_reinsert_1pct_ms,
        db_size_mib,
    })
}

fn main() -> Result<()> {
    // Register sqlite-vec for every new connection before any open.
    unsafe {
        type AutoExtFn = unsafe extern "C" fn(
            *mut rusqlite::ffi::sqlite3,
            *mut *mut std::ffi::c_char,
            *const rusqlite::ffi::sqlite3_api_routines,
        ) -> std::ffi::c_int;
        sqlite3_auto_extension(Some(std::mem::transmute::<*const (), AutoExtFn>(
            sqlite3_vec_init as *const (),
        )));
    }

    let sizes: Vec<usize> = {
        let args: Vec<usize> = std::env::args()
            .skip(1)
            .map(|a| a.parse().context("sizes must be integers"))
            .collect::<Result<_>>()?;
        if args.is_empty() {
            vec![25_000, 50_000, 100_000]
        } else {
            args
        }
    };

    let mut reports = Vec::new();
    for size in sizes {
        reports.push(bench_size(size)?);
    }
    println!("{}", serde_json::to_string_pretty(&reports)?);
    Ok(())
}
