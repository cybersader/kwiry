# Kwiry roadmap

This roadmap describes product sequencing and owner-review gates. It does not replace [`CONTRACT.md`](CONTRACT.md), and it does not treat every planned capability as already committed or implemented.

Kwiry has two related tracks:

- **Desktop/local delivery** establishes useful retrieval for a single local user and clients such as Obsidian.
- **Identity-governed delivery (`IG-N`)** proves that the same capabilities can operate safely for multiple enterprise subjects and agents through OpenClast.

An IG label is a security sequencing gate, not a product exclusion. Semantic search, hybrid ranking, MCP, connectors, and agent access remain part of the intended system even when an earlier governed checkpoint exposes only lexical search.

## Status vocabulary

- **Contractual** — explicitly owner-approved and binding in `CONTRACT.md`; later implementation review does not reopen the invariant.
- **Delivered** — implemented, verified, and available as part of the current product baseline; separately named field-quality or daily-use acceptance may still remain open.
- **Implemented, review pending** — built and verified, but awaiting the named owner checkpoint.
- **Planned** — intended sequencing with no claim that the feature exists.
- **Research** — direction is being evaluated and is not yet a product commitment.
- **Deferred** — intentionally outside the current gate.

## Desktop and local retrieval

### D1 — deterministic lexical core — Delivered

- Register Markdown and text trees.
- Deterministic discovery, parsing, heading-aware chunks, and chunk IDs.
- Tantivy lexical indexing and direct CLI search.
- Disposable rebuildable index state.

See [`docs/vertical-1.md`](docs/vertical-1.md).

### D2 — daemon and live reconciliation — Delivered

- Authenticated loopback HTTP.
- File watching, incremental updates, rename/delete correctness, and boot reconciliation.
- Stable health, status, and search behavior.

See [`docs/vertical-2.md`](docs/vertical-2.md).

### D3 — local semantic and hybrid retrieval — Delivered; field acceptance remains iterative

- Local bge-small-en-v1.5 embeddings through fastembed.
- sqlite-vec exact nearest-neighbor retrieval.
- Reciprocal-rank fusion for hybrid search.
- Explicit mode availability and lexical failure isolation.

See [`docs/vertical-3.md`](docs/vertical-3.md).

### D4 — guided setup and native per-user lifecycle — Delivered

- Guided setup and automation-safe dry runs.
- Windows Task Scheduler and Linux `systemd --user` lifecycle support.
- Readiness checks, recovery, and uninstall behavior.

Native installers and packages are still planned. See [`docs/setup.md`](docs/setup.md).

### D5A — daemon-backed Obsidian client — Delivered beta

- BRAT-installable desktop client for a running local daemon.
- Query modal, result rendering, status indication, and explicit lexical, semantic, and hybrid selection.
- The token is read on demand and never persisted by the plugin.

### D5B — no-daemon in-plugin lexical host — Gate 3 accepted; backend-neutral Gate 4 next

- Active-vault Markdown search remains the urgent need where policy or device constraints prohibit a native daemon.
- The first path proved thread-free fresh Tantivy indexes can run in WASM, but Tantivy 0.26.1's normal `IndexWriter` failed while spawning its segment-updater thread. The hard gate stopped before a fork or production integration.
- The isolated official SQLite FTS5-WASM Gate 1 passed: the pinned runtime, FTS5 behavior, external-content synchronization, weighted BM25, query/excerpt behavior, transactional source replacement, rollback, integrity, and repeated close lifecycle were verified.
- The separate one-file compatibility probe now passes automated packaging, protocol, real-Worker, privacy, corruption, deterministic-build, and 25-cycle lifecycle checks. CI enforces the Gate 1 baseline plus Gate 2 checks on Node 22 and 24. See [`bench/fts5-wasm-obsidian-probe/README.md`](bench/fts5-wasm-obsidian-probe/README.md).
- Automation reported `READY_FOR_FIELD_TEST`; the owner then accepted Gate 2 after frozen BRAT `0.0.1` installation and ten runs, update to `0.0.2`, full restart/rerun, rollback to `0.0.1`, and final rerun all passed under desktop Obsidian. BRAT's add-plugin modal remained open after successful installation and was accepted as a non-blocking upstream UI defect. A separate filesystem hash of the installed copies was not recorded; exact release hashes, versions, public delivery, and execution were verified.
- Portable Rust Gate 3 is implemented, verified, and owner-accepted GO. The next approved implementation boundary is Gate 4's backend-neutral plugin integration; active-vault lifecycle/measurement, packaging acceptance, and a delivered-profile claim remain later gates.

