---
title: Kwiry retrieval contract (v0 + Gate A + D5B host and engine boundary)
description: Frozen product contract for the local-first retrieval engine, including HTTP/MCP commitments, deterministic rebuilds, explicit host profiles, and the owner-approved identity-governance boundary.
stratum: 2
status: approved
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

**v0 APPROVED at Gate 1 (2026-07-18). Gate A identity-governance amendment APPROVED (2026-07-21). D5B in-plugin host clarification and FTS5-WASM feasibility path APPROVED (2026-07-22).** Gate A makes authorization-before-retrieval constitutional, assigns OpenClast as the sole identity/entitlement authority and Kwiry as the retrieval enforcement point, adopts exact resource tuples, and freezes the first enterprise slice to governed lexical search. D5B clarifies that a declared host may colocate portable Kwiry Rust preprocessing and query planning with a presentation client while retrieval remains behind a project-owned interface. The approved next feasibility path uses official SQLite FTS5-WASM in an application-owned worker; this records a spike direction, not a delivered profile.

Decisions already locked at Gate 0: standalone repo; embedded Rust daemon (no external engine server); algorithms come from libraries only (Tantivy for lexical, an embedded vector index, ONNX runtime for local embeddings); index is always a disposable derived artifact.

## 1. Design invariants (the constitution)

1. **Files are the sole source of truth.** Every byte of index state is derivable from the registered trees. `rebuild` from an empty data dir must converge to equivalent query behavior (identical lexical results; semantically equivalent vector results given the same model+version).
2. **Authorization precedes retrieval.** Authorization constrains candidate generation, scoring statistics, vector/graph traversal, fusion, hydration, chunk fetch, counts, status, caches, and tool execution. Filtering after candidate generation is an integrity backstop, never the authorization mechanism.
3. **Explicit profiles, one project-owned model.** Desktop and OpenClast hosts use the same request/result models and core retrieval logic, but mount only the surfaces that their trust model authorizes. Profiles never silently authenticate as or fall back to one another.
4. **Presentation clients are dumb; hosts own retrieval.** No presentation adapter owns chunking, ranking, entitlement, or index logic. A declared host may colocate portable Kwiry Rust preprocessing and query planning inside the client process (the in-plugin lite profile), while retrieval remains behind project-owned host interfaces. Browser clients reach OpenClast, not the Kwiry sidecar.
5. **The engine is an implementation detail.** Native desktop and OpenClast lexical retrieval use Tantivy; the approved constrained-host feasibility path uses official SQLite FTS5-WASM. Both remain behind project-owned adapters and models, and no engine type crosses the host contract.
6. **No algorithm authorship.** Scoring, ANN, tokenization, embedding — imported only. The in-plugin host may bind a project-owned allowlisted query plan to fixed parameterized FTS5 SQL, but TypeScript does not implement tokenization or a bespoke ranker. The one exception is RRF fusion of two ranked lists (a formula, not an algorithm ecosystem).
7. **One entitlement authority.** OpenClast owns authentication, group expansion, entitlement policy, delegation, lifecycle, and capability issuance. Kwiry verifies and enforces materialized capabilities; it never reads OpenClast policy stores, interprets roles, accepts browser/OIDC sessions, or independently derives grants.

## 2. Host profiles

| Profile | Process model | Who starts it | Transport |
|---|---|---|---|
| **Desktop sidecar** | Single binary beside Obsidian | Manual or per-user service | HTTP on `127.0.0.1:<port>`; local bearer authentication |
| **OpenClast sidecar** (IG-1+) | Same binary on an internal container/service network | Container orchestration | Only the authenticated OpenClast gateway talks to it, using short-lived audience-bound capabilities; browsers never receive the sidecar address or credential |
| **In-plugin lite** (optional, explicitly degraded) | Portable Rust preprocessing/query planning plus official SQLite FTS5-WASM in an application-owned worker inside Obsidian | The plugin | Direct project-owned worker interface (no HTTP) |

### The in-plugin tier, honestly

Purpose: **not just a fallback — the only deployable tier in constrained environments.** On a locked-down enterprise desktop (no admin rights, no installing daemons, plugin-store-only software), "just a little plugin that indexes" is the entire reason Omnisearch wins there. Lite mode is how this project exists in that world at all. Scope:

