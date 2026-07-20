# Kwiry Search (Obsidian plugin)

Search your notes through a local [kwiry](https://github.com/cybersader/kwiry) daemon: lexical (BM25), semantic (local embeddings), and hybrid (RRF) ranking, over any registered tree — including trees outside this vault.

This plugin is a deliberately **dumb client**: a query modal, a results renderer, and a status-bar light. All chunking, ranking, and index logic lives in the daemon.

## Requirements

- A running kwiry daemon (`kwiry serve`, add `--semantic` for semantic/hybrid modes)
- Desktop only (the plugin reads the daemon's token file from disk)

## Network and privacy disclosure

- The plugin communicates **only** with the daemon URL you configure (default `http://127.0.0.1:32189`), using Obsidian's `requestUrl`. It calls `POST /v0/search`, `GET /v0/status`, and `GET /v0/health`.
- Search queries are sent to that daemon and nowhere else. With a default localhost daemon, nothing leaves your machine.
- The daemon's bearer token is read on demand from the file path you configure and is **never** stored in plugin data, logged, or displayed.
- No telemetry of any kind.

## Install via BRAT

1. Install [BRAT](https://github.com/TfTHacker/obsidian42-brat) from Community plugins.
2. Run **BRAT: Add a beta plugin** and enter `cybersader/kwiry`.
3. While this repository is private, add a fine-grained GitHub personal access token (read-only Contents on this repo) in BRAT's settings first.

BRAT installs from GitHub release assets (`main.js`, `manifest.json`, `styles.css`), which the release workflow publishes on every version tag. Use **Add a beta plugin with frozen version** to pin a specific release.

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
