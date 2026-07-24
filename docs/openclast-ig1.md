# OpenClast identity-governed lexical search (IG-1)

IG-1 connects an OpenClast control plane to a Kwiry sidecar without giving browsers a Kwiry credential or address.

## Status

The Kwiry-side capability, profile, partition, and authorized-BM25 enforcement checkpoint is committed. The companion OpenClast gateway changes and cross-repository witness described below remain pending in the sibling project, so this page is the integration/operator target rather than a claim that the complete cross-repository path is published and deployable today.

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

IG-1 introduces index format 4. Existing data roots created by an earlier Kwiry format require a one-time `kwiry index` rebuild before `serve`; the index is disposable and source files remain authoritative.

OpenClast mode:

- does not create or read a desktop token;
- does not write `connection.json`;
- may bind to a non-loopback internal address;
- rejects `serve --semantic`;
- live-reloads supported vault registration changes, while profile, bind, authentication, semantic, or configuration-version changes mark the daemon degraded and require a restart;
- exposes public `GET /v0/health` and capability-protected `POST /v0/search` only.

Keep the Kwiry listener on an internal network reachable by OpenClast, not by browsers or the public internet.

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

The gateway requires explicit `mode: "lexical"`, re-resolves grants on every request, mints a short-lived internal capability, and proxies the safe Kwiry response.

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
| Invalid/expired internal capability at Kwiry | nonrevealing gateway failure; never desktop fallback |

## Verification

The committed Kwiry repository tests cover capability key/claim validation, profile separation, no desktop credential artifacts, exact resource partitions, and authorized-only BM25 statistics. Live grant resolution, gateway token non-disclosure, audit privacy, and the gated real-process two-user/two-room witness belong to the pending companion OpenClast integration and are not part of the committed C1 baseline.

After those companion changes are committed, run the cross-repository witness against a built Kwiry binary:

```bash
KWIRY_BIN=/path/to/kwiry \
  bun test --cwd=/path/to/openclast/orchestrator test/kwiry-cross-repo.test.ts
```
