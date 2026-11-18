# IG-2 S1a/S1b — scoped lexical headers and identity-free context

This implementation closes a cross-room activity signal: the OpenClast profile no longer exposes a whole-instance generation or freshness state to a caller authorized for only some rooms. It also stops retaining or logging capability subject and actor identities in Kwiry.

**Review gate:** this implements proposed contract amendments **A2/A4**, which require owner approval before merge. `CONTRACT.md` is unchanged. The connected **IG-1 witness re-run is pending**; the local regression tests do not replace it. S2 and later slices, custody confirmation, retirement, and semantic enablement are not implemented here. The authoritative slice and cutover requirements are [IG-2A §12–§13](https://github.com/cybersader/kwiry/blob/592800484ddcf020476bcfc240cf61b322dc2c99/docs/roadmap/ig2a-governed-semantic-design.md#12-slices).

## OpenClast behavior

- `X-Kwiry-Generation: v1-<hex>` describes the effective authorized resource set (the signed resources narrowed by the request's vault/room filters), never a whole-instance publication ID. An empty effective set has a deterministic empty-set digest and `current` freshness.
- Resource tuples sort bytewise by `(tenant_id, vault_id, room_id)`. The SHA-256 input starts with mode class `L`, followed by the resource count and each tuple plus its transitional revision. Counts and byte-string lengths are u64 little-endian.
- The transitional revision is SHA-256 over a framed `kwiry-resource-revision-v1` domain, the five manifest-level versions as u64 little-endian, and the resource's source-key-ordered manifest rows. Every key and compact deterministic serde JSON row is separately length-framed. This includes every persisted `ManifestFile` field, not just file content hashes. Global publication IDs, sync timestamps, and state revisions do not participate.
- An mtime-only change reindexes the governed document so indexed mtime, range-query results, and generation identity remain consistent. Desktop's existing mtime reconciliation behavior is unchanged.
- Freshness is maintained per resource: a watcher event makes only affected vaults stale; covering passes move those resources through reconciling to current, or stale if incomplete, failed, or dirtied again. Full passes cover all configured resources. Responses report the worst state in their effective set and the configured evidence basis. A response from a superseded authorized revision is at least stale; an unmapped effective resource is refused, not silently skipped.
- A metadata-audit backlog is not itself dirty state: metadata reuse is already declared by `basis=metadata_audit`. Treating the shared audit budget as per-room incompleteness would reintroduce the forbidden-room signal.
- Desktop takes an explicit profile branch, including its `resources: []` principal. Its generation and freshness headers retain their prior meaning.

## Capability transition and operator cutover

The verified principal retains expiry, the exact action, JTI, resource tuples, limit, scopes, and the existing policy revision. It has no subject, actor, or subject revision. Repeated vault IDs are refused, including when their room tuples differ. Both accepted shapes retain the existing issuer, audience, TTL, key ID, signature, and algorithm checks.

The optional `[auth.openclast]` settings are:

```toml
# Exact bytewise workload subjects only. Never add user identities.
workload_subject_allowlist = ["workload:synthetic-search"]
# legacy_identity_accept_until = <coordinated Unix-seconds cutoff>
```

1. Deploy the S1b-capable release everywhere. With the cutoff unset, today's legacy shape remains accepted; an empty allowlist enables no workload subjects.
2. OpenClast switches its issuer to workload-shaped capabilities and confirms `T_switch`.
3. Only then set `legacy_identity_accept_until` to `T_switch + max_token_ttl_seconds + clock_skew_allowance` (the design's defaults are 60 seconds plus 30 seconds). Authentication configuration changes require a daemon restart, as before; arrange that restart to activate the cutoff.
4. Before that cutoff, legacy `sub` and `actor` must be present, distinct, and nonempty; they are validated and discarded. At and after it, the legacy shape returns 401. Workload tokens require an exactly allowlisted `sub`, with neither `actor` nor `subject_revision` present.

Workload tokens reject unknown claims at every signed object level and reject explicitly null identity claims. The legacy branch keeps its previous parsing behavior, including ignoring unknown fields, until cutover. No unknown field is retained in the verified context or logged. Workload allowlist configuration is an operator trust decision; Kwiry cannot infer whether an arbitrary subject string represents a person.

## Regression evidence

The tests cover forbidden publication, reconciliation, removal/re-addition, full rebuild and actual notify-watcher events; authorized-only physical baseline equality under a contended metadata-audit budget; freshness transitions, dirty-during-pass epochs, superseded responses, unmapped resources, empty effective sets, and tuple ordering; all persisted manifest fields and five versions; mtime-only range-query/result identity consistency; and unchanged desktop HTTP headers with the empty-resource desktop principal.

Capability tests cover both shapes, the configured cutoff and unset-cutoff compatibility, bytewise allowlisting, repeated vault IDs, closed claim parsing, and the shared verification restrictions. A real daemon test captures audit output after accepted legacy/workload requests and a rejected bearer, then asserts that neither identity values nor subject/actor fields survive in the logs.
