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

State: urgent need; Gates 1–3 are GO and Gate 4 backend-neutral integration is the current approved implementation boundary, while the production host remains undelivered.

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

### Checkpoint B4 — backend-neutral plugin — Current approved boundary

Add explicit daemon/in-plugin selection and capability-aware mode/status handling. Preserve and harden current daemon behavior and prevent silent fallback. The worker binds allowlisted Rust plans to fixed parameterized FTS5 SQL; TypeScript does not own Markdown parsing, IDs, tokenization, or scoring. Stop before active-vault enumeration or events.

### Checkpoint B5 — active-vault lifecycle, package, and field-test

Build an atomic initial in-memory index and reconcile create, modify, delete, and rename events. Measure generated and real vaults, verify one-file packaging, install through BRAT, test upgrades/rollback, and obtain owner acceptance before calling the profile delivered.

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
