# Official SQLite FTS5-WASM feasibility probe

## Gate 1 verdict

**GO for the standalone runtime only.** The exact official `@sqlite.org/sqlite-wasm` package initializes an in-memory SQLite database under Node, includes FTS5, and passes the required schema, query, ranking, excerpt, mutation, rollback, integrity, and lifecycle tests without a patch, custom SQLite build, persistence layer, network dependency, or production integration.

This does **not** prove or authorize the no-daemon Obsidian profile. Obsidian Electron/CSP behavior, one-file Worker/WASM packaging, BRAT installation, portable Rust extraction, active-vault indexing, performance, and production integration remain separate gates.

## Pinned runtime

| Item | Verified value |
|---|---|
| npm package | `@sqlite.org/sqlite-wasm@3.53.0-build1` |
| package wrapper license | Apache-2.0 |
| SQLite runtime | `3.53.0` |
| `ENABLE_FTS5` | `1` |
| official `sqlite3.wasm` size | `864752` bytes |
| official `sqlite3.wasm` SHA-256 | `02d7e48164395fa68f81c6ec33e9da5461be397dc57602ac0cd89b4bbba1d312` |
| package-lock integrity | `sha512-PfWPWN2n+/37doa8oh2/oUXk4OOsRYZsxc1W1sDXIGb/Pu5Yrb+f2eyYpgQMGITVX7HVgxhs9P18Rc6I97ym/g==` |
| test framework | `vitest@3.2.7` |

If the package version, lock integrity, WASM byte length, or SHA-256 changes, the probe fails rather than silently accepting a different artifact.

## What the probe implements

The relational `chunks` table is the hydration and source-replacement owner. Its metadata includes source/chunk/vault/path identity, headings, frontmatter, mtime, content hash, chunking version, and the fixed lexical fields. An external-content `chunks_fts` table is synchronized only through canonical insert, delete, and update triggers.

The fixed `lexical-v1` field weights are:

| FTS5 field | Weight |
|---|---:|
| filename | 5.0 |
| stem | 6.0 |
| aliases | 6.0 |
| title | 6.0 |
| heading text | 3.0 |
| path text | 1.0 |
| tags | 2.0 |
| content | 1.0 |
| identifiers | 5.0 |

The query uses standard FTS5 `bm25()` and negates its lower-is-better cost for Kwiry's higher-is-better score convention. Query text and limits are bound parameters. Equal scores use `chunk_id ASC`, then path, as deterministic tie-breakers. These weights are a fixed engine baseline, not the future configurable relevance system.

Snippets and highlights use inert private-use sentinel markers rather than HTML. The helper rejects reserved input markers and malformed or nested output markers. Markup-looking note text remains text; no result is rendered through `innerHTML` here.

## Test coverage

The real SQLite WASM tests cover:

- exact package version, lock integrity, WASM size, and SHA-256;
- SQLite version, FTS5 compile option, OO1 `:memory:` initialization, schema, triggers, integrity, close, and explicit use-after-close failure;
- ordinary terms, quoted phrases, prefixes, Boolean `AND`/`OR`/`NOT`, and allowlisted column syntax;
- explicit malformed-query errors and proof that bound query text cannot execute additional SQL;
- weighted filename/title/alias evidence versus body-only evidence;
- finite scores, repeated deterministic ordering, and equal-score tie-breaking;
- `snippet()` and `highlight()` with inert markers and markup-looking source text;
- insert/upsert, complete source replacement, delete, path-derived rename, repeated idempotent replacement, injected rollback, and absence of partially replaced searchable state;
- repeated create/query/close cycles with an in-memory database only.

Expected malformed-query cases can cause the upstream SQLite wrapper to write diagnostic SQL to stderr while Vitest verifies the explicit error. They do not indicate a failed test or expose note content in this synthetic probe.

## Reproduce

From the repository root:

```bash
npm ci --prefix bench/fts5-wasm
npm test --prefix bench/fts5-wasm
npm run evidence --prefix bench/fts5-wasm
```

The evidence command reruns the Vitest suite and prints a machine-readable aggregate. The verified checkpoint recorded 18 of 18 tests passing and 14 of 14 aggregate evidence checks passing under Node `v24.12.0` on Linux x64.

## Gate boundary

A later Gate 2 may create a separate compatibility plugin that embeds an application-owned Worker and the official SQLite WASM bytes into one CommonJS `main.js`. It must not read the vault or alter production settings. Actual installed Obsidian Electron/CSP and frozen-release BRAT behavior require owner field testing.

Only after packaging proof may Gate 3 feature-gate portable Kwiry Rust preparation and query planning. Native desktop and OpenClast lexical retrieval remain on Tantivy. Cross-engine acceptance compares identical prepared chunks/metadata, deterministic behavior within each engine/profile/version, technical-identifier/filter behavior, and judged relevance floors—not raw score equality or exact total ordering.

Gate 1 intentionally excludes:

- Obsidian, Electron, CSP, and BRAT compatibility;
- Worker or one-file production packaging;
- OPFS or persistent state;
- portable Rust/core changes;
- production plugin integration;
- active-vault lifecycle and real-vault performance;
- semantic or hybrid search.
