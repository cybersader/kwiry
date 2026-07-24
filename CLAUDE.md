# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

Machine-specific paths and workspace tooling live in `CLAUDE.local.md` (untracked); this file stays free of private information and absolute local paths.

## Search safety

Do not invoke `ck`, the ck-search MCP server, or any `cks*` helper. Direct ck indexing repeatedly consumed 10–17 GiB RSS, exhausted swap, and froze WSL. Also do not run recursive `find`, `grep -R`, or equivalent scans over all of `~/`, Windows `Documents`, or `~/.claude`; scope every search to this repository or another explicitly named narrow path.

## Commands

Run Rust commands from `daemon/`:

```bash
cargo build --workspace
cargo run -p kwiry -- --help
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace
cargo test -p kwiry-core chunk::tests::splits_oversized_sections_with_overlap
cargo test -p kwiry --test cli lifecycle_indexes_and_searches_fixture
cargo test -p kwiry --test daemon daemon_watches_files_reloads_config_and_reconciles_offline_changes
```

CLI and daemon lifecycle:

```bash
cargo run -p kwiry -- --config /tmp/kwiry-config.toml --data-dir /tmp/kwiry-data vault add --id notes --path /absolute/path/to/tree
cargo run -p kwiry -- --config /tmp/kwiry-config.toml --data-dir /tmp/kwiry-data index
cargo run -p kwiry -- --config /tmp/kwiry-config.toml --data-dir /tmp/kwiry-data search "query"
cargo run -p kwiry -- --config /tmp/kwiry-config.toml --data-dir /tmp/kwiry-data serve --bind 127.0.0.1:32189
cargo run -p kwiry -- --config /tmp/kwiry-config.toml --data-dir /tmp/kwiry-data serve --bind 127.0.0.1:32189 --semantic
```

HTTP callers must read the configured token file and send an explicit `mode` to `POST /v0/search`. Without `--semantic`, only `mode: "lexical"` is served; with it, `semantic` and `hybrid` (RRF) are also available. See `docs/vertical-2.md` and `docs/vertical-3.md`.

## Architecture

`daemon/` is a Rust workspace with one shipped executable:

- `kwiry-core` owns durable registration config, deterministic discovery, Markdown/frontmatter/wikilink parsing, section chunking, generation/manifest/reconciliation state, project-owned request/result models, the Tantivy adapter, and the semantic layer (fastembed embedder behind the `semantic-onnx` feature, sqlite-vec store, RRF fusion). Engine types — Tantivy and rusqlite/sqlite-vec alike — stay inside their adapters.
- `kwiry` composes the CLI, Tokio/Axum host, authentication, watcher, and lifecycle around core APIs. Ranking, parsing, and index logic must not move into clients or HTTP handlers.
- `fixtures/vault/` drives rebuild and lexical-result tests. Files are the sole content source of truth; all index data is disposable.

The dependency flow is CLI → config/registration → walker → parser/chunker → Tantivy adapter → project-owned search results. Tantivy types stay inside the adapter so later HTTP, MCP, and WASM hosts can reuse the core without exposing the engine.

## Binding constraints

`CONTRACT.md` is frozen except for owner-approved amendments. `ROADMAP.md` owns current sequencing and named review gates; `HANDOFF.md` is historical origin/provenance, not current implementation truth. Stop for owner review at every roadmap boundary. A capability existing in the desktop profile does not imply that its OpenClast authorization proof is complete. Semantic failures must never degrade lexical search, and unavailable modes return an explicit `mode_unavailable` — no silent fallback.

Chunk IDs are deterministic and path-derived. A rename is an atomic orphan-free removal/reinsert, so IDs change when the path changes.

Do not add AI attribution, generated-by text, or AI co-author trailers to commits.

## Knowledge operations

The knowledge-ops layer (`knowledge-base/`, `.claude/`, `tests/`) is local, untracked working infrastructure — present on the development machine but never published.

`knowledge-base/` is durable project memory in five maturity zones (`00-inbox` → `01-working` → `02-learnings` → `03-reference` → `04-archive`). Begin recovery at `knowledge-base/03-reference/recovery-index.md`; use `current-project-state.md` for volatile status and `question-decision-register.md` for stable owner questions and decisions. Capture is mandatory for owner direction, contract amendments, review checkpoints, non-obvious verified behavior, reusable incidents/corrections, research outliving the session, and disproved assumptions. Skip formatting, lint, mechanical cleanup, resultless exploration, and facts a canonical source already fully owns.

- Use the `kwiry-knowledge-curator` agent for routine single-note capture; use the `knowledge-capture` workflow (three static agents, ≤12 validated repo-relative paths) for multi-source synthesis, question/decision review, and checkpoints.
- Link to canonical sources and tests instead of copying them; `CONTRACT.md` is never edited through curation.
- Never infer owner approval from implementation, passing tests, silence, or research recommendations. Bubble contract, security, public-behavior, roadmap, acceptance, and cross-repository authority questions through the register.
- Never capture conversation transcripts, secrets, tokens, temp paths, logs, or generated indexes; the ck/broad-scan prohibition applies to curation too.
- Promotion, archival, supersession, and owner-boundary resolution are reviewed. Hooks (`.claude/hooks/knowledge.py`, wired in `.claude/settings.json`) remind and validate but never change authority. Run `python3 .claude/hooks/knowledge.py validate --repo-root .`, `python3 .claude/hooks/knowledge.py health --repo-root .`, and `python3 -m unittest tests.test_knowledge_ops` to verify the layer.
