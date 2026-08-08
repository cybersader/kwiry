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

The current BRAT-installable plugin provides a query modal, result rendering, explicit mode controls, and local daemon status. Current review work implements no-match copy plus intentional current/tab/split/background open, note/section insertion, exact selected-text aliases, and physical <kbd>Ctrl</kbd>+<kbd>J</kbd>/<kbd>K</kbd> navigation. Daily-drive UX, explanations, field/release acceptance, and broader distribution remain open.

## D5B — no-daemon in-plugin lexical host

State: Gate 5 and the owner-authorized durable machine-local warm-start design have implemented and verified review checkpoints; Gates 1–4 are owner-accepted GO. The warm-start checkpoint includes preservation of the complete active generation when a replacement rebuild is incomplete. Neither Gate 5 nor the warm-start implementation is owner/field-accepted or delivered; generated/reference-hardware performance, installed packaging/update/rollback evidence, private aggregate-only evidence, and explicit owner review remain pending.

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

The candidate builds an atomic initial in-memory index and reconciles create, modify, delete, and rename events with bounded reads/queues and no partial-corpus publication. Its deterministic functional and 10,000-note/50-MiB corpora, strict aggregate evidence schemas, lifecycle suite, exact generated Worker, disposable-vault smoke, one-file build, and candidate/publication workflow separation are implemented and pass. The generated Node Worker capture meets the provisional build, warm-search, update-visibility, and event-loop targets but misses the provisional 300 MiB added-memory target; installed startup/progress and declared-reference-hardware measurements remain unavailable. Exact installed Obsidian/BRAT evidence, upgrades/rollback, private aggregate-only measurement, and explicit owner acceptance remain before calling the profile delivered.

### Checkpoint B6 — durable differential warm start — Implemented and verified in current review work; acceptance pending

The implementation keeps the versioned disposable cache on machine-local storage outside the vault. It restores the previous complete generation as searchable but `stale`/`reconciling` and reconciles under an explicit freshness policy: `strict_hash` reads and hashes every discovered source, while opt-in `metadata_audit` may reuse settled metadata matches with racy-file checks and bounded rolling audits. `producer_manifest` remains unimplemented architecture. A new cache is persisted only after a complete clean generation exists. The Worker remains unable to access persistence APIs; a main-thread cache port owns bounded atomic storage. An incomplete replacement rebuild cannot displace the complete active generation or prove deletions.

The feasibility gate ran before protocol integration and passed: the exact official SQLite WASM proved export/deserialize integrity, restore speed, event-loop behavior, and bounded memory on the generated corpus, and a comparison matrix measured the cache mechanism and schema variants now in use. See [`../../bench/fts5-export-restore/README.md`](../../bench/fts5-export-restore/README.md). The implementation checkpoint does not establish installed production-stack proof, owner/field acceptance, merge, release, publication, or delivery, and does not authorize OPFS, a helper Worker, an extra WASM payload, or vault-relative cache storage.

## D5C — relevance signals and configuration

State: the open-property projection foundation is implemented and verified in current review work; query/ranking behavior and owner acceptance remain pending.

### C1 — recency

Choose a testable decay model and protect canonical older notes from disappearing merely because they are old. No recency source, decay, default, or ranking effect is accepted yet.

### C2 — properties

Preserve the complete recursive typed property bag and durable Tantivy/SQLite projections by default as disposable derived state. Ordinary lexical search does not yet read the bag for eligibility or scoring. Open-default projection does not authorize unrestricted search or ranking exposure of private metadata. Separately resolve bounded field scopes, exact/range/text rules, policy ownership, field weights, privacy exclusions, validation, disclosure, degradation, and rebuild semantics. No grammar, default profile, evidence contract, degradation behavior, or changed ordering is accepted yet.

### C3 — folder hierarchy

Represent ancestor segments and depth; test root/deep priors, archive penalties, active-folder proximity, and named authority folders. No folder-ranking defaults are accepted yet.

### C4 — relevance profiles

Evaluate named, versioned, judged profiles before exposing arbitrary sliders. Profile fields, weights, defaults, effective-profile disclosure, reset, migration, evidence, and deterministic fixtures remain separately owner-reviewed.

Query-assistance status is split: zero-result copy is implemented; field scopes remain conditionally authorized and design-gated rather than accepted as a final grammar/default; typo assistance remains prototype-only pending performance and visible-limitation evidence.

## D5D — daily drive and distribution

- no-match copy plus intentional current/tab/split/background open, note/section insertion, exact selected-text aliases, and physical <kbd>Ctrl</kbd>+<kbd>J</kbd>/<kbd>K</kbd> navigation are implemented in current review work;
- truthful per-result evidence and mode presentation remain owner-reviewed;
- setup/discovery and recovery polish remain open;
- release compatibility, community review, and supported distribution remain open;
- long-running real-vault quality, resource, daily-drive, and field acceptance remain open.

## Gates

- D5B can proceed independently of the OpenClast IG-1 acceptance gate.
- The Tantivy-WASM NO-GO returned engine strategy to the owner; the resulting FTS5-WASM selection authorizes only the named staged gates.
- Gate B1 GO proves runtime viability only; B2 packaging, B3 core extraction, and production integration remain separately gated.
- Current branch review checkpoints are durable open-property projection (`79361a9`), complete-active-generation preservation during incomplete replacement rebuilds (`0b33e42`), and intentional search-result actions (`ca91078`); none establishes merge, delivery, field acceptance, or D5C acceptance.
- D5C scoring, result-order, profile/default, rule-grammar, evidence, and degradation changes require judged evidence and separate owner review of public behavior.
- Passing tests do not establish owner daily-drive or distribution acceptance.
- The B6 cache remains disposable and machine-local; cache corruption/version mismatch must discard and rebuild within In-plugin · Lexical, never switch profiles.
- Strong hashes remain authoritative. Metadata-audit fast paths require racy-file checks and bounded rolling verification; a complete producer manifest may be authoritative only for an already-approved materialized root.
