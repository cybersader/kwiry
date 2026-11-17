# IG-2A design — governed semantic and hybrid retrieval

Status: **design, owner review pending.** Nothing here is implemented or approved. This page expands [IG-2](governed-semantic-hybrid.md) and is bound by [`CONTRACT.md`](../../CONTRACT.md), which this design does not edit. The contract amendments it needs are listed in [§11](#11-contract-amendments-needed).

## 1. The idea in one paragraph

Semantic search in the governed (`openclast`) profile works exactly like IG-1 lexical search already does: **every room gets its own physical vector store, and a request opens only the stores of the rooms it is signed for.** Each room's vectors live inside that room's partition directory in the same immutable generation as its lexical index, so the two can never disagree. A query embeds the question once, does an exact nearest-neighbour scan inside each authorized room, and merges the per-room lists with a fixed, rowid-independent sort, distance first and then chunk ID. Hybrid fuses that list with the IG-1 authorized lexical list using RRF (`k = 60`). Forbidden rooms are never opened, so they cannot change what a user sees. The acceptance test is the same as IG-1's: results must be byte-equal to a separate instance that holds only the authorized rooms. If any authorized room is not ready, or either hybrid leg fails, the whole request is refused. Kwiry never returns a partial answer and never silently switches mode.

## 2. Decisions the owner needs to make

These are the only calls that gate building. Each has a recommendation. The detail behind each is in the section named.

1. **Approve the contract amendment set ([§11](#11-contract-amendments-needed)).** The frozen contract currently forbids semantic and hybrid search in the governed profile. It also defaults `mode` to `hybrid` and requires every capability to name a subject and an actor. Owner decisions D2 (default lexical) and D4 (no user IDs at the engine) conflict with the last two. *Recommendation: approve A1–A4 as one amendment. Building can start before approval, but mounting the HTTP surface cannot.*
2. **When a room's vectors lag behind its text, refuse or serve stale?** *Recommendation: refuse with `semantic_not_ready` for that request.* Serving older vectors would mix two versions of the room, which no physical baseline can reproduce. Lexical search stays available throughout ([§5](#5-readiness-errors-and-http-mapping)).
3. **Spend extra memory on a dedicated query embedder?** Today one shared model serves both indexing and queries. A user's query can therefore wait behind embedding work for a room they cannot see, and the delay reveals that room's activity. *Recommendation: yes, one extra model instance for queries only (on the order of a few hundred MiB RSS; to be measured in S6). Residual CPU contention is accepted and documented ([§9](#9-side-channel-review)).*
4. **Reject non-room filters (`path_prefix`, `tags`, `frontmatter_equals`) for semantic and hybrid in IG-2A?** Applying them after the vector scan would be a post-filter, which the seam forbids. Applying them before the scan, as a pre-filter, is a separate piece of work. *Recommendation: reject explicitly with `501 filter_unavailable` now. Add pre-scan filtering later as S8.*

Already decided and not reopened here: D1–D4 (2026-11-17), physical per-resource partitions, exact KNN, caps applied after the authorization boundary, and the OpenClast `retrieval` seam position.

## 3. What exists today, and why it cannot be reused

Read from `origin/main` (`74551c1`):

- **Desktop vectors are one mutable store outside generations.** `install_semantic` opens `<data-dir>/semantic/semantic.db` (`kwiry/src/server.rs`). `DesktopIndexManager::reconcile` mutates it in place after each lexical publish. The vectors are not versioned with the generation they describe, and the store holds the whole corpus. That is the "whole-corpus candidate generation" which the contract §5 prohibits for governed use.
- **The governed path has no semantic leg.** `OpenClastIndexManager::reconcile` passes `semantic: None`. `require_desktop_index` returns `SemanticUnavailable` for the openclast profile, and `serve --semantic` is rejected in openclast mode.
- **Desktop KNN ties depend on rowids.** `SemanticStore::search` uses vec0 `MATCH … AND k = ?` and then sorts by `distance, chunk_id`. Which rows vec0 returns *at the k boundary* when distances tie depends on its internal chunk and rowid layout, and that layout depends on insert history. An instance built fresh and one updated over time can return different sets.
- **Desktop hydration drops silently.** `hydrate_ordered` returns only the candidates it finds and skips the rest. That is acceptable on desktop but forbidden by the seam rule "any post-filter rejection refuses the whole call".
- **The desktop vec0 column declares no `distance_metric`**, so sqlite-vec uses L2. With L2-normalized vectors the ordering matches cosine. However, desktop reports `1 − distance` as "cosine similarity", which is not what it is. This is a desktop labelling defect to verify separately. It does not affect IG-2A ordering.
- **What carries over unchanged:** `EmbeddingProfile` and its fingerprint, `embedding_text`, `embedding_source_identity`, `rrf_fuse_traced` (BTreeMap accumulation, `total_cmp`, then `chunk_id`), `PartitionedSearchIndex`'s lazy authorized opening, `validate_document_resource`, and the IG-1 authorized lexical-v2 path (`search_partitions_with_quality`).

## 4. On-disk layout and lifecycle

### 4.1 Layout

```text
<data-dir>/
  current.json
  generations/<generation-id>/
    manifest.json
    layout.json                       # layout_version 3 (was 2)
    partitions/<partition_id>/
      index/                          # Tantivy, unchanged from IG-1
      vectors/
        vectors.json                  # descriptor (below)
        vectors.sqlite                # write-once, opened read-only + immutable
  vector-staging/<partition_id>/      # off-lock build area; purged with the partition
  models/                             # model weights only; never corpus data
```

`partition_id` is the existing IG-1 `sha256("kwiry-resource-v1" ‖ len-prefixed tenant ‖ vault ‖ room)`. The openclast profile never creates `<data-dir>/semantic/`. A test asserts that the directory is absent.

`vectors.sqlite` holds one plain table. It deliberately does not use vec0, so that scan order and storage layout cannot affect results:

```sql
CREATE TABLE vectors (
  chunk_id        TEXT PRIMARY KEY,
  source_key      TEXT NOT NULL,
  source_identity TEXT NOT NULL,   -- embedding_source_identity(content_hash, format)
  embedding       BLOB NOT NULL    -- little-endian f32 × dimensions
) WITHOUT ROWID;
```

The file is written once with `journal_mode=DELETE`, then fsynced and closed. Readers open it as `file:…?mode=ro&immutable=1`. That creates no `-wal`, `-shm` or `-journal` file inside an immutable generation, and leaves no stray copy that a purge could miss.

`vectors.json`:

```jsonc
{
  "vector_schema_version": 1,
  "partition_id": "<hex>",
  "embedding_fingerprint": "<EmbeddingProfile::fingerprint()>",
  "embedding_input_version": 1,          // embedding_text() shape; bump on any change
  "distance": "l2_normalized",           // see §6.2
  "content_digest": "<hex>",             // §4.2
  "chunk_count": 1234,
  "vectors_sha256": "<hex of vectors.sqlite>"
}
```

### 4.2 Readiness is computed, not stored

At generation install, `PartitionedSearchIndex::open` computes a readiness map for each partition. It is immutable for the life of the generation. A partition is **semantic-ready** only if all of the following hold:

1. `vectors.json` parses, its `vector_schema_version` and `embedding_input_version` are supported, and `partition_id` matches;
2. `embedding_fingerprint` equals the loaded model's fingerprint;
3. `content_digest` equals the digest recomputed from **this generation's** `manifest.json` for that resource. The digest is SHA-256 over the sorted `(source_key, embedding_source_identity, sorted chunk_ids)` of every indexed source in the partition, with domain separation and length prefixes;
4. `chunk_count` equals the manifest's chunk count for the resource.

Steps 3–4 tie the vectors to the exact lexical content in the same generation, so the two can never describe different states of a room. Reading the descriptor is O(1). Recomputing the digest from the already-loaded manifest is O(sources), with no file I/O on the vectors. `vectors_sha256` is checked by the builder at attach time, not on every open.

States per partition: `ready`, `pending` (descriptor missing or stale against the current content), `unavailable` (model not loaded, or build failed for this fingerprint). Only `ready` serves.

### 4.3 Build and attach, kept off the writer lock

Lexical publication never waits for vectors. That is contract §1 failure isolation and the vertical-3 rule "semantic failures must never degrade lexical".

1. A lexical reconcile publishes generation *G* exactly as IG-1 does. Unchanged partitions carry their `vectors/` directory forward. Changed partitions come out `pending`, because their content digest moved.
2. A background **vector builder** takes `pending` partitions in `ResourceKey` order. Order affects only latency, never results. For each one it:
   - reads chunk text from *G*'s **lexical partition** (stored `content` and `heading_path`) and computes `embedding_text(heading_path, content)`. The vectors therefore describe exactly what *G* indexed, and the source tree is not re-read;
   - reuses vectors from the partition's previous `vectors.sqlite` where `(chunk_id, source_identity)` is unchanged, and embeds the rest;
   - **batches per source only.** A fastembed batch never spans two sources, so a chunk's vector depends only on its own file's chunk texts. A forbidden or neighbouring file cannot change padding or batch composition (see risk R1);
   - rejects any non-finite component and any vector with the wrong dimension count, then writes `vector-staging/<pid>/<content_digest>.sqlite`.
3. **Attach** is a short critical section under the existing writer lock. It creates a candidate from the active generation. For each staged partition whose `content_digest` still matches the candidate's manifest, it hard-links the staged file into `partitions/<pid>/vectors/` and writes `vectors.json`. Then it publishes. A staged file whose digest no longer matches is discarded and the partition is rebuilt from it next round, reusing its vectors.
4. **Candidate copies hard-link immutable vector files** instead of copying them. `copy_tree_contents` currently copies every byte, which at 384 × 4 B per chunk is about 150 MiB per 100k chunks per publish. This is safe because vector files are never modified in place. If hard-linking fails (for example across devices), the code falls back to a copy.

A crash at any step leaves either the previous complete generation or a `pending` partition. Abandoned staging is removed at startup, just like `.staging-*` generations today.

### 4.4 Model fingerprint

The model is fixed for the life of the process. A fingerprint change means a restart with a different model, profile, `embedding_input_version`, distance kind or schema. That change makes every partition `pending` while lexical stays served, and the builder then rebuilds all partitions with no reuse, because reuse is keyed by fingerprint. Vectors from different fingerprints never coexist inside one generation, and readiness checks the fingerprint on every request path. Status and logs report the fingerprint, never vectors.

### 4.5 Retirement purge

IG-1 already drops a removed or reclassified room's partition from the next layout (the W13 rows). IG-2A adds one rule: **a retired partition's vectors must not survive anywhere Kwiry can reach.**

- When a publish removes a partition, the publish **immediately prunes every older retained generation that still contains it**. This overrides `RETAINED_GENERATIONS = 3` for that case. It also deletes `vector-staging/<pid>/`.
- No other copies exist. The embedder holds no passage text after a batch. There is no query or result cache. `models/` holds weights only.
- A request already running against the old generation keeps its open file descriptors (POSIX unlink semantics). Its result is then refused by the end-of-request check in [§7](#7-hydration-time-resource-re-check).
- Recreating a room under a new opaque room ID gives a new `partition_id` and an empty `pending` state. Nothing is inherited.
- **Limit:** purge means the files are unlinked from every live, retained and staging path. It is not cryptographic erasure of the storage medium. Per-partition keys with crypto-shredding would be a separate, later decision (Q5).

## 5. Readiness, errors and HTTP mapping

### 5.1 Request gate order

The order is fixed. Every check depends only on the request and its signed resources.

1. Capability verification (unchanged). Then the **action must equal the requested mode** ([§6.5](#65-capability-binding-and-effective-mode)).
2. `mode` must be present in the openclast profile (D2: the default is chosen at the OpenClast seam, not inferred by Kwiry).
3. Query and control validation. The existing lexical-v2 rules in contract §10d.6 still apply: hybrid accepts `>field` and rejects `in:`, and semantic rejects every lexical control. Non-room filters are rejected for semantic and hybrid (decision 4).
4. Model loaded? If not, `mode_unavailable`.
5. Pin the active generation with one `load_full()`. Every authorized resource left after vault/room narrowing must have a partition, otherwise `resource_unavailable`. This is IG-1 behaviour.
6. Every such resource must be semantic-`ready`, otherwise `semantic_not_ready`. **No partition is opened before steps 5–6 pass for all of them.**
7. Execute. Any failure refuses the whole call.

### 5.2 Taxonomy

| Condition | Kwiry `code` | HTTP | OpenClast seam (peer-reported) |
|---|---|---|---|
| Model not loaded, build lacks `semantic-onnx`, or semantic disabled in config | `mode_unavailable` | 501 | passed through as `mode_unavailable` |
| `mode` omitted in openclast profile | `invalid_request` | 400 | seam supplies the D2 default before dispatch |
| Capability action ≠ requested mode | `forbidden` | 403 | gateway defect; `search_provider_violation` |
| Non-room filter with semantic or hybrid | `filter_unavailable` (new) | 501 | explicit refusal |
| Lexical control not allowed for the mode | existing `invalid_query` family | 400 | explicit refusal |
| Cursor supplied | `cursor_unavailable` | 501 | `ranked_unpageable` |
| Signed resource has no partition, or retired mid-request (§7) | `resource_unavailable` | 503 | `search_provider_failed`, no partial re-query |
| Authorized partition `pending` or `unavailable` | `semantic_not_ready` (new) | 503 | per-room disclosure before dispatch (§5.3) |
| Query embedding or vector read fails at run time, either leg | `semantic_failed` (new) | 503 | hybrid with one leg down refuses |
| Hydration integrity failure (§7) | `internal_error` (fixed message) | 500 | refuse |
| Capability expired before the response is assembled | `unauthorized` | 401 | re-mint |
| No generation yet | `index_building` | 503 | unchanged |

All messages are fixed strings. None contains a path, partition ID, count, query fragment or engine error text. The frozen envelope `{ "error": { "code", "message" } }` is unchanged.

### 5.3 Disclosing per-room readiness before dispatch

The seam needs to tell a user *which* of their rooms are not semantic-ready before it dispatches. The frozen error envelope has no field for that. **Recommended: a capability-protected scoped readiness probe** (amendment A3):

```http
POST /v0/readiness            # capability action: "readiness"; same resource set
200 { "resources": [ { "vault_id": "...", "lexical": "ready", "semantic": "ready|pending|unavailable" } ],
      "embedding_fingerprint": "<hex>" }
```

The probe returns only the signed resources, as closed enums, with no counts and no timestamps. OpenClast probes, discloses, and then dispatches. Search still re-checks readiness at step 6, so a race between probe and search is resolved by refusal, not by a partial answer. The fallback, if A3 is declined, is that `semantic_not_ready` stays undifferentiated and OpenClast discloses "one or more rooms not ready".

The desktop `/v0/status` is unchanged. The openclast profile still mounts no `status` endpoint.

## 6. Query path and determinism rules

### 6.1 Semantic (`mode: "semantic"`)

```text
gate (§5.1) → embed query once (query embedder, BGE query prefix)
→ for each authorized ready partition P (sorted by ResourceKey):
     open P/vectors.sqlite read-only+immutable (per request; no connection cache)
     SELECT chunk_id, vec_distance_l2(embedding, :q) AS d
       FROM vectors ORDER BY d, chunk_id LIMIT :C
→ global merge: concat, sort by (d, chunk_id), truncate to C
→ strict hydration per partition (§7) → truncate to limit → response
```

- `C = 100` (the existing `HYBRID_CANDIDATES`; `limit ≤ 100`). The cap is applied **only after** authorization has chosen the partitions, both per partition (`LIMIT C`) and globally. Every element of the global top-C is in its own partition's top-C, so merging per-partition top-C lists gives exactly the global top-C of the union. That union equals what a physical baseline holding only those rooms would compute.
- The scan is exact: every vector in every authorized partition is scored. Work is O(Σ authorized chunks × dimensions). Forbidden partitions cost nothing.
- Distances come from sqlite-vec's scalar function, an imported algorithm (contract invariant 6). Kwiry implements no distance math itself.

### 6.2 Float and tie rules

- **Write:** reject a vector with any NaN or ±∞ component, or with the wrong dimension count. The partition stays `pending` and the failure is logged as a closed category.
- **Query:** reject a non-finite query vector with `semantic_failed`.
- **Distance:** `vec_distance_l2` over L2-normalized vectors, which gives the same ordering as cosine. The f32 result is widened exactly to SQLite REAL (f64).
- **Signed zero:** canonicalize `-0.0` to `+0.0` before any Rust-side comparison, because SQLite already treats them as equal.
- **Order:** within SQLite, `ORDER BY d, chunk_id` (REAL, then BINARY-collated TEXT). In the Rust merge, `d` by `f64::total_cmp` on the canonicalized values, then `chunk_id` bytewise ascending. NaN cannot reach either sort.
- **Chunk IDs are unique across partitions**, because the hash input includes `vault_id` and each vault maps to exactly one resource. The tie-break is therefore a total order.
- **Scores returned:** semantic `score = 1.0 − d` as f32, labelled "semantic distance score", not cosine. Ordering never reads the f32 value.
- **Byte equality is promised for the same binary, model artifact and CPU feature set.** The witness runs the governed instance and the baseline instance on one host. Equality across different SIMD paths is not promised.

### 6.3 Hybrid (`mode: "hybrid"`)

```text
gate → lexical leg: IG-1 search_partitions_with_quality over the same authorized
       partitions, limit C, lexical-v2 with optional >field emphasis
     → semantic leg: §6.1 list (top C) over the stripped query text
     → rrf_fuse_traced(lexical_ids, semantic_ids, k = 60)
     → strict hydration → truncate to limit
```

- **RRF constants:** `k = 60.0` (f64). Rank is 1-based within each authorized list. Each contribution is `1.0 / (k + rank)`. The fused value is the lexical contribution plus the semantic contribution, always added in that order. The sort is fused descending by `total_cmp`, then `chunk_id` ascending. `score` is that f64 cast to f32 after ordering.
- **Both legs run on the one pinned generation.** If either leg errors, or the semantic leg is not ready, the request refuses. There is no fallback to a single leg (seam: one hybrid leg down → refuse).
- The lexical leg's ranking is already proven equal to the authorized-only baseline by IG-1 (W5). Fusion is a pure function of the two authorized lists, so it inherits that equality.

### 6.4 Excerpts

Each hit is hydrated and excerpted **from its own partition's searcher**. The `SnippetGenerator` term statistics are therefore those of that partition only, which is the same partition the baseline instance has. No aggregated snippet generator ever spans partitions. A hit is one passage from one file.

### 6.5 Capability binding and effective mode

- `actions` becomes exactly one of `search:lexical`, `search:semantic` or `search:hybrid`, and it must equal the request `mode`. A lexical capability can never run a semantic scan.
- Success responses add `X-Kwiry-Effective-Mode: <mode>`, which always equals the requested mode because nothing falls back. The OpenClast provider echoes it and raises `search_provider_violation` on a mismatch.
- Kwiry returns the existing hit schema (`chunk_id`, `score`, `excerpt`, …) to the **server-side gateway only**. The seam rule "no scores, no chunk IDs out" is enforced by the OpenClast provider, which strips them. Removing them inside Kwiry would change the frozen hit schema (see Q6).

## 7. Hydration-time resource re-check

Desktop hydration skips candidates it cannot find. Governed hydration instead **refuses**:

1. Candidates stay grouped by the partition that produced them. Lexical hits map back through `vault_id` to the request's resource set, which is a function because §6.5 rejects capabilities that repeat a `vault_id`. Hydration looks up each chunk **only in its own partition**, never by global chunk ID.
2. The stored `(vault_id, room)` of each document must equal the partition's resource (`validate_document_resource`, as IG-1 does). That resource must be a member of **this request's** effective authorized set, checked again at hydration against the request-scoped set rather than any shared state.
3. Every candidate must hydrate. A candidate missing from its own partition in the pinned generation is an integrity failure: `500 internal_error`, whole call. Nothing is dropped. Because the vectors and the lexical index belong to the same generation and are bound by `content_digest`, this should never happen. If it does, it is a bug, not a user-visible partial answer.
4. **End-of-request check (Kwiry's half of "authority checked twice"):** before responding, Kwiry confirms that every searched resource is still in the *then-active* layout, and that the capability's `exp` has not passed. A room retired or reclassified mid-request gives `503 resource_unavailable`, and an expired capability gives `401`. OpenClast performs the second, grant-level check after the response and returns `search_authorization_changed` on any narrowing. Kwiry cannot see grants and does not emit that code.

Mid-request *grant* revocation is OpenClast's responsibility. Kwiry's capability is fixed for the request and lives at most 60 s. IG-4 epochs remain the later, stronger mechanism.

## 8. Cross-file content check (seam: one passage, one file)

Result of reading the code: **today's chunker and embedding input include no cross-file content.**

- Markdown sections are byte slices of the file's own body between heading offsets (`formats/markdown.rs`). `![[embed]]` and `[[link]]` remain literal text, and nothing resolves or dereferences them.
- `links_out` (`links.rs`) is a regex over the file's own text. It is `STORED` only (`index.rs`): it is never indexed, never embedded and never used in excerpts. No backlink computation exists anywhere in the crate.
- `embedding_text` = the file's own `heading_path` joined by ` > `, plus the chunk's own `content`. Frontmatter, aliases and `links_out` are not included.
- The Canvas extractor emits a file node's **path string** (the canvas's own authored reference) and never reads the target. The contract already forbids dereferencing for Excalidraw (§10a.3) and HTML (§10c.3). DOCX, PDF and Excel read only their own package.
- Excerpts come from the hit's own stored `content`.

**No fix needed.** S2 adds a regression test (§10). A note containing `![[forbidden-room-note]]` and `[[forbidden-room-note]]` must produce chunk text, embedding input and excerpt that contain only the literal link text. Its vector must be bit-identical whether the target exists in a forbidden room, exists in an authorized room, or is absent. Any future link-resolution feature must keep resolution out of chunk text and embedding input, or bump `embedding_input_version` with its own review.

## 9. Side-channel review

| Channel | Finding | Design response |
|---|---|---|
| **Scan cost** | Work is O(authorized chunks); forbidden partitions are never opened. | Bounded by construction. Latency reveals only the size of the caller's own rooms, which the caller can already read. |
| **Shared embedder** | Today one mutex-serialized model serves indexing and queries. Embedding a forbidden room delays the query, which leaks that room's write activity and volume. | Dedicated query embedder (decision 3). The builder runs at lower priority. Remaining CPU and memory-bandwidth contention is documented as an accepted residual, as IG-1 reconcile already is. |
| **Warm vs cold** | IG-1 lexical readers are cached per resource, so first-open latency shows whether someone else recently searched that room. | Vector files are opened per request (no connection cache). Lexical reader caching is unchanged. It leaks only co-user activity on a room the caller can read, and it is documented. OS page-cache warmth is a host-level residual. |
| **Caches** | D4 allows caches keyed only by the authorized-set digest, or none. | **None in IG-2A.** No query-embedding cache, no result cache, no fused-list cache. Staged vectors are keyed by content digest, are not user-related, and are purged with the room. |
| **Counts** | Hits ≤ `limit`. No totals, no candidate counts. | Byte-equal to the baseline, including truncation. `X-Kwiry-Lexical-Match-Quality` is `not_applicable` for semantic and hybrid (existing behaviour). |
| **Errors** | Gate order and messages are fixed (§5). | `semantic_not_ready` and `resource_unavailable` are computed over signed resources only. The readiness probe returns signed resources only. |
| **Generation header** *(existing IG-1 issue, made worse by IG-2A)* | `X-Kwiry-Generation` is a global ID that embeds a nanosecond timestamp and changes when *any* room changes, including forbidden ones. Vector attach publishes add more such changes. | A4: in the openclast profile, return an **authorized view digest** instead. It is computed over the pinned generation's `(partition_id, lexical content digest, vector descriptor digest)` for the signed resources only, so it changes only when authorized content changes. |
| **Freshness header** *(existing IG-1 issue)* | `X-Kwiry-Index-Freshness` reads the daemon-wide `rebuilding` and `dirty` flags, so it reveals reconciliation of forbidden rooms. | A4: compute freshness over the signed resources only, using per-partition state. |
| **Logs** *(existing; conflicts with D4)* | `server.rs` logs `subject = sha256(subject)`, which is unkeyed and dictionary-reversible, and logs `actor` in plain text. | Remove both. Log only `jti`, mode, resource count, outcome code and latency bucket. Never log query text, its fingerprint, embeddings, distances or chunk IDs. The D3 keyed query fingerprint belongs to OpenClast's audit, not Kwiry's. |
| **Purge residue** | Retained generations and staging could keep a retired room's vectors. | §4.5: immediate prune and staging purge. A test scans the data root after retirement. |

## 10. Slices

Each slice is a small PR with its own tests, reviewed at the owner checkpoint. S2–S4 change only core code that nothing mounts. Nothing is reachable over HTTP before S5, and S5 needs the amendment.

| Slice | Content | Tests (new) |
|---|---|---|
| **S1 — amendment** | Contract text for A1–A4 (docs only, owner-approved). | — |
| **S2 — partition vector store** | `vector_partition.rs`: write-once plain-table store, read-only immutable open, exact scan `ORDER BY d, chunk_id LIMIT C`, non-finite and dimension rejection, `vectors.json` descriptor, content digest. Cross-file regression test (§8). | identical results under permuted insert order (rowid independence); ties straddling the `LIMIT` boundary; ±0.0; NaN/∞ refused; immutable open creates no sidecar files; descriptor mismatch (fingerprint, digest, count, versions) → not ready; embed-link note vectors identical across target presence |
| **S3 — generation integration and builder** | `layout_version` 3; readiness map at open; hard-link carry; off-lock builder with per-source batches and reuse; attach-publish; startup staging cleanup; retirement purge with immediate prune; openclast config `[semantic] enabled`. | lexical publish proceeds when the embedder faults (fake failing embedder); `pending → ready` transitions per partition (room B stays ready while room A changes); fingerprint change → all pending, lexical served; crash between stage and attach → no servable partial; retirement → no file under the data root contains the retired `partition_id` or its chunk IDs; recreation inherits nothing; per-source batching gives vectors identical to single-source embedding; carried vector files are hard links |
| **S4 — governed query path (core)** | `search_authorized_semantic` and `search_authorized_hybrid` on `PartitionedSearchIndex`: pinned generation, gate steps 5–7, merge, RRF, strict hydration, end-of-request check. | **physical baseline:** instance A (rooms 1–4) vs instance B (only the authorized rooms 1–2), byte-equal hits, order, truncation, scores and coverage for semantic and hybrid; forbidden add/remove/mutate on A leaves results unchanged; two-user asymmetric fixture; cross-partition fan-in with engineered equal distances; 256-resource fan-in bound; missing candidate → refuse, not drop; resource mismatch → refuse; a pending partition → refuse before any vector file opens; one-leg failure → refuse; desktop results unchanged |
| **S5 — capability and HTTP** | action-per-mode; explicit mode required; repeated `vault_id` in a capability → 403; new codes and mapping; `X-Kwiry-Effective-Mode`; drop subject/actor from logs (coordinated claim transition, A2); remove the openclast `--semantic` rejection; readiness probe (A3). | one server test per §5.2 row; captured logs contain no query text, subject or actor; the probe returns only signed resources; a lexical capability cannot run semantic |
| **S6 — side-channel hardening** | dedicated query embedder; authorized-view generation and freshness headers (A4); RSS measurement. | headers unchanged under forbidden-room publish and reconcile; query latency unaffected by a forbidden-room embedding storm (bounded statistical check, documented residual) |
| **S7 — observer and witness** | Extend the internal observer (§10.1) and the connected witness rows (§10.2). | shipped and observer builds produce the same functional digest across all rows |
| **S8 — later** | pre-scan filter rowset inside an authorized partition (`path_prefix`, tags, frontmatter). | baseline equality with filters |

Dependency: S7 needs the internal witness observer, which today exists only on the unmerged `feat/ig1-witness-observer` branch (`5ee249e`). It has to land on `main`, or be rebased, before S7.

### 10.1 Observer events (feature-gated, absent from shipped builds)

The vocabulary is added to the existing per-resource events (`selected`, `reader_*`, `scoring`, `hydrate_*`): `vector_gate_ready` / `vector_gate_refused` (per resource, before any open), `vector_open`, `vector_scan`, `vector_refused`, `query_embed` (no resource), `fusion` (count of each list only), `end_recheck_ok` / `end_recheck_refused`. The invariant stays the same: **zero vector events for any excluded resource.**

### 10.2 Witness mapping

V-row numbers belong to OpenClast and are not in this repository, so the mapping is by theme. Numbers get pinned when the OpenClast V1–V16 table is shared.

| OpenClast V-theme | New Kwiry evidence | Extends existing row |
|---|---|---|
| Two-user vector and hybrid | asymmetric two-user/two-room semantic and hybrid requests; observer shows only signed partitions | W2, W8 (zero excluded-partition access) |
| Forbidden-partition invariance | byte-equal hits, order, truncation and coverage vs a physical baseline holding only authorized rooms, unchanged under forbidden add, remove or mutate | W5 (authorized-only baseline) |
| Mid-request revocation | room retired between gate and response → `resource_unavailable`; capability expiry → 401; OpenClast post-check → `search_authorization_changed` | W8 (live grant), W13a |
| Retirement purge | after retirement, no vector file or staging entry for the old `partition_id`; recreation gets new IDs and an empty `pending` state | W13a–W13f |
| Cross-file embeds | embed and link fixture: literal text only; vector identical across target presence | new row |
| Audit privacy | Kwiry logs and data root contain no query text, subject, actor, embeddings or distances; OpenClast audit holds only the D3 keyed fingerprint | W11 |
| Named errors, no fallback | `mode_unavailable`, `semantic_not_ready`, `semantic_failed`, one-leg-down, `filter_unavailable`, `ranked_unpageable` | W10 |
| Shipped = observer | functional digest identical across all semantic and hybrid rows | W12a, W12b |

W1, W3, W4, W6, W7 and W9 are re-run unchanged as regression.

## 11. Contract amendments needed

Flagged here only. `CONTRACT.md` is not edited by this design.

- **A1 — governed modes and default (§4, §11).** Admit `semantic` and `hybrid` in the OpenClast profile under §5's authorization-before-retrieval rule as designed here. In that profile `mode` is **required** at Kwiry. The D2 default (`lexical`) is applied at the OpenClast seam. The desktop default (`hybrid`) is unchanged unless the owner extends D2 (Q4).
- **A2 — capability identity (§5).** §5 currently requires "represented subject, distinct current actor". D4 forbids user IDs at the engine, even pseudonymous ones. Amend this so the Kwiry capability carries no subject, actor or subject revision. The verified principal becomes the gateway workload, `sub` is a fixed workload identifier, and `jti` stays a per-request random nonce. The change needs a coordinated OpenClast claim transition.
- **A3 — scoped readiness probe (§4).** A capability-protected `POST /v0/readiness` in the OpenClast profile returning closed per-resource enums for signed resources only (§5.3). The alternative is to decline, which leaves undifferentiated disclosure.
- **A4 — governed freshness and generation headers (§4, IG-1 doc).** In the OpenClast profile, `X-Kwiry-Generation` becomes an authorized view digest and `X-Kwiry-Index-Freshness` is computed over signed resources only (§9). This also fixes an existing IG-1 leak.

Not amendments: refusing non-room filters (the explicit-refusal rule already covers it), exact baseline equality (stronger than §1.1, not in conflict), and `ranked_unpageable` (an OpenClast mapping of `cursor_unavailable`).

## 12. Risks and open questions

**Risks**

- **R1 — ONNX determinism.** Byte equality needs bit-identical embeddings for the same text. Per-source batching removes cross-file padding effects. ORT thread-count or kernel selection could still vary between runs. *Mitigation:* fix the ORT intra-op thread count for the governed profile, and add an S3 test that re-embeds and compares bits (feature-gated ONNX test). If that test fails, the baseline proof must be re-scoped with the owner. **Highest technical risk.**
- **R2 — Storage and copy cost.** Vectors add about 1.5 KiB per chunk per generation. Without hard links (S3), every publish copies them, and with three retained generations that is about 3× the vector size.
- **R3 — Memory.** The model in the sidecar, plus a second query embedder (decision 3), adds hundreds of MiB RSS. To be measured; tmpfs and host headroom matter.
- **R4 — Time to ready.** Initial embedding of a large room can take minutes, so newly enrolled rooms report `pending` for that long. Lexical stays available.
- **R5 — Exact scan latency.** sqlite-vec measured about 39 ms at 100k chunks (desktop, vec0). A plain-table scalar scan is expected to be similar but is unmeasured. With 256 resources, latency is bounded by total authorized chunks. Any ANN must first re-prove baseline equality (fixed input).
- **R6 — Observer not on `main`.** S7 is blocked until the observer branch lands.
- **R7 — Existing IG-1 header and log leaks (§9).** These are real today, independent of IG-2A. Worth fixing even if IG-2A slips.
- **R8 — Purge is not secure erase** (§4.5).

**Open questions** (beyond the four decisions in §2)

- **Q5** — Per-partition encryption with crypto-shredding on retirement: wanted later, or out of scope?
- **Q6** — Should Kwiry itself omit `score` and `chunk_id` in the openclast profile, as defence in depth instead of relying on OpenClast stripping them? That would need a hit-schema amendment.
- **Q4** — Should D2 (default lexical) also change the desktop default, which is `hybrid` per contract §4 and vertical-3?
- **Q7** — Desktop labels L2-derived scores as "cosine similarity" (§3). Fix it as a separate desktop correction?
- **Q8** — Kwiry's `openclast` profile is to be renamed to a generic governed profile (IG-1 follow-up). IG-2A code should use the generic name where it is new.
