// SPDX-FileCopyrightText: 2026 cybersader
// SPDX-License-Identifier: GPL-3.0-only

import { encodeExactIdentifierMatch, encodeExactIdentifierToken } from "./exact-identifier-token";
import type { EvidenceProbePlan, ExecutionPlan, StagePlan } from "./rust-adapter";

export const FTS5_PROFILE_ID = "lexical-v1" as const;
export const FTS5_WEIGHTS = [5, 6, 6, 6, 3, 1, 2, 1] as const;

// A content role never transforms a score: contract §10.5 ranks every format
// by identical text-evidence rules, and banding Excel scores into [0,3) both
// demoted strong Excel matches below mid-strength Markdown matches and
// promoted weak ones above them. Non-primary content is kept un-boosted at
// preparation instead (it carries no heading or identifier fields), and it
// loses ties to primary content because the role byte leads the chunk ID,
// which ORDER BY already sorts ascending after score.
function contentRoleScoreSql(rawScore: string): string {
  return `(${rawScore})`;
}

const BM25_SCORE_SQL = `-bm25(chunks_fts, ${FTS5_WEIGHTS.join(", ")})`;

const SEARCH_SQL = `
SELECT
  c.chunk_id,
  c.vault_id,
  c.path,
  c.heading_path_json,
  c.frontmatter_json,
  ${contentRoleScoreSql(BM25_SCORE_SQL)} AS score
FROM chunks_fts
JOIN chunks AS c ON c.rowid = chunks_fts.rowid
JOIN sources AS s ON s.source_key = c.source_key
WHERE chunks_fts MATCH ?
ORDER BY score DESC, c.chunk_id ASC, c.path ASC
LIMIT ?
`;

const REQUIRED_IDENTIFIER_SEARCH_SQL = `
WITH eligible(rowid) AS (
  SELECT rowid
  FROM chunk_exact_identifier_fts
  WHERE chunk_exact_identifier_fts MATCH ?
)
SELECT
  c.chunk_id,
  c.vault_id,
  c.path,
  c.heading_path_json,
  c.frontmatter_json,
  ${contentRoleScoreSql(BM25_SCORE_SQL)} AS score
FROM chunks_fts
JOIN chunks AS c ON c.rowid = chunks_fts.rowid
JOIN sources AS s ON s.source_key = c.source_key
JOIN eligible ON eligible.rowid = c.rowid
WHERE chunks_fts MATCH ?
ORDER BY score DESC, c.chunk_id ASC, c.path ASC
LIMIT ?
`;

const IDENTIFIER_ONLY_SEARCH_SQL = `
WITH eligible(rowid) AS (
  SELECT rowid
  FROM chunk_exact_identifier_fts
  WHERE chunk_exact_identifier_fts MATCH ?
)
SELECT
  c.chunk_id,
  c.vault_id,
  c.path,
  c.heading_path_json,
  c.frontmatter_json,
  ${contentRoleScoreSql("5.0")} AS score
FROM eligible JOIN chunks AS c ON c.rowid = eligible.rowid
JOIN sources AS s ON s.source_key = c.source_key
ORDER BY score DESC, c.chunk_id ASC, c.path ASC
LIMIT ?
`;

const EXACT_CANDIDATES_SQL = `
  SELECT c.rowid, 12.0 AS score
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
  FROM exact JOIN source_exact_aliases AS alias ON alias.value = exact.value
  JOIN chunks AS c ON c.source_key = alias.source_key
  UNION ALL
  SELECT c.rowid, 3.0
  FROM exact JOIN chunks AS c ON c.exact_heading = exact.value
  UNION ALL
  SELECT rowid, 5.0
  FROM exact_identifier_matches
`;

const EXACT_SEARCH_SQL = `
WITH exact(value) AS (VALUES (?)),
exact_identifier_matches(rowid) AS (
  SELECT rowid
  FROM chunk_exact_identifier_fts
  WHERE chunk_exact_identifier_fts MATCH ?
),
candidates(rowid, score) AS (
${EXACT_CANDIDATES_SQL}
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
  ${contentRoleScoreSql("ranked.score")} AS score
FROM ranked JOIN chunks AS c ON c.rowid = ranked.rowid
JOIN sources AS s ON s.source_key = c.source_key
ORDER BY score DESC, c.chunk_id ASC, c.path ASC
LIMIT ?
`;

