# kwir

**kwir** ("knowledge-work IR", pronounced "quire" — a gathering of pages) is a standalone, self-contained search daemon for knowledge workspaces: any registered tree of markdown/text files — Obsidian vaults, project repos, docs trees. Obsidian is one client among several, not the definition.

A single Rust binary provides:

- **Lexical search** (Tantivy BM25) over heading-split section chunks
- **Semantic search** (local ONNX embeddings, bge-small-en-v1.5, fully offline) — opt-in via `serve --semantic`
- **Hybrid ranking** (reciprocal rank fusion over both legs)
- A **watching daemon** with authenticated HTTP API, incremental hash-based updates, rename/delete correctness, and boot reconciliation for offline changes
- A **disposable index**: files are the sole source of truth; all derived state rebuilds from nothing, deterministically

## Quick start

```bash
cd daemon
cargo build --workspace

# Register a tree and search it from the CLI
cargo run -p kwir -- --config /tmp/kwir.toml --data-dir /tmp/kwir-data vault add --id notes --path /absolute/path/to/notes
cargo run -p kwir -- --config /tmp/kwir.toml --data-dir /tmp/kwir-data index
cargo run -p kwir -- --config /tmp/kwir.toml --data-dir /tmp/kwir-data search "your query"

# Run the daemon (add --semantic for semantic/hybrid modes)
cargo run -p kwir -- --config /tmp/kwir.toml --data-dir /tmp/kwir-data serve --semantic
```

The daemon prints its bearer-token file path on startup. Search over HTTP:

```bash
curl -X POST http://127.0.0.1:32189/v0/search \
  -H "Authorization: Bearer $(cat /tmp/kwir.token)" \
  -H 'Content-Type: application/json' \
  -d '{"q":"your query","mode":"hybrid","limit":20}'
```

## Documentation

- [`CONTRACT.md`](CONTRACT.md) — the frozen product contract: invariants, HTTP/MCP surface, host profiles
- [`docs/vertical-1.md`](docs/vertical-1.md) — index + query core
- [`docs/vertical-2.md`](docs/vertical-2.md) — daemon, watcher, auth, incremental correctness
- [`docs/vertical-3.md`](docs/vertical-3.md) — semantic + hybrid search

## Repository layout

| Path | Contents | License |
|---|---|---|
| `daemon/` | Rust workspace: `kwir-core` library + `kwir` binary | TBD (permissive) |
| `clients/obsidian/` | Obsidian plugin (dumb client: query box + renderer + status light) | GPL-3.0 |
| `fixtures/vault/` | CI fixture vault for determinism tests | — |
| `bench/` | Standalone benchmark crates (embedding runtime, vector store) | — |

The Obsidian plugin is GPL-3.0 and ports code from [Omnisearch](https://github.com/scambier/obsidian-omnisearch) by Simon Cambier. The plugin talks to the daemon only over localhost HTTP; the daemon and core carry their own (permissive) license and contain no GPL code.

## Design invariants

1. Files are the sole source of truth — every byte of index state is derivable from the vault.
2. One contract, many hosts — desktop sidecar today; container sidecar and WASM lite tier share the same surface later.
3. Clients are dumb — no chunking, ranking, or index logic outside the daemon.
4. The engine is an implementation detail — Tantivy, sqlite-vec, and fastembed live behind adapters.
5. No algorithm authorship — scoring, ANN, tokenization, and embeddings are imported; RRF fusion is the one permitted formula.
