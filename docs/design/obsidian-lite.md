# Obsidian in-plugin lexical host

## Status

D5B remains the urgent desktop need. The first approved engine path reached a technical **NO-GO** on 2026-07-22: the standalone Tantivy 0.26.1 probe proved thread-free fresh-index construction and search in WebAssembly, then failed when the normal `IndexWriter` attempted to create its segment-updater thread.

The approved boundary prohibited a Tantivy fork or replacement writer, so broad plugin/core integration stopped before daemon behavior changed. The owner subsequently selected **official SQLite FTS5-WASM** as the next bounded feasibility path. Its isolated Gate 1 runtime checkpoint passed without a patch, custom build, persistence, network dependency, or production integration.

The no-daemon host remains contractual but not delivered. The existing released Obsidian plugin remains daemon-backed. Gates 1–4 are owner-accepted GO. The current Gate 5 candidate implements active-vault snapshotting, bounded reads, atomic complete-generation publication, live create/modify/delete/rename reconciliation, progress, manual rebuild, explicit capacity failures, and bounded Worker replacement. Its automated lifecycle and exact-Worker matrices pass. The generated Node Worker capture meets the provisional build, warm-search, update-visibility, and event-loop targets but misses the provisional added-memory target; installed startup/progress and declared-reference-hardware measurements remain unavailable. Installed Obsidian, packaging/update/rollback acceptance, private aggregate-only evidence, and explicit owner acceptance still block a delivered-profile claim. Historical Tantivy evidence lives in [`../../bench/tantivy-wasm/README.md`](../../bench/tantivy-wasm/README.md); the verified FTS5 Gate 1 evidence lives in [`../../bench/fts5-wasm/README.md`](../../bench/fts5-wasm/README.md), and the Gate 3 native/WASM parity witness lives in [`../../bench/portable-core-wasm/README.md`](../../bench/portable-core-wasm/README.md).

## Problem

Some managed desktops permit Obsidian plugins but prohibit a native binary or background service. In those environments the current plugin cannot search at all, while Omnisearch can build an index inside Obsidian. D5B supplies that constrained profile without moving retrieval policy into TypeScript.

## Profile boundary

| Property | Daemon-backed desktop | In-plugin lite |
|---|---|---|
| Host | `kwiry` native process | Portable Kwiry Rust preparation/planning plus official SQLite FTS5-WASM in an application-owned worker |
| Source scope | Registered Markdown/text trees | Current open vault, Markdown only |
| Transport | Authenticated loopback HTTP | Direct project-owned worker interface |
| Modes | Lexical; semantic/hybrid when loaded | Lexical only |
| Derived state | Native disposable index | Initially in-memory disposable index |
| Credential | Local token file | None |

Selection is explicit. Daemon failure never activates lite mode, and lite never pretends to serve semantic or hybrid search.

## Staged feasibility checkpoints

### Gate 0 — Tantivy-WASM normal writer — Completed NO-GO

Native controls and the thread-free fresh-index route passed, but standard `IndexWriter` failed at `index_writer:create_writer` with `Failed to spawn segment updater thread`. Commit, term deletion, incremental replacement, Electron/BRAT packaging, shared-core extraction, and plugin integration were not attempted because the hard stop had fired.

### Gate 1 — official SQLite FTS5-WASM runtime — Completed GO

The exact `@sqlite.org/sqlite-wasm@3.53.0-build1` artifact passed 18 real-WASM tests and 14 aggregate evidence checks. SQLite 3.53.0 reported FTS5 enabled; the 864,752-byte artifact matched SHA-256 `02d7e48164395fa68f81c6ec33e9da5461be397dc57602ac0cd89b4bbba1d312`. External-content triggers, fixed weighted standard BM25, query syntax, inert excerpts, source replacement, delete/rename, rollback, integrity, clean close, and repeated lifecycle passed in memory.

