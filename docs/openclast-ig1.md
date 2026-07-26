# OpenClast identity-governed lexical search (IG-1)

IG-1 connects an OpenClast control plane to a Kwiry sidecar without giving browsers a Kwiry credential or address.

## Status

The Kwiry-side capability, profile, partition, and authorized-BM25 enforcement checkpoint is committed. The companion OpenClast gateway and real-process two-user/two-room witness are implemented and verified in the sibling project's pending review work; owner acceptance and publication remain pending. This page is the integration/operator target, not a claim that the complete cross-repository path is released today.

```text
authenticated OpenClast request
→ live OpenClast grant resolution
→ <=60-second search-only capability
→ Kwiry OpenClast profile
→ authorized resource partitions and BM25 statistics
```

This checkpoint serves **lexical search only**. Governed semantic/hybrid search, status, chunk fetch, rebuild/admin HTTP, MCP, and revocation epochs are later verticals.

## Security model

- OpenClast is the identity and entitlement policy decision point.
- Kwiry is a deny-by-default retrieval enforcement point.
- Every grant is an exact `{ tenant_id, vault_id, room_id }` tuple.
- OpenClast uses a dedicated search signing key. It must not reuse the session/CRDT signing key.
- Kwiry receives only the public search JWKS.
- The browser receives only the OpenClast response; the internal capability remains server-side.
- Kwiry opens and scores only authorized physical partitions. BM25 statistics are aggregated only across those partitions.

## Generate separate keys

The companion OpenClast integration's multi-user key script is expected to generate independent session and Kwiry-search pairs:

```bash
cd deploy/multi-user
bash gen-keys.sh
```

The search files are:

```text
config/kwiry-search-key.pem       # private: OpenClast only
config/kwiry-search-jwks.json     # public: Kwiry only
```

For another deployment system, generate an Ed25519, P-256, or RSA keypair and publish a JWKS with a nonempty `kid`, `use: "sig"`, `key_ops: ["verify"]`, and matching `alg` (`EdDSA`, `ES256`, or `RS256`).

## Configure Kwiry

Each registered tree is one exact OpenClast resource. `vault_id` must match the OpenClast room-to-vault mapping.

```toml
version = 1

[server]
profile = "openclast"
bind = "0.0.0.0:32189"

[auth.openclast]
tenant_id = "example-tenant"
issuer = "openclast-search"
audience = "kwiry-search"
jwks_file = "/run/secrets/kwiry-search-jwks.json"
max_token_ttl_seconds = 60

[[vaults]]
vault_id = "engineering"
path = "/srv/knowledge/engineering"
room = "folder-engineering"

[[vaults]]
vault_id = "handbook"
path = "/srv/knowledge/handbook"
room = "folder-handbook"
```

Build the disposable partitions, then start the sidecar:

```bash
kwiry --config /etc/kwiry/config.toml --data-dir /var/lib/kwiry index
kwiry --config /etc/kwiry/config.toml --data-dir /var/lib/kwiry serve
```

IG-1 originally introduced index format 4. The durable-generation checkpoint advances the manifest to version 2, the generation layout to version 2, and the index format to version 5. Existing data roots require a one-time `kwiry index` rebuild before `serve`; the index is disposable and source files remain authoritative.

OpenClast mode:

- does not create or read a desktop token;
- does not write `connection.json`;
- may bind to a non-loopback internal address;
- rejects `serve --semantic`;
- live-reloads supported vault registration changes, while profile, bind, authentication, semantic, or configuration-version changes mark the daemon degraded and require a restart;
- exposes public `GET /v0/health` and capability-protected `POST /v0/search` only.

Keep the Kwiry listener on an internal network reachable by OpenClast, not by browsers or the public internet.

## Derived-state storage and freshness

Source roots may be read-only local mounts or SMB/NFS-backed materialized trees. The Kwiry `--data-dir` must be a separate machine-local/local-block path with reliable exclusive locking, atomic rename, and durable flush semantics. Do not place it under the network source root and do not share one data root between sidecar replicas.

The approved durability model keeps strong content hashes authoritative while allowing three explicit reconciliation bases:

- `strict_hash`: verify every source byte before declaring the generation current;
- `metadata_audit`: use size/mtime as a fast path, immediately hash changed/racy entries, and continuously verify bounded deterministic audit batches;
- `producer_manifest`: use a complete versioned SHA-256 manifest emitted beside an already-approved materialized root.

