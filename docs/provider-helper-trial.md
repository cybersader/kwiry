# Provider helper mode (EXPERIMENTAL, trial only)

> **Experimental.** This mode exists for one owner-approved trial: comparing a
> search index that runs *beside* a host (this helper) with one that runs
> *inside* it, on synthetic notes, and keeping whichever wins. It is desktop and
> non-governed, carries no access rules and no member identity, and is not part
> of `CONTRACT.md`, the governed profile, or the live-check wire. It implements
> the partner's **search provider contract, DRAFT v1** (wire form §2.3), which
> is not frozen; message shapes may still change. Synthetic data only.

## What it is

`kwiry provider-helper` is a separate process that a host feeds notes into and
asks "which notes might contain these words?". It speaks HTTP/1.1 + JSON on a
local Unix domain socket.

```bash
kwiry --data-dir <dir> provider-helper --socket <dir-owned-by-you>/provider.sock
# with search by meaning (semantic and hybrid), from pinned local model files:
kwiry --data-dir <dir> provider-helper --socket <dir>/provider.sock \
    --model-dir <dir-with-pinned-model-files> [--embed-threads N]
```

- **Socket:** created with mode `0600`. Its directory must exist, be owned by
  the current user and not be group- or world-writable. A stale socket file is
  replaced; any other file at that path is refused. Removed on SIGTERM/Ctrl-C.
  Each connection's peer user is read with `SO_PEERCRED`; a different user is
  refused (`403 {"error":{"code":"forbidden"}}`, an adapter rule, §2.4).
- **No TCP listener at all**, no outbound network, no secrets, no tokens. The
  binary still links the daemon's other code paths (for example the semantic
  model downloader); this mode never calls them. The embedding model is read
  only from `--model-dir` (see "Model files" below) and never downloaded.
- **Read-only on the workspace:** it never opens a workspace file. It indexes
  only the bytes it is sent.
- **Data directory:** disposable. Cleared at start (refused if non-empty and
  not created by this mode — marker file `.provider-store`). Nothing is kept
  across restarts: after a restart every partition is `unavailable` until the
  host feeds it again with `begin(full)`. The directory and every directory
  under it are mode `0700`; the helper runs with umask `0077`, so every file
  it writes (index files, the marker) is `0600`.

## Engine and storage

Each note goes through the daemon's own preparation: `prepare_source_buffer`
(format from the path extension, frontmatter, aliases, links, heading-aware
chunking, technical identifiers), the same Tantivy schema and lexical analyser,
and the same lexical-v2 evidence ladder and ranking as `kwiry search`. The path
is an attribute only: it supplies filename, stem and heading signals.

Storage is **one disposable Tantivy index per partition**, not the daemon's
generation machinery (which writes a complete new generation per change —
about 375 MiB per edit at 62,000 notes — and would dominate one-edit timings):

- `begin(full)` creates an empty index that receives every following write;
  the committed index keeps serving until `commit` swaps the new one in, so
  every id not put since the `begin` is gone.
- `begin(delta)` changes the committed index in place.
- `checkpoint(n)` makes the writes so far durable in both Tantivy indexes;
  `commit` makes everything up to the last checkpoint visible (reader reload
  and a swap of the path → reference map). Writes after the last checkpoint
  are dropped at commit.

Deleted documents stay in their segment until Tantivy's merge policy rewrites
it (the daemon expunges them at every publication; this mode does not).
Retrieval never returns a deleted note, but BM25 document frequencies can
count dead copies until a merge, so ranking after many edits can differ
slightly from a rebuild of the same notes.

Each generation also holds a small **body-words index**: one document per
searchable note with its `current-body-words-v1` words (see below), used only
to decide which notes contain all of a query's `terms`.

### Lexical: ranking and the recall obligation

Ranking: the query runs over exactly the requested partitions that are ready
(BM25 statistics summed over that set only). Kwiry ranks chunks; the helper
ranks the plan's whole candidate window (`MAX_TOTAL_CANDIDATES`, 512 chunks)
and keeps each note's best chunk. The ladder includes typo, prefix and
partial-term fallbacks, so Kwiry's own list can hold notes that do not contain
every term.

The partner's **lexical recall obligation (superset-under-limit)** is met on
top of that ranking:

- **Tier 1** is every committed note of the asked partitions whose body
  contains all of `terms` under `current-body-words-v1`. Its members that
  Kwiry ranked come first, in Kwiry's order; the rest follow by a BM25 score
  over the body words (statistics summed over the asked partitions only).
- **Tier 2** is every other note Kwiry ranked, in Kwiry's order.
- The answer is tier 1 then tier 2, cut at `limit`. So when tier 1 fits in
  `limit` every one of its notes is returned, and when it does not, every
  returned note is a tier-1 note; a non-matching note only ever fills space
  left over.
- Equal ranks (equal Kwiry scores, equal body-words scores) are ordered by
  `id` bytes ascending.

`terms` is **authoritative** for tier 1. It may be the draft-v1 plain list
(`"terms": ["quokka"]`) or the draft-v2 form
(`"terms": {"matcher": "current-body-words-v1", "list": ["quokka"]}`); any
other matcher is refused with `400 matcher_unavailable`. Absent or empty, the
terms are derived from `q.text` with the same matcher. Kwiry still ranks on
`q.text` with its own analyser.

The matcher is a port of the partner's `sync-core/src/search-match.ts`
(OpenClast commit `5eefd8bf`): the eligible body is the text after an initial
YAML block (`---` … `---`/`...`, BOM ignored for detection, unclosed block =
no body); folding is NFD, full default case folding, then every `M*` code
point dropped, using the partner's per-line and bounded NFD segmentation;
words are maximal runs of `L*`/`Nd`. General categories and
folding use the partner's generated Unicode 15.1 tables
(`daemon/crates/kwiry-core/src/provider/unicode_tables.rs`, Unicode License v3,
`LICENSE-UNICODE` beside it); NFD uses `unicode-normalization`. Words over 240
bytes are indexed and asked for by digest so neither side drops them. A note
that is not UTF-8 owes no body words (the host's matcher cannot read it
either).

### Semantic and hybrid

With `--model-dir`, hello declares `retrieval: ["lexical","semantic","hybrid"]`
and every commit is also embedded:

- **Storage, per partition:** one SQLite file with chunk vectors scored with
  sqlite-vec (`vectors-<full-sequence>-<suffix>.sqlite` beside the lexical
  index). It ties each note's vectors to its id and the version (SHA-256 of
  the exact bytes) they were computed from. The worker reads chunk text from
  the lexical snapshot (heading breadcrumb + content, as the daemon embeds
  them); it needs no passage text queued by the feed.
- **Background embedding:** a lexical commit does not wait for embedding.
  One worker handles one running job and at most one queued job per
  partition. It coalesces pending changes at the latest commit and rebuilds
  from that snapshot if more than 4,096 distinct changes accumulate. Passage
  batches hold at most 128 chunks and 1 MiB of text, except that a larger
  single chunk runs alone. A `full` build publishes a fresh file only when
  complete; a `delta` streams into a single SQLite transaction. A later
  `full` commit or a retire makes earlier work moot.
- **State per kind:** the semantic kind is `ready` only when its vectors hold
  exactly the lexical kind's committed `asOf`. While embedding lags, it is
  `building` at the last fully embedded commit of the current full feed,
  or at `null` with no semantic refs until that feed has vectors. On an
  embedding failure, the current generation is `unavailable`; a new `full`
  feed is needed to recover. Semantic answers exclude vectors whose note
  version does not match the lexical snapshot.
- **semantic:** the query is embedded once; each asked partition scans its
  eligible notes and ranks by best chunk (cosine-equivalent L2 on normalized
  vectors), then `id` bytes, before cutting at the limit. The per-partition
  answers are merged by the same order.
- **hybrid:** reciprocal-rank fusion (`rrf_fuse`, k = 60, the daemon's
  formula) of the lexical answer and the semantic answer at note level, each
  up to max(`limit`, 100) deep. Both legs use the same lexical snapshot; if it
  changes between the legs, both retry. Ties by `id` bytes. `coverage` holds two
  entries per partition: `mode: "lexical"` and `mode: "semantic"`. The
  recall obligation is a lexical rule and does not apply to `hybrid`.
- **status** adds `"semantic": {"state", "asOf", "pendingNotes"}` when a model
  is loaded.

### Partition scoping (why an unasked partition cannot influence a result)

Candidates and statistics come only from the asked partitions:

- Lexical ranking and the body-words BM25 sum their statistics over exactly
  the asked partitions' searchers.