const REQUIRED_IDENTIFIER_EXACT_SEARCH_SQL = `
WITH exact(value) AS (VALUES (?)),
eligible(rowid) AS (
  SELECT rowid
  FROM chunk_exact_identifier_fts
  WHERE chunk_exact_identifier_fts MATCH ?
),
exact_identifier_matches(rowid) AS (
  SELECT rowid
  FROM chunk_exact_identifier_fts
  WHERE chunk_exact_identifier_fts MATCH ?
),
candidates(rowid, score) AS (
${EXACT_CANDIDATES_SQL}
),
ranked(rowid, score) AS (
  SELECT candidates.rowid, max(candidates.score)
  FROM candidates JOIN eligible ON eligible.rowid = candidates.rowid
  GROUP BY candidates.rowid
)
SELECT
  c.chunk_id,
  c.vault_id,
  c.path,
  c.heading_path_json,
  c.frontmatter_json,
  ${contentRoleScoreSql("ranked.score")} AS score
FROM ranked JOIN chunks AS c ON c.rowid = ranked.rowid
JOIN sources AS s ON s.source_key = c.source_key
ORDER BY score DESC, c.chunk_id ASC, c.path ASC
LIMIT ?
`;

const FTS_EXISTS_SQL = `
SELECT EXISTS(
  SELECT 1
  FROM chunks_fts
  WHERE chunks_fts MATCH ?
  LIMIT 1
)
`;

const EXACT_IDENTIFIER_EXISTS_SQL = `
SELECT EXISTS(
  SELECT 1
  FROM chunk_exact_identifier_fts
  WHERE chunk_exact_identifier_fts MATCH ?
  LIMIT 1
)
`;

const PREFIX_EXPANSIONS_SQL = `
SELECT term
FROM (
  SELECT term
  FROM (
    SELECT
      term,
      max(col IN ('filename', 'stem', 'aliases', 'title')) AS in_metadata
    FROM chunks_fts_vocab
    WHERE col IN ('filename', 'stem', 'aliases', 'title', 'heading_text', 'tags', 'content')
      AND term LIKE ? ESCAPE '\\'
      AND length(CAST(term AS blob)) <= ?
    GROUP BY term
    ORDER BY term ASC
    LIMIT ?
  )
  ORDER BY in_metadata DESC, length(CAST(term AS blob)) ASC, term ASC
  LIMIT ?
)
ORDER BY term ASC
`;

