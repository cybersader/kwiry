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
```

- **Socket:** created with mode `0600`. Its directory must exist, be owned by
  the current user and not be group- or world-writable. A stale socket file is
  replaced; any other file at that path is refused. Removed on SIGTERM/Ctrl-C.
  Each connection's peer user is read with `SO_PEERCRED`; a different user is
  refused (`403 {"error":{"code":"forbidden"}}`, an adapter rule, §2.4).
- **No TCP listener at all**, no outbound network, no secrets, no tokens. The
  binary still links the daemon's other code paths (for example the semantic
  model downloader); this mode never calls them.
- **Read-only on the workspace:** it never opens a workspace file. It indexes
  only the bytes it is sent.
- **Data directory:** disposable. Cleared at start (refused if non-empty and
  not created by this mode — marker file `.provider-store`). Nothing is kept
  across restarts: after a restart every partition is `unavailable` until the
  host feeds it again with `begin(full)`.

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
- `checkpoint(n)` makes the writes so far durable (one Tantivy commit);
  `commit` makes everything up to the last checkpoint visible (reader reload
  and a swap of the path → reference map). Writes after the last checkpoint
  are dropped at commit.

Deleted documents stay in their segment until Tantivy's merge policy rewrites
it (the daemon expunges them at every publication; this mode does not).
Retrieval never returns a deleted note, but BM25 document frequencies can
count dead copies until a merge, so ranking after many edits can differ
slightly from a rebuild of the same notes.

Ranking: the query runs over exactly the requested partitions that are ready
(BM25 statistics summed over that set only). Kwiry ranks chunks; the helper
ranks the plan's whole candidate window (`MAX_TOTAL_CANDIDATES`, 512 chunks)
and keeps each note's best chunk, so a reply holds up to `limit` (≤ 200) notes
in deterministic lexical-v2 order. The ladder includes typo, prefix and
partial-term fallbacks, so replies can hold notes that do not contain every
term (false positives, which the contract allows; the host re-checks).
`terms` is accepted as a hint and not used: ranking runs on `q.text` with
Kwiry's own analyser.

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
keys and error tokens live in one module
(`daemon/crates/kwiry/src/provider_helper.rs`, `mod wire`).

| Call | Route | Request → reply |
|---|---|---|
| hello | `GET /v1/hello` | → `{"contract":"search-provider/1","name":"kwiry","version":"0.1.0","capabilities":{"retrieval":["lexical"],"ordering":"relevance","maxLimit":200,"maxPartitions":64,"maxNoteBytes":10485760},"experimental":true,"build":{"commit":"<sha or null>","target":"x86_64-unknown-linux-gnu/release"}}` |
| begin | `POST /v1/partitions/{p}/begin` | `{"epoch":1,"mode":"full"\|"delta"}` → `{}` |
| put | `PUT /v1/partitions/{p}/items/{id}` | raw bytes, `Content-Type: application/octet-stream`, `Provider-Item-Version: <64 hex>`, `Provider-Item-Path: <percent-encoded>` → `{}` |
| delete | `DELETE /v1/partitions/{p}/items/{id}` | → `{}` |
| checkpoint | `POST /v1/partitions/{p}/checkpoint` | `{"n":42}` → `{}` |
| commit | `POST /v1/partitions/{p}/commit` | `{}` → `{"asOf":{"epoch":1,"n":42}}` |
| retire | `DELETE /v1/partitions/{p}` | → `{}` (idempotent) |
| query | `POST /v1/query` | see below |
| status | `GET /v1/partitions/{p}/status` | see below |

**Feed rules.** `full` needs `epoch` ≥ the current epoch (else
`409 epoch_regressed`); `delta` needs `epoch` = the committed epoch (else
`409 epoch_mismatch`). One open feed per partition: a new `begin` abandons an
open, uncommitted one. `put`, `delete`, `checkpoint` and `commit` without an
open feed: `409 no_open_feed`. `n` must strictly increase within an epoch
(`409 checkpoint_not_increasing`). Before a partition's first commit its
state is `building`.

**Size cap.** A `put` body is capped at **10,485,760 bytes** (10 MiB), Kwiry's
`MAX_FILE_BYTES` for files it reads from disk, which matches the contract's
per-note rule. A `Content-Length` over the cap is answered `413 too_large`
before any body byte is read; a body without a length is read only up to the
cap. Either way the id is recorded as excluded with reason
`over_transport_cap`. `maxNoteBytes` is the same 10 MiB: everything that
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
- `q.mode` defaults to `lexical`; any other mode is refused with
  `400 mode_unavailable`, never silently switched.
- `limit`: 1–200. `deadlineMs`: absolute Unix time in ms (default: now + 10 s,
  at most 60 s ahead). Past it, or when the search does not finish by it:
  `503 unavailable` with `Retry-After: 1` (the search thread is not
  cancelled; its answer is dropped).
- `coverage`: exactly one entry per requested partition: `ready` (a committed
  feed is served, possibly with a newer feed open), `building` (known, nothing
  committed yet), `unavailable` (unknown or retired). `asOf` is `null` when
  nothing is committed.
- `refs`: one per (partition, id), deterministic order, with the version that
  was committed at that partition's `asOf`. No scores, text or excerpts.

**status** — `GET /v1/partitions/{p}/status` →

```json
{"state":"ready","asOf":{"epoch":1,"n":63},"notes":61998,
 "excluded":{"over_transport_cap":1,"unsupported_format":1},
 "partial":{"frontmatter_not_extracted":12},"indexBytes":398000000}
```

`notes` counts notes the serving index holds (fully or partly searchable),
including writes of an open delta feed. `excluded` counts accepted notes that
are not searchable, by reason: `over_transport_cap`, `unsupported_format`
(path extension Kwiry does not read), `no_extractable_text`, `unreadable`,
`quarantined` (a per-source budget), `invalid_source`. `partial` (an addition
to the contract's shape) counts notes that are searchable only in part, for
example `frontmatter_not_extracted` (invalid YAML: the body is indexed, the
properties are not). Exclusions never take a partition down and are never
returned as refs. An unknown partition answers `state: "unavailable"`.

| Status | Codes used |
|---|---|
| 400 | `bad_request`, `mode_unavailable` |
| 403 | `forbidden` (peer is another user; adapter rule) |
| 404 | `not_found` |
| 405 | `method_not_allowed` |
| 409 | `no_open_feed`, `epoch_mismatch`, `epoch_regressed`, `checkpoint_not_increasing` |
| 413 | `too_large` |
| 422 | `version_mismatch` |
| 503 | `unavailable` (+ `Retry-After`) |
| 500 | `internal` |

## Tests and measurements

- Socket test: `cargo test -j 2 -p kwiry --test provider_helper` drives the
  whole wire above, including every refusal listed.
- Store tests: `cargo test -j 2 -p kwiry-core --lib provider::`.
- Measurement driver: `bench/provider-helper/drive.py` (feed time, index size,
  peak RSS, query p50/p95 over the `bench/scale-62k` query set, one-edit and
  delete visibility). The partner's bake-off driver (`--socket PATH --pid PID`)
  runs against it unchanged. Results are in the PR that introduced this mode.
