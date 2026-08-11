---
title: IR daemon — Gate 1 contract draft (v0)
description: Frozen-surface draft for the standalone vault search project (embedded Rust daemon — Tantivy + vectors + local ONNX embeddings). Defines the HTTP + MCP contract, chunk/metadata schema, scoping model, rebuild semantics, and host profiles (desktop sidecar, OpenClast container sidecar, and an honest degraded in-plugin tier). Drafted for owner review before any code; moves to the new repo at scaffold time.
stratum: 2
status: draft
date: 2026-07-18
tags:
  - research
  - search
  - contract
  - mcp
  - obsidian
  - openclast
---

## Status

**v0 APPROVED at Gate 1 (2026-07-18)** — owner reviewed and signed off ("everything looks good"), with two owner-driven amendments folded in: the lite tier's enterprise-deployment rationale, and semantic-lite reframed possible-but-expensive (Smart Connections precedent) rather than impossible. This document becomes `CONTRACT.md` in the new repo at scaffold time; this copy then becomes a pointer. Open questions in §10 default to the stated leans unless the owner overrides during implementation.

Decisions already locked at Gate 0: standalone repo; embedded Rust daemon (no external engine server); v1 = Linux daemon + CLI + MCP + dumb Obsidian plugin; algorithms come from libraries only (Tantivy for lexical, an embedded vector index, ONNX runtime for local embeddings); index is always a disposable derived artifact.

## 1. Design invariants (the constitution)

1. **Files are the sole source of truth.** Every byte of index state is derivable from the vault. `rebuild` from an empty data dir must converge to equivalent query behavior (identical lexical results; semantically equivalent vector results given the same model+version).
2. **One contract, many hosts.** Desktop sidecar, OpenClast container sidecar, and any future host expose the *same* HTTP + MCP surface. Clients (Obsidian plugin, CLI, agents, OpenClast gateway) never know which host they're talking to.
3. **Clients are dumb.** No chunking, ranking, or index logic in any client. The Obsidian plugin is a query box + renderer + daemon-status light.
4. **The engine is an implementation detail.** Tantivy/vector-index/embedder live behind the contract; swapping any of them must not change this document.
5. **No algorithm authorship.** Scoring, ANN, tokenization, embedding — imported only. The one exception: RRF fusion of two ranked lists (a formula, not an algorithm ecosystem).

## 2. Host profiles

| Profile | Process model | Who starts it | Transport |
|---|---|---|---|
| **Desktop sidecar** (v1) | Single binary beside Obsidian; Linux first, Windows-native later | Manual, service, or spawned by the plugin | HTTP on `127.0.0.1:<port>` + MCP (stdio and Streamable HTTP) |
| **OpenClast sidecar** (post-v1) | Same binary in the container beside the sync daemon | Container orchestration | HTTP on the container network; **only the orchestrator gateway talks to it** — browser clients go through the gateway, which enforces room scoping (Challenge 45 capability A) |
| **In-plugin lite** (optional, explicitly degraded) | Rust core compiled to WASM, running inside Obsidian's process | The plugin | Direct JS calls (no HTTP) |

### The in-plugin tier, honestly

Purpose: **not just a fallback — the only deployable tier in constrained environments.** On a locked-down enterprise desktop (no admin rights, no installing daemons, plugin-store-only software), "just a little plugin that indexes" is the entire reason Omnisearch wins there. Lite mode is how this project exists in that world at all. Scope:

- **Lexical-lite is the guaranteed tier** (Tantivy-in-WASM or FTS5-in-WASM), implementing a subset of the same contract (`search` with `mode=lexical`, `status`) as a JS adapter — plugin code identical against sidecar or lite.
- **Semantic-lite is a flagged experiment, not a promise.** Field evidence (Smart Connections) proves in-plugin local embeddings are *possible*; the cost (editor-process CPU during indexing, model RAM footprint) is what made Omnisearch-class tools stay lexical, and it's load the OpenClast browser client can't carry. Attempt only if a small quantized model via WASM/WebGPU proves nearly free on real vault sizes; otherwise lite stays lexical and semantic remains the sidecar's job.
- **Kill criterion:** if lite-mode work ever exceeds "compile the core to WASM + thin adapter" — i.e. we're re-implementing rather than re-hosting — we stop; Omnisearch already owns bespoke in-process lexical and does it well. Lite exists only while it's nearly free.

## 3. Chunk & metadata schema

Unit of indexing = **section chunk** (split at headings, with size caps and overlap policy owned by the daemon, versioned as `chunking_version`).

```jsonc
{
  "chunk_id": "sha256(vault_id + path + heading_path + chunk_ix)", // path-stable, rename-aware via path field update
  "vault_id": "string",          // registered tree identity
  "room": "string | null",       // OpenClast room tag; null on plain desktop
  "path": "notes/foo.md",        // vault-relative, forward slashes
  "heading_path": ["H1", "H2"],  // breadcrumb to the section
  "content": "string",           // the chunk text
  "frontmatter": { "...": "selected fields per daemon config (title, description, tags, status, date by default)" },
  "links_out": ["other-note"],   // wikilink targets (resolved best-effort)
  "mtime": 1234567890,
  "content_hash": "sha256",      // incremental-sync key
  "chunking_version": 1,
  "embedding": "internal — never exposed via API"
}
```

