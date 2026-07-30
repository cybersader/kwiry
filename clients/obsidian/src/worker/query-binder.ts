// SPDX-FileCopyrightText: 2026 cybersader
// SPDX-License-Identifier: GPL-3.0-only

import type { EvidenceProbePlan, ExecutionPlan, StagePlan } from "./rust-adapter";

export const FTS5_PROFILE_ID = "lexical-v1" as const;
export const FTS5_WEIGHTS = [5, 6, 6, 6, 3, 1, 2, 1, 5] as const;

const SEARCH_SQL = `
SELECT
  c.chunk_id,
  c.vault_id,
  c.path,
  c.heading_path_json,
  c.frontmatter_json,
  -bm25(chunks_fts, ${FTS5_WEIGHTS.join(", ")}) AS score
FROM chunks_fts
JOIN chunks AS c ON c.rowid = chunks_fts.rowid
WHERE chunks_fts MATCH ?
ORDER BY score DESC, c.chunk_id ASC, c.path ASC
LIMIT ?
`;

const EXACT_SEARCH_SQL = `
WITH exact(value) AS (VALUES (?)),
candidates(rowid, score) AS (
  SELECT c.rowid, 12.0
  FROM exact JOIN sources AS s ON s.exact_filename = exact.value
  JOIN chunks AS c ON c.source_key = s.source_key
  UNION ALL
  SELECT c.rowid, 12.0
  FROM exact JOIN sources AS s ON s.exact_stem = exact.value
  JOIN chunks AS c ON c.source_key = s.source_key
  UNION ALL
  SELECT c.rowid, 12.0
  FROM exact JOIN sources AS s ON s.exact_title = exact.value
  JOIN chunks AS c ON c.source_key = s.source_key
  UNION ALL
  SELECT c.rowid, 12.0
  FROM exact JOIN sources AS s
  JOIN json_each(s.exact_aliases_json) AS alias ON alias.value = exact.value
  JOIN chunks AS c ON c.source_key = s.source_key
  UNION ALL
  SELECT c.rowid, 3.0
  FROM exact JOIN chunks AS c ON c.exact_heading = exact.value
  UNION ALL
  SELECT c.rowid, 5.0
  FROM exact JOIN chunks AS c
  JOIN json_each(c.identifiers_json) AS identifier ON identifier.value = exact.value
),
ranked(rowid, score) AS (
  SELECT rowid, max(score) FROM candidates GROUP BY rowid
)
SELECT
  c.chunk_id,
  c.vault_id,
  c.path,
  c.heading_path_json,
  c.frontmatter_json,
  ranked.score
FROM ranked JOIN chunks AS c ON c.rowid = ranked.rowid
ORDER BY ranked.score DESC, c.chunk_id ASC, c.path ASC
LIMIT ?
`;

const EXISTS_SQL = `
SELECT EXISTS(
  SELECT 1
  FROM chunks_fts
  WHERE chunks_fts MATCH ?
  LIMIT 1
)
`;

const PREFIX_EXPANSIONS_SQL = `
SELECT term
FROM chunks_fts_vocab
WHERE col IN ('filename', 'stem', 'aliases', 'title', 'heading_text', 'content')
  AND term LIKE ? ESCAPE '\\'
  AND length(CAST(term AS blob)) <= ?
GROUP BY term
ORDER BY term ASC
LIMIT ?
`;

const MATCH_STAGE_IDS = new Set<StagePlan["plan_id"]>([
  "lexical_explicit_v2",
  "lexical_exact_phrase_v2",
  "lexical_all_terms_v2",
  "lexical_partial_coverage_v2",
  "lexical_prefix_v2",
]);

export interface BoundSearchStage {
  sql: string;
  bind: readonly unknown[];
}

export interface BoundExistsProbe {
  sql: string;
  bind: readonly [string];
}

export interface BoundPrefixProbe {
  sql: string;
  bind: readonly [string, number, number];
}

