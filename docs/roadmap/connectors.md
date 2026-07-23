# Connector materialization roadmap

Connectors are tabled while D5B has implementation priority. This page preserves the intended boundary and the questions that must be answered before selecting a first deep connector.

## Boundary

A connector synchronizes a source system into deterministic files plus versioned provenance/ACL metadata. Kwiry indexes the materialization; it does not embed source-specific API clients or entitlement policy into retrieval core.

A synchronization credential authorizes acquisition. It does not grant end-user search access. Enterprise permissions must be normalized into exact governed resources before retrieval.

## Materialization contract

A connector vertical must define:

- stable source and object identity;
- deterministic paths/content and collision handling;
- provenance, source URL, version, and deletion tombstones;
- delta cursor/checkpoint semantics and replay-safe reset;
- attachment/content extraction boundaries;
- rate limits, backoff, partial failure, and observability;
- secret storage and log redaction;
- schema/version migration and rebuild behavior.

## ACL normalization

The first governed connector must exercise real permission complexity:

- inherited versus direct grants;
- groups and current membership;
- moves between protected containers;
- sharing/revocation priority;
- unmapped or unsupported policy failing closed;
- mapping into exact `{ tenant_id, vault_id, room_id }` resources without an accidental Cartesian product.

## Proposed sequence

1. owner selects one source using value, API/licensing, representative data, delta model, and ACL complexity;
2. build a local non-governed materializer and deterministic rebuild witness;
3. add versioned provenance and deletion/reset recovery;
4. design and owner-review ACL normalization;
5. prove revocation and asymmetric access through OpenClast;
6. generalize only seams demonstrated by the first connector.

## Non-goals

No generic connector framework before one deep vertical, no connector-defined search authorization, no cloud content as the only durable truth, and no silent flattening of permissions that cannot be represented safely.
