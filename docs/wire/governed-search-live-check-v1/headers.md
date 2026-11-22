# Headers and first-failing stage

Only `POST /v0/search` negotiates. HTTP field names are case-insensitive. After outer OWS removal compare the profile value case-sensitively; never split comma lists or deduplicate repeated fields. No `Accept` negotiation. Every application response has `Cache-Control: no-store`. Search replies have `Content-Type: application/json`, `Kwiry-Id-Scheme: room-v1` and `Kwiry-Multi-Room: closed`. Health (`GET, HEAD /v0/health`) is baseline liveness, never search readiness. Neither it nor an off-route response carries search headers or an echo.

| Order | Check | Failure |
|---|---|---|
| 0 | Mounted route | 404 `not_found` outside mounted schemas; health method mismatch 405 |
| 1 | Exactly one literal `Authorization: Bearer <compact JWS>`, [claims and replay](capability.md) | 401 `unauthorized` + `WWW-Authenticate: Bearer`, 403 `forbidden`, or 503 `replay_state_unavailable` |
| 2 | POST | 405 `method_not_allowed`, `Allow: POST` (framework can also set Allow on earlier auth failures) |
| 3 | Exactly one `Kwiry-Profile: live-check-v1` | Missing, unknown, repeated (even identical), or comma list → 406 `profile_unsupported` |
| 4 | One signed tuple | 403 `forbidden` |
| 5 | Holder, valid serving config, global engine and strict-hash basis | 503 `resource_unavailable`, no indexes |
| 6 | Exactly one bare `Content-Type: application/json` (ASCII case-insensitive); ≤2 MiB UTF-8 JSON named object; closed fields; integer literal limit in 1–100 | 400 `invalid_request`. Then compare to the signed ceiling: lower → 400 `invalid_request`, higher → 403 `limit_exceeded`, equal proceeds |
| 7 | `q`: Unicode 15.1 word starts L or Nd, continues L/Nd/M. One or more ASCII spaces between words, no outer spaces, no standalone case-insensitive AND/OR/NOT. ≤4096 UTF-8 bytes, ≤128 input words and ≤128 planned evidence terms | 400 `invalid_query` |
| 8 | Acquire the tuple's immutable labeled reader and search; stage the whole page | Missing/unusable partition or binding → 503 `resource_unavailable` with `resource_indexes:[0]`. Task/index/response faults including invalid stored hash or impossible hit shape → 500 `internal_error` |
| 9 | Check global availability again, then resource availability | Global 503 without indexes, resource-specific 503 with `[0]`; never relabel the prepared page |

No echo through stage 3. **Every** selected-profile reply (stages 4–9 and successes) echoes exactly one `Kwiry-Profile: live-check-v1`. No success headers, snapshots, hashes or partial page on errors. Never forward or log error text. Do not retry.

Only search 200 carries:

- `X-Kwiry-Index-Freshness: <current|stale|reconciling>; basis=strict_hash`
- `X-Kwiry-Generation: v1-<64 lowercase hex>` — not an edition label
- `X-Kwiry-Lexical-Profile: lexical-v2`
- `X-Kwiry-Lexical-Match-Quality: standard_only|mixed|partial_only|none`
- `X-Kwiry-Field-Scope: none` and `X-Kwiry-Field-Emphasis: none`

The 512 KiB decoded response limit and end-to-end deadline are gateway policies, not engine guarantees.
