# Kwiry Search (Obsidian plugin)

Kwiry Search has three backend profiles. **Daemon** provides lexical (BM25), semantic (local embeddings), and hybrid (RRF) retrieval over registered trees. **In-plugin · Lexical** performs active-vault lexical retrieval inside desktop Obsidian through portable Kwiry Rust plus official SQLite FTS5-WASM, without a daemon or token file. The profiles never silently fall back into one another.

The presentation plugin does not own parsing, chunking, ranking, authorization, or index behavior. Project-owned Rust and engine adapters retain those responsibilities in the desktop profiles. **Host search · Lexical** is provisional and delegates them to the host.

## Current requirements

- **Daemon** profile: a running kwiry daemon (`kwiry serve`, add `--semantic` for semantic/hybrid modes).
- **In-plugin · Lexical:** desktop Obsidian; no daemon or token file; supported multi-format extraction, with HTML on by default and PDF and Excel off by default.

## Host search (provisional)

A fresh install selects **Host search · Lexical** only when the host exposes a compatible v1 search capability through its host services slot. The slot may be installed before plugins load or provided later by another plugin, so a fresh install that finds no capability at load keeps checking for up to five seconds after the workspace layout is ready. During that wait the status bar shows “starting…”, nothing is indexed or read, and the settings tab shows no controls, so the provisional fallback cannot be saved. If the capability appears, the plugin selects host search; otherwise fresh installs still select **Daemon** where supported and **In-plugin · Lexical** elsewhere, and do not check again. The plugin saves only that first host choice, without adding other settings; a fallback chosen because the capability was absent is never saved. Stored selections (including older settings) are not migrated to host search. A selected host profile without a compatible capability stays unavailable; it never switches backends automatically.

Use **Kwiry Search: Search notes**, submit terms explicitly, and choose **Next page** when offered. The modal shows one host-supplied group at a time. If the host denies access, all open host search modals clear their searches and results. The plugin neither builds nor restores an index for this profile and does not keep a query or result history. Host search is modal-only: the cross-plugin API's `ready()` and valid `search()` calls reject with `backend_unavailable` while this profile is selected.

## In-plugin profile status

**In-plugin · Lexical** provides active-vault indexing, atomic publication, live create/modify/delete/rename reconciliation, progress, manual rebuild, bounded Worker recovery, and a validated disposable machine-local warm-start cache. Generated evidence meets the provisional build, warm-search, update-visibility, and event-loop targets but misses the provisional added-memory target; declared-reference-hardware measurement, private aggregate-only evidence, installed long-running quality, and explicit owner field acceptance remain pending. Passing automation and the narrow real-Obsidian WebDriver result-click proof are not owner acceptance.

Published scope:

- current open vault using Markdown, plain text, Base, Canvas, DOCX, Excalidraw, PDF, Excel, and standalone UTF-8 HTML (`.html`/`.htm`); HTML is on by default, while PDF and Excel are off by default;
- HTML canonical titles use the normal title/display lane without becoming authored properties or body text; latent descriptions, chrome, and hidden text remain searchable, with no URL dereference, embedded-resource read, locator, or section-link navigation;
- one active FTS5 generation, optionally accelerated by a validated disposable cache outside the vault on machine-local storage;
- lexical mode only, with no semantic/hybrid fallback;
- one shared-Rust-owned bounded exploratory partial pass for fully supported ordinary queries with at least two useful optional terms, activated only when prior authorized standard lanes reach fewer than 20 distinct sources; required identifier anchors remain mandatory, optional terms use the same bounded prefix expansions as standard lanes when available (otherwise their analyzed exact form), at least one optional alternative must match, and partial or mixed result quality is disclosed in the search status;
- a 100 ms trailing-edge typeahead scheduler with at most one active backend request and one replaceable latest pending request, preventing obsolete synchronous Worker searches from accumulating;
- retrieval stops after the fixed 512-unique-candidate window fills, and when the fixed 100-section source-discovery response window comes back full, the query-free search status discloses that additional sources may be unobserved beyond that window, independent of both the source-row limit's exact omission count and candidate-window completeness;
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
- Daemon search queries are sent only to the selected local daemon. In-plugin queries remain inside the Worker and are never written to the cache. Host search queries go only to the host capability; the plugin does not record or export them, result content, or continuation tokens.
- The daemon bearer token is read from a bounded regular non-symlink file immediately before each authenticated request and is never stored in plugin data, sent to the Worker, logged, or displayed.
- No telemetry of any kind.

## Install via BRAT

