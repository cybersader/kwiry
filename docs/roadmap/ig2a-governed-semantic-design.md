# IG-2A design — governed semantic and hybrid retrieval

Status: **design, revision 5, independent review pending.** Nothing here is implemented or approved. This page expands [IG-2](governed-semantic-hybrid.md) and is bound by [`CONTRACT.md`](../../CONTRACT.md), which this design does not edit. The amendments it needs are in [§13](#13-contract-amendment-package-a1a7). Room lifecycle follows the frozen OpenClast Holdings wire contract (§5). What remains to ask of OpenClast is in [§14](#14-openclast-seam-asks).

## Revision history

| Revision | Head | Independent review result |
|---|---|---|
| 1 | `1442e668` | NO-SHIP. Core approach judged sound. |
| 2 | `b70c902d` | NO-SHIP, converging. |
| 3 | `82f53cb4` | NO-SHIP. Closed: timeout vs leases, retirement vs admitted builder, manifests, reuse identity, permanent tombstones, failed-purge startup, effective vs signed set, superseded freshness, A2 issuer/TTL retention, A3, and A4 encoding with desktop branching. |
| 4 | `59280048` | NO-SHIP. Architecture sound; most F1–F16 closed. Open: R4-1 (cached reader blocks doom), R4-2 (held-tuple queries inconsistent), R4-3 (unsent completion unrecoverable), R4-4 (no revoked/purge-pending state), R4-5 (no executor reservation), R4-6 (A7 overclaims), R4-7 (rollback rule contradicts itself). |
| 5 | this page | Replaces the bespoke custody seam (§5, §14) with Kwiry as a consumer of the owner-approved, frozen Holdings contract (`Holdings.List`, `Obligation.Report`). Closes R4-1 to R4-7. Storage, scheduler and query path are unchanged except for R4-1 (§4.3) and R4-5 (§4.4). P-a and P-b are unchanged. |

§16 lists the properties that are closed in the design but can only be proven by tests.

| Fourth-review item | Resolution | Where |
|---|---|---|
| R4-1 Cached readers block the doom that would evict them | "Retrieval revoked" (no count precondition; detaches the cached reader under L3 and drops it outside) is split from "deletable" (all lease, handle and init counts zero, no references). Nothing overrides the drain. | §4.3, §5.6 |
| R4-2 Held-tuple status queries cannot describe changes consistently | Removed. Each list is the holder's complete projection at one `rev`, so every held resource is classified from the list alone; unlisted means unknown, which is withheld. | §5.3 |
| R4-3 Purge completion cannot be reconstructed after the last local reference disappears | The obligation lives at the authority and stays listed until `complete`. Kwiry re-learns it from the next list and re-checks absence, even after losing the data root. Tombstones survive in durable holder state outside the data root. | §5.1, §5.2, §5.6 |
| R4-4 The report cannot express "revoked, purge pending" | A real state machine, `revoked → incomplete → complete`. `revoked` is sent before any drain. Sync failure, deletion failure and unknown attribution each map to exactly one state. A fresh barrier and final scan precede `complete`. | §5.6 |
| R4-5 Semaphores do not reserve execution capacity | Each pool owns a dedicated executor of exactly `W_L` or `W_S` threads; search workers leave Tokio's shared blocking pool. The claim is narrowed to admission and thread availability. | §4.4 |
| R4-6 A7 claims timing cannot reveal corpus information | A7 restated as a shared-service timing and availability residual that may depend on other workloads' data-dependent cost. Load-dependent outcomes are excluded from error equality. | §4.4, §10, A7 |
| R4-7 Rollback support boundary contradicts itself | One rule: pre-custody rollback is supported only inside a window that closes at upgrade confirmation or the first durable tombstone. Deployment pinning is mandatory after that, and the MRCB detects violations. | §2.2, §4.10, A6, R2 |

The third-review table below is kept for provenance. Rows F9, F10, F11 and F14 are superseded by revision 5's §4.3 and §5.

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
| F9 Canonical custody state equality | superseded: no state equality; high-water rule, §5.3 |
| F10 Stale-authority window during custody outage | §5.4 (now bounded by the capability TTL plus the 15-minute lease) |
| F11 Reader cells on doomed objects | §4.3 (closed with R4-1) |
| F12 Lifecycle rules before custody | §12 (S2 and S3 ship as one release unit) |
| F13 Fixed-cost finalization | §4.9 |
| F14 Mandatory durable purge reporting | §5.6, A6 (Holdings `Obligation.Report`) |
| F15 Transitional identity must cover mtime | §12 (S1a) |
| F16 Capability expiry vs execution deadline | §4.4, §6.2, §8.3 |

## 1. The idea in one paragraph

Governed semantic search follows the IG-1 lexical model. **Every room has its own immutable stores, one lexical and one vector.** A request leases and opens only the stores of the rooms it is signed for. A vector store is derived from exactly one lexical store and records that store's identity, so the two can never disagree.

A query embeds the question once, then scans each authorized room exactly for its nearest neighbours. It merges the per-room lists by distance, then chunk ID; rowids play no part. Hybrid fuses that list with the IG-1 authorized lexical list using RRF (`k = 60`).

Three things hold for forbidden rooms. They are never opened. They cannot invalidate an authorized request's stores. And they cannot change what it returns: results are byte-equal to those of a separate instance holding only the authorized rooms.

**OpenClast is the authority on which rooms exist.** Kwiry is one holder in OpenClast's engine-neutral Holdings lifecycle (§5). It serves and builds only rooms its latest signed list shows `active` within a 15-minute lease. It purges only under an authority-owned obligation, and it reports progress with signed receipts. Its whole data root stays disposable. A small holder-state file kept next to its configuration remembers what must never be served again.

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
- **Room lifecycle (owner, 2026-11-17, Q1–Q6), and the joint Holdings wire contract frozen the same day** with Kwiry's amendments A–C. Kwiry consumes it as written (§5). The questions in §14.2 cover the gaps that remain.
  - Q1: pull a signed list, with nudges later.
  - Q2: one generic Holding record.
  - Q3: OpenClast's existing governance store.
  - Q4: an engine serve lease of 15 minutes, then quarantine, never deletion.
  - Q5: delete-only credentials, for devices only.
  - Q6: crypto-erase later; until then receipts say "confirmed".

### 2.2 Owner decisions

Each comes with a recommendation, or with a default the owner can override.

1. **Approve amendment package A1–A7 (§13).**
   - A1: semantic and hybrid in the governed profile, with an explicit mode.
   - A2: capabilities name a workload, never a person, after a bounded transition.
   - A3: a scoped readiness probe.
   - A4: room-scoped generation and freshness headers.
   - A5: the readiness-timing limit.
   - A6: the holder lifecycle, durable purge, holder state and the rollback rule.
   - A7: disclosure of the service-load residual.

   *A2 and A4 also change the existing lexical endpoint (§12).*
2. **Move to per-resource objects, as a recoverable storage transition (§4.10).**
   - **Disk headroom:** roughly twice the governed lexical index while old and new layouts coexist.
   - **Rollback among new releases:** always allowed, to any release at or above the minimum rollback-compatible binary.
   - **Fast rollback to the old layout and a pre-custody binary:** supported **only inside the fast-rollback window**. The window closes at upgrade confirmation or the first durable tombstone, whichever comes first. While the old binary runs, lifecycle guarantees are suspended. After the window closes, deployments must pin the minimum (§4.10).
   - **Rebuild fallback:** always goes through a release at or above the minimum, using a fresh verified list.
   - The IG-1 witness rows must be re-run.

   *Recommendation: yes.*
3. **Refuse rather than serve stale vectors** when a room's vectors lag its text. *Recommendation: refuse for IG-2A.*
4. **Pay the RAM for a dedicated query embedder.** It is a prerequisite for HTTP. *Recommendation: yes; measure in S8.*
5. **Reject non-room filters** for semantic and hybrid in IG-2A, before embedding and before any store opens. *Recommendation: yes.*
6. **(P-a, default) Lifecycle authority stays with OpenClast.** Obligations, retirement and completion records live at the authority (§5.1). As the frozen contract requires, Kwiry keeps only refusal state outside the data root: a high-water mark and grow-only tombstones. That state never authorizes serving, building or deleting (§5.2). *Override only if the owner wants Kwiry to keep its own record.*
7. **(P-b, default) A failed purge quarantines only the obligated resource.** Healthy rooms and lexical search stay online (§5.6). *Override only if the owner prefers a fail-stop.*

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
  quarantine/<random>/         # moved, never-served artifacts (§5.5)
  quarantine/ledger.json       # quarantine ledger (§5.5)
  upgrade.json, current.v2.json, rollback.json   # migration and rollback markers; §4.10
  models/                      # model artifacts only

<config-dir>/holder/           # OUTSIDE the data root: holder key and durable holder state (§5.2)
```

- **A generation record** is small and immutable. It holds:
  - the format versions, `state_revision` and `last_sync`;
  - the layout: resource tuple → `{ lexical_object, vector_object? }`;
  - the **withheld** map (§5.3): resources that are kept but never servable.
  
  It contains **no** paths, hashes, lengths or timestamps of sources. Those live in the lexical object's own `manifest.json` shard, so they are deleted together with the object.
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
- **Reader handles**, which are open readers of an object. They come in three kinds:
  - **cache handles**, held by the object's reader init cell;
  - **external handles**, borrowed by in-flight workers;
  - **init-in-progress markers**.

**Object registry.** An in-process registry holds, for each object:
- its state: `live`, `revoked`, `doomed` or `deleted`;
- its lease count;
- its cache-handle and external-handle counts;
- its init-in-progress count;
- its reader init cell;
- the generations (layout or withheld) that reference it. It is guarded by **L3**, the innermost lock. L3 is held only for in-memory bookkeeping and is **never held across I/O, fsync, deletion or any wait.**

**Single publisher (F2).** The existing `DataRootLock` is a file lock that excludes other processes. It does not serialize callers inside one process. IG-2A therefore adds one **publisher actor**: a single task that holds the `DataRootLock` for the life of the process and is the **only** writer of `generations/` and `current.json`.

Reconcile, vector attach, list apply (revocation), the migration switch and generation deletion by GC are all **transactions submitted to the actor**. The actor runs them one at a time. Each transaction is one serialized unit:

1. revalidate the layout against the current active generation;
2. revalidate holdings (§5.3): every resource the new layout maps must be listed `active` in the last applied list, configured and **not tombstoned**. A lapsed resource keeps its existing mapping, but no transaction may publish a new object for it. A transaction that would map a tombstoned resource is refused;
3. build the generation record;
4. publish it (§4.2, steps 4–5);
5. under L3, swap the in-memory active pointer and update object reference sets.

The actor reads admission gates only as a non-blocking state check. **The actor never waits on admitted operations, leases, handles or other transactions inside a transaction.** Anything that must wait does so outside the actor, holding no lock.

**Admission gates (L2).** Each resource has a small mutex and condition variable, with a state (`open`, `closing` or `closed`) and a count of admitted operations. A builder or attach step takes an **admission token** first, and the token is refused unless the gate is `open`. Holding the token, the builder:
- re-checks that the resource is servable-eligible (listed active, leased, not tombstoned);
- performs its filesystem work;
- may submit an attach transaction and wait for the actor's answer.

The actor never needs that token, so there is no cycle. The builder releases the token afterwards.

An obligation revokes retrieval first, without waiting (§5.6). It then closes the gate and waits for the admitted count to drain while holding nothing. An admitted builder's attach is refused by publisher revalidation, because the resource is now tombstoned.

**Lock order:** actor serialization → L2 → L3. No lock is acquired while holding a later one, and no wait happens while holding L2 or L3.

**Linearized pin-and-lease (F1).** `pin_and_lease(E, class)` is one critical section under L3:

1. read the in-memory active pointer and the lease table **inside** L3. The actor swaps both only under L3, so pointer, leases and object states form one consistent snapshot;
2. verify that every resource in `E` is servable (§5.4); otherwise refuse with `resource_unavailable`;
3. resolve the objects of `E` for the mode class;
4. verify that every one of them is `live`;
5. increment their lease counts.

If any object is `revoked` or `doomed`, nothing is leased. The call releases L3 and retries against the then-current pointer, up to 3 times. After that it refuses with `503 index_building`, or with `resource_unavailable` if a resource has since become unservable. A worker can never load a generation outside L3 and register a lease against it later: the load and the registration are the same critical section.

**Two predicates, never one (R4-1).** Revision 4 had a deadlock. Doom required a zero handle count, but an idle cached reader released its handle only on doom, so neither could happen first. Revision 5 splits *stop using it* from *may delete it*:

- **Retrieval revoked** (`live → revoked`). This step has **no count precondition**. Under L3 it:
  - marks the object `revoked`;
  - **detaches** its reader init cell from the map, moving the cached reader's `Arc` onto a drop list;
  - refuses every later lease and re-initialization.

  L3 is then released and the detached reader is dropped outside it, which closes its files and mappings and decrements the cache-handle count. An initialization still in progress keeps its marker. When it finishes, it sees `revoked`, discards the reader it opened, and never installs it.

  Two things revoke an object:
  - a publisher transaction that drops the object's resource from the layout (an obligation, §5.6);
  - an object that **no** generation (layout or withheld) and no build root references any longer (GC, §4.5).

  Lapse and unknown never revoke: their objects stay referenced.
- **Deletable** (`revoked → doomed`, the claim). Under L3, an object may be claimed only if **all** of these hold:
  - its state is `revoked`;
  - its lease count, cache-handle count, external-handle count and init-in-progress count are all zero;
  - no active generation, retained generation, withheld map or build root references it.

  Nothing overrides these. An obligation may only *cause* the reference conditions to become true, in two ways:
  - by publishing a layout without the resource, which also turns retained generations naming it into garbage;
  - by cancelling the resource's build roots through the admission gate.

  It never overrides the lease or handle drain. Deletion happens after L3 is released, and only for objects this caller claimed.

**Why it cannot deadlock.** Revocation is unconditional, and it removes the only holder that perpetuated itself: the cache. What remains is held by in-flight workers. Those workers never wait on retirement or GC, and they finish at their next checkpoint or end-of-request check (§4.4, §8.3), which refuses a revoked resource. Each count is decremented with a condition-variable notify, so a waiting purge or GC pass wakes **outside** every lock and retries its claim. Purge latency is therefore bounded by worker progress, not by a cycle (Q7).

**Reader init cells (F11, closed with R4-1).** The map lock is held only to find or insert a cell, and the open runs inside the cell (closed in revision 2). Cells are keyed by object ID. A `revoked` object has no cell, and an attempt to create one is refused.

**Tests (S2 and S3):**

- **F1 interleaving.** Worker W reads generation G and pauses, by test hook, *before* registering. G becomes unretained. GC runs. W then registers. In the linearized design, W either never held G outside L3 (its pin re-reads the pointer and leases G′) or, through a deliberately stale test path, presents G's objects and is refused as `revoked` or `doomed` and retries. No lease ever succeeds on a revoked or doomed object. No leased object is ever deleted.
- **Publisher serialization.** Two concurrent in-process publishers (reconcile and attach) submit transactions. Every published generation passes revalidation, and none overwrites another's swap.
- **Admission.** A builder is paused after its eligibility and gate check but before its filesystem operation, while an obligation applies. Revocation completes without waiting, and `revoked` is reported. The builder's attach is refused by revalidation. The purge waits at drain. The builder finishes. The purge deletes what the builder wrote, and its final scan is clean.
- **Idle reader (R4-1, F11).** An object is opened by a request that then finishes, so its reader is idle and cached with open mappings.
  - **Retirement path:** an obligation revokes the object. The test asserts that revocation succeeds with the cache handle still held; that the cell is detached and its reader dropped outside L3; that re-initialization is refused; that the claim succeeds only after every count reaches zero; and that the final scan finds no process file descriptor referring to the object.
  - **GC path:** the same, for a superseded object of an **active** room.
  - **Concurrency:** an initialization racing the revoke discards its reader.
- **Reader isolation.** Room B's reader init is blocked while room A's cached access completes.
- **No lock held across I/O.** A debug lock tracker asserts that L3 is never held across I/O and that the actor never waits inside a transaction.

### 4.4 Request workers, deadlines and capacity (F7, F16)

**Leases are owned by the worker.**

- The handler creates a cancellation token and a drop guard. If the handler future is dropped, by client disconnect or the HTTP stack, the guard cancels the token.
- The worker (in the openclast profile, a thread of the mode's dedicated executor, below) calls `pin_and_lease` itself and holds the leases and reader handles in RAII guards.
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
- **Reserved execution capacity, not just admission (R4-5).** Each pool owns a **dedicated executor**: exactly `W_L` (or `W_S`) OS threads, created at startup, used by nothing else. Search workers no longer run on Tokio's shared blocking pool.
  - A permit is a claim on one of those threads. It is acquired in the async handler, moves into the worker, and is released only when the worker exits.
  - A cancelled but still-running worker keeps its thread and its permit. Stalled semantic workers can therefore exhaust only `W_S` threads. They can never delay a lexical request's start of execution.
  - Every other blocking producer stays off the lexical executor: reconcile, the builder (its own thread, §4.9), list and report I/O, startup loads, and GC deletion.
- **Fair admission:** each pool queues FIFO, up to `Q` waiters. A waiter waits for a permit at most until the earlier of its two limits. Beyond `Q` waiters the request is refused immediately with `503 busy`.
- **What is and is not isolated.** Lexical **admission and thread availability** are independent of semantic load. CPU cores, SMT siblings, memory bandwidth, caches, the page cache, disk I/O and the allocator remain shared. Lexical latency can still rise under semantic load, and no general latency isolation is promised.
- **Desktop** keeps today's behaviour, with no pools.

**Residual (A7, corrected for R4-6).** Queueing delay, `busy` and `deadline_exceeded` are a **shared-service timing and availability residual**. They depend on other workloads, which includes other users' request rates **and the data-dependent cost of their requests**. Exact scans cost O(chunks in `E`), and lexical search and hydration costs depend on content. So a caller who holds its own load fixed may observe outcomes that vary with the size or contents of rooms it cannot see.
- **What the residual exposes:** no room identifiers, no content fields, and no change to any successful result.
- **What it does not promise:** that nothing about other rooms' size, content or activity can be inferred.
- **Error equality** (§10) covers results and every deterministic refusal. It explicitly excludes these load-dependent outcomes.

This residual is separate from A5's readiness-timing limit and is disclosed separately.

**Tests (S2 lexical, S7 semantic):**

- At each barrier (pin, embed, open, scan, hydrate), inject: execution-budget expiry; capability expiry; both at once (expect 401); client disconnect; a panic.
- Assert: refusal within the limit; leases, handles and permits released only on worker acknowledgement; registry counts return to zero; a waiting purge completes after acknowledgement.
- **Controlled stall test:** fill `W_S` with workers stalled at the uninterruptible embedding barrier. Assert that lexical requests acquire a lexical thread without waiting on any semantic worker, and complete within their execution budget. The test measures latency but asserts no general latency bound. Semantic requests queue and then return `busy` or `deadline_exceeded`.
- **Executor separation:** a debug assertion confirms that no search worker runs on Tokio's blocking pool and that no non-search task runs on a search executor.

### 4.5 Reachability garbage collection

An object is **reachable** if any of these references it:
- the active generation's layout or withheld map;
- a retained generation that names only resources listed active;
- a live build root;
- a lease;
- a reader handle.

Everything else under `objects/`, `staging/` and `generations/` is garbage. So is any layout-v2 tree no longer covered by §4.10.

**GC never deletes on its own authority (HW §3.4 rule 5).** Each piece of garbage is handled according to its attribution: the resource in its descriptor or journal.
- **Attributed to a resource listed `active` in the last applied list** (for example, superseded objects of a live room): revoke, drain and delete (§4.3). This is housekeeping inside an active holding, not deletion of a holding.
- **Attributed to an obligated resource:** handled by that obligation's purge (§5.6).
- **Attributed to an unknown or tombstoned-unlisted resource, or unattributable:** moved to `quarantine/` and entered in the ledger. It is **never deleted** except by a `quarantine_release` (§5.5).
- **Before the first list applies after startup:** nothing is deleted. Only tombstone revocation runs.

**At startup**, before the listener binds:
1. run migration recovery (§4.10);
2. load holder state (§5.2);
3. revoke every tombstoned resource through the publisher, and write `rollback.json` if tombstones exist;
4. resume valid resumable staging as build roots (§11.2).

The listener then binds. Requests refuse `resource_unavailable` until the first list applies (§5.3). Classification-dependent GC and purges run after it.

**Periodically, and after every publication**, the same rules apply. Generation records are deleted only through publisher transactions.

**Retained generations.** A retained generation that names a tombstoned or unlisted resource is itself garbage. Retention for crash recovery never outranks an obligation. The objects of an unknown resource are carried in the active generation's withheld map, so dropping the old record never deletes them.

A deletion failure moves the artifact to quarantine (§5.5). GC never blocks serving.

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
- Attach takes an admission token (§4.3), checks that the resource is listed active, leased and not tombstoned (§5.4), and confirms the layout still maps the resource to the same lexical object. It then performs the durability order (§4.2) and publishes.

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

**Minimum rollback-compatible binary (MRCB).** The MRCB is the first release that contains **both** S2 and S3 (§12): the layout-v3 objects, the publisher, the Holdings consumer (§5), obligation purge and reporting, and A2 phase-1 verification. Any release at or above the MRCB enforces holdings at startup and before every rebuild.

**Upgrade protocol.** Each marker is written atomically and fsynced. `upgrade.json` holds `{ state, from, to }`.

1. Write `upgrade.json = { state: "building", from: <v2 generation> }`.
2. Apply a fresh verified list (§5.3). Build layout-v3 objects and the generation record only for resources listed `active`. v2 partitions of other resources are classified like any held data (§5.3): purged under an obligation, otherwise withheld.
3. Copy the old pointer to `current.v2.json` and fsync it.
4. Write `upgrade.json = { state: "switching", to: <v3 generation> }`.
5. Switch `current.json` to the v3 generation and fsync the data root. **A crash barrier sits immediately after this step.**
6. Write `upgrade.json = { state: "published" }`.
7. **Confirmation.** `kwiry gc --confirm-upgrade`, or the first successful serve after a configured grace period, deletes the v2 trees, `current.v2.json` and `upgrade.json`. Before deletion, any v2 partition of an unknown or unattributable resource is moved to quarantine (§4.5). Confirmation also closes the fast-rollback window.

**Recovery matrix.** The MRCB applies this at startup, before holder-state load and tombstone revocation (§4.5):

| `upgrade.json` | `current.json` points to | Meaning | Recovery |
|---|---|---|---|
| absent | v3 | steady state | none |
| absent | v2 | not yet upgraded | run the upgrade (refuse to serve v2) |
| `building` | v2 | crashed during build | discard unpublished v3 objects and records as garbage; restart at step 1 |
| `building` | v3 | impossible under the protocol | fail closed: refuse to start and tell the operator to re-run the upgrade; v3 is treated as unpublished |
| `switching` | v2 | crashed before the pointer switch | if the target v3 generation validates, resume at step 5; otherwise discard it and restart |
| `switching` | v3 | **crashed after the pointer switch** | roll forward: write `published` (step 6) |
| `published` | v3 | upgraded, unconfirmed | normal; fast rollback possible only while the window is open |
| `published` | v2, window open | operator restored the old pointer for a supported fast rollback | the MRCB refuses to serve v2; the operator either runs the pre-custody binary or re-upgrades |
| any or absent | v2, **`rollback.json` present or tombstones exist** | an **unsupported** rollback ran after the window closed | refuse to serve v2; raise `unsupported_rollback`; re-upgrade from a fresh list; v2 data of obligated resources is purged, and everything else is withheld or quarantined |
| any | missing or invalid | pointer lost | choose the newest valid **v3** generation; never fall back to v2 automatically |

**One support rule for rollback (R4-7).** Revision 4 both allowed and forbade pre-custody rollback after the MRCB had run. Revision 5 adopts one rule:

> **Rollback to a pre-custody binary is supported only inside the fast-rollback window.** The window opens at the pointer switch (step 5). It closes at the **earlier** of upgrade confirmation (step 7) and the moment the **first tombstone becomes durable** (§5.6, step 0). Outside the window, pre-custody binaries are unsupported in the governed profile, and **deployment pinning of the MRCB image is mandatory** from the moment the window closes.

- **Why tombstones close it.** A pre-custody binary cannot read holder state, list holdings or honour obligations. Once anything has been retired, such a binary could rebuild the retired room from configuration and source files. Before any retirement, there is nothing retired for it to restore.
- **Markers.** When the window closes, the MRCB writes `rollback.json = { fast_rollback: "disabled" }`, deletes `current.v2.json`, fsyncs both, and raises the operator alert `fast_rollback_closed`. The v2 trees become garbage under §4.5's attribution rules. An old binary does not understand these markers, which is why deployment pinning, not the markers, enforces the boundary. The markers let the MRCB **detect** a violation afterwards (matrix row above). They do not prevent one.
- **What holds during a supported rollback.** The pre-custody binary serves under shipped IG-1 rules: configured rooms, capability-gated. A6 is suspended while it runs:
  - no lease;
  - no list;
  - no withholding of unknown rooms;
  - no obligation processing.

  Capabilities are still minted only for active IDs (§5.1 (b)), so its stale-authority exposure is the capability TTL, as in IG-1 today. Retirements decided at the authority during the rollback stay listed as obligations. The MRCB processes them on re-upgrade. Holder state is untouched, because the old binary never opens the holder directory.
- **Honesty.** A6's guarantees hold only while a binary at or above the MRCB operates the data root. Inside the window, a supported rollback suspends them as stated above. Outside the window, running a pre-custody binary is a deployment error. Kwiry detects it on the next MRCB start, but cannot prevent it.

**Rebuild fallback.** A rebuild always runs through a binary at or above the MRCB, from a fresh verified list (§5.3).

**Headroom:** roughly twice the governed lexical index until the fast-rollback window closes.

**Tests (S2 and S3, combined):**

- A crash at each step 1–6, including the barrier right after the pointer switch.
- Every row of the recovery matrix.
- **Window closes on the first tombstone.** An obligation applies, then a deletion failure, then a crash, then an attempted fast rollback. Assert that `rollback.json` and the tombstone are durable before the first revocation publication; that the v2 trees are purged, withheld or quarantined according to attribution; that restoring the old pointer is refused; that rebuild requires a fresh list; and that the retired room is never rebuilt.
- **Supported rollback inside the window.** No tombstone exists, so the pre-custody binary starts on the restored pointer. Re-upgrade then lists afresh, and an obligation created meanwhile is processed.
- **Unsupported rollback detected.** A pre-custody binary rebuilds v2 after the window closed. On the next MRCB start, the `unsupported_rollback` row fires, nothing is served from v2, and the retired room's rebuilt data is purged under its still-listed obligation, or withheld if it is no longer listed.

## 5. Holder lifecycle: Kwiry as a `Holdings` consumer

Revision 5 drops Kwiry's bespoke custody query, `custody-cache.json` and its state-equality table. Kwiry now consumes the engine-neutral lifecycle contract that the owner approved and OpenClast froze on 2026-11-17. Kwiry is one **holder** of kind `engine`. It pulls a signed, complete list of what it holds (`Holdings.List`) and reports purge progress (`Obligation.Report`). The authority owns every obligation. Kwiry keeps only refusal state, and that state can never widen access.

**Binding inputs:**
- **The contract:** *OpenClast Holdings wire contract*, FROZEN 2026-11-17, `.workspace/kwiry-integration/HOLDINGS-WIRE.md` in the OpenClast workspace. Below it is cited as **HW §n**. HW §13 holds Kwiry's agreed answers and amendments A–C. This page restates nothing the contract owns; it defines only Kwiry's consumer behaviour.
- **Owner decisions Q1–Q6 (2026-11-17):**
  - Q1: pull a signed list; nudges come later (S5).
  - Q2: one generic Holding record.
  - Q3: the existing governance store.
  - Q4: an engine serve lease of 15 minutes, then quarantine, never deletion.
  - Q5: delete-only credentials, for devices only, not Kwiry.
  - Q6: crypto-erase comes later; until then receipts say `"confirmed"`.

### 5.1 Role, scope and what stays at the authority

- **(b), unchanged and mandatory.** OpenClast mints capabilities only for enrolled, active, random IDs. A retired ID is never minted.
- **Authority-owned (P-a):** holdings, phases, obligations, deadlines, escalation, completion records and receipts. An obligation exists at the authority until it is `complete`, whether or not Kwiry remembers it. That is what closes R4-3.
- **Kwiry-owned:**
  - its holder key;
  - a durable holder-state file: high-water, grow-only tombstones and a write-ahead report outbox (§5.2);
  - in-memory leases;
  - the data root.
  
  Tombstones and the high-water only ever **refuse**. Nothing local authorizes serving, building or deleting.
- **Mapping.** A list item's `resource` is `{kind: "room", id: <room_id>}`, where `room_id` is the holder-scoped `random-128-v1` ID Kwiry already uses (HW §3.3). Kwiry's resource tuple `{tenant_id, vault_id, room_id}` comes from operator configuration. The list confirms `room_id` only:
  - Configuration validation refuses two configured tuples with the same `room_id` under one holder, and refuses tuples with more than one `tenant_id` under one holder, because a holder is bound to one provider id-space (HW §2.1; §14.2 Q5).
  - A capability's tuple must still match a mapped tuple exactly (§6.1). A mis-bound tenant or vault therefore fails closed; it never widens access.
- **Holder granularity (HW §13):** one holder per data root. One holder has one key, one holder-state file and one high-water mark. Replicas use separate data roots and are separate holders, each with its own obligations. A room counts as purged only when every holder's obligation for it is complete.
- **Slice scope.** Kwiry implements HW S1 in release unit L. HW S2 (epoch chain, tombstone merge) is a later Kwiry slice. Until then, any epoch change fails closed (§5.3).

### 5.2 Holder identity and durable holder state

**Location.** Holder state lives in a holder directory **next to the operator configuration, never inside the disposable data root** (HW §13). Every file is `0600` and the directory is `0700`:

```text
<config-dir>/holder/
  holder.key          # Ed25519 (preferred) or ES256 private JWK; never leaves the host
  holder.json         # { holder_id, lifecycle_base_url, enrolled_jkt, pinned lc-* JWKS, initialized_at }
  holder-state.json   # durable, fsynced; below
  receipts/<obligation_id>.json   # final-scan records and verified acks (no resource IDs)
```

**Enrollment.**
- `kwiry holder init` creates the key and an empty `holder-state.json`, then prints the public JWK.
- The OpenClast admin enrolls it with `enroll-holder --kind engine` (HW §2.1) and returns the `holder_id` and the lifecycle JWKS. Kwiry pins both in `holder.json`.
- Key rotation is HW §2.1's overlap-window admin action.
- A lost key cannot sign, so every list fails. Leases then lapse, data is withheld and nothing is deleted until the key is re-enrolled.

**`holder-state.json`** (schema 1) holds exactly:

| Field | Purpose |
|---|---|
| `holder_id` | Must equal `holder.json`; on mismatch, fail closed |
| `high_water` | `{epoch, seq}` of the last applied list, or `null` before first contact |
| `tombstones` | Grow-only set of `{resource_id, holding_id}` ever seen `terminating` or `retired` (HW §3.4 rule 6) |
| `outbox` | Per open obligation: the highest-state report **written before it is sent**: its state, its idempotency key and its canonical claims minus `jti`, `iat` and `exp` (§5.6) |
| `quarantine_releases_applied` | IDs of `quarantine_release` items already executed (§5.5; shape pending, §14.2) |

**Write rule.** Write to a temporary file, fsync it, rename it, then fsync the directory. A failed write aborts whatever depends on it. In particular:
- no list is applied unless its tombstones and high-water are durable first;
- no report is sent unless its outbox entry is durable first.

**Loss and reset (HW §13, accepted).**
- `holder-state.json` is created at `holder init`. A **missing, unreadable or mismatched** file afterwards is a reset.
- On reset, Kwiry:
  - raises the `holder_state_reset` alert;
  - moves any unreadable file aside, keeping it;
  - sends `high_water: null`;
  - trusts the first verified, nonce-bound list at whatever epoch it carries (trust on first use).
- Accepted residual: if the authority is restored at the same moment, it is detected only by HW S2's off-store epoch record.
- **Losing only the data root is not a reset.** Tombstones and the high-water survive, and Kwiry rebuilds from the next list (§5.6).

### 5.3 Listing and applying a list

**When Kwiry lists:**
- **At startup**, after migration recovery (§4.10). The listener binds, but every governed request refuses `resource_unavailable` until the first list applies. Leases are not persisted, so a restart never serves on a previous process's lease.
- **Every `relist_seconds`** from the last applied list (engines: 300), with ±10 % jitter. Failures back off exponentially and never exceed HW §7's start rate.
- **Before every rebuild** (`kwiry index`, an embedding-identity change, the upgrade). The list must start after the rebuild was requested.
- **Early, per amendment B.** A capability that names a `room_id` absent from the current list triggers one relist. At most one is in flight, at most one every 5 s, and the triggers coalesce. The request itself refuses `resource_unavailable` at once and does not wait. Tombstoned IDs never trigger a relist.
- **On a `410 list_expired`**, Kwiry discards every page and relists from `cursor: null` with a fresh nonce. After 5 consecutive `410`s it backs off and raises `lifecycle_list_livelock` (HW §3.7).

**Request.** Each request is a holder-signed request object (HW §2.2, §3.1):
- a fresh nonce of at least 128 bits, identical on every page of one list;
- the stored `high_water`;
- `page_size: 500`.

**Verification.** Each page goes through the HW §3.8 checklist in order, failing closed. The whole list then must pass the HW §3.7 consistency rules. Kwiry adds three item checks:
- every `resource.kind` is `room`;
- no `resource.id` appears twice in one list;
- the item's phase and obligation form a permitted combination (HW §3.3).

Any failure discards the entire list and raises `lifecycle_list_invalid`. The previous state stays, and its leases keep running down.

**High-water (HW §3.7), enforced by Kwiry:**

| Received `rev` versus stored high-water | Action |
|---|---|
| `high_water` is `null` (first contact or reset) | Accept |
| Same epoch, `seq ≥ hw.seq` | Accept; set `hw = rev` as part of the apply |
| Same epoch, `seq < hw.seq` | **Regression.** Reject, raise `lifecycle_regressed`, keep the previous state, and cite this list in no report |
| Different epoch | **S1: reject** with `unexpected_epoch` and an alert. Leases run down. HW S2 later accepts only a verified chain |

Equal `seq` with different bytes is not an error. `state_digest` covers `list_id` and `nonce`, so it differs on every list by design. Kwiry never compares digests. It echoes them only in `list_ref`.

**Classification.** The list is the holder's **complete** projection at `rev` (HW §3.4). Every resource Kwiry holds therefore has exactly one class, computed from the list alone. No status is ever request-dependent, which closes R4-2. "Holds" means any of these names the `room_id`: configuration, a layout, the withheld set, an object descriptor, a staging journal, a quarantine ledger entry, or a layout-v2 tree.

| Class | Condition | Serving | Building | Data |
|---|---|---|---|---|
| **active** | listed `active`, not tombstoned | until its lease deadline (§5.4) | allowed while leased and configured | kept |
| **lapsed** | listed `active` earlier, lease deadline passed, no newer list | refused | paused | kept in place |
| **obligated** | listed `terminating` (any obligation state), or `retired` with `escalated` | refused now | cancelled | purged under the obligation (§5.6) |
| **retired-complete** | listed `retired` with `complete` | refused | never | if any artifact remains: purge, then report again (HW §3.4 rule 4) |
| **tombstoned-unlisted** | in the tombstone set, absent from the list | refused forever | never | **withheld**; counted as unattributed (HW §3.3 retention) |
| **unknown** | not in the list, not tombstoned | refused | never | **withheld**; counted as unattributed (§5.5) |

A tombstoned ID listed `active` is a `tombstone_conflict` alert. It stays refused (HW H13). In HW S2 it also goes into a merge request.

**Atomic apply.** One apply is one ordered sequence. Each step completes before the next:

1. Add every `terminating` or `retired` ID to the tombstones, and set the high-water to `rev`. Write both durably (§5.2).
2. Submit one publisher transaction (§4.3). Its layout drops every obligated, retired, tombstoned and unknown resource. Unknown resources' objects move to the generation record's **withheld** map: they stay referenced, so GC keeps them, but they are never servable. The same transaction swaps the in-memory lease table under L3, so `pin_and_lease` sees the new leases and the new layout in one snapshot.
3. Under L3, revoke the objects of every resource dropped in step 2 (§4.3). Revocation evicts cached readers unconditionally.
4. Rebuild the obligation work queue from the list's obligations, and the unattributed count from the classes above.

A crash after step 1 leaves the tombstones durable. Startup then revokes those resources before serving (§4.5), so the safe direction always wins.

### 5.4 Serving rule and leases

A resource `r` in `E` is **servable** only while all of these hold:
- it is configured;
- it is mapped in the active layout;
- it is not tombstoned;
- it is listed `active` in the last applied list;
- `now` is before its **lease deadline**.

`pin_and_lease` checks this under L3 (§4.3, gate step 6), and the end-of-request check repeats it (§8.3).

**Lease deadline:** the earlier of the item's `quarantine_after`, judged by the wall clock, and `t_recv + (quarantine_after − iat)`, judged by the **monotonic** clock from when the page verified. A wall-clock step on Kwiry's host therefore cannot extend serving. HW H17's ±120 s skew applies only to the wall-clock comparison.

**Lapse (Q4).** At the deadline, `r` refuses with `resource_unavailable` and its A4 freshness becomes `unavailable`. Builds and publications for `r` pause. **Nothing is deleted or moved.** The next verified list that shows `r` active with a later `quarantine_after` restores serving without a rebuild. Lapse is per item (HW §13). If the authority stays unreachable, every room lapses within 15 minutes of its last renewal, lexical serving included. This replaces revision 4's "keep serving during an outage" rule, deliberately (§12, R6).

**Stale-authority window.** Two facts bound the window:
- OpenClast never mints a capability for a retired ID (b), and a `terminating` item's `quarantine_after` is already past.
- So once a list carrying the retirement applies, the room stops at once.

Before that list arrives, the room can still serve two kinds of capability:
- those minted before the retirement, which live at most the capability TTL (60 s);
- and nothing else, because no new ones are minted.

The window is therefore at most the capability TTL, whether or not the authority is reachable. OpenClast's fail-closed post-check is the user-facing control (§14, ask 6). A list or nudge only shortens the window. The lease is not a revocation bound; it bounds how long Kwiry serves **without any authority contact**.

### 5.5 Unknown data, quarantine and release (amendment C)

- **Kwiry never deletes data it cannot attribute to a listed obligation** (HW §3.4 rule 5). Neither lease expiry, nor authority errors, nor regression, nor reset, nor being unknown authorizes deletion.
- **Withheld vs. quarantined.**
  - *Withheld* data stays where it is and is referenced by the generation record's withheld map. That covers unknown and tombstoned-unlisted resources whose objects are intact.
  - *Quarantined* data has been moved into `quarantine/<random>/` and recorded in `quarantine/ledger.json`, the data-root quarantine ledger: entry name, attributed `room_id` if readable, and a monotone ledger sequence. That covers artifacts that are unreadable, failed to delete, or come from garbage GC could not attribute (§4.5).
  
  Neither kind is ever leased, opened, built from or served.
- **Units.** A unit is one artifact directory: an object, a staging build, a quarantine entry, or one resource's partition in a layout-v2 tree. The authority checks only zero versus non-zero (HW §4.1).
- **`quarantined_unattributed`** is holder-wide (HW §4.1). It counts:
  - every withheld unit of an unknown or tombstoned-unlisted resource;
  - every quarantine entry that is not attributed to a currently listed obligation.
- While it is above zero, Kwiry never reports `complete` for **any** obligation (HW §4.1, H1).
- **How it clears:**
  1. a later list classifies the resource. If it is active, the data returns to the layout without a rebuild. If it is obligated, it is purged under that obligation.
  2. Otherwise a human-authorized `quarantine_release` arrives in a verified list.
- **Executing a release.** Kwiry deletes **exactly** the set it held when the release was issued. That set is the ledger and withheld entries counted in the report the release cites. Kwiry then fsyncs the parents, records the release ID in `quarantine_releases_applied`, and reports the new count. The item's wire shape is not yet in HW §3.2/§3.3, so this behaviour waits on §14.2 Q1. Until it is specified, a list carrying any unrecognized element is rejected whole.
- **Operator actions are outside the holder.** An operator who deletes the whole data root, as the contract's disposability rule (CONTRACT §1.1) permits, has not been overridden by Kwiry. The next lists rebuild active rooms, and open obligations complete against an empty root (§5.6).

### 5.6 Obligations: revoke, purge and report

Kwiry keeps no local history of what it owes. The work queue is rebuilt from every applied list's obligations, and the tombstones keep those IDs refused forever. Recovery therefore works when the last local reference to a room has gone: after a crash, after config removal, or after losing the whole data root. The authority's obligation stays listed until `complete`, so Kwiry re-learns it and re-checks absence. This closes R4-3.

**Per obligation** (each step idempotent and crash-safe):

0. **Close the fast-rollback window** (§4.10). The window closes when the first tombstone becomes durable. Immediately afterwards, and before the step-2 publication, Kwiry writes `rollback.json`, deletes `current.v2.json` and fsyncs both. If startup finds tombstones but no `rollback.json`, it writes the marker before doing anything else.
1. **Tombstone.** Done durably by the apply (§5.3, step 1).
2. **Revoke retrieval.** Done by the apply's publication and L3 revocation (§5.3, steps 2–3; §4.3):
   - new pins cannot lease the resource's objects;
   - cached readers are evicted and their handles dropped;
   - in-flight requests refuse at their end-of-request check.
   
   **Revocation never waits for any drain.**
3. **Report `revoked`** once steps 1–2 are durable, unless `max(outbox state, last_accepted)` is already at least `revoked`. This is the "retrieval revoked, purge pending" state R4-4 asked for, and it is sent **before** any drain.
4. **Drain**, waiting outside every lock:
   - set the resource's admission gate to `closing`;
   - cancel queued units and stop a running unit at its next yield;
   - wait for the admitted count to reach zero, then set the gate to `closed`;
   - wait for each revoked object's lease, handle and init counts to reach zero (§4.3).
   
   A builder that was already admitted cannot republish, because the publisher refuses tombstoned resources in every transaction.
5. **Delete.** Claim `revoked → doomed` and delete:
   - the resource's objects and staging;
   - the generation records naming it (through the publisher);
   - its layout-v2 partitions;
   - its attributed quarantine entries.
   
   Fsync every parent directory. An artifact that cannot be deleted is renamed into quarantine and attributed. Deletion retries with backoff. Healthy resources and lexical search stay online (P-b).
6. **Durability barrier and final scan.**
   - **Barrier, run fresh on every pass** (including after a restart): fsync `objects/`, `staging/`, `generations/`, `quarantine/` and the data root.
   - **Scan:** check every §5.7 location and confirm that no process file descriptor refers to a deleted path (`/proc/self/fd`).
   - **Record:** write a canonical JSON final-scan record to `receipts/<obligation_id>.json`. It holds `holder_id`, `obligation_id`, the scanned location names, `found`, `fd_refs`, `barrier_ok`, `scanned_at` and `binary_version`, and no resource ID.
7. **Report** the state computed below.

**State machine (Kwiry's mapping of HW §4):**

| State | Kwiry sends it when | Counts |
|---|---|---|
| `revoked` | steps 1–2 are durable; drain or deletion may still be pending | `units_remaining` = artifacts not yet deleted |
| `incomplete` | a purge pass has run steps 4–6 and either `units_remaining > 0` (deletion failure, quarantined attributed artifact, a parent fsync or barrier failure since the last deletion) or `units_remaining == 0` with `quarantined_unattributed > 0` | as measured on that pass |
| `complete` | `units_remaining == 0`, the barrier succeeded **after** the last deletion, the final scan found nothing, no fd refers to a deleted path, **and** `quarantined_unattributed == 0` | `method: "logical_delete"`, `completed_at`, `evidence: {kind: "witness-digest", digest: "sha256:" + SHA-256(final-scan record bytes)}` |

- **Coverage.** Drain pending stays `revoked`. A successful unlink whose parent fsync failed counts as a remaining unit until a later barrier succeeds. Unknown attribution is the holder-wide count. Every R4-4 case therefore has exactly one state.
- **Monotonicity (HW §4.4).** Kwiry sends a state only if it is at least `max(outbox state, last_accepted.state)`.
  - A strictly higher state, or the same state with changed counts (at most once per relist interval), gets a new report.
  - A lower computed state sends nothing new. If the outbox holds an unacknowledged higher report, Kwiry resends exactly that report, which describes a true past state stamped with its `completed_at`.
- **Idempotency key.** `ik_` + base64url(SHA-256(`"kwiry-report/v1"` ‖ 0x00 ‖ JCS(claims minus `jti`, `iat`, `exp`)))[0..43].
  - It is stable for a given body, and every retry is byte-identical apart from `jti`, `iat` and `exp`. So a lost ack yields the original ack (HW H10).
  - A changed body gets a new key, so `409 idempotency_conflict` can only mean a bug.
  - Keys carry no IDs.
- **Write-ahead.** The outbox entry is fsynced before the first send. It is cleared only by a verified ack at or above its state. A crash between write and send therefore resends; it never sends lower (HW H11).
- **`list_ref`** is the list Kwiry acted on. It is never a regressed or rejected list (HW H3). A list ref stays valid across restarts, because `list_tag` is stateless. On `409 stale_list_ref`, Kwiry relists and re-reports under the new ref, which means a new key with the same state.
- **Acks.**
  - **Verification:** `typ`, `lc-*` key, `aud`, `sub`, `idempotency_key`, `report_digest` (equal to SHA-256 of the exact JWS sent), `accepted_state`, and `receipt_label: "confirmed"`.
  - **Storage:** the ack is stored in `receipts/` as the durable receipt. Kwiry never describes a `logical_delete` as anything stronger than "confirmed".
- **Error handling** (HW §4.5):

| Response | Kwiry's action |
|---|---|
| `409 state_regression` | Adopt the ack in the body |
| `409 complete_refused_unattributed` | Recompute and alert. A refused `complete` is treated as not accepted (§14.2 Q3) |
| `404 obligation_not_found` | Relist |
| `401` | Stop and raise `lifecycle_unauthorized` |
| `403 holder_not_authorized` | Refuse every held resource and alert |
| `429`, `503` or a network failure | Back off; leases keep running |

  Report traffic stays within HW §7's 10/s per holder.
- **Logs and events.** Kwiry's operator logs and observer events carry `holding_id`, `obligation_id`, states and counts. A resource appears only as a per-process keyed digest (§10). Kwiry never logs `resource.id`, nonces, `state_digest`, `list_tag`, cursors, JWS bodies or evidence digests (HW §8).

**Tests (S3).** The reference fake authority and the `lifecycle-vectors.v1.json` golden vectors (HW §9) run in Kwiry CI. Two sets of tests:

*HW H1–H17 holder oracle*, using built binaries and synthetic rooms only:
- H1–H2: mixed list and unattributed quarantine;
- H3–H4: regression, nonce mismatch and replay;
- H5–H6: `typ` and `aud` swaps, including Kwiry's own capability verifier refusing every lifecycle artefact and the reverse (HW §2.5);
- H7: authority unreachable past `quarantine_after`, so everything lapses and **no byte is deleted**;
- H8–H9: `410` on page 2 and malformed pagination;
- H10: a lost ack;
- H11: a restart mid-purge;
- H12: holder state wiped;
- H13: a tombstoned ID re-listed active;
- H16: report content;
- H17: lease timing.

*Kwiry-specific tests:*
- a crash after each of steps 0–7;
- a crash between the holder-state write and the publication;
- data root deleted with holder state intact, so the open obligation completes against the empty root;
- holder state deleted with the data root intact, giving `holder_state_reset`, trust on first use, and unknowns withheld rather than deleted;
- an early relist bounded to one in flight and one per 5 s, with the request refused rather than waiting;
- a monotonic-versus-wall-clock lease with the wall clock stepped backwards;
- unlink succeeds but the parent fsync fails, giving `incomplete` and then `complete` after a later barrier;
- R4-1's idle-cached-reader drain (§4.3);
- a paused admitted builder whose attach the publisher refuses after the tombstone;
- recreation under a fresh ID, which inherits nothing;
- the rollback-window tests (§4.10).

### 5.7 Allowed residual after `complete`

**Inside the data root: nothing that names or contains the obligated resource.** The final scan covers:

- `current.json` and every generation record, including both layouts and withheld maps;
- every object descriptor and every lexical `manifest.json` shard;
- `staging/`, including build journals;
- `quarantine/` and `quarantine/ledger.json`;
- `upgrade.json`, `current.v2.json` and `rollback.json`;
- every layout-v2 generation tree, including its manifest and partitions;
- the legacy `semantic/` directory, which must be absent in the openclast profile;
- lock and probe files;
- the process's open file descriptors.

**Outside the data root, stated and not covered:**

- **The holder directory (§5.2).** The contract requires it.
  - The tombstone set keeps the retired `room_id` and `holding_id` forever. Both are random 128-bit identifiers with no label.
  - `receipts/` keeps the final-scan record and the ack by `obligation_id`, with no resource ID.
- the operator-owned configuration file;
- external log sinks, where resources appear only as per-process keyed digests;
- storage-medium remanence: `logical_delete` is unlink plus fsync, not sanitization (HW amendment A; Q6);
- OpenClast's authority records.

## 6. Readiness, errors and HTTP mapping

### 6.1 Effective set and gate order

The **signed set** `S` is the capability's resources. The **effective set** is `E = S ∩ {vault_id, room filters}`. `E` alone drives the gates, leases, generation digest and freshness. The readiness probe uses `S`. If `E` is empty, the response is `200` with no hits, a digest over the empty set and freshness `current`; nothing is embedded or leased.

1. **Verify the capability.** The verified context keeps `exp`, the exact action, `jti`, resources and `max_limit` (A2). The action must match the route and the mode (§7.5).
2. **Require `mode`** in the openclast profile. The D2 default is applied at the seam.
3. **Validate** the query, controls and filters. Non-room filters are refused here for semantic and hybrid, before any embedding or open.
4. **Semantic and hybrid only (F6):** the model must be loaded with the expected `embedding_identity`, otherwise `mode_unavailable`. **Lexical never consults the model.**
5. **Acquire a permit** from the mode's pool (§4.4).
6. **The worker calls `pin_and_lease(E, class)`** (§4.3). Every resource in `E` must be mapped and servable: listed active, within its lease and not tombstoned (§5.4). Otherwise the call refuses with `resource_unavailable`, and an unlisted `room_id` triggers the early relist (§5.3).
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
| Resource unmapped, unlisted, lease lapsed, tombstoned, or retired mid-request; no list applied yet | `resource_unavailable` | 503 | `search_provider_failed`, no partial re-query |
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
2. If any resource in `E` is no longer mapped in the then-active layout, or is no longer servable (§5.4): `503 resource_unavailable`.
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

**Separate disclosed residual (A7).** Queueing delay, `busy` and `deadline_exceeded` are a shared-service timing and availability residual. They may depend on other workloads, including other users' request rates and the data-dependent cost of their requests, and therefore indirectly on the size or contents of rooms the caller cannot see. They expose no room identifiers or content fields and never change a successful result. Lexical admission and execution threads are reserved apart from semantic ones (§4.4), so stalled semantic work cannot take lexical threads. CPU, memory and I/O remain shared.

**Bounded stale-authority window (§5.4).** An already-issued capability for a just-retired room can be served until it expires: at most the 60 s TTL, before the list carrying the retirement applies. The OpenClast post-check is the fail-closed control for users. The 15-minute lease bounds how long Kwiry serves without any authority contact; it is not a revocation bound.

| Channel | Response |
|---|---|
| Readiness timing | Accepted under A5. Fair rotation; the probe returns enums only; advisory witness. |
| Service load | Disclosed under A7, which may reflect other workloads' data-dependent cost. Separate lexical and semantic pools, each with FIFO admission and a dedicated executor. |
| Query embedder | A dedicated instance is required before HTTP. It is necessary but not sufficient. Still shared: CPU and SMT siblings, memory bandwidth and last-level cache, page cache and disk I/O, the blocking pool, the allocator, and the query-model queue. These are documented residuals. Builders run at lower OS priority. |
| Lifetime and retirement | Linearized pin-and-lease, unconditional revocation separate from the doom claim, a single publisher, admission gates and handle drain. Retiring B never breaks A. |
| Holdings list and report | Server-to-server, holder-signed. Triggered by startup, `relist_seconds`, rebuilds and capability arrival (one in flight, at most one per 5 s). Users never see a list or receipt, and an early relist never makes a request wait. |
| Caches | None: no query-embedding, result or fusion cache. |
| Counts and errors | Byte-equal to the baseline, **except** the load-dependent `busy` and `deadline_exceeded` outcomes (A7) and lifecycle `resource_unavailable` caused by a lapsed lease. Fixed messages over `E` or `S` only. |
| Headers | A4 authorized-view digest and per-resource freshness. Class L ignores vectors. Desktop is unchanged. |
| Logs | No subject, actor, query text or fingerprint, query vectors, distances or chunk IDs. Resources appear only as per-process keyed digests. |
| Purge residue | §5.6 and §5.7, with an exact allowed residual, signed `Obligation.Report` receipts labelled "confirmed", and `logical_delete` stated honestly as unlink plus fsync. |

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
- On restart, a journal is resumed only if its resource is listed active and leased in the first applied list (§5.3), and its lexical object, `preparation_identity` and `embedding_identity` still match. Resuming re-registers it as a build root. Anything else is garbage.
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
- `Holdings.List` latency and page count at the expected room count; report latency; lease-lapse behaviour with an unreachable authority.

## 12. Slices

### Intentional changes to governed public behaviour in shipped releases

- **S1a:** the meaning of `X-Kwiry-Generation` and `X-Kwiry-Index-Freshness` changes.
- **S1b:** subject and actor stop being logged or retained. Capabilities are otherwise accepted as today.
- **Release unit L (S2 + S3, shipped together):**
  - a one-time rebuild into the object layout;
  - the header value changes once;
  - governed rooms are served only while listed `active` under a **15-minute lease**. If OpenClast is unreachable, governed search, **lexical included**, stops for each room within 15 minutes of its last renewal, and resumes on the next verified list. Nothing is deleted;
  - after a restart, nothing is served until the first list applies;
  - unlisted (unknown) rooms are withheld, never built and never deleted;
  - obligations purge and are reported with signed receipts;
  - the fast-rollback window closes at the first durable tombstone or at upgrade confirmation;
  - a holder key and holder-state file live next to the configuration, and the holder is enrolled with OpenClast.
- **A2 cutover**, a configuration step after OpenClast's transition (A2): identity-bearing capabilities become invalid.

### Build gates

- **`governed-lifecycle-v3`** (non-default) holds S2 and S3 code on `main` until both have merged. It is then removed in a single release, so lifecycle storage rules never ship without the Holdings consumer (F12).
- **`governed-semantic-internal`** (non-default) holds every vector-building, vector-publishing and vector-serving path until S10. No shipped release before S10 publishes a vector object, and class L headers ignore vectors by construction.

Desktop keeps its current headers, freshness and behaviour in every slice. Desktop regression tests run on every slice, including for the empty-resource-set desktop principal.

| Slice | Content | Build | Tests |
|---|---|---|---|
| **S0** | Amendment text A1–A7 | — | — |
| **S1a — scoped lexical headers (A4), transitional identity (F15)** | `X-Kwiry-Generation: v1-<hex>` over a **transitional per-resource revision**: SHA-256 over the resource's complete manifest entries, every persisted field of each `ManifestFile` (including `mtime_nanos`, `byte_length`, `content_hash`, `format_identity`, `extraction_profile`, `registration_fingerprint`, `chunk_count`, `coverage`, `outcome` and `warning`), canonically serialized in `source_key` order, plus the manifest-level versions (manifest, index format, chunking, preparation, format-identity schema). All length prefixes are u64-LE. Indexed and queryable `mtime` therefore changes the identity. Adds per-resource freshness and an explicit profile branch. | shipped | invariance under forbidden publish, reconcile and watcher events; **an mtime-range query (`mtime:[a TO b]`) across an mtime-only rebuild changes both results and identity consistently**; desktop header regression including `resources: []`; IG-1 witness re-run |
| **S1b — verified context; A2 phase 1 (F8)** | The context keeps `exp` and the action; a repeated `vault_id` is refused; subject and actor are dropped from `Principal` and logs. **Two bounded verification branches:** (1) **workload shape** — `sub` on the workload allowlist, no `actor` or `subject_revision`; (2) **legacy shape**, accepted only while `legacy_identity_accept_until` (configuration) is in the future — `sub` and `actor` must be present, distinct and non-empty as today, are validated, then discarded and never stored or logged. User subjects are **never** added to the allowlist. All existing issuer, audience, TTL, `kid` and algorithm checks apply to both branches. | shipped | both shapes accepted before the cutoff; legacy shape refused after it; user subject not on the allowlist refused in the workload shape; no subject or actor in logs or context |
| **S2 — objects, publisher, leases, migration (lexical)** | §4.1–§4.5, §4.10; worker-owned leases; deadlines; separate capacity pools with dedicated executors (R4-5); revoke/doom split with reader-cell detachment (R4-1); header switches to `v2-<hex>`. | `governed-lifecycle-v3` | F1 interleaving; publisher serialization; recursive-sync crash injection with nested index files; migration matrix with the post-switch barrier; idle-reader revoke-then-drain on both the retirement and GC paths; controlled stall test and executor separation; desktop regression; IG-1 W1–W13f |
| **S3 — Holdings consumer, purge, reporting (lexical)** | §5 in full: holder key and holder state; list, verify and atomic apply; high-water and `unexpected_epoch`; leases; withheld and quarantined data; obligation state machine; outbox; acks; `rejectLifecycleArtefact` in Kwiry's capability verifier (HW §2.5). `quarantine_release` waits on §14.2 Q1. | `governed-lifecycle-v3`; CI needs the HW §9 reference fake authority and golden vectors | HW H1–H13, H16, H17 and every §5.6 Kwiry-specific test; rollback-window tests (§4.10); purge witness over §5.7 |
| **Release unit L** | Removes `governed-lifecycle-v3`. **This release is the MRCB.** | owner checkpoint | full IG-1 witness on the release build; Holdings, lease, obligation and purge witness rows against a deployed OpenClast S1 authority |
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
- Release unit L needs:
  - A6 and A7;
  - OpenClast Holdings **S1** deployed **before** that release: list, report, lifecycle JWKS, holder enrollment and lease policy (§14 ask 9);
  - the HW §9 reference fake and golden vectors for CI;
  - answers to the §14.2 questions marked *blocking L*.
  
  HW S2 (epoch chain, merge) is **not** needed for L; Kwiry fails closed on `unexpected_epoch` until a later Kwiry slice consumes it. Watch nudges (HW S5) are not needed.
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
- `end_recheck_ok` and `end_recheck_refused`;
- lifecycle events (counts and holding or obligation IDs only, never `resource.id`): `list_applied` (rev seq, item counts by class), `list_rejected` (reason code), `lease_lapsed`, `object_revoked`, `object_doomed`, `report_sent` and `report_acked` (state), `holder_state_reset`.

### 12.2 Witness mapping

The mapping is by theme until OpenClast shares its V1–V16 numbers.

| Theme | Kwiry evidence | Extends |
|---|---|---|
| Two-user vector and hybrid | Asymmetric requests; leases and opens only within `E` | W2, W8 |
| Forbidden-partition invariance | Byte-equal results against an authorized-only instance under forbidden add, remove and mutate | W5 |
| **Readiness under forbidden load (owner decision)** | Instance 1: pending room A plus a large forbidden resource that sorts before A. Instance 2: only A. **Assert** exact equality once A is ready on both. **Record** time-to-ready as advisory only. | new |
| Mid-request retirement, timeout, disconnect | Forbidden room B retired at each barrier while A's request succeeds exactly. A's own room retired → `resource_unavailable`. Timeout and disconnect → bounded refusal, leases released on worker acknowledgement. | W8, W13a |
| Retirement purge | The fake authority lists B `terminating`. Kwiry tombstones B, revokes it, and reports `revoked` before drain. The data root is then inspected at **every §5.7 location: generation records, layouts and withheld maps, `current.json`, object descriptors, lexical manifest shards, staging journals, quarantine and its ledger, upgrade and rollback records, layout-v2 trees and manifests, legacy `semantic/`, and open file descriptors.** No artifact names or contains B. Every remaining object's resource is listed active or withheld. `complete` is sent only after the barrier and a clean scan, with `method: logical_delete` and the evidence digest. Crash-injected and deletion-failure runs report `incomplete`, then `complete`, and keep healthy rooms and lexical served. *(Ownership and artifacts are checked, not chunk IDs, which omit the room.)* | W13a–W13f |
| Holdings consumer | HW H1–H13, H16 and H17 against the reference fake. Unknown rooms withheld, never deleted. `quarantined_unattributed > 0` blocks `complete`. Regression refused. `unexpected_epoch` refused. Authority unreachable → every room lapses within 15 minutes with no bytes deleted. A capability for an unlisted room → one early relist and an immediate `resource_unavailable`. Holder state wiped → `holder_state_reset` and trust on first use. Data root wiped → the open obligation completes against the empty root. Stale-authority window bounded by capability TTL. | new |
| Idle reader at revocation (R4-1) | Cached reader present when B is revoked: revocation succeeds, the reader is dropped, the claim waits for all counts, deletion follows; the same on the GC path | W13 family |
| Capacity isolation (A7) | Semantic workers stalled at the embedding barrier; lexical requests on another room still get a lexical thread and succeed within budget; semantic requests return `busy` or `deadline_exceeded`. Latency is recorded, not asserted. | new |
| Rollback window | First durable tombstone → `rollback.json`; deletion failure → crash → attempted fast rollback: refused; the retired room is never rebuilt; an unsupported rollback is detected on the next MRCB start | W13f |
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
  - `reconciling → stale` when the pass ends incomplete, including when publishing is paused by a lapsed lease.
  - `current → stale` on a watcher event.
  - `stale → reconciling` when the next pass starts.
  - Any state becomes `unavailable` when the resource is unmapped or not servable (unlisted, lease lapsed or tombstoned, §5.4), and such a resource is never served.
  - A superseded leased object raises the resource to at least `stale` for its mode class.
- **Reported value:** the worst state over `E`, together with the configured evidence basis.

### A5 — semantic readiness-timing limit (owner decision 2026-11-17)

Semantic and hybrid results are exactly invariant to forbidden resources. Readiness *timing* is not. A fair shared scheduler builds vectors, and when a resource first answers rather than returning `semantic_not_ready` may depend on server load, including indexing load from resources the caller cannot see. Kwiry bounds this with fair scheduling and bounded work units, exposes readiness only as closed enums, and makes no claim of timing invariance. Any scheduling change that adds a channel requires amending A5.

### A6 — holder lifecycle, durable purge, holder state, rollback (§1.1, §5)

- **Authority.** OpenClast is the authority on holdings, phases and obligations, through the Holdings wire contract frozen 2026-11-17.
  - Kwiry serves and builds a resource only while its latest verified, nonce-bound list shows it `active` and its lease (15 minutes, Q4) has not lapsed.
  - Kwiry never applies a list with a lower `seq` in the same epoch. Until it consumes the epoch chain, it never applies a list from a different epoch.
- **Delete only under an obligation or a human-authorized release.**
  - Lease expiry, unknown data, authority errors, regression and holder-state reset never authorize deletion.
  - Unknown data is withheld or quarantined and counted.
- **Three reported states,** each sent with idempotent, signed `Obligation.Report` claims and never regressing:
  - **revoked:** no new pin can use the resource, its cached readers are evicted, and in-flight requests are refused at their end-of-request check. Sent before any drain.
  - **incomplete:** retrieval is revoked, but artifacts remain on disk or in quarantine, or holder-wide unattributed data exists.
  - **complete:** every data-root artifact of the resource is unlinked, every parent directory is durably synced in a barrier after the last deletion, no lease or reader handle remains, a final scan is clean, and holder-wide unattributed data is zero.
- **Receipts.**
  - The method is `logical_delete` (unlink plus fsync, not sanitization). Kwiry retries until it receives a signed ack labelled `"confirmed"` (Q6).
  - Failed artifacts are quarantined, only the obligated resource is affected, and the report says `incomplete`.
- **Clarification of §1.1.** The data root stays disposable: deleting it never widens access, and open obligations complete against the empty root. **Holder state is not index data.** The holder key, the high-water mark, the grow-only tombstones and the report outbox live outside the data root, are durable, and can only refuse. Losing holder state causes trust in the first verified list plus a `holder_state_reset` alert. That is an accepted residual: an authority restore at the same moment is caught only by OpenClast's later epoch chain. Rebuild converges given the same files and the same list.
- **Rollback.** Pre-custody rollback is supported only inside the fast-rollback window. The window closes at upgrade confirmation or the first durable tombstone. While the old binary runs, A6 is suspended. After the window closes, deployments must pin the MRCB; Kwiry detects a violation but cannot prevent it. The purge guarantee holds only while binaries at or above the MRCB operate the data root.
- **Stale-authority window.** An already-issued capability for a just-retired room can be served until it expires, at most the capability TTL.
- **Scope.** Purge is file unlinking, not cryptographic erasure. Crypto-erase is a later slice (Q6).

### A7 — shared-service timing and availability residual (new; corrected in revision 5)

In the OpenClast profile, request admission uses separate, fair lexical and semantic pools, and each has a dedicated executor of reserved threads.

- **What may vary:** queueing delay, `busy` and `deadline_exceeded` may depend on other workloads. That includes other users' request rates and the **data-dependent cost** of their requests, so these outcomes can vary with the size or contents of rooms the caller cannot see.
- **What they never do:** expose room identifiers or content fields, or change a successful result.
- **Equality:** they are excluded from the error-equality guarantee.
- **Lexical:** semantic load can never take lexical admission or lexical execution threads. CPU, memory and I/O remain shared, so no general latency isolation is promised.

This residual is separate from A5.

**Not amendments:**

- refusing non-room filters;
- exact baseline equality;
- `ranked_unpageable`.

## 14. OpenClast seam asks

Revision 4's asks 1–4 (custody query, canonical state, custody credentials, purge reporting) are **withdrawn**. The owner-approved Holdings contract, frozen 2026-11-17, replaces them. Asks 5–9 are unrelated to lifecycle mechanics and stay, with ask 9 updated.

### 14.1 Settled by the frozen Holdings contract

| Revision 4 ask | Now provided by |
|---|---|
| 1 Custody query with held-tuple statuses | `Holdings.List`: a complete per-holder projection at `rev`; unlisted means unknown (HW §3, §3.4) |
| 2 Canonical state; unchanged-snapshot refresh | Per-holder HMAC `state_digest` and stateless `list_tag` (HW §3.5, §3.6). Kwiry compares only `rev` against its high-water (§5.3) |
| 3 Credentials, keys, typed failures | Holder-signed request objects; a dedicated `lc-*` key with explicit `typ` and per-holder `aud`; verifiers separated in both directions; RFC 9457 error codes (HW §2, §4.5) |
| 4 Durable purge reporting | `Obligation.Report` with idempotency, monotone states, signed acks labelled `"confirmed"`, and authority-owned obligations (HW §4) |

### 14.2 Questions on the frozen contract

Kwiry cannot consume these points as written. They are questions, not edits; Kwiry has not changed any OpenClast file. *Blocking L* means Kwiry needs the answer before release unit L can claim the related path.

1. **`quarantine_release` has no wire shape** (*blocking L* for the release path). Amendment C says a signed release item is "carried in the next list". But HW §3.2/§3.3 define no field for it, and §3.3 makes any other element a malformed list. Kwiry needs:
   - its location, for example a top-level `quarantine_releases` array;
   - its fields;
   - whether `state_digest` and `list_tag` cover it;
   - **how it binds to "the set held when issued".** Kwiry proposes binding to the `report_digest` of the holder Report whose `quarantined_unattributed` count the human approved, plus that count, so Kwiry can resolve the exact set from its ledger.

   Until then, Kwiry rejects any list carrying an unrecognized element (§5.5).
2. **Unattributed quarantine with no open obligation** (*blocking L*). `quarantined_unattributed` travels only inside an `Obligation.Report`, which needs an `obligation_id`. A holder that holds unknown data but has no open obligation cannot tell the authority. The operator then never learns a release is needed, and the count after a release cannot be reported. Is a holder-level status report planned? Or how should Kwiry surface this?
3. **A refused `complete` and monotonicity.** HW §4.4 says a holder must never send a lower state than one it already sent. HW §4.5 tells a holder refused with `complete_refused_unattributed` to report `incomplete`. Kwiry treats a refused report as **not sent** for monotonicity. Please confirm.
4. **S1 behaviour after an authority restore.** In S1, a restore keeps the epoch and rolls `seq` back. Every engine holder then rejects every list as a regression until `seq` passes its old high-water, and all governed rooms lapse within 15 minutes. Is that the intended S1 outcome, and what is the operator procedure? Kwiry will not add a local override that accepts a regression, and resetting holder state would discard tombstones.
5. **One id-space per data root.** HW §2.1 binds an engine holder to one provider id-space, and HW §13 has one holder per data root. Kwiry will therefore refuse configuration that mixes `tenant_id`s, or that repeats a `room_id`, under one holder. Please confirm that a provider id-space never spans tenants. Otherwise, say how a multi-tenant data root enrolls.
6. **Duplicate `resource.id` within one list.** Kwiry rejects a list that names the same `resource.id` under two holdings, reading DECISION-230 as giving every re-enrolment a new ID. Please confirm the authority never emits that.
7. **Count-only report updates.** Each accepted report is one rewrite of the governance store (HW §10). Kwiry sends a same-state report with changed counts at most once per obligation per `relist_seconds`. Please confirm that is acceptable, or give a lower bound.
8. **`retired` with `escalated`.** Kwiry treats this combination like `terminating`: purge, report, and let `complete` close it. Please confirm.

### 14.3 Remaining seam asks

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
   - Confirm that lifecycle audit events follow HW §8: no `resource.id`, nonces, digests, tags or JWS bodies.
9. **Release order.**
   - Before Kwiry release unit L (the MRCB), the following are deployed:
     - Holdings **S1**: list, report, lifecycle JWKS, holder enrollment with the engine lease policy, and the obligation-creation reconcile;
     - the HW §9 reference fake authority and golden vectors;
     - the issuer's ability to emit both capability shapes.
   - HW S2 and S5 are not prerequisites for L.
   - The issuer transition (ask 5) happens after release L.
   - Kwiry never ships lifecycle rules that depend on an undeployed endpoint (§12).

## 15. Risks and open questions

**Risks**

- **R1 — ONNX determinism.** The highest technical risk. S6 is a hard gate, with `B = 1` as the fallback; otherwise re-scope with the owner.
- **R2 — Storage transition.** Roughly double the disk until the fast-rollback window closes, and an IG-1 re-witness is required. Pre-custody rollback is supported only inside the window (upgrade confirmation or the first durable tombstone). After that, deployments must pin the MRCB; Kwiry detects violations but cannot prevent them.
- **R3 — Memory.** Two embedders plus the sidecar, measured in S8.
- **R4 — Readiness timing under load (A5)** and the **shared-service residual (A7)**, which may reflect other workloads' data-dependent cost.
- **R5 — Purge latency.** It waits for workers to acknowledge cancellation and is not time-bounded. Refusal is bounded.
- **R6 — Authority dependency (Q4).** Governed serving, lexical included, depends on reaching OpenClast:
  - an outage stops each room within 15 minutes of its last renewal;
  - a restart serves nothing until the first list applies;
  - a holder key that is lost or revoked darkens every room until re-enrollment.

  Nothing is deleted in any of these cases. Release unit L needs Holdings S1 deployed first.
- **R10 — S1 epoch and restore handling.** Until Kwiry consumes the epoch chain, any epoch change, and in S1 any restore-driven `seq` regression, refuses every list and darkens governed search (§14.2 Q4). The epoch chain (HW S2) stays high priority.
- **R11 — Unattributed data blocks completion holder-wide.** One unknown room's withheld data keeps every obligation at this holder short of `complete` until a list classifies it or a human releases it. The release wire shape and the no-obligation reporting path are open (§14.2 Q1–Q2).
- **R12 — Holder state is durable and outside the data root.** Losing it causes trust in the first list plus an alert. An authority restore at the same moment would be missed until HW S2.
- **R7 — Observer not on `main`.** This blocks S9.
- **R8 — Existing IG-1 leaks.** S1a and S1b fix them regardless.
- **R9 — Cross-repository sequencing.** Release unit L and the A2 cutover depend on OpenClast delivery and its security review.

**Open questions**

- **Q1** — Should D2 also change the desktop default?
- **Q2** — Should Kwiry itself omit `score` and `chunk_id` in the governed profile? That would need a hit-schema amendment.
- **Q3** — *Answered by the owner (Q6, 2026-11-17):* crypto-erase is a later slice. Until then, receipts say "confirmed" and the method is `logical_delete`.
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
| Lifetime linearization | §4.3 pin-and-lease under L3; unconditional revocation separate from the doom claim; handle drain | S2 F1 interleaving; idle-reader revoke-then-drain on retirement and GC paths; init-races-revoke; lock tracker |
| Publisher serialization | §4.3 single actor; no waits inside transactions | S2 concurrent-publisher test |
| Obligation purge and reporting | §5.6 state machine, write-ahead outbox, barrier and final scan; §5.7 residual | S3 crash injection at steps 0–7; unlink-then-fsync failure; lost acks (H10); restart mid-purge (H11); data-root loss; holder-state loss (H12) |
| Holdings consumption | §5.3 verification, atomic apply, high-water and classification; §5.4 leases | HW §9 golden vectors and reference fake: H1–H9, H13, H16, H17; monotonic-clock lease; early-relist bound |
| Scheduler fairness | §4.9 bounded accounting turns and stated assumptions | S5 reused-room interleaving test, plus measurements (§11.3) |
| Capacity isolation | §4.4 separate FIFO pools with dedicated executors; permits and threads held until worker exit | S8 controlled stall test (lexical gets a thread and completes within budget); executor-separation assertion |
| Physical-baseline equality | §7 merge and RRF rules over `E` | S7 two-instance tests and the S9 witness rows, including readiness under forbidden load |
| Lexical without the model | §6.1 step 4 applies to semantic and hybrid only | S7/S8 tests with no `semantic-onnx`, semantic disabled, and model init failure |
