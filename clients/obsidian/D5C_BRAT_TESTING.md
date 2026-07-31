# Private D5C Balanced playground field test

This is a fixture-only evaluation build, not a production Kwiry release. It does not connect to Google Docs or Canva, does not change the normal search profile, and does not make Balanced a public or default relevance policy.

The test plugin uses the distinct ID `kwiry-d5c-balanced-playground`, so it can be installed beside `kwiry-search`. Its build disables the normal active-vault disk cache and the playground reads only its embedded synthetic fixtures.

## Install through BRAT

1. Use a disposable Obsidian vault.
2. Install and enable BRAT.
3. Run **BRAT: Plugins: Add a beta plugin for testing (with or without version)**.
4. Enter `cybersader/kwiry-d5c-balanced-playground`.
5. Select the frozen test release requested for the test, initially `0.0.1`.
6. Confirm Obsidian lists **Kwiry D5C Balanced Playground**, not the normal **Kwiry Search** plugin.
7. Run **Kwiry D5C Balanced Playground: Open private D5C Balanced playground**.

The installed plugin directory must contain exactly `main.js`, `manifest.json`, and `styles.css`. Compare their SHA-256 values with the release attestation before testing.

## What to evaluate

For each embedded case:

- compare Text with strict Balanced ordering;
- confirm a neutralized counterfactual is unmistakably labeled and never presented as strict Balanced;
- confirm fatal cases retain Text but show no Balanced ordering;
- try explanation levels **Off**, **Summary**, and **Rules**;
- try the bounded property experiment packs;
- record whether rank movements feel useful, surprising, or wrong.

Then repeat runs, close the modal during work, disable and re-enable the plugin, restart Obsidian, and rerun while offline. Check that there are no network requests, loose Worker/WASM requests, vault-file changes, continuing CPU activity, or orphan Workers.

If normal `kwiry-search` is co-installed, confirm its settings and warm-start cache still work before and after installing, updating, rolling back, and removing the playground plugin.

## Sanitized report

Record only:

- operating system and architecture;
- Obsidian, Electron, and Chromium versions;
- test release and installed artifact hashes;
- aggregate timings and pass/fail stages;
- subjective notes about the ordering, discrepancy labels, properties, and explanation levels.

Do not include vault paths, note content, query text, credentials, or private source data.

The separate production 10,000-note/50-MiB Worker capacity regression is not evaluated by this fixture playground and remains independently tracked.
