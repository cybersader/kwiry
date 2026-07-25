# Kwiry Search (Obsidian plugin)

The delivered beta searches through a local [kwiry](https://github.com/cybersader/kwiry) daemon: lexical (BM25), semantic (local embeddings), and hybrid (RRF) ranking over any registered tree, including trees outside this vault.

The presentation plugin does not own parsing, chunking, ranking, authorization, or index behavior. The released beta uses the native daemon. The current D5B candidate also implements a second explicit host for environments where Obsidian may run but a daemon may not: portable Kwiry Rust preprocessing/query planning plus official SQLite FTS5-WASM behind an application-owned worker. That candidate remains awaiting Gate 5 field acceptance and is not yet a delivered profile.

## Current requirements

- **Daemon** profile: a running kwiry daemon (`kwiry serve`, add `--semantic` for semantic/hybrid modes).
- **In-plugin · Lexical** candidate: desktop Obsidian; no daemon or token file.

## No-daemon profile status

The **In-plugin · Lexical** profile is contractual but not delivered in the current release. The Tantivy normal incremental writer reached a technical NO-GO, then the official SQLite FTS5-WASM runtime and one-file installed Obsidian/frozen-BRAT gates passed. Portable Rust Gate 3 and backend-neutral production integration Gate 4 are owner-accepted GO. The current Gate 5 candidate now implements active-vault snapshotting, atomic publication, live create/modify/delete/rename reconciliation, progress, manual rebuild, and bounded Worker recovery. Its expanded automated matrix passes. The generated Node Worker capture meets the provisional build, warm-search, update-visibility, and event-loop targets, but misses the provisional added-memory target; installed startup/progress and declared-reference-hardware measurements remain unavailable. Installed Obsidian, BRAT upgrade/rollback, private aggregate-only evidence, and explicit owner acceptance remain required before a delivered claim.

Candidate scope:

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

Daemon results from a different registered tree remain searchable but show a factual notice instead of opening in this vault. Selecting **In-plugin · Lexical** is explicit and never reads the daemon token. In the current Gate 5 candidate it builds and reconciles the active Markdown vault in memory; the released profile status remains unchanged until field evidence and owner acceptance authorize delivery.

## Development

Prerequisites include Rust 1.95.0, `wasm32-unknown-unknown`, `wasm-bindgen-cli` 0.2.126, and Node 22 or 24.

```bash
npm install
npm run dev        # development two-WASM build
npm test           # unit, real-FTS5, and exact generated-Worker tests
npm run build      # typecheck + deterministic production main.js
npm run evidence          # strict aggregate Gate 5 automation evidence
npm run corpus:smoke       # deterministic moderate generated-corpus smoke
npm run performance:gate5  # full 10k/~50 MiB generated Node-Worker capture
npm run test-vault -- /absolute/path/to/empty-vault [--profile functional|performance]
npm run test-vault:smoke    # functional package install/refusal/hash smoke
npm run verify              # complete non-field plugin verification sequence
```

Copy or symlink `main.js`, `manifest.json`, and `styles.css` into `<vault>/.obsidian/plugins/kwiry-search/`, then enable the plugin. The [Hot-Reload plugin](https://github.com/pjeby/hot-reload) makes iteration painless.

## License

GPL-3.0-only — see [LICENSE](LICENSE). Bundled Rust, wasm-bindgen, SQLite wrapper/core, and Omnisearch provenance are recorded in [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md), with the Apache 2.0 text under [`licenses/`](licenses/). The kwiry daemon remains a separate program reached only over HTTP and carries its own license.
