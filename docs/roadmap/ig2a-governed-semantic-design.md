# IG-2A design — governed semantic and hybrid retrieval

Status: **design, revision 7, independent review pending.** The room lifecycle is specified normatively by the [§5.8 transition table](#58-lifecycle-transition-table-normative). Nothing here is implemented or approved. This page expands [IG-2](governed-semantic-hybrid.md) and is bound by [`CONTRACT.md`](../../CONTRACT.md), which this design does not edit. The amendments it needs are in [§13](#13-contract-amendment-package-a1a7). Room lifecycle follows the frozen OpenClast Holdings wire contract (§5). What remains to ask of OpenClast is in [§14](#14-openclast-seam-asks).

## Revision history

| Revision | Head | Independent review result |
|---|---|---|
| 1 | `1442e668` | NO-SHIP. Core approach judged sound. |
| 2 | `b70c902d` | NO-SHIP, converging. |
| 3 | `82f53cb4` | NO-SHIP. Closed: timeout vs leases, retirement vs admitted builder, manifests, reuse identity, permanent tombstones, failed-purge startup, effective vs signed set, superseded freshness, A2 issuer/TTL retention, A3, and A4 encoding with desktop branching. |
| 4 | `59280048` | NO-SHIP. Architecture sound; most F1–F16 closed. Open: R4-1 (cached reader blocks doom), R4-2 (held-tuple queries inconsistent), R4-3 (unsent completion unrecoverable), R4-4 (no revoked/purge-pending state), R4-5 (no executor reservation), R4-6 (A7 overclaims), R4-7 (rollback rule contradicts itself). |
| 5 | `a69b1e6e` | Replaced the bespoke custody seam (§5, §14) with Kwiry as a consumer of the owner-approved, frozen Holdings contract. Review: NO-SHIP. Confirmed closed: R4-1, R4-2, R4-4, R4-5, R4-6 and R4-7. R4-3's recovery depends on the reporting fixes F5-1 and F5-2. Open: F5-1 to F5-9, all in the consumer state machine; the architecture is kept. |
| 6 | `0dddc353` | Fixed F5-1 to F5-9 and consumed HW §14 D, E and F. Review: NO-SHIP. Confirmed closed: F5-1, F5-3, F5-5, F5-7 (within its assumptions), F5-8, F5-9. Open: R6-1 to R6-9. |
| 7 | this page | Adds a normative lifecycle transition table (§5.8, 105 rows) that wins over prose, with unlisted transitions refused. Fixes R6-1 to R6-9 and adopts HW §14 G. The architecture and the frozen contract are unchanged. |

§16 lists the properties that are closed in the design but can only be proven by tests.

| Sixth-review item | Resolution | Where |
|---|---|---|
| R6-1 Release targets stay referenced and can change class | Strict check, then a durable reservation (W), then detachment from the withheld map (G), then revocation with `reserved_by`, all in one publisher transaction. Reserved units are never re-mapped. Startup detaches reservations before any list. The drain runs outside the actor. Reclassification before reservation is a mismatch | §5.5; L1, L4–L11, C1, S10, O4, O5 |
| R6-2 Writer rules block rejection recovery | Accepted floors (monotone within an epoch) are separate from a replaceable pending report, with typed `reject` and `invalidate` mutations. Same-state reconciliation and lower re-derivation are allowed after a refusal. Floors move to history on a consumed epoch acceptance, so `complete` can be re-confirmed | §5.2, §5.6; R1, R3, R7, R13, R14 |
| R6-3 A queued `complete` is sent after its proof is false | A transmission guard on every send and retry re-derives the claim from the current snapshot (`complete` after a restart needs a fresh in-process scan). Failure invalidates and re-derives under a new key; the exact sent bytes stay in `uncertain` until resolved | §5.6; R2, R3, R15, T2, T3 |
| R6-4 Intersection release contradicts D | Strict, whole-manifest scope (HW §14 G.5): any change means `mismatch`, nothing deleted, and `release_result: mismatch` | §5.5; L1, L2 |
| R6-5 Accepted epochs become permanent permissions | One armed acceptance, bound to the current baseline and the exact target, consumed or voided once. History is audit only | §5.2, §5.4; H4–H11 |
| R6-6 "Both lost" declared while content survives | The loss proof enumerates the whole data root. Only recognized non-content entries qualify; anything else, including a lone `semantic/semantic.db`, fails closed | §5.2; S4, S5, S16 |
| R6-7 Lease-only renewals abort completion | Separate counters (`lease_ver`, `class_ver`, `res_ver`, `obl_ver`, `inv_ver`, `layout_ver`). Completion reads only `obl_ver[o]`, `res_ver[r]`, `inv_ver` and the epoch | §5.3, §5.6; A4, R1 |
| R6-8 No bounded failed-attempt path at startup | Connect 10 s, each response 30 s, whole list 120 s, under a cancellation token. Expiry discards partial pages, frees the slot and backs off | §5.3; F1, F6 |
| R6-9 Retained bytes exhaust cleanup space | Preallocated reserves for lifecycle writes. Build admission counts retained bytes. Pressure pauses builders and never deletes | §5.5, §4.9; K1–K4 |
| G.1 Status `typ` and ack | Adopted; the status receipt path checks every G.1 claim | §5.6, T4 |
| G.2 `release_result` | Adopted on status reports; included in the status key (v2) | §5.5, §5.6; L2, L6–L8 |
| G.3 `bound_report_digest` | Adopted: `sha256:` over the exact compact JWS | §5.5 |
| G.4 Mandatory `accept-epoch` | Adopted as a one-use transition | §5.4; H5–H11 |
| G.5 Strict release | Adopted | §5.5; L1, L2 |
| G backup rule | Normative: a backup older than the retention window is refused | §5.2; S15; A6 |

| Fifth-review item | Resolution | Where |
|---|---|---|
| F5-1 Retry acks fail verification; convergence acks carry another key | Every attempt's `{jti, sha256(JWS)}` is committed before sending and survives restart. **Replay receipts** must match the key, an attempt digest, the holding, the obligation and the state. **Convergence receipts** (`state_regression`, `already_complete`) bind to holder, holding and obligation, raise the floor, and are never presented as receipts for Kwiry's own claims | §5.2, §5.6 |
| F5-2 Holder-state updates race; clearing the outbox loses the floor | One serialized leaf writer with per-field monotone merges and whole-state commits. Per-obligation floors are raised in the same commit that clears the pending entry, after the receipt is durable | §5.2, §5.6, §4.3 |
| F5-3 Unknown resources can never return to service | Withheld (reversible, objects stay `live`, cache-only eviction) is separate from revoked. Only an obligation, GC of unreferenced active-resource objects, or an executed release revokes | §4.3, §5.3, §5.5 |
| F5-4 Completion can see a half-applied classification | *(Refined in revision 7: separate version counters, R6-7.)* Class, leases, layout, withheld map and unattributed inventory change together in one snapshot. The apply is one publisher transaction, and completion is validated and durably enqueued inside a serialized report transaction that aborts if `class_rev` moved | §5.3, §5.6 |
| F5-5 Migration recovery runs before holder state | Startup: load and validate holder state; inspect migration read-only; narrowing-only actions; bind; first list; then every build, publish or delete action | §4.5, §4.10 |
| F5-6 Reset broadens trust on first use | Trust on first use only when holder state and data are both lost. Corruption or identity mismatch fails closed. State-only loss fails closed until an audited `recover-state` | §5.2, A6 |
| F5-7 Undetected authority rollback | HW §14 F's guard maps `503 authority_epoch_mismatch` to unreachable, with a distinct alert. A supported restore needs an explicit `accept-epoch`. The 60 s claim excludes exactly the F residual (raw restore plus record loss), with an operational restriction | §5.3, §5.4, §10, A6, R10 |
| F5-8 Early relist behind the model and admission gates; jitter beyond 300 s | The trigger moves to gate step 3a, before the model and permit gates. The interval is drawn from `[0.8, 1.0] × relist_seconds` | §5.3, §6.1 |
| F5-9 The idempotency key includes itself | The key is derived from a frozen JCS business body that excludes `jti`, `iat`, `exp` and `idempotency_key`, with domain separation per report type | §5.6 |
| HW §14 D Release instructions | *(Superseded in revision 7 by strict G.5 with reservation, R6-1 and R6-4.)* Manifests are bound by report key and attempt digests; a mismatch deletes nothing; an unknown `kind` is skipped | §5.5 |
| HW §14 E Holder status | Sent on reset, on manifest change, after a release, and daily while non-zero, at most once per 60 s, using the same attempt and ack machinery | §5.5, §5.6 |

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
6. **(P-a, default) Lifecycle authority stays with OpenClast.** Obligations, retirement and completion records live at the authority (§5.1). As the frozen contract requires, Kwiry keeps only refusal and reporting state outside the data root: a high-water mark, grow-only tombstones, per-obligation accepted floors and a write-ahead report outbox. That state never authorizes serving, building or deleting (§5.2). *Override only if the owner wants Kwiry to keep its own record.*
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

**Lock order:** actor serialization → L2 → L3. The holder-state writer (§5.2) is a leaf outside this order: the actor may wait for its durable reply, but it never takes L2 or L3 and never waits on anything except its own I/O. No lock is acquired while holding a later one, and no wait happens while holding L2 or L3.

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

  Only three things revoke an object:
  - a publisher transaction that drops an **obligated or retired** resource from the layout (§5.3, §5.6);
  - an object that **no** generation (layout or withheld) and no build root references any longer (GC, §4.5);
  - a `quarantine_release` **reservation** that targets it (§5.5; §5.8 L1).

  Lapse, unknown and tombstoned-unlisted never revoke. Their objects stay `live` and referenced (F5-3).
- **Cache-only eviction (withholding, F5-3).** When a resource moves to the withheld map, its objects' cells are detached under L3 and their readers dropped outside it, exactly as in revocation, **but the object stays `live`**. A later re-initialization is allowed once the resource is servable again. Withholding therefore frees memory without making the object deletable, and reversal needs no rebuild.
- **Deletable** (`revoked → doomed`, the claim). Under L3, an object may be claimed only if **all** of these hold:
  - its state is `revoked`;
  - the claimant owns the revocation reason: the obligation for its resource, GC, or the release that reserved it (§5.8 O5);
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

**At startup (F5-5).** §5.8 machine M1 (rows S1–S18) is normative. Nothing that lists, classifies, builds, publishes a new object or deletes runs before holder identity and state are loaded and the first list applies. In outline:

1. **Load and validate** the configuration, `holder.json`, the key and `holder-state.json` (S1–S3). Apply the §5.2 loss table, including the full data-root content scan (S4, S5). A fail-closed row stops here, and nothing below runs.
2. **Acquire** the `DataRootLock` and `holder.lock`, start the holder-state writer and the publisher actor, and preallocate the reserves (S6, K1).
3. **Inspect migration state, read-only** (S7, §4.10).
4. **Narrowing-only actions** (S8–S10), which need no list:
   - write `rollback.json` and delete `current.v2.json` if tombstones exist;
   - marker-only roll-forward;
   - one publication that drops tombstoned resources from the layout and **detaches every unit reserved by an outstanding release** from the withheld map, before any list can map it.

   Nothing is built or deleted.
5. **Bind the listener** (S11). Every governed request refuses `resource_unavailable`.
6. **First list**, under F1's deadlines, then apply (S12).
7. **Recovery actions** (S13): the matrix's build, resume, discard and confirmation actions; staging resume only for resources listed active and leased; GC; obligations; releases; status.

The `kwiry index` process follows the same order. It cannot run beside the daemon, because both take the `DataRootLock`.

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

**Disk admission (R6-9).** A unit is admitted only if §5.8 row K3 passes. Retained bytes, withheld, quarantined and staging alike, count against `data_root_max_bytes` and free space, together with the lifecycle reserves. Under pressure the scheduler pauses, and it never deletes anything to make room.

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

**Recovery matrix.** The MRCB **inspects** this at startup step 3 (§4.5), after holder state is loaded and before any list. The *Recovery* column is executed in two parts:
- fail-closed refusals and marker-only roll-forward run at inspection;
- every action that builds, discards objects, resumes, publishes a new object or deletes waits for startup step 7, after the first list has classified what is held (F5-5).


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

Kwiry consumes the engine-neutral lifecycle contract that the owner approved and OpenClast froze on 2026-11-17. Kwiry is one **holder** of kind `engine`. It pulls a signed, complete list of what it holds (`Holdings.List`), reports purge progress (`Obligation.Report`), and reports its holder-wide quarantine (`holders/status`). The authority owns every obligation and every release decision. Kwiry keeps only refusal state, and that state can never widen access. Revisions 6 and 7 keep that architecture and the contract, and correct the consumer state machine. **Revision 7 makes it normative: §5.8 is a mechanically checkable transition table. Anything it does not list is refused, and where prose disagrees, the table wins.**

**Binding inputs:**
- **The contract:** *OpenClast Holdings wire contract*, FROZEN 2026-11-17, `.workspace/kwiry-integration/HOLDINGS-WIRE.md` in the OpenClast workspace. It is cited as **HW §n**.
  - HW §13 holds Kwiry's answers and amendments A–C.
  - HW §14 holds amendments 2 and 3: **D** (`quarantine_release` instructions), **E** (holder status report), **F** (raw-restore guard), **G** (status `typ` and ack, `release_result`, the digest encoding, the mandatory holder-side runbook step, strict release scope, the holder-backup rule), and the confirmations of Kwiry's questions Q3–Q8.
  
  This page restates nothing the contract owns. It defines only Kwiry's consumer behaviour.
- **Owner decisions Q1–Q6 (2026-11-17):**
  - Q1: pull a signed list; nudges come later (S5).
  - Q2: one generic Holding record.
  - Q3: the existing governance store.
  - Q4: an engine serve lease of 15 minutes, then quarantine, never deletion.
  - Q5: delete-only credentials, for devices only, not Kwiry.
  - Q6: crypto-erase comes later; until then receipts say `"confirmed"`.

### 5.1 Role, scope and what stays at the authority

- **(b), unchanged and mandatory.** OpenClast mints capabilities only for enrolled, active, random IDs. A retired ID is never minted. Under HW §14 F, a suspected authority rollback also stops minting.
- **Authority-owned (P-a):** holdings, phases, obligations, deadlines, escalation, completion records, release decisions and receipts. An obligation exists at the authority until it is `complete`, whether or not Kwiry remembers it (R4-3).
- **Kwiry-owned:**
  - its holder key;
  - a durable holder-state file (§5.2): high-water, grow-only tombstones, per-obligation floors, the write-ahead report outbox and release progress;
  - quarantine manifests;
  - in-memory leases and classification;
  - the data root.
  
  Tombstones, floors and the high-water only ever **refuse**. Nothing local authorizes serving, building or deleting.
- **Mapping.** A list item's `resource` is `{kind: "room", id: <room_id>}`, where `room_id` is the holder-scoped `random-128-v1` ID Kwiry already uses (HW §3.3). Kwiry's resource tuple `{tenant_id, vault_id, room_id}` comes from operator configuration. The list confirms `room_id` only.
  - Configuration validation refuses two configured tuples with the same `room_id` under one holder. It also refuses tuples with more than one `tenant_id` under one holder, because enrollment is per tenant (HW §14 Q5).
  - A capability's tuple must still match a mapped tuple exactly (§6.1). A mis-bound tenant or vault therefore fails closed; it never widens access.
- **Holder granularity (HW §13):** one holder per data root. One holder has one key, one holder-state file and one high-water mark. Replicas use separate data roots and are separate holders, each with its own obligations. A room counts as purged only when every holder's obligation for it is complete.
- **Slice scope.** Kwiry implements HW S1, including amendments D–G, in release unit L. HW S2 (epoch chain, tombstone merge) is a later Kwiry slice. Until then, any epoch change fails closed, and only a one-use operator acceptance moves past it (§5.4).

### 5.2 Holder identity and durable holder state

**Location.** Holder state lives in a holder directory **next to the operator configuration, never inside the disposable data root** (HW §13). Every file is `0600` and the directory is `0700`. Nothing in it names a resource, except the tombstone set, which the contract requires.

```text
<config-dir>/holder/
  holder.key            # Ed25519 (preferred) or ES256 private JWK; never leaves the host
  holder.json           # { holder_id, lifecycle_base_url, enrolled_jkt, pinned lc-* JWKS,
                        #   retention_window_seconds (default 2592000 = 30 d), initialized_at }
  holder-state.json     # durable, fsynced; below
  sent/<digest>.jws     # exact compact JWS bytes of every send attempt whose acceptance is uncertain
  receipts/<key>.json   # verified acks, refusals and final-scan records, by idempotency key
  manifests/<key>.json  # quarantine manifest bound to a report (§5.5); unit IDs only
  .reserve              # preallocated space for lifecycle metadata (§5.5)
  holder.lock           # flock held by the single holder-state writer (daemon or offline operator command)
```

**Enrollment.**
- `kwiry holder init` creates the key and an empty `holder-state.json`, then prints the public JWK.
- The OpenClast admin enrolls it per tenant with `enroll-holder --kind engine` (HW §2.1) and returns the `holder_id` and the lifecycle JWKS. Kwiry pins both in `holder.json`.
- Key rotation is HW §2.1's overlap-window admin action.
- A lost key cannot sign, so every list fails. Leases then lapse, data is withheld and nothing is deleted until the key is re-enrolled.

**`holder-state.json`** (schema 3) holds exactly the following fields. Each field changes **only** through the typed mutations named here, and each of those is a row of §5.8. A mutation that no row allows is refused.

| Field | Content | Mutations (§5.8 rows) |
|---|---|---|
| `holder_id` | must equal `holder.json` | none (immutable) |
| `committed_at` | wall-clock time of the last commit | every commit; read only by `recover-state` (S15) |
| `high_water` | `{epoch, seq}` of the last applied list, or `null` | `apply_list` (H1, H2, H6, H7) |
| `reset_pending` | `true` after trust on first use until a status report with `holder_state: "reset"` is accepted | set by S4; cleared by T4 |
| `epoch_acceptance` | `null`, or **one** armed acceptance `{from: {epoch, seq}, to: <epoch>, operator, armed_at}` | `arm_epoch` (H5), `consume_epoch` (H6), `void_epoch` (H7, H11) |
| `epoch_history` | audit records of consumed or voided acceptances | append only; **never read by any guard** |
| `tombstones` | `{resource_id, holding_id}` ever seen `terminating` or `retired` (HW §3.4 rule 6) | union only (`apply_list`) |
| `floors` | per obligation: `{epoch, state, receipt_key}`, the highest state the authority **accepted** in the current epoch | raised within an epoch (R5, R6, R13); removed when the obligation leaves the list (R12); moved to `floor_history` on an epoch change (R14) |
| `floor_history` | floors from earlier epochs | append only; audit only |
| `pending` | per obligation: at most one current report, with status `ready`, `sent`, `invalidated`, `rejected` or `halted`; plus its key, canonical business body, `derived_at` and `attempts` | `put_pending` (R1), `append_attempt` (R2), `invalidate` (R3, R14), `reject` (R7, R8), `halt` (R9), `clear` (R5, R6, R12, R13) |
| `uncertain` | per idempotency key: attempts of an **invalidated** report that may have been accepted, kept as evidence until resolved | moved in by `invalidate`; pruned by R15 |
| `status` | the current holder status report (same sub-states as `pending`) and the last accepted one | rows T1–T6 |
| `instructions` | per `instruction_id`: `reserved{units, manifest_digest}`, `draining`, `deleted`, `result_sent`, `done`, `mismatch{observed_manifest_digest}` or `mismatch_reported` | rows L1–L11 |

**One serialized writer.** A single **holder-state writer** task owns every file in the holder directory, and nothing else reads, modifies and writes them.
- **How it works.** For each typed mutation the writer:
  - checks the mutation's guard against its authoritative in-memory copy;
  - writes any side file the row requires (`sent/`, `receipts/`, `manifests/`);
  - writes the **whole merged state** (temporary file, fsync, rename, fsync the directory);
  - only then answers the caller.

  Mutations may be batched into one commit, and none is answered before its commit is durable.
- **Failure.** A failed commit fails the caller's row, and the in-memory copy rolls back to the last durable state.
- **Lock position.** The writer is a **leaf**. It waits only on its own I/O and calls nothing. The publisher actor may wait on it inside a transaction (§4.3).

**Loss, corruption and reset.** The contract accepts trust on first use only when **both** holder state and data are lost (HW §13).

**"Data lost" is proven, not assumed (R6-6).** At startup, Kwiry enumerates the configured data root recursively, without following symbolic links. The root counts as lost only if it is absent, or if every entry falls into one of these **recognized non-content** classes:
- a directory with no file anywhere beneath it;
- `daemon.lock`;
- abandoned storage-probe files, which the lock already removes;
- `.reserve`;
- files under `models/` whose SHA-256 matches the configured embedding identity's artifact list (§4.7).

Any other entry is treated as possible content. That includes every §5.7 residual location (for example a lone `semantic/semantic.db`), any symbolic link, and any unreadable entry. In each of those cases the root is **intact**.

| `holder.json` | `holder-state.json` | Data root | Outcome (§5.8 row) |
|---|---|---|---|
| absent or invalid | any | any | **Fail closed** `not_enrolled` (S2) |
| valid | unreadable, schema-invalid, or `holder_id` mismatch | any | **Fail closed** `holder_state_corrupt` or `holder_identity_mismatch` (S3). The file is left untouched |
| valid | absent | **lost**, as proven above | **Trust on first use** (S4) |
| valid | absent | **intact** | **Fail closed** `holder_state_missing` (S5) |
| valid | valid | any | Normal start (S3) |

**Operator recovery** (`kwiry holder recover-state`, audited, run while the daemon is stopped):
- **`--from-backup <dir>` (S15).** Restores the holder directory from the operator's own backup. Its guards are all mandatory:
  - the `holder_id` matches;
  - the backup's `committed_at` is **newer than `now − retention_window`** (HW §14 G, holder-backup rule). **An older backup is refused, without exception.**
  
  The restored high-water then takes part in the normal epoch rules.
- **`--discard-data-root` (S16).** Deletes the data root's contents. This is an operator action under CONTRACT §1.1 disposability, not a holder deletion. The next start then proves the root lost and takes trust on first use (S4). This is the required path whenever no backup inside the retention window exists.
- **`--set-aside-corrupt` (S17).** Renames a corrupt `holder-state.json` to `holder-state.corrupt-<time>`. Recovery then follows one of the two paths above.

There is no path that applies trust on first use while content may remain.

**Losing only the data root is not a reset.** Tombstones, floors and the high-water survive. Kwiry rebuilds from the next list, and open obligations complete against the empty root (§5.6).

### 5.3 Listing, classifying and applying

**When Kwiry lists:**
- **At startup**, after holder state is loaded (§4.5 phase P5). Every governed request refuses `resource_unavailable` until the first list applies.
- **Periodically.** Each interval is drawn uniformly from `[0.8, 1.0] × relist_seconds` and never exceeds it.
- **Before every rebuild** (`kwiry index`, an embedding-identity change, the upgrade). The list must start after the rebuild was requested.
- **Early, per amendment B.** Fired at gate step 3a (§6.1) when `S` names an unlisted, non-tombstoned `room_id`. It never makes the request wait. At most one is in flight, at most one every 5 s, and triggers coalesce.
- **On a `410 list_expired`**, Kwiry discards every page and relists from `cursor: null` with a fresh nonce. After 5 consecutive `410`s it backs off and raises `lifecycle_list_livelock` (HW §3.7).

**Bounded fetches (R6-8).** Every fetch, the first one at startup included, runs under a cancellation token with three deadlines:
- 10 s to connect;
- 30 s for each response;
- 120 s for the whole list, all pages included.

On expiry, cancellation or shutdown, Kwiry closes the connection, **discards every partial page**, frees the in-flight slot, and enters the retry policy (F6). Retries start at 10 s and back off exponentially, capped at `relist_seconds / 2`, within HW §7's start rate. Leases are unaffected; they simply run down.

**Request.** A holder-signed request object (HW §2.2, §3.1) carries:
- a fresh nonce of at least 128 bits, identical on every page;
- the stored `high_water`;
- `page_size: 500`.

**Verification.** Each page goes through the HW §3.8 checklist in order, failing closed. The whole list must then pass the HW §3.7 consistency rules. Kwiry adds these checks:
- every item's `resource.kind` is `room`;
- no `resource.id` appears twice (HW §14 Q6);
- the item's phase and obligation form a permitted combination (HW §3.3);
- `instructions[]` appears only on page 0 (HW §14 D).

A failure discards the whole list (F4). An instruction with an unknown `kind` is skipped and logged; it never fails the list (L3).

**High-water and epoch.** These rules are §5.8 machine M2, rows H1–H11:
- **Same epoch, `seq ≥ hw.seq`:** accept.
- **Same epoch, lower `seq`:** refuse with `lifecycle_regressed`.
- **Different epoch:** refuse with `unexpected_epoch`, **unless** it is exactly the target of the single armed acceptance **and** the high-water still equals that acceptance's `from` baseline (§5.4).
- **`503 authority_epoch_mismatch`:** treat as unreachable, and raise `authority_rollback_suspected`.

Kwiry never compares `state_digest` values; it only echoes them in `list_ref`.

**Classification.** The list is the holder's complete projection at `rev` (HW §3.4). Every resource Kwiry holds therefore has exactly one class, computed from the list alone (R4-2). "Holds" means the `room_id` appears in any of these:
- configuration;
- the layout or the withheld map;
- an object descriptor;
- a staging journal;
- a quarantine ledger entry;
- a layout-v2 tree;
- a release reservation.

| Class | Condition | Serving | Building | Objects | Counted unattributed |
|---|---|---|---|---|---|
| **active** | listed `active`, configured, not tombstoned | until the lease deadline (§5.4) | allowed while leased | `live`, in the layout | no |
| **lapsed** | as active, but the lease deadline has passed | refused | paused | `live`, in the layout | no |
| **active-unconfigured** | listed `active`, not configured | refused | never | `live`, withheld | no (it is attributed to a listed holding) |
| **obligated** | listed `terminating` (any obligation state), or `retired` with `escalated` (HW §14 Q8) | refused | cancelled | **revoked** | no (attributed to the obligation) |
| **retired-complete** | listed `retired` with `complete` | refused | never | revoked if any remain | no |
| **tombstoned-unlisted** | tombstoned and not listed | refused forever | never | remaining units quarantined (C8) | yes |
| **unknown** | not listed, not tombstoned | refused | never | `live`, withheld | yes |

- **Withheld is reversible; revoked is not (F5-3).** Withheld objects stay `live`, and a list showing them `active` maps them back without a rebuild (C5).
- **Reserved units are never re-mapped.** Units reserved by a release (§5.5) are excluded from any layout or withheld mapping until the release ends (C1, L9).
- **Tombstone conflict.** A tombstoned ID listed `active` raises `tombstone_conflict` and stays refused (C9).

**Snapshot and versions (F5-4, R6-7).** One in-memory **classification snapshot** holds:
- per-resource classes;
- the lease table;
- the layout pointer and the withheld map;
- the release reservations;
- the **unattributed inventory**: unit IDs, count and `quarantine_manifest_digest` (§5.5).

It changes only in one L3 swap at the end of a publisher transaction. It carries **separate monotone version counters**:

| Counter | Bumped when | Who reads it |
|---|---|---|
| `lease_ver` | lease deadlines change and nothing else does | serving only |
| `class_ver[r]` | resource `r` changes class | the builder, and its own resource's paths |
| `res_ver[r]` | any artifact attributed to `r` is published, deleted or moved | the completion guard for `r` |
| `obl_ver[o]` | obligation `o` appears or disappears, changes phase, or changes `last_accepted` | report guards for `o` |
| `inv_ver` | the unattributed inventory or a reservation changes | report and release guards |
| `layout_ver` | a generation is published | the pin path |

A lease-only renewal bumps only `lease_ver`, so it never invalidates a completion or a pending report.

**Apply transaction** (rows A1–A6, executed by the publisher actor after verification, which happens outside it):
1. **A1.** The writer commits `apply_list`: tombstones ∪ the new terminating and retired IDs; the high-water; consuming or voiding an armed epoch acceptance where H6 or H7 applies.
2. **A2.** If A1 added the first tombstone ever: write `rollback.json`, delete `current.v2.json`, fsync both (§4.10).
3. **A3.** Publish the generation record:
   - the layout holds active and lapsed resources;
   - the withheld map holds active-unconfigured and unknown resources;
   - obligated, retired and tombstoned resources are dropped;
   - reserved units are excluded.
4. **A4.** Under L3, swap the snapshot and bump the affected counters.
5. **A5.** Under L3, revoke the objects of every resource dropped as obligated or retired. Withheld resources get cache-only eviction (§4.3).
6. **A6.** Rebuild the obligation, instruction and status work queues from the list. Execution happens outside the actor.

A crash after A1 or A2 leaves the tombstones durable. Startup phase P4 then revokes those resources before any serving (S10).

### 5.4 Serving, leases and authority rollback

A resource `r` in `E` is **servable** only while all of these hold:
- it is configured;
- it is mapped in the layout of the current snapshot, not merely withheld;
- it is not tombstoned;
- it is listed `active` in the last applied list;
- `now` is before its **lease deadline**.

`pin_and_lease` checks this under L3 (§4.3, gate step 6), and the end-of-request check repeats it (§8.3).

**Lease deadline:** the earlier of the item's `quarantine_after`, judged by the wall clock, and `t_recv + (quarantine_after − iat)`, judged by the monotonic clock from when the page verified. HW H17's ±120 s skew applies only to the wall-clock comparison.

**Lapse (Q4).** At the deadline, `r` refuses with `resource_unavailable`, and its A4 freshness becomes `unavailable`. Builds for `r` pause. **Nothing is deleted, moved or revoked** (C2). A fresh list restores serving without a rebuild (C3). If the authority stays unreachable, every room lapses within 15 minutes of its last renewal, lexical serving included (§12, R6).

**Unreachable, refused or suspected-rollback authority.** These responses all get the same treatment: leases run down, nothing is deleted, nothing is reclassified, and retries back off.

| Response | Alert |
|---|---|
| Network failure, a deadline, `429`, `503 lifecycle_unavailable` | `lifecycle_unreachable` |
| `503 authority_epoch_mismatch` | `authority_rollback_suspected` (H10) |
| `401` | `lifecycle_unauthorized` (no retry until the operator acts) |

**Epoch acceptance is a one-use transition (R6-5, HW §14 G.4).** OpenClast's restore runbook makes holder-side acceptance a **required** step.
- **Arming.** `kwiry holder accept-epoch <ep_…>` is audited and runs while the daemon is stopped, taking `holder.lock`. It arms exactly one acceptance, bound to the **current baseline** `{epoch, seq}` and the exact target epoch (H5). Arming replaces any earlier armed acceptance; the replaced one goes to `epoch_history`.
- **Consuming.** The first verified list whose epoch is the target, while the high-water still equals `from`, consumes the acceptance in the same commit that sets the new high-water (H6).
- **Voiding.** A same-epoch list that advances the high-water voids the acceptance as stale (H7), and so does `--cancel` (H11).
- **No permanent permissions.** History is audit only and never consulted. A list from any earlier epoch, accepted before or not, is refused with `unexpected_epoch` (H4, H8).
- **Tombstones are kept and still win.** A room this holder saw retired cannot be revived by a restore.
- **Floors and pending reports.** At consumption, floors move to `floor_history` and pending reports are invalidated (R14). Kwiry may then re-confirm obligations in the new epoch, including re-sending `complete` if the restore lost that acceptance (R6-2).

**Stale-authority window.** OpenClast never mints a capability for a retired ID, and a `terminating` item's `quarantine_after` is already past. So once a list carrying the retirement applies, the room stops at once. Before that, only capabilities minted before the retirement can name it, and those live at most the capability TTL (60 s).

> **The 60 s bound holds unless the authority's history is rolled back without detection.**

| Restore situation | Detection | Effect on Kwiry |
|---|---|---|
| Supported restore | A new epoch (HW §14 Q4, G.4) | Kwiry refuses with `unexpected_epoch` until `accept-epoch`; leases lapse; no stale serving |
| Raw restore with the off-store high-water record intact | `authority_rollback_suspected` (HW §14 F): minting and the search seam stop | Kwiry sees `503 authority_epoch_mismatch` and treats it as unreachable; no stale serving beyond the TTL |
| **Residual until HW S2:** raw restore **and** loss of the off-store record together | Undetected | Example: the holder saw seq 90, missed a retirement at seq 100, and the authority is raw-restored to seq 90. Kwiry accepts the equal `seq`, holds no tombstone, and serves the room; the restored authority can also mint for it. The exposure is unbounded until the retirement is re-applied |

**Operational restriction for the governed profile:**
- the off-store record is never stored or restored with the state backup;
- raw restores are unsupported;
- restores run only through the epoch-minting tooling.

HW S2's signed epoch chain removes the residual. The lease bounds how long Kwiry serves without any authority contact; it is not a revocation bound.

### 5.5 Unknown data, quarantine, releases, status and disk

- **Kwiry never deletes data that is not attributed to a listed obligation or to an executed release that reserved it** (HW §3.4 rule 5). Lease expiry, authority errors, regression, rollback suspicion, reset and **disk pressure** never authorize deletion.
- **Withheld vs. quarantined.**
  - *Withheld* data stays in place, `live`, referenced by the withheld map: unknown and active-unconfigured resources.
  - *Quarantined* data has been revoked, drained, renamed into `quarantine/<random>/` and recorded in `quarantine/ledger.json`. That covers unreadable artifacts, failed deletions, the remaining units of tombstoned-unlisted resources, and garbage GC could not attribute (§4.5).
  
  Neither kind is ever leased, opened for a request, built from or served.
- **Units** carry random IDs and no resource ID: an object ID, a staging build ID, a quarantine entry name, or a `(v2-tree, partition)` pair.
- **Unattributed inventory.** This is the set of units counted "yes" in the §5.3 class table, plus quarantine entries not attributed to a listed obligation, plus **reserved units until they are deleted**. Its size is `quarantined_unattributed` (HW §4.1). While it is non-empty, no `complete` is enqueued or transmitted (R1, R2).
- **`quarantine_manifest_digest` (HW §14 D):** `qm1.` + base64url(SHA-256(`"kwiry-quarantine-manifest/v1"` ‖ 0x00 ‖ JCS(`{"holder_id", "count", "units": [sorted unit IDs]}`))). Before the first transmission of any report carrying it, the writer stores `manifests/<idempotency_key>.json`.
  - **Lookup.** A manifest is found through any of its key's attempt digests (`sent/` and `attempts`) or through its ack's `report_digest`. Under HW §14 G.3, `bound_report_digest` is that same `sha256:` over the exact compact JWS.
  - **Retention.** Manifests are kept for accepted reports of the last 30 days and for any manifest bound by a listed instruction.
- **Releases are strict (HW §14 G.5; R6-1, R6-4).** §5.8 machine M7 is normative. In outline:
  1. **Bind and check (L1, L2).** This happens inside one publisher transaction, reading `inv_ver`. The release proceeds only if all of these hold:
     - a manifest exists for `bound_report_digest`;
     - its count equals `approved_count`;
     - **every** manifest unit is still in the current unattributed inventory, unreserved, with unchanged attribution.

     Otherwise nothing is reserved or deleted. The instruction becomes `mismatch`, and a status report carries `release_result: {instruction_id, result: "mismatch", observed_manifest_digest}`. There is **no partial or intersection execution**.
  2. **Reserve and detach (L1).** In the same transaction:
     - the writer durably records `instructions[id] = reserved{units}`;
     - a generation is published with those units **removed from the withheld map**;
     - under L3 the units are revoked with `reserved_by = id`.
     
     From then on no list can promote, re-map or reuse them (C1). Startup honours the reservation before any list is applied (S10). Reclassification *after* reservation does not undo it, because the human approved exactly these bytes. A room re-listed active is rebuilt from its source files.
  3. **Drain and delete (L4, L5), outside the actor.** Kwiry waits for each unit's lease, handle and init counts to reach zero, then claims, unlinks and fsyncs. Only the reserving release may claim a reserved unit. A failed unlink is retried with backoff, and the instruction stays `draining` until every unit is gone.
  4. **Report (L6, L7).** Remove the deleted units from the inventory (an `inv_ver` transaction). Then send a status report carrying `release_result: executed` and the new count and digest. The instruction becomes `done` when that report is accepted. Under HW §14 G.2, the authority retires it at the same moment.
- **Holder status reports (HW §14 E, G.1, G.2).** The request `typ` is `openclast-holder-status+jwt`, sent to `POST /api/lifecycle/v1/holders/status`, carrying:
  - `list_ref`, `idempotency_key`, `quarantined_unattributed`, `quarantine_manifest_digest`, `holder_state`;
  - and optionally `release_result`.

  Triggers (T1):
  - `reset_pending`;
  - an inventory digest change;
  - a release outcome;
  - at least daily while the count is non-zero.

  Sends are coalesced to at most one per 60 s.
- **Retained bytes and disk pressure (R6-9).**
  - **Reserves.** At startup Kwiry preallocates and fsyncs `.reserve` files: by default 64 MiB in the data root and 8 MiB in the holder directory (K1).
  - **Lifecycle writes.** The following may consume a reserve when the volume is full: holder-state commits, receipts, manifests, rollback markers, apply, withholding, revocation and reservation publications, and quarantine renames with their ledger. On `ENOSPC` they truncate the matching reserve by the needed amount, fsync, retry once, and raise `disk_reserve_used` (K2). Builders never use a reserve.
  - **Build admission** counts every retained byte: layout, withheld, quarantined and staging bytes, plus the estimated build, plus both reserve targets. These must fit within `data_root_max_bytes` and within free space (K3). If they don't, builders pause and Kwiry raises `disk_pressure`. Serving, listing, reporting and obligation purges continue. **Disk pressure never authorizes deleting withheld or quarantined data.**
  - **Replenishment.** The reserves are re-preallocated as soon as space allows (K4).
- **Operator actions are outside the holder.** An operator who deletes the whole data root (CONTRACT §1.1) has not been overridden by Kwiry. Holder state stays, and open obligations complete against the empty root.

### 5.6 Obligations: revoke, purge and report

Kwiry keeps no local history of what it owes. The obligation queue is rebuilt from every applied list, and the tombstones keep those IDs refused forever (R4-3).

**Per obligation:**
0. **Close the fast-rollback window** in A2 when the first tombstone becomes durable (§4.10).
1. **Tombstone** (A1).
2. **Revoke retrieval** (A3–A5; §4.3). Revocation never waits for any drain.
3. **Report `revoked`** (R1) once A1–A5 are durable and in memory.
4. **Drain**, waiting outside every lock:
   - set the admission gate to `closing`;
   - cancel queued units and stop a running unit at its next yield;
   - wait for admitted operations to reach zero, then set the gate to `closed`;
   - wait for each revoked object's counts to reach zero.
5. **Delete** (O5–O7). Claim `revoked → doomed` and delete:
   - the resource's objects and staging;
   - the generation records naming it (through the publisher);
   - its layout-v2 partitions;
   - its attributed quarantine entries.

   Fsync every parent. A failed delete is renamed into quarantine, stays attributed and is retried. Healthy resources and lexical search stay online (P-b).
6. **Barrier and final scan.**
   - **Barrier, run fresh on every pass**, including after a restart: fsync `objects/`, `staging/`, `generations/`, `quarantine/` and the data root.
   - **Scan:** check every §5.7 location and `/proc/self/fd`.
   - **Record:** the final-scan record holds `holder_id`, `obligation_id`, the location names, `found`, `fd_refs`, `barrier_ok`, `scanned_at`, `binary_version`, and the `res_ver[r]` and `inv_ver` it read. It contains no resource ID.
7. **Derive and enqueue (R1).** Inside a publisher transaction, the guard re-reads `obl_ver[o]`, `res_ver[r]`, `inv_ver` and the epoch, **not** `lease_ver` or other resources' classes (R6-7). If any of them moved, the pass repeats steps 6–7. Otherwise the writer stores the final-scan record, the manifest and `pending = ready`.

**State derivation:**

| State | Derived when | Counts and fields |
|---|---|---|
| `revoked` | A1–A5 are applied; drain or deletion may be pending | `units_remaining` = artifacts of `r` not yet deleted |
| `incomplete` | a pass has completed steps 4–6, and either `units_remaining > 0` or the unattributed inventory is non-empty. Remaining units include failed deletions, attributed quarantine entries, and a failed parent fsync or barrier since the last deletion | measured in that pass |
| `complete` | `units_remaining == 0`, the barrier succeeded after the last deletion, the scan found nothing and no fd refers to a deleted path, **and** the unattributed inventory is empty | `method: "logical_delete"`, `completed_at`, `evidence: {kind: "witness-digest", digest: "sha256:" + SHA-256(final-scan record bytes)}` |

Every report carries `counts.quarantined_unattributed` and `quarantine_manifest_digest`.

**Accepted floors and replaceable reports (R6-2).**
- **Floors are monotone within an epoch.** `floors[o]` records the highest state the authority **accepted** in the current epoch. Evidence for it is a replay or convergence ack, or a verified list's `last_accepted`. It only rises, until a consumed epoch acceptance moves it to history (R14).
- **The pending report is replaceable.** It is never a floor. It can be:
  - **sent**, while its proof still holds;
  - **invalidated**, when its proof no longer holds;
  - **rejected**, when the authority definitively refuses it.

  After invalidation or rejection a new report is derived under a new key.
- **The enqueue guard** for a new report is: state > floor, and either no pending report exists, or the pending report is invalidated or rejected, or the new state is strictly higher than the pending state.
- **Same-state reconciliation is explicitly allowed** in two cases:
  - after a rejection or an invalidation;
  - as a counts update at most once per 300 s (HW §14 Q7).
- **Re-deriving a lower state is allowed** after the higher one was invalidated or rejected. Monotonicity is measured against the accepted state, and a refused report counts as never sent (HW §14 Q3). If an uncertain higher report was in fact accepted, the authority answers `409 state_regression` and Kwiry converges (R6).

**Validation at every transmission (R6-3).** Every first send and every retry, including the first retry after a restart, passes a **transmission guard** (R2) that re-derives the claim from the current snapshot:
- the obligation is still listed in the current epoch;
- the pending `list_ref` is in that epoch;
- the state, counts, manifest digest and, for `complete`, the "nothing remains" proof are still true.

For `complete` after a restart, the proof requires a barrier and final scan completed **in this process**. The body itself is not changed, so the key stays stable.

If any check fails, the report is **invalidated** (R3) and re-derived under a new key. Wherever an attempt of the invalidated key may already have been accepted, its exact sent bytes (`sent/<digest>.jws`) and attempt records move to `uncertain`, and stay there until resolved (R15). One of three things resolves them:
- a replay or convergence ack;
- a verified list whose `last_accepted` covers the state;
- a verified list whose `iat` exceeds every remaining attempt's `exp + 120 s + 300 s` while `last_accepted` is still lower. Those attempts can then no longer have been accepted.

**Idempotency keys (F5-9, HW §14 G.2).**
- Obligation reports: `ik_` + base64url(SHA-256(`"kwiry-report-key/v1"` ‖ 0x00 ‖ JCS(B)))[0..43]. `B` is exactly:
  - `iss`, `sub`, `aud`, `htu`;
  - `obligation_id`, `holding_id`, `state`;
  - `list_ref`, `counts`, `quarantine_manifest_digest`;
  - `method`, `completed_at`, `evidence` and `operator` when present.
- Status reports: `"kwiry-status-key/v2"` over:
  - `iss`, `sub`, `aud`, `htu`;
  - `list_ref`, `quarantined_unattributed`, `quarantine_manifest_digest`, `holder_state`;
  - `release_result` when present.

  Version 2 adds `release_result`.

`B` excludes `jti`, `iat`, `exp` and `idempotency_key`. An absent claim is omitted, never `null`. Adding any business claim requires a new version.

**Send attempts and ack verification (F5-1, HW §14 G.1).** Every attempt is a fresh compact JWS. Before sending, the writer stores its exact bytes in `sent/` and appends `{jti, digest, exp}` to the key's attempts (R2).

Every ack must first pass common checks:
- the expected `typ`;
- a pinned `lc-*` `kid` and a valid signature;
- exact `iss`, `aud = openclast-lifecycle:<own holder_id>` and `sub = own holder_id`.

It then takes exactly one path:

| Path | Applies to | Binds to | Effect (§5.8 row) |
|---|---|---|---|
| **Replay receipt** | a `200` for one of Kwiry's keys | `idempotency_key` equals a pending or uncertain key; `report_digest` is in that key's attempts; `holding_id` and `obligation_id` equal the body's; `accepted_state` equals the body's state | Receipt for that key; floor raised; pending cleared if covered (R5) |
| **Convergence receipt** | `409 state_regression` (ack in the body), `200 already_complete` (may carry another key) | `holding_id` and `obligation_id` equal the obligation Kwiry reported; `obligation_state` and `phase` form a valid HW §4.2 pair; no key or digest match is required or claimed | Stored as a convergence receipt, never as a receipt for Kwiry's claims; floor raised (R6) |
| **Status receipt** | a `200` on `/holders/status` | `typ` `openclast-holder-status-ack+jwt`; `idempotency_key` equals the status key; `report_digest` is in its attempts and `report_jti` is that attempt's `jti`; `accepted_quarantined_unattributed` and `accepted_manifest_digest` equal the body's; no `exp` | Status accepted (T4) |

An ack matching no path is rejected with `ack_unverifiable`, and the pending report is unchanged (R17). Every stored obligation receipt must say `receipt_label: "confirmed"`.

**Error handling (HW §4.5, §14).** The §5.8 rows are normative:

| Response | Row |
|---|---|
| `409 state_regression`, `200 already_complete` | R6 |
| `409 complete_refused_unattributed`, `409 stale_list_ref` | R7: rejected, definitively not accepted, because idempotency is checked before these. Relist first for `stale_list_ref` |
| `404 obligation_not_found` | R8 |
| `409 idempotency_conflict` | R9: halt the obligation and alert |
| `401`, `403` | R11 |
| `503 authority_epoch_mismatch` | R10, plus H10 |
| `429`, other `503`s, network failure, deadline | R10: back off |

Report traffic stays within HW §7's 10/s per holder.

**Logs and events.** These carry `holding_id`, `obligation_id`, `instruction_id`, states and counts. A resource appears only as a per-process keyed digest (§10). Kwiry never logs `resource.id`, nonces, `state_digest`, `list_tag`, cursors, JWS bodies, manifest digests or evidence digests (HW §8).

**Tests (S3).** **Every §5.8 row has a conformance test.** Each row that performs durable writes is also crash-injected after each write in its fsync order. The test asserts the row's stated crash outcome and that no unlisted transition occurs. On top of that:
- the HW §9 reference fake and golden vectors, extended for amendments D–G, run in Kwiry CI;
- HW H1–H13, H16 and H17 (H14 and H15 are S2) run with built binaries and synthetic rooms;
- **round-6 scenarios:**
  - a release whose unit is re-listed active before reservation is a mismatch and deletes nothing;
  - the same after reservation is deleted, and the room is rebuilt;
  - `stale_list_ref` leads to a same-state re-derivation;
  - `complete_refused_unattributed` leads to a lower re-derivation;
  - after a restore and `accept-epoch`, `complete` is re-confirmed;
  - `complete` is enqueued at inventory version v, a list withholds X, and the retry is invalidated and re-derived as `incomplete`, with the old bytes kept in `uncertain`;
  - after E1→E2→E3, an E2 list is refused;
  - holder state is missing with only `semantic/semantic.db` present, which fails closed;
  - a final scan runs longer than three lease renewals and still completes;
  - a server stalls on page 1 and the 120 s deadline frees the slot;
  - a full disk with withheld data makes the apply consume the reserve, builders pause, and nothing is deleted.

### 5.7 Allowed residual after `complete`

**Inside the data root: nothing that names or contains the obligated resource.** The final scan covers:

- `current.json` and every generation record, including both layouts and withheld maps;
- every object descriptor and every lexical `manifest.json` shard;
- `staging/`, including build journals;
- `quarantine/` and `quarantine/ledger.json`;
- `upgrade.json`, `current.v2.json` and `rollback.json`;
- every layout-v2 generation tree, including its manifest and partitions;
- the legacy `semantic/` directory, which must be absent in the openclast profile;
- lock and probe files, and `.reserve`;
- the process's open file descriptors.

The same list, plus `models/`, is the scope of the §5.2 "data lost" proof.

**Outside the data root, stated and not covered:**

- **The holder directory (§5.2).** The contract requires it.
  - The tombstone set keeps the retired `room_id` and `holding_id` forever. Both are random 128-bit identifiers with no label.
  - `sent/`, `receipts/` and `manifests/` keep JWS bytes, acks, final-scan records and unit-ID manifests by key or digest. Report bodies name `holding_id` and `obligation_id`, never a resource ID.
- the operator-owned configuration file;
- external log sinks, where resources appear only as per-process keyed digests;
- storage-medium remanence: `logical_delete` is unlink plus fsync, not sanitization (HW amendment A; Q6);
- OpenClast's authority records.

### 5.8 Lifecycle transition table (normative)

This table is the lifecycle contract. **Any state and event pair that no row lists is refused:** nothing is written, the state is unchanged, and Kwiry raises `lifecycle_transition_refused` with the machine and row context. Where prose in §4 or §5 disagrees with a row, **the row wins**, and the prose is a defect to fix.

**Notation for durable writes**, listed in fsync order and separated by `→`:

| Symbol | Meaning |
|---|---|
| **W** | Holder-state writer commit: temporary file, fsync, rename, fsync the holder directory (§5.2) |
| **W[x]** | Holder-directory side file `x`, written the same way, before any later W |
| **G** | Generation publication (§4.2 steps 4–5): record, fsync, rename, fsync `generations/`; then `current.json` and fsync the data root |
| **M** | Rollback markers: write `rollback.json` and fsync; unlink `current.v2.json` and fsync the data root |
| **D** | Unlink, then fsync every parent directory |
| **Q** | Rename into `quarantine/<random>/`, fsync both parents, update `quarantine/ledger.json` and fsync |
| **L3** | An in-memory change under L3, with no durability |
| **—** | No write |

In the *Crash after* column, "restart" means startup machine M1 from S1. *Reads* lists the versions, fields and rows each guard depends on.

#### M1 — Startup phases

States: `P0` start → `P1` holder loaded → `P2` locked → `P3` inspected → `P4` narrowed → `P5` listening → `P6` classified → `P7` running. There is also `FC(reason)`, fail-closed.

| ID | State + event | Guard (reads) | Durable writes | Next | Crash after write | Implements |
|---|---|---|---|---|---|---|
| S1 | P0, process start | Configuration valid (§5.1: unique `room_id`, one `tenant_id` per holder) | — | P0 → S2; otherwise `FC(config_invalid)` | — | §5.1, HW §14 Q5 |
| S2 | P0, load identity | `holder.json` and `holder.key` readable and valid | — | S3/S4/S5; otherwise `FC(not_enrolled)` | — | HW §2.1 |
| S3 | P0, `holder-state.json` present | Parses; schema 3; `holder_id` equals `holder.json` | — | P1. A parse or schema failure gives `FC(holder_state_corrupt)`; a mismatch gives `FC(holder_identity_mismatch)` | — | F5-6 |
| S4 | P0, `holder-state.json` absent | Data root proven lost (§5.2 scan) | W (fresh state: `high_water: null`, `reset_pending: true`) | P1 | Restart takes S3 with `reset_pending` still set, so trust on first use continues | HW §13 |
| S5 | P0, `holder-state.json` absent | Data root intact | — | `FC(holder_state_missing)` | — | R6-6, F5-6 |
| S6 | P1 | `DataRootLock` and `holder.lock` acquired | — (abandoned probes are removed by the lock); then K1 | P2. A lock already held means exit | — | §4.3 |
| S7 | P2 | Read-only read of markers and pointers; choose the §4.10 matrix row | — | P3; a fail-closed row gives `FC(migration)` | — | F5-5 |
| S8 | P3 | `tombstones ≠ ∅` and `rollback.json` absent | M | P3 | Restart redoes S8 | §4.10, R4-7 |
| S9 | P3 | Matrix row `switching` with pointer v3 | `upgrade.json = published` (temporary file, fsync, rename, fsync root) | P3 | Restart redoes S9 | §4.10 |
| S10 | P3 | The v3 layout maps a tombstoned resource, or the withheld map holds a unit reserved in `instructions` | G (without them) → L3 revoke (`reserved_by` for reserved units) | P4 | Restart redoes S10; with the first G durable it is a no-op | §5.3, §5.5, R6-1 |
| S11 | P4 | — | — | P5: listener bound; every governed request refuses `resource_unavailable` | — | §5.3 |
| S12 | P5, a verified list accepted by H1, H2, H6 or H7 | As in that H row | A1–A6 | P6 | As in A1–A3 | F5-5 |
| S13 | P6 | — | — | P7: enables the matrix's recovery actions, staging resume (listed active and leased only), GC, M6, M7 and M8 | — | F5-5 |
| S14 | `FC(*)`, any event except S15–S17, `holder init` or re-enrollment | — | — | Unchanged: no list, serve, build, publish or delete | — | F5-6 |
| S15 | `FC(holder_state_missing)`, operator `recover-state --from-backup` (daemon stopped) | Backup `holder_id` equals `holder.json`; backup `committed_at > now − retention_window` | W[copied side files] → W (`holder-state.json` last) | Restart, then S3 | A partial copy without `holder-state.json` gives S5 again, and the operator reruns | HW §14 G (backup rule) |
| S16 | `FC(holder_state_missing)`, operator `recover-state --discard-data-root` | `holder-state.json` absent | D over the data root's contents | Restart, then S4 (or S5 if anything remains) | Remaining content gives S5, and the operator reruns | CONTRACT §1.1, R6-6 |
| S17 | `FC(holder_state_corrupt` or `holder_identity_mismatch)`, operator `--set-aside-corrupt` | — | Rename to `holder-state.corrupt-<t>`, fsync the directory | Restart, then S4 or S5 | Either name, and the next start classifies it | F5-6 |
| S18 | P5–P7, shutdown | — | — (cancel F-rows; builders stop at a yield; any in-progress W completes) | Exit | — | §4.4 |

#### M2 — Holder epoch and high-water

States: `N` (`high_water: null`), `B(E,s)` (baseline), `A(E,s→E')` (one armed acceptance). The event `L(E_L,s_L)` is a list that passed F3.

| ID | State + event | Guard (reads) | Durable writes | Next | Crash after write | Implements |
|---|---|---|---|---|---|---|
| H1 | N + L | — (first contact, or trust on first use after S4) | A1–A6, with W (`hw = (E_L,s_L)`, tombstones ∪) | B(E_L,s_L) | As in A1 | HW §3.7, §13 |
| H2 | B(E,s) + L, `E_L = E`, `s_L ≥ s` | — | A1–A6, with W (`hw = (E,s_L)`) | B(E,s_L) | As in A1 | HW §3.7 |
| H3 | B or A + L, `E_L = E`, `s_L < s` | — | — | Unchanged; L is never cited; `lifecycle_regressed` | — | HW §3.7, H3 |
| H4 | B(E,s) + L, `E_L ≠ E` | — | — | Unchanged; `unexpected_epoch` | — | HW §5, §14 Q4 |
| H5 | B or A, operator `accept-epoch E'` (daemon stopped) | `E' ≠ E`; `hw ≠ null` | W (`epoch_acceptance = {from: (E,s), to: E'}`; any earlier one goes to `epoch_history`) | A(E,s→E') | Either before or after | HW §14 G.4, R6-5 |
| H6 | A(E,s→E') + L, `E_L = E'` | `hw` equals `from` | A1–A6, with W (`hw = (E',s_L)`; acceptance consumed into `epoch_history`; R14 in the same commit) | B(E',s_L) | Either A(E,s→E') or B(E',s_L) | R6-5, R6-2 |
| H7 | A(E,s→E') + L, `E_L = E`, `s_L ≥ s` | — | A1–A6, with W (`hw = (E,s_L)`; acceptance voided into `epoch_history`) | B(E,s_L); `epoch_acceptance_stale` | As in A1 | R6-5 |
| H8 | A(E,s→E') + L, `E_L ∉ {E, E'}` | — | — | Unchanged; `unexpected_epoch` | — | R6-5 |
| H9 | N, or `E' = E`, operator `accept-epoch` | — | — | Refused (nothing to accept) | — | R6-5 |
| H10 | Any state + `503 authority_epoch_mismatch` on any route | — | — | Unchanged; `authority_rollback_suspected`; leases run down; back off | — | HW §14 F, F5-7 |
| H11 | A, operator `accept-epoch --cancel` (daemon stopped) | — | W (acceptance voided into `epoch_history`) | B(E,s) | Either | R6-5 |

#### M3 — List fetch

States: `Idle`, `InFlight(nonce, pages, deadlines)`.

| ID | State + event | Guard (reads) | Durable writes | Next | Crash after write | Implements |
|---|---|---|---|---|---|---|
| F1 | Idle + trigger: startup, an interval in `[0.8, 1.0] × relist_seconds`, a rebuild, the step-3a early relist, or a backoff timer | Nothing in flight; HW §7 start rate; an early relist needs ≥ 5 s since the last start | — | InFlight, with a fresh nonce and deadlines of 10 s (connect), 30 s (each response) and 120 s (whole list) | — | HW §3.1, §7, amendment B, R6-8 |
| F2 | InFlight + a page that passes HW §3.8, with more pages to come | Same `list_id`, `rev`, nonce, digest and tag as page 0 | — | InFlight | — | HW §3.7, §3.8 |
| F3 | InFlight + last page verified | HW §3.7 consistency; Kwiry's item checks; `instructions[]` only on page 0 | — | Idle; L goes to M2 | — | HW §3.7, §14 D |
| F4 | InFlight + any verification failure | — | — | Idle; all pages discarded; `lifecycle_list_invalid`; back off | — | HW §3.8 |
| F5 | InFlight + `410 list_expired` | Fewer than 5 consecutive | — | InFlight with a fresh nonce from `cursor: null`. At 5: Idle, back off, `lifecycle_list_livelock` | — | HW §3.7 |
| F6 | InFlight + deadline expiry, or cancellation | — | — | Idle; connection closed; partial pages discarded; slot freed; back off | — | R6-8 |
| F7 | InFlight + network failure, `429`, or a `503` other than H10 | — | — | Idle; back off (from 10 s, capped at `relist_seconds / 2`) | — | HW §4.5 |
| F8 | InFlight + `401` or `403` | — | — | Idle; `lifecycle_unauthorized`, or C11 for `403` | — | HW §4.5 |
| F9 | InFlight + another trigger | — | — | Unchanged (coalesced) | — | Amendment B |

#### M4a — Apply transaction (inside the publisher actor)

| ID | Step | Guard (reads) | Durable writes | Next | Crash after write | Implements |
|---|---|---|---|---|---|---|
| A1 | Commit the list | The H row admitted it | W (`apply_list`: tombstones ∪; `hw`; the H6/H7 acceptance change; R14 where H6) | A2 | Restart: S8 and S10 enforce the tombstones; leases are not persisted, so nothing is served until the next list | HW §3.4 rule 6, §3.7 |
| A2 | First tombstone ever | `rollback.json` absent | M | A3 | Restart: S8 | R4-7 |
| A3 | Publish the classification | `instructions` reservations (from W) | G (layout = active and lapsed; withheld = unknown and active-unconfigured; obligated, retired and tombstoned dropped; reserved units excluded) | A4 | Restart: the generation is durable; S10 is consistent | §5.3, R6-1 |
| A4 | Swap the snapshot | — | L3 (pointer, leases, classes, inventory, reservations; bump `lease_ver`, `class_ver[r]`, `res_ver[r]`, `obl_ver[o]`, `inv_ver`, `layout_ver` as affected) | A5 | — | F5-4, R6-7 |
| A5 | Revoke | — | L3 (revoke the objects of obligated and retired resources, O4; cache-only eviction for withheld ones, O3) | A6 | — | §4.3 |
| A6 | Queue work | — | — | Done; M6, M7 and M8 queues rebuilt from L | — | §5.6 |

#### M4b — Resource class

Writes happen through A1–A6 unless stated. "Configured" and "tombstoned" are read from the configuration and from W.

| ID | State + event | Guard (reads) | Durable writes | Next | Crash after write | Implements |
|---|---|---|---|---|---|---|
| C1 | absent, unknown, active-unconfigured, lapsed or active + L lists `r` `active` | Configured; not tombstoned; units reserved by an outstanding release are excluded | A3 maps `r`'s live, unreserved objects into the layout. If there are none, `r` is active and unbuilt, and the builder may build it under K3 | active | As in A1–A3 | HW §3.4 rule 2, R6-1 |
| C2 | active + lease deadline passes | `lease_ver` timer | — (L3) | lapsed | — | Q4, HW §3.3 |
| C3 | lapsed + L `active` with a later `quarantine_after` | — | A1 (hw only) → L3 | active | — | Q4 |
| C4 | active, lapsed or active-unconfigured + L does not list `r` | Not tombstoned | A3 (layout → withheld) → A5 cache-only eviction | unknown | As in A3 | HW §3.4 rule 1, amendment C |
| C5 | unknown + L `active` | Configured; units not reserved | A3 (withheld → layout; objects still live) | active | As in A3 | F5-3 |
| C6 | any non-tombstoned + L `terminating`, or `retired` with `escalated` | — | A1 (tombstone) → A3 (drop) → A5 (revoke) | obligated | Restart: S10 | HW §3.4 rule 3, §14 Q8 |
| C7 | any + L `retired` with `complete` | — | A1 → A3 → A5, if objects remain | retired-complete | Restart: S10 | HW §3.4 rule 4 |
| C8 | obligated or retired-complete + L no longer lists `r` | Remaining units of `r` | After drain: Q for each remaining unit (unattributed) | tombstoned-unlisted | Restart: a quarantine scan rebuilds missing ledger entries as unattributed | HW §3.3 retention |
| C9 | tombstoned (any class) + L `active` | — | — | Unchanged; `tombstone_conflict` | — | HW H13 |
| C10 | active or lapsed + L `active`, but `r` is unconfigured | — | A3 (layout → withheld) → A5 cache-only eviction | active-unconfigured | As in A3 | §5.3 |
| C11 | any + `403 holder_not_authorized` | — | — (L3: nothing servable) | Unchanged classes, all refused until a list succeeds | — | HW §4.5 |
| C12 | obligated + L still `terminating` or `retired` | — | — | obligated | — | HW §3.4 |

#### M5 — Object lifetime

States: `live`, `revoked`, `doomed`, `deleted`, `quarantined`.

| ID | State + event | Guard (reads) | Durable writes | Next | Crash after write | Implements |
|---|---|---|---|---|---|---|
| O1 | (none) + build commit | Build root; admission token; K3 | §4.2 steps 1–3 (`sync_tree`, rename, fsync both parents) | live (build-root reference) | Unreferenced, so it becomes garbage (O4b or O8) | §4.2 |
| O2 | live + publication referencing it | Publisher revalidation (§4.3) | G | live (layout or withheld reference) | — | §4.3 |
| O3 | live + withholding (C4, C10) | — | G → L3 cache-only eviction | live | — | F5-3 |
| O4 | live + revoke reason: (a) obligated or retired drop (A5); (b) unreferenced and attributed to an active resource (GC); (c) release reservation (L1) | The reason is already durable: W tombstone, G, or W reservation | L3 (`revoked`, cell detached, reader dropped outside L3) | revoked | Restart re-derives `revoked` from the durable reason (S10, GC scan) | §4.3, R4-1 |
| O5 | revoked + claim | Lease, handle and init counts all zero; no layout, withheld, retained or build-root reference; the claimant owns the reason (obligation for `r`, GC, or the release that reserved it) | — (L3) | doomed | — | §4.3, R6-1 |
| O6 | doomed + delete | — | D | deleted | A partial tree remains with its reason still durable, so O4–O6 repeat | §5.6 step 5 |
| O7 | doomed + delete failure | — | Q (attributed to the claimant's reason) | quarantined (attributed) | Restart: the quarantine scan rebuilds the ledger entry from the descriptor, otherwise as unattributed | P-b |
| O8 | live, unreferenced, attributed to an unknown, tombstoned-unlisted or unattributable resource | — | L3 revoke → drain → Q | quarantined (unattributed) | As in O7 | §4.5, amendment C |
| O9 | revoked or doomed + lease or reader-init request | — | — | Refused (pin retries, then `index_building`) | — | F1, R4-1 |
| O10 | live (withheld only) + lease request | Resource not servable | — | Refused (`resource_unavailable`) | — | §5.4 |

#### M6 — Obligation report (per obligation `o`)

The floor is `F = floors[o]` in the current epoch. Pending states: `none`, `ready`, `sent`, `invalidated`, `rejected`, `halted`. The transmission guard and the enqueue guard are defined in §5.6.

| ID | State + event | Guard (reads) | Durable writes | Next | Crash after write | Implements |
|---|---|---|---|---|---|---|
| R1 | none, invalidated or rejected (or ready/sent with a lower state) + a derived state `s` | Publisher transaction: `obl_ver[o]`, `res_ver[r]`, `inv_ver` and the epoch unchanged since the pass; `s > F`; the enqueue guard; for `complete`, an empty inventory; a same-state re-derivation at most once per 300 s | W[`receipts/<key>` scan record, for `complete`] → W[`manifests/<key>`] → W (`pending = ready{key, B}`) | ready | After a side file: an orphan, later pruned. After W: ready, re-validated by R2 after the restart | HW §4.1, §4.4, §14 Q3, Q7; F5-4; R6-7 |
| R2 | ready or sent + transmission | The transmission guard passes against the current snapshot. For `complete` after a restart, a barrier and scan completed in this process | W[`sent/<d>.jws`] → W (append `{jti, d, exp}`) → send | sent | After W: the attempt counts as possibly accepted | F5-1, R6-3 |
| R3 | ready or sent + transmission | The transmission guard fails | W (`pending → invalidated`; any attempts and their `sent/` bytes move to `uncertain[key]`) | invalidated, then R1 | Either before or after | R6-3 |
| R4 | ready or sent, restart | — | — | Unchanged; only R2 or R3 may follow, and only from P6 | — | F5-5 |
| R5 | sent + replay receipt | Replay path (§5.6) | W[`receipts/<key>` ack] → W (`F = max(F, s)`; clear pending if `F ≥ pending.state`; prune the key's `sent/` and attempts) | none, or the remaining pending | After the receipt: the next retry gets the same original ack, so R5 again | HW §4.3, §4.4 |
| R6 | sent or invalidated + convergence receipt (`409 state_regression` body, `200 already_complete`) | Convergence path (§5.6) | W[`receipts/conv-<ack jti>`] → W (`F = max(F, accepted_state)`; clear pending and uncertain entries that are covered) | none, or pending | As in R5 | HW §4.4 |
| R7 | sent + `409 complete_refused_unattributed` or `409 stale_list_ref` | Definitively not accepted: HW §4.1 checks idempotency before `list_ref` and before `complete` preconditions | W[`receipts/<key>` refusal] → W (`pending → rejected`; prune the key's `sent/`) | rejected, then R1 at any state above F. For `stale_list_ref`, F1 runs first | Either | HW §14 Q3, §4.5, R6-2 |
| R8 | sent + `404 obligation_not_found` | — | W (`pending → rejected`) | rejected; F1. If `o` is then absent, R12 | Either | HW §4.5 |
| R9 | sent + `409 idempotency_conflict` | — | W (`pending → halted`) | halted; alert | Either | HW §4.5 |
| R10 | sent + `429`, a `503`, network failure or deadline | — | — | Unchanged; back off; `authority_epoch_mismatch` also H10 | — | HW §4.5, §14 F |
| R11 | sent + `401` or `403` | — | — | Every report halted until the operator acts; `403` also C11 | — | HW §4.5 |
| R12 | any + a verified list without `o` | — | W (pending archived to `receipts/`; `floors[o]` removed; uncertain kept for R15) | none (ended) | Either | HW §3.3 retention |
| R13 | any + a verified list in which `o.last_accepted = a` and `a > F` | Same epoch | W (`F = a`, receipt `list:<list_id>`; clear pending if covered) | — | Either | HW §3.3 |
| R14 | any + H6 (epoch acceptance consumed) | — | Inside H6's W: floors go to `floor_history`; pending and status are invalidated (attempts go to `uncertain`) | none (re-confirmation allowed) | As in H6 | R6-2, R6-5 |
| R15 | `uncertain[k]` + a verified list | Either `last_accepted ≥ k.state`, or `list.iat > max(attempt.exp) + 420 s` with `last_accepted < k.state` | W (prune `uncertain[k]`) → D (`sent/` bytes) | resolved | Leftover `sent/` files pruned later | R6-3 |
| R16 | invalidated or rejected + same-state re-derivation | Less than 300 s since the last same-state report | — | Wait | — | HW §14 Q7 |
| R17 | sent + an ack with a valid signature that matches no path | — | — | Unchanged; `ack_unverifiable`; after 3 in a row, halted | — | F5-1 |

#### M7 — Release instruction (per `instruction_id`)

States: `unseen`, `reserved`, `draining`, `deleted`, `result_sent`, `done`, `mismatch`, `mismatch_reported`.

| ID | State + event | Guard (reads) | Durable writes | Next | Crash after write | Implements |
|---|---|---|---|---|---|---|
| L1 | unseen + a `quarantine_release` entry on page 0 | Publisher transaction reading `inv_ver`: `issued_seq ≤ rev.seq`; a manifest is retained for `bound_report_digest`; its count equals `approved_count`; **every** manifest unit is in the current unattributed inventory, unreserved, with unchanged attribution | W (`instructions[id] = reserved{units, digest}`) → G (withheld map without the units) → L3 (revoke, `reserved_by = id`; bump `inv_ver`) | reserved | After W: S10 detaches the units before any list applies. After G: complete | HW §14 D, G.5, R6-1 |
| L2 | unseen + the same entry | Any L1 guard fails | W (`instructions[id] = mismatch{observed: current digest}`) | mismatch, then T1 with `release_result: mismatch` | Either | HW §14 G.2, G.5, R6-4 |
| L3 | unseen + an entry of unknown `kind` | — | — | Ignored and logged | — | HW §14 D |
| L4 | reserved + start drain (outside the actor) | — | W (`draining`) | draining | Either | R6-1 |
| L5 | draining + all of a unit's counts zero | Claimant is `id` | L3 doom → D for each unit; when all are gone, W (`deleted`) | deleted | Mid-way: S10 keeps the reservation and L5 resumes; units already deleted are absent | HW §14 G.5 |
| L5f | draining + a unit's delete fails | — | — (the unit stays revoked and reserved; retried with backoff) | draining | — | P-b |
| L6 | deleted | Publisher transaction | L3 (remove the units from the inventory, bump `inv_ver`) → T1 with `release_result: executed` | result_sent | Restart: the inventory is rebuilt from disk, and T1 is re-derived | HW §14 G.2 |
| L7 | result_sent + T4 for that result | — | Inside T4's W: `done` | done | As in T4 | HW §14 G.2 |
| L8 | mismatch + T4 for the mismatch result | — | Inside T4's W: `mismatch_reported` | mismatch_reported | As in T4 | HW §14 G.2 |
| L9 | reserved or draining + L reclassifies a reserved unit's resource | — | — | Unchanged; reserved units are never re-mapped (C1) | — | R6-1 |
| L10 | done or mismatch_reported + still listed | — | — | Unchanged; T1 only if no accepted status report carries the result | — | HW §14 G.2 |
| L11 | reserved, draining or deleted + no longer listed | — | — | Continue to done | — | HW §14 G.5 |

#### M8 — Holder status report

| ID | State + event | Guard (reads) | Durable writes | Next | Crash after write | Implements |
|---|---|---|---|---|---|---|
| T1 | none, invalidated or rejected + a trigger (`reset_pending`, a digest change, a release outcome, daily while non-zero) | At least 60 s since the last send; publisher transaction reading `inv_ver` | W[`manifests/<key>`] → W (`status = ready{key, body}`) | ready | Either | HW §14 E, G.2 |
| T2 | ready or sent + transmission | Re-derived count, digest, `holder_state` and `release_result` all equal the body | W[`sent/<d>.jws`] → W (attempt) → send | sent | As in R2 | HW §14 G.1, R6-3 |
| T3 | ready or sent + transmission | The guard fails | W (`invalidated`; attempts go to `uncertain`) | invalidated, then T1 | Either | R6-3 |
| T4 | sent + status receipt | Status path (§5.6) | W[`receipts/<key>`] → W (status accepted; `reset_pending = false` if the body said reset; L7 or L8 in the same commit) | none | After the receipt: the retry gets the same ack | HW §14 G.1 |
| T5 | sent + `429`, a `503`, network failure or deadline | — | — | Back off; `authority_epoch_mismatch` also H10 | — | HW §14 E, F |
| T6 | sent + another `4xx` | — | W (`rejected`) | rejected, then T1 | Either | HW §4.5 |

#### M9 — Disk reserve and admission

| ID | State + event | Guard (reads) | Durable writes | Next | Crash after write | Implements |
|---|---|---|---|---|---|---|
| K1 | P1 → P2 | Free space ≥ the reserve targets | Preallocate `.reserve` in the data root and holder directory, fsync each | Reserves present; otherwise `disk_reserve_missing` | Recreated next start | R6-9 |
| K2 | A lifecycle write (W, W[x], G for A3/L1/S10, M, Q) gets `ENOSPC` | The matching reserve has enough bytes | Truncate the reserve by the needed amount, fsync → retry the write once | Write retried; `disk_reserve_used` | Smaller reserve; K4 refills it | R6-9 |
| K3 | A builder requests admission | Layout + withheld + quarantined + staging bytes + estimate + reserves ≤ `data_root_max_bytes`, and ≤ free space | — | Admitted; otherwise paused with `disk_pressure`. **Never** a deletion | — | R6-9 |
| K4 | Space available and a reserve below target | — | Preallocate, fsync | Reserves full | Retried | R6-9 |

**Size:** M1 18 rows, M2 11, M3 9, M4a 6, M4b 12, M5 10, M6 17, M7 12, M8 6, M9 4: **105 rows** in total.

## 6. Readiness, errors and HTTP mapping

### 6.1 Effective set and gate order

The **signed set** `S` is the capability's resources. The **effective set** is `E = S ∩ {vault_id, room filters}`. `E` alone drives the gates, leases, generation digest and freshness. The readiness probe uses `S`. If `E` is empty, the response is `200` with no hits, a digest over the empty set and freshness `current`; nothing is embedded or leased.

1. **Verify the capability.** The verified context keeps `exp`, the exact action, `jti`, resources and `max_limit` (A2). The action must match the route and the mode (§7.5).
2. **Require `mode`** in the openclast profile. The D2 default is applied at the seam.
3. **Validate** the query, controls and filters. Non-room filters are refused here for semantic and hybrid, before any embedding or open.
   - **3a. Early-relist trigger (F5-8).** If `S` names a `room_id` absent from the current list and not tombstoned, schedule the bounded early relist (§5.3). This never waits, and it fires before the model gate and permit admission, so a request refused there still triggers it.
4. **Semantic and hybrid only (F6):** the model must be loaded with the expected `embedding_identity`, otherwise `mode_unavailable`. **Lexical never consults the model.**
5. **Acquire a permit** from the mode's pool (§4.4).
6. **The worker calls `pin_and_lease(E, class)`** (§4.3). Every resource in `E` must be mapped and servable: listed active, within its lease and not tombstoned (§5.4). Otherwise the call refuses with `resource_unavailable`. The early relist was already triggered at step 3a.
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

**Bounded stale-authority window (§5.4).** An already-issued capability for a just-retired room can be served until it expires: at most the 60 s TTL, before the list carrying the retirement applies.
- **Exception: an undetected authority rollback.** That is a raw restore *together with* loss of the authority's off-store high-water record (HW §14 F), and it remains a residual until HW S2. In that case a retirement the holder never saw can disappear, and exposure is unbounded.
- **Detected rollbacks fail closed:** a supported restore mints a new epoch, and a raw restore with an intact record makes the authority refuse with `authority_epoch_mismatch`.
- **Operational restriction:** the off-store record is never stored or restored with the state backup, and raw restores are unsupported.

The OpenClast post-check is the fail-closed control for users. The 15-minute lease bounds how long Kwiry serves without any authority contact; it is not a revocation bound.

| Channel | Response |
|---|---|
| Readiness timing | Accepted under A5. Fair rotation; the probe returns enums only; advisory witness. |
| Service load | Disclosed under A7, which may reflect other workloads' data-dependent cost. Separate lexical and semantic pools, each with FIFO admission and a dedicated executor. |
| Query embedder | A dedicated instance is required before HTTP. It is necessary but not sufficient. Still shared: CPU and SMT siblings, memory bandwidth and last-level cache, page cache and disk I/O, the blocking pool, the allocator, and the query-model queue. These are documented residuals. Builders run at lower OS priority. |
| Lifetime and retirement | Linearized pin-and-lease, unconditional revocation separate from the doom claim, a single publisher, admission gates and handle drain. Retiring B never breaks A. |
| Holdings list, reports and status | Server-to-server, holder-signed. Lists are triggered by startup, a jittered interval of at most `relist_seconds`, rebuilds and capability arrival (gate step 3a; one in flight, at most one per 5 s). Status reports go out at most once per 60 s. Users never see a list or receipt, and an early relist never makes a request wait. |
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
  - corrupt or mismatched holder state, or missing holder state with intact data, stops governed serving until the operator recovers;
  - after a supported OpenClast restore (new epoch), governed serving stops until the operator accepts the new epoch on each Kwiry holder;
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
| **S3 — Holdings consumer, purge, reporting (lexical)** | §5 in full: holder key; serialized holder-state writer and the loss table; list, verify and the atomic apply transaction with `class_rev`; high-water, `unexpected_epoch`, `accept-epoch` and `authority_epoch_mismatch`; leases; withheld vs. revoked; quarantine manifests; releases (HW §14 D); holder status reports (HW §14 E); obligation state machine with validated enqueue, attempts, floors and both ack paths; `rejectLifecycleArtefact` in Kwiry's capability verifier (HW §2.5). **Implemented to the §5.8 transition table**, with an explicit refusal for unlisted transitions | `governed-lifecycle-v3`; CI needs the HW §9 reference fake authority and golden vectors, extended for D–G | **one conformance test per §5.8 row (105), each crash-injected after every durable write**; HW H1–H13, H16, H17; the §5.6 round-6 scenarios; purge witness over §5.7 |
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
  - OpenClast Holdings **S1**, including amendments D, E and F, deployed **before** that release: list (with `instructions[]`), report, holder status, lifecycle JWKS, holder enrollment and lease policy, the off-store high-water guard, and the epoch-minting restore tooling (§14 ask 9);
  - the HW §9 reference fake and golden vectors for CI;
  - answers to the open §14.2 questions marked *blocking L*.
  
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
- lifecycle events (counts and holding or obligation IDs only, never `resource.id`): `list_applied` (rev seq, item counts by class), `list_rejected` (reason code), `lease_lapsed`, `object_revoked`, `object_doomed`, `report_sent` and `report_acked` (state, receipt path), `status_sent`, `release_executed` and `release_mismatch` (`instruction_id`, counts), `holder_state_reset`, `authority_rollback_suspected`.

### 12.2 Witness mapping

The mapping is by theme until OpenClast shares its V1–V16 numbers.

| Theme | Kwiry evidence | Extends |
|---|---|---|
| Two-user vector and hybrid | Asymmetric requests; leases and opens only within `E` | W2, W8 |
| Forbidden-partition invariance | Byte-equal results against an authorized-only instance under forbidden add, remove and mutate | W5 |
| **Readiness under forbidden load (owner decision)** | Instance 1: pending room A plus a large forbidden resource that sorts before A. Instance 2: only A. **Assert** exact equality once A is ready on both. **Record** time-to-ready as advisory only. | new |
| Mid-request retirement, timeout, disconnect | Forbidden room B retired at each barrier while A's request succeeds exactly. A's own room retired → `resource_unavailable`. Timeout and disconnect → bounded refusal, leases released on worker acknowledgement. | W8, W13a |
| Retirement purge | The fake authority lists B `terminating`. Kwiry tombstones B, revokes it, and reports `revoked` before drain. The data root is then inspected at **every §5.7 location: generation records, layouts and withheld maps, `current.json`, object descriptors, lexical manifest shards, staging journals, quarantine and its ledger, upgrade and rollback records, layout-v2 trees and manifests, legacy `semantic/`, and open file descriptors.** No artifact names or contains B. Every remaining object's resource is listed active or withheld. `complete` is sent only after the barrier and a clean scan, with `method: logical_delete` and the evidence digest. Crash-injected and deletion-failure runs report `incomplete`, then `complete`, and keep healthy rooms and lexical served. *(Ownership and artifacts are checked, not chunk IDs, which omit the room.)* | W13a–W13f |
| Holdings consumer | §5.8 row-conformance suite with crash injection; HW H1–H13, H16 and H17 against the reference fake, plus the revision-6 and revision-7 tests (§5.6): ack verification after a restart, writer races, withheld reversal, validated completion, startup order, the loss table, epoch acceptance, `authority_epoch_mismatch`, releases and status reports. Unknown rooms withheld, never deleted. `quarantined_unattributed > 0` blocks `complete`. Regression refused. `unexpected_epoch` refused. Authority unreachable → every room lapses within 15 minutes with no bytes deleted. A capability for an unlisted room → one early relist and an immediate `resource_unavailable`. Holder state and data both wiped → `holder_state_reset` and trust on first use. Holder state alone missing or corrupt → fail closed. Data root wiped → the open obligation completes against the empty root. Stale-authority window bounded by capability TTL. | new |
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
  - Kwiry never applies a list with a lower `seq` in the same epoch. Until it consumes the epoch chain, it applies a list from a different epoch only through a **one-use** operator acceptance. That acceptance is bound to the current baseline and the exact target epoch, and earlier acceptances grant nothing. Tombstones still win.
  - `503 authority_epoch_mismatch` is treated like an unreachable authority: leases run down, nothing is deleted, and Kwiry raises a distinct alert.
- **Delete only under an obligation or a human-authorized release.**
  - Lease expiry, unknown data, authority errors, regression, rollback suspicion and holder-state reset never authorize deletion.
  - Unknown data is withheld (reversible, objects stay live) or quarantined, and counted.
  - A release executes only if the current unattributed set still contains the **entire** bound manifest (HW §14 G.5). Its targets are durably reserved and detached before deletion. Otherwise nothing is deleted, and the mismatch is reported.
  - Kwiry reports its holder-wide unattributed count and manifest digest in every report and in holder status reports.
- **Three reported states,** each sent with idempotent, signed `Obligation.Report` claims and never regressing:
  - **revoked:** no new pin can use the resource, its cached readers are evicted, and in-flight requests are refused at their end-of-request check. Sent before any drain.
  - **incomplete:** retrieval is revoked, but artifacts remain on disk or in quarantine, or holder-wide unattributed data exists.
  - **complete:** every data-root artifact of the resource is unlinked, every parent directory is durably synced in a barrier after the last deletion, no lease or reader handle remains, a final scan is clean, and holder-wide unattributed data is zero in the same classification revision. That revision is validated when the report is durably enqueued.
- **Receipts.**
  - The method is `logical_delete` (unlink plus fsync, not sanitization). Kwiry retries until it receives a signed ack labelled `"confirmed"` (Q6).
  - Every send attempt is durably recorded before it is sent, so the authority's original ack verifies after a restart.
  - Convergence acks raise Kwiry's floor but are never presented as receipts for Kwiry's own claims.
  - Failed artifacts are quarantined, only the obligated resource is affected, and the report says `incomplete`.
- **Clarification of §1.1.** The data root stays disposable: deleting it never widens access, and open obligations complete against the empty root. **Holder state is not index data.** The holder key, high-water mark, grow-only tombstones, floors, outbox and release progress live outside the data root. They are written by one serialized writer with monotone merges, and they can only refuse.
  - Trust on first use applies **only** when holder state and data are both lost, and it raises `holder_state_reset`.
  - Corrupt or mismatched holder state fails closed.
  - Missing holder state with possibly intact data fails closed until an audited operator recovery runs. "Data lost" is proven by a full content scan.
  - **Holder state older than the retention window (30 days) is never restored.** The discard-and-rebuild path is used instead (HW §14 G).
  
  Rebuild converges given the same files and the same list.
- **Rollback.** Pre-custody rollback is supported only inside the fast-rollback window. The window closes at upgrade confirmation or the first durable tombstone. While the old binary runs, A6 is suspended. After the window closes, deployments must pin the MRCB; Kwiry detects a violation but cannot prevent it. The purge guarantee holds only while binaries at or above the MRCB operate the data root.
- **Stale-authority window.** An already-issued capability for a just-retired room can be served until it expires, at most the capability TTL. **The exception is an undetected authority rollback:** a raw restore together with loss of OpenClast's off-store high-water record, a residual until the signed epoch chain. The governed profile therefore requires that record to be stored and restored separately from state backups, and forbids raw restores.
- **Normative form.** The §5.8 transition table is the lifecycle contract. Transitions it does not list are refused.
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

Revision 5 asked eight questions. OpenClast answered all of them in HW §14 (amendments 2), and revision 6 consumes those answers. Kwiry has not edited any OpenClast file.

| # | Question (revision 5) | Answer (HW §14) | Consumed in |
|---|---|---|---|
| 1 | `quarantine_release` had no wire shape | **Answered, D:** an `instructions[]` entry on page 0, `{instruction_id, kind, bound_report_digest, approved_count, issued_seq}`; every report carries `quarantine_manifest_digest`; a mismatch means `release_binding_mismatch` and nothing deleted; an unknown `kind` is skipped | §5.3, §5.5 |
| 2 | Unattributed quarantine with no open obligation | **Answered, E:** `POST /holders/status` with a signed ack, at most 1 per 60 s | §5.5 |
| 3 | A refused `complete` and monotonicity | **Confirmed:** monotonicity is against the last accepted state, and a refused report counts as never sent | §5.6 |
| 4 | S1 behaviour after an authority restore | **Changed:** a supported restore mints a new epoch, and holders refuse with `unexpected_epoch`; recovery is a runbook admin step. **Added, F:** an off-store high-water guard makes a raw restore return `503 authority_epoch_mismatch` and stop minting. The residual (raw restore plus record loss) remains until S2 | §5.3, §5.4, §10, A6 |
| 5 | One id-space per data root | **Confirmed:** enrollment is per tenant, and cross-tenant holdings are refused | §5.1 |
| 6 | Duplicate `resource.id` in one list | **Confirmed:** never emitted | §5.3 |
| 7 | Count-only report updates | **Confirmed:** at most one per obligation per 300 s | §5.6 |
| 8 | `retired` with `escalated` | **Confirmed:** handle like `terminating` | §5.3 |

**Revision 6's four open items are answered by HW §14 G** (freeze amendments 3), and revision 7 consumes them:

| # | Revision-6 item | Answer | Consumed in |
|---|---|---|---|
| 1 | Status request `typ` and ack claims | **G.1:** request `openclast-holder-status+jwt`; ack `openclast-holder-status-ack+jwt` with `idempotency_key`, `report_jti`, `report_digest`, `accepted_quarantined_unattributed`, `accepted_manifest_digest`, `rev` and `iat`, and no `exp` | §5.5, §5.6, T4 |
| 2 | How `release_binding_mismatch` reaches the authority | **G.2:** an optional `release_result: {instruction_id, result: executed or mismatch, observed_manifest_digest}` on status reports; a mismatched instruction is marked `superseded_mismatch` and re-approved | §5.5, L2, L6–L8, the status key (v2) |
| 3 | `bound_report_digest` encoding | **G.3:** `sha256:` over the exact compact JWS of the accepted report | §5.5 |
| 4 | Holder-side epoch acknowledgement | **G.4:** `accept-epoch` on every holder is a required, audited runbook step; a restore is complete only when every holder has accepted or been re-enrolled | §5.4, H5–H11 |

G.5 (strict release scope) and the holder-backup rule are adopted in §5.5, L1, L2 and S15. **No contract question is open in revision 7.**

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
     - Holdings **S1** with amendments D–G: list with `instructions[]`, report, holder status, lifecycle JWKS, holder enrollment with the engine lease policy, the obligation-creation reconcile, the off-store high-water guard, and epoch-minting restore tooling;
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
  - a holder key that is lost or revoked darkens every room until re-enrollment;
  - corrupt holder state, or missing holder state with intact data, darkens every room until the operator recovers;
  - `authority_rollback_suspected` darkens every room until OpenClast's restore runbook runs.

  Nothing is deleted in any of these cases. Release unit L needs Holdings S1, with D, E and F, deployed first.
- **R10 — Restore handling and the residual.**
  - A supported restore mints a new epoch. Every Kwiry holder goes dark until an operator runs the one-use `accept-epoch`, a required runbook step (HW §14 G.4).
  - A raw restore with OpenClast's off-store record intact is detected as `authority_epoch_mismatch`.
  - **A raw restore together with loss of that record is undetected until HW S2.** A retirement the holder never saw can disappear and be served without bound (§5.4).
  - Mitigation is operational: keep the record apart from backups, and use only the restore tooling. The epoch chain (HW S2) stays high priority.
- **R11 — Unattributed data blocks completion holder-wide.** One unknown room's withheld data keeps every obligation at this holder short of `complete` until a list classifies it or a human releases it (HW §14 D). Status reports (HW §14 E) make the count visible with or without an open obligation.
- **R12 — Holder state is durable and outside the data root.**
  - Trust on first use applies only when holder state and data are both provably lost (full content scan). Otherwise loss or corruption fails closed until an audited operator recovery runs.
  - **Restoring holder state older than the retention window is forbidden and refused** (HW §14 G). The operator discards and rebuilds instead.
  - Residual: a backup inside the window still lacks tombstones added after it. Under HW §14 G, an affected item that is unlisted is unknown, so it is withheld and never served. The cost is leftover unattributed data, not re-serving.
- **R13 — Disk pressure.** Withheld and quarantined bytes are retained by design. They count against build admission, so a full volume pauses builders. Lifecycle writes consume the reserves; they never delete retained data to make room. Operators must size `data_root_max_bytes` and the volume for retained data, and release it through approved releases.
- **R14 — Lifecycle complexity.** 105 normative transitions. The mitigation is row-level conformance tests with crash injection, and the rule that unlisted transitions are refused.
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
| Lifecycle transition table | §5.8: 105 rows; unlisted transitions refused; prose subordinate | S3 row-conformance suite: every row exercised, crash-injected after each durable write, and a randomized interleaving harness that asserts only listed transitions occur |
| Obligation purge and reporting | §5.6 state machine, validated enqueue against separate version counters, transmission-time validation, write-ahead attempts, floors, both ack paths, barrier and final scan; §5.7 residual | S3 crash injection at steps 0–7; lost ack with a post-restart retry; convergence acks; writer races; stale-inventory interleaving; unlink-then-fsync failure; H10–H12 |
| Holdings consumption | §5.2 writer and loss table; §5.3 verification, apply transaction, high-water, epochs and classification; §5.4 leases and rollback handling; §5.5 releases and status | HW §9 golden vectors and reference fake extended for D, E and F: H1–H9, H13, H16, H17; withheld reversal; startup order; loss table; `accept-epoch`; `authority_epoch_mismatch`; release intersection and mismatch; jitter bound; key vectors |
| Scheduler fairness | §4.9 bounded accounting turns and stated assumptions | S5 reused-room interleaving test, plus measurements (§11.3) |
| Capacity isolation | §4.4 separate FIFO pools with dedicated executors; permits and threads held until worker exit | S8 controlled stall test (lexical gets a thread and completes within budget); executor-separation assertion |
| Physical-baseline equality | §7 merge and RRF rules over `E` | S7 two-instance tests and the S9 witness rows, including readiness under forbidden load |
| Lexical without the model | §6.1 step 4 applies to semantic and hybrid only | S7/S8 tests with no `semantic-onnx`, semantic disabled, and model init failure |