const MATCH_STAGE_IDS = new Set<StagePlan["plan_id"]>([
  "lexical_explicit_v3",
  "lexical_exact_phrase_v3",
  "lexical_all_terms_v3",
  "lexical_partial_coverage_v3",
  "lexical_prefix_metadata_v3",
  "lexical_prefix_v3",
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
  bind: readonly [string, number, number, number];
}

export interface BoundEvidenceProbe {
  exists: BoundExistsProbe;
  prefix: BoundPrefixProbe | null;
}

export function requireExecutionPlanIdentity(plan: ExecutionPlan): void {
  if (plan.schema_version !== 4
    || plan.profile_id !== FTS5_PROFILE_ID
    || plan.max_total_candidates !== 512
    || plan.stages.length > 6
    || plan.stages.some((stage, index) => stage.ordinal !== index)) {
    throw new Error("unsupported Rust FTS5 execution plan");
  }
  if (plan.disposition === "empty_no_evidence") {
    if (plan.stages.length !== 0) throw new Error("invalid empty FTS5 execution plan");
    return;
  }
  if (plan.disposition === "explicit_bypass") {
    if (plan.stages.length !== 1 || plan.stages[0]?.plan_id !== "lexical_explicit_v3") {
      throw new Error("invalid explicit FTS5 execution plan");
    }
    return;
  }
  if (plan.disposition !== "ready" || plan.stages.length === 0
    || plan.stages.some((stage) => stage.plan_id === "lexical_explicit_v3")) {
    throw new Error("invalid assisted FTS5 execution plan");
  }
}

export function bindSearchStage(stage: StagePlan, limit: number): BoundSearchStage {
  if (!isLimit(limit) || stage.max_candidates < limit) {
    throw new Error("invalid FTS5 stage search limit");
  }
  const required = requireIdentifiers(stage.required_identifiers);
  if (stage.plan_id === "lexical_exact_metadata_v3") {
    if (!isOpaqueUnicodeScalarValue(stage.exact_value, 4_096) || stage.match_value !== undefined) {
      throw new Error("unsupported Rust FTS5 exact stage");
    }
    const exactIdentifierToken = encodeExactIdentifierToken(stage.exact_value);
    return required.length === 0
      ? { sql: EXACT_SEARCH_SQL, bind: [stage.exact_value, exactIdentifierToken, limit] }
      : {
          sql: REQUIRED_IDENTIFIER_EXACT_SEARCH_SQL,
          bind: [
            stage.exact_value,
            encodeExactIdentifierMatch(required),
            exactIdentifierToken,
            limit,
          ],
        };
  }
  if (!MATCH_STAGE_IDS.has(stage.plan_id) || stage.exact_value !== undefined) {
    throw new Error("unsupported Rust FTS5 match stage");
  }
  if (stage.plan_id === "lexical_explicit_v3" && required.length !== 0) {
    throw new Error("unsupported Rust FTS5 explicit stage");
  }
  if (stage.match_value === undefined) {
    if (required.length === 0 || stage.plan_id === "lexical_explicit_v3") {
      throw new Error("unsupported Rust FTS5 match stage");
    }
    return {
      sql: IDENTIFIER_ONLY_SEARCH_SQL,
      bind: [encodeExactIdentifierMatch(required), limit],
    };
  }
  if (!isOpaqueValue(stage.match_value, 16_384)) {
    throw new Error("unsupported Rust FTS5 match stage");
  }
  return required.length === 0
    ? { sql: SEARCH_SQL, bind: [stage.match_value, limit] }
    : {
        sql: REQUIRED_IDENTIFIER_SEARCH_SQL,
        bind: [encodeExactIdentifierMatch(required), stage.match_value, limit],
      };
}

export function bindEvidenceProbe(plan: EvidenceProbePlan): BoundEvidenceProbe {
  if (plan.schema_version !== 4) {
    throw new Error("unsupported Rust FTS5 evidence probe");
  }
  if (plan.plan_id === "identifier_metadata_v3") {
    if (!isOpaqueValue(plan.match_value, 16_384)) {
      throw new Error("unsupported Rust FTS5 metadata probe");
    }
    return { exists: { sql: FTS_EXISTS_SQL, bind: [plan.match_value] }, prefix: null };
  }
  if (plan.plan_id !== "term_support_v3"
    || plan.max_prefix_expansions !== 16
    || plan.max_prefix_expansion_scan !== 256
    || plan.max_prefix_term_bytes !== 96
    || !Number.isSafeInteger(plan.probe_id)
    || !Number.isSafeInteger(plan.term_index)) {
    throw new Error("unsupported Rust FTS5 term probe");
  }
  const hasMatch = isOpaqueValue(plan.match_value, 16_384);
  const hasIdentifier = isOpaqueUnicodeScalarValue(plan.exact_identifier, 4_096);
  if (hasMatch === hasIdentifier) throw new Error("unsupported Rust FTS5 term probe");
  const prefix = plan.prefix_pattern === null
    ? null
    : hasMatch && isOpaqueValue(plan.prefix_pattern, 4_096)
      ? {
          sql: PREFIX_EXPANSIONS_SQL,
          bind: [
            plan.prefix_pattern,
            plan.max_prefix_term_bytes,
            plan.max_prefix_expansion_scan,
            plan.max_prefix_expansions,
          ] as const,
        }
      : null;
  if (plan.prefix_pattern !== null && prefix === null) {
    throw new Error("unsupported Rust FTS5 prefix probe");
  }
  return {
    exists: hasIdentifier
      ? {
          sql: EXACT_IDENTIFIER_EXISTS_SQL,
          bind: [encodeExactIdentifierToken(plan.exact_identifier as string)],
        }
      : { sql: FTS_EXISTS_SQL, bind: [plan.match_value as string] },
    prefix,
  };
}

function requireIdentifiers(value: unknown): string[] {
  if (value === undefined) return [];
  if (!Array.isArray(value)
    || value.length < 1
    || value.length > 128
    || !value.every((identifier) => isOpaqueUnicodeScalarValue(identifier, 4_096))
    || new Set(value).size !== value.length) {
    throw new Error("unsupported Rust FTS5 identifier constraints");
  }
  return value;
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

function isOpaqueUnicodeScalarValue(value: unknown, maximumBytes: number): value is string {
  return typeof value === "string"
    && value.length > 0
    && new TextEncoder().encode(value).byteLength <= maximumBytes
    && !value.includes("\0");
}