This **GO** means only that the official FTS5 runtime is technically viable. No OPFS, CDN, custom SQLite build, Obsidian dependency, worker packaging, portable-core change, or production code was used in Gate 1. Gate 2 separately proved one-file installed Obsidian and frozen-BRAT compatibility.

### Gate 2 — one-file Obsidian/Electron/BRAT compatibility — Completed GO

The isolated [`../../bench/fts5-wasm-obsidian-probe/`](../../bench/fts5-wasm-obsidian-probe/) package builds one CommonJS `main.js` containing a classic application-owned Worker and the exact official SQLite WASM bytes. Eighteen automated tests and fifteen evidence checks passed, including exact browser-wrapper/artifact selection, deterministic one-file packaging, fixed synthetic FTS5 behavior, corrupt-artifact rejection, zero network/persistence/helper-Worker attempts, and 25 fresh lifecycle cycles.

Automation reported **`READY_FOR_FIELD_TEST`** and could not issue GO. The owner then accepted Gate 2 after frozen BRAT `0.0.1` installation and ten runs, update to `0.0.2`, full restart/rerun, rollback to `0.0.1`, and final rerun passed under desktop Obsidian. BRAT's add-plugin modal remained open after successful installation and was accepted as a non-blocking upstream UI defect. Exact release hashes, matching versions, public delivery, and execution were verified; a separate filesystem hash of the installed copies was not recorded. The probe uses no runtime CDN, dynamic fetch, loose Worker/WASM asset, OPFS, vault access, settings, token, production plugin code, or Rust changes.

### Gate 3 — portable Rust seam — Completed GO

`kwiry-core` now exposes a conservatively feature-gated portable boundary for source validation, Markdown/frontmatter/wikilink parsing, heading-aware chunking, deterministic IDs, retrieval metadata, technical identifiers, and query classification/planning. Native desktop and OpenClast Tantivy behavior remains unchanged. The full native regression matrix, portable-only native/WASM builds, and 21 byte-identical native/Node-WASM parity cases passed before the owner accepted Gate 3 as GO.

### Gate 4 — backend-neutral plugin integration — Completed GO

The Gate 4 production baseline has explicit daemon/in-plugin selection, capability-aware status/modes, fresh bounded no-follow token-file reads, literal-loopback daemon validation, safe exact response parsing, stale-session rejection, and identity-safe result opening with a separate daemon-to-current-vault mapping. A standalone production Rust adapter emits fixed metadata/search plan identities and opaque FTS5 MATCH values. One long-lived classic Worker embeds that Rust WASM plus the exact official SQLite WASM, owns fixed parameterized SQL and complete/staging generations, and denies network, persistence, and nested-Worker capabilities. Automated evidence reports `READY_FOR_OWNER_REVIEW`; the complete matrix and installed disposable-vault Obsidian UI-foundation witness passed before the owner accepted Gate 4 as GO.

Gate 4 deliberately stopped at `index_building` before active-vault enumeration, file reads/events, reconciliation, or a delivered-profile claim. The Gate 5 candidate now supplies that lifecycle without changing Gate 4's accepted host boundary.

### Gate 5 — active-vault lifecycle and acceptance — Implemented, awaiting field acceptance

The candidate implements atomic initial build plus create/modify/delete/rename reconciliation and has deterministic corpus, aggregate-evidence, lifecycle, exact-Worker, disposable-vault, and candidate-package automation. The generated Node Worker capture reports all seven target records: four measurable runtime targets met, added memory missed, and the two installed-host startup/progress targets unavailable. Declared-reference-hardware measurement, installed Obsidian and BRAT upgrade/rollback testing, private aggregate-only evidence, and explicit owner acceptance remain required before calling the profile delivered.

## Portable Rust boundary

The portable WASM boundary should reuse:

- project-owned request, result, frontmatter, and status models;
- source validation and Markdown/frontmatter/wikilink parsing;
- heading-aware chunking, size caps, overlap, deterministic IDs, and retrieval metadata;
- technical-identifier extraction and allowlisted query classification/planning;
- explicit mode/error vocabulary.

