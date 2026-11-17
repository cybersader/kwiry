# IG-2A design — governed semantic and hybrid retrieval

Status: **design, revision 3, owner review pending.** Nothing here is implemented or approved. This page expands [IG-2](governed-semantic-hybrid.md) and is bound by [`CONTRACT.md`](../../CONTRACT.md), which this design does not edit. The amendments it needs are in [§13](#13-contract-amendment-package-a1a6). What it asks of OpenClast is in [§14](#14-openclast-seam-asks).

## Revision history

- **Revision 1** (`1442e668`): rated NO-SHIP by an independent review. The core approach was judged sound.
- **Revision 2** (`b70c902d`): rated NO-SHIP on re-review, but converging.
  - Closed by that review: per-resource reader init cells, coverage integrity, A2, A3.
  - Partially closed: lifetime, purge, scheduling, slice order, embedding identity, A4.
  - Not a blocker: the owner's readiness-timing limit.
- **Revision 3** (this page) answers the second review and records OpenClast's design-stage answer on retirement authority.

| Re-review finding | Resolved in |
|---|---|
| 1 (P1) Request workers outlive the HTTP deadline but leases do not | §4.4: worker-owned leases, cancellation checkpoints, bounded refusal (not bounded purge) |
| 2 (P1) Retirement races an admitted builder | §4.3: admission barrier and lock order; paused-builder test |
| 3 (P1) Manifests survive purge | §4.1: per-resource manifest shards inside objects; §5.7: exact allowed residual; purge witness |
| 4 (P1) Reuse identity is too weak | §4.7: preparation identity plus a digest of the canonical text sequence |
| 5 (P1) Permanent Kwiry tombstones | §5: OpenClast holds tombstone authority (default P-a); Kwiry keeps a disposable cache only |
| 6 (P1) A failed purge blocks startup | §5.6: quarantine only the retired resource (default P-b) |
| 7 (P2) Scheduler budget covers only embedding | §4.9: cost units for every phase, yield points, stated assumptions, reused-room fairness test |
| 8 (P2) Ownership outside requests | §4.3 and §4.5: build leases, publication ownership, reachability GC, durability order; §4.10: migration |
| 9 (P2) Effective set | §6.1: one effective set for gates, leases, digest and freshness; the probe uses the signed set |
| 10 (P2) Freshness of a superseded lease | §8.4: per-resource stale labelling, by mode class |
| 11 (P2) S1a ordering | §12: transitional per-resource revision identity; corrected release-behaviour statement |
| A2, A4, decision 2, round-robin wording, behaviour-change statement | §13, §2.2, §11.1, §12 |

## 1. The idea in one paragraph

Governed semantic search follows the IG-1 lexical model. **Every room has its own immutable stores, one lexical and one vector.** A request leases and opens only the stores of the rooms it is signed for. A vector store is derived from exactly one lexical store and records that store's identity, so the two can never disagree.

A query embeds the question once, then scans each authorized room exactly for its nearest neighbours. It merges the per-room lists by distance, then chunk ID; rowids play no part. Hybrid fuses that list with the IG-1 authorized lexical list using RRF (`k = 60`).

Three things hold for forbidden rooms. They are never opened. They cannot invalidate an authorized request's stores. And they cannot change what it returns: results are byte-equal to those of a separate instance holding only the authorized rooms.

**OpenClast is the authority on which rooms exist.** Kwiry builds only rooms OpenClast confirms are active, and it purges rooms OpenClast reports as retired. Its whole data root stays disposable.

**Stated limit:** the equality covers results, not timing. How soon a room becomes semantically searchable depends on shared server load, including indexing work for rooms the caller cannot see.

## 2. Decisions

### 2.1 Recorded decisions (not reopened)

- **D1–D4 (owner, 2026-11-17).**
  - D1: the retrieval mode is selectable per request, and an unavailable mode is an explicit error.
  - D2: the default mode is lexical.
  - D3: the audit records only a keyed query fingerprint.
  - D4: no user IDs at the engine; caches are keyed only by the authorized-set digest, or there are none.
- **Readiness scheduling (owner, 2026-11-17).** A **fair shared indexing scheduler** with an explicitly stated limit. Results are exactly invariant to the forbidden corpus. How soon a room becomes searchable may depend on server load, including load from rooms the caller cannot see. This design never claims unconditional forbidden-corpus invariance (§10, A5).
- **Engineering choices, fixed by the brief:**
  - physical per-resource partitions;
  - exact KNN;
  - a deterministic merge;
  - candidate caps applied only after the authorization boundary;
  - any later ANN must re-prove baseline equality before adoption;
  - the whole-corpus desktop KNN path stays desktop-only.
- **OpenClast retirement-authority answer (peer-reported, design-stage, 2026-11-17).** Recorded in §5.1.

### 2.2 Owner decisions

Each comes with a recommendation or a default the owner can override.

1. **Approve amendment package A1–A6 (§13).** A1 admits semantic and hybrid with an explicit mode. A2 makes capabilities name a workload, never a person. A3 adds a scoped readiness probe. A4 scopes the generation and freshness headers to the caller's rooms. A5 states the readiness-timing limit. A6 defines retirement, durable purge and the custody cache. *A2 and A4 also change the existing lexical endpoint (§12).*
2. **Make a recoverable storage transition to per-resource objects (§4.10).** This lets retiring room B never break an in-flight request for room A, and lets purge reach every byte that names B.
   - **Disk headroom:** the old layout and the new one coexist until the upgrade is confirmed, so plan for roughly twice the governed lexical index size during the transition.
   - **Rollback:** before confirmation, fast rollback restores the saved old pointer and runs the previous binary. Rollback is always possible by rebuilding with the previous binary, because the index is disposable.
   - **Retirement overrides retention:** a retirement during the window deletes old-layout trees that contain the retired room, which removes fast rollback for that data root.
   - Re-running IG-1 witness rows W1–W13f is required. *Recommendation: yes.*
3. **Refuse rather than serve stale vectors when a room's vectors lag its text.** A coherent stale answer is possible, using older lexical and vector objects that agree with each other. It would mean retaining and leasing older objects against the purge guarantee, plus a separate staleness disclosure. *Recommendation: refuse for IG-2A.*
4. **Pay the RAM for a dedicated query embedder.** It is a prerequisite for HTTP enablement (§12). It is necessary but not sufficient (§10). *Recommendation: yes; measure the cost in S8.*
5. **Reject non-room filters (`path_prefix`, `tags`, `frontmatter_equals`) for semantic and hybrid in IG-2A.** Rejection happens before embedding and before any store opens. *Recommendation: yes.*
6. **(P-a, default) Tombstone authority stays with OpenClast.** Kwiry keeps no permanent tombstones and no history of retired partition IDs. It rebuilds its exclusions from OpenClast's custody query (§5) and refuses to index anything it cannot confirm is active. This fits "files are the source of truth, indexes are disposable" (CONTRACT §1.1, §7) and "OpenClast is the identity and entitlement authority" (§1.7). *Override only if the owner wants Kwiry to keep its own record.*
7. **(P-b, default) A failed purge quarantines only the retired resource.** Kwiry enforces retirements before accepting requests, quarantines whatever it cannot delete, keeps retrying, and reports purge-incomplete. It never takes healthy rooms or lexical search offline, which upholds the binding rule that semantic failures never degrade lexical search. *Override only if the owner prefers a fail-stop.*

## 3. What exists today

Read from `origin/main` at `74551c1`. Confirmed by the independent reviews unless stated otherwise.

- **Desktop vectors.** They live in one mutable whole-corpus store, `<data-dir>/semantic/semantic.db`, outside the generations. The governed reconcile passes `semantic: None`, and openclast mode rejects `serve --semantic`.
- **Desktop KNN and hydration.** KNN ties at the k boundary depend on insert history (vec0 `MATCH … k`). Hydration drops candidates silently. `embed_and_replace_source` zips IDs with vectors, so a short embedder output truncates silently.
- **Embedding identity is descriptive.** `EmbeddingProfile::fingerprint()` hashes names and settings, not artifact bytes. `embedding_source_identity` binds `content_hash` and the per-format identity, but not the core preparation schema or chunking version (`SOURCE_PREPARATION_SCHEMA_VERSION`, `CHUNKING_VERSION`).
- **Lifetime.**
  - A request pins a generation with `ArcSwap::load_full()`, which keeps only the in-memory struct alive. `prune_generations` can delete files before a lazy open.
  - The search handler runs in `spawn_blocking`. That work continues after the HTTP future is dropped, by timeout or disconnect, so any lease must be owned by the worker, not by the handler.
  - `sync_tree` covers only the candidate directory.
- **Manifests carry resource-identifying data.** Every generation's `manifest.json` holds per-source entries with vault, path, resource tuple, content hash, byte length and mtime. Retained generations keep that data after a room leaves the layout.
- **Readers.** A map-wide mutex covers IG-1 partition reader opens.
- **Existing IG-1 leaks.** The global, timestamped `X-Kwiry-Generation`. The daemon-wide `X-Kwiry-Index-Freshness`. Logs carrying an unkeyed `sha256(subject)` and a plain-text `actor`.
- **Request context.** `Principal` keeps neither `exp` nor the exact action. Desktop principals carry an empty resource set (`Principal::desktop`).
- **Chunk IDs** hash `vault_id` and path but not the room.

## 4. Storage, ownership and indexing

### 4.1 Per-resource immutable objects

```text
<data-dir>/
  current.json                 # { layout_version: 3, generation }
  generations/<generation-id>.json
  objects/<object-id>/         # random 128-bit ID; no resource identity in any path
    descriptor.json            # kind (lexical|vector), resource tuple, content digests
    lexical: index/ + manifest.json   (this resource's manifest entries only)
    vector:  vectors.sqlite
  staging/<build-id>/          # in-progress builds; owned by a live build root (§4.3)
  quarantine/<random>/         # undeletable unreachable artifacts (§5.6)
  custody-cache.json           # disposable; §5.5
  models/                      # model artifacts only
```

- **A generation record** is small and immutable. It holds the format versions, `state_revision`, `last_sync`, and the layout: resource tuple → `{ lexical_object, vector_object? }`. It contains **no** paths, hashes, lengths or timestamps of sources. Those live in the lexical object's own `manifest.json` shard, so they are deleted together with the object.
- Object IDs are fresh random values and never reused. No filesystem name carries a tenant, vault, room or partition digest.
- Objects are immutable once published. Vector stores open with `file:…?mode=ro&immutable=1`, so no SQLite sidecar files appear.

### 4.2 Durability order

Each step completes before the next. `sync_tree` no longer covers external objects.

1. Write the object files under `staging/<build-id>/` and fsync each file.
2. Fsync the staging directory. Rename it to `objects/<object-id>/` and fsync `objects/`.
3. Write the generation record to a temporary file, fsync it, rename it into `generations/`, and fsync `generations/`.
4. Write `current.json` atomically (temporary file, fsync, rename) and fsync the data root.

A crash before step 3 leaves an unreferenced object, which reachability GC removes (§4.5). A crash before step 4 leaves an unreferenced generation record, also collected. Readers only ever follow `current.json`.

### 4.3 Ownership, locks and the admission barrier

There are three kinds of owner, and nothing is deleted while any owner holds it:

- **Request leases.** These are taken by a request worker (§4.4) on the effective set's objects (§6.1).
- **Build leases.** These are taken by a builder on its input lexical object and on any vector object it reuses, for the whole build.
- **Build roots.** These are registered in memory for each `staging/<build-id>/`. A build root also covers the window between the rename into `objects/` and the publication of a generation that references the object (*publication ownership*). It is released only after that publication completes, or after the builder abandons the object.

**Lock order.** Locks are always taken in this order, and a lock is never taken while holding a later one:

1. **L1 — the data-root writer lock** (exists today). It serializes publication and may be held across I/O.
2. **L2 — a per-resource admission gate.** A small mutex and condition variable with state `open | closing | closed` and a count of admitted operations. It is held only to change that state or count, never across I/O.
3. **L3 — the lease and ownership registry mutex.** It is held only for in-memory bookkeeping. It is **never held across I/O, deletion, fsync or any wait.**

**Admission.** Every builder or attach step that touches the filesystem for a resource first takes an **admission token** from that resource's gate. Taking a token fails if the gate is not `open`. While holding the token, the step re-checks custody (§5) and then performs its filesystem operation. The token is released afterwards. Retirement (§5.6) closes the gate and waits for the admitted count to drain before its final deletion scan, so no admitted builder can recreate staging afterwards.

**Request leases** do not use admission gates. Revocation reaches them by publishing a generation that omits the resource, together with the end-of-request check (§8.3). Doomed objects can never be leased again.

**Per-object reader init cells (closed in revision 2).** The map lock is held only to find or insert a cell. The open runs inside that cell, outside the map lock.

**Tests (S2, S3):**

- A builder paused after its custody and gate check but **before** its filesystem operation, with retirement started. Retirement must wait at drain. The builder then completes. Retirement must delete what the builder wrote and finish with a clean final scan.
- A blocked reader init for room B, during which room A's cached access must still complete.
- L3 never held across I/O, asserted with a debug lock-tracker.

### 4.4 Request workers own their leases (finding 1)

- The handler creates a **cancellation token** and a **drop guard**. If the handler future is dropped, by client disconnect or by the HTTP stack, the guard cancels the token.
- The `spawn_blocking` worker takes the request leases itself, at pin time (§6.1, step 5), and holds them in an RAII guard. Leases are released when that guard drops: on normal return, on error, on cancellation, and on **panic unwind** (the workspace uses the default `panic = "unwind"`). Release decrements the registry under L3 and signals any waiting purge.
- **Cancellation checkpoints:** after pin and lease; before embedding; after embedding; before each resource open; during each vector scan; between resources; before and after the lexical leg; before each resource's hydration; and before response assembly.
- **Scan interruption:** a SQLite progress handler checks the token every *N* VM steps and aborts the statement.
- **Uninterruptible steps**, each bounded by input size: query-embedding inference (one input of at most 512 tokens), a single resource's Tantivy search, and a single resource's hydration. The worker checks the token as soon as each one returns.
- **The handler's promise is bounded refusal.** It waits on the worker for at most the request deadline: the capability's remaining lifetime, capped at a configured maximum, 30 s by default. When the deadline passes it returns `503 deadline_exceeded` without waiting for the worker.
- **Purge latency is not promised.** A purge waiting on a lease completes only after the worker reaches its next checkpoint and releases. Kwiry promises bounded *refusal*, not bounded purge completion. Bounded completion would need terminable workers, for example process isolation, which is a later option (Q7).
- **Bounded concurrency.** At most `W` search workers run at once, cancelled-but-unfinished ones included. Beyond `W`, requests are refused with `503 busy`. Cancelled workers can therefore not pile up without limit.

**Tests (S2 lexical, S7 semantic).** Inject a timeout and a client disconnect at each barrier: pin, embed, open, scan and hydrate. Assert all of the following:

- The response is refused within the deadline.
- Leases are released only after the worker acknowledges.
- Lease counts return to zero.
- A retirement purge waiting on those leases completes after the acknowledgement.
- A panicking worker releases its leases.

### 4.5 Reachability garbage collection

Reachable means referenced by the active generation, by a retained generation that names only resources confirmed active (§5), by a live build root, or by a live lease. Everything else under `objects/`, `staging/`, `generations/` and the layout-v2 trees (§4.10) is garbage.

- **At startup**, before binding the listener, the order is:
  1. take L1;
  2. enforce custody (§5.6), including any publication that excludes non-active resources;
  3. resume valid resumable staging as build roots (§11.2);
  4. delete garbage.
- **Periodically, and after every publication**, garbage is collected under the same rules. Live roots and leases are read from the registry under L3 and released before any deletion.
- Retained generations that name a resource not confirmed active are themselves garbage. Retention for crash recovery never outranks retirement.
- A deletion that fails moves to quarantine (§5.6). GC never blocks serving.

### 4.6 Vector objects and readiness

A vector object holds `vectors.sqlite`, a plain table (`chunk_id` primary key, `source_key`, `source_identity`, `embedding` blob, `WITHOUT ROWID`). Its `descriptor.json` holds:

```jsonc
{
  "kind": "vector",
  "vector_schema_version": 1,
  "resource": { "tenant_id": "...", "vault_id": "...", "room_id": "..." },
  "lexical_object_id": "<id>",
  "preparation_identity": "<hex>",   // §4.7
  "embedding_identity": "<hex>",     // §4.7
  "inventory_digest": "<hex>",       // computed from validated rows (§4.8)
  "chunk_count": 1234,
  "vectors_sha256": "<hex>"
}
```

A resource is `ready` in the pinned generation only if three things hold. The layout maps it to a vector object. That object's `lexical_object_id` equals the layout's lexical object for the resource. Its `embedding_identity` equals the loaded query embedder's identity. Otherwise the resource is `pending` (no matching object yet) or `unavailable` (no model, or a failed build under this identity).

### 4.7 Embedding, preparation and reuse identity (finding 4)

- **`embedding_identity`** is SHA-256 over domain-separated, u64-LE length-prefixed components:
  - **Artifacts:** the bytes of `model.onnx`, `tokenizer.json`, `tokenizer_config.json`, `special_tokens_map.json` and `config.json` as actually loaded.
  - **Preprocessing:** `embedding_input_version`, query and passage prefixes, truncation (512), pooling (CLS), L2 normalization, `vector_schema_version`, and distance (`l2_normalized`).
  - **Execution policy:** fastembed and ONNX Runtime versions, CPU provider, fixed intra-op and inter-op thread counts, graph optimization level, `B = 8`, and pad-to-longest.
- **`preparation_identity`** is SHA-256 over:
  - `SOURCE_PREPARATION_SCHEMA_VERSION`;
  - `CHUNKING_VERSION`;
  - `INDEX_FORMAT_VERSION`;
  - `FORMAT_IDENTITY_SCHEMA_VERSION`;
  - the extraction-policy fingerprint;
  - each enabled format's `format_identity_fingerprint`.

  It is recorded on both lexical and vector objects.
- **Text-sequence digest per source.** SHA-256 over the source's canonical sequence of `(chunk_id, embedding_text)` pairs, each u64-LE length-prefixed, in `chunk_id` bytewise order. It is computed from the lexical object being embedded.
- **Reuse rule.** A source's rows may be reused from an earlier vector object only if `source_key`, `source_identity`, `preparation_identity`, `embedding_identity` **and the text-sequence digest** all match. Reuse is all-or-nothing per source. Reuse across different `preparation_identity` values is forbidden. A source whose rows can't be reused is re-embedded whole.
- **Canonical order and batching.** Within a source, chunks are ordered by `chunk_id` bytewise. Batches are consecutive groups of `B`, never spanning two sources. Batch composition is therefore independent of segment order, reuse history and other sources.
- **Test (S5).** Change the prepared text while keeping source bytes, chunk IDs and chunk counts identical: for example, a test-only chunker normalization change under a bumped preparation identity, and separately the same text change with identity held fixed. Reuse must be refused in both cases, the first by identity and the second by the text digest.
- **Mandatory real-ONNX gate (S6).** Vectors must be bitwise identical, and results byte-equal, across fresh processes, a clean-target fresh build, incremental rebuild, reuse versus full re-embed, and permuted discovery order. If the gate fails, fall back to `B = 1` and re-run it. If that also fails, re-scope with the owner before any HTTP enablement.

### 4.8 Building and coverage integrity (closed in revision 2; restated)

- The builder holds a build lease on the lexical object it embeds. It reads stored `content`, `heading_path`, `chunk_id` and `source_key` from it.
- The embedder must return exactly one vector per input text. A length-checked pairing replaces `zip`.
- Before attach, the staged rows must match the expected `(chunk_id, source_key, source_identity)` inventory exactly. Missing, duplicate, unexpected and mis-associated rows all fail the partition. So do wrong dimension counts and non-finite components.
- `chunk_count`, `inventory_digest` and `vectors_sha256` are derived from the validated rows.
- Attach takes an admission token (§4.3), checks custody, and confirms the layout still maps the resource to the same lexical object. It then performs the durability order (§4.2) and publishes.

### 4.9 Fair shared scheduler and work budget (finding 7)

**Policy (design recommendation under the owner's fairness decision).** Pending resources sit in a round-robin rotation. Each turn runs one bounded work unit for the head resource, then moves it to the tail. New and changed resources join at the tail. Nothing is ordered by key, size or demand.

**Budget covers every substantial phase.** One work unit has a budget of `U_max` *cost units*:

| Phase | Charged per |
|---|---|
| Enumerating the lexical object's stored documents | 1 per 64 documents read |
| Copying reused rows | 1 per 256 rows |
| Embedding | 1 per chunk |
| Inventory validation | 1 per 256 rows |
| Hashing (`vectors_sha256`, digests) | 1 per MiB |
| Finalization (fsync, rename, publication) | a fixed charge per object |

**Yield points** sit at every budget boundary inside every phase. Enumeration resumes from a saved segment and document cursor. Copying, validation and hashing are streaming and resumable from saved offsets. Embedding yields only at batch boundaries. A phase that cannot split finer, such as one finalization, is charged whole, and its cost is bounded by object size.

**Bounded work versus measured time.** `U_max` bounds *work* deterministically. Wall-clock time per unit is only *measured*, because it varies with hardware and load. No time bound is promised.

**Fairness claim and its assumptions.** A resource needing `u` units completes within `u` of its own turns. Between two of its turns, at most `P_max − 1` other turns run. Assumptions:

- one builder worker;
- turns are never preempted by other builder work;
- `P_max` is the maximum number of pending resources during the interval, and arrivals raise it;
- each turn consumes at most `U_max` cost units.

Elapsed time additionally depends on CPU contention from queries and reconciliation. That is A5's stated limit.

**Fairness test (S5).** A large **reused** room spends almost all its budget on enumeration, copying, validation and hashing. Next to it sits a small room A. The test asserts that A's turns interleave with the large room's turns, that no turn exceeds `U_max` cost units, and that A becomes ready within its bound in turns.

### 4.10 Migration from layout v2 (recoverable storage transition)

- `kwiry index` (offline) builds a full layout-v3 data set into `objects/` and `generations/`, alongside the existing v2 generation directories. The new binary refuses to serve layout v2. This matches the IG-1 precedent of a one-time rebuild.
- **Publication.** The upgrade first writes `upgrade.json { from: v2-generation, state: "building" }`, then builds v3. It then saves `current.v2.json` as a copy of the old pointer and switches `current.json` to v3. Finally it sets `state: "published"`.
- **Interrupted upgrade.** On restart, `state: "building"` means unpublished v3 objects and records are garbage. The upgrade restarts from scratch, and v2 is left untouched.
- **Confirmation.** `kwiry gc --confirm-upgrade` deletes the v2 trees, `current.v2.json` and `upgrade.json`. So does the first successful serve after a configured grace period.
- **Rollback before confirmation.** Restore `current.v2.json` as `current.json` and run the previous binary. Rollback is always possible by rebuilding with the previous binary.
- **Retirement overrides retention.** If custody reports retired any resource present in a v2 tree, GC deletes that v2 tree immediately (§4.5), which ends fast rollback.
- **Headroom.** v2 and v3 coexist until confirmation, so plan for roughly twice the governed lexical index size.

## 5. Retirement authority and purge

### 5.1 The seam (OpenClast answer, peer-reported, design-stage)

- **(b) Mandatory, and already true.** Capabilities only ever name enrolled, active, random IDs. A retired ID is never minted.
- **(a) New.** A signed server-to-server **custody query** for the `kwiry` provider. It returns the provider's **active set**, its **retired set** (tombstoned IDs only, no labels or names) and a **monotone custody revision** so Kwiry can detect staleness. It uses a dedicated provider credential scoped to reading that provider's custody only. Every query writes an unsampled audit event on OpenClast's side.
- **Rule.** Kwiry refuses to index or build anything not confirmed active at the latest revision it has seen.
- OpenClast will design the endpoint, with an independent security review, after receiving Kwiry's final asks (§14).

### 5.2 When Kwiry queries

- **At startup**, before the listener binds and before any build or GC (§4.5).
- **Before every full rebuild** (`kwiry index`, embedding-identity change, migration). A rebuild requires a fresh successful query, never the cache alone.
- **Periodically**, every `custody_poll_interval` (default 60 s).
- **On capability arrival** naming a resource not in the cached active set. This triggers one coalesced, rate-limited query (at most one in flight, at most one per `custody_min_interval`, default 5 s). The request itself does not wait: it is refused with `resource_unavailable`. Under (b), such a capability can only follow a fresh enrollment.

### 5.3 Validation and monotonicity

Every response must satisfy all of the following:

- it verifies against the custody signing keys (§14);
- it names this provider and tenant;
- it echoes the request's fresh nonce;
- it is within its validity window;
- its active and retired sets are disjoint.

Then:

- **Revision lower than the highest seen:** refused. Kwiry keeps its current state, raises an operator alarm `custody_regressed` and retries. A lower revision is never accepted.
- **Same revision, different content:** refused, with the alarm `custody_inconsistent`.
- **Higher revision:** accepted atomically. Exclusions are recomputed and the cache is replaced.
- **No cache** (fresh or wiped data root): the first valid, nonce-bound live response is accepted. It cannot be a replay, because it echoes this request's nonce.

### 5.4 When the endpoint is unreachable

- Kwiry **keeps serving** resources that are already built and confirmed active at the latest revision it has seen. Every request still needs a valid signed capability, and (b) ensures capabilities name only active IDs.
- **It builds nothing new.**
  - No enrollment of a resource not in the cached active set.
  - Once the last successful confirmation is older than `custody_max_age` (default 5 min), **no publication of any lexical or vector object** until a query succeeds.
  - Rebuilds are refused.
  - Affected resources then report `stale` freshness as soon as they have dirty state (A4), never `current`.
- **No cache and endpoint unreachable at startup:** nothing is confirmed active, so nothing is served or built. Lexical and semantic both refuse with `resource_unavailable` until the first successful query. This fails closed.
- Failures are recorded as closed operator categories: `custody_unreachable`, `custody_unauthorized`, `custody_invalid`, `custody_regressed`.

### 5.5 Disposable cache

`custody-cache.json` holds `{ provider, tenant, revision, confirmed_at, active: [tuples] }` and nothing else. **The retired set is never persisted.** It is used only transiently, during the response that delivers it. The cache is written atomically (§4.2 step 4 pattern). It may be deleted at any time; Kwiry then behaves as in §5.4 until the next successful query. It is derived authorization state, not index state, and A6 states that explicitly.

### 5.6 Retirement: quarantine and purge (default P-b)

**Trigger.** A resource newly present in the retired set, or present in Kwiry's layout or configuration but absent from the active set, is **excluded**. Only a confirmed retirement produces a purge report.

1. **Close admission.** Set the resource's gate to `closing`. Cancel its queued work units. A running unit stops at its next yield point. Wait for the admitted count to reach zero (no L3 held).
2. **Revoke retrieval.** Under L1, publish a generation whose layout omits the resource. New requests can no longer lease its objects. In-flight requests are refused at their end-of-request check (§8.3). The resource is now **retrieval-revoked**. Set its gate to `closed`.
3. **Doom.** Under L3 briefly, mark every object of the resource as doomed. That includes objects referenced only by retained generations, which become garbage (§4.5).
4. **Delete.** After the leases on each doomed object drain (§4.4), delete it. Also delete the resource's staging, any garbage generation record that names it, and any v2 tree that contains it. Fsync each parent directory. L3 is never held during this step.
5. **Final scan.** Scan the whole data root for artifacts of the resource (§5.7).
6. **Report.** Only if the scan is clean is the resource **purge-completed**, and only then is completion reported (§14 ask 4). Otherwise it is **purge-incomplete**.

**Failure handling.** An artifact that cannot be deleted is renamed into `quarantine/<random>/` where possible. If even the rename fails, it is recorded in the in-memory quarantine set, which reachability GC rebuilds at startup. Quarantined artifacts are never reachable, leased or read. GC retries with backoff. The resource stays purge-incomplete, and Kwiry never reports completion. **Healthy resources and lexical search are never taken offline.**

**Restart.** Startup enforces custody first (§4.5), so any unfinished retirement is revoked again before requests are accepted. Deletion resumes, and failures quarantine as above. Startup never waits on deletion.

**Tests (S3), crash-injected and interleaved:**

- a crash after each of steps 1–6;
- the paused-builder admission test (§4.3);
- retirement while request leases are held, combined with timeouts and disconnects (§4.4);
- deletion failure injected, showing quarantine, purge-incomplete, healthy rooms and lexical still served, and completion reported after a later successful retry;
- restart recovery before serving;
- a regressed custody revision refused;
- endpoint unreachable, showing serving continues and building freezes;
- recreation under fresh IDs inheriting nothing.

### 5.7 Allowed residual after purge-completed

**Inside the data root: nothing that names or contains the retired resource.** The final scan covers:

- `current.json` and every generation record (layouts);
- every object descriptor and lexical `manifest.json` shard;
- `staging/`, including build journals;
- `quarantine/`;
- `custody-cache.json`, whose active set no longer lists the resource;
- `upgrade.json` and `current.v2.json`;
- every layout-v2 generation tree, including its `manifest.json` and `partitions/`;
- the legacy desktop `semantic/` directory, which must be absent in the openclast profile;
- lock and probe files.

**Outside the data root, stated and not covered:**

- The operator's configuration file, which is operator-owned.
- External log sinks. Kwiry logs refer to resources only by a per-process keyed digest with a random key that is never persisted, so log lines cannot be linked across restarts or to a resource.
- Storage-medium remanence. Purge unlinks files; it does not cryptographically erase them.
- OpenClast's own tombstone, which is the authority record.

## 6. Readiness, errors and HTTP mapping

### 6.1 Effective set and gate order (finding 9)

The **signed set** `S` is the capability's resources. The **effective set** is `E = S ∩ {vault_id, room filters}`. `E` alone drives the gates, the leases, the generation digest (A4) and freshness. The readiness probe (§6.3) is defined over `S`. If `E` is empty, the response is `200` with no hits. Its view digest is computed over the empty set and its freshness is `current`. Nothing is embedded or leased.

1. Capability verification. The verified context keeps `exp`, the exact action, `jti`, resources and `max_limit` (A2). The action matches the route and mode (§7.5).
2. In the openclast profile, `mode` is required. D2's default is applied at the OpenClast seam.
3. Query, control and filter validation. **Non-room filters are refused here for semantic and hybrid, before any embedding or open.**
4. Model loaded with the expected `embedding_identity`? If not, `mode_unavailable`.
5. The worker pins the generation and takes leases on `E` (§4.4). Each resource in `E` must be mapped in the layout and confirmed active in custody, otherwise `resource_unavailable`.
6. Each resource in `E` must be semantic-`ready` (semantic and hybrid), otherwise `semantic_not_ready`.
7. Execute, with cancellation checkpoints. Any failure refuses the whole call.

### 6.2 Taxonomy

| Condition | Kwiry `code` | HTTP | OpenClast seam (peer-reported) |
|---|---|---|---|
| No model, wrong identity, no `semantic-onnx`, semantic disabled | `mode_unavailable` | 501 | passed through |
| `mode` omitted (openclast profile) | `invalid_request` | 400 | seam applies D2 first |
| Route or action mismatch; subject not on the allowlist; identity-bearing claim after cutover | `forbidden` / `unauthorized` | 403 / 401 | `search_provider_violation` |
| Non-room filter with semantic or hybrid | `filter_unavailable` (new) | 501 | explicit refusal |
| Lexical control not allowed for the mode | existing `invalid_query` family | 400 | explicit refusal |
| Cursor | `cursor_unavailable` | 501 | `ranked_unpageable` |
| Resource unmapped, not confirmed active, or retired mid-request | `resource_unavailable` | 503 | `search_provider_failed`, no partial re-query |
| Resource `pending` or `unavailable` | `semantic_not_ready` (new) | 503 | per-room disclosure before dispatch |
| Embedding or scan failure, either leg | `semantic_failed` (new) | 503 | one leg down → refuse |
| Request deadline passed | `deadline_exceeded` (new) | 503 | retry or refuse |
| Worker limit `W` reached | `busy` (new) | 503 | retry |
| Capability `exp` passed before response | `unauthorized` | 401 | re-mint |
| Hydration integrity failure | `internal_error` (fixed message) | 500 | refuse |
| No generation yet | `index_building` | 503 | unchanged |

Messages are fixed strings. None contains paths, object IDs, counts, query text or engine text. The envelope `{ "error": { "code", "message" } }` is unchanged.

### 6.3 Scoped readiness probe (A3, closed in revision 2)

`POST /v0/readiness`, action exactly `["readiness"]`. It returns, for each **signed** resource, the exact `{ tenant_id, vault_id, room_id }` tuple sorted bytewise, with `lexical: ready|unavailable`, `semantic: ready|pending|unavailable`, and the `embedding_identity`. It returns no counts, progress, queue positions or timestamps. `/search` re-checks readiness at gate step 6.

## 7. Query path and determinism

### 7.1 Semantic

```text
gate (§6.1) → embed query once (dedicated query embedder)
→ for each resource in E (sorted by ResourceKey), with checkpoints:
     open its vector object read-only + immutable (per request)
     SELECT chunk_id, vec_distance_l2(embedding, :q) AS d
       FROM vectors ORDER BY d, chunk_id LIMIT :C        -- progress handler interrupts
→ merge: concatenate, sort by (d, chunk_id), truncate to C (= 100)
→ strict hydration (§8) → truncate to limit
```

Every global top-C element lies in its own resource's top-C, so the merge equals an authorized-only physical baseline. Work is O(Σ chunks in `E`).

### 7.2 Floats and ties

- Writes reject non-finite components and wrong dimension counts.
- A non-finite query vector returns `semantic_failed`.
- The distance is L2 over normalized vectors, which orders like cosine. The f32 result widens exactly to REAL.
- `-0.0` is canonicalized to `+0.0`.
- SQLite sorts with `ORDER BY d, chunk_id` (REAL, BINARY). The Rust merge sorts by `total_cmp` on canonical `d`, then `chunk_id` bytewise.
- Chunk IDs are unique within a generation, so the order is total.
- `score = 1.0 − d` (f32) is a semantic distance score; ordering never reads it.
- Byte equality is promised for the same binary, `embedding_identity` and CPU feature set.

### 7.3 Hybrid

The lexical leg is IG-1 authorized lexical-v2 over `E` (limit `C`, `>field` emphasis allowed). The semantic leg is the §7.1 list, built from the stripped query text. The two are fused with `rrf_fuse_traced(lexical, semantic, 60.0)`:

- ranks are 1-based;
- each contribution is `1.0 / (60.0 + rank)` in f64, summed lexical then semantic;
- the sort is descending by `total_cmp`, then `chunk_id`;
- scores are cast to f32 after ordering.

Both legs use the same leased objects. Any leg failure, or any non-ready resource, refuses the request.

### 7.4 Excerpts

Each hit is hydrated and excerpted from its own resource's leased lexical object and searcher. A hit is one passage from one file.

### 7.5 Actions and effective mode

- On `/v0/search`, the action is exactly one of `search:lexical | search:semantic | search:hybrid`, and it equals `mode`.
- On `/v0/readiness`, it is exactly `readiness`.
- A capability is valid on one route only.
- Successful searches return `X-Kwiry-Effective-Mode`.
- Kwiry returns the frozen hit schema to the server-side gateway. The OpenClast provider strips `score` and `chunk_id` at the seam.

## 8. Hydration and end-of-request checks

### 8.1 Grouping

Candidates stay grouped by their resource. Lexical hits map back through `vault_id`, which is unique per capability (A2). Each candidate is looked up only in its own resource's leased lexical object.

### 8.2 Resource checks

The stored `(vault_id, room)` must equal the resource (`validate_document_resource`). The resource must belong to this request's leased `E`. **Every candidate must hydrate;** a missing candidate is a `500 internal_error` for the whole call.

### 8.3 End of request

Before responding, Kwiry checks three things:

- every resource in `E` is still mapped in the then-active layout and confirmed active, otherwise `503 resource_unavailable`;
- `exp` has not passed, otherwise `401`;
- the deadline has not passed, otherwise `503 deadline_exceeded`.

OpenClast's grant-level post-check returns `search_authorization_changed` on any narrowing. Kwiry never emits that code.

### 8.4 Freshness of superseded leases (finding 10)

For each resource `r` in `E`, the response computes a per-resource state: the A4 transition state of `r`, raised to at least `stale` if the objects leased for this request's **mode class** have been superseded in the then-active layout.

- Class L (lexical) compares only the lexical object, so an unrelated vector publication never downgrades a lexical response.
- Class S (semantic and hybrid) compares both objects.

The header reports the worst state over `E` (`stale` > `reconciling` > `current`) together with the configured evidence basis: `X-Kwiry-Index-Freshness: <state>; basis=<basis>`. The contract's freshness evidence basis is preserved. The generation digest describes the objects actually served, which are the leased ones.

## 9. Cross-file content check

Unchanged and confirmed by both reviews: no cross-file content reaches chunk text, embedding input or excerpts. The S4 regression test (embed and link fixture, vectors bitwise identical across target presence) stays.

## 10. Threat model and side channels

**Guarantee.** For a fixed binary, `embedding_identity` and CPU feature set, the hits, order, truncation, coverage and scores of a request equal those of a physical instance holding only `E`. They are unchanged when forbidden resources are added, removed or mutated.

**Stated limit (owner decision; A5).** When a resource first answers semantic and hybrid requests depends on shared indexing load, including resources the caller cannot see. Watching one's own rooms become ready can reveal coarse facts about concurrent indexing elsewhere: roughly how many rooms are pending, and for how long. Fair scheduling and bounded work units (§4.9) bound this. They do not eliminate it.

| Channel | Response |
|---|---|
| Readiness timing | Accepted under A5. Fair rotation; the probe returns enums only; advisory witness (§12.2). |
| Query embedder | A dedicated instance is required before HTTP. It is **necessary but not sufficient.** Still shared: CPU cores and SMT siblings, memory bandwidth and last-level cache, page cache and disk I/O, the blocking pool and worker limit `W`, the allocator, and the single query model queue (co-user query load shows up as latency). These are documented residuals. Builders run at lower OS priority. |
| Lifetime and retirement | Request and build leases, admission barriers, reachability GC. Retiring room B never breaks room A's request. |
| Custody query | Server-to-server, triggered by startup, timers and capability arrival. Users see no response content. Coalesced, so a request cannot force repeated queries. |
| Reader init | Per-object init cells. |
| Caches | None: no query-embedding, result or fusion cache. |
| Counts and errors | Byte-equal to the baseline. Fixed messages over `E` or `S` only. |
| Headers | A4 authorized-view digest and per-resource freshness. Class L ignores vectors. Desktop is unchanged. |
| Logs | No subject, actor, query text or fingerprint, query vectors, distances or chunk IDs. Resources appear only as per-process keyed digests. |
| Purge residue | §5.6 and §5.7 protocol with an exact allowed residual. |

## 11. Research: indexing order and work units (options, not decisions)

The owner decided on **fair sharing**. Round-robin is this design's **recommendation** for implementing it, not an owner decision.

### 11.1 Which rooms first

| Policy | Benefit | Cost | New cross-room channel? |
|---|---|---|---|
| **Round-robin (design recommendation)** | Bounded turns; simple | Small rooms wait behind many pending rooms | Only A5's: the count and duration of pending rooms |
| Smallest-first | Best mean time-to-ready | Large rooms starve without aging | **Yes:** relative sizes of other rooms |
| Demand-driven or recently requested | Indexes what is used | Needs per-room demand state | **Yes, the strongest:** other users' search activity, visible through readiness timing |
| New-enrollment priority | New rooms useful sooner | Bursts starve others | **Yes, mild:** enrollment events. A bounded share (1 slot in every k) weakens it. |
| Deficit round-robin by cost units | Same fairness, insensitive to unit variance | More bookkeeping | Same as round-robin |

**Recommended first-slice default:** round-robin, one bounded work unit per turn, newcomers at the tail. Any other policy requires measurement and an owner amendment to A5. Demand-driven priority would also need its own threat review.

### 11.2 Bounded, resumable work units

- A unit is at most `U_max` cost units (§4.9) and stays within one resource. A source larger than one unit is split at batch boundaries in canonical order, so its vectors are identical to an unsplit run.
- Each unit appends its rows and its cursor to a build journal in `staging/<build-id>/` in one SQLite transaction.
- On restart, a journal is resumed only if its resource is confirmed active and its lexical object, `preparation_identity` and `embedding_identity` still match. Resuming re-registers it as a build root. Anything else is garbage.
- Cancellation happens at yield points: retirement, a superseded lexical object, or shutdown.

### 11.3 Measure (S5, S8)

- Embedding throughput on the declared reference hardware.
- Cost-unit calibration: wall time per unit for each phase.
- Unit durations at `U_max` ∈ {128, 256, 512}.
- Time-to-ready versus `P` and room size, for both reused and fresh rooms.
- Query p50 and p99 under builder load, with and without the dedicated embedder.
- RSS.
- Staging fsync cost.
- Resume overhead after a forced kill.
- Exact-scan latency at 25k and 100k chunks and at 256-resource fan-in.
- Custody query latency, and the effect of an unreachable endpoint.

## 12. Slices

**Intentional changes to governed public behaviour in shipped releases:**

- **S1a:** generation and freshness header semantics change.
- **S1b, then the S3 cutover:** identity-bearing capabilities become invalid after the coordinated OpenClast transition.
- **S2:** a one-time rebuild, and the header value changes once at migration.
- **S3:** resources not confirmed active are withheld.

Every vector-building, vector-publishing and vector-serving path is compiled only under the non-default Cargo feature **`governed-semantic-internal`**, which is excluded from default and release feature sets. So no shipped release publishes a vector object before S10. Class L headers also ignore vector objects by construction.

Desktop keeps its current headers and freshness in every slice. Desktop regression tests run on every slice, including the empty-resource-set desktop principal.

| Slice | Content | Build | Tests |
|---|---|---|---|
| **S0** | Amendment text A1–A6 | — | — |
| **S1a — scoped lexical headers (A4), transitional identity** | `X-Kwiry-Generation: v1-<hex>` over a **transitional per-resource revision**: SHA-256 over the resource's manifest entries in `source_key` order (`source_key`, `content_hash`, format identity, `chunk_count`, outcome, coverage), plus the manifest, index, chunking and preparation versions. Every field is u64-LE length-prefixed. Per-resource freshness. Explicit profile branch: desktop unchanged. | shipped | invariance under forbidden publish, reconcile and watcher events; desktop header regression, including `resources: []`; IG-1 witness re-run |
| **S1b — capability context, A2 phase 1** | Workload allowlist; context keeps `exp` and action; repeated `vault_id` refused; subject and actor removed from logs. Legacy identity claims accepted but ignored and never logged, until cutover. | shipped | allowlist; issuer, audience, TTL and algorithm restrictions unchanged; legacy accepted and unlogged; `exp` and action present |
| **S2 — objects, ownership, migration (lexical)** | §4.1–§4.5, §4.10; worker-owned leases, cancellation, `W` limit; per-object init cells. The header switches to `v2-<hex>` over object IDs. | shipped | barrier, timeout and disconnect at pin, open and hydrate; lock-order tracker; durability-order crash injection; GC of crash orphans; interrupted upgrade; rollback; desktop regression; IG-1 W1–W13f |
| **S3 — custody seam and purge (lexical); A2 cutover** | §5 in full; legacy claims rejected after OpenClast confirms its issuer transition. | shipped; needs the OpenClast endpoint | all §5.6 tests; regression, unreachable and no-cache cases; purge witness over the §5.7 locations |
| **S4 — vector object store** | Plain-table store, scan, descriptor; cross-file test | internal | rowid independence, boundary ties, ±0.0, non-finite, no sidecars |
| **S5 — builder and scheduler** | §4.7–§4.9, §11.2 | internal | inventory failures; reuse-identity tests; paused-builder admission; reused-room fairness; resume; embed barrier; measurements |
| **S6 — real-ONNX gate** | §4.7 gate, `B = 1` fallback | internal, mandatory before S8 | fresh process, build, incremental, reuse, permuted order |
| **S7 — governed query path (core)** | §6–§8 | internal | physical baseline A (rooms 1–4) versus B (authorized rooms 1–2), semantic and hybrid; forbidden add, remove and mutate; two-user; fan-in; filter refused before embedding or open; timeout and disconnect at embed and scan; superseded-lease freshness by class |
| **S8 — query embedder, HTTP, probe** | Dedicated embedder; `/search` semantic and hybrid; `/readiness` | internal | one server test per §6.2 row; probe returns signed tuples only; latency under load |
| **S9 — observer and witness** | §12.1–§12.2 | internal + observer | shipped versus observer functional digest |
| **S10 — enablement** | Removes the internal gate after owner review | owner checkpoint | full witness on the release build |
| **S11 — later** | Pre-scan filter rowsets | — | baseline with filters |

Dependencies:

- S1a and S1b need A4 and A2.
- S3 needs the OpenClast custody endpoint (§14) and A6.
- S8 needs A1, A3 and S6.
- S9 needs the internal observer, which today exists only on the unmerged `feat/ig1-witness-observer` branch (`5ee249e`).
- S10 needs every amendment.

### 12.1 Observer events (feature-gated)

The existing events gain the following, and the invariant stays zero lease, open and scan events for any resource outside `E`:

- `lease_acquired` and `lease_released` (with a worker-ack flag);
- `vector_gate_ready` and `vector_gate_refused`;
- `vector_open`, `vector_scan` and `vector_refused`;
- `query_embed`;
- `fusion` (list lengths only);
- `cancel_checkpoint`;
- `end_recheck_ok` and `end_recheck_refused`.

### 12.2 Witness mapping

The mapping is by theme until OpenClast shares its V1–V16 numbers.

| Theme | Kwiry evidence | Extends |
|---|---|---|
| Two-user vector and hybrid | Asymmetric requests; leases and opens only within `E` | W2, W8 |
| Forbidden-partition invariance | Byte-equal results against an authorized-only instance under forbidden add, remove and mutate | W5 |
| **Readiness under forbidden load (owner decision)** | Instance 1: pending room A plus a large forbidden resource that sorts before A. Instance 2: only A. **Assert** exact equality once A is ready on both. **Record** time-to-ready as advisory only. | new |
| Mid-request retirement, timeout, disconnect | Forbidden room B retired at each barrier while A's request succeeds exactly. A's own room retired → `resource_unavailable`. Timeout and disconnect → bounded refusal, leases released on worker acknowledgement. | W8, W13a |
| Retirement purge | Custody marks B retired. Data root inspected at **every §5.7 location: generation records, layouts, `current.json`, object descriptors, lexical manifest shards, staging journals, quarantine, custody cache, upgrade records, layout-v2 trees and manifests, legacy `semantic/`.** No artifact names or contains B. Every remaining object's descriptor resource is confirmed active and mapped. Purge-completed is reported only after durable deletion. Crash-injected and deletion-failure runs quarantine and keep healthy rooms and lexical served. *(Ownership and artifacts are checked, not chunk IDs, which omit the room.)* | W13a–W13f |
| Custody seam | Regressed revision refused; unreachable endpoint → serving continues, building frozen; no cache → fail closed; capability for a not-yet-cached resource → coalesced query | new |
| Cross-file embeds | Literal text only; vectors identical across target presence | new |
| Audit and storage privacy | Logs contain no subject, actor, query text or fingerprint. No query embedding is ever persisted, and no diagnostic vector dump exists. Passage vectors exist only in live vector objects and live build staging. | W11 |
| Named errors, no fallback | `mode_unavailable`, `semantic_not_ready`, `semantic_failed`, `deadline_exceeded`, `busy`, `filter_unavailable`, `ranked_unpageable` | W10 |
| Shipped = observer | Identical functional digest | W12a, W12b |

W1, W3, W4, W6, W7 and W9 are re-run as regression.

## 13. Contract amendment package A1–A6

Flagged only. `CONTRACT.md` is not edited here.

### A1 — governed modes and default (§4, §11)

Admit `semantic` and `hybrid` in the OpenClast profile as designed here. `mode` is required at Kwiry in that profile, and D2's default is applied at the seam. Desktop keeps its default unless the owner extends D2 (Q1).

### A2 — capability identity and verified context (§5)

- **Unchanged:** the existing issuer, audience, TTL, `kid` and algorithm restrictions.
- **Claims:**
  - The capability carries no user identity.
  - `sub` must match an entry in a configured **workload-subject allowlist** exactly and bytewise.
  - `jti` is a per-request random nonce.
  - The verified context preserves `exp`, the exact action, `jti`, resources and `max_limit`.
  - Each `vault_id` appears at most once.
- **Transition, coordinated with OpenClast:**
  - In phase 1 (S1b), legacy `actor` and `subject_revision` claims are accepted, ignored and never logged.
  - In phase 2 (S3), after OpenClast confirms its issuer emits only the workload form, identity-bearing or unknown claims are refused with `401`.

### A3 — scoped readiness probe (§4)

`POST /v0/readiness` with action `readiness` returns closed per-resource enums for the exact signed tuples (§6.3). Search actions are valid only on `/search`.

### A4 — scoped generation and freshness headers (§4; IG-1 doc)

**Profile branch.** The desktop profile keeps today's global generation ID and daemon-wide freshness unchanged. Everything below applies to the OpenClast profile only.

**Encoding.** Every length prefix and count is a **u64, little-endian**. The class is one byte.

```text
digest = SHA-256(
  "kwiry-authorized-view-v2\0"
  ‖ class:u8                       # 'L' = lexical, 'S' = semantic|hybrid
  ‖ count:u64le                    # |E|
  ‖ for r in E sorted bytewise by (tenant_id, vault_id, room_id):
        len:u64le ‖ tenant_id ‖ len:u64le ‖ vault_id ‖ len:u64le ‖ room_id
        ‖ len:u64le ‖ lexical_object_id
        ‖ [class S only] len:u64le ‖ vector_object_id
)
X-Kwiry-Generation: v2-<lowercase hex>
```

The transitional S1a form `v1-<hex>` uses the domain `"kwiry-authorized-view-v1\0"` and the per-resource revision (§12) in place of object IDs. Both forms are computed over the **effective set** `E`, using the objects actually served.

**Per-resource freshness transitions:**

- `current → reconciling` when a pass whose read scope includes the resource's vault starts. Path-scoped passes include only vaults with changed paths. Full passes are triggered by boot or schedule.
- `reconciling → current` when that pass publishes with no remaining dirty state for the vault.
- `reconciling → stale` when the pass ends incomplete: incomplete enumeration, retained content, unavailable vault, or publication frozen by custody (§5.4).
- `current → stale` on a watcher event for the vault.
- `stale → reconciling` when the next covering pass starts.
- Any state becomes `unavailable` when the resource is unmapped or not confirmed active. Such a resource is never served.
- A superseded leased object raises the resource to at least `stale` for its mode class (§8.4).

The header reports the worst state over `E` with the configured evidence basis.

### A5 — semantic readiness-timing limit (owner decision 2026-11-17)

Semantic and hybrid results are exactly invariant to forbidden resources. Readiness *timing* is not. A fair shared scheduler builds vectors, and when a resource first answers rather than returning `semantic_not_ready` may depend on server load, including indexing load from resources the caller cannot see. Kwiry bounds this with fair scheduling and bounded work units, exposes readiness only as closed enums, and makes no claim of timing invariance. A scheduling change that adds a channel (§11) requires amending A5.

### A6 — retirement, durable purge, custody cache (§1.1, §5)

- **Authority.** OpenClast is the authority on enrolled, active and retired resources. Kwiry builds and serves only resources confirmed active at the latest custody revision it has seen. It never accepts a lower revision.
- **States.** Retirement has two separately reported states:
  - **retrieval revoked:** no new request can use the resource, and in-flight requests are refused at their end-of-request check;
  - **purge completed:** every data-root artifact naming or containing the resource is unlinked, the deletions and parent directories are durably synced, no lease remains, and a final scan is clean.
- **Honesty.** Kwiry never reports purge completed after any deletion or sync failure. Failed artifacts are quarantined, only the retired resource is affected, and purge-incomplete is reported.
- **Durable completion record.** It lives with the authority (OpenClast), not in Kwiry's disposable data root.
- **Clarification of §1.1.** Kwiry's derived state may include a **disposable cache of authority-issued lifecycle state** (the custody revision and active set). Deleting it never widens access: Kwiry fails closed until a live query succeeds. Rebuild converges given the same files and the same custody revision.
- **Scope of purge.** Purge is file unlinking, not cryptographic erasure.

**Not amendments:**

- refusing non-room filters, which explicit refusal already covers;
- exact baseline equality, which is stronger than §1.1;
- `ranked_unpageable`, which is an OpenClast mapping.

## 14. OpenClast seam asks

The final list for OpenClast's endpoint design and independent security review:

1. **Custody query** for provider `kwiry`, server to server.
   - **Request:** a Kwiry-generated random nonce.
   - **Response:** signed, binding the provider ID, tenant, a **strictly monotone custody revision** (u64), `issued_at` and `expires_at`, the nonce echo, the **active set** (exact `{tenant_id, vault_id, room_id}` tuples) and the **retired set** (tuples only; no labels or names).
   - **Revision rules:** the revision advances on every enrollment or retirement and never decreases, across restarts and failover.
   - **Retired-set scope:** the set must include every retirement Kwiry could still hold artifacts for. The complete provider history is simplest.
2. **Separate credentials.**
   - A dedicated provider client credential, scoped to reading this provider's custody only.
   - A response-signing key published in a JWKS distinct from search capabilities, or a distinct `typ` and audience, so a custody document can never verify as a capability and vice versa.
   - A rotation procedure for both.
3. **Typed failures** that let Kwiry distinguish unauthorized, rate-limited and unavailable.
4. **Purge-completion acknowledgement.** An optional endpoint where Kwiry reports "as of custody revision R, purge-completed for these retired tuples" or "purge-incomplete". OpenClast records it durably as the A6 completion record, and its audit can show retrieval-revoked separately from purge-completed.
5. **Capability transition (A2).** The issuer moves to workload `sub`, with no `actor` or `subject_revision`. OpenClast tells Kwiry when legacy claims are no longer issued, so Kwiry can start refusing them (S3).
6. **Retrieval seam behaviour:**
   - the `retrieval` field with the D2 default applied at the seam;
   - mode bound to the capability action;
   - the effective mode echoed (`X-Kwiry-Effective-Mode`), with a mismatch raising `search_provider_violation`;
   - `score` and `chunk_id` stripped;
   - the §6.2 error mapping, including `deadline_exceeded` and `busy`;
   - readiness probe consumption before dispatch;
   - the grant-level post-check returning `search_authorization_changed`.
7. **Headers.** Forward only the A4 scoped generation and freshness headers.
8. **Threat model.** Record the A5 readiness-timing limit in the seam's threat model.
9. **Audit.** Confirm the unsampled custody-query audit records no Kwiry-side content beyond provider identity, revision and outcome.

## 15. Risks and open questions

**Risks**

- **R1 — ONNX determinism.** Highest technical risk. S6 is a hard gate; fallback `B = 1`, otherwise re-scope with the owner.
- **R2 — Storage transition.** Roughly twice the disk during migration, IG-1 re-witness required, fast rollback lost on retirement (§4.10).
- **R3 — Memory.** Two embedders plus the sidecar, measured in S8.
- **R4 — Time to ready under load.** The A5 limit. Lexical search is unaffected.
- **R5 — Purge latency.** It depends on worker acknowledgement (§4.4) and is not time-bounded. Refusal is bounded.
- **R6 — Custody dependency.** An unreachable endpoint freezes building after `custody_max_age`, so lexical freshness turns `stale`. A fresh data root with no reachable endpoint serves nothing.
- **R7 — Observer not on `main`.** It blocks S9.
- **R8 — Existing IG-1 leaks.** S1a and S1b fix them regardless of whether IG-2A proceeds.
- **R9 — Cross-repository sequencing.** S3 waits on the OpenClast endpoint and its security review.

**Open questions**

- **Q1** — Should D2 also change the desktop default?
- **Q2** — Should Kwiry itself omit `score` and `chunk_id` in the governed profile, as defence in depth? That would need a hit-schema amendment.
- **Q3** — Do we want per-resource encryption keys with crypto-shredding later?
- **Q4** — Desktop labels L2-derived scores as "cosine similarity". Fix that separately?
- **Q5** — Rename the `openclast` profile to a generic governed profile. New identifiers should use the generic name.
- **Q6** — Should the advisory time-to-ready evidence feed a published operator expectation?
- **Q7** — Should search workers move into terminable processes, so purge completion can also be time-bounded?
