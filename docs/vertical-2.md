# Vertical 2: daemon, HTTP, and live reconciliation

Vertical 2 keeps the Vertical 1 lexical behavior while adding one long-running daemon, authenticated HTTP search/status/health, hash-incremental updates, a debounced watcher, live registration reload, and boot reconciliation.

Semantic/vector search, hybrid RRF, cursors, signed claim tokens, HTTP vault administration, rebuild HTTP, MCP, and the Obsidian client remain deferred to later verticals.

## Commands

Run from `daemon/`:

```bash
cargo run -p kwiry -- vault add --id notes --path /absolute/path/to/tree
cargo run -p kwiry -- index
cargo run -p kwiry -- serve
```

The default listener is `127.0.0.1:32189`. Override it for an isolated run:

```bash
cargo run -p kwiry -- --config /tmp/kwiry.toml --data-dir /tmp/kwiry-data serve --bind 127.0.0.1:0
```

Vertical 2 accepts loopback addresses only.

## Token file

The daemon stores only the token-file path in config. On first serve it creates a cryptographically random bearer token in an owner-only file beside the selected config, unless `auth.token_file` points elsewhere.

The daemon prints the token-file path but never prints the token. Read it locally when calling protected endpoints:

```bash
token=$(tr -d '\n' < ~/.config/kwiry/config.token)
```

`GET /v0/health` is public. Search and status require:

```text
Authorization: Bearer <token>
```

## HTTP API implemented in Vertical 2

### Health

```bash
curl http://127.0.0.1:32189/v0/health
```

```json
{"status":"ok"}
```

Health is process liveness, not index readiness.

### Status

```bash
curl -H "Authorization: Bearer $token" \
  http://127.0.0.1:32189/v0/status
```

Status reports daemon state, active generation, document/chunk counts, last sync, `chunking_version`, dirty/rebuilding flags, and per-vault state. `model` is `null` until Vertical 3.

### Lexical search

Vertical 2 callers must explicitly send `"mode":"lexical"`:

```bash
curl -X POST http://127.0.0.1:32189/v0/search \
  -H "Authorization: Bearer $token" \
  -H 'Content-Type: application/json' \
  -d '{
    "q": "phosphorescent indexing",
    "mode": "lexical",
    "filters": {
      "vault_id": "notes",
      "path_prefix": "research/",
      "tags": ["search"],
      "frontmatter_equals": {"status": "active"}
    },
    "limit": 20
  }'
```

Supported filters are exact vault, room, tags, and selected frontmatter equality (`title`, `description`, `status`, `date`), plus normalized vault-relative path prefix. Tags are ANDed. User filters are also designed to compose with future token-pinned filters.

Omitted/hybrid/semantic mode returns `501 mode_unavailable`. A non-null cursor returns `501 cursor_unavailable`. Until a complete generation exists, search returns `503 index_building`. Errors use:

```json
{"error":{"code":"...","message":"..."}}
```

Every successful HTTP search preserves the frozen JSON body and adds:

- `X-Kwiry-Index-Freshness: <state>; basis=<basis>`, where state is `current`, `reconciling`, or `stale` for a searchable generation and basis is the configured evidence basis (`strict_hash` by default, `metadata_audit` when enabled);
- `X-Kwiry-Generation: <generation-id>`, identifying the exact immutable generation that produced the hits.

The existing direct CLI remains compatible and does not require the bearer token:

```bash
kwiry search "query"
kwiry search "query" --json
```

## Data layout

`--data-dir` is now a data root:

```text
<data-root>/
  daemon.lock
  current.json
  generations/
    <generation-id>/
      index/
      manifest.json
```

Each generation contains a Tantivy index and a derived per-file manifest. `current.json` atomically selects the active generation. Index format 5 uses manifest version 2 and generation-layout version 2. Older disposable roots require an explicit `kwiry index` rebuild.

`kwiry index` and live reconciliation build or copy aside, validate the complete candidate, flush its files and directories, rename it into place, then atomically replace and flush `current.json`. Startup removes abandoned staging directories and recovers the newest valid retained predecessor when the pointer is missing, corrupt, or incomplete. The active generation plus two validated predecessors are retained; cleanup that is temporarily blocked by a live reader is retried by later publication.

The data root must be machine-local or local-block storage with reliable exclusive locking, atomic rename, and durable flush behavior. Known SMB/CIFS/NFS and other network-drive types are rejected for derived state; registered source roots may still be network-mounted.

Files and registration config remain the sole source of truth. The generations, manifest, locks, and token can be recreated or rotated as appropriate.

## Freshness basis configuration

Reconciliation runs under an explicit evidence basis. Configure it through the CLI — no manual file editing is required, updates are lock-serialized/validated/atomic, and a running daemon applies them on its next reconcile pass:

```bash
kwiry config set-indexing --basis metadata-audit
kwiry config set-indexing --audit-sources 32 --audit-bytes 33554432 --racy-window-ms 5000
kwiry config set-indexing --basis strict-hash
kwiry config show
```

Invalid values are rejected before anything is written. The stored form in the daemon config is:

