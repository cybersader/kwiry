# Kwiry product map

Kwiry is a local-first retrieval substrate for knowledge that remains owned by files. It is designed to serve people searching from a desktop or Obsidian today, and governed enterprise users and agents through OpenClast as the system matures.

Obsidian is an important client, not the product boundary. A registered source can be an Obsidian vault, a project repository, a documentation tree, or a future connector-materialized tree.

## Product principles

- **Files are the source of truth.** Lexical indexes, vectors, caches, and future graph projections are disposable and rebuildable.
- **Authorization precedes retrieval.** Enterprise authorization constrains candidate generation, statistics, traversal, fusion, hydration, and returned evidence.
- **Presentation clients stay thin.** Portable Rust owns parsing, extraction, chunking, deterministic metadata, and query planning; declared hosts own engine lifecycle. The published in-plugin host owns a disposable official SQLite FTS5-WASM index behind that boundary.
- **Profiles are explicit.** Daemon, in-plugin, and OpenClast hosts use declared capability/trust boundaries and never silently fall back into one another.
- **Search modes fail explicitly.** An unavailable semantic model or unsupported governed mode never silently becomes lexical search.
- **Algorithms are imported.** Kwiry composes maintained search, embedding, and vector engines rather than inventing replacements.
- **Authored structure outranks inferred structure.** Links, headings, paths, frontmatter, and connector metadata may define relationships. Model-inferred entities or graphs remain derived suggestions, never the sole durable owner of a fact or entitlement.

The binding version of these principles lives in [`CONTRACT.md`](../CONTRACT.md).

## System map

```text
Files and connector materializations
    │
    ├─ Markdown, text, and enabled supported document formats
    ├─ standalone HTML extraction supported and enabled by default
    ├─ PDF and Excel extraction supported but disabled by default
    ├─ headings, links, paths, and frontmatter
    └─ future provenance and ACL sidecars
    │
    ▼
Kwiry core
    ├─ deterministic discovery and parsing
    ├─ section chunking
    ├─ lexical projection: native Tantivy BM25; published in-plugin SQLite FTS5-WASM
    ├─ semantic projection: local embeddings / sqlite-vec
    ├─ hybrid projection: reciprocal-rank fusion
    └─ future authorized structural projections
    │
    ├─ desktop sidecar ── CLI, local HTTP, full Obsidian modes, future local MCP
    ├─ in-plugin lexical ── portable Rust preparation + FTS5-WASM
    │
    └─ OpenClast sidecar ── governed enterprise search and future agent access
```

## Surfaces and trust profiles

### Desktop profile

The desktop profile is the local `kwiry` daemon and CLI, not necessarily a separate graphical application. It uses a loopback HTTP listener and a local bearer-token file. It currently supports lexical search and optional local semantic/hybrid search.

The [Kwiry Search Obsidian plugin](../clients/obsidian/README.md) is a desktop client. In Daemon mode it reads the local token only when making a request and delegates indexing and ranking to the native daemon. In In-plugin · Lexical mode, portable Rust and an application-owned FTS5-WASM Worker maintain a disposable active-vault index; the presentation UI still does not invent parsing or ranking policy.

### OpenClast profile

OpenClast is the enterprise identity and entitlement authority. It authenticates the subject, resolves current grants, and mints a short-lived capability for an internal Kwiry sidecar. Kwiry verifies that capability and performs retrieval only over exact authorized resources.

The browser communicates with OpenClast, not directly with Kwiry. The internal Kwiry address and search capability remain server-side. The implemented lexical checkpoint, whose owner acceptance remains open, is documented in [`openclast-ig1.md`](openclast-ig1.md).

### Governed MCP and agents

Kwiry's planned MCP surface is read-only retrieval: search, scoped evidence/chunk access, and scoped status. It must reuse the same request models, authorization context, and enforcement handlers as HTTP rather than becoming a second policy path.

For enterprise use, remote agents should reach an OpenClast gateway. OpenClast remains responsible for identity, delegation, and grant resolution; Kwiry remains responsible for retrieval enforcement. Write or synchronization tools belong to the appropriate workspace/sync system, not to Kwiry's retrieval service.

### Connector materializers

Future connectors for systems such as document drives or collaboration platforms should synchronize deterministic files and versioned metadata into a registered source tree. Kwiry remains connector-blind: it indexes the resulting materialization rather than embedding source-specific APIs and entitlement logic into the search core.

A connector credential permits synchronization; it does not define a user's search entitlement. Enterprise ACLs must be normalized into the governed resource model before retrieval.

### In-plugin lexical profile

The constrained desktop profile serves environments where Obsidian may run but a native daemon may not. It uses portable Rust plus official SQLite FTS5-WASM and supports Markdown, plain text, Base, Canvas, DOCX, Excalidraw, PDF, Excel, and standalone UTF-8 HTML (`.html`/`.htm`). HTML is enabled by default; PDF and Excel are the only default-off formats. The profile never exposes semantic or hybrid modes. D5C property projection is published but excluded from lexical ranking; D5D grouped UX is published while daily-drive and distribution acceptance remain pending.

See [`design/obsidian-lite.md`](design/obsidian-lite.md) and [`roadmap/desktop-obsidian.md`](roadmap/desktop-obsidian.md).

## Search modes

### Lexical

Lexical search matches terms, phrases, headings, filenames, aliases, and technical identifiers. Native/OpenClast use Tantivy BM25. The published in-plugin profile uses FTS5 BM25 over project-owned prepared chunks and metadata; raw scores and exact total ordering are not expected to match across engines.

### Semantic

Semantic search embeds the query and passages locally, then retrieves conceptually similar chunks. It helps when the searcher remembers the idea but not the source's exact vocabulary—for example, relating a plain-language internal-audit question to material about the IIA Three Lines Model.

### Hybrid

Hybrid search combines independently ranked lexical and semantic candidates using reciprocal-rank fusion. Exact textual evidence remains valuable while vocabulary-mismatched conceptual results can also surface. It is intended to become the normal general-purpose experience where the semantic leg is available and properly authorized.

Desktop semantic and hybrid behavior is documented in [`vertical-3.md`](vertical-3.md). The OpenClast profile currently serves governed lexical search only; governed semantic/hybrid requires an authorized vector and fusion baseline before it can be exposed.

## Capability map

| Capability | Desktop/local | OpenClast enterprise |
|---|---|---|
| Lexical retrieval | Available | IG-1 working checkpoint |
| Semantic retrieval | Available when the local model is loaded | Planned governed gate |
| Hybrid retrieval | Available when the local model is loaded | Planned governed gate |
| Obsidian daemon-backed search | Available as a desktop beta | OpenClast uses its gateway instead |
| Obsidian in-plugin lexical host | Published; standalone HTML supported/on by default, PDF and Excel off by default; owner field acceptance pending | Not an enterprise browser profile |
| Read-only MCP retrieval | Planned | Planned through governed OpenClast mediation |
| Scoped chunk/evidence access | Planned | Planned with the same authorization context |
| External connectors | File-tree registration today; materializers planned | Planned with ACL/resource normalization |
| Structural graph retrieval | Research/later gate | Requires authorization parity before exposure |

A capability existing in the desktop profile does not imply that its enterprise authorization proof is complete. Published property projection does not imply accepted property ranking, and published grouped UX does not imply owner daily-drive or distribution acceptance.

## Canonical ownership

- [`CONTRACT.md`](../CONTRACT.md) owns binding invariants and committed interfaces.
- Current code and passing tests own implemented behavior.
- [`vertical-1.md`](vertical-1.md), [`vertical-2.md`](vertical-2.md), and [`vertical-3.md`](vertical-3.md) own delivered desktop behavior.
- [`setup.md`](setup.md) owns guided setup and per-user lifecycle operation.
- [`openclast-ig1.md`](openclast-ig1.md) owns the governed lexical operator path.
- [`../ROADMAP.md`](../ROADMAP.md) owns public sequencing and review gates.

## Non-goals

Kwiry is not intended to become:

- a cloud-only knowledge store;
- the sole durable home of facts that do not exist in files;
- a second OpenClast identity, role, or entitlement engine;
- a note-writing or synchronization API;
- an administrative MCP toolbox;
- a collection of client-specific ranking implementations;
- a system that hides unavailable modes behind silent fallback.
