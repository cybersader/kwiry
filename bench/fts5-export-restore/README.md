# SQLite WASM export/restore feasibility gate

Standalone Node harness for the Obsidian durable-cache feasibility question:
can the exact official SQLite WASM runtime export a complete FTS5 generation
and restore it exactly, within the kill limits, with an async main thread as
the cache port and never more than one worker alive?

This gate runs **before** any Worker protocol change. It proves runtime/API
feasibility on the required corpus scale; it does not implement the
production cache, and its automated verdict is not an owner decision. The
harness was adversarially reviewed by three independent reviewers and
rebuilt to their findings before the verdict below was accepted; the first
version's flaws (a null event-loop test, sampled rather than byte-exact
restoration proof, literal attestations, an unrealistic 120-word vocabulary,
and inflated memory baselines) are all closed in this version.

## Pinned runtime

Reuses the byte-identical installation pinned by the Gate 1 probe in
[`../fts5-wasm/`](../fts5-wasm/): `@sqlite.org/sqlite-wasm@3.53.0-build1`,
SQLite 3.53.0 with FTS5. Both the official `sqlite3.wasm` (864,752 bytes,
SHA-256 `02d7e481…ba1d312`) **and** the JS glue that implements
`sqlite3_js_db_export`/`sqlite3_deserialize` (`dist/node.mjs`, SHA-256
`d74e4b74…c1b29c4`) are hash-verified before every worker initializes. Run
`npm ci` in `../fts5-wasm/` first; this directory has no dependencies of its
own.

## What it does

1. Streams a deterministic seeded corpus — 10,000 notes, ≥50 MiB of
   composed real Markdown (measured UTF-8 bytes, not an estimate), ~20k
   chunks, Zipfian vocabulary of ~80k types with heavy-tailed note sizes —
   note by note into a clean build of the Gate 1 schema in one worker,
   which then exports via `sqlite3_js_db_export` and is terminated.
2. The main thread round-trips the blob through disk **asynchronously**
   (temp write, handle fsync, rename, async read-back) — hashing happens in
   the workers, never on the host thread.
3. A short-lived proof worker deserializes the blob and **re-exports it:
   the result must reproduce the original bit for bit**, which subsumes
   every schema/row/index comparison.
4. A production-shape restore worker verifies the blob hash, deserializes,
   releases every redundant blob reference, validates (schema objects, an
   all-columns whole-table digest, `PRAGMA integrity_check`, the strong
   external-content FTS5 `'integrity-check'` with non-zero rank, seven
   query result sets with unrounded scores), then runs the full cache
   loop: delete a marker source, insert a replacement, prove the deletion
   and insertion are both observable, probe `chunks_fts` directly for
   orphaned postings, re-run the strong integrity check, and re-export the
   mutated database. It then stays alive holding the live index while
   steady-state memory is measured, as an increment over process baseline.
5. A negative-control worker proves the machinery can fail: a
   desynchronized external-content table makes the strong integrity check
   throw, and a truncated blob is rejected instead of restored.
6. Instruments: a 5 ms heartbeat gap detector for host-thread stalls
   (immune to the `monitorEventLoopDelay` re-baselining blindness that made
   the first version a null test) applied to **every** phase; an off-thread
   RSS sampler with timestamped phase attribution and increment-over-floor
   accounting; derived (never literal) checks for worker exclusivity,
   guard-counter cleanliness (network/helper-worker/persistence stubs, as
   in Gate 2), corpus scale, and byte exactness; full provenance
   (timestamp, runtime, platform, harness and glue hashes) in the evidence.

## Kill limits

- cache blob ≤ 384 MiB;
- validated restore < 5 s **and** < 0.5× clean build+validate;
- host-thread stall < 100 ms in every phase;
- restore added RSS ≤ 1.25× clean-build added RSS (increment over floor);
- steady-state **added** RSS with the live index resident vs the 300 MiB
  target is reported (informational — a previously known miss).

## Run

```bash
node scripts/gate.mjs   # exit 0 = automated GO, 1 = NO-GO
```

Expected stderr note: the negative control makes the SQLite wrapper print
one `SQLITE_CORRUPT_VTAB` diagnostic while proving the integrity check can
fail. That line is the control working, not an error.

## Automated result (2026-07-25, this development machine, 3 consecutive runs)

Verdict: **GO** — all thirteen checks pass in every run, with a
byte-identical blob across runs.

- clean build ≈5.5 s + 1.3 s validation; export ≈100 ms; blob 99.7 MiB
  (1.96 blob bytes per Markdown byte at realistic vocabulary);
- restore: deserialize ≈43 ms; fully validated ready ≈1.4 s (≈0.21× clean
  build); cold wall including worker spawn, init, and disk read ≈1.5 s;
- byte-exact proof (deserialize + re-export + hash compare) ≈250 ms;
- host-thread stall maximum 1–6 ms across all phases (limit 100 ms);
- added RSS: restore ≈0.94× clean build (limit 1.25×);
- steady-state added RSS with the live index resident ≈583 MiB —
  **misses** the 300 MiB informational target in this Node harness.

## What the GO establishes — and what it does not

It establishes that the official export/deserialize API pair, on the exact
pinned runtime, round-trips a corpus-scale FTS5 generation byte-exactly
through an atomic disk cache with a responsive host thread, a live and
mutable restored index, working failure detection, and restore ≈4× faster
than a clean rebuild.

It does **not** establish: the production Worker protocol; export from a
long-lived worker that keeps serving searches; staging-restore beside a
resident generation; co-residency with the Rust WASM module (all memory
figures here are single-WASM); Electron-renderer behavior; cache
identity/versioning; or behavior near the blob cap. Extrapolating the
measured density, a vault beyond ≈196 MiB of Markdown would exceed the
384 MiB cap — a soft boundary (discard cache, clean build), reached only by
unusually large vaults, but a real one. The steady-state memory target
remains missed and is an owner-facing caveat, not something this gate can
wave away.

## Memory experiments (owner rider, 2026-07-25/26)

`scripts/memory-experiments.mjs` measures schema variants on the same
corpus. Headline: **contentless FTS5 (`contentless_delete=1`) yields a
46.0 MiB image (0.46× baseline) with phrase queries, column filters, BM25
ranking, and deletes intact** — only `snippet()` is lost, with excerpts
hydrating from vault files (the authoritative source) at render time.
`optimize`+`VACUUM` at export time and a slimmed metadata table are
capability-neutral and reach 35.9 MiB; `detail=column` reaches 25.7 MiB but
drops phrase queries (an owner-level capability trade).

`experiments/` preserves a working research prototype (`vfs.mjs`,
`roundtrip.mjs`, plus builders/measurers; `node build.mjs` regenerates the
image files): a complete JS `sqlite3_vfs` on the pinned build backing the
database with deflate-compressed 64 KiB blocks held in JS memory instead of
`sqlite3_deserialize`'s permanent wasm allocation. Verified: byte-exact
round trip, live writes, both integrity checks, ~3 ms ranked queries.
Effect: the wasm floor stays ~8-10 MiB regardless of database size and the
resident total for the contentless image drops ~56 → ~37 MiB; it also
eliminates the export-time full-image wasm copy. Adopting it would change
the cache restore mechanism and format — an owner/contract decision for the
integration gate, recorded in the private register. Measured dead ends
(page_size, mmap, kvvfs at 3.6× expansion, zstd, shared-dictionary deflate,
`columnsize=0`) are recorded there too.