- Vectors live in one file per partition and a query opens only the files of
  the asked partitions; there is no shared vector table to search and then
  filter. Distance needs no corpus statistics, so a query over a set is
  exactly the merge of the per-partition queries.
- Tests: `provider::tests::semantic_search_reads_only_the_asked_partitions`
  (an unasked partition whose vectors are nearer than every asked vector
  changes neither membership nor order; a set query equals the merge of
  per-partition queries in membership and keeps each partition's order;
  retiring the other partition changes nothing) and
  `provider::tests::lexical_obligation_counts_only_the_asked_partitions`.

### Model files (offline, pinned)

The helper loads `BAAI/bge-small-en-v1.5` as exported to ONNX by Xenova,
revision `ea104dacec62c0de699686887e3f920caeb4f3e3` of
`https://huggingface.co/Xenova/bge-small-en-v1.5`, from five files in
`--model-dir`, each checked against a pinned SHA-256
(`PINNED_MODEL_FILES` in `daemon/crates/kwiry-core/src/provider/pinned_model.rs`):

| File in `--model-dir` | Upstream path | SHA-256 |
|---|---|---|
| `model.onnx` | `onnx/model.onnx` | `828e1496d7fabb79cfa4dcd84fa38625c0d3d21da474a00f08db0f559940cf35` |
| `tokenizer.json` | `tokenizer.json` | `d241a60d5e8f04cc1b2b3e9ef7a4921b27bf526d9f6050ab90f9267a1f9e5c66` |
| `config.json` | `config.json` | `fa73f90bf92c8cace1fbcb709626306f2bdbc9ea3e5b5f94b440df9b6aa56350` |
| `special_tokens_map.json` | `special_tokens_map.json` | `b6d346be366a7d1d48332dbc9fdf3bf8960b5d879522b7799ddba59e76237ee3` |
| `tokenizer_config.json` | `tokenizer_config.json` | `9261e7d79b44c8195c1cada2b453e55b00aeb81e907a6664974b4d7776172ab3` |

The model is not bundled in the helper or the repository. The operator
fetches the five files on a connected machine, for example
`curl -fL -o model.onnx https://huggingface.co/Xenova/bge-small-en-v1.5/resolve/ea104dacec62c0de699686887e3f920caeb4f3e3/onnx/model.onnx`
(and the same for the other four), checks the hashes, and copies the
directory. A missing, unreadable or different file makes the helper **refuse
to start** (it never fetches and never silently drops to lexical only).
Without `--model-dir` the helper is lexical only and hello says so.

Two model instances are loaded: one for queries (2 ONNX threads), one owned
by the background worker (`--embed-threads`, default all CPUs the process may
use), so a long feed never queues a query behind a passage batch.

**Build-time download.** `semantic-onnx` enables fastembed's
`ort-download-binaries-rustls-tls`. A connected build can download a prebuilt
ONNX Runtime archive via `ort-sys` (2.0.0-rc.12), independently of the model
files. To preprovision an offline build, cache the locked Cargo dependencies
and copy the matching target's extracted ONNX Runtime from a connected build
(`~/.cache/ort.pyke.io/dfbin/<target>/<archive-sha256>/`). Set
`ORT_LIB_LOCATION` to the directory containing its `libonnxruntime.a` before
building with `cargo --offline` (or `CARGO_NET_OFFLINE=true`). `ort-sys` checks
that explicit library path before its offline/skip-download switch; setting
only the switch skips its cache lookup and can leave `OrtGetApiBase` unlinked.
Without that switch, a matching pre-populated cache also avoids the download.
The helper downloads nothing at run time. A lexical-only `kwiry` build
(`--no-default-features`) has no ONNX Runtime and refuses `--model-dir`.

## Identity, versions and partitions

- **partition:** `[A-Za-z0-9_-]{1,64}`; anything else is `404 not_found`.
- **id:** opaque, 1–256 UTF-8 bytes, percent-encoded in the path.
- **version:** lowercase hex SHA-256 of the exact bytes; recomputed and a
  mismatch refused (`422 version_mismatch`). `put` is idempotent on
  (`id`, `version`, path).