See the historical Tantivy evidence in [`bench/tantivy-wasm/README.md`](bench/tantivy-wasm/README.md) and the selected sequence in [`docs/design/obsidian-lite.md`](docs/design/obsidian-lite.md).

### D5C — relevance signals and configuration — Planned

- Recency with protection for authoritative older notes.
- Typed Obsidian properties/frontmatter and bounded per-field relevance.
- Folder hierarchy, depth, ancestor, archive, and authority signals.
- Versioned named relevance profiles before arbitrary user-controlled sliders.

### D5D — daily-drive acceptance and distribution — Planned

- Real-vault acceptance, transparent mode/evidence UX, and richer keyboard/open/insert actions.
- BRAT/release hardening, installer/upgrade behavior, and eventual community-distribution readiness.

See [`docs/roadmap/desktop-obsidian.md`](docs/roadmap/desktop-obsidian.md), [`docs/design/obsidian-lite.md`](docs/design/obsidian-lite.md), and [`clients/obsidian/README.md`](clients/obsidian/README.md).

D5B is independent of IG-1 acceptance. Gate 1 and owner-reviewed installed Obsidian/frozen-BRAT Gate 2 are GO; production Obsidian/core work now proceeds through the portable-Rust, integration, lifecycle, and acceptance gates. IG-2 remains the next enterprise implementation lane after IG-1 acceptance; MCP, connectors, and structural-agent work are tabled as implementation, not canceled.

## Identity-governed enterprise retrieval

### IG-0 — constitutional authorization boundary — Contractual

- Authorization precedes retrieval.
- OpenClast is the identity and entitlement authority.
- Kwiry is a deny-by-default retrieval enforcement point.
- Exact `{ tenant_id, vault_id, room_id }` resources replace independent vault/room sets.
- Desktop and OpenClast profiles never authenticate or fall back into one another.

The binding rules are in [`CONTRACT.md`](CONTRACT.md).

### IG-1 — governed lexical OpenClast gateway — Kwiry enforcement implemented; companion integration pending

- The committed Kwiry checkpoint verifies short-lived lexical-only capabilities, opens only request-authorized resource partitions, and computes BM25 statistics only from those partitions.
- The companion OpenClast gateway must resolve live grants on every request, mint capabilities with a separate search key, and keep the internal capability and Kwiry address server-side.
- Full IG-1 implementation and owner review remain pending until the companion OpenClast changes and real two-user/two-room witness are committed and verified together.

See [`docs/openclast-ig1.md`](docs/openclast-ig1.md).

### IG-2 — governed semantic and hybrid retrieval — Planned next

- Authorized vector candidate generation through physical partitions or an equivalently strong mandatory rowset boundary.
- Authorized-only semantic ranking and hybrid fusion.
- Physical-baseline tests showing forbidden vectors cannot change authorized candidates or ordering.
- Policy-consistent hydration and explicit handling of authorization changes during a request.
- User-visible evidence explaining lexical, semantic, and fused contributions without exposing unauthorized corpus information.

The local semantic implementation is not reused in the OpenClast profile until this proof exists because its current whole-corpus candidate generation is suitable only for the desktop trust boundary. See [`docs/roadmap/governed-semantic-hybrid.md`](docs/roadmap/governed-semantic-hybrid.md).

### IG-3A — read-only governed MCP — Planned closely after IG-2

