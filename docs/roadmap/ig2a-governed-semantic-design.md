# IG-2A design — governed semantic and hybrid retrieval

Status: **design, revision 2, owner review pending.** Nothing here is implemented or approved. This page expands [IG-2](governed-semantic-hybrid.md) and is bound by [`CONTRACT.md`](../../CONTRACT.md), which this design does not edit. The contract amendments it needs are listed in [§12](#12-contract-amendment-package-a1a6).

Revision 2 answers an independent read-only review of revision 1 (`1442e668`). The review rated the design NO-SHIP for implementation readiness. It judged the core approach sound and confirmed the code claims. Every finding maps to a section here:

| Finding | Where it is resolved |
|---|---|
| 1. A pinned generation does not keep its files alive | §4.1–§4.3: independently leased immutable partition objects, barrier tests |
| 2. Purge protocol is neither durable nor idempotent | §4.8: retirement journal, revoked vs purge-completed, crash recovery; A6 |
| 3. Queue order changes authorized errors | §4.7 fair scheduler; the stated limit in A5 and §9; new witness in §11.2 |
| 4. Slice order breaks lexical isolation | §11: scoped headers and capability context ship first; everything else behind a non-default build gate until enablement |
| 5. Map-wide lexical reader mutex | §4.3: per-resource init cells; test in S2 |
| 6. Descriptive-only embedding identity; history-dependent batching | §4.5: artifact-hash fingerprint, canonical order, fixed batches, whole-source reuse, mandatory real-ONNX gate |
| 7. Coverage integrity; silent `zip` truncation | §4.6: exact inventory validation before attach |
| A2 / A3 / A4 / witness / recommendation corrections | §12, §5.3, §6.5, §11.2, §2 |

Owner research request, scheduling and work units: [§10](#10-research-indexing-order-and-work-units-options-not-decisions).

## 1. The idea in one paragraph

Governed semantic search follows the IG-1 lexical model. **Every room has its own immutable stores — one lexical, one vector — and a request leases and opens only the stores of the rooms it is signed for.** A vector store is derived from exactly one lexical store and records that store's identity, so the two can never describe different states of a room. A query embeds the question once, runs an exact nearest-neighbour scan inside each authorized room, and merges the per-room lists. The merge sorts by distance, then chunk ID, independent of rowids. Hybrid fuses that list with the IG-1 authorized lexical list using RRF (`k = 60`). Forbidden rooms are never opened. Forbidden-room retirement cannot invalidate an authorized request's stores. The results a user receives are byte-equal to those of a separate instance that holds only the authorized rooms. **Stated limit:** that equality covers results, not timing. How soon a room becomes semantically searchable depends on shared server load, including indexing work for rooms the caller cannot see.

## 2. Decisions

### 2.1 Recorded owner decisions (not reopened)

- **D1–D4 (2026-11-17).**
  - D1: retrieval mode is selectable per request, and an unavailable mode is an explicit error.
  - D2: the default mode is lexical.
  - D3: the audit records only a keyed query fingerprint, never query text.
  - D4: no user IDs reach the engine, not even pseudonymous ones. Caches are keyed only by the authorized-set digest, or there are none.
- **Semantic readiness scheduling (2026-11-17).** A **fair, shared indexing scheduler** with an explicitly stated limit:
  - results are exactly invariant to the forbidden corpus;
  - how soon a room becomes searchable may depend on server load, including load from rooms the caller cannot see.
  
  This design never claims unconditional forbidden-corpus invariance. The limit is written into A5 and the threat model (§9).
- **Engineering choices, fixed by the brief:**
  - physical per-resource partitions;
  - exact KNN;
  - deterministic merge;
  - candidate caps applied only after the authorization boundary;
  - any later ANN must re-prove baseline equality before it is adopted;
  - the existing whole-corpus desktop KNN path stays desktop-only.

### 2.2 Remaining owner decisions

Each comes with a recommendation.

1. **Approve the amendment package A1–A6 (§12).**
   - A1: semantic and hybrid in the governed profile, with an explicit mode.
   - A2: capabilities name a workload, never a person.
   - A3: a readiness probe scoped to the caller's own rooms.
   - A4: generation and freshness headers that describe only the caller's rooms.
   - A5: the readiness-timing limit.
   - A6: what "purged" means and when Kwiry may say it.
   
   *A2 and A4 also change the existing lexical endpoint, so they ship first (§11).*
2. **Change IG-1's on-disk layout to independently leased per-room objects (§4.1).** This is what lets retiring room B never break an in-flight request for room A. It needs a one-time rebuild of disposable indexes and a re-run of the IG-1 witness rows. *Recommendation: yes.*
3. **When a room's vectors lag its text, refuse rather than serve the previous version.** A coherent stale answer is possible: an older snapshot whose lexical and vector stores agree. Serving it would mean keeping and leasing older per-room objects, which fights the purge guarantee and needs its own staleness disclosure. *Recommendation: refuse for IG-2A and revisit later.*
4. **Pay the RAM for a dedicated query embedder.** It is now a prerequisite for enabling HTTP (§11). It removes the worst timing channel, but not all of them (§9). *Recommendation: yes. Measure the cost in S7.*
5. **Reject non-room filters (`path_prefix`, `tags`, `frontmatter_equals`) for semantic and hybrid in IG-2A.** Reject them before embedding and before any store opens. Pre-scan filtering is a later slice. *Recommendation: yes.*
6. **Keep a minimal tombstone for every retired room forever, and refuse to re-index that exact resource.** OpenClast already issues fresh IDs on recreation, so a re-appearing tuple can only be a configuration error. *Recommendation: yes. The tombstone holds only the partition ID digest and the purge state.*

## 3. What exists today, and why it cannot be reused

Read from `origin/main` at `74551c1` and confirmed by the independent review:

- **Desktop vectors live in one mutable store outside the generations.** It is `<data-dir>/semantic/semantic.db`, mutated in place after each lexical publish and holding the whole corpus. Contract §5 prohibits that pattern for governed use.
- **The governed path has no semantic leg.** `OpenClastIndexManager::reconcile` passes `semantic: None`, and openclast mode rejects `serve --semantic`.
- **Desktop KNN ties depend on insert history.** vec0 `MATCH … k = ?` picks among equal distances at the k boundary according to its internal rowid and chunk layout.
- **Desktop hydration drops silently.** `hydrate_ordered` skips candidates it cannot find.
- **`embed_and_replace_source` zips chunk IDs with vectors.** A short embedder output therefore truncates silently.
- **`EmbeddingProfile::fingerprint()` is descriptive.** It hashes names and settings, not the model, tokenizer or configuration bytes actually loaded.
- **One map-wide mutex covers IG-1 partition readers.** `PartitionedSearchIndex::authorized_reader` holds it while opening a cold partition, so a cold open of room B blocks room A.
- **Snapshot file lifetime is not protected.** A request pins a generation with `ArcSwap::load_full()`, but that keeps only the in-memory struct alive. `prune_generations` can delete the directory before the request lazily opens a partition. Today that surfaces as `index_building`. Under IG-2A, retirement-driven pruning would turn it into an authorized-request failure caused by a forbidden room.
- **Existing IG-1 leaks:**
  - `X-Kwiry-Generation` is global and embeds a nanosecond timestamp.
  - `X-Kwiry-Index-Freshness` reads daemon-wide `rebuilding` and `dirty` flags.
  - Logs record `sha256(subject)` (unkeyed) and `actor` in plain text.
- **The verified request context drops claims.** `Principal` keeps neither `exp` nor the exact action, which collapses to `Scope::Search`. Any expiry or action recheck therefore needs new plumbing.
- **Chunk IDs hash `vault_id` and path but not the room.** A reclassified vault can reuse chunk IDs under a new resource, so retirement tests must reason about resources and artifacts, not chunk IDs.
- **What carries over unchanged:**
  - `embedding_text`;
  - `embedding_source_identity`;
  - `rrf_fuse_traced` (BTreeMap accumulation, `total_cmp`, `chunk_id` tie-break);
  - `validate_document_resource`;
  - the IG-1 authorized lexical-v2 path.

## 4. Storage, lifetime and indexing

### 4.1 Independently leased immutable partition objects

IG-1 copies a whole generation directory per publish, with every partition inside it. IG-2A replaces this with **per-partition immutable objects** referenced by small generation records:

```text
<data-dir>/
  current.json
  generations/<generation-id>/
    manifest.json
    layout.json             # layout_version 3: resource → { lexical_object, vector_object? }
  objects/<partition_id>/
    lex-<object_id>/        # Tantivy index for one resource, immutable once published
    vec-<object_id>/        # vectors.sqlite + vectors.json, immutable once published
  staging/<partition_id>/   # in-progress builds for that resource only
  retirements/<partition_id>.json
  models/                   # model artifacts only; never corpus data
```

- `object_id` is a fresh random identifier assigned when the object is published. The object's descriptor records its content digest. IDs are never reused.
- A publish writes only the objects whose content changed, then writes a new generation record that references objects. Unchanged rooms are referenced, never copied. This also removes the copy cost noted in revision 1.
- Objects never change after publication. Readers open vector stores as `file:…?mode=ro&immutable=1`, so SQLite creates no journal, WAL or SHM file.
- The openclast profile never creates `<data-dir>/semantic/`.

### 4.2 What a generation means now

A generation is an atomic, immutable **mapping from resources to objects**, plus the manifest. `current.json` names the active generation, exactly as today. Prior generations are retained for crash recovery (`RETAINED_GENERATIONS`). Retaining a generation keeps its objects referenced, except objects of retired resources (§4.8).

### 4.3 Lifetime protocol: leases

**Rule: an object may be deleted only when no retained generation references it and no lease holds it.** Retirement is the one exception to the first condition; the second always applies.

- **Lease registry.** An in-process registry maps object ID to lease count, guarded by one short mutex that is never held across I/O.
- **Pin and lease, atomically.** Under the registry mutex, a request:
  1. loads the active generation;
  2. selects the signed resources that survive vault/room narrowing;
  3. takes a lease on exactly those resources' lexical objects, plus their vector objects for semantic and hybrid;
  4. releases the mutex.
  
  The lease set depends only on the request's own resources. A forbidden room's objects are never leased by this request.
- **Deleters respect leases.** Pruning and retirement check the registry under the same mutex. A leased object is marked *doomed* rather than deleted. The last lease release deletes it, fsyncs and records the result. A doomed object can never be leased again.
- **Opening.** Readers open after the lease is taken, outside every global lock. Retiring room B therefore cannot invalidate any open room A has pending, because A's objects are never doomed by B's retirement.
- **Bounded lease time.** Kwiry enforces a hard per-request deadline: the capability's remaining lifetime, capped at a configurable maximum (default 30 s). A request past its deadline is refused and its leases are released, so purge completion is bounded.
- **Lexical reader cache (review finding 5).** The cache maps object ID to a **per-object init cell**. The global map lock is held only to find or insert the cell. The open itself runs inside that one cell, outside the map lock. A failed open leaves the cell empty for a retry. The cache sits behind leases: a cached reader is reused only while its object is leased by the current request. Doomed objects drop their cells.

**Barrier tests (S2), injected through test-only hooks at four points: *pin*, *embed*, *open* and *hydrate*.** At each barrier, while request A waits:

- retire forbidden room B;
- run ordinary retention pruning;
- publish a new generation that changes room A.

In every case A completes byte-equal to its pinned snapshot. B's retirement proceeds without waiting for A. A's objects are deleted only after A releases them, and only if they are no longer referenced.

A separate test blocks room B's reader init and asserts that A's cached access to its own room completes.

### 4.4 Vector objects and readiness

`vec-<object_id>/vectors.sqlite` holds one plain table. vec0 is not used, so that layout and rowids cannot affect results:

```sql
CREATE TABLE vectors (
  chunk_id        TEXT PRIMARY KEY,
  source_key      TEXT NOT NULL,
  source_identity TEXT NOT NULL,   -- embedding_source_identity(content_hash, format)
  embedding       BLOB NOT NULL    -- little-endian f32 × dimensions
) WITHOUT ROWID;
```

`vectors.json`:

```jsonc
{
  "vector_schema_version": 1,
  "partition_id": "<hex>",
  "lexical_object_id": "<id>",       // the exact lexical object these vectors derive from
  "embedding_identity": "<hex>",     // §4.5
  "inventory_digest": "<hex>",       // §4.6, computed from validated rows
  "chunk_count": 1234,               // computed from validated rows
  "vectors_sha256": "<hex>"
}
```

**Readiness** of a resource in the pinned generation is computed per request from immutable inputs. A resource is `ready` only if:

- the layout maps it to a vector object;
- that object's `lexical_object_id` equals the layout's lexical object for the same resource;
- `embedding_identity` equals the loaded query embedder's identity.

Otherwise it is `pending` (no matching vector object yet) or `unavailable` (no model, or the build failed under this identity). Only `ready` is served. Full inventory validation happens once, before attach (§4.6). It is not repeated per request.

### 4.5 Embedding identity and canonical batching (review finding 6)

`embedding_identity` is a SHA-256, with domain separation and length prefixes, over three groups:

- **Artifacts:** the bytes of `model.onnx`, `tokenizer.json`, `tokenizer_config.json`, `special_tokens_map.json` and `config.json` as actually loaded. Kwiry hashes the files fastembed opens from `models/` and refuses to start the semantic path if any is missing.
- **Preprocessing:**
  - `embedding_input_version` (the `embedding_text` shape);
  - query and passage prefixes;
  - truncation length (512);
  - pooling (CLS);
  - L2 normalization;
  - `vector_schema_version`;
  - distance (`l2_normalized`).
- **Execution policy:**
  - fastembed and ONNX Runtime versions;
  - execution provider (CPU only);
  - fixed intra-op and inter-op thread counts;
  - graph optimization level;
  - batch size `B = 8`;
  - padding policy (pad to the longest item in the batch).

Changing any of these makes every vector object non-ready. Lexical search is unaffected.

**Canonical order and batching.** Batch composition must be a function of the source alone. It must not depend on segment order, reuse history or other sources.

- Chunks of one source are ordered by `chunk_id` bytewise ascending. No chunk ordinal is stored, and Tantivy segment order is not canonical.
- Batches are consecutive groups of `B` in that order, with the last one possibly short. A batch never spans two sources.
- Reuse works per source, never per chunk. A source's rows are reused only if its `(source_key, source_identity)` and the `embedding_identity` are unchanged, and then all of its rows are reused. Otherwise the whole source is re-embedded in canonical batches. A partially reused source cannot change any chunk's batch neighbours.

**Mandatory gate (S5) using real ONNX, not the hash embedder.** Vectors must be bitwise identical, and semantic and hybrid results byte-equal, across:

- fresh processes;
- a fresh build in a clean target directory;
- incremental rebuild after edits elsewhere in the room;
- the reuse path versus a full re-embed;
- builds that permute source discovery order.

If the gate fails, the fallback is batch size 1, re-gated. If that also fails, the baseline proof is re-scoped with the owner before any HTTP enablement.

### 4.6 Building and coverage integrity (review finding 7)

The builder reads chunk text from the **lexical object** it is deriving from (stored `content`, `heading_path`, `chunk_id` and `source_key`). The vectors therefore describe exactly what that object indexes, and the source tree is not re-read.

The expected inventory is the set of `(chunk_id, source_key)` pairs stored in that lexical object. Each source key is paired with `source_identity` computed from the manifest's `content_hash` and `format`. The manifest's per-source `chunk_count` must match the lexical object's per-source count.

**Exactly one embedding per expected chunk.** The embedder must return exactly as many vectors as the texts it was given. The `zip` is replaced by a length-checked pairing. A mismatch fails the whole source, and therefore the whole partition build.

**Validation before attach**, against the staged `vectors.sqlite`:

- the set of `(chunk_id, source_key, source_identity)` equals the expected inventory exactly;
- there is no missing, duplicate or unexpected row, and no row associated with the wrong source;
- every vector has the declared dimension count and finite components.

`chunk_count`, `inventory_digest` and `vectors_sha256` are then **derived from the validated rows**, never from the descriptor or from counters. Any failure discards the staged object and leaves the resource `pending`, or `unavailable` after repeated failure. The cause is recorded as a closed category.

**Attach** runs under the writer lock and is short:

- the layout must still map the resource to the same lexical object, and the resource must not be tombstoned;
- the staged directory is renamed into `objects/<partition_id>/vec-<new_id>/` and fsynced;
- a new generation record is published.

A stale build (the lexical object moved on meanwhile) is discarded. Its rows remain eligible for whole-source reuse in the next build, because they are keyed by `source_identity`.

### 4.7 Fair shared scheduler (owner decision; review finding 3)

Revision 1 drained a queue ordered by `ResourceKey`. A large forbidden room that sorted early would therefore change when room A stopped returning `semantic_not_ready`. That was an unstated, unbounded dependency. Revision 2 adopts the owner-decided policy:

- **Fairness.** Pending resources sit in a round-robin rotation. Each turn runs one **bounded work unit** (§10.2) for the resource at the head, then moves it to the tail. New and changed resources join at the tail. No resource is ever ordered by its key, size or demand.
- **Per-resource work bound.** A resource needing `u` units becomes ready after at most `u` rounds. Each round costs at most `P × U_max`, where `P` is the number of pending resources and `U_max` is the work-unit bound. Nothing else changes when it becomes ready.
- **What stays exact.** Results never depend on the scheduler. A resource serves only when its vector object matches its lexical object, and that content is a pure function of the room.
- **What is load-dependent (the stated limit).** When room A stops returning `semantic_not_ready` depends on `P` and on how long other resources stay pending, including rooms the caller cannot see. This is written plainly in A5 and §9. It is not described as invariance.
- **No load-dependent result changes.** Lexical search is unaffected by the scheduler. Semantic and hybrid either serve exact results or refuse.

### 4.8 Retirement and durable purge (review finding 2)

Two states are distinct and reported separately:

- **Retrieval revoked:** no new request can lease the resource's objects.
- **Purge completed:** every artifact of the resource is unlinked, the deletions are durable, and no lease remains.

**Protocol.** Each step is idempotent and journaled in `retirements/<partition_id>.json`. The journal is written atomically and fsynced, as are its parent directories.

1. **Record.** Write `{state: "revoking"}`. From this point the resource is **tombstoned**:
   - the builder must not create staging for it;
   - attach refuses it;
   - the scheduler drops its queued work units;
   - a running unit for it is cancelled cooperatively at the next batch boundary.
   
   Builder steps that create staging (`create`, `append`, `rename`) take the per-resource build lock and re-check the tombstone under it, so staging cannot be re-created after step 3.
2. **Revoke.** Publish a generation whose layout omits the resource. New requests can no longer lease its objects. In-flight requests that hold leases are refused by the end-of-request check (§7). Set `{state: "revoked"}`.
3. **Remove.** Doom every object under `objects/<partition_id>/`, including objects still referenced by retained generations. Delete `staging/<partition_id>/` and any staging candidate for this resource. Retained generation records that reference the resource keep only a reference. They are never served for it, and crash-recovery fallback treats the resource as retired there too.
4. **Drain and sync.** Once leases drain (bounded by the request deadline), unlink every doomed object. Fsync each parent directory. Then re-scan `objects/<partition_id>/` and `staging/<partition_id>/` to confirm they are absent.
5. **Complete.** Write `{state: "purged"}` and fsync. The journal shrinks to the minimal tombstone (decision 6).

**Failure.** Any deletion or fsync error leaves the state at `revoked` with a closed `purge_failed` category. Purge completion is never claimed. Retries back off. The condition shows in logs and the operator's retirement state, never in user-facing errors.

**Restart.** Before binding the listener, startup reads `retirements/` and resumes any state other than `purged` from its recorded step. It also deletes staging and objects belonging to tombstoned resources. Startup never serves while an earlier retirement is incomplete, which also covers a crash between publish and prune.

**Tests (S2, S4, crash-injected and interleaved):**

- a crash after each step 1–5;
- retirement while the builder is mid-unit, mid-validation or mid-attach for the same resource;
- an attach racing retirement;
- retirement while a lease on the resource is held;
- a deletion failure (permission fault injected), which stays `revoked` and never reports purged;
- restart recovery before serving;
- re-adding a tombstoned tuple, which is refused;
- recreation under fresh IDs, which inherits nothing.

## 5. Readiness, errors and HTTP mapping

### 5.1 Request gate order

The order is fixed. Steps 1–6 depend only on the request and its signed resources. No store opens and no embedding runs before step 6 completes.

1. Capability verification. The verified request context keeps `exp`, the exact action, `jti`, resources and `max_limit` (A2). On `/search`, the action must equal the requested mode (§6.5).
2. `mode` is required in the openclast profile. D2's default is applied at the OpenClast seam.
3. Validation of query, controls and filters:
   - lexical-v2 control rules from contract §10d.6 still apply;
   - **non-room filters are rejected for semantic and hybrid here, before embedding and before any open.**
4. Model loaded, with the expected `embedding_identity`? If not, `mode_unavailable`.
5. Pin and lease (§4.3). Every signed resource left after narrowing must be mapped in the layout, otherwise `resource_unavailable` (IG-1 behaviour).
6. Every such resource must be semantic-`ready` (§4.4), otherwise `semantic_not_ready`. Leases are released.
7. Execute: embed, then open, scan and hydrate. Any failure refuses the whole call.

### 5.2 Taxonomy

| Condition | Kwiry `code` | HTTP | OpenClast seam (peer-reported) |
|---|---|---|---|
| No model, wrong identity, build without `semantic-onnx`, semantic disabled | `mode_unavailable` | 501 | passed through |
| `mode` omitted (openclast profile) | `invalid_request` | 400 | seam applies the D2 default first |
| `/search` action ≠ requested mode; identity-bearing legacy claims; subject not on the allowlist | `forbidden` / `unauthorized` | 403 / 401 | gateway defect → `search_provider_violation` |
| Non-room filter with semantic or hybrid | `filter_unavailable` (new) | 501 | explicit refusal |
| Lexical control not allowed for the mode | existing `invalid_query` family | 400 | explicit refusal |
| Cursor supplied | `cursor_unavailable` | 501 | `ranked_unpageable` |
| Signed resource unmapped, or retired mid-request (§7) | `resource_unavailable` | 503 | `search_provider_failed`, no partial re-query |
| Signed resource `pending` or `unavailable` | `semantic_not_ready` (new) | 503 | per-room disclosure before dispatch (§5.3) |
| Embedding or scan failure in either leg | `semantic_failed` (new) | 503 | one hybrid leg down → refuse |
| Hydration integrity failure (§7) | `internal_error` (fixed message) | 500 | refuse |
| Deadline or capability `exp` passed before the response | `unauthorized` | 401 | re-mint |
| No generation yet | `index_building` | 503 | unchanged |

Messages are fixed strings. None contains a path, an object or partition ID, a count, query text or engine text. The envelope `{ "error": { "code", "message" } }` is unchanged.

### 5.3 Scoped readiness probe (A3)

```http
POST /v0/readiness              # capability action "readiness"; only valid on this route
200 {
  "resources": [
    { "tenant_id": "...", "vault_id": "...", "room_id": "...",
      "lexical": "ready|unavailable",
      "semantic": "ready|pending|unavailable" }
  ],
  "embedding_identity": "<hex>"
}
```

- Returns exactly the signed resource tuples, sorted by `(tenant_id, vault_id, room_id)` bytewise.
- Values are closed enums only. There are no counts, progress values, queue positions or timestamps, because progress would expose scheduler state across rooms more finely than A5 accepts.
- OpenClast probes, discloses not-ready rooms, then dispatches. `/search` still re-checks at step 6, so any race resolves to a refusal.
- If A3 is declined, `semantic_not_ready` stays undifferentiated.

The openclast profile still mounts no `/v0/status`.

## 6. Query path and determinism rules

### 6.1 Semantic

```text
gate (§5.1) → embed query once (dedicated query embedder, BGE query prefix)
→ for each leased ready resource (sorted by ResourceKey):
     open its vec object read-only + immutable (per request; no connection cache)
     SELECT chunk_id, vec_distance_l2(embedding, :q) AS d
       FROM vectors ORDER BY d, chunk_id LIMIT :C
→ global merge: concatenate, sort by (d, chunk_id), truncate to C
→ strict hydration (§7) → truncate to limit
```

- `C = 100` (`limit ≤ 100`). The cap applies only after authorization has chosen the resources, both per resource and globally. Every element of the global top-C lies in its own resource's top-C, so the merge equals the top-C of the union. That is exactly what an authorized-only physical baseline computes.
- Every vector of every leased resource is scored. Work is O(Σ authorized chunks × dimensions).
- Distances come from sqlite-vec's scalar function (an imported algorithm, contract invariant 6).

### 6.2 Float and tie rules

- **Writes:** non-finite components and wrong dimension counts are refused at build validation (§4.6).
- **Query:** a non-finite query vector returns `semantic_failed`.
- **Distance:** L2 over normalized vectors, which orders the same as cosine. The f32 result widens exactly to SQLite REAL.
- **Signed zero:** `-0.0` is canonicalized to `+0.0` before Rust comparisons. SQLite already treats them as equal.
- **Order:** SQLite sorts by `ORDER BY d, chunk_id` (REAL, then BINARY TEXT). The Rust merge sorts by `total_cmp` on canonical `d`, then `chunk_id` bytewise. NaN cannot reach either sort.
- **Chunk IDs are unique across resources within one generation**, because they hash `vault_id` and each vault maps to one resource. The tie-break is therefore total.
- **Scores:** `score = 1.0 − d` as f32, described as a semantic distance score. Ordering never reads it.
- **Scope of byte equality:** promised for the same binary, artifacts (`embedding_identity`) and CPU feature set. The witness runs both instances on one host.

### 6.3 Hybrid

```text
gate → lexical leg: IG-1 authorized lexical-v2 over the same leased resources, limit C
     → semantic leg: §6.1 list (top C) over the stripped query text
     → rrf_fuse_traced(lexical_ids, semantic_ids, k = 60.0)
     → strict hydration → truncate to limit
```

- Ranks are 1-based. Each contribution is `1.0 / (60.0 + rank)` in f64, summed lexical-then-semantic. Ordering is fused descending by `total_cmp`, then `chunk_id` ascending. Scores are cast to f32 after ordering.
- Both legs use the same pinned and leased objects. Any leg failure, or a non-ready resource, refuses the request.
- The lexical leg is already proven baseline-equal by IG-1 (W5). Fusion is a pure function of the two authorized lists.

### 6.4 Excerpts

Each hit is hydrated and excerpted from its own resource's lexical object and searcher. Snippet statistics are therefore those of that resource alone, which the baseline instance shares. A hit is one passage from one file.

### 6.5 Capability actions and effective mode

- **On `/v0/search`**, `actions` must be exactly one of `search:lexical`, `search:semantic` or `search:hybrid`, equal to the request mode.
- **On `/v0/readiness`**, `actions` must be exactly `["readiness"]`.
- A capability is valid on one route only. Search actions are refused on the readiness route and vice versa.
- Successful searches return `X-Kwiry-Effective-Mode: <mode>`. The OpenClast provider echoes it, and a mismatch raises `search_provider_violation`.
- Kwiry still returns the frozen hit schema (with `chunk_id` and `score`) to the server-side gateway only. The OpenClast provider strips scores and chunk IDs at the seam (Q2).

## 7. Hydration-time resource re-check

1. Candidates stay grouped by the resource that produced them. Lexical hits map back through `vault_id` to the request's resource set; A2 rejects capabilities that repeat a `vault_id`, which makes that mapping a function. Each candidate is looked up only in its own resource's leased lexical object.
2. The stored `(vault_id, room)` must equal that resource (`validate_document_resource`). The resource must belong to **this request's** leased set. The check runs against request-scoped state, never shared state.
3. **Every candidate must hydrate.** A missing candidate is an integrity failure (`500 internal_error`, whole call). Nothing is dropped. Vector objects are validated against their exact lexical object (§4.6), so this indicates a bug, not a user condition.
4. **End-of-request check.** Before responding, Kwiry confirms three things:
   - every leased resource is still mapped in the *then-active* layout and not tombstoned, otherwise `503 resource_unavailable`;
   - the verified context's `exp` has not passed, otherwise `401`;
   - the request deadline has not passed, otherwise `401`.
   
   `exp` arrives through the capability-context slice (S1b), which precedes this check (§11).
5. OpenClast performs the grant-level second check after the response and returns `search_authorization_changed` on any narrowing. Kwiry cannot see grants and never emits that code. Mid-request grant revocation remains OpenClast's responsibility until IG-4 epochs.

## 8. Cross-file content check

Unchanged from revision 1 and confirmed by the review: **no cross-file content reaches chunk text, embedding input or excerpts.**

- Markdown sections are slices of the file's own body. `![[embed]]` and `[[link]]` stay literal.
- `links_out` is `STORED` only: never indexed, embedded or excerpted.
- No backlinks are computed.
- The Canvas extractor emits a file node's path string and never the target's content.
- Excalidraw and HTML forbid dereferencing (contract §10a.3, §10c.3).
- `embedding_text` is the file's own `heading_path` plus the chunk's own `content`.

No fix is needed. S3 adds the regression test: an embed and link fixture produces literal text only, and its vectors are bitwise identical whether the target exists in a forbidden room, an authorized room or nowhere.

## 9. Threat model and side channels

**Guarantee.** For a fixed binary, embedding identity and CPU feature set, the hits, order, truncation, coverage and scores returned for a request equal those of a physical instance holding only the request's authorized resources. Adding, removing or mutating forbidden resources does not change them.

**Stated limit (owner decision, 2026-11-17; amendment A5).** *When* a resource first answers semantic or hybrid requests, rather than refusing with `semantic_not_ready`, depends on shared indexing load. That includes work for resources the caller cannot see. A caller who watches when its own rooms become ready can therefore learn coarse facts about concurrent indexing elsewhere, such as how many rooms are pending and for roughly how long. Kwiry bounds this with a fair scheduler and bounded work units (§4.7). It does not eliminate it. Results never vary; only readiness timing does.

| Channel | Finding | Response |
|---|---|---|
| **Readiness timing** | Depends on shared indexing load (above). | Accepted under A5; fair round-robin with bounded units; probe exposes enums only; advisory time-to-ready witness (§11.2). |
| **Scan cost** | O(authorized chunks); forbidden objects are never leased or opened. | Bounded by construction. |
| **Query embedder** | Revision 1's shared model put queries behind indexing of forbidden rooms. | A dedicated query embedder is a prerequisite for HTTP enablement. It is **necessary but not sufficient.** Still shared: CPU cores and SMT siblings, memory bandwidth and last-level cache, OS page cache and disk I/O, the Tokio blocking pool, the allocator, and the single query model itself (one queue across all concurrent users, so co-user query load is visible as latency). These are accepted, documented residuals. Builder threads run at lower OS priority. |
| **Reader init** | A map-wide mutex let a cold forbidden open block room A. | Per-object init cells (§4.3) with a test. |
| **Lifetime** | Pruning or retirement could break an authorized request mid-flight. | Leases (§4.3) with barrier tests. |
| **Warm vs cold** | Cached lexical readers reveal recent co-user activity in a room the caller can read. | Documented. Vector objects open per request. Page-cache warmth is a host residual. |
| **Caches** | D4 allows authorized-set-digest keys, or none. | **None.** No query-embedding, result or fused-list cache. |
| **Counts** | Hits ≤ `limit`; no totals. | Byte-equal to baseline, including truncation. |
| **Errors** | Fixed gate order over signed resources only. | Fixed messages. The probe returns signed tuples only. |
| **Generation header** (existing) | Global ID with an embedded timestamp. | A4: authorized-view digest, per mode. Vector publication never changes the lexical view. |
| **Freshness header** (existing) | Daemon-wide state. | A4: per-resource state over signed resources. |
| **Logs** (existing; D4) | Unkeyed subject hash and plain-text actor. | Removed (S1b). Logs carry `jti`, mode, resource count, outcome code and latency bucket only. Never query text or its fingerprint, query vectors, distances or chunk IDs. |
| **Purge residue** | Off-lock builds and crashes could leave retired vectors behind. | §4.8 protocol: tombstone, journal, recovery before serving. |

## 10. Research: indexing order and work units (options, not decisions)

The owner asked for near-term optimization research. This section lists options and tradeoffs. It **decides nothing beyond the owner's 2026-11-17 decision** (a fair shared scheduler with the stated limit). For each option, the question is whether it adds a new cross-room channel beyond A5's.

### 10.1 Which rooms to index first

| Policy | Benefit | Cost | New cross-room channel? |
|---|---|---|---|
| **Round-robin (owner baseline)** | Simple; worst-case time-to-ready is bounded by `u × P × U_max` | Small rooms wait behind many concurrently pending rooms | Only A5's: readiness time reflects how many rooms are pending (`P`) and for how long. It does not reveal their identity or what anyone searched. |
| **Smallest-first** (fewest remaining units) | Small and new rooms become ready fast; best mean time-to-ready | Large rooms can starve under churn; needs an aging bound | **Yes.** A's wait now depends on the *sizes* of other pending rooms relative to A's, which leaks a size ordering. Aging reduces starvation, not the leak. |
| **Recently requested / demand-driven** | Indexes what people actually use | Needs per-room demand state; complex | **Yes, the strongest.** A's wait shifts when *other users search other rooms*, which reveals search activity in rooms the caller cannot see. Demand state is also a new store of user activity (not identities, but timing). Restricting demand to the caller's own rooms still lets others' demand delay A. |
| **New-enrollment priority** | A freshly enrolled room is useful sooner | Bursty enrollments starve others | **Yes, mild.** A slows exactly when another room is enrolled, which reveals enrollment events. Granting new rooms a bounded share (for example, one slot in every k) rather than strict priority limits both starvation and the signal's strength. |
| **Weighted / deficit round-robin by units** | Same fairness as round-robin, independent of unit-size variance | Slightly more bookkeeping | Same as round-robin. |

**Recommended default for the first slice:** plain round-robin over pending resources, one bounded work unit per turn, new and changed resources joining at the tail. No demand signal and no size ordering. This keeps the cross-room channel exactly at A5's stated limit. Revisit smallest-first with aging, or a bounded-share enrollment boost, only with measurements and an explicit owner amendment to A5. Demand-driven priority would need its own threat review.

### 10.2 Bounded, resumable work units

Goal: no single room monopolizes the builder, and progress survives restarts.

- **Unit** = a contiguous run of canonical batches (§4.5), up to `U_max` chunks (initial proposal 256, measured in S4). It stays inside one resource and never splits a batch. Small sources are grouped whole into units. A source larger than `U_max` (a long PDF, say) is split **at batch boundaries in canonical order**, so batch composition, and therefore every vector, is identical to an unsplit run.
- **Whole-source reuse is unchanged.** Reused sources cost no embedding units, only a copy.
- **Resumability.** Each unit appends its rows to `staging/<partition_id>/<lexical_object_id>/vectors.sqlite` in one transaction. It also records `(source_key, source_identity, first_batch, last_batch)` in a build journal in the same transaction. After a restart the builder re-validates the journal against the current lexical object and `embedding_identity`, then continues from the next unit. Any mismatch discards the staging. Staging is subject to tombstones and purge (§4.8).
- **Cooperative cancellation** at batch boundaries for retirement, superseded lexical objects or shutdown.
- **Channel analysis.** A forbidden room's size affects A only through the number of rounds in which it holds a turn. That is A5's stated limit, nothing more. Bounding `U_max` caps how long any single turn can delay A.

### 10.3 What needs measurement (S4 and S7)

- Passage-embedding throughput (chunks per second) on the declared reference hardware, with the fixed thread policy from §4.5.
- Unit duration distribution at `U_max` ∈ {128, 256, 512}.
- Time-to-ready versus pending-room count `P` and room size.
- Query latency (p50, p99) under full builder load, with and without the dedicated query embedder.
- RSS of the builder and query embedders together.
- Staging I/O and fsync cost per unit.
- Resume overhead after a forced kill.
- Exact-scan latency for plain-table `vec_distance_l2` at 25k, 100k and 256-resource fan-in.

## 11. Slices

**Isolation rule:** every intermediate release keeps the existing lexical endpoint's isolation. Two conditions enforce it:

- The lexical-surface fixes (A4 headers, A2 context and logs, reader init cells) ship **before** any vector object can be published.
- **Everything that builds, publishes or serves vectors is compiled only under a non-default Cargo feature, `governed-semantic-internal`.** The feature is excluded from default and release feature sets, as the internal witness observer is. Release binaries cannot publish vector objects or change any lexical header until the enablement slice, an owner checkpoint, removes the gate.

| Slice | Content | Gate | Tests |
|---|---|---|---|
| **S0 — amendment** | A1–A6 text (docs only, owner-approved). | — | — |
| **S1a — scoped lexical headers (A4)** | Authorized-view generation digest and per-resource freshness on the existing lexical endpoint. | shipped | headers unchanged under forbidden-room publish, reconcile and watcher events; IG-1 witness re-run |
| **S1b — verified capability context (A2)** | Workload-subject allowlist; refusal of identity-bearing claims; context keeps `exp` and the exact action; repeated `vault_id` refused; subject and actor removed from logs; coordinated OpenClast claim transition. | shipped | allowlist on and off; legacy `actor` and `subject_revision` → 401; `exp` and action present in the context; captured logs contain no subject, actor or query |
| **S2 — partition objects, leases, retirement journal (lexical)** | §4.1–§4.3 and §4.8 for lexical objects; per-object reader init cells; layout v3; one-time rebuild. | shipped | barrier tests at pin, open and hydrate; B's blocked init does not block A's cached access; retirement crash injection; deletion-failure honesty; restart recovery; IG-1 W1–W13f re-run |
| **S3 — vector object store** | Plain-table store, read-only immutable open, deterministic scan, descriptor; cross-file regression test. | internal | rowid independence; ties at the `LIMIT` boundary; ±0.0; non-finite refused; no sidecar files; cross-file fixture |
| **S4 — builder and fair scheduler** | Round-robin scheduler, bounded resumable units, canonical batches, whole-source reuse, exact inventory validation, attach, tombstone checks, `embedding_identity` from artifact hashes. | internal | short embedder output fails; same count with wrong IDs fails; duplicate, unexpected and mis-associated rows fail; resume after kill; retirement interleaved at unit, validation and attach; lexical publish unaffected by embedder faults; barrier test at *embed*; §10.3 measurements |
| **S5 — real-ONNX determinism gate** | §4.5 gate; batch-size-1 fallback path. | internal (mandatory before S7) | fresh process, fresh build, incremental, reuse versus full, permuted discovery |
| **S6 — governed query path (core)** | §5–§7: gate order, merge, RRF, strict hydration, end-of-request check (uses S1b's `exp`). | internal | **physical baseline**: instance A (rooms 1–4) versus instance B (authorized rooms 1–2), byte-equal for semantic and hybrid; forbidden add, remove and mutate invariance; two-user asymmetric fixture; equal-distance fan-in; 256-resource bound; missing candidate refuses; filter rejected before embed or open (observer-free assertion via a test hook); one-leg failure refuses |
| **S7 — query embedder, HTTP, probe** | Dedicated query embedder; `/search` semantic and hybrid; `/readiness` (A3); taxonomy and headers. | internal | one server test per §5.2 row; probe returns exact tuples only; action scoped per route; latency-under-load measurement |
| **S8 — observer and witness** | §11.1–§11.2 on the internal build. | internal + observer | shipped and observer builds share a functional digest |
| **S9 — enablement** | Removes the `governed-semantic-internal` gate after owner review of S0–S8 evidence. | owner checkpoint | full witness re-run on release build |
| **S10 — later** | Pre-scan filter rowsets inside authorized resources. | — | baseline equality with filters |

Dependencies:

- S8 needs the internal witness observer, which today lives only on the unmerged `feat/ig1-witness-observer` branch (`5ee249e`).
- S1a and S1b need A4 and A2 approved.
- S7 needs A1 and A3.
- S9 needs every amendment.

### 11.1 Observer events (feature-gated)

The existing per-resource vocabulary (`selected`, `reader_*`, `scoring`, `hydrate_*`) gains:

- `lease_acquired`, `lease_released`;
- `vector_gate_ready`, `vector_gate_refused`;
- `vector_open`, `vector_scan`, `vector_refused`;
- `query_embed` (no resource);
- `fusion` (list lengths only);
- `end_recheck_ok`, `end_recheck_refused`.

The invariant is unchanged: zero lease, open or scan events for any excluded resource.

### 11.2 Witness mapping

OpenClast's V-row numbers are not in this repository, so the mapping is by theme. Numbers get pinned when OpenClast shares its V1–V16 table.

| Theme | Kwiry evidence | Extends |
|---|---|---|
| Two-user vector and hybrid | Asymmetric two-user, two-room semantic and hybrid requests; the observer shows leases and opens only on signed resources | W2, W8 |
| Forbidden-partition invariance | Byte-equal hits, order, truncation, scores and coverage against an authorized-only physical instance, unchanged under forbidden add, remove or mutate | W5 |
| **Readiness under forbidden load (new; owner decision)** | **Two instances. Instance 1 holds pending room A plus a large forbidden resource that sorts *before* A by `ResourceKey`. Instance 2 holds only A. Once A is ready on both, results must be exactly equal (assertion). Time-to-ready on each instance is recorded as advisory only, never pass/fail, as evidence for A5.** | new row |
| Mid-request retirement and revocation | Forbidden room B retired at each barrier (pin, embed, open, hydrate) while A's request proceeds: A succeeds exactly. A's own room retired mid-request → `resource_unavailable`. Capability expiry → 401. OpenClast post-check → `search_authorization_changed` | W8, W13a |
| Retirement purge | After retirement, no object, staging or candidate artifact exists for the retired `partition_id`. Every remaining vector object belongs to a live resource whose descriptor and layout agree. The journal reaches `purged` only after durable deletion. Crash-injected runs recover before serving. Recreation under fresh IDs inherits nothing. *(Not "no retired chunk IDs anywhere": chunk IDs omit the room and may legitimately recur under a reclassified vault.)* | W13a–W13f |
| Cross-file embeds | Literal text only; vectors bitwise identical across target presence | new row |
| Audit and storage privacy | Kwiry logs contain no query text or fingerprint, subject or actor. **No query embedding is ever persisted, and no diagnostic vector dump exists anywhere.** Passage vectors exist only inside live vector objects and in-progress staging for live resources. The OpenClast audit holds only the D3 keyed fingerprint. | W11 |
| Named errors, no fallback | `mode_unavailable`, `semantic_not_ready`, `semantic_failed`, one leg down, `filter_unavailable`, `ranked_unpageable` | W10 |
| Shipped = observer | Identical functional digest across every semantic and hybrid row | W12a, W12b |

W1, W3, W4, W6, W7 and W9 are re-run as regression.

## 12. Contract amendment package A1–A6

Flagged here only. `CONTRACT.md` is not edited by this design.

- **A1 — governed modes and default (§4, §11).**
  - Admit `semantic` and `hybrid` in the OpenClast profile under §5's rule as designed here.
  - In that profile `mode` is required at Kwiry. D2's default (`lexical`) is applied at the OpenClast seam.
  - The desktop default (`hybrid`) is unchanged unless the owner extends D2 (Q1).
- **A2 — capability identity and verified context (§5).**
  - Replace "represented subject, distinct current actor" with: the capability carries **no** user identity, pseudonymous or otherwise.
  - `sub` must exactly match an entry in Kwiry's configured **workload-subject allowlist** (for example `["openclast-search-gateway"]`, compared bytewise).
  - A capability carrying `actor`, `subject_revision` or any identity-bearing claim outside the declared claim set is refused with `401`. Unknown claims are refused, not ignored.
  - `jti` stays a per-request random nonce.
  - The verified request context preserves `exp`, the exact action, `jti`, resources and `max_limit` for later rechecks.
  - Each `vault_id` may appear at most once among the resources.
  - Requires a coordinated OpenClast claim transition.
- **A3 — scoped readiness probe (§4).** A capability-protected `POST /v0/readiness`, action `readiness`, returning closed per-resource enums for the exact signed `{tenant_id, vault_id, room_id}` tuples only (§5.3). Search actions are scoped to `/search` (§6.5).
- **A4 — scoped freshness and generation headers (§4; IG-1 doc).** Applies to the existing lexical endpoint as well.
  - **Authorized-view encoding.** `X-Kwiry-Generation: v1-<hex>`, where `<hex>` is SHA-256 over:
    - `"kwiry-authorized-view-v1\0"`;
    - the mode class byte (`L` for lexical; `S` for semantic and hybrid);
    - the u64 little-endian resource count;
    - for each signed resource in bytewise `(tenant_id, vault_id, room_id)` order: the length-prefixed `tenant_id`, `vault_id` and `room_id`, the length-prefixed lexical object ID, and, in class `S` only, the length-prefixed vector object ID.
    
    The class `L` digest ignores vector objects, so vector publication never changes the lexical view. The header contains no timestamp and changes only when an authorized resource's objects change.
  - **Per-resource freshness.** Each resource is `current`, `reconciling`, `stale` or `unavailable`:
    - `current → reconciling` when a pass whose read scope includes that resource's vault starts. Path-scoped passes include only vaults with changed paths. Full passes are boot- or schedule-triggered, never content-triggered.
    - `reconciling → current` when the pass publishes with no remaining dirty state for that vault.
    - `reconciling → stale` when it ends incomplete: incomplete enumeration, retained content, or an unavailable vault.
    - `current → stale` on a watcher event for that vault.
    - `stale → reconciling` when the next covering pass starts.
    - Any state becomes `unavailable` when the resource is unmapped or tombstoned. Such a resource is never served.
    
    The response header reports the **worst** state among the signed resources, ordered `stale` > `reconciling` > `current`. Resources that are `unavailable` are refused earlier.
- **A5 — semantic readiness-timing limit (owner decision 2026-11-17).** For a request, semantic and hybrid results are exactly invariant to forbidden resources. Readiness *timing* is not. A fair shared scheduler builds vectors, and the moment a resource first answers rather than returning `semantic_not_ready` may depend on server load, including indexing load from resources the caller cannot see. Kwiry bounds this with round-robin scheduling and bounded work units, exposes readiness only as closed enums, and makes no claim of timing invariance. Changing the scheduling policy (§10) in a way that adds a channel requires amending A5.
- **A6 — retirement and durable purge (§5).**
  - Retirement has two distinct, separately reported states.
  - **Retrieval revoked:** no new request can use the resource; in-flight requests are refused at their end-of-request check.
  - **Purge completed:** every Kwiry-held artifact of the resource (lexical and vector objects, staging and candidates) is unlinked, the deletions are durable (parent directories fsynced), no lease remains, and the completion record is durable.
  - Kwiry never reports purge completed after a deletion or sync failure.
  - Incomplete retirements are resumed before serving after any restart.
  - A retired resource tuple is never re-indexed.
  - Purge is file unlinking, not cryptographic erasure of the storage medium.

Not amendments:

- refusing non-room filters, which the existing explicit-refusal rule already covers;
- exact baseline equality, which is stronger than §1.1 and does not conflict with it;
- `ranked_unpageable`, which is an OpenClast mapping.

## 13. Risks and open questions

**Risks**

- **R1 — ONNX determinism.** This is the highest technical risk, and S5 is a hard gate (§4.5). Fallback: batch size 1. Otherwise re-scope with the owner.
- **R2 — Layout migration (S2).** It changes IG-1's accepted storage and needs a full IG-1 witness re-run. It is limited to disposable state with a one-time rebuild.
- **R3 — Memory.** The query embedder plus the builder embedder plus sidecar RSS, measured in S7. Host headroom matters.
- **R4 — Time to ready under load.** This is the stated limit (A5). Large enrollments can keep other rooms `pending` for a while. Lexical search stays available throughout.
- **R5 — Purge completion latency.** Purge waits on lease drain, which the request deadline bounds. A stuck fsync keeps the state at `revoked`, which is honest but needs operator visibility.
- **R6 — Exact-scan latency.** Plain-table scans are unmeasured, and the fan-in can reach 256 resources.
- **R7 — Observer not on `main`.** This blocks S8.
- **R8 — Existing IG-1 leaks.** These are real today. S1a and S1b fix them regardless of whether IG-2A proceeds.

**Open questions**

- **Q1** — Should D2 (default lexical) also change the desktop default?
- **Q2** — Should Kwiry itself omit `score` and `chunk_id` in the governed profile, as defence in depth? That needs a hit-schema amendment.
- **Q3** — Do we want per-resource encryption keys with crypto-shredding on retirement later?
- **Q4** — Desktop reports L2-derived scores as "cosine similarity". Fix that separately?
- **Q5** — The `openclast` profile is to be renamed to a generic governed profile. New IG-2A identifiers should use the generic name.
- **Q6** — Should the advisory time-to-ready evidence (§11.2) feed a published operator expectation, such as a target per thousand chunks on reference hardware?