export interface BoundEvidenceProbe {
  exists: BoundExistsProbe;
  prefix: BoundPrefixProbe | null;
}

export function requireExecutionPlanIdentity(plan: ExecutionPlan): void {
  if (plan.schema_version !== 2
    || plan.profile_id !== FTS5_PROFILE_ID
    || plan.max_total_candidates !== 512
    || plan.stages.length > 5
    || plan.stages.some((stage, index) => stage.ordinal !== index)) {
    throw new Error("unsupported Rust FTS5 execution plan");
  }
  if (plan.disposition === "empty_no_evidence") {
    if (plan.stages.length !== 0) throw new Error("invalid empty FTS5 execution plan");
    return;
  }
  if (plan.disposition === "explicit_bypass") {
    if (plan.stages.length !== 1 || plan.stages[0]?.plan_id !== "lexical_explicit_v2") {
      throw new Error("invalid explicit FTS5 execution plan");
    }
    return;
  }
  if (plan.disposition !== "ready" || plan.stages.length === 0
    || plan.stages.some((stage) => stage.plan_id === "lexical_explicit_v2")) {
    throw new Error("invalid assisted FTS5 execution plan");
  }
}

export function bindSearchStage(stage: StagePlan, limit: number): BoundSearchStage {
  if (!isLimit(limit) || stage.max_candidates < limit) {
    throw new Error("invalid FTS5 stage search limit");
  }
  if (stage.plan_id === "lexical_exact_metadata_v2") {
    if (!isOpaqueUnicodeScalarValue(stage.exact_value, 256) || stage.match_value !== undefined) {
      throw new Error("unsupported Rust FTS5 exact stage");
    }
    return { sql: EXACT_SEARCH_SQL, bind: [stage.exact_value, limit] };
  }
  if (!MATCH_STAGE_IDS.has(stage.plan_id)
    || !isOpaqueValue(stage.match_value, 16_384)
    || stage.exact_value !== undefined) {
    throw new Error("unsupported Rust FTS5 match stage");
  }
  return { sql: SEARCH_SQL, bind: [stage.match_value, limit] };
}

export function bindEvidenceProbe(plan: EvidenceProbePlan): BoundEvidenceProbe {
  if (plan.schema_version !== 2 || !isOpaqueValue(plan.match_value, 16_384)) {
    throw new Error("unsupported Rust FTS5 evidence probe");
  }
  if (plan.plan_id === "identifier_metadata_v2") {
    return { exists: { sql: EXISTS_SQL, bind: [plan.match_value] }, prefix: null };
  }
  if (plan.plan_id !== "term_support_v2"
    || plan.max_prefix_expansions !== 16
    || plan.max_prefix_term_bytes !== 96
    || !Number.isSafeInteger(plan.probe_id)
    || !Number.isSafeInteger(plan.term_index)) {
    throw new Error("unsupported Rust FTS5 term probe");
  }
  const prefix = plan.prefix_pattern === null
    ? null
    : isOpaqueValue(plan.prefix_pattern, 4_096)
      ? {
          sql: PREFIX_EXPANSIONS_SQL,
          bind: [
            plan.prefix_pattern,
            plan.max_prefix_term_bytes,
            plan.max_prefix_expansions + 1,
          ] as const,
        }
      : null;
  if (plan.prefix_pattern !== null && prefix === null) {
    throw new Error("unsupported Rust FTS5 prefix probe");
  }
  return { exists: { sql: EXISTS_SQL, bind: [plan.match_value] }, prefix };
}

function isLimit(value: number): boolean {
  return Number.isSafeInteger(value) && value >= 1 && value <= 512;
}

function isOpaqueValue(value: unknown, maximum: number): value is string {
  return typeof value === "string"
    && value.length > 0
    && value.length <= maximum
    && !value.includes("\0");
}

function isOpaqueUnicodeScalarValue(value: unknown, maximum: number): value is string {
  return typeof value === "string"
    && value.length > 0
    && [...value].length <= maximum
    && !value.includes("\0");
}
