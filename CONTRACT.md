---
title: Kwiry retrieval contract (v0 + Gate A + D5B host and engine boundary)
description: Frozen product contract for the local-first retrieval engine, including HTTP/MCP commitments, deterministic rebuilds, explicit host profiles, and the owner-approved identity-governance boundary.
stratum: 2
status: approved
date: 2026-07-31
tags:
  - research
  - search
  - contract
  - mcp
  - obsidian
  - openclast
---

## Status

**v0 APPROVED at Gate 1 (2026-07-18). Gate A identity-governance amendment APPROVED (2026-07-21). D5B in-plugin host clarification and FTS5-WASM feasibility path APPROVED (2026-07-22). Durable derived-state and freshness amendment APPROVED (2026-07-25). Bounded relevance-policy amendment APPROVED (2026-07-31). Multi-format and resumable-indexing amendment APPROVED (2026-08-02).** Gate A makes authorization-before-retrieval constitutional, assigns OpenClast as the sole identity/entitlement authority and Kwiry as the retrieval enforcement point, adopts exact resource tuples, and freezes the first enterprise slice to governed lexical search. D5B clarifies that a declared host may colocate portable Kwiry Rust preprocessing and query planning with a presentation client while retrieval remains behind a project-owned interface. The durability amendment permits versioned machine-local indexes and caches only as disposable accelerators, requires truthful stale/reconciling disclosure, and keeps strong content hashes as the correctness authority. The relevance-policy amendment permits a shared source-neutral metadata model and bounded user/configuration control to reorder equally strong lexical matches, while stronger text evidence always wins. The approved constrained-host engine remains official SQLite FTS5-WASM in one application-owned worker.

Decisions already locked at Gate 0: standalone repo; embedded Rust daemon (no external engine server); retrieval algorithms come from libraries (Tantivy for lexical, an embedded vector index, ONNX runtime for local embeddings), with only the bounded policy layer allowed by invariant 6; index is always a disposable derived artifact.

## 1. Design invariants (the constitution)

1. **Files are the sole source of truth.** Every byte of index state is derivable from the registered trees or approved materialized outputs. Native indexes and in-plugin caches are disposable, versioned accelerators; deleting them and rebuilding must converge to equivalent query behavior (identical lexical results; semantically equivalent vector results given the same model+version).
2. **Authorization precedes retrieval.** Authorization constrains candidate generation, scoring statistics, vector/graph traversal, fusion, hydration, chunk fetch, counts, status, caches, and tool execution. Filtering after candidate generation is an integrity backstop, never the authorization mechanism.
3. **Explicit profiles, one project-owned model.** Desktop and OpenClast hosts use the same request/result models and core retrieval logic, but mount only the surfaces that their trust model authorizes. Profiles never silently authenticate as or fall back to one another.
4. **Presentation clients are dumb; hosts own retrieval.** No presentation adapter owns chunking, ranking, entitlement, or index logic. A declared host may colocate portable Kwiry Rust preprocessing and query planning inside the client process (the in-plugin lite profile), while retrieval remains behind project-owned host interfaces. Browser clients reach OpenClast, not the Kwiry sidecar.
5. **The engine is an implementation detail.** Native desktop and OpenClast lexical retrieval use Tantivy; the approved constrained host uses official SQLite FTS5-WASM. Both remain behind project-owned adapters and models, and no engine type crosses the host contract.
6. **Imported retrieval algorithms; bounded project-owned relevance policy.** Lexical scoring, ANN, tokenization, and embedding remain imported. Kwiry may apply a deterministic, bounded, source-neutral metadata policy after imported lexical retrieval to reorder candidates only within the same lexical-evidence tier. Recency, hierarchy, path, typed properties, source-specific mappings, and bounded user/configuration controls may refine equally strong text matches, but metadata may never let a weaker text match outrank a stronger one, widen authorization, or replace the engine's lexical score. Presentation clients do not implement this policy: shared Rust owns its model, validation, limits, and ordering, while adapters only map authorized source data and execute the declared plan. The in-plugin host may bind a project-owned allowlisted query plan to fixed parameterized FTS5 SQL, but TypeScript does not implement tokenization or a bespoke lexical ranker. RRF fusion of ranked lists remains permitted as a bounded formula.
7. **One entitlement authority.** OpenClast owns authentication, group expansion, entitlement policy, delegation, lifecycle, and capability issuance. Kwiry verifies and enforces materialized capabilities; it never reads OpenClast policy stores, interprets roles, accepts browser/OIDC sessions, or independently derives grants.
8. **Freshness is explicit.** A previous complete generation may remain searchable while authoritative reconciliation runs, but searchable never implies current. Hosts report `current`, `reconciling`, `stale`, or `unavailable` together with the declared evidence basis. Unsupported or failed fast paths rebuild within the same explicit profile; they never trigger profile or mode fallback.
9. **Derived state is machine-local.** Native data roots and in-plugin caches require machine-local or local-block storage with single-writer, reliable lock, atomic-replace, and durable-flush semantics. SMB/CIFS/NFS may host source trees, but not the derived-state root.

