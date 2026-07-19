# Vertical 2: daemon, HTTP, and live reconciliation

Vertical 2 keeps the Vertical 1 lexical behavior while adding one long-running daemon, authenticated HTTP search/status/health, hash-incremental updates, a debounced watcher, live registration reload, and boot reconciliation.

Semantic/vector search, hybrid RRF, cursors, signed claim tokens, HTTP vault administration, rebuild HTTP, MCP, and the Obsidian client remain deferred to later verticals.

## Commands

Run from `daemon/`:

```bash
cargo run -p kwir -- vault add --id notes --path /absolute/path/to/tree
cargo run -p kwir -- index
cargo run -p kwir -- serve
```

The default listener is `127.0.0.1:32189`. Override it for an isolated run:

```bash
cargo run -p kwir -- --config /tmp/kwir.toml --data-dir /tmp/kwir-data serve --bind 127.0.0.1:0
```

Vertical 2 accepts loopback addresses only.

## Token file

The daemon stores only the token-file path in config. On first serve it creates a cryptographically random bearer token in an owner-only file beside the selected config, unless `auth.token_file` points elsewhere.

The daemon prints the token-file path but never prints the token. Read it locally when calling protected endpoints:

```bash
token=$(tr -d '\n' < ~/.config/kwir/config.token)
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

Omitted/hybrid/semantic mode returns `501 mode_unavailable`. A non-null cursor returns `501 cursor_unavailable`. Errors use:

```json
{"error":{"code":"...","message":"..."}}
```

The existing direct CLI remains compatible and does not require the bearer token:

```bash
kwir search "query"
kwir search "query" --json
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

Each generation contains a Tantivy index and a derived per-file manifest. `current.json` atomically selects the active generation. `kwir index` builds aside and publishes only after the candidate index and manifest validate, so a failed rebuild leaves prior results active.

A Vertical 1 root-level Tantivy index remains readable. The next `kwir index` or first daemon start builds and activates a Vertical 2 generation without treating the old index as source data.

Files and registration config remain the sole source of truth. The generations, manifest, locks, and token can be recreated or rotated as appropriate.

## Incremental correctness

The daemon uses content hashes, not mtime, as the correctness key. Mtime and size are hints/metadata only.

For every reconciliation batch it:

1. Discovers the final filesystem state with the same policy as full indexing.
2. Deletes all changed/removed source keys first.
3. Adds replacement/new chunks.
4. Commits once.
5. Explicitly reloads the long-lived reader.
6. Atomically persists the manifest.

A rename therefore removes the old path-derived IDs and inserts the new IDs in one visible commit. Delete removes every chunk for the source. A temporarily unavailable vault retains its last committed searchable content and reports degraded/dirty state instead of appearing empty.

Watch events are hints. The daemon debounces bursts, reconciles final state, performs periodic safety reconciliation, and runs a full reconciliation at boot to catch offline changes.

## Live config reload

`kwir vault add` remains usable while the daemon runs. Config writes are lock-serialized and atomic. The daemon watches the config and applies registration diffs:

- Added vault: start watching and reconcile it.
- Removed vault: stop watching and remove its indexed sources.
- Changed path or room: replace the watcher and delete/reinsert the vault atomically.

An invalid config update does not replace the last-known-good runtime. The daemon keeps serving prior results and reports degraded/dirty until a valid config is written.

## Checkpoint

Vertical 2 is complete when a running daemon observes a note edit through authenticated lexical search, removes old results after rename/delete, activates a `vault add` without restart, and catches an offline edit after restart. Stop for owner approval before adding semantic/vector/RRF work.
