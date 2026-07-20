# Vertical 3 — semantic and hybrid search

Vertical 3 adds local semantic search and RRF hybrid ranking on top of the
Vertical 2 lexical daemon. Lexical behavior is unchanged; semantic is opt-in
per daemon run and can never take lexical search down.

## Running

```bash
cargo run -p kwiry -- --config /tmp/kwiry-config.toml --data-dir /tmp/kwiry-data serve --bind 127.0.0.1:32189 --semantic
```

`--semantic` loads the embedding model at startup. The first run downloads
the model (~130 MB) into `<data-dir>/models/`; later starts load from that
cache in well under a second. Without `--semantic`, the daemon behaves
exactly as Vertical 2: `mode` values other than `lexical` return
`501 mode_unavailable`.

## Search modes

`POST /v0/search` accepts `mode: "lexical" | "semantic" | "hybrid"`
(omitted defaults to `hybrid`):

- `lexical` — BM25 as in Vertical 2; identical results and behavior.
- `semantic` — nearest chunks by embedding cosine distance over the whole
  corpus, then hydrated and filter-checked against the lexical index.
  Scores are cosine similarity.
- `hybrid` — the top 100 candidates from each leg fused with reciprocal
  rank fusion (`k = 60`, the contract's one permitted formula). Ties break
  deterministically on `chunk_id`. Scores are fused RRF scores.

All filters (`vault_id`, `room`, `path_prefix`, `tags`,
`frontmatter_equals`) apply to every mode. Cursor pagination remains
unavailable.

## Engine composition

- **Embedder:** fastembed (bge-small-en-v1.5, fp32, 384 dimensions,
  512-token input cap, CLS pooling, L2-normalized). Queries carry the BGE
  query instruction prefix; passages are embedded as
  `heading breadcrumb + "\n" + chunk content`. Passage batches are 8 —
  peak RSS scales with batch size (measured ~784 MiB at 8 vs ~4 GiB at 64).
- **Vector store:** sqlite-vec (stable, exact KNN) via rusqlite at
  `<data-dir>/semantic/semantic.db`. A `chunk_map` table joins vector
  rowids to chunk IDs and source keys; deletes and reinserts happen in one
  SQLite transaction per source, mirroring the lexical delete-first model.
  Measured exact-KNN latency: ~11 ms at 25k chunks, ~39 ms at 100k
  (k=40, 384-dim), leaving usearch as an escape hatch only for far larger
  corpora.
- **Fusion:** `rrf_fuse` in `kwiry-core` — a formula over two ranked ID
  lists, no tuning surface beyond the constant.

## Model identity and reindexing

The daemon computes an embedding fingerprint from the canonical
`EmbeddingProfile` (model ID, artifact, precision, dimensions, prefixes,
pooling, normalization, runtime). `/v0/status` reports it as
`model.name` / `model.version`. The semantic store records the fingerprint
it was built with; opening the store under a different fingerprint drops
all vectors and re-embeds from scratch. Vectors from different fingerprints
never mix.

## Incremental behavior and failure isolation

- Semantic updates ride the existing reconcile path: after each lexical
  commit, changed sources are re-embedded and replaced transactionally,
  keyed by the same content hashes and source keys. Unchanged sources
  short-circuit on their stored hash.
- Boot backfill is automatic: the first reconcile offers every discovered
  source, so a fresh (or fingerprint-reset) store fills without a separate
  command, and sources deleted offline are dropped by reconciliation
  against the manifest.
- Any semantic failure (model, store, embedding) surfaces as a warning and
  leaves lexical search untouched. Without a loaded model, `semantic` and
  `hybrid` requests return `501 mode_unavailable` with an explanatory
  message — never a silent fallback to lexical.

## Verification

```bash
cargo test --workspace
```

Core coverage includes: transactional replace/delete round trips,
fingerprint-change reset, semantic/hybrid determinism, reconcile-driven
embedding updates (edit, rename, delete), and explicit
`SemanticUnavailable` behavior when no model is loaded. The end-to-end
probe mirrors the Vertical 2 lifecycle with `--semantic`: live edits become
semantically searchable within seconds, and SIGTERM shuts down cleanly.

Chunk-size note: chunks are capped at 4,000 characters, which can exceed
the model's 512-token input; oversized sections are truncated by the
tokenizer for embedding purposes only (lexical indexing sees full text).
Token-aware chunking is a flagged follow-up that would require a
`chunking_version` bump and owner approval.