## 2. Host profiles

| Profile | Process model | Who starts it | Transport |
|---|---|---|---|
| **Desktop sidecar** | Single binary beside Obsidian | Manual or per-user service | HTTP on `127.0.0.1:<port>`; local bearer authentication |
| **OpenClast sidecar** (IG-1+) | Same binary on an internal container/service network | Container orchestration | Only the authenticated OpenClast gateway talks to it, using short-lived audience-bound capabilities; browsers never receive the sidecar address or credential |
| **In-plugin lite** (optional, explicitly degraded) | Portable Rust preprocessing/query planning plus official SQLite FTS5-WASM in one application-owned worker inside Obsidian; an optional main-thread store may persist a versioned machine-local cache | The plugin | Direct project-owned worker interface (no HTTP) |

### The in-plugin tier, honestly

Purpose: **not just a fallback — the only deployable tier in constrained environments.** On a locked-down enterprise desktop (no admin rights, no installing daemons, plugin-store-only software), "just a little plugin that indexes" is the entire reason Omnisearch wins there. Lite mode is how this project exists in that world at all. Scope:

- **Lexical-lite is the guaranteed tier.** The first feasibility path proved Tantivy's thread-free fresh-index route but reached the kill criterion when its normal incremental writer tried to spawn a segment-updater thread. The owner-approved official SQLite FTS5-WASM path subsequently passed its isolated runtime, one-file packaging, portable-core, and backend-neutral integration gates. Portable Rust owns source validation, Markdown/frontmatter/wikilink parsing, heading-aware chunking, deterministic IDs, retrieval metadata, technical identifiers, and query classification/planning; FTS5 owns tokenization, matching, BM25, snippets, and transactional index state; one application-owned worker binds allowlisted plans to fixed parameterized SQL. A main-thread cache store may persist only a validated complete FTS generation outside the vault on machine-local storage. The Worker itself retains no filesystem, OPFS, network, or helper-Worker capability, and the presentation plugin remains backend-neutral.
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

Ingestion correctness requirements: strong-hash authoritative updates; rename = atomic orphan-free removal/reinsert (path-derived chunk IDs change with the path); delete = chunk removal; boot-time reconciliation for offline changes; path-aware debounced watching; and atomic build-aside publication. Three explicit freshness bases are allowed: `strict_hash`, `metadata_audit` (stat fast path plus racy-file verification and bounded rolling audits), and `producer_manifest` (complete versioned strong-hash manifest over an approved materialized root). Metadata, watcher events, and future journal cursors are accelerators, never unconditional proof. Incomplete enumeration or invalid acceleration evidence suppresses inferred deletions and leaves the previous complete generation stale/reconciling.

In the OpenClast profile every registered source and chunk binds to exactly one non-null tuple:

```text
ResourceKey { tenant_id, vault_id, room_id }
```

Independent vault and room sets are prohibited because they can authorize an accidental Cartesian product. Empty or missing rooms never imply global visibility. Desktop may retain `room: null` because its OS-user boundary is a different explicit profile.

## 4. HTTP API (v0 surface)

All under `/v0/`. Content-type JSON. Errors: `{ "error": { "code", "message" } }`.

