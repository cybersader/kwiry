# Governed MCP and evidence roadmap

This page expands IG-3A and IG-3B. Work is tabled while D5B has implementation priority and remains downstream of the governed retrieval gates.

## Purpose

Give agents a small read-only retrieval surface without creating a second policy engine or bypassing OpenClast identity, delegation, grants, gateway, or audit.

## Proposed slices

### Slice 1 — governed search

- mirror the project-owned search request/result model;
- reuse the same authorization context and server-side handler as HTTP;
- expose only modes proven for the active OpenClast gate;
- return explicit unavailable/authorization errors.

### Slice 2 — scoped evidence/chunk retrieval

- fetch only chunks from exact authorized resources;
- bind evidence to the originating result/resource;
- recheck identity at hydration;
- prevent arbitrary global chunk enumeration.

### Slice 3 — scoped status

- expose authorization-safe aggregates needed by callers;
- omit global corpus, resource, model, or operator detail not authorized for the subject;
- define privacy-minimized health versus status behavior.

## Transport and authority

- desktop agents may eventually use stdio under the desktop OS-user boundary;
- remote enterprise agents reach an OpenClast-mediated Streamable HTTP boundary;
- the browser/model never receives the internal Kwiry address or capability;
- OpenClast owns identity, delegation, grants, capability issuance, gateway, and audit;
- Kwiry owns retrieval enforcement.

## Deliberate exclusions

No note writing, synchronization, vault registration, rebuild, key management, user/group administration, policy interpretation, unscoped status, or independent MCP entitlement system.

## Evidence envelope

The shared envelope should eventually carry source identity, requested/effective mode, bounded contribution/provenance information, partial-failure state, and links suitable for the calling surface. Cursor semantics wait until deterministic multi-leg behavior is specified.

## Gate

The owner must choose the minimum useful slice before implementation. Each slice requires authorization-before-retrieval tests and a cross-surface proof that HTTP and MCP share enforcement behavior rather than merely similar code.
