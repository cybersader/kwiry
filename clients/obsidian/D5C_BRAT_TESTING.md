# D5C Text-versus-Balanced field test

This is a local active-vault search experiment, not a production Kwiry release. It does not connect to Google Docs or Canva, does not change the normal search profile, and does not make Balanced a public or default relevance policy.

The plugin uses the distinct ID `kwiry-d5c-balanced-playground`, so it can be installed beside `kwiry-search`. Its build contains one in-memory local Worker, disables the normal active-vault disk cache, and compiles out daemon, network, credential, settings-profile, and cache modules. It reads Markdown from the active disposable vault only while enabled. The index is discarded on unload and rebuilt after restart.

## Install through BRAT

1. Use a disposable Obsidian vault.
2. Install and enable BRAT.
3. Run **BRAT: Plugins: Add a beta plugin for testing (with or without version)**.
4. Enter `cybersader/kwiry-d5c-balanced-playground`.
5. Select the owner-authorized `0.0.2` test release.
6. Confirm Obsidian lists **Kwiry D5C Balanced Playground**, not the normal **Kwiry Search** plugin.
7. Run **Kwiry D5C Balanced Playground: Open Text vs Balanced search**.

The installed plugin directory must contain exactly `main.js`, `manifest.json`, and `styles.css`. Compare their SHA-256 values with the release attestation before testing.

## What to evaluate

Create recognizable notes in the disposable vault, including:

- multiple notes containing one shared search term;
- one clearly stronger text match;
- a recent and an older note;
- a note under `reference/`;
- a note under `archive/`.

Then verify:

1. Cold start shows `Kwiry: Starting index…`, followed by a real count and percentage without question marks.
2. After the first completed batch, the modal clearly labels results as partial and an indexed result can already be opened.
3. **Text** shows the original lexical order.
4. **Balanced preview** only moves results among equally strong text matches; the clearly stronger text match stays ahead.
5. Results show recognizable titles, paths/headings, and snippets rather than scores, evidence tiers, points, hashes, rule states, or candidate IDs.
6. Final publication automatically replaces the partial label with complete or honestly incomplete coverage.
7. Close and reopen the modal, rebuild, disable and re-enable the plugin, restart Obsidian, and repeat while offline. Check for no network requests, loose Worker/WASM requests, vault-file changes, continuing CPU activity, or orphan Workers.
8. If normal `kwiry-search` is co-installed, confirm its settings and warm-start cache are unchanged before and after the experiment.

The current 10,000-note/50-MiB Worker capacity regression is separately tracked. This field candidate must report existing limits honestly; it does not fix or bypass that regression.

## Technical summary

Use **Copy technical summary** to return aggregate evidence. The exact-schema summary contains only bounded counts, fixed coverage states, movement totals, overlap totals, and allowlisted failure codes.

Do not include vault paths, note content, query text, credentials, screenshots containing private notes, or other private source data in a report.