| Method & path | Purpose | Notes |
|---|---|---|
| `POST /v0/search` | The query call | Body: `q`, `mode` (`lexical` \| `semantic` \| `hybrid`, default `hybrid`), `filters` (`vault_id`, `room`, `path_prefix`, `tags`, frontmatter equals-matches), `limit` (default 20, max 100), `cursor`. The frozen JSON body remains `hits` + `next_cursor`; successful native responses also carry safe freshness and generation headers. |
| `GET /v0/chunks/{chunk_id}` | Fetch one chunk's full content + metadata | For result expansion |
| `GET /v0/status` | Daemon + per-vault index state | doc/chunk counts, last-sync, model name+version, `chunking_version`, dirty/rebuilding flags, and explicit freshness basis/state |
| `POST /v0/vaults` | Register a tree to index | `{ path, vault_id, room? }` — admin-scoped |
| `DELETE /v0/vaults/{vault_id}` | Unregister + drop its index | admin-scoped |
| `POST /v0/rebuild` | Full rebuild (per-vault or all) | admin-scoped; async — poll `/v0/status`; build-aside + atomic swap so search stays up |
| `GET /v0/health` | Liveness | unauthenticated |

Profile availability is explicit:

- Desktop exposes its currently implemented `/v0/health`, authenticated search, and authenticated status surfaces; later contract endpoints remain vertical-gated.
- IG-1 OpenClast exposes unauthenticated liveness plus capability-protected `POST /v0/search` in explicit `lexical` mode only. Semantic/hybrid, status, chunk fetch, MCP, vault registration, and rebuild are absent or return explicit unavailable/forbidden errors until their governed verticals.
- In-plugin lite uses the same project-owned search/result/status models through direct calls rather than HTTP, exposes lexical mode only, and returns explicit unavailable/building/freshness states rather than partial or silently converted results.
- A valid previous generation may answer search while reconciliation runs. Its native response reports `X-Kwiry-Index-Freshness` and `X-Kwiry-Generation`; OpenClast may forward equivalent safe headers. Searchability alone never means current.
- Unsupported modes never silently fall back to lexical, authorization failure never falls back to desktop authentication, a failed fast path only rebuilds within its selected profile, and a daemon connection failure never selects in-plugin lite automatically.

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

The repo ships fixture corpora and CI jobs that index → record query results → delete every derived index/cache → rebuild → compare behavior. Native Tantivy lexical results must be identical and hybrid results stable within the defined tolerance; in-plugin FTS5 must be deterministic for its fixed engine/profile/version. Strict filesystem, metadata-audit after complete verification, and producer-manifest paths must converge to the same applicable engine results for the same bytes. Cross-engine raw scores or exact total ordering are not compared. Any PR that makes a cache or manifest load-bearing, or breaks the applicable profile's empty-state determinism, fails CI.

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

## 10. Multi-format and resumable indexing (amendment, 2026-08-02)

1. **Source formats.** The source model admits a closed, versioned set of formats: `markdown`, `text`, `base`, `canvas`, `docx`, `pdf`. Rust core owns all parsing and extraction; hosts only classify files and read bytes.
2. **Extracted sections.** Non-Markdown formats produce ordered, deterministic extracted sections that feed the same chunker, property bag, and identifier pipeline as Markdown. Chunk IDs remain deterministic and path-derived.
3. **Coverage honesty.** Source outcomes are: indexed-complete, indexed-partial, skipped-no-extractable-text, unreadable/omitted, and quarantined. A partially extracted source counts as indexed and as partial; counts are reported per format. No silent skips.
4. **Locators.** Results may carry a non-ranking locator (for example PDF page, Base view, Canvas node) used only for navigation and display.
5. **Ranking neutrality by default.** All formats rank by identical text-evidence rules. Format may act as an optional, user-configured ranking signal on the same footing as other bounded relevance-policy signals: it may only reorder equally strong text matches and may never let weaker text evidence outrank stronger text evidence.
6. **Format policy versioning.** Enabled formats and extraction policy participate in cache and index compatibility; a policy change triggers an honest rebuild and never silent serving of rows built under a different policy.
7. **Resumable builds.** Interrupted initial builds may checkpoint acknowledged progress to machine-local storage and resume after restart. A checkpoint is never a complete or servable generation; resumed work is reconciled against current file state before continuing, and any identity or policy mismatch discards the checkpoint in favor of an honest fresh build.

## 11. Reserved later gates

The following remain outside IG-1 and require their own owner-reviewed verticals: governed semantic/hybrid retrieval; scoped chunk/status surfaces; MCP parity; monotonic policy/subject epochs and revocation; connector ACL normalization; remote agent delegation; and structural graph traversal. None may weaken the invariants above or silently broaden a profile.
