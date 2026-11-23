# Scale corpus and measurements (1,800 and 62,000 notes)

Synthetic Markdown vaults at the sizes OpenClast workspaces reach, plus a harness
that measures the shipped `kwiry` release binary on them in the desktop and the
governed (`live-check-v1`) profiles. Nothing generated is committed.

## Generate a corpus

```bash
node bench/scale-62k/generate.mjs --notes 62000 --seed 62000 --out <dir>
node bench/scale-62k/generate.mjs --notes 1800  --seed 1800  --out <dir>
```

`<dir>` must not exist. Output: `<dir>/vault/` (notes), `<dir>/manifest.tsv`
(`path`, `bytes`, `sha256`; vault-relative `/` paths sorted by UTF-8 bytes) and
`<dir>/README.md` (seed, generator commit, totals). Same seed and generator
commit give byte-identical output. Shape: nested folders (depth 1–5, ~40 notes
per folder), log-normal sizes (median ~2 KiB, long tail capped near 50 KiB, ~7%
stubs), YAML frontmatter on ~45%, headings, lists, task lists, fenced code,
tables, quotes, wikilinks (plain, aliased, path, heading, some dangling),
repeated basenames across folders, and non-ASCII words, names and paths. All
text is invented by a seeded PRNG.

## Measure

Build the release binary once, then point the harness at a corpus and an
empty scratch directory (never under tmpfs for 62k):

```bash
cargo build -j 2 --release -p kwiry          # from daemon/
python3 bench/scale-62k/measure.py --kwiry daemon/target/release/kwiry \
    --corpus <corpus-dir> --work <scratch>/run --fixtures <scratch>/fixtures
```

`measure.py` reflink-copies the vault (the corpus is never modified) and records
to `<scratch>/run/results.json`:

- desktop: `kwiry index` wall time and peak RSS (`/usr/bin/time -v`), data-dir
  size, reconcile of 10 modified + 5 deleted + 5 renamed notes three ways (live
  watcher, offline then boot reconciliation, CLI rebuild), single-edit watcher
  latency, 50 derived queries x 3 passes over HTTP, 30 s idle CPU/read;
- governed: the pilot configuration (`profile = "openclast"`,
  `profile_required`, loopback, `strict_hash`, workload-only auth, one room
  with `snapshot_id`), `kwiry index`, then the same queries with
  `Kwiry-Profile: live-check-v1` and one fresh capability per request, plus
  `hits` / `no-hits` fixtures in the wire-fixture descriptor format.

Capabilities are signed with `openssl pkeyutl` using the synthetic Ed25519
test key in `daemon/crates/kwiry/tests/daemon.rs` and the claim shape of
`format_claims()` there; no other key material is introduced.

Follow-ups that reuse `<scratch>/run`:

- `governed_passes.py` — samples the governed daemon from `/proc` (CPU,
  bytes read, RSS) every 0.1 s with a search every 0.2 s, segments strict_hash
  passes, then repoints the room to a fresh copy and times recovery
  (`--idle`, `--repoint-timeout`).
- `quiet_control.py` — same daemon with no traffic until after the first 60 s
  safety tick, then probes; separates engine behaviour from request load.
- `capture_window.py` — boots the governed daemon a bounded number of times and
  keeps the first 200 as a fixture, for when the gate is only briefly open.
- `stalled_reader.py` — the GDB non-stop barrier from
  `daemon/crates/kwiry/tests/support/live_check_barrier.rs`, against the
  release binary (breaks on `kwiry_core::search::search_partitions_with_quality`,
  the callee of the function the test uses, which is inlined in release).

The harness kills kwiry if `MemAvailable` drops below 4 GiB.
