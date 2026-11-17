# Quality Gate v1.1

## Local and CI checks

After installing the pinned Rust toolchain and wasm32 target, wasm-bindgen CLI
0.2.126, and the plugin dependencies with `npm ci --prefix clients/obsidian`, run
`make check` from the repository root. It runs daemon fmt/clippy/tests and plugin
lint/typecheck/tests/version consistency. The plugin tests build WASM, so this is
not a lightweight command. Use Node 22.23.1; CI additionally covers Node 24.22.3.

CI calls the same gate split into `make check COMPONENT=daemon` and
`make check COMPONENT=obsidian` on separate GitHub-hosted jobs. The Windows daemon,
standalone portable adapter, production plugin adapter, rebuild determinism and
FTS5 probes retain their additional checks. Every PR and main push runs the gate
without path filters. Builds remain separate explicit CI steps.

The plugin uses eslint-plugin-obsidianmd's full recommended configuration, with
one typed program scoped to `src/` and root package/manifest validation.
`eslint-suppressions.json` records adoption-time error debt (24 file/rule pairs,
139 violations); recommended warning-level rules remain warnings. No rule is
disabled. Run normal `npm run lint` in the plugin directory. Do not regenerate or
increase the baseline for new code; after fixing debt, use `npm run lint:prune`
and review the reduction. For local lint and pruning, use
`NODE_OPTIONS=--max-old-space-size=1536 nice -n 10 npm run lint` (or `lint:prune`).
TypeScript strict mode remains enabled.

## Tag-built provenance and existing BRAT publication

1. An authorized human pushes a plain version tag (including existing BRAT beta
   versions) whose manifest, package, package-lock and versions entry agree.
   The new `Build and attest tagged Obsidian plugin` workflow builds that tagged
   checkout and attests exactly `main.js`, `manifest.json` and `styles.css`.
   It cannot publish a release. A tag created using `GITHUB_TOKEN` does not start
   this workflow; do not rely on that token to trigger the provenance job.
2. Run the existing CI workflow at the tag and the existing release-candidate
   workflow from trusted main with that successful CI run ID. The candidate still
   rebuilds the tag, runs the pinned real Obsidian WebDriver gate, and constructs
   the immutable exact-byte handoff. Wait for the tag provenance workflow to
   succeed before publication.
3. Dispatch the existing publish workflow with the same exact candidate run ID,
   artifact ID and digest. The protected `obsidian-release` environment and all
   tag/handoff checks remain in force. Before creating a draft, publication now
   verifies that the tested runtime bytes have attestations from the tag workflow,
   the exact tag ref and commit, on GitHub-hosted runners. A mismatch or missing
   attestation fails closed; there is no rebuild or fallback in the publish job.
4. Asset names, ZIP contents, licensing notices, prerelease detection, draft
   creation, uploaded-byte verification and final BRAT publication are unchanged.
   The new provenance artifact is CI retention only, not a replacement release ZIP.

This rollout does not create tags, publish a release, change repository settings
or establish owner field acceptance. Old tags without the new workflow cannot
satisfy the new provenance requirement; make the next release from a commit that
contains this rollout rather than moving an existing tag.

## Audit limitations and owner setup

The v1.1 factory auditor examines root ecosystems only, so reproducible installs
are reported as not applicable for this nested monorepo. Actual JavaScript
installs use `npm ci`; Rust workflow commands and the production WASM build use
`--locked`. Dependabot covers GitHub Actions and the nested Cargo/npm package
roots (dependency-free npm wrappers need no update entry).

The auditor conservatively flags `${{ matrix.node }}` even when all matrix values
are exact versions; CI's Node matrices explicitly use 22.23.1 and 24.22.3. Keep the
matrix coverage rather than duplicate jobs just to satisfy a text heuristic.

The root LICENSE is an index of the existing licenses, not a relicensing.
GitHub private vulnerability reporting must be enabled by the owner before the
SECURITY.md form accepts private reports. Local audits cannot verify that setting;
it was disabled when this rollout was prepared. No repository settings are
changed by the PR.