```toml
[indexing]
basis = "strict_hash"            # or "metadata_audit"
audit_sources_per_pass = 16      # 1..=256, metadata_audit only
audit_bytes_per_pass = 67108864  # bounded rolling-audit read budget
racy_window_millis = 2000        # recently modified sources are always read
```

- `strict_hash` (default) reads and hashes every discovered source on every pass. This is the unchanged Vertical 2 behavior.
- `metadata_audit` reuses a source without reading its bytes only when its size, mtime, registration fingerprint, and resource classification all match the previous complete generation, and the mtime is older than the racy window. Every new, size-changed, mtime-changed, reclassified, or recently modified source is still read and hashed immediately, as is any source whose semantic vectors are missing. A bounded deterministic rolling audit additionally re-reads metadata-equal sources each pass (up to `audit_sources_per_pass` sources and `audit_bytes_per_pass` bytes, rotating a cursor across passes), so a content change that preserves size and mtime is still caught without a watcher hint.
- `producer_manifest` is contract-approved but not yet available; configuring it is rejected.

Reconcile reports disclose the operation counts (`source_files_read`, `source_bytes_read`, `audited_sources`, `audit_pending`) so unchanged-vault passes are measurable rather than assumed. Strong content hashes remain the sole correctness authority: metadata equality is never treated as proof outside the declared `metadata_audit` policy, and the response freshness header carries the effective basis.

## Incremental correctness

The daemon uses content hashes, not mtime, as the correctness key. Mtime and size are hints/metadata only.

For every reconciliation batch it:

1. Discovers the final filesystem state with the same policy as full indexing.
2. Copies the active complete generation into private staging state.
3. Deletes all changed/removed source keys from staging first.
4. Adds replacement/new chunks and commits each affected staging index.
5. Persists and validates the complete staging manifest/layout.
6. Durably publishes one new generation and atomically swaps the long-lived runtime reader.

The published generation is never mutated in place. A rename therefore removes the old path-derived IDs and inserts the new IDs in one visible generation swap. Delete removes every chunk for the source. A temporarily unavailable vault retains or withholds its last committed content according to registration safety instead of presenting a partially reclassified corpus.

The desktop daemon serves first: it binds and answers HTTP immediately, replying from the last complete generation (disclosed as `stale` with `starting` status) while boot reconciliation runs, and with the typed `503 index_building` before any generation exists — including when registration is empty or broken, where it stays up `degraded` instead of exiting. The readiness line and `connection.json` are still written only once the boot pass completes, so "listening" in the log still means "reconciled". When semantic is enabled, the embedding model loads in the background: `semantic`/`hybrid` return an explicit `mode_unavailable` until it is ready, then a backfill pass embeds the corpus; lexical is never delayed by the model. Shutdown may take up to one in-flight reconciliation pass. The OpenClast profile intentionally keeps boot-before-serve.

Watch events are hints, now carried as per-path evidence rather than a single dirty bit. The watcher accumulates a bounded set of normalized `(vault, path)` entries (both sides of renames; capacity 4,096) and hands them to the debounced pass. Enumeration always remains complete — the path set never restricts discovery or deletion inference; it only *forces* byte reads of the named paths, which under `metadata_audit` closes the same-size/same-mtime window for live edits without waiting for the rolling audit. Watcher errors, backend rescan flags, channel overflow, unwatchable roots, capacity overflow, and config changes all escalate the batch to one full authoritative pass, and a failed pass requeues its batch for the next debounce window instead of waiting for the safety interval. Under the default `strict_hash` basis, watcher evidence never narrows what a pass reads — every pass still hashes every source; path evidence accelerates only the declared `metadata_audit` basis. Path-scoping fidelity is verified on Linux only; on macOS and Windows this is a bounded claim: because path evidence can only force extra reads, a platform delivering poor events costs at worst a missed acceleration, never an incorrect index. Periodic safety reconciliation and boot reconciliation remain full passes. A daemon that just built its first generation in the same process skips the boot pass — there are no offline changes a moments-old build could have missed — while any pre-existing generation is still fully reconciled at startup. A boot reconciliation failure no longer kills the daemon: the last complete generation keeps serving as degraded/stale while the watcher retries. A queued dirty event reports the searchable generation as `stale`; an active strict-hash pass reports it as `reconciling`; only a completed pass reports the resulting generation as `current`.

## Live config reload

`kwiry vault add` remains usable while the daemon runs. Config writes are lock-serialized and atomic. The daemon watches the config and applies registration diffs:

- Added vault: start watching and reconcile it.
- Removed vault: stop watching and remove its indexed sources.
- Changed path or room: replace the watcher and delete/reinsert the vault atomically.

An invalid config update does not replace the last-known-good runtime. The daemon keeps serving prior results and reports degraded/dirty until a valid config is written.

## Checkpoint

Vertical 2 is complete when a running daemon observes a note edit through authenticated lexical search, removes old results after rename/delete, activates a `vault add` without restart, and catches an offline edit after restart. The 2026-07-25 durability follow-on adds format-5 crash consistency, recovery, retention, immutable reconciliation generations, local-storage enforcement, a shared strict-hash reconciliation plan, and generation-bound freshness headers. Passing tests is implementation evidence, not owner acceptance. Metadata reuse, path-scoped watching, and bind-first stale startup remain separate later checkpoints.
