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

Beta.15 publishes one BRAT-installable plugin with explicit **Daemon** and **In-plugin · Lexical** profiles. Daemon retains lexical, semantic, and hybrid modes plus local daemon status. The published grouped result UX includes intentional current/tab/split/background open, note/section insertion, exact selected-text aliases, and physical <kbd>Ctrl</kbd>+<kbd>J</kbd>/<kbd>K</kbd> navigation. Daily-drive quality, explanations, owner field acceptance, and broader distribution remain open.

## D5B — no-daemon in-plugin lexical host

State: beta.15 publishes the Gate 5 active-vault lifecycle and the owner-authorized durable machine-local warm start; Gates 1–4 are owner-accepted GO. The warm-start implementation preserves the complete active generation when a replacement rebuild is incomplete. Generated/reference-hardware performance, installed long-running quality, private aggregate-only evidence, and explicit owner field acceptance remain pending.

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

### Checkpoint B5 — active-vault lifecycle and package — Published in beta.15; owner acceptance pending

Beta.15 publishes an atomic initial index and reconciles create, modify, delete, and rename events with bounded reads/queues and no partial-corpus publication. Deterministic corpora, strict aggregate evidence, lifecycle and exact-Worker tests, disposable-vault smoke, and separated candidate/publication workflows supported publication. The generated Node Worker capture meets the provisional build, warm-search, update-visibility, and event-loop targets but misses the provisional 300 MiB added-memory target; declared-reference-hardware, private real-vault, and owner field acceptance remain separate.

The current extractor baseline supports Markdown, plain text, Base, Canvas, DOCX, Excalidraw, PDF, Excel, and standalone UTF-8 HTML (`.html`/`.htm`). HTML is enabled by default; PDF and Excel remain the only default-off formats. HTML canonical title metadata is searchable and returned without becoming an authored property or body copy; latent HTML description/chrome/hidden text remains searchable without heading or identifier boosts, and HTML carries no locator or section-link support.

### Checkpoint B6 — durable differential warm start — Published in beta.15; owner acceptance pending

The implementation keeps the versioned disposable cache on machine-local storage outside the vault. It restores the previous complete generation as searchable but `stale`/`reconciling` and reconciles under an explicit freshness policy: `strict_hash` reads and hashes every discovered source, while opt-in `metadata_audit` may reuse settled metadata matches with racy-file checks and bounded rolling audits. `producer_manifest` remains unimplemented architecture. A new cache is persisted only after a complete clean generation exists. The Worker remains unable to access persistence APIs; a main-thread cache port owns bounded atomic storage. An incomplete replacement rebuild cannot displace the complete active generation or prove deletions.

The feasibility gate ran before protocol integration and passed: the exact official SQLite WASM proved export/deserialize integrity, restore speed, event-loop behavior, and bounded memory on the generated corpus, and a comparison matrix measured the cache mechanism and schema variants now in use. See [`../../bench/fts5-export-restore/README.md`](../../bench/fts5-export-restore/README.md). The cache implementation is published in beta.15. `producer_manifest` remains unimplemented, installed long-running field quality and owner acceptance remain pending, and publication does not authorize OPFS, a helper Worker, an extra WASM payload, or vault-relative cache storage.

## D5C — relevance signals and configuration

State: the open-property projection foundation is published in beta.15. Properties remain excluded from ordinary lexical eligibility, scoring, and ranking; ranking semantics and changed ordering remain separately owner-reviewed.

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

- beta.15 publishes grouped source rows, exact drill-down, format chips, no-match copy, intentional current/tab/split/background open, note/section insertion, exact selected-text aliases, and physical <kbd>Ctrl</kbd>+<kbd>J</kbd>/<kbd>K</kbd> navigation;
- truthful per-result evidence and mode presentation remain owner-reviewed;
- setup/discovery and recovery polish remain open;
- release compatibility, community review, and supported distribution remain open;
- long-running real-vault quality, resource, daily-drive, and field acceptance remain open.

## Gates

- D5B can proceed independently of the OpenClast IG-1 acceptance gate.
- The Tantivy-WASM NO-GO returned engine strategy to the owner; the resulting FTS5-WASM selection authorizes only the named staged gates.
- Gate B1 GO proves runtime viability only; B2 packaging, B3 core extraction, and production integration remain separately gated.
- Beta.15 publishes the Gate 5 lifecycle, durable warm start, D5C property projection, and D5D grouped UX.
- D5C scoring, result-order, profile/default, rule-grammar, evidence, and degradation changes require judged evidence and separate owner review of public behavior.
- Passing tests or a narrow real-host WebDriver regression proof do not establish owner field, daily-drive, ranking, or distribution acceptance.
- The B6 cache remains disposable and machine-local; cache corruption/version mismatch must discard and rebuild within In-plugin · Lexical, never switch profiles.
- Strong hashes remain authoritative. Metadata-audit fast paths require racy-file checks and bounded rolling verification; a complete producer manifest may be authoritative only for an already-approved materialized root.
