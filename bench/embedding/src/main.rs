//! Embedding-runtime micro-benchmark for kwir Vertical 3.
//!
//! Measures cold start (model load), throughput (chunks/sec), and peak RSS
//! against real chunker output from a vault tree. One runtime per build via
//! feature flags so candidates never share a process or a lockfile
//! resolution.
//!
//! Usage:
//!   cargo run --release --features bench-fastembed -- <vault-path> [repeat] [batch]
//!
//! `repeat` multiplies the chunk list to give the runtime enough work for a
//! stable throughput number on small fixtures (default 1). `batch` is the
//! embedding batch size (default 32; peak RSS scales with it).

use std::path::PathBuf;
use std::time::Instant;

use anyhow::{bail, Context, Result};
use kwir_core::VaultRegistration;
use serde::Serialize;

#[derive(Serialize)]
struct BenchReport {
    runtime: &'static str,
    model: String,
    dimensions: usize,
    documents: usize,
    unique_chunks: usize,
    embedded_chunks: usize,
    total_chars: usize,
    cold_start_ms: u128,
    embed_ms: u128,
    chunks_per_sec: f64,
    peak_rss_mib: u64,
    threads: usize,
    batch_size: usize,
}

fn peak_rss_mib() -> u64 {
    // VmHWM from /proc/self/status is the process high-water mark in kiB.
    let status = std::fs::read_to_string("/proc/self/status").unwrap_or_default();
    status
        .lines()
        .find(|line| line.starts_with("VmHWM:"))
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|value| value.parse::<u64>().ok())
        .map(|kib| kib / 1024)
        .unwrap_or(0)
}

fn load_chunks(vault_path: &PathBuf) -> Result<(usize, Vec<String>)> {
    let registration = VaultRegistration {
        id: "bench".to_string(),
        path: vault_path.clone(),
        room: None,
    };
    let report = kwir_core::ingest_vault(&registration);
    if report.chunks.is_empty() {
        bail!("no chunks produced from {}", vault_path.display());
    }
    let texts = report
        .chunks
        .iter()
        .map(|chunk| {
            // Embed what search will see: heading breadcrumb + content.
            let mut text = chunk.heading_path.join(" > ");
            if !text.is_empty() {
                text.push('\n');
            }
            text.push_str(&chunk.content);
            text
        })
        .collect();
    Ok((report.documents, texts))
}

#[cfg(feature = "bench-fastembed")]
fn run_bench(documents: usize, texts: Vec<String>, unique: usize, batch: usize) -> Result<BenchReport> {
    use fastembed::{EmbeddingModel, InitOptions, TextEmbedding};

    let model_id = EmbeddingModel::BGESmallENV15;
    let cold_start = Instant::now();
    let mut embedder = TextEmbedding::try_new(
        InitOptions::new(model_id.clone()).with_show_download_progress(false),
    )
    .context("fastembed model init failed")?;
    let cold_start_ms = cold_start.elapsed().as_millis();

    let total_chars: usize = texts.iter().map(String::len).sum();
    let refs: Vec<&str> = texts.iter().map(String::as_str).collect();

    let embed_start = Instant::now();
    let embeddings = embedder
        .embed(refs, Some(batch))
        .context("fastembed embed failed")?;
    let embed_ms = embed_start.elapsed().as_millis();

    let dimensions = embeddings.first().map(Vec::len).unwrap_or(0);
    let secs = (embed_ms as f64 / 1000.0).max(f64::EPSILON);
    Ok(BenchReport {
        runtime: "fastembed",
        model: format!("{model_id:?}"),
        dimensions,
        documents,
        unique_chunks: unique,
        embedded_chunks: embeddings.len(),
        total_chars,
        cold_start_ms,
        embed_ms,
        chunks_per_sec: embeddings.len() as f64 / secs,
        peak_rss_mib: peak_rss_mib(),
        threads: std::thread::available_parallelism().map(|n| n.get()).unwrap_or(0),
        batch_size: batch,
    })
}

#[cfg(not(feature = "bench-fastembed"))]
fn run_bench(_documents: usize, _texts: Vec<String>, _unique: usize, _batch: usize) -> Result<BenchReport> {
    bail!("build with exactly one bench feature, e.g. --features bench-fastembed");
}

fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let vault = PathBuf::from(
        args.next()
            .context("usage: kwir-embed-bench <vault-path> [repeat]")?,
    );
    let repeat: usize = args.next().map(|r| r.parse()).transpose()?.unwrap_or(1);
    let batch: usize = args.next().map(|b| b.parse()).transpose()?.unwrap_or(32);

    let (documents, unique_texts) = load_chunks(&vault)?;
    let unique = unique_texts.len();
    let mut texts = Vec::with_capacity(unique * repeat);
    for _ in 0..repeat {
        texts.extend(unique_texts.iter().cloned());
    }

    let report = run_bench(documents, texts, unique, batch)?;
    println!("{}", serde_json::to_string_pretty(&report)?);
    Ok(())
}