- **Lexical-lite is the guaranteed tier.** The first feasibility path proved Tantivy's thread-free fresh-index route but reached the kill criterion when its normal incremental writer tried to spawn a segment-updater thread. The owner-approved official SQLite FTS5-WASM Gate 1 subsequently passed as a standalone in-memory runtime proof; Obsidian packaging and production integration remain unproved. In the intended boundary, portable Rust owns source validation, Markdown/frontmatter/wikilink parsing, heading-aware chunking, deterministic IDs, retrieval metadata, technical identifiers, and query classification/planning; FTS5 owns tokenization, matching, BM25, snippets, and transactional index state; an application-owned worker binds allowlisted plans to fixed parameterized SQL. The presentation plugin remains backend-neutral.
- **Cross-engine acceptance is semantic, not numeric.** Native/OpenClast Tantivy and in-plugin FTS5 must receive identical prepared chunks and metadata. Behavior must be deterministic within each engine/profile/version and satisfy judged relevance floors, filters, and technical-identifier expectations. Raw scores and exact total ordering are not required to match across Tantivy and FTS5.
- **Semantic-lite is a flagged experiment, not a promise.** Field evidence (Smart Connections) proves in-plugin local embeddings are *possible*; the cost (editor-process CPU during indexing, model RAM footprint) is what made Omnisearch-class tools stay lexical, and it's load the OpenClast browser client can't carry. Attempt only if a small quantized model via WASM/WebGPU proves nearly free on real vault sizes; otherwise lite stays lexical and semantic remains the sidecar's job.
- **Kill criterion:** stop if lite-mode work requires a Tantivy or SQLite fork, custom tokenizer/ranker, TypeScript Markdown parsing or ID generation, duplicated canonical models, or a broad core rewrite solely for the plugin. Lite exists only while it is a bounded host, not a second authored retrieval implementation.

## 3. Chunk & metadata schema

Unit of indexing = **section chunk** (split at headings, with size caps and overlap policy owned by Kwiry core and versioned as `chunking_version`; native and in-plugin hosts reuse that policy).