Ingestion correctness requirements (behavior copied from markdown-vault-mcp's working model, implementation ours): hash-based incremental updates; rename = ID-stable move (path updates, no orphan); delete = chunk removal; boot-time reconciliation for offline changes; debounced watcher; atomic full rebuild (build-aside, swap).

## 4. HTTP API (v0 surface)

All under `/v0/`. Content-type JSON. Errors: `{ "error": { "code", "message" } }`.

| Method & path | Purpose | Notes |
|---|---|---|
| `POST /v0/search` | The query call | Body: `q`, `mode` (`lexical` \| `semantic` \| `hybrid`, default `hybrid`), `filters` (`vault_id`, `room`, `path_prefix`, `tags`, frontmatter equals-matches), `limit` (default 20, max 100), `cursor` | Returns ranked chunks: `chunk_id`, `path`, `heading_path`, `score`, `excerpt` (highlighted), `frontmatter` subset |
| `GET /v0/chunks/{chunk_id}` | Fetch one chunk's full content + metadata | For result expansion |
| `GET /v0/status` | Daemon + per-vault index state | doc/chunk counts, last-sync, model name+version, `chunking_version`, dirty/rebuilding flags |
| `POST /v0/vaults` | Register a tree to index | `{ path, vault_id, room? }` — admin-scoped |
| `DELETE /v0/vaults/{vault_id}` | Unregister + drop its index | admin-scoped |
| `POST /v0/rebuild` | Full rebuild (per-vault or all) | admin-scoped; async — poll `/v0/status`; build-aside + atomic swap so search stays up |
| `GET /v0/health` | Liveness | unauthenticated |

Non-goals for v0: write/edit APIs (this is a *search* service — vault writes belong to the editor/sync layer), aggregations, saved searches, multi-query.

## 5. Scoping & auth model

- **Desktop default:** loopback bind, single bearer token from the daemon's config file. Plugin/CLI read it locally. Good enough for a personal machine; not the trust boundary.
- **Token scopes:** `search` (search/chunks/status), `admin` (vaults/rebuild). MCP gets `search` by default.
- **Filter-pinned tokens:** a token may carry pinned filters (`vault_id`/`room` sets) that are ANDed into every query server-side — the mechanism the OpenClast gateway uses for room scoping (gateway holds admin; mints/uses pinned-scope access per session). Modeled on Meilisearch tenant tokens; ours is a signed claim the daemon verifies.
- **The real trust boundary lives in the host:** on desktop, OS user; in OpenClast, the gateway. The daemon enforces scopes but never invents identity.

## 6. MCP surface (v0)

Transports: stdio (desktop agents) + Streamable HTTP (gateway/remote). Tools deliberately few:

1. `search` — mirrors `POST /v0/search`; description written for agent ergonomics ("hybrid semantic+keyword search over the user's notes; use `mode=lexical` for exact identifiers").
2. `get_chunk` — mirrors chunk fetch.
3. `index_status` — mirrors `/v0/status`.

No `rebuild`/`register` via MCP in v0 (admin ops stay human). Add tools only with evidence of need — 3 good tools beat 51.

## 7. Rebuild-from-nothing as a CI contract

The repo ships a fixture vault and a CI job: index it → record query results for a fixed query set → delete all derived state → rebuild → assert lexical results identical and hybrid results stable within a defined tolerance. This is the delete-the-DB test made permanent (mirrors OpenClast's golden-hash pattern). Any PR that breaks determinism fails CI.

## 8. Engine internals (informative, not contractual)

- Lexical: **Tantivy** (BM25, the ck lineage).
- Vectors: candidate crates — usearch, hnsw_rs, or sqlite-vec-via-rusqlite; pick at implementation time by maintenance + WASM story (lite mode prefers an FTS-only WASM core anyway).
- Embeddings: **fastembed-rs** (ONNX, bge-small class — the ck default lineage) vs **ort** directly vs **candle** — open question flagged at Gate 0, resolve in the first implementation wave with a micro-benchmark on real vault content. Model + version recorded in `/v0/status`; model change ⇒ semantic reindex (hash-keyed, incremental).
- Fusion: RRF over the two ranked lists (the permitted formula).

## 9. Repo shape & naming

```
<repo>/
  CONTRACT.md          ← this document, frozen
  daemon/              ← Rust workspace (core lib + bin)
  clients/obsidian/    ← the dumb plugin (TS)
  clients/cli/         ← thin CLI (may be a daemon subcommand instead)
  fixtures/vault/      ← CI fixture
  docs/
```

Working name: **kwir** ("knowledge-work IR", pronounced "quire" — a gathering of pages). Scope note: "vault" in this document means any registered tree of markdown/text (workspaces, repos, docs), not specifically an Obsidian vault — Obsidian is one client among several. Owner may still rename at repo creation.

## 10. Open questions for the review

1. In-plugin lite tier: keep (with the kill criterion) or cut from the contract entirely until sidecar v1 ships?
2. Frontmatter fields in the default index set — the listed five, or configurable-only from day one?
3. Should the CLI be a separate client or `daemon search "q"` subcommands? (Lean: subcommand — one binary.)
4. Port + config conventions: fixed default port vs per-vault registry file (lean: one daemon, many vaults, one port, config in `~/.config/<name>/`).
5. Handoff timing to OpenClast: send this draft now for gateway-requirements feedback (Challenge 45 reply-handoff), or freeze v0 first and hand off frozen? (Lean: send draft now — cheaper to incorporate before freezing.)
