// SPDX-FileCopyrightText: 2026 cybersader
// SPDX-License-Identifier: GPL-3.0-only

import { FTS_HIGHLIGHT_END, FTS_HIGHLIGHT_START } from "../excerpt";
import type { MatchPlan, MetadataProbePlan } from "./rust-adapter";

export const FTS5_PROFILE_ID = "lexical-v1" as const;
export const FTS5_WEIGHTS = [5, 6, 6, 6, 3, 1, 2, 1, 5] as const;

const SEARCH_SQL = `
SELECT
  c.chunk_id,
  c.vault_id,
  c.path,
  c.heading_path_json,
  c.frontmatter_json,
  -bm25(chunks_fts, ${FTS5_WEIGHTS.join(", ")}) AS score,
  snippet(chunks_fts, 7, '${FTS_HIGHLIGHT_START}', '${FTS_HIGHLIGHT_END}', '…', 24) AS excerpt
FROM chunks_fts
JOIN chunks AS c ON c.rowid = chunks_fts.rowid
WHERE chunks_fts MATCH ?
ORDER BY score DESC, c.chunk_id ASC, c.path ASC
LIMIT ?
`;

const METADATA_PROBE_SQL = `
SELECT EXISTS(
  SELECT 1
  FROM chunks_fts
  WHERE chunks_fts MATCH ?
  LIMIT 1
)
`;

const SEARCH_PLANS: Readonly<Record<MatchPlan["plan_id"], string>> = Object.freeze({
  lexical_any_v1: SEARCH_SQL,
  lexical_all_v1: SEARCH_SQL,
  lexical_explicit_v1: SEARCH_SQL,
});

export interface BoundSearchPlan {
  sql: string;
  bind: readonly [string, number];
}

export interface BoundMetadataProbe {
  sql: string;
  bind: readonly [string];
}

export function bindSearchPlan(plan: MatchPlan, limit: number): BoundSearchPlan {
  const sql = SEARCH_PLANS[plan.plan_id];
  if (plan.schema_version !== 1 || !sql || !isMatchValue(plan.match_value)) {
    throw new Error("unsupported Rust FTS5 search plan");
  }
  if (!Number.isSafeInteger(limit) || limit < 1 || limit > 100) {
    throw new Error("invalid FTS5 search limit");
  }
  return { sql, bind: [plan.match_value, limit] };
}

export function bindMetadataProbe(plan: MetadataProbePlan): BoundMetadataProbe {
  if (plan.schema_version !== 1
    || plan.plan_id !== "metadata_probe_v1"
    || !isMatchValue(plan.match_value)) {
    throw new Error("unsupported Rust FTS5 metadata probe plan");
  }
  return { sql: METADATA_PROBE_SQL, bind: [plan.match_value] };
}

function isMatchValue(value: string): boolean {
  return value.length > 0 && value.length <= 16_384 && !value.includes("\0");
}
