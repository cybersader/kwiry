# Agents and structural retrieval roadmap

This page preserves later IG-6 direction while near-term implementation focuses on D5B and governed semantic/MCP foundations.

## Agent access

Remote enterprise agents should use OpenClast-mediated retrieval. OpenClast owns workload/subject identity, delegation, current grants, capability issuance, gateway, and audit. Kwiry verifies the materialized capability and constrains retrieval before candidates exist.

Future agent work may include:

- represented subject versus current actor and delegation-chain evidence;
- proof-bound workloads and audience/action constraints;
- external IGA/SCIM/access-review seams where owner value justifies them;
- quotas, revision/epoch behavior, cancellation, and privacy-minimized audit;
- interoperability through small governed retrieval tools rather than broad administration.

## Structural retrieval

Authored structure may improve retrieval:

- Markdown headings and section containment;
- wikilinks and explicit references;
- paths, folder ancestors, and selected properties;
- connector provenance and owner-supplied relationships.

Derived graph indexes remain disposable projections. Model-inferred entities or relationships are suggestions, not the sole durable owner of a fact, entitlement, or navigation edge.

## Authorization requirement

Structural traversal must satisfy the same oracle as text/vector retrieval:

- only authorized nodes/edges are opened or traversed;
- forbidden structure cannot alter authorized candidates, paths, scores, or explanations;
- hydration rechecks exact resource identity;
- caches and precomputed neighborhoods remain authorization-safe;
- post-traversal filtering is not the access-control mechanism.

## Staged direction

1. improve authored local structure signals with judged retrieval tests;
2. define project-owned structural models and disposable projection versions;
3. prove authorized traversal on exact physical fixtures;
4. expose only bounded read-only evidence through governed surfaces;
5. evaluate inferred suggestions separately from canonical retrieval truth.

## Non-goals

Kwiry does not become an identity system, policy engine, autonomous note writer, durable agent-memory authority, or canonical inferred knowledge graph.
