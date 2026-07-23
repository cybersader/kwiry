# Governed semantic and hybrid retrieval roadmap

This page expands IG-2. Work is tabled while D5B has implementation priority and remains blocked on explicit IG-1 owner acceptance.

## Goal

Expose semantic and hybrid retrieval through OpenClast without letting forbidden vectors, corpus statistics, or documents influence authorized candidates, scores, fusion order, hydration, evidence, caches, or timing-sensitive state.

## Security oracle

Every result must match an authorized-only physical baseline. Candidate generation over a broader corpus followed by filtering is prohibited.

The design review must compare:

- physical vector partitions by exact `ResourceKey`;
- a mandatory authorized rowset mechanism with equivalent noninterference evidence;
- memory, fan-in, latency, revocation, compaction, and operational tradeoffs.

## Vertical sequence

### IG-2A — authorized vector baseline

- define partition/opening behavior;
- prove forbidden vectors cannot alter authorized nearest neighbors;
- bind model/version and vector schema to status and rebuild behavior;
- preserve lexical availability when semantic initialization or querying fails.

### IG-2B — authorized fusion

- fuse only authorized lexical and semantic lists;
- define deterministic tie behavior and limits;
- compare results with an authorized-only physical fixture;
- exclude forbidden-resource influence from evidence and metrics.

### IG-2C — hydration and race semantics

- recheck exact resource identity during hydration;
- specify policy/subject changes during a request;
- decide fail/partial/retry behavior before exposing it;
- isolate caches and async work by authorization context.

### IG-2D — truthful evidence

- show requested and effective modes;
- describe lexical, semantic, and fused contribution without corpus leakage;
- include provenance/source links and bounded partial-failure semantics;
- avoid presenting generated explanations as authorization or canonical fact.

## Required tests

- asymmetric two-user/two-room vector and hybrid witnesses;
- forbidden-partition add/remove invariance;
- cross-resource fan-in and deterministic tie tests;
- model failure with lexical isolation;
- hydration mismatch and authorization-change cases;
- cache and status non-disclosure;
- latest-binary cross-repository OpenClast witness.

## Owner gate

Implementation begins only after IG-1 acceptance and owner selection of an authorization design that satisfies the physical baseline. Local desktop semantic capability does not imply enterprise authorization completeness.