```jsonc
{
  "chunk_id": "sha256(vault_id + path + heading_path + chunk_ix)", // deterministic and path-derived; changes when the path changes
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

Ingestion correctness requirements (behavior copied from markdown-vault-mcp's working model, implementation ours): hash-based incremental updates; rename = atomic orphan-free removal/reinsert (path-derived chunk IDs change with the path); delete = chunk removal; boot-time reconciliation for offline changes; debounced watcher; atomic full rebuild (build-aside, swap).

In the OpenClast profile every registered source and chunk binds to exactly one non-null tuple:

```text
ResourceKey { tenant_id, vault_id, room_id }
```

Independent vault and room sets are prohibited because they can authorize an accidental Cartesian product. Empty or missing rooms never imply global visibility. Desktop may retain `room: null` because its OS-user boundary is a different explicit profile.

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

Profile availability is explicit:

- Desktop exposes its currently implemented `/v0/health`, authenticated search, and authenticated status surfaces; later contract endpoints remain vertical-gated.
- IG-1 OpenClast exposes unauthenticated liveness plus capability-protected `POST /v0/search` in explicit `lexical` mode only. Semantic/hybrid, status, chunk fetch, MCP, vault registration, and rebuild are absent or return explicit unavailable/forbidden errors until their governed verticals.
- In-plugin lite uses the same project-owned search/result/status models through direct calls rather than HTTP, exposes lexical mode only, and returns explicit unavailable/building states rather than partial or silently converted results.
- Unsupported modes never silently fall back to lexical, authorization failure never falls back to desktop authentication, and a daemon connection failure never selects in-plugin lite automatically.

Non-goals for v0: write/edit APIs (this is a *search* service — vault writes belong to the editor/sync layer), aggregations, saved searches, multi-query.

## 5. Identity, scoping, and authorization model

### Desktop

- Loopback bind and one local bearer token stored under owner-only OS permissions.
- The OS user is the trust boundary. This credential is never accepted by the OpenClast profile.
- Search and administrative authority remain profile-local and are not transferable to enterprise capabilities.

### OpenClast PDP → Kwiry PEP

- OpenClast authenticates the subject, expands groups, resolves current folder grants, owns policy/lifecycle, and mints a fresh internal capability per request.
- Kwiry accepts only a signed, short-lived, asymmetric, `kid`-selected capability with exact issuer/audience, represented subject, distinct current actor, JTI, time bounds, allowed actions, exact `ResourceKey` values, and request constraints.
- The external/browser credential is never forwarded to Kwiry. The internal Kwiry capability is never returned to the browser, model, agent message, or client.
- Search and admin use separate audiences and trust roots. A search issuer cannot authorize an admin endpoint even if a token claims an admin-like string.
- Caller filters are narrowing-only query refinements: effective resources are capability resources intersected with requested vault/room filters. They never grant authority.
- Missing, malformed, expired, stale, wrongly-audienced, unsupported-algorithm, unevaluable, or resource-empty authorization fails closed.

### Retrieval enforcement

- Lexical search opens only authorized physical resource partitions. BM25 document counts, field token totals, and term document frequencies are aggregated only across those authorized partitions before `TopDocs`; forbidden documents cannot affect authorized scores or ordering.
- Semantic KNN, hybrid fusion, graph traversal, chunk fetch, scoped status, and caches must achieve the same authorization-before-retrieval rule before being mounted in OpenClast. Whole-corpus candidate generation followed by filtering is prohibited.
- Hydration rechecks stored resource identity before returning metadata or content.
- The security oracle is an authorized-only physical baseline: unauthorized partitions are never opened, traversed, scored, fused, hydrated, or cached, and promised authorized results/scores/order match that baseline.

## 6. MCP surface (governed later vertical)

MCP is not mounted in the IG-1 OpenClast profile. When delivered, it reuses the same authorization context and server-side handlers as HTTP; it does not create a second policy path. Transports: stdio (desktop agents) + Streamable HTTP through the authenticated OpenClast boundary. Tools deliberately few:

1. `search` — mirrors `POST /v0/search`; description written for agent ergonomics ("hybrid semantic+keyword search over the user's notes; use `mode=lexical` for exact identifiers").
2. `get_chunk` — mirrors chunk fetch.
3. `index_status` — mirrors `/v0/status`.

No `rebuild`/`register` via MCP in v0 (admin ops stay human). Add tools only with evidence of need — 3 good tools beat 51.

## 7. Rebuild-from-nothing as a CI contract

The repo ships a fixture vault and a CI job: index it → record query results for a fixed query set → delete all derived state → rebuild → assert native Tantivy lexical results identical and hybrid results stable within a defined tolerance. Each later in-plugin FTS5 profile must likewise rebuild to deterministic results for its own fixed engine/profile/version. Cross-engine raw scores or exact total ordering are not compared. This is the delete-the-DB test made permanent (mirrors OpenClast's golden-hash pattern). Any PR that breaks the applicable profile's determinism fails CI.

## 8. Engine internals (informative, not contractual)

- Lexical: native desktop/OpenClast use **Tantivy** BM25; the approved D5B feasibility path evaluates official **SQLite FTS5-WASM** BM25 behind the same project-owned models.
- Vectors: desktop semantic storage currently uses sqlite-vec through rusqlite. Semantic-lite remains outside the guaranteed in-plugin tier and requires its own evidence and owner gate.
- Embeddings: desktop semantic retrieval uses **fastembed-rs** with the local bge-small-en-v1.5 ONNX model. Model identity is reported through status, and changing the configured model requires rebuilding the disposable semantic projection.
- Fusion: RRF over the two ranked lists (the permitted formula).

## 9. Repo shape & naming

```
<repo>/
  CONTRACT.md          ← this document, frozen
  daemon/              ← Rust workspace (core lib + bin)
  clients/obsidian/    ← presentation adapter; daemon-backed today, with explicit daemon/in-plugin selection reserved for the D5B integration gate (TS)
  clients/cli/         ← thin CLI (may be a daemon subcommand instead)
  fixtures/vault/      ← CI fixture
  docs/
```

Name: **kwiry** ("Knowledge Workspace Information Retrieval Yoke", pronounced "query"; owner-renamed from the working name "kwir" on 2026-07-20 under this clause, expansion owner-fixed the same day). Scope note: "vault" in this document means any registered tree of markdown/text (workspaces, repos, docs), not specifically an Obsidian vault — Obsidian is one client among several.

## 10. Reserved later gates

The following remain outside IG-1 and require their own owner-reviewed verticals: governed semantic/hybrid retrieval; scoped chunk/status surfaces; MCP parity; monotonic policy/subject epochs and revocation; connector ACL normalization; remote agent delegation; and structural graph traversal. None may weaken the invariants above or silently broaden a profile.
