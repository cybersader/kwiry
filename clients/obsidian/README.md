# Kwiry Search (Obsidian plugin)

Kwiry Search publishes two explicit profiles. **Daemon** provides lexical (BM25), semantic (local embeddings), and hybrid (RRF) retrieval over registered trees. **In-plugin · Lexical** performs active-vault lexical retrieval inside desktop Obsidian through portable Kwiry Rust plus official SQLite FTS5-WASM, without a daemon or token file. The profiles never silently fall back into one another.

The presentation plugin does not own parsing, chunking, ranking, authorization, or index behavior. Project-owned Rust and engine adapters retain those responsibilities in both profiles.

## Current requirements

- **Daemon** profile: a running kwiry daemon (`kwiry serve`, add `--semantic` for semantic/hybrid modes).
- **In-plugin · Lexical:** desktop Obsidian; no daemon or token file; supported multi-format extraction, with HTML on by default and PDF and Excel off by default.

## In-plugin profile status

**In-plugin · Lexical** provides active-vault indexing, atomic publication, live create/modify/delete/rename reconciliation, progress, manual rebuild, bounded Worker recovery, and a validated disposable machine-local warm-start cache. Generated evidence meets the provisional build, warm-search, update-visibility, and event-loop targets but misses the provisional added-memory target; declared-reference-hardware measurement, private aggregate-only evidence, installed long-running quality, and explicit owner field acceptance remain pending. Passing automation and the narrow real-Obsidian WebDriver result-click proof are not owner acceptance.

Published scope:

- current open vault using Markdown, plain text, Base, Canvas, DOCX, Excalidraw, PDF, Excel, and standalone UTF-8 HTML (`.html`/`.htm`); HTML is on by default, while PDF and Excel are off by default;
- HTML canonical titles use the normal title/display lane without becoming authored properties or body text; latent descriptions, chrome, and hidden text remain searchable, with no URL dereference, embedded-resource read, locator, or section-link navigation;
- one active FTS5 generation, optionally accelerated by a validated disposable cache outside the vault on machine-local storage;
- lexical mode only, with no semantic/hybrid fallback;
- explicit backend selection rather than automatic daemon failover;
- active-vault create/modify/delete/rename reconciliation;
- explicit `strict_hash` or metadata-audit freshness behavior with stale/reconciling disclosure;
- published recursive typed property projection as disposable derived state, not used for lexical eligibility, scoring, or ranking;
- separately reviewed future relevance phases for recency, property ranking, folder hierarchy, and named profiles.

See [`../../docs/design/obsidian-lite.md`](../../docs/design/obsidian-lite.md), [`../../docs/roadmap/desktop-obsidian.md`](../../docs/roadmap/desktop-obsidian.md), the Gate 1 [`../../bench/fts5-wasm/README.md`](../../bench/fts5-wasm/README.md) evidence, and the Gate 2 [`../../bench/fts5-wasm-obsidian-probe/README.md`](../../bench/fts5-wasm-obsidian-probe/README.md) automation and field record.

## Network and privacy disclosure

- In **Daemon** mode, the plugin communicates only with a configured literal loopback HTTP origin, using Obsidian's `requestUrl` for `POST /v0/search`, `GET /v0/status`, and `GET /v0/health`.
- In **In-plugin · Lexical** mode, the embedded Worker has no network, filesystem/OPFS persistence, helper-Worker, daemon URL, or daemon-token capability.
- A main-thread machine-local cache port outside the vault owns the published disposable warm-start cache; the Worker receives/returns only bounded validated generation bytes.
- Daemon search queries are sent only to the selected local daemon. In-plugin queries remain inside the Worker and are never written to the cache.
- The daemon bearer token is read from a bounded regular non-symlink file immediately before each authenticated request and is never stored in plugin data, sent to the Worker, logged, or displayed.
- No telemetry of any kind.

## Install via BRAT

1. Install [BRAT](https://github.com/TfTHacker/obsidian42-brat) from Community plugins.
2. Run **BRAT: Plugins: Add a beta plugin for testing (with or without version)** and enter `cybersader/kwiry`.
3. Select the intended released version; no GitHub token is required for this public repository.

BRAT installs `main.js`, `manifest.json`, and `styles.css` from each versioned GitHub release. Beta.15 contains both explicit Daemon and In-plugin · Lexical profiles in the production plugin. The earlier isolated `cybersader/kwiry-fts5-wasm-probe` remains historical compatibility evidence; it is not the production host and was not itself owner acceptance of the published profile.

## Setup

1. In **Settings → Kwiry Search**, choose **In-plugin · Lexical** or **Daemon** explicitly.
2. For In-plugin · Lexical, configure any desired supported extractors. HTML is enabled by default; PDF and Excel are disabled by default. No daemon or token is required.
3. For Daemon, start `kwiry serve`, configure the literal-loopback URL and token path, and map the current vault for local open actions.
4. Run **Kwiry Search: Search notes**. `Tab` cycles only modes supported by the selected profile. `Enter` opens a grouped source generally; `Ctrl+L` toggles between Sources and that source's already returned sections, where `Enter` opens the exact selected heading, view, or PDF page. `Ctrl+H` remains a compatibility return to Sources, and `Ctrl+Enter` opens the selected target in a new tab.

In-plugin · Lexical builds, restores, and reconciles the current vault locally. Daemon results may include registered trees outside the current vault; those results remain searchable but show a factual notice instead of opening locally.

## Development

Prerequisites include Rust 1.95.0, `wasm32-unknown-unknown`, `wasm-bindgen-cli` 0.2.126, and Node 22 or 24.

```bash
npm install
npm run dev        # development two-WASM build
npm test           # ordinary tests in parallel; WASM-heavy files in bounded groups
npm run build      # typecheck + deterministic production main.js
npm run evidence          # strict aggregate Gate 5 automation evidence
npm run corpus:smoke       # deterministic moderate generated-corpus smoke
npm run performance:gate5  # full 10k/~50 MiB generated Node-Worker capture
npm run test-vault -- /absolute/path/to/empty-vault [--profile functional|performance]
npm run test-vault:smoke    # functional package install/refusal/hash smoke
npm run verify              # complete non-field plugin verification sequence
```

`npm test` keeps ordinary test files at Vitest's normal parallelism, runs lexical conformance alone, and limits the six tests that build Rust adapters to two workers. This prevents independent test forks from launching enough concurrent `wasm-bindgen` processes to monopolize a development workstation while preserving fast execution for lightweight tests. `npm run test:d5c` uses the same two-worker ceiling for its build-heavy path.

Copy or symlink `main.js`, `manifest.json`, and `styles.css` into `<vault>/.obsidian/plugins/kwiry-search/`, then enable the plugin. The [Hot-Reload plugin](https://github.com/pjeby/hot-reload) makes iteration painless.

## License

GPL-3.0-only — see [LICENSE](LICENSE). Bundled Rust, wasm-bindgen, SQLite wrapper/core, and Omnisearch provenance are recorded in [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md), with the Apache 2.0 text under [`licenses/`](licenses/). The kwiry daemon remains a separate program reached only over HTTP and carries its own license.