1. Install [BRAT](https://github.com/TfTHacker/obsidian42-brat) from Community plugins.
2. Run **BRAT: Plugins: Add a beta plugin for testing (with or without version)** and enter `cybersader/kwiry`.
3. Select the intended released version; no GitHub token is required for this public repository.

BRAT installs `main.js`, `manifest.json`, and `styles.css` from each versioned GitHub release. Beta.15 contains both explicit Daemon and In-plugin · Lexical profiles in the production plugin. The earlier isolated `cybersader/kwiry-fts5-wasm-probe` remains historical compatibility evidence; it is not the production host and was not itself owner acceptance of the published profile.

## Setup

1. In **Settings → Kwiry Search**, choose **In-plugin · Lexical**, **Daemon**, or **Host search · Lexical** (when offered).
2. For In-plugin · Lexical, configure any desired supported extractors. HTML is enabled by default; PDF and Excel are disabled by default. No daemon or token is required.
3. For Daemon, start `kwiry serve`, configure the literal-loopback URL and token path, and map the current vault for local open actions.
4. Run **Kwiry Search: Search notes**. In the Daemon and In-plugin profiles, `Tab` cycles only modes supported by the selected profile. `Enter` opens a grouped source generally; `Ctrl+L` toggles between Sources and that source's already returned sections, where `Enter` opens the exact selected heading, view, or PDF page. `Ctrl+H` remains a compatibility return to Sources, and `Ctrl+Enter` opens the selected target in a new tab.

File previews use Obsidian's core **Page preview** plugin. Enable it, then hold `Ctrl` (Windows/Linux) or `Cmd` (macOS) while hovering a source or section row; native Page preview preferences can override the modifier requirement for Kwiry Search. Both row types request the whole file, not a heading, PDF page, or sheet; supported preview formats depend on Obsidian.

In-plugin · Lexical builds, restores, and reconciles the current vault locally. Daemon results may include registered trees outside the current vault; those results remain searchable but show a factual notice instead of opening locally.

## Public search API for other plugins

Any Obsidian plugin can use `app.plugins.plugins["kwiry-search"]?.api`. Check
`api?.version === 1` before calling it, and catch rejected promises. The exported
TypeScript contract and `KwiryApiError` live in [`src/public-api.ts`](src/public-api.ts):

```ts
interface KwirySearchApiV1 {
  readonly version: 1;
  ready(): Promise<void>;
  search(query: string, opts?: { limit?: number }): Promise<
    Array<{ path: string; score: number; heading?: string }>
  >;
}

const api = app.plugins.plugins["kwiry-search"]?.api;
if (api?.version === 1) {
  try {
    await api.ready();
    const hits = await api.search("pear blossoms", { limit: 30 });
    // Consume hits in backend order; paths are relative to the current vault.
  } catch (error) {
    // Cross-plugin bundles should inspect error.name === "KwiryApiError"
    // and error.code rather than rely on instanceof across bundle boundaries.
  }
}
```

- **Versioning:** additive changes retain `version: 1`; breaking changes require a
  new API version. The API version is independent of the plugin release version.
- **Backend and mode:** uses the same backend manager as the search modal, without
  starting another backend or index. In-plugin uses lexical; Daemon uses the
  configured default search mode. Unsupported modes reject, never fall back.
- **Current-vault scope:** Daemon requires the existing current-vault mapping
  (`daemonCurrentVaultId`) and sends that vault ID as a search filter. The modal's
  optional registered-tree filter is not used: this API has no cross-vault result
  identity. Invalid or foreign-vault hits reject rather than masquerade as local
  paths. In-plugin already searches only the active vault.
- **Readiness:** `ready()` checks current availability, including mode and mapping.
  Before backend activation or while no searchable generation exists, it rejects;
  it does not poll or wait indefinitely for an index build. Retry after the backend
  becomes ready. A rebuild with an existing searchable generation is allowed.
  Readiness is a snapshot, so `search()` checks again and can still reject.
- **Inputs and limits:** queries must be non-empty strings (whitespace-only is
  invalid). Limit defaults to 20; finite numeric values are floored and clamped to
  **1–100**. Non-finite or non-numeric limits and malformed options reject.
- **Results:** section hits in engine order, without grouping, deduplication, or
  reranking; a file may appear more than once. Scores are backend-owned and are not
  comparable across modes/backends. `heading` is the final Markdown section
  heading when present. Each hit is frozen and the array is a fresh defensive copy.
  An empty array means a successful search with no matches, never a hidden error.
- **Read-only lifecycle:** no vault writes, settings changes, or credentials are
  exposed. Calls through retained API references reject after plugin unload;
  in-flight results are also refused if the plugin unloads or the backend changes.

Every rejection is a named `KwiryApiError` with a stable `code` and a safe message:

| Code | Meaning |
|---|---|
| `invalid_query` | Non-string/empty input, or the backend rejects query syntax. |
| `invalid_options` | Malformed options or non-finite/non-numeric limit. |
| `backend_unavailable` | No selected backend, unreachable daemon, or disposed/unavailable backend. |
| `index_not_ready` | No searchable index exists yet. |
| `mode_unavailable` | Configured mode is unsupported or could not be honored. |
| `vault_mapping_required` | Current vault is not mapped to the daemon. |
| `invalid_result` | Unsafe path, foreign/stale hit, or invalid score. |
| `backend_changed` | Backend changed during the call; retry against the new backend. |
| `plugin_unloaded` | Plugin unloaded; reacquire the API after enabling it again. |
| `search_failed` | Other backend failure; raw internal error details are not exposed. |

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
