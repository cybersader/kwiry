# Kwiry FTS5-WASM Obsidian compatibility probe

## Status

**Automated verdict: `READY_FOR_FIELD_TEST`; owner field verdict: Gate 2 GO.** The clean automated checkpoint passed 18 tests and 15 aggregate evidence checks, including 25 exact generated-Worker lifecycle cycles. The owner then accepted installed desktop Obsidian and frozen-BRAT compatibility after `0.0.1` installation and ten runs, update to `0.0.2`, full restart/rerun, rollback to `0.0.1`, and final rerun all passed. This isolated plugin exists only to test whether installed Obsidian Electron can run the exact official SQLite FTS5-WASM runtime from an application-owned Worker embedded inside one CommonJS `main.js`.

It is not a search plugin and does not read the vault. It does not alter the production `kwiry-search` plugin, daemon, Rust core, settings, or release stream. Frozen releases use the isolated public [`cybersader/kwiry-fts5-wasm-probe`](https://github.com/cybersader/kwiry-fts5-wasm-probe) repository.

The only command is:

> **Kwiry FTS5-WASM Compatibility Probe: Run embedded FTS5-WASM compatibility probe**

It runs fixed synthetic data through SQLite 3.53.0 and FTS5, checks a bound lexical query, BM25, inert snippet markers, transaction rollback, integrity, and explicit database close, then terminates the Worker and revokes its Blob URL.

## One-file architecture

The production build uses two esbuild passes:

1. `src/worker.ts` is bundled as a classic browser Worker IIFE with the official `sqlite3.wasm` loaded through esbuild's binary loader.
2. The complete Worker source is injected as a string into the outer Obsidian CommonJS build.
3. `src/main.ts` creates a local Blob URL only when the command runs.
4. The Worker initializes SQLite from embedded `wasmBinary` and opens only `:memory:`.
5. The final runtime has one generated JavaScript file: `main.js`.

There is no loose Worker, `.wasm`, source map, dynamic import, CDN, runtime fetch, OPFS database, or application data file.

The upstream Worker1/Promiser protocol is not used. The project-owned protocol exposes only `initialize`, `probe`, and `dispose`; it accepts no SQL, query, path, note text, or vault payload.

## Pinned artifact

| Item | Value |
|---|---|
| Package | `@sqlite.org/sqlite-wasm@3.53.0-build1` |
| SQLite | `3.53.0` |
| FTS5 | required |
| WASM bytes | `864752` |
| WASM SHA-256 | `02d7e48164395fa68f81c6ec33e9da5461be397dc57602ac0cd89b4bbba1d312` |
| Package-lock integrity | `sha512-PfWPWN2n+/37doa8oh2/oUXk4OOsRYZsxc1W1sDXIGb/Pu5Yrb+f2eyYpgQMGITVX7HVgxhs9P18Rc6I97ym/g==` |

Build and test operations fail if this identity changes.

## Automated verification

From the repository root:

```bash
npm ci --prefix bench/fts5-wasm-obsidian-probe
npm run typecheck --prefix bench/fts5-wasm-obsidian-probe
npm test --prefix bench/fts5-wasm-obsidian-probe
npm run build --prefix bench/fts5-wasm-obsidian-probe
npm run evidence --prefix bench/fts5-wasm-obsidian-probe
```

The suite verifies:

- strict protocol validation, response correlation, timeouts, Worker failures, and idempotent cleanup;
- exact browser SQLite wrapper selection and exclusion of Node/Worker1/OPFS helper artifacts;
- exactly one embedded official WASM payload;
- deterministic one-file CommonJS builds with the GPL banner and no release source map;
- exact generated Worker execution through Node `worker_threads`;
- zero runtime network, persistence, and helper-Worker attempts;
- synthetic FTS5 query, finite BM25 score, inert markers, rollback, integrity, and close;
- corrupt embedded-WASM rejection;
- serialized lifecycle requests and 25 fresh initialize/probe/dispose cycles;
- manifest/package/versions agreement and absence of production, vault, settings, token, filesystem, or network imports in first-party host code.

`npm run evidence` reruns the suite and emits sanitized JSON. Automation may report only `READY_FOR_FIELD_TEST` or `NO-GO`; it cannot report Gate 2 GO.

## Direct sideload field test

Use an empty disposable vault. Do not install this probe into a vault containing sensitive material merely for convenience.

1. Build the probe and record the SHA-256 of `main.js`, `manifest.json`, and `styles.css`.
2. Copy `main.js`, `manifest.json`, `styles.css`, `LICENSE`, and `THIRD_PARTY_NOTICES.md` into:

   ```text
   <disposable-vault>/.obsidian/plugins/kwiry-fts5-wasm-probe/
   ```

3. Fully restart Obsidian and enable **Kwiry FTS5-WASM Compatibility Probe**.
4. Open Developer Tools, clear Console and Network, and disconnect networking if practical.
5. Run the probe command.
6. Confirm the success notice reports the embedded Worker, SQLite 3.53.0, FTS5, fixed query, rollback, and clean close.
7. Confirm there is no HTTP(S), XHR, WebSocket, Worker-file, or `.wasm` request. A local `blob:` Worker is expected.
8. Confirm the plugin directory contains no loose Worker/WASM/map/database and no `data.json`.
9. Run the command ten times, invoke it twice rapidly, disable during a run, re-enable, rerun, and restart Obsidian.
10. Confirm there is no stale notice, unhandled rejection, orphan Worker, continuing CPU activity, vault change, settings change, or token access.
11. Record only sanitized environment details: OS/architecture, Obsidian/Electron/Chromium versions, artifact hashes, aggregate timings, and pass/fail stages. Do not record vault paths or content.

A direct-sideload pass establishes local Electron/CSP compatibility only. It does not establish BRAT compatibility.

## Frozen BRAT field record

The probe was not published through the production repository's ordinary release stream. The isolated public [`cybersader/kwiry-fts5-wasm-probe`](https://github.com/cybersader/kwiry-fts5-wasm-probe) repository carries the unique `kwiry-fts5-wasm-probe` ID and frozen `0.0.1` and `0.0.2` prereleases.

All six public assets for each release were independently downloaded without authentication and hash-verified. Both releases use the exact clean-CI `main.js`; `0.0.2` changes only the manifest version and checksum file. Under desktop Obsidian, frozen `0.0.1` installation and ten runs, update to `0.0.2`, full restart/rerun, rollback to `0.0.1`, and final rerun passed. BRAT's add-plugin modal remained open after successful installation; the owner accepted that as a non-blocking upstream completion-UI defect. Exact release hashes, matching manifest versions, public delivery, and execution were established, while a separate filesystem hash of the installed copies was not reported.

Gate 2 GO authorizes portable Rust Gate 3. It does not make the production in-plugin lexical host delivered.

## Privacy and security claims

The defensible claim is that this probe's source and instrumented execution do not use vault, settings, token, persistence, or network APIs. Obsidian plugins are not permission-sandboxed, so this probe must not claim that the host platform makes such access impossible.

The probe:

- uses fixed synthetic strings only;
- registers no settings tab, ribbon, vault event, or background poll;
- calls neither `loadData` nor `saveData`;
- does not import the production daemon client;
- returns fixed status text and stable error codes;
- does not reflect SQL, stack traces, paths, or environment values into Notices.

## Scope exclusions

This checkpoint does not implement:

- production in-plugin search;
- portable Rust preparation or query planning;
- active-vault discovery or reconciliation;
- persistence;
- recency, properties, or folder hierarchy relevance;
- semantic or hybrid search;
- publication through the production `cybersader/kwiry` release or BRAT channel;
- a delivered no-daemon profile.
