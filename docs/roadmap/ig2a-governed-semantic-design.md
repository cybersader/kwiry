# IG-2A design — governed semantic and hybrid retrieval

Status: **design, revision 4, owner review pending.** Nothing here is implemented or approved. This page expands [IG-2](governed-semantic-hybrid.md) and is bound by [`CONTRACT.md`](../../CONTRACT.md), which this design does not edit. The amendments it needs are in [§13](#13-contract-amendment-package-a1a7). What it asks of OpenClast is in [§14](#14-openclast-seam-asks).

## Revision history

| Revision | Head | Independent review result |
|---|---|---|
| 1 | `1442e668` | NO-SHIP. Core approach judged sound. |
| 2 | `b70c902d` | NO-SHIP, converging. |
| 3 | `82f53cb4` | NO-SHIP. Closed: timeout vs leases, retirement vs admitted builder, manifests, reuse identity, permanent tombstones, failed-purge startup, effective vs signed set, superseded freshness, A2 issuer/TTL retention, A3, and A4 encoding with desktop branching. |
| 4 | this page | Answers the third review's F1–F16 and finalizes the OpenClast seam asks. Architecture, P-a and P-b are unchanged. |

§16 lists the properties that are closed in the design but can only be proven by tests.

| Third-review item | Resolved in |
|---|---|
| F1 Atomic pin-and-lease; live → doomed claim | §4.3 |
| F2 In-process publisher serialization | §4.3 |
| F3 Rollback can restore retired content | §4.10, A6 |
| F4 Migration recovery matrix | §4.10 |
| F5 Recursive object sync before rename | §4.2 |
| F6 Lexical must not require the model | §6.1 |
| F7 Separate lexical capacity; `busy` residual | §4.4, A7 |
| F8 Allowlist vs legacy capabilities | §12, A2 |
| F9 Canonical custody state equality | §5.3, §5.5 |
| F10 Stale-authority window during custody outage | §5.4, §14 |
| F11 Reader cells on doomed objects | §4.3 |
| F12 Lifecycle rules before custody | §12 (S2 and S3 ship as one release unit) |
| F13 Fixed-cost finalization | §4.9 |
| F14 Mandatory durable purge reporting | §5.6, A6, §14 |
| F15 Transitional identity must cover mtime | §12 (S1a) |
| F16 Capability expiry vs execution deadline | §4.4, §6.2, §8.3 |

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

Each comes with a recommendation, or with a default the owner can override.

1. **Approve amendment package A1–A7 (§13).**
   - A1: semantic and hybrid in the governed profile, with an explicit mode.
   - A2: capabilities name a workload, never a person, after a bounded transition.
   - A3: a scoped readiness probe.
   - A4: room-scoped generation and freshness headers.
   - A5: the readiness-timing limit.
   - A6: retirement, durable purge, the custody cache and the rollback rule.
   - A7: disclosure of the service-load residual.

   *A2 and A4 also change the existing lexical endpoint (§12).*
2. **Move to per-resource objects, as a recoverable storage transition (§4.10).**
   - **Disk headroom:** roughly twice the governed lexical index while old and new layouts coexist.
   - **Rollback among new releases:** always allowed, to any release at or above the minimum rollback-compatible binary.
   - **Fast rollback to the old layout and a pre-custody binary:** allowed only until the first retirement is *accepted*. It is then durably disabled before retrieval is revoked.
   - **Rebuild fallback:** always goes through a release at or above the minimum, using a fresh authority-confirmed active set.
   - The IG-1 witness rows must be re-run.

   *Recommendation: yes.*
3. **Refuse rather than serve stale vectors** when a room's vectors lag its text. *Recommendation: refuse for IG-2A.*
4. **Pay the RAM for a dedicated query embedder.** It is a prerequisite for HTTP. *Recommendation: yes; measure in S8.*
5. **Reject non-room filters** for semantic and hybrid in IG-2A, before embedding and before any store opens. *Recommendation: yes.*
6. **(P-a, default) Tombstone authority stays with OpenClast.** Kwiry keeps only a disposable custody cache (§5). *Override only if the owner wants Kwiry to keep its own record.*
7. **(P-b, default) A failed purge quarantines only the retired resource.** Healthy rooms and lexical search stay online (§5.6). *Override only if the owner prefers a fail-stop.*

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
- **Publication.** `DataRootLock` is a file lock that only excludes other processes. In-process publishers (desktop and openclast index managers) are not serialized by it. The `kwiry index` path and the daemon are separate processes.
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
  upgrade.json, current.v2.json, rollback.json   # migration and rollback markers; §4.10
  models/                      # model artifacts only
```

- **A generation record** is small and immutable. It holds the format versions, `state_revision`, `last_sync`, and the layout: resource tuple → `{ lexical_object, vector_object? }`. It contains **no** paths, hashes, lengths or timestamps of sources. Those live in the lexical object's own `manifest.json` shard, so they are deleted together with the object.
- Object IDs are fresh random values and never reused. No filesystem name carries a tenant, vault, room or partition digest.
- Objects are immutable once published. Vector stores open with `file:…?mode=ro&immutable=1`, so no SQLite sidecar files appear.

### 4.2 Durability order (F5)

Each step completes before the next one starts. Only the publisher (§4.3) performs steps 4 and 5.

1. Write the object's files under `staging/<build-id>/<object-id>/`.
2. **Run a recursive `sync_tree` over the staged object directory.** This is the existing `state.rs` helper: it refuses symbolic links and unsupported entries, fsyncs every file, then fsyncs every directory bottom-up. It covers nested lexical index files, such as Tantivy segment files under `index/`.
3. Rename `staging/<build-id>/<object-id>/` to `objects/<object-id>/`. Then fsync **both parents**: `staging/<build-id>/` and `objects/`.
4. Write the generation record: temporary file, fsync, rename into `generations/`, fsync `generations/`.
5. Write `current.json` atomically (temporary file, fsync, rename) and fsync the data root.

If a crash happens at any point, every object is either fully durable or unreferenced garbage (§4.5).

**Tests (S2):** inject a crash after each step and an fsync failure at each step. Assert that no generation ever references an object whose nested lexical files are not durable, and that an fsync failure aborts the publish.

### 4.3 Ownership, publisher, locks and admission

**Owners.** Nothing is deleted while any of these holds an object:

- **Request leases**, taken by search workers (§4.4).
- **Build leases**, taken by builders on their input lexical object and on any vector object they reuse, for the whole build.
- **Build roots**, which own `staging/<build-id>/` and every object renamed from it until a publication references it, or until the builder abandons it (*publication ownership*).
- **Reader handles**, which are open readers of an object, including an idle cached reader held in its init cell.

**Object registry.** An in-process registry holds, for each object, its state (`live`, `doomed` or `deleted`), its lease count, its handle count, its reader init cell, and the generations that reference it. It is guarded by **L3**, the innermost lock. L3 is held only for in-memory bookkeeping and is **never held across I/O, fsync, deletion or any wait.**

**Single publisher (F2).** The existing `DataRootLock` is a file lock that excludes other processes. It does not serialize callers inside one process. IG-2A therefore adds one **publisher actor**: a single task that holds the `DataRootLock` for the life of the process and is the **only** writer of `generations/` and `current.json`.

Reconcile, vector attach, retirement revoke, the migration switch and generation deletion by GC are all **transactions submitted to the actor**. The actor runs them one at a time. Each transaction is one serialized unit:

1. revalidate the layout against the current active generation;
2. revalidate custody (§5): every resource the new layout maps must be confirmed active at the latest accepted revision;
3. build the generation record;
4. publish it (§4.2, steps 4–5);
5. under L3, swap the in-memory active pointer and update object reference sets.

The actor reads admission gates only as a non-blocking state check. **The actor never waits on admitted operations, leases, handles or other transactions inside a transaction.** Anything that must wait does so outside the actor, holding no lock.

**Admission gates (L2).** Each resource has a small mutex and condition variable, with a state (`open`, `closing` or `closed`) and a count of admitted operations. A builder or attach step takes an **admission token** first, and the token is refused unless the gate is `open`. Holding the token, the builder re-checks custody, performs its filesystem work, and may submit an attach transaction and wait for the actor's answer. The actor never needs that token, so there is no cycle. The builder releases the token afterwards.

Retirement closes the gate and waits for the admitted count to drain while holding nothing (§5.6).

**Lock order:** actor serialization → L2 → L3. No lock is acquired while holding a later one, and no wait happens while holding L2 or L3.

**Linearized pin-and-lease (F1).** `pin_and_lease(E, class)` is one critical section under L3:

1. read the in-memory active pointer **inside** L3. The actor swaps that pointer only under L3, so the pointer and object states form one consistent snapshot;
2. resolve the objects of `E` for the mode class;
3. verify that every one of them is `live`;
4. increment their lease counts.

If any object is `doomed`, nothing is leased. The call releases L3 and retries against the then-current pointer, up to 3 times. After that it refuses with `503 index_building`. A worker can never load a generation outside L3 and register a lease against it later: the load and the registration are the same critical section.

**Atomic live → doomed claim.** GC and retirement never delete an object they have not claimed. Under L3, an object becomes `doomed` only if all of the following hold:

- its lease count and handle count are zero;
- no active generation, retained generation or build root references it;
- or, for retirement, its resource has been revoked.

Lease acquisition and reader initialization refuse `doomed` objects. Deletion happens after L3 is released, and only for objects this caller claimed.

**Reader init cells on doomed objects (F11).**

- The map lock is held only to find or insert a cell, and the open runs inside the cell (closed in revision 2).
- On doom, under L3, the cell is **evicted** from the map and marked doomed. Re-initialization of a doomed object is refused.
- The cell's own handle is released at eviction. Deletion waits until the handle count reaches zero, which means every reader, including idle cached ones, has closed its files and mappings.
- Purge completion (§5.6) requires that drain.

**Tests (S2 and S3):**

- **F1 interleaving.** Worker W reads generation G and pauses, by test hook, *before* registering. G becomes unretained. GC runs. W then registers. In the linearized design, W either never held G outside L3 (its pin re-reads the pointer and leases G′) or, through a deliberately stale test path, presents G's objects and is refused as `doomed` and retries. No lease ever succeeds on a doomed object. No leased object is ever deleted.
- **Publisher serialization.** Two concurrent in-process publishers (reconcile and attach) submit transactions. Every published generation passes revalidation, and none overwrites another's swap.
- **Admission.** A builder is paused after its custody and gate check but before its filesystem operation, with retirement started. Retirement waits at drain. The builder finishes. Retirement deletes what the builder wrote and its final scan is clean.
- **Idle reader (F11).** An object is opened by a request that then finishes, so its reader is idle and cached with open mappings. The object is doomed. The test asserts the cell is evicted, re-initialization is refused, and the handle count drains before deletion. The final scan also confirms that no process file descriptor refers to the object.
- **Reader isolation.** Room B's reader init is blocked while room A's cached access completes.
- **No lock held across I/O.** A debug lock tracker asserts that L3 is never held across I/O and that the actor never waits inside a transaction.

### 4.4 Request workers, deadlines and capacity (F7, F16)

**Leases are owned by the worker.**

- The handler creates a cancellation token and a drop guard. If the handler future is dropped, by client disconnect or the HTTP stack, the guard cancels the token.
- The `spawn_blocking` worker calls `pin_and_lease` itself and holds the leases and reader handles in RAII guards.
- The guards release on return, error, cancellation, and panic unwind (the workspace uses the default `panic = "unwind"`).

**Cancellation checkpoints:** after pin and lease; before embedding; after embedding; before each resource open; inside each vector scan (a SQLite progress handler fires every *N* VM steps); between resources; around the lexical leg; before each resource's hydration; before response assembly.

**Uninterruptible steps**, each bounded by input size: query-embedding inference (one input of at most 512 tokens), one resource's Tantivy search, and one resource's hydration.

**Two different limits (F16):**

- **Authorization limit:** the capability's `exp`, kept in the verified context.
- **Execution budget:** `start + max_execution`. `max_execution` defaults to 30 s and is configured below the gateway timeout.

The handler waits for the worker until the **earlier** of the two:

- If `exp` is reached first, or both have passed when checked, the handler returns `401 capability_expired`. **Expiry takes precedence**, because authorization is gone, and the gateway may re-mint.
- If the execution budget runs out while the capability is still valid, it returns `503 deadline_exceeded`.

Both cancel the worker. The same precedence applies at the end-of-request check (§8.3).

**Promise:** refusal within the earlier limit. Purge completion is **not** time-bounded, because it waits for workers to acknowledge cancellation (Q7).

**Separate, fair capacity (F7).** In the openclast profile:

- **Two pools,** each a FIFO semaphore: lexical `W_L` and semantic `W_S`. A hybrid request uses `W_S`, and its lexical leg runs inside that worker. A lexical request never waits on `W_S`.
- **A permit is acquired in the async handler before `spawn_blocking`.** It moves into the worker and is released only when the worker exits. A cancelled but still-running worker keeps its permit, so stalled semantic workers can exhaust only `W_S`.
- **Fair admission:** each pool queues FIFO, up to `Q` waiters. A waiter waits for a permit at most until the earlier of its two limits. Beyond `Q` waiters the request is refused immediately with `503 busy`.
- **Desktop** keeps today's behaviour, with no pools.

**Residual (A7).** Queueing delay, `busy` and `deadline_exceeded` depend on aggregate request load per pool, including other users' requests on other rooms. They reveal request volume and timing, not corpus content and not which rooms are busy. This residual is separate from A5's readiness-timing limit and is disclosed separately.

**Tests (S2 lexical, S7 semantic):**

- At each barrier (pin, embed, open, scan, hydrate), inject: execution-budget expiry; capability expiry; both at once (expect 401); client disconnect; a panic.
- Assert: refusal within the limit; leases, handles and permits released only on worker acknowledgement; registry counts return to zero; a waiting purge completes after acknowledgement.
- Fill `W_S` with workers stalled at the uninterruptible embedding barrier. Lexical requests still succeed at normal latency. Semantic requests queue and then return `busy` or `deadline_exceeded`.

### 4.5 Reachability garbage collection

An object is reachable if it is referenced by the active generation, by a retained generation that names only resources confirmed active, by a live build root, by a lease, or by a reader handle. Everything else under `objects/`, `staging/` and `generations/`, plus any layout-v2 tree no longer covered by §4.10, is garbage.

- **At startup**, before the listener binds:
  1. run migration recovery (§4.10);
  2. run custody enforcement through the publisher (§5.6);
  3. resume valid resumable staging as build roots (§11.2);
  4. claim (`live → doomed`) and delete garbage.
- **Periodically, and after every publication**, the same rules apply. Generation records are deleted only through publisher transactions.
- A retained generation that names a resource not confirmed active is itself garbage. Retention for crash recovery never outranks retirement.
- A deletion failure moves the artifact to quarantine (§5.6). GC never blocks serving.

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

### 4.9 Fair shared scheduler and work budget

**Policy.** This is the design's recommendation for implementing the owner's fairness decision. Pending resources sit in a round-robin rotation. Each turn runs one bounded work unit for the resource at the head, then moves it to the tail. New and changed resources join at the tail. Nothing is ordered by key, size or demand.

**Every substantial phase is budgeted.** Each unit has a budget of `U_max` *cost units*:

| Phase | Charged per | Yield point |
|---|---|---|
| Enumerating the lexical object's stored documents | 1 per 64 documents | saved segment and document cursor |
| Copying reused rows | 1 per 256 rows | row offset |
| Embedding | 1 per chunk | batch boundary |
| Inventory validation | 1 per 256 rows | row offset |
| Hashing (`vectors_sha256`, digests) | 1 per MiB | byte offset, streaming |
| Durable row commit | included in the row charges above | each unit ends with one SQLite transaction commit (`journal_mode=DELETE`), so a unit's rows are durable when it ends |
| Final object sync (§4.2) | 1 per file | per file |
| Rename and parent fsyncs | fixed, small | none |

**Finalization is incremental (F13).** Rows become durable as each unit commits, so the final `sync_tree` flushes little or no dirty data. Hashing streams and yields per MiB.

The **unsplittable remainder** of any turn is bounded by:

- one SQLite commit of at most `U_max` rows;
- one fsync of a file whose dirty data is bounded by that commit;
- one rename;
- two directory fsyncs.

Lexical index publication is not scheduled here; it belongs to reconcile.

**Bounded accounting versus measured time.** The fairness claim is stated in **bounded accounting turns**: each turn consumes at most `U_max` cost units plus the bounded unsplittable remainder. Wall-clock time per turn is measured, never promised.

**Claim and its assumptions.** A resource needing `u` units completes within `u` of its own turns. Between two of its turns, at most `P_max − 1` other turns run. Assumptions:

- one builder worker;
- no preemption by other builder work;
- `P_max` is the maximum number of pending resources during the interval, and arrivals raise it.

Elapsed time also depends on contention from queries and reconciliation. That is A5's stated limit.

**Fairness test (S5).** A large **reused** room spends almost all of its budget on enumeration, copying, validation and hashing. Next to it is a small room A. The test asserts that their turns interleave, that no turn exceeds its accounted budget, and that A becomes ready within its bound in turns.

### 4.10 Migration, rollback and recovery (F3, F4)

**Minimum rollback-compatible binary (MRCB).** The MRCB is the first release that contains **both** S2 and S3 (§12): the layout-v3 objects, the publisher, the custody client, retirement and purge, and A2 phase-1 verification. Any release at or above the MRCB enforces custody on startup and before every rebuild.

**Upgrade protocol.** Each marker is written atomically and fsynced. `upgrade.json` holds `{ state, from, to }`.

1. Write `upgrade.json = { state: "building", from: <v2 generation> }`.
2. Query custody fresh (§5.2). Build the layout-v3 objects and generation record for resources confirmed active only.
3. Copy the old pointer to `current.v2.json` and fsync it.
4. Write `upgrade.json = { state: "switching", to: <v3 generation> }`.
5. Switch `current.json` to the v3 generation and fsync the data root. **A crash barrier sits immediately after this step.**
6. Write `upgrade.json = { state: "published" }`.
7. **Confirmation.** `kwiry gc --confirm-upgrade`, or the first successful serve after a configured grace period, deletes the v2 trees, `current.v2.json` and `upgrade.json`.

**Recovery matrix.** The MRCB applies this at startup, before custody enforcement:

| `upgrade.json` | `current.json` points to | Meaning | Recovery |
|---|---|---|---|
| absent | v3 | steady state | none |
| absent | v2 | not yet upgraded | run the upgrade (refuse to serve v2) |
| `building` | v2 | crashed during build | discard unpublished v3 objects and records as garbage; restart at step 1 |
| `building` | v3 | impossible under the protocol | fail closed: refuse to start and tell the operator to re-run the upgrade; v3 is treated as unpublished |
| `switching` | v2 | crashed before the pointer switch | if the target v3 generation validates, resume at step 5; otherwise discard it and restart |
| `switching` | v3 | **crashed after the pointer switch** | roll forward: write `published` (step 6) |
| `published` | v3 | upgraded, unconfirmed | normal; fast rollback still possible unless disabled |
| `published` | v2 | operator restored the old pointer for fast rollback | the MRCB refuses to serve v2; the operator must run the pre-custody binary (if fast rollback is still enabled) or re-upgrade |
| any | missing or invalid | pointer lost | choose the newest valid **v3** generation; never fall back to v2 automatically |

**Fast rollback is durably disabled when a retirement is accepted.** A pre-custody binary cannot enforce retirements, and configuration and source files survive. So **step 0 of every retirement** (§5.6) is to write `rollback.json = { fast_rollback: "disabled" }`, delete `current.v2.json`, and fsync both, **before** the gate closes or retrieval is revoked. From that moment the v2 trees are garbage, and a `published + v2` or manual-pointer attempt is refused.

**Rebuild fallback.** Rebuild is allowed only through a binary at or above the MRCB, which requires a **fresh** authority-confirmed active set (§5.2). Pre-custody binaries are unsupported in the governed profile once the MRCB has run. Deployments must pin that minimum image version.

**Honesty.** A6's purge guarantee holds only while binaries at or above the MRCB operate the data root. Running a pre-custody binary after the upgrade breaks it, and A6 says so.

**Headroom:** roughly twice the governed lexical index until confirmation, or until fast rollback is disabled.

**Tests (S2 and S3, combined):**

- A crash at each step 1–6, including the barrier right after the pointer switch.
- Every row of the recovery matrix.
- **Retirement accepted, then a deletion failure, then a crash, then an attempted fast rollback.** Assert that `rollback.json` disables fast rollback durably, that the v2 trees are garbage or quarantined, that restoring the old pointer is refused, that rebuild requires a fresh custody query, and that the retired room is never rebuilt.

## 5. Retirement authority and purge

### 5.1 The seam (OpenClast answer, peer-reported, design-stage)

- **(b) Mandatory, and already true.** Capabilities only ever name enrolled, active, random IDs. A retired ID is never minted.
- **(a) New.** A signed server-to-server **custody query** for the `kwiry` provider. It returns the provider's active set, retirement information (tombstoned IDs only, no labels), and a monotone custody revision. It uses a dedicated provider credential scoped to reading that provider's custody. Every query writes an unsampled audit event on OpenClast's side.
- **Rule.** Kwiry refuses to index or build anything not confirmed active at the latest revision it has seen.

**Bounded retirement information.** Kwiry does not need OpenClast's unbounded retired history. Excluding anything absent from the active set is equally authoritative for Kwiry's actions. Retirement information matters only to distinguish *retired* from *unknown*, and for purge reporting. Kwiry therefore asks (§14) for retirement status of only the tuples it **holds**: those in its configuration, layout, object descriptors or quarantine that are not in the active set. Each returns `retired` or `unknown`. A full retired set remains acceptable if OpenClast prefers it.

### 5.2 When Kwiry queries

- **At startup**, after migration recovery and before the listener binds.
- **Before every rebuild** (`kwiry index`, an embedding-identity change, the upgrade). A rebuild requires a fresh successful query, never the cache alone.
- **Periodically**, every `custody_poll_interval` (default 60 s).
- **On capability arrival** naming a resource not in the cached active set. This triggers one coalesced, rate-limited query (one in flight, at most one per `custody_min_interval`, default 5 s). The request itself is refused with `resource_unavailable` and does not wait.

### 5.3 Validation and canonical state equality (F9)

**Envelope checks.** Every response must:

- verify against the custody signing keys;
- name this provider and tenant;
- echo the request's fresh nonce;
- fall within its validity window.

Nonce, signature, `issued_at` and `expires_at` belong to the **envelope** and never take part in state comparison.

**Canonical lifecycle state** is the tuple `(provider, tenant, revision, active, state_digest)`, where:

- `active` is the active set of exact `{tenant_id, vault_id, room_id}` tuples, deduplicated and sorted bytewise. Its canonical encoding is a u64 little-endian count followed by u64-LE length-prefixed fields.
- `state_digest` is an **authority-issued opaque commitment** to OpenClast's complete lifecycle state for this provider at `revision`, active and retired together. It is computed by OpenClast with a provider-scoped key, so it cannot confirm guesses about retired IDs (§14 ask 2). Kwiry never computes a digest over retired IDs.

Two responses are **equal** when their canonical states are byte-equal. Set order and envelope fields are irrelevant. Held-tuple statuses are request-dependent, so they are checked for consistency with `active` (disjointness) but are not part of state equality.

| Received versus highest accepted | Action |
|---|---|
| Higher revision | Accept atomically: recompute exclusions (§5.6), replace the cache, set `confirmed_at`. |
| Equal revision, equal state | **Refresh confirmation:** update `confirmed_at` only. |
| Equal revision, different state | Refuse. Raise the alarm `custody_inconsistent` and keep the current state. |
| Lower revision | Refuse. Raise the alarm `custody_regressed` and keep the current state. |
| No cache (fresh or wiped data root) | Accept the first valid, nonce-bound live response. |

**What survives a restart.** The cache keeps `(revision, active, state_digest, confirmed_at)`. After a restart, regression and same-revision inconsistency are therefore still detected against the cached revision. If the cache is lost, that detection resets to "first live response wins". The response cannot be a replay, because it echoes the new nonce. A6 permits exactly this cached commitment and nothing more.

**Tests (S3):**

- reordered active sets and changed nonces, signatures or timestamps compare equal and refresh confirmation;
- an unchanged state refreshes `confirmed_at`;
- changed retirement content at the same revision (a different `state_digest`) is refused;
- a lower revision is refused;
- restart with the cache detects both;
- restart without the cache accepts the first live response.

### 5.4 When the endpoint is unreachable (F10)

- **Keep serving** resources that are already built and confirmed active at the latest accepted revision. Every request still needs a valid signed capability.
- **Build nothing new.**
  - Never enroll a resource that is not in the cached active set.
  - Once `confirmed_at` is older than `custody_max_age` (default 5 min), publish no lexical or vector object until a query succeeds.
  - Refuse rebuilds.
  - Affected resources report `stale` freshness once they have dirty state.
- **The publish freeze is not a revocation bound.** It limits building only.
- **Stale-authority window.** OpenClast never mints a capability for a retired ID (b), so after a retirement the only capabilities that can still name the room are those already issued. Kwiry serves such a capability until it expires, whether or not custody is reachable, because Kwiry cannot learn of the retirement in time. The window after retirement is therefore at most the capability TTL (`max_token_ttl_seconds`, at most 60 s). Kwiry is not the user-facing control for this window. OpenClast's post-check is, and it must fail closed when continuing authorization cannot be established (§14 ask 6). Once custody reports the retirement, Kwiry revokes at once (§5.6), in which case the window is shorter.
- **No cache and endpoint unreachable at startup.** Nothing is confirmed active, so nothing is served or built. Requests refuse with `resource_unavailable` until the first successful query. This fails closed.
- **Operator categories:** `custody_unreachable`, `custody_unauthorized`, `custody_invalid`, `custody_regressed`, `custody_inconsistent`.

### 5.5 Disposable cache

`custody-cache.json` holds `{ provider, tenant, revision, active, state_digest, confirmed_at }` and nothing else. Retirement statuses are never persisted; they are used only during the response that delivers them. The cache is written atomically and may be deleted at any time; Kwiry then behaves as in §5.4 until the next successful query. The cache is derived authorization state, which A6 permits.

### 5.6 Retirement: quarantine, purge and mandatory reporting (P-b, F14)

**Trigger.** A resource Kwiry holds that is not in the accepted active set is **excluded**:

- if its status is `retired`, it is purged and reported;
- if its status is `unknown`, it is excluded and garbage-collected the same way, and reported as unattributed.

**Protocol** (each step idempotent):

0. **Disable fast rollback** durably (§4.10). This is done at the moment the retirement is *accepted*.
1. **Close admission.** Set the gate to `closing`, cancel queued units, and stop a running unit at its next yield point. Wait, holding no lock, until the admitted count is zero.
2. **Revoke retrieval.** Submit a publisher transaction whose layout omits the resource. The resource is now **retrieval-revoked**: new pins cannot lease its objects, and in-flight requests refuse at their end-of-request check (§8.3). Set the gate to `closed`.
3. **Claim.** Under L3, claim `live → doomed` for each of the resource's objects as it becomes unleased, unhandled and unreferenced (§4.3), and evict its reader cells.
4. **Delete.** Delete the claimed objects, the resource's staging, garbage generation records that name it (through the publisher), and v2 trees that contain it. Fsync every parent directory. No lock is held during this step.
5. **Final scan.** Scan every §5.7 location for artifacts of the resource, and confirm that no process file descriptor refers to them.
6. **Report** to the authority (below).

**Failure handling.** An artifact that cannot be deleted is renamed into `quarantine/<random>/` where possible; otherwise it is listed in the in-memory quarantine set, which GC rebuilds at startup. Quarantined artifacts are never reachable, leased or read. Deletion is retried with backoff. **Healthy resources and lexical search are never taken offline.**

**Restart.** Startup enforces custody through the publisher before accepting requests, resumes deletion, and quarantines anything that cannot be deleted. It never waits on deletion.

**Mandatory, state-based purge reporting (F14).** Kwiry reports a **purge state**, not events, so reporting needs no local history and survives cache loss. Each report is:

```text
{ provider, tenant, custody_revision: R,
  complete: [tuples confirmed retired at R that have no artifact left anywhere in the data root],
  incomplete: [retired tuples with attributed quarantined artifacts],
  unattributed_quarantine: <count of quarantined artifacts whose resource cannot be read>,
  report_id: SHA-256(canonical body) }
```

- **Authenticated** with the dedicated provider credential, which carries write scope for this provider's purge reports only.
- **Idempotent:** the same body produces the same `report_id`, and a repeated report is a no-op.
- **Acknowledged:** OpenClast returns a signed acknowledgement binding `report_id`. Kwiry retries with backoff until it receives one.
- **Recovery after loss of the local cache:** Kwiry recomputes the report from the data root and a fresh custody query. Because reports describe state, the recomputed report is correct without history.
- **Conservative:** while `unattributed_quarantine > 0`, OpenClast must not mark any retirement at revision ≤ R complete.

The **durable completion record lives with OpenClast** and is required (A6, §14 ask 4). Kwiry never reports `complete` for a resource after any deletion or sync failure affecting it.

**Tests (S3), crash-injected and interleaved:**

- a crash after each of steps 0–6;
- paused-builder admission (§4.3);
- an idle cached reader at doom (§4.3);
- retirement with leases held, combined with timeouts and disconnects (§4.4);
- an injected deletion failure, which quarantines, keeps healthy rooms and lexical served, reports `incomplete`, and then reports `complete` after a later successful retry;
- report retries, duplicate reports and a lost acknowledgement;
- cache loss followed by recomputed reports;
- custody unreachable, which keeps serving and freezes building;
- the rollback-combination test (§4.10);
- recreation under fresh IDs, which inherits nothing.

### 5.7 Allowed residual after purge-completed

**Inside the data root: nothing that names or contains the retired resource.** The final scan covers:

- `current.json` and every generation record (the layouts);
- every object descriptor and every lexical `manifest.json` shard;
- `staging/`, including build journals;
- `quarantine/`;
- `custody-cache.json`, whose active set no longer lists the resource;
- `upgrade.json`, `current.v2.json` and `rollback.json`;
- every layout-v2 generation tree, including its manifest and partitions;
- the legacy `semantic/` directory, which must be absent in the openclast profile;
- lock and probe files;
- the process's open file descriptors.

**Outside the data root, stated and not covered:**

- the operator-owned configuration file;
- external log sinks (resources appear there only as per-process keyed digests);
- storage-medium remanence (purge is unlinking, not erasure);
- OpenClast's authority records.

## 6. Readiness, errors and HTTP mapping

### 6.1 Effective set and gate order

The **signed set** `S` is the capability's resources. The **effective set** is `E = S ∩ {vault_id, room filters}`. `E` alone drives the gates, leases, generation digest and freshness. The readiness probe uses `S`. If `E` is empty, the response is `200` with no hits, a digest over the empty set and freshness `current`; nothing is embedded or leased.

1. **Verify the capability.** The verified context keeps `exp`, the exact action, `jti`, resources and `max_limit` (A2). The action must match the route and the mode (§7.5).
2. **Require `mode`** in the openclast profile. The D2 default is applied at the seam.
3. **Validate** the query, controls and filters. Non-room filters are refused here for semantic and hybrid, before any embedding or open.
4. **Semantic and hybrid only (F6):** the model must be loaded with the expected `embedding_identity`, otherwise `mode_unavailable`. **Lexical never consults the model.**
5. **Acquire a permit** from the mode's pool (§4.4).
6. **The worker calls `pin_and_lease(E, class)`** (§4.3). Every resource in `E` must be mapped and confirmed active, otherwise `resource_unavailable`.
7. **Semantic and hybrid only:** every resource in `E` must be `ready`, otherwise `semantic_not_ready`.
8. **Execute** with checkpoints. Any failure refuses the whole call.

**Tests (S7 and S8, F6).** Lexical requests succeed with byte-equal IG-1 results in each of these cases:

- a build without `semantic-onnx`;
- semantic disabled in configuration;
- model initialization failed.

In the same three cases, semantic and hybrid requests return `mode_unavailable`.

### 6.2 Taxonomy

| Condition | Kwiry `code` | HTTP | OpenClast seam (peer-reported) |
|---|---|---|---|
| Semantic or hybrid with no model, wrong identity, no `semantic-onnx`, or semantic disabled | `mode_unavailable` | 501 | passed through |
| `mode` omitted (openclast profile) | `invalid_request` | 400 | seam applies D2 first |
| Route or action mismatch; subject not on the allowlist; legacy shape after cutover | `forbidden` / `unauthorized` | 403 / 401 | `search_provider_violation` |
| Non-room filter with semantic or hybrid | `filter_unavailable` (new) | 501 | explicit refusal |
| Lexical control not allowed for the mode | existing `invalid_query` family | 400 | explicit refusal |
| Cursor | `cursor_unavailable` | 501 | `ranked_unpageable` |
| Resource unmapped, not confirmed active, or retired mid-request | `resource_unavailable` | 503 | `search_provider_failed`, no partial re-query |
| Resource `pending` or `unavailable` | `semantic_not_ready` (new) | 503 | per-room disclosure before dispatch |
| Embedding or scan failure in either leg | `semantic_failed` (new) | 503 | one leg down → refuse |
| Capability `exp` reached; takes precedence over the budget (§4.4) | `capability_expired` (new) | 401 | re-mint and retry |
| Execution budget exhausted while the capability is valid | `deadline_exceeded` (new) | 503 | retry or refuse |
| Pool queue full (A7 residual) | `busy` (new) | 503 | retry |
| Linearized pin retries exhausted | `index_building` | 503 | retry |
| Hydration integrity failure | `internal_error` (fixed message) | 500 | refuse |

Messages are fixed strings and contain no paths, object IDs, counts, query text or engine text. The envelope `{ "error": { "code", "message" } }` is unchanged.

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

Before responding, Kwiry runs these checks in this order of precedence:

1. If `exp` has passed: `401 capability_expired`.
2. If any resource in `E` is no longer mapped in the then-active layout, or is no longer confirmed active: `503 resource_unavailable`.
3. If the execution budget is exhausted: `503 deadline_exceeded`.

OpenClast's grant-level post-check returns `search_authorization_changed` on any narrowing. It must fail closed when continuing authorization cannot be established (§14). Kwiry never emits that code.

### 8.4 Freshness of superseded leases (finding 10)

For each resource `r` in `E`, the response computes a per-resource state: the A4 transition state of `r`, raised to at least `stale` if the objects leased for this request's **mode class** have been superseded in the then-active layout.

- Class L (lexical) compares only the lexical object, so an unrelated vector publication never downgrades a lexical response.
- Class S (semantic and hybrid) compares both objects.

The header reports the worst state over `E` (`stale` > `reconciling` > `current`) together with the configured evidence basis: `X-Kwiry-Index-Freshness: <state>; basis=<basis>`. The contract's freshness evidence basis is preserved. The generation digest describes the objects actually served, which are the leased ones.

## 9. Cross-file content check

Unchanged and confirmed by both reviews: no cross-file content reaches chunk text, embedding input or excerpts. The S4 regression test (embed and link fixture, vectors bitwise identical across target presence) stays.

## 10. Threat model and side channels

**Guarantee.** For a fixed binary, `embedding_identity` and CPU feature set, a request's hits, order, truncation, coverage and scores equal those of a physical instance holding only `E`. They do not change when forbidden resources are added, removed or mutated.

**Stated limit (A5).** When a resource first answers semantic and hybrid requests depends on shared indexing load, including load from resources the caller cannot see. Fair scheduling and bounded work units (§4.9) bound this dependence but do not eliminate it.

**Separate disclosed residual (A7).** Queueing delay, `busy` and `deadline_exceeded` depend on aggregate request load per pool, including other users and other rooms. They reveal request volume and timing, not corpus content and not which rooms are busy. Lexical capacity is reserved apart from semantic capacity, so stalled semantic work cannot starve lexical.

**Bounded stale-authority window (§5.4).** An already-issued capability for a just-retired room can be served until it expires: at most the 60 s TTL, during a custody outage or before the next poll. The OpenClast post-check is the fail-closed control for users. The publish freeze is not a revocation bound.

| Channel | Response |
|---|---|
| Readiness timing | Accepted under A5. Fair rotation; the probe returns enums only; advisory witness. |
| Service load | Disclosed under A7. Separate lexical and semantic pools with FIFO admission. |
| Query embedder | A dedicated instance is required before HTTP. It is necessary but not sufficient. Still shared: CPU and SMT siblings, memory bandwidth and last-level cache, page cache and disk I/O, the blocking pool, the allocator, and the query-model queue. These are documented residuals. Builders run at lower OS priority. |
| Lifetime and retirement | Linearized pin-and-lease, an atomic doom claim, a single publisher, admission gates and handle drain. Retiring B never breaks A. |
| Custody query | Server-to-server, triggered by startup, timers and capability arrival, coalesced. Users never see a response. |
| Caches | None: no query-embedding, result or fusion cache. |
| Counts and errors | Byte-equal to the baseline. Fixed messages over `E` or `S` only. |
| Headers | A4 authorized-view digest and per-resource freshness. Class L ignores vectors. Desktop is unchanged. |
| Logs | No subject, actor, query text or fingerprint, query vectors, distances or chunk IDs. Resources appear only as per-process keyed digests. |
| Purge residue | §5.6 and §5.7, with an exact allowed residual and mandatory authority-side reporting. |

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

### Intentional changes to governed public behaviour in shipped releases

- **S1a:** the meaning of `X-Kwiry-Generation` and `X-Kwiry-Index-Freshness` changes.
- **S1b:** subject and actor stop being logged or retained. Capabilities are otherwise accepted as today.
- **Release unit L (S2 + S3, shipped together):**
  - a one-time rebuild into the object layout;
  - the header value changes once;
  - resources not confirmed active by custody are withheld;
  - retirement purges;
  - fast rollback is disabled at the first accepted retirement.
- **A2 cutover**, a configuration step after OpenClast's transition (A2): identity-bearing capabilities become invalid.

### Build gates

- **`governed-lifecycle-v3`** (non-default) holds S2 and S3 code on `main` until both have merged. It is then removed in a single release, so lifecycle rules never ship without custody (F12).
- **`governed-semantic-internal`** (non-default) holds every vector-building, vector-publishing and vector-serving path until S10. No shipped release before S10 publishes a vector object, and class L headers ignore vectors by construction.

Desktop keeps its current headers, freshness and behaviour in every slice. Desktop regression tests run on every slice, including for the empty-resource-set desktop principal.

| Slice | Content | Build | Tests |
|---|---|---|---|
| **S0** | Amendment text A1–A7 | — | — |
| **S1a — scoped lexical headers (A4), transitional identity (F15)** | `X-Kwiry-Generation: v1-<hex>` over a **transitional per-resource revision**: SHA-256 over the resource's complete manifest entries, every persisted field of each `ManifestFile` (including `mtime_nanos`, `byte_length`, `content_hash`, `format_identity`, `extraction_profile`, `registration_fingerprint`, `chunk_count`, `coverage`, `outcome` and `warning`), canonically serialized in `source_key` order, plus the manifest-level versions (manifest, index format, chunking, preparation, format-identity schema). All length prefixes are u64-LE. Indexed and queryable `mtime` therefore changes the identity. Adds per-resource freshness and an explicit profile branch. | shipped | invariance under forbidden publish, reconcile and watcher events; **an mtime-range query (`mtime:[a TO b]`) across an mtime-only rebuild changes both results and identity consistently**; desktop header regression including `resources: []`; IG-1 witness re-run |
| **S1b — verified context; A2 phase 1 (F8)** | The context keeps `exp` and the action; a repeated `vault_id` is refused; subject and actor are dropped from `Principal` and logs. **Two bounded verification branches:** (1) **workload shape** — `sub` on the workload allowlist, no `actor` or `subject_revision`; (2) **legacy shape**, accepted only while `legacy_identity_accept_until` (configuration) is in the future — `sub` and `actor` must be present, distinct and non-empty as today, are validated, then discarded and never stored or logged. User subjects are **never** added to the allowlist. All existing issuer, audience, TTL, `kid` and algorithm checks apply to both branches. | shipped | both shapes accepted before the cutoff; legacy shape refused after it; user subject not on the allowlist refused in the workload shape; no subject or actor in logs or context |
| **S2 — objects, publisher, leases, migration (lexical)** | §4.1–§4.5, §4.10; worker-owned leases; deadlines; separate capacity pools; reader-cell eviction; header switches to `v2-<hex>`. | `governed-lifecycle-v3` | F1 interleaving; publisher serialization; recursive-sync crash injection with nested index files; migration matrix with the post-switch barrier; idle-reader drain; capacity isolation; desktop regression; IG-1 W1–W13f |
| **S3 — custody seam, purge, reporting (lexical)** | §5 in full | `governed-lifecycle-v3`; needs the OpenClast custody and report endpoints | every §5.3 and §5.6 test; rollback combination; purge witness over §5.7 |
| **Release unit L** | Removes `governed-lifecycle-v3`. **This release is the MRCB.** | owner checkpoint | full IG-1 witness on the release build; custody, retirement and purge witness rows |
| **S4 — vector object store** | Plain table, scan, descriptor; cross-file test | internal | rowid independence, boundary ties, ±0.0, non-finite values, no sidecar files |
| **S5 — builder and scheduler** | §4.7–§4.9, §11.2 | internal | inventory failures; reuse identity; paused-builder admission; reused-room fairness; resume; embed barrier; measurements |
| **S6 — real-ONNX gate** | §4.7 gate, `B = 1` fallback | internal, mandatory before S8 | fresh process, fresh build, incremental, reuse, permuted order |
| **S7 — governed query path (core)** | §6–§8 | internal | physical baseline A (rooms 1–4) versus B (authorized rooms 1–2), semantic and hybrid; forbidden add, remove and mutate; two-user; fan-in; filter refused before embed or open; lexical-without-model tests; timeout, expiry and disconnect at embed and scan; superseded-lease freshness by class |
| **S8 — query embedder, HTTP, probe** | Dedicated embedder; semantic pool; `/search` semantic and hybrid; `/readiness` | internal | one server test per §6.2 row; probe returns signed tuples only; stalled semantic workers do not starve lexical; latency under load |
| **S9 — observer and witness** | §12.1–§12.2 | internal + observer | shipped versus observer functional digest |
| **S10 — enablement** | Removes `governed-semantic-internal` | owner checkpoint | full witness on the release build |
| **S11 — later** | Pre-scan filter rowsets | — | baseline with filters |

### A2 cutover sequence (F8)

1. A release that contains S1b is deployed everywhere.
2. OpenClast switches its issuer to the workload shape at time `T_switch` and confirms it.
3. The operator sets `legacy_identity_accept_until = T_switch + max_token_ttl_seconds + clock_skew_allowance`. The defaults are 60 s TTL and 30 s allowance. The legacy branch is enforced off at that time, which outlasts every capability minted before the switch.
4. Kwiry never enforces the workload-only rule before OpenClast confirms step 2.

Dependencies:

- S1a and S1b need A4 and A2.
- Release unit L needs A6, A7, and the OpenClast custody and report endpoints, deployed **before** that release (§14 ask 9).
- S8 needs A1, A3 and S6.
- S9 needs the internal observer, which today exists only on the unmerged `feat/ig1-witness-observer` branch.
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
| Custody seam | Regressed revision refused; equal-revision, different-state response refused; equal-state response refreshes confirmation; unreachable endpoint → serving continues and building freezes; no cache → fail closed; a capability for a not-yet-cached resource → coalesced query; stale-authority window bounded by capability TTL; purge reports acknowledged, idempotent and recomputed after cache loss | new |
| Capacity isolation (A7) | Semantic workers stalled at the embedding barrier; lexical requests on another room still succeed; semantic requests return `busy` or `deadline_exceeded` | new |
| Rollback after retirement | Retirement accepted → deletion failure → crash → attempted fast rollback: refused; the retired room is never rebuilt | W13f |
| Cross-file embeds | Literal text only; vectors identical across target presence | new |
| Audit and storage privacy | Logs contain no subject, actor, query text or fingerprint. No query embedding is ever persisted, and no diagnostic vector dump exists. Passage vectors exist only in live vector objects and live build staging. | W11 |
| Named errors, no fallback | `mode_unavailable` (semantic and hybrid only; lexical unaffected), `semantic_not_ready`, `semantic_failed`, `capability_expired`, `deadline_exceeded`, `busy`, `filter_unavailable`, `ranked_unpageable` | W10 |
| Shipped = observer | Identical functional digest | W12a, W12b |

W1, W3, W4, W6, W7 and W9 are re-run as regression.

## 13. Contract amendment package A1–A7

These amendments are flagged here only. `CONTRACT.md` is not edited.

### A1 — governed modes and default (§4, §11)

Admit `semantic` and `hybrid` in the OpenClast profile as designed here. In that profile `mode` is required at Kwiry, and the D2 default is applied at the seam. Desktop keeps its default unless the owner extends D2 (Q1).

### A2 — capability identity and verified context (§5)

- **Unchanged:** the existing issuer, audience, TTL, `kid` and algorithm restrictions, for every shape.
- **Target claims:**
  - no user identity of any kind;
  - `sub` exactly matches a workload-subject allowlist entry, compared bytewise;
  - `jti` is a per-request nonce;
  - the verified context preserves `exp`, the exact action, `jti`, resources and `max_limit`;
  - each `vault_id` appears at most once.
- **User subjects are never allowlisted.**
- **Transition:**
  - Until the coordinated cutover, a bounded legacy branch accepts today's shape. In that shape, `sub` and `actor` are validated, discarded, and never retained or logged.
  - The cutover time is `T_switch + max_token_ttl_seconds + clock_skew_allowance`. After it, identity-bearing or unknown claims are refused with `401`.
  - Kwiry enforces the cutover only after OpenClast confirms its issuer transition (§12).

### A3 — scoped readiness probe (§4)

`POST /v0/readiness`, action `readiness`, returns closed per-resource enums for the exact signed tuples (§6.3). Search actions are valid only on `/search`.

### A4 — scoped generation and freshness headers (§4; IG-1 doc)

- **Profile branch:** desktop is unchanged. The rest of this amendment applies to the OpenClast profile only.
- **Encoding:**
  - every length prefix and count is a u64, little-endian;
  - the mode class is one byte, `L` or `S`;
  - the digest is computed over `E` sorted bytewise by `(tenant_id, vault_id, room_id)`, covering each resource's tuple fields and its lexical object ID, plus its vector object ID in class `S`;
  - the header value is `X-Kwiry-Generation: v2-<hex>`;
  - the transitional `v1-<hex>` uses the S1a per-resource revision (§12).
- **Per-resource freshness transitions:**
  - `current → reconciling` when a covering pass starts. Path-scoped passes cover only the vaults whose paths changed; full passes start at boot or on schedule.
  - `reconciling → current` when the pass publishes with no remaining dirty state.
  - `reconciling → stale` when the pass ends incomplete, including when publishing is frozen by custody.
  - `current → stale` on a watcher event.
  - `stale → reconciling` when the next pass starts.
  - Any state becomes `unavailable` when the resource is unmapped or not confirmed active, and such a resource is never served.
  - A superseded leased object raises the resource to at least `stale` for its mode class.
- **Reported value:** the worst state over `E`, together with the configured evidence basis.

### A5 — semantic readiness-timing limit (owner decision 2026-11-17)

Semantic and hybrid results are exactly invariant to forbidden resources. Readiness *timing* is not. A fair shared scheduler builds vectors, and when a resource first answers rather than returning `semantic_not_ready` may depend on server load, including indexing load from resources the caller cannot see. Kwiry bounds this with fair scheduling and bounded work units, exposes readiness only as closed enums, and makes no claim of timing invariance. Any scheduling change that adds a channel requires amending A5.

### A6 — retirement, durable purge, custody cache, rollback (§1.1, §5)

- **Authority.** OpenClast is the authority on enrolled, active and retired resources. Kwiry builds and serves only resources confirmed active at the latest accepted custody revision, and never accepts a lower revision or a conflicting equal one.
- **Two reported states:**
  - **retrieval revoked:** no new pin can use the resource, and in-flight requests are refused at their end-of-request check;
  - **purge completed:** every data-root artifact of the resource is unlinked, every parent directory is durably synced, no lease or reader handle remains, and a final scan is clean.
- **Mandatory authority-side record.** Kwiry sends authenticated, idempotent, state-based purge reports and retries until it receives a signed acknowledgement. OpenClast durably records completion and keeps "retrieval revoked" distinct from "purge completed". Kwiry never reports completion after any deletion or sync failure. Failed artifacts are quarantined, only the retired resource is affected, and the report says incomplete.
- **Clarification of §1.1.** Derived state may include a disposable cache of authority-issued lifecycle state: the revision, the active set, an authority-issued opaque state commitment, and the confirmation time. Deleting the cache never widens access; Kwiry fails closed until a live query succeeds. Rebuild converges given the same files and the same custody state.
- **Rollback.** Fast rollback to a pre-custody binary is durably disabled the moment a retirement is accepted. Rebuilds run only through a binary at or above the MRCB, with a fresh authority-confirmed active set. The purge guarantee holds only while such binaries operate the data root.
- **Stale-authority window.** An already-issued capability for a just-retired room can be served until it expires, at most the capability TTL.
- **Scope.** Purge is file unlinking, not cryptographic erasure.

### A7 — service-load residual (new)

In the OpenClast profile, request admission uses separate, fair lexical and semantic capacity pools. Queueing delay, `busy` and `deadline_exceeded` may depend on aggregate request load, including requests by other users on other rooms. They disclose request volume and timing, never corpus content or the identity of busy rooms. Semantic load can never exhaust lexical capacity. This residual is separate from A5.

**Not amendments:**

- refusing non-room filters;
- exact baseline equality;
- `ranked_unpageable`.

## 14. OpenClast seam asks

This is the final list for OpenClast's endpoint design and independent security review.

1. **Custody query** for provider `kwiry`, server to server.
   - **Request:** a fresh random nonce, plus the list of **held tuples** that are not in Kwiry's cached active set.
   - **Signed response:** provider, tenant, a strictly monotone u64 `revision` (it never decreases across restarts or failover, and it advances on every enrollment or retirement), `issued_at`, `expires_at`, the nonce echo, the **active set** of exact tuples, a `retired | unknown` **status for each held tuple**, and `state_digest`.
   - The status list replaces any need for unbounded retired history. A full retired set is acceptable but not required.
2. **Canonical state and unchanged-snapshot refresh.**
   - `state_digest` is an opaque commitment to the provider's complete lifecycle state at `revision`, computed with a provider-scoped secret so it cannot confirm guesses.
   - Equal revision implies an equal active set and an equal `state_digest`.
   - An unchanged snapshot is re-served with a fresh envelope (new nonce echo, signature and timestamps) and the same canonical state, which Kwiry treats as a confirmation refresh.
3. **Credentials and keys.**
   - A dedicated provider credential: read scope for this provider's custody, write scope for this provider's purge reports.
   - A response and acknowledgement signing key in a JWKS separate from search capabilities, or a distinct `typ` and audience, so no custody document or acknowledgement can ever verify as a capability, and the reverse.
   - Rotation for all of these.
   - Typed failures: unauthorized, rate-limited, unavailable.
4. **Mandatory durable purge reporting.**
   - An endpoint that accepts Kwiry's state-based report (§5.6), deduplicated by `report_id`.
   - It returns a signed acknowledgement binding `report_id` and records completion durably.
   - It keeps "retrieval revoked" distinct from "purge completed".
   - It never marks a retirement at revision ≤ R complete while that report shows unattributed quarantine.
5. **Issuer transition with token-drain sequencing.**
   - Switch capabilities to workload `sub`, with no `actor` or `subject_revision`, at `T_switch`, and confirm it to Kwiry.
   - Kwiry rejects legacy shapes only after `T_switch + max_token_ttl_seconds + clock_skew_allowance`.
   - Never issue a user subject in the workload shape.
6. **Outstanding-capability behaviour and a fail-closed post-check.**
   - Acknowledge the stale-authority window: an already-issued capability for a just-retired room may be served for at most its TTL.
   - The gateway's post-check must **fail closed**, refusing to release results, whenever continuing authorization cannot be established, including during OpenClast-side lookup failures.
7. **Retrieval seam behaviour.**
   - A `retrieval` field, with the D2 default applied at the seam.
   - The mode bound to the capability action, with actions scoped per route.
   - The `X-Kwiry-Effective-Mode` echo, with a mismatch raising `search_provider_violation`.
   - `score` and `chunk_id` stripped.
   - The §6.2 mapping, including `capability_expired` (re-mint), `deadline_exceeded` and `busy`.
   - The readiness probe consumed before dispatch.
   - `search_authorization_changed` returned on narrowing.
8. **Headers and threat model.**
   - Forward only the A4 scoped headers.
   - Record the A5 and A7 residuals and the stale-authority window in the seam threat model.
   - Confirm that the custody audit records only provider identity, revision and outcome.
9. **Release order.**
   - The custody query and purge-report endpoints, and the issuer's ability to emit both shapes, are deployed **before** Kwiry release unit L (the MRCB).
   - The issuer transition (ask 5) happens after release L.
   - Kwiry never ships lifecycle rules that depend on an undeployed endpoint (§12).

## 15. Risks and open questions

**Risks**

- **R1 — ONNX determinism.** The highest technical risk. S6 is a hard gate, with `B = 1` as the fallback; otherwise re-scope with the owner.
- **R2 — Storage transition.** Roughly double the disk until confirmation; an IG-1 re-witness is required; fast rollback ends at the first accepted retirement; pre-custody binaries become unsupported after the MRCB.
- **R3 — Memory.** Two embedders plus the sidecar, measured in S8.
- **R4 — Readiness timing under load (A5)** and the **service-load residual (A7)**.
- **R5 — Purge latency.** It waits for workers to acknowledge cancellation and is not time-bounded. Refusal is bounded.
- **R6 — Custody dependency.** An outage freezes building after `custody_max_age`. A fresh data root with no reachable endpoint serves nothing. Release unit L needs OpenClast endpoints first.
- **R7 — Observer not on `main`.** This blocks S9.
- **R8 — Existing IG-1 leaks.** S1a and S1b fix them regardless.
- **R9 — Cross-repository sequencing.** Release unit L and the A2 cutover depend on OpenClast delivery and its security review.

**Open questions**

- **Q1** — Should D2 also change the desktop default?
- **Q2** — Should Kwiry itself omit `score` and `chunk_id` in the governed profile? That would need a hit-schema amendment.
- **Q3** — Do we want per-resource encryption keys with crypto-shredding later?
- **Q4** — Desktop labels its L2 scores as "cosine". Fix that separately?
- **Q5** — Rename the profile to a generic governed name. New identifiers should use the generic name.
- **Q6** — Should the advisory time-to-ready evidence become a published operator expectation?
- **Q7** — Should search workers run in terminable processes, so purge completion can also be time-bounded?

## 16. Implementation-time verification

The design contract for these properties is closed above. Whether an implementation meets it can only be shown by the named tests, so reviews of this design should judge the contract and leave the proof to the tests.

| Property | Design contract | Proven only by |
|---|---|---|
| ONNX determinism | §4.7 artifact identity, canonical batching, whole-source reuse | S6 real-ONNX gate: fresh process, fresh build, incremental, reuse versus full, permuted order; fallback `B = 1` |
| Crash safety of durability order | §4.2 recursive sync, parent fsyncs, publisher-only pointer writes | S2 crash injection after each step, including nested Tantivy files |
| Migration recovery | §4.10 marker protocol and recovery matrix | S2/S3 crash barrier after the pointer switch; every matrix row; the rollback-combination test |
| Lifetime linearization | §4.3 pin-and-lease under L3; atomic doom claim; handle drain | S2 F1 interleaving and idle-reader tests; lock tracker |
| Publisher serialization | §4.3 single actor; no waits inside transactions | S2 concurrent-publisher test |
| Retirement and purge | §5.6 protocol and §5.7 residual | S3 crash injection at steps 0–6; deletion failure; report retries and lost acknowledgements; cache loss |
| Custody comparison | §5.3 canonical state | S3 reordering, nonce, equal-state refresh, changed-state, regression and restart tests |
| Scheduler fairness | §4.9 bounded accounting turns and stated assumptions | S5 reused-room interleaving test, plus measurements (§11.3) |
| Capacity isolation | §4.4 separate FIFO pools; permits held until worker exit | S8 stalled-semantic test with lexical succeeding |
| Physical-baseline equality | §7 merge and RRF rules over `E` | S7 two-instance tests and the S9 witness rows, including readiness under forbidden load |
| Lexical without the model | §6.1 step 4 applies to semantic and hybrid only | S7/S8 tests with no `semantic-onnx`, semantic disabled, and model init failure |
