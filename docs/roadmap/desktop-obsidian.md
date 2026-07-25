# Desktop and Obsidian roadmap

This page expands D1–D5 from the root roadmap. The root [`ROADMAP.md`](../../ROADMAP.md) remains the concise status and gate index.

## Product shape

Kwiry supports two desktop deployment profiles:

- the native sidecar for full local lexical, semantic, and hybrid retrieval across registered trees;
- an explicitly degraded in-plugin lexical host for the current Obsidian vault when no native process may run.

The presentation experience should converge while capabilities remain truthful and explicit.

## Delivered foundation

### D1 — deterministic lexical core

Heading-aware chunks, deterministic path-derived IDs, Tantivy BM25, project-owned results, and disposable rebuildable index state.

### D2 — daemon and reconciliation

Authenticated loopback HTTP, watching, incremental hashing, rename/delete correctness, and offline-change reconciliation.

### D3 — local semantic/hybrid

Local embeddings, sqlite-vec retrieval, RRF, explicit mode availability, and lexical failure isolation. Real-vault relevance remains iterative.

### D4 — guided lifecycle

Guided setup plus native Windows/Linux per-user lifecycle. Native installers remain planned.

## D5A — daemon-backed plugin

State: delivered beta.

The current BRAT-installable plugin provides a query modal, result rendering, explicit mode controls, and local daemon status. Daily-drive UX, explanations, richer insert/open actions, and broader distribution remain open.

## D5B — no-daemon in-plugin lexical host

State: Gate 5 candidate implemented and under verification; Gates 1–4 are owner-accepted GO. The production host remains undelivered pending generated/reference-hardware performance, installed packaging/update/rollback evidence, private aggregate-only evidence, and explicit field acceptance.

### Checkpoint B0 — Tantivy-WASM feasibility — Completed NO-GO for normal incremental writer

Native controls passed. Minimal Tantivy 0.26.1 compiled to `wasm32-unknown-unknown`, and Node successfully executed the thread-free fresh-index path with normal tokenization, query parsing, BM25, stored fields, and RAM-directory reopen. Standard `IndexWriter` then failed at creation because it could not spawn the segment-updater thread.

The approved kill criterion stopped work before Electron/BRAT packaging, core extraction, or plugin integration. See [`../design/obsidian-lite.md`](../design/obsidian-lite.md) and [`../../bench/tantivy-wasm/README.md`](../../bench/tantivy-wasm/README.md).

The owner selected official SQLite FTS5-WASM as the next bounded path. This does not reopen or erase the Tantivy evidence.

### Checkpoint B1 — official FTS5-WASM runtime — Completed GO

The pinned official package, FTS5 availability, realistic external-content schema/triggers, fixed weighted standard BM25, query syntax, inert excerpts, transactional source replacement, delete/rename behavior, rollback, integrity, close, and repeated lifecycle passed in an isolated in-memory package. No OPFS, CDN, custom SQLite build, Obsidian dependency, worker packaging, or production integration was used. See [`../../bench/fts5-wasm/README.md`](../../bench/fts5-wasm/README.md).

### Checkpoint B2 — one-file Obsidian compatibility — Completed GO

The isolated compatibility plugin embeds a classic application-owned Worker and the exact SQLite WASM payload into one CommonJS `main.js`. Automated protocol, artifact, privacy, real-Worker, corruption, deterministic-build, and 25-cycle lifecycle checks pass with no runtime network, persistence, helper Worker, or loose runtime asset. CI enforces the Gate 1 baseline and Gate 2 checks on Node 22 and 24.

Automation reported `READY_FOR_FIELD_TEST`; the owner then accepted B2 after frozen BRAT `0.0.1` installation and ten runs, update to `0.0.2`, full restart/rerun, rollback to `0.0.1`, and final rerun passed under desktop Obsidian. The stuck BRAT add-plugin modal was accepted as a non-blocking upstream UI defect. Exact release hashes, matching versions, public delivery, and execution were verified; a separate filesystem hash of the installed copies was not recorded. See [`../../bench/fts5-wasm-obsidian-probe/README.md`](../../bench/fts5-wasm-obsidian-probe/README.md).

### Checkpoint B3 — portable Rust seam — Completed GO

`kwiry-core` exposes source-buffer ingestion, project-owned models, parsing, heading-aware chunking, deterministic IDs/metadata, technical identifiers, and query classification/planning through a portable feature. Native/OpenClast Tantivy behavior remains unchanged, and the native/portable/WASM parity and daemon regression matrix passed before owner acceptance.

### Checkpoint B4 — backend-neutral plugin — Completed GO

The Gate 4 production baseline has explicit daemon/in-plugin selection, hardened daemon credentials/responses, truthful capability/mode/status behavior, stale-result and result-origin enforcement, a strict portable Rust adapter, fixed parameterized FTS5 SQL, and a long-lived classic Worker with one complete plus at most one staging generation. The deterministic CommonJS artifact embeds exactly the portable Rust and official SQLite WASM payloads. The complete Rust/WASM/plugin matrix, corruption and denied-capability tests, Node 22/24 exact-Worker execution, deterministic artifact evidence, and installed disposable-vault Obsidian UI-foundation witness passed before the owner accepted B4 as GO. Active-vault enumeration/events and a delivered-profile claim remain excluded.

### Checkpoint B5 — active-vault lifecycle, package, and field-test — Implemented, awaiting field acceptance

The candidate builds an atomic initial in-memory index and reconciles create, modify, delete, and rename events. Its deterministic corpus, strict aggregate evidence schemas, lifecycle suite, exact generated Worker, disposable-vault smoke, one-file build, and candidate/publication workflow separation are implemented. The generated Node Worker capture meets the provisional build, warm-search, update-visibility, and event-loop targets but misses added memory; installed startup/progress and declared-reference-hardware measurements remain unavailable. Exact installed Obsidian/BRAT evidence, upgrades/rollback, private aggregate-only measurement, and explicit owner acceptance remain before calling the profile delivered.

## D5C — relevance signals and configuration

State: planned after the dependable lexical host.

### C1 — recency

Choose a testable decay model and protect canonical older notes from disappearing merely because they are old.

### C2 — properties

Normalize selected frontmatter/property types, support exact filters, and evaluate bounded field weights without indexing arbitrary private metadata by default.

### C3 — folder hierarchy

Represent ancestor segments and depth; test root/deep priors, archive penalties, active-folder proximity, and named authority folders.

### C4 — relevance profiles

Ship named, versioned, judged profiles before exposing arbitrary sliders. Require effective-profile disclosure, reset, migration, and deterministic test fixtures.

## D5D — daily drive and distribution

- truthful per-result evidence and mode presentation;
- keyboard-first open, new-tab, and link insertion workflows;
- setup/discovery and recovery polish;
- release compatibility, community review, and supported distribution;
- long-running real-vault quality and resource acceptance.

## Gates

- D5B can proceed independently of the OpenClast IG-1 acceptance gate.
- The Tantivy-WASM NO-GO returned engine strategy to the owner; the resulting FTS5-WASM selection authorizes only the named staged gates.
- Gate B1 GO proves runtime viability only; B2 packaging, B3 core extraction, and production integration remain separately gated.
- D5C scoring changes require judged evidence and owner review of public behavior.
- Passing tests do not establish owner daily-drive or distribution acceptance.