- **path:** normalized relative forward-slash path (no leading/trailing `/`,
  no empty, `.` or `..` component, no `\`), ≤ 1,024 bytes, sent
  percent-encoded in `Provider-Item-Path`. A put of a path another id holds
  replaces that holder (a rename arrives as delete + put).

Each partition's chunks carry an internal owner tag derived from the partition
name, so the same path in two partitions stays two notes in a set query.

## Wire (search provider contract DRAFT v1, §2.3)

Every request must carry `Search-Provider-Profile: v1` (else
`400 bad_request`); every response carries it. Errors are
`{"error":{"code":"<token>"}}` with no other text. JSON request bodies are
capped at 65,536 bytes and refuse unknown keys. Routes, header names, JSON
keys and shapes, and error tokens with their HTTP statuses live in one
module (`daemon/crates/kwiry/src/provider_helper.rs`, `mod wire`); the store
(`kwiry-core`) returns Rust types and an error enum and knows none of them.
The exclusion reason classes in `status` are Kwiry's own and come from the
store.

| Call | Route | Request → reply |
|---|---|---|
| hello | `GET /v1/hello` | → `{"contract":"search-provider/1","name":"kwiry","version":"0.1.0","capabilities":{"retrieval":["lexical"],"matchers":["current-body-words-v1"],"ordering":"relevance","maxLimit":200,"maxPartitions":64,"maxNoteBytes":10485760},"experimental":true,"build":{"commit":"<sha or null>","target":"x86_64-unknown-linux-gnu/release"}}` (`retrieval` is `["lexical","semantic","hybrid"]` with a model loaded) |
| begin | `POST /v1/partitions/{p}/begin` | `{"epoch":1,"mode":"full"\|"delta"}` → `{}` |
| put | `PUT /v1/partitions/{p}/items/{id}` | raw bytes, `Content-Type: application/octet-stream`, `Provider-Item-Version: <64 hex>`, `Provider-Item-Path: <percent-encoded>` → `{}` |
| delete | `DELETE /v1/partitions/{p}/items/{id}` | → `{}` |
| checkpoint | `POST /v1/partitions/{p}/checkpoint` | `{"n":42}` → `{}` |
| commit | `POST /v1/partitions/{p}/commit` | `{}` → `{"asOf":{"epoch":1,"n":42}}` |
| retire | `DELETE /v1/partitions/{p}` | → `{}` (idempotent) |
| query | `POST /v1/query` | see below |
| status | `GET /v1/partitions/{p}/status` | see below |

**Feed rules.** `full` needs `epoch` ≥ every epoch begun so far, committed
or abandoned (else `409 epoch_regressed`); `delta` needs `epoch` = the
committed epoch (else `409 epoch_mismatch`). One open feed per partition:
every `begin`, even one that is then refused, abandons an open, uncommitted
one. `put`, `delete`, `checkpoint` and `commit` without an open feed:
`409 no_open_feed`. `n` must strictly increase within an epoch, counted from
the last commit (`409 checkpoint_not_increasing`). Before a partition's first
commit its state is `building`.

**Abandoning a delta after a checkpoint.** A `delta` writes into the serving
index in place, and a checkpoint makes its writes durable there; they cannot
be taken back. So when a `delta` that checkpointed is abandoned (by a new
`begin`), the partition keeps serving its last commit (queries, coverage
`asOf` and `status` all report that commit, never the abandoned writes), and
it refuses `delta` with `409 epoch_mismatch` until a `full` feed commits. The
host recovers as contract §2.2 rule 2 describes for a lost feed: a `full`
resync with the same epoch, whose checkpoints count from the committed `n`
(so resending the abandoned checkpoint number is accepted). A `delta`
abandoned before any checkpoint leaves nothing behind and needs no resync,
unless a checkpoint failed. If either index fails a checkpoint, the helper
cannot know whether both persisted the same writes. It refuses to publish
or to take another delta on that generation, and keeps serving the last
commit until a full resync succeeds.

**Size cap.** A `put` body is capped at **10,485,760 bytes** (10 MiB), Kwiry's
`MAX_FILE_BYTES` for files it reads from disk, which matches the contract's
per-note rule. A `Content-Length` over the cap is answered `413 too_large`
before any body byte is read; a body without a length is read only up to the
cap. Either way the id is recorded as excluded with reason
`over_transport_cap`. With no open feed there is nothing to record it in, so
an over-cap `put` gets the same `409 no_open_feed` as any other `put`. `maxNoteBytes` is the same 10 MiB: everything that
arrives is prepared and searched unless the preparation itself excludes it.

**query** — `POST /v1/query`:

```json
{"partitions":["p1","p2"],"q":{"text":"quokka habitat","mode":"lexical"},"terms":["quokka","habitat"],"limit":100,"deadlineMs":1795545600000}
```

→

```json
{"mode":"lexical",
 "coverage":[{"partition":"p1","mode":"lexical","state":"ready","asOf":{"epoch":1,"n":42}},
             {"partition":"p2","mode":"lexical","state":"building","asOf":null}],
 "refs":[{"partition":"p1","id":"…","version":"…"}]}
```

- `partitions`: 1–64 distinct valid names (else `400 bad_request`).
- `q.mode` defaults to `lexical`; a mode hello does not declare is refused
  with `400 mode_unavailable`, never silently switched.
- `terms`: see "Lexical: ranking and the recall obligation"; at most 128.
- `limit`: 1–200. `deadlineMs`: absolute Unix time in ms (default: now + 10 s,
  at most 60 s ahead). Past it, or when the search does not finish by it:
  `503 unavailable` with `Retry-After: 1` (the search thread is not
  cancelled; its answer is dropped).
- `coverage`: exactly one entry per requested partition and kind (two for
  `hybrid`): `ready` (a committed feed is served, possibly with a newer feed
  open), `building` (known, nothing committed yet; for the semantic kind also
  while embedding lags), `unavailable` (unknown or retired; semantic after an
  embedding failure). `asOf` is `null` when nothing is committed.
- `refs`: one per (partition, id), deterministic order, with the version that
  was committed at that partition's `asOf`. No scores, text or excerpts.

**status** — `GET /v1/partitions/{p}/status` →

```json
{"state":"ready","asOf":{"epoch":1,"n":63},"notes":61998,
 "excluded":{"over_transport_cap":1,"unsupported_format":1},
 "partial":{"frontmatter_not_extracted":12},"indexBytes":398000000}
```

`status` reports the last commit, like `asOf`: `notes` counts the notes it
holds (fully or partly searchable); writes of an open or abandoned feed are
not counted. `excluded` counts accepted notes that
are not searchable, by reason: `over_transport_cap`, `unsupported_format`
(path extension Kwiry does not read), `no_extractable_text`, `unreadable`,
`quarantined` (a per-source budget), `invalid_source`. `partial` (an addition
to the contract's shape) counts notes that are searchable only in part, for
example `frontmatter_not_extracted` (invalid YAML: the body is indexed, the
properties are not). Exclusions never take a partition down and are never
returned as refs. An unknown partition answers `state: "unavailable"`.

| Status | Codes used |
|---|---|
| 400 | `bad_request`, `mode_unavailable`, `matcher_unavailable` |
| 403 | `forbidden` (peer is another user; adapter rule) |
| 404 | `not_found` |
| 405 | `method_not_allowed` |
| 409 | `no_open_feed`, `epoch_mismatch`, `epoch_regressed`, `checkpoint_not_increasing` |
| 413 | `too_large` |
| 422 | `version_mismatch` |
| 503 | `unavailable` (+ `Retry-After`) |
| 500 | `internal` |

## Tests and measurements

- Socket tests: `cargo test -j 2 -p kwiry --test provider_helper` drive the
  whole wire above, including every refusal listed, recovery from an
  abandoned delta checkpoint, data-directory permissions and a chunked body
  over the cap.
- Store tests: `cargo test -j 2 -p kwiry-core --lib provider::` (feeds,
  abandoned-delta recovery, the recall obligation including the partner's
  `galka` case, partition scoping of vector search, embedding lag, the
  matcher port, the vector file, and — with `--features semantic-onnx` — the
  refusal of missing or different model files).
- Meaning driver: `bench/provider-helper/drive_meaning.py` (feed, lexical and
  embedding build time, RSS, index size, p50/p95 per kind, one-edit
  visibility per kind; `--max-notes` for a subset).
- Measurement driver: `bench/provider-helper/drive.py` (feed time, index size,
  peak RSS, query p50/p95 over the `bench/scale-62k` query set, one-edit and
  delete visibility). The partner's bake-off driver (`--socket PATH --pid PID`)
  runs against it unchanged. Results are in the PR that introduced this mode.