It should exclude native registration/configuration, filesystem walking, data-root locking, generation directories, Tantivy, native SQLite/vector state, ONNX, OpenClast partitions/capabilities, and daemon lifecycle.

Obsidian supplies source records containing a vault-relative path, text, and source metadata. Rust prepares deterministic chunks and query plans. Official SQLite FTS5 owns tokenization, matching, BM25, snippets, and transactional index state. A TypeScript worker may bind an allowlisted plan to fixed parameterized SQL and own transactions; it must not parse Markdown, create IDs, extract identifiers, tokenize, or calculate a bespoke score.

## Plugin host interface

The presentation layer should depend on a neutral backend with:

- profile ID and supported modes;
- initialize/dispose;
- search using the existing `SearchRequest`/`SearchResponse` shape;
- neutral status and build progress;
- optional manual rebuild.

The daemon backend wraps the existing HTTP client and fresh token-file reads. The lite backend owns one long-lived application Worker containing the portable Rust and official FTS5-WASM runtime.

Lite UI truthfulness requirements:

- show **In-plugin · Lexical**;
- do not cycle to semantic or hybrid;
- return `mode_unavailable` for unsupported requests;
- return `index_building` until an atomic complete generation is ready;
- never persist note content, queries, the index, or a bearer token in plugin settings.

## Active-vault lifecycle

The first implementation is desktop-only, Markdown-only, and in-memory:

1. register vault events before taking the initial snapshot;
2. enumerate Markdown files and read source text through Obsidian APIs;
3. build aside while buffering create/modify/delete/rename events;
4. replay buffered changes and atomically publish the ready generation;
5. serialize/coalesce later mutations;
6. implement rename as old-path removal plus new-path insertion because IDs are path-derived;
7. release handlers, pending work, and WASM resources on unload.

Partial-corpus search is not exposed as success.

## D5B lexical profile

The MVP preserves existing lexical fields and technical-identifier behavior: filename/stem, title, selected tags, heading breadcrumb, path text, and section content. Path text is searchable, but folder depth or ancestor importance does not yet change scores.

## D5C relevance phases

These are designed after the dependable lexical host exists:

- **Recency:** decay horizon, query/global behavior, mtime trust, and protection for authoritative older notes.
- **Properties:** typed normalization, allowlists, exact filters, per-property weighting, privacy exclusions, and rebuild semantics.
- **Folder hierarchy:** path segments, depth, ancestor matching, archive penalties, active-folder proximity, and named authority folders.
- **Configuration:** named tested profiles first, bounded values, reset, effective-profile disclosure, schema/profile versions, and migrations.

Each signal needs judged queries and reproducible regressions; arbitrary sliders are not an adequate relevance design.

## Verification and release gates

Required evidence includes:

- byte-identical prepared chunk/metadata fixtures across native and portable Rust paths;
- deterministic empty-state rebuilds within each engine/profile/version;
- judged relevance floors and technical-identifier/filter behavior across Tantivy and FTS5, without requiring raw-score or exact total-order equality;
- upsert/delete/rename and repeated initialize/dispose tests;
- actual JavaScript runtime loading, not Rust-only compilation;
- plugin tests for explicit profile/mode/building behavior and cleanup;
- generated-corpus and private real-vault performance runs with aggregate-only reporting;
- BRAT installation from exact release assets, with the Worker and WASM embedded into the standard one-file `main.js`;
- daemon-mode regression coverage and rollback to the previous daemon-only release.

Provisional release targets on declared reference hardware are: asynchronous startup begins within 100 ms, visible progress within 500 ms, roughly 10,000 notes/50 MiB indexed within 30 seconds, warm search p95 under 100 ms, normal updates visible within 300 ms, no event-loop stall over 100 ms, and added steady-state memory under 300 MiB. Measurements may revise these targets at the owner gate; they must not be silently omitted.

## Non-goals

D5B does not include semantic-lite, hybrid-lite, external trees, enterprise authorization, connector APIs, persistent index state, new ranking formulas, or fully configurable relevance.
