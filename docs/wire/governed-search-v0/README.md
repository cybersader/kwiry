# Governed search wire schema: `governed-search-v0`

This is a description of the direct Kwiry daemon wire at commit `655c243815342618f776c6b78a749335a8197450`, not a change to the frozen contract or a grant to enable it in production. It describes the deployed `server.profile = "openclast"` build (`room-v1`, `multi_room=closed`), not the `open` unit-test override, desktop profile, or an OpenClast gateway. The format flags are derived in [room_format.rs:1–54](../../../daemon/crates/kwiry-core/src/room_format.rs#L1-L54).

- `POST /v0/search`: [request](request.schema.json), [HTTP 200](success.schema.json), and [errors](error.schema.json). Send an object with explicit `mode: "lexical"`. Read freshness and generation from the [response headers](headers.md), not the JSON.
- `GET /v0/health`: unauthenticated [liveness and format](health.schema.json), not an index readiness or authorization check.
- [Capability signer and verifier rules](capability.md). A valid capability and schema-valid request can still fail if an effective signed resource is not searchable.
- All four body schemas use JSON Schema Draft 2020-12. The `x-http-status` annotations in `error.schema.json` enumerate the HTTP status paired with each body code; the status is not a body member. Branch on status/code, not the human-readable error `message`.

## Deployment layout

Keep **config + public JWKS**, **room note roots**, and **index data** in three disjoint directory trees. The config parent (not just `config.toml`) and the configured JWKS are protected paths. No room may contain or alias protected state, including through bind mounts or other mount aliases. Use a canonical absolute room path and private machine-local index data with reliable locking, atomic rename, and durable flushes; the secure governed walker is Linux-only and requires `openat2`. See [config.rs:146–180](../../../daemon/crates/kwiry-core/src/config.rs#L146-L180), [room_boundary.rs:171–218](../../../daemon/crates/kwiry-core/src/room_boundary.rs#L171-L218), [room_boundary.rs:449–513](../../../daemon/crates/kwiry-core/src/room_boundary.rs#L449-L513), and [room_walk.rs:56–104](../../../daemon/crates/kwiry-core/src/room_walk.rs#L56-L104).

```text
/srv/kwiry/
├── control/                 # --config /srv/kwiry/control/config.toml
│   ├── config.toml
│   └── search-jwks.json      # auth.openclast.jwks_file (absolute path)
├── room-notes/              # registered vault path; notes only
│   └── handbook.md
└── index-data/              # --data-dir /srv/kwiry/index-data
```

The issuer's private signing key is not part of the Kwiry deployment.

## Known differences from CONTRACT.md and the IG-1 guide

| Source | Deployed behavior |
|---|---|
| [CONTRACT.md §4](../../../CONTRACT.md#4-http-api-v0-surface) describes `hits` + `next_cursor`. | Success also has `extraction_policy_fingerprint` (the build's extraction policy, not the index digest). `next_cursor` is always `null`. Hits report `room_id`, not the internal chunk model's `room`, and use room-relative paths. Hits carry a fixed frontmatter projection, format, indexed coverage, and nullable tagged locator. [CONTRACT.md §10.4](../../../CONTRACT.md#10-multi-format-and-resumable-indexing-amendment-2026-08-02) gives a Canvas node as an example, but the deployed locator tags are only `base_view`, `pdf_page` and `excel_cell`. |
| [CONTRACT.md §4](../../../CONTRACT.md#4-http-api-v0-surface) lists the shared hybrid default, modes and cursor member. | Only lexical succeeds here. Omission parses as hybrid and produces `501 mode_unavailable`; string cursors produce `501 cursor_unavailable` after earlier validation, while `null` is allowed. In addition to the named JSON object, Serde accepts a positional array for the request and for `filters`; clients should not depend on that representation. |
| [CONTRACT.md §5](../../../CONTRACT.md#5-identity-scoping-and-authorization-model) describes a required represented subject and distinct actor. | The workload branch instead requires an allow-listed workload `sub`, typed JWS and no actor or revision members. An operator-gated actor-bearing legacy branch still exists and does not use the workload replay checks. `room-v1` enforces opaque IDs, strict tuple ordering, unique room IDs and, in the `closed` build, unique vault IDs. |
| [CONTRACT.md §10d](../../../CONTRACT.md#10d-lexical-v2-field-aware-relevance-amended-2026-09-05) excludes arbitrary numeric boosts and query text/engine exceptions from safe diagnostics. | The separate native explicit-syntax path accepts Tantivy numeric boosts (a nonfinite engine score is serialized as `null`). Those boost behaviors are recorded, not recommended client features. |
| [IG-1 freshness](../../openclast-ig1.md#derived-state-storage-and-freshness) shows `strict_hash` and an exact immutable generation. | The response discloses a scoped `v1-…` digest and per-effective-set freshness, never a global generation ID. `metadata_audit` is also possible; the proposed `producer_manifest` basis is rejected by configuration. Search also sets room-format and lexical-policy/match/field headers; health signals room format in JSON only. |
| [CONTRACT.md §4](../../../CONTRACT.md#4-http-api-v0-surface) and the [IG-1 failure table](../../openclast-ig1.md#failure-behavior) do not fully describe direct sidecar errors. | Consult the closed error schema. `503 resource_unavailable` for an unavailable effective partition has the signed positions required by §4/R17; the shared resource-unavailable envelope also allows a global availability failure without positions. An empty intersection can return `200` with no hits. Gateway-only `502`/`504` are not sidecar JSON errors. Governed startup waits for a usable snapshot; the shared mapper's `503 index_building` is not a deployed governed startup response. Unmounted paths return `404 not_found` outside these two routes. |
| [IG-1 example configuration and startup](../../openclast-ig1.md#configure-kwiry) and [freshness](../../openclast-ig1.md#derived-state-storage-and-freshness) are older. | Replace human-readable example IDs with the opaque `room-v1` forms and configure workload subjects and, when appropriate, the legacy cutoff. The manifest/layout/index versions here are **5 / 3 / 13**, not 2 / 2 / 5. OpenClast runs a boot reconciliation even on a freshly built generation; the daemon starts serving only after boot checks. Path-scoped watching is implemented; bind-first HTTP serving is still desktop-only. |
| [IG-1 status and verification](../../openclast-ig1.md#status) report the witness and foundation acceptance as pending and describe a proposed gateway. | [The roadmap](../../../ROADMAP.md#ig-1--governed-lexical-openclast-gateway--foundation-owner-accepted-2026-11-17-production-enablement-not-yet-authorized) records the synthetic connected witness on the existing engine-neutral search seam and the 2026-11-17 foundation acceptance. This does not authorize production enablement or make the proposed gateway routes/forwarded headers part of the daemon wire. |

**Change since that capture ([#149](https://github.com/cybersader/kwiry/issues/149)):** Search errors no longer quote query text, filter values or keys, cursor text, or JSON input. The governed `501 mode_unavailable` message now says that `mode` is required and must be `lexical`. Request handling, statuses, and error codes are unchanged.

## Live fixtures

[Fixtures](fixtures/) are real HTTP/1.1 exchanges from the deployable Linux binary launched on loopback with synthetic notes, IDs, signing keys and configuration. `request.authorization` describes the capability scenario, not bearer bytes; string `request.body` denotes raw malformed JSON and `null` denotes no body. Fixtures retain the observed response headers and body.

The [integration test](../../../daemon/crates/kwiry/tests/support/governed_search_wire.rs) validates the live bodies and checks the status, headers and body against the recorded fixture; only the HTTP date, scoped generation digest and extraction-policy fingerprint are normalized for comparison. It captures a stale retained hit by making a note subdirectory unreadable; that case is skipped only if the test process can still read a mode-000 directory. The `reconciling` header, non-default freshness basis, PDF/Excel locator variants and unexpected `500 internal_error` are source-derived rather than fixture-proven. A separate `reconciling` capture would depend on catching an in-flight pass; the test does not rely on that race.
EOF