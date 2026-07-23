# Kwiry Search (Obsidian plugin)

The delivered beta searches through a local [kwiry](https://github.com/cybersader/kwiry) daemon: lexical (BM25), semantic (local embeddings), and hybrid (RRF) ranking over any registered tree, including trees outside this vault.

The presentation plugin does not own parsing, chunking, ranking, authorization, or index behavior. Today those capabilities live in the native daemon. D5B is evaluating a second explicit host for environments where Obsidian may run but a daemon may not: portable Kwiry Rust preprocessing/query planning plus official SQLite FTS5-WASM behind an application-owned worker.

## Current requirements

- A running kwiry daemon (`kwiry serve`, add `--semantic` for semantic/hybrid modes)
- Desktop only (the current profile reads the daemon's token file from disk)

## No-daemon profile status

The **In-plugin · Lexical** profile is contractual but not delivered in the current release. The Tantivy normal incremental writer reached a technical NO-GO, then the official SQLite FTS5-WASM runtime gate passed. A separate one-file CommonJS compatibility probe passed automated Worker, artifact, privacy, corruption, deterministic-build, and lifecycle checks; the owner then accepted installed desktop Obsidian/Electron and frozen BRAT install, update, restart, rollback, and rerun evidence as Gate 2 GO. Portable Rust extraction and production integration remain separate gates.

Planned first scope:

- current open vault and Markdown files only;
- in-memory disposable index rebuilt from source files;
- lexical mode only, with no semantic/hybrid fallback;
- explicit backend selection rather than automatic daemon failover;
- active-vault create/modify/delete/rename reconciliation;
- future, separately reviewed relevance phases for recency, properties, folder hierarchy, and configurable profiles.

See [`../../docs/design/obsidian-lite.md`](../../docs/design/obsidian-lite.md), [`../../docs/roadmap/desktop-obsidian.md`](../../docs/roadmap/desktop-obsidian.md), the Gate 1 [`../../bench/fts5-wasm/README.md`](../../bench/fts5-wasm/README.md) evidence, and the Gate 2 [`../../bench/fts5-wasm-obsidian-probe/README.md`](../../bench/fts5-wasm-obsidian-probe/README.md) automation and field record.

## Network and privacy disclosure

- The plugin communicates **only** with the daemon URL you configure (default `http://127.0.0.1:32189`), using Obsidian's `requestUrl`. It calls `POST /v0/search`, `GET /v0/status`, and `GET /v0/health`.
- Search queries are sent to that daemon and nowhere else. With a default localhost daemon, nothing leaves your machine.
- The daemon's bearer token is read on demand from the file path you configure and is **never** stored in plugin data, logged, or displayed.
- No telemetry of any kind.

## Install via BRAT

1. Install [BRAT](https://github.com/TfTHacker/obsidian42-brat) from Community plugins.
2. Run **BRAT: Plugins: Add a beta plugin for testing (with or without version)** and enter `cybersader/kwiry`.
3. Select the intended released version; no GitHub token is required for this public repository.

BRAT installs `main.js`, `manifest.json`, and `styles.css` from each versioned GitHub release. Current `cybersader/kwiry` releases contain the daemon-backed production plugin. D5B's Worker/SQLite compatibility was tested through the isolated public `cybersader/kwiry-fts5-wasm-probe` repository and does not make in-plugin search available in the production plugin yet.

## Setup

1. Start the daemon; it prints its bearer-token file path.
2. In **Settings → Kwiry Search**, set the daemon URL and that token file path.
3. Run the **Kwiry Search: Search notes** command (or the ribbon icon).
4. `Tab` cycles lexical → semantic → hybrid inside the modal; `Enter` opens, `Ctrl+Enter` opens in a new tab.

Results from registered trees that are not this vault show a notice instead of opening.

## Development

```bash
npm install
npm run dev        # esbuild watch
npm test           # vitest unit tests
npm run build      # typecheck + production main.js
```

Copy or symlink `main.js`, `manifest.json`, and `styles.css` into `<vault>/.obsidian/plugins/kwiry-search/`, then enable the plugin. The [Hot-Reload plugin](https://github.com/pjeby/hot-reload) makes iteration painless.

## License

GPL-3.0-only — see [LICENSE](LICENSE). This plugin's UX design is informed by, and portions may be adapted from, [Omnisearch](https://github.com/scambier/obsidian-omnisearch) by Simon Cambier and contributors (GPL-3.0); adapted files carry provenance headers naming the upstream revision and modification dates. The kwiry daemon is a separate program reached only over HTTP and carries its own license.
