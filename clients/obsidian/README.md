# Kwiry Search (Obsidian plugin)

The delivered beta searches through a local [kwiry](https://github.com/cybersader/kwiry) daemon: lexical (BM25), semantic (local embeddings), and hybrid (RRF) ranking over any registered tree, including trees outside this vault.

The presentation plugin does not own parsing, chunking, ranking, authorization, or index behavior. Today those capabilities live in the native daemon. D5B is evaluating a second explicit host for environments where Obsidian may run but a daemon may not: portable Kwiry Rust preprocessing/query planning plus official SQLite FTS5-WASM behind an application-owned worker.

## Current requirements

- A running kwiry daemon (`kwiry serve`, add `--semantic` for semantic/hybrid modes)
- Desktop only (the current profile reads the daemon's token file from disk)

## No-daemon profile status

The **In-plugin · Lexical** profile is contractual but not delivered in the current release. The Tantivy normal incremental writer reached a technical NO-GO, then the official SQLite FTS5-WASM runtime and one-file installed Obsidian/frozen-BRAT gates passed. Portable Rust Gate 3 and backend-neutral production integration Gate 4 are owner-accepted GO. Gate 4's explicit backend selection, credential hardening, portable Rust plus official SQLite in one Worker, fixed query binding, generation isolation, deterministic artifact, complete automated matrix, and installed disposable-vault UI-foundation witness all pass in the current Gate 4 baseline. The profile still reports `index_building` because active-vault lifecycle and delivered-profile acceptance remain Gate 5.

Planned first scope:

- current open vault and Markdown files only;
- in-memory disposable index rebuilt from source files;
- lexical mode only, with no semantic/hybrid fallback;
- explicit backend selection rather than automatic daemon failover;
- active-vault create/modify/delete/rename reconciliation;
- future, separately reviewed relevance phases for recency, properties, folder hierarchy, and configurable profiles.

See [`../../docs/design/obsidian-lite.md`](../../docs/design/obsidian-lite.md), [`../../docs/roadmap/desktop-obsidian.md`](../../docs/roadmap/desktop-obsidian.md), the Gate 1 [`../../bench/fts5-wasm/README.md`](../../bench/fts5-wasm/README.md) evidence, and the Gate 2 [`../../bench/fts5-wasm-obsidian-probe/README.md`](../../bench/fts5-wasm-obsidian-probe/README.md) automation and field record.

## Network and privacy disclosure

- In **Daemon** mode, the plugin communicates only with a configured literal loopback HTTP origin, using Obsidian's `requestUrl` for `POST /v0/search`, `GET /v0/status`, and `GET /v0/health`.
- In **In-plugin · Lexical** mode, the embedded Worker has no network, persistence, helper-Worker, daemon URL, or daemon-token capability.
- Daemon search queries are sent only to the selected local daemon. In-plugin queries remain inside the in-memory Worker.
- The daemon bearer token is read from a bounded regular non-symlink file immediately before each authenticated request and is never stored in plugin data, sent to the Worker, logged, or displayed.
- No telemetry of any kind.

## Install via BRAT

1. Install [BRAT](https://github.com/TfTHacker/obsidian42-brat) from Community plugins.
2. Run **BRAT: Plugins: Add a beta plugin for testing (with or without version)** and enter `cybersader/kwiry`.
3. Select the intended released version; no GitHub token is required for this public repository.

BRAT installs `main.js`, `manifest.json`, and `styles.css` from each versioned GitHub release. Current `cybersader/kwiry` releases contain the daemon-backed production plugin. D5B's Worker/SQLite compatibility was tested through the isolated public `cybersader/kwiry-fts5-wasm-probe` repository and does not make in-plugin search available in the production plugin yet.

## Setup

1. Start the daemon; it prints its bearer-token file path.
2. In **Settings → Kwiry Search**, select **Daemon**, set the loopback URL and token file path, and map the current Obsidian vault to its daemon vault ID for local open actions.
3. Run the **Kwiry Search: Search notes** command (or the ribbon icon).
4. `Tab` cycles only modes supported by the selected backend; `Enter` opens, and `Ctrl+Enter` opens in a new tab.

Daemon results from a different registered tree remain searchable but show a factual notice instead of opening in this vault. Selecting **In-plugin · Lexical** is explicit and never reads the daemon token; at the owner-accepted Gate 4 boundary it truthfully remains `index_building` until Gate 5 supplies active-vault indexing.

## Development

Prerequisites include Rust 1.95.0, `wasm32-unknown-unknown`, `wasm-bindgen-cli` 0.2.126, and Node 22 or 24.

```bash
npm install
npm run dev        # development two-WASM build
npm test           # unit, real-FTS5, and exact generated-Worker tests
npm run build      # typecheck + deterministic production main.js
npm run evidence   # sanitized READY_FOR_OWNER_REVIEW evidence
npm run test-vault -- /absolute/path/to/empty-vault
npm run verify     # complete plugin verification sequence
```

Copy or symlink `main.js`, `manifest.json`, and `styles.css` into `<vault>/.obsidian/plugins/kwiry-search/`, then enable the plugin. The [Hot-Reload plugin](https://github.com/pjeby/hot-reload) makes iteration painless.

## License

GPL-3.0-only — see [LICENSE](LICENSE). Bundled Rust, wasm-bindgen, SQLite wrapper/core, and Omnisearch provenance are recorded in [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md), with the Apache 2.0 text under [`licenses/`](licenses/). The kwiry daemon remains a separate program reached only over HTTP and carries its own license.