A previous complete generation may answer authorized searches while reconciliation runs. Successful Kwiry responses disclose `X-Kwiry-Index-Freshness: <state>; basis=strict_hash` and the exact immutable `X-Kwiry-Generation`; the OpenClast gateway may forward equivalent safe headers. `stale` or `reconciling` never widens the signed resource set, opens an unauthorized partition, or implies that search is current.

The current implementation checkpoint includes crash-consistent candidate publication, startup recovery, bounded retention, immutable reconciliation generations, the local data-root suitability probe, and a shared reconciliation plan used by desktop and OpenClast partitions. The default `strict_hash` basis reads and hashes every discovered source; the optional `[indexing] basis = "metadata_audit"` configuration reuses settled metadata-equal sources without byte reads while immediately hashing new, changed, reclassified, or racy entries and continuously re-verifying a bounded deterministic audit batch. Authorized partition readers are opened lazily on first authorized use and cached for the lifetime of the immutable generation; unauthorized partitions are never preloaded, and a generation swap discards the cache. The daemon still performs eager startup reconciliation when a generation pre-exists (a freshly built first generation skips the redundant boot pass); path-scoped watching and bind-first service remain later checkpoints. Neither branch implementation nor passing tests is a delivered or owner-accepted claim.

## Configure OpenClast companion integration

Once the companion changes are committed, set the internal endpoint, trust tuple, separate private key, and explicit resolver-room mapping:

```dotenv
OPENCLAST_KWIRY_URL=http://kwiry:32189
OPENCLAST_KWIRY_TENANT_ID=example-tenant
OPENCLAST_KWIRY_SEARCH_KEY=/config/kwiry-search-key.pem
OPENCLAST_KWIRY_SEARCH_ISSUER=openclast-search
OPENCLAST_KWIRY_SEARCH_AUDIENCE=kwiry-search
OPENCLAST_KWIRY_SEARCH_KID=openclast-kwiry-search
OPENCLAST_KWIRY_RESOURCE_MAP={"folder-engineering":"engineering","folder-handbook":"handbook"}
OPENCLAST_KWIRY_SEARCH_TTL_SECONDS=60
OPENCLAST_KWIRY_MAX_LIMIT=100
OPENCLAST_KWIRY_TIMEOUT_MS=10000
```

`OPENCLAST_KWIRY_RESOURCE_MAP` is not an entitlement source. OpenClast first resolves the caller's current grants, then intersects the resulting rooms with this operator map. Unknown rooms are omitted; zero mapped resources returns `403`.

Clients call OpenClast:

```http
POST /api/knowledge/search
Authorization: Bearer <OpenClast session credential>
Content-Type: application/json

{
  "q": "incident response",
  "mode": "lexical",
  "limit": 20
}
```

The gateway requires explicit `mode: "lexical"`, re-resolves grants on every request, mints a short-lived internal capability, and proxies the safe Kwiry response. The approved durability follow-on also forwards only the safe freshness/generation response headers; it never exposes index paths, global counts, or the internal capability.

## Audit and privacy

When `OPENCLAST_AUDIT_DIR` is enabled, search events are appended to `knowledge-search.jsonl`. Events may include subject identity, authentication method, decision/JTI, authorized resource identifiers, backend status, result count, outcome, and latency.

The gateway and Kwiry enforcement logs do **not** record:

- bearer or capability token bytes;
- raw query text;
- note content or excerpts;
- browser-visible Kwiry credentials.

## Failure behavior

| Condition | Result |
|---|---|
| Missing or invalid OpenClast authentication | `401` |
| No currently granted mapped resource | `403` |
| Semantic or hybrid mode | `501 mode_unavailable` |
| Cursor supplied | `501 cursor_unavailable` |
| Invalid request or filters | `400` |
| Kwiry unavailable | `502 backend_unavailable` |
| Kwiry timeout | `504 backend_timeout` |
| No complete generation yet | nonrevealing gateway failure until Kwiry can serve; no partial corpus |
| Previous complete generation reconciling | successful authorized response with explicit stale/reconciling freshness header |
| Invalid/expired internal capability at Kwiry | nonrevealing gateway failure; never desktop fallback |

## Verification

The committed Kwiry repository tests cover capability key/claim validation, profile separation, no desktop credential artifacts, exact resource partitions, and authorized-only BM25 statistics. The sibling integration tests cover live grant resolution, gateway token non-disclosure, audit privacy, and the gated real-process two-user/two-room witness. Passing both establishes implementation verification, not owner acceptance.

Run the cross-repository witness against a built Kwiry binary:

```bash
KWIRY_BIN=/path/to/kwiry \
  bun test --cwd=/path/to/openclast/orchestrator test/kwiry-cross-repo.test.ts
```