- `search`, scoped evidence/chunk retrieval, and scoped index status.
- Shared project-owned request models and enforcement handlers with HTTP.
- OpenClast-mediated identity and delegation for remote enterprise agents.
- No independent MCP policy engine and no administrative or note-writing tools.

This gate may be delivered in small owner-reviewable slices, but no slice may bypass the authorization-before-retrieval invariant. See [`docs/roadmap/governed-mcp-evidence.md`](docs/roadmap/governed-mcp-evidence.md).

### IG-3B — evidence and scoped retrieval surfaces — Planned

- Stable retrieval evidence envelope and truthful effective-mode reporting.
- Scoped chunk access and status aggregates.
- Provenance, source links, partial-failure semantics, and hydration rechecks.
- Cursor behavior only after deterministic multi-leg semantics are specified.

### IG-4 — lifecycle and operations — Planned

- Policy and subject revisions/epochs.
- Revocation behavior and race handling.
- Key rotation, cache isolation, quotas, and security status.
- Durable privacy-minimized audit and operational recovery.

### IG-5 — connector governance — Research/planned

- Deterministic file materialization and versioned provenance/ACL sidecars.
- Delta synchronization and replay-safe recovery.
- ACL normalization into exact governed resources.
- Priority permission revocation and one deep connector with real inherited-ACL behavior.

Connectors remain external materializers; Kwiry remains a retrieval engine over their files. See [`docs/roadmap/connectors.md`](docs/roadmap/connectors.md).

### IG-6 — later agent and structural expansion — Research

- Remote delegation and proof-bound workloads.
- External IGA/SCIM and access-review seams where justified.
- Authorized structural traversal derived from authored relationships.
- Broader agent interoperability without making generated graphs or memories canonical.

See [`docs/roadmap/agents-and-structural-retrieval.md`](docs/roadmap/agents-and-structural-retrieval.md).

## Cross-cutting quality tracks

These tracks continue across the numbered gates:

- **Retrieval quality:** aliases, technical identifiers, explanation, grouping/diversification, recency/authority signals, and evaluation fixtures.
- **User experience:** stupid-easy setup, transparent mode behavior, strong keyboard workflows, installability, and recovery.
- **Knowledge operations:** current-state recovery, canonical-source routing, stable question/decision records, evidence-backed checkpoints, and bounded curation workflows.
- **Privacy and security:** no credential persistence in clients, no secret-bearing logs, explicit trust profiles, and privacy scans before publication.
- **Rebuildability:** source files remain authoritative and every derived projection remains disposable.

## Current implementation and review boundaries

The D5B Tantivy-WASM checkpoint completed as a technical **NO-GO** for the normal incremental writer. The owner then selected official FTS5-WASM: Gate 1 is verified GO, the owner accepted the isolated one-file installed Obsidian/frozen-BRAT Gate 2 as GO, and portable Rust Gate 3 is implemented, verified, and owner-accepted GO. The next approved boundary is Gate 4 backend-neutral plugin integration. This remains independent of the OpenClast profile.

The current enterprise owner checkpoint remains the **IG-1 foundation**. Acceptance means the enterprise lexical authorization model is suitable as the base for governed semantic/hybrid and read-only MCP work; it does **not** mean lexical-only enterprise search is the end state.

Before beginning the next governed enterprise gate, confirm:

1. the OpenClast/Kwiry authority split and exact resource model remain correct;
2. request-authorized partition opening and authorized-only BM25 satisfy the intended security oracle;
3. the separate search key and server-side capability boundary are operationally understandable;
4. the next enterprise delivery lane is governed semantic/hybrid, followed closely by read-only governed MCP.

D5B has passed the Gate 3 native/portable parity and regression boundary. Gate 3 GO authorizes the separately approved Gate 4 backend-neutral integration plan; it does not authorize active-vault indexing, production publication, Gate 5 work, or a delivered no-daemon profile. Stop again after Gate 4's integrated Worker, explicit backend/mode/status behavior, deterministic one-file artifact, daemon regressions, and disposable-vault UI-foundation review.
