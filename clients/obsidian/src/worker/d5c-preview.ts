// SPDX-FileCopyrightText: 2026 cybersader
// SPDX-License-Identifier: GPL-3.0-only

import {
  finalize_d5c_preview,
  prepare_d5c_preview,
} from "virtual:kwiry-rust-wasm-bindings";

import {
  D5C_COMPARE_SCHEMA_VERSION,
  INTERNAL_D5C_COMPARE_OPERATION,
  isD5cCompareRequest,
  type D5cCompareRequest,
  type D5cCompareResult,
} from "./d5c-compare-protocol";
import type { Fts5GenerationIndex, SQLiteDatabase } from "./fts5-index";
import {
  WORKER_PROTOCOL_VERSION,
  fixedWorkerError,
  type WorkerError,
  type WorkerSearchHit,
} from "./protocol";
import {
  finalizeQueryWithRust,
  prepareQueryWithRust,
  type ExecutionPlan,
  type StagePlan,
} from "./rust-adapter";

const ABI_VERSION = 2;
const AUTHORIZATION_SCOPE = "obsidian-active-vault" as const;
const BALANCED_PROFILE = Object.freeze({
  schema_version: 1,
  profile_id: "d5c-preview-v1",
  retrieval_profile_id: "lexical-v1",
  recency: Object.freeze({
    id: "00-balanced-recency",
    clock: "source_mtime",
    horizon: "quarter",
    strength: "low",
  }),
  hierarchy: Object.freeze({
    authority_folders: Object.freeze([
      Object.freeze({ id: "10-authority", prefix: "reference", strength: "standard" }),
    ]),
    archive_folders: Object.freeze([
      Object.freeze({ id: "20-archive", prefix: "archive", strength: "standard" }),
    ]),
  }),
  property_rules: Object.freeze([]),
});
const MAX_PROFILE_BYTES = 64 * 1024;
const MAX_ADAPTER_REQUEST_BYTES = 64 * 1024 * 1024;
const MAX_HYDRATED_SIGNAL_BYTES = MAX_ADAPTER_REQUEST_BYTES / 4;
const U64_MAX = 18_446_744_073_709_551_615n;

const CANDIDATE_IDENTITIES_SQL = `
WITH requested(ordinal, chunk_id) AS (
  SELECT
    CAST(json_extract(value, '$.ordinal') AS INTEGER),
    json_extract(value, '$.chunk_id')
  FROM json_each(?)
)
SELECT requested.ordinal, c.source_key, c.chunk_id, c.vault_id, c.path, s.mtime_nanos
FROM requested
JOIN chunks AS c ON c.chunk_id = requested.chunk_id
JOIN sources AS s ON s.source_key = c.source_key
WHERE c.vault_id = ? AND s.vault_id = ?
ORDER BY requested.ordinal
LIMIT ?
`;

const SOURCE_SIGNAL_SQL = `
WITH requested_property(property_name) AS (SELECT value FROM json_each(?))
SELECT
  p.property_name,
  scalar.json_pointer,
  scalar.scalar_type,
  scalar.exact_value
FROM source_properties AS p
JOIN requested_property ON requested_property.property_name = p.property_name
LEFT JOIN source_property_scalars AS scalar
  ON scalar.source_key = p.source_key AND scalar.property_name = p.property_name
WHERE p.source_key = ?
ORDER BY p.property_name, scalar.json_pointer
LIMIT ?
`;

type EvidenceTier =
  | "explicit"
  | "exact_metadata"
  | "exact_phrase"
  | "all_terms"
  | "partial_coverage"
  | "prefix";

type RankingScalar =
  | { type: "null" }
  | { type: "boolean"; value: boolean }
  | { type: "i64" | "u64" | "f64" | "string" | "date"; value: string };

interface SignalPlan {
  schema_version: 1;
  requires_source_mtime: boolean;
  property_names: string[];
  max_candidates: 512;
  max_candidates_per_stage: 256;
  max_property_values_per_source: 256;
}

interface InternalCandidate {
  hit: WorkerSearchHit;
  source_key: string;
  mtime_nanos: string;
  evidence_tier: EvidenceTier;
  lexical_ordinal: number;
}

interface CandidateWire {
  source: { authorization_scope: typeof AUTHORIZATION_SCOPE; source_key: string };
  chunk_id: string;
  path: string;
  evidence_tier: EvidenceTier;
  lexical_score: number;
  lexical_ordinal: number;
}

interface SourceSignalWire {
  source: CandidateWire["source"];
  source_mtime_epoch_seconds?: string;
  present_properties: string[];
  property_values: Array<{ property: string; pointer: string; value: RankingScalar }>;
}

interface RankingEvidence {
  schema_version: 1;
  candidate_count: number;
  source_count: number;
  entries: Array<{ tier: EvidenceTier; ordinal: number; points: number }>;
}

interface ComparisonTarget {
  id: string;
  index: Fts5GenerationIndex;
  publication: "active" | "initial_staging";
  revision: number | null;
}

interface PreviewContext {
  scope: DedicatedWorkerGlobalScope;
  resolveTarget(generation: string, revision: number | null): ComparisonTarget | null;
  getInitializedVaultId(): string;
  requireInitialized(): void;
  getLastRequestId(): number;
  setLastRequestId(id: number): void;
  mapError(error: unknown): WorkerError;
}

class D5cPreviewFailure extends Error {
  constructor(public readonly code: string) {
    super("Internal D5C preview failed.");
    this.name = "D5cPreviewFailure";
  }
}

export function createInternalD5cPreviewHandler(
  context: PreviewContext,
): (value: unknown) => Promise<boolean> {
  return async (value: unknown): Promise<boolean> => {
    if (!isRecord(value) || value.operation !== INTERNAL_D5C_COMPARE_OPERATION) return false;
    const id = Number.isSafeInteger(value.id) && Number(value.id) > 0 ? Number(value.id) : 1;
    if (!isD5cCompareRequest(value) || id <= context.getLastRequestId()) {
      context.scope.postMessage({
        version: WORKER_PROTOCOL_VERSION,
        id,
        operation: INTERNAL_D5C_COMPARE_OPERATION,
        ok: false,
        error: fixedWorkerError("invalid_request", "protocol", "Invalid Worker request.", false),
      });
      return true;
    }
    context.setLastRequestId(id);
    try {
      const result = runComparison(value, context);
      context.scope.postMessage({
        version: WORKER_PROTOCOL_VERSION,
        id,
        operation: INTERNAL_D5C_COMPARE_OPERATION,
        ok: true,
        result,
      });
    } catch (error) {
      const mapped = error instanceof D5cPreviewFailure
        ? { code: error.code, stage: "query", message: error.message, retryable: false }
        : context.mapError(error);
      context.scope.postMessage({
        version: WORKER_PROTOCOL_VERSION,
        id,
        operation: INTERNAL_D5C_COMPARE_OPERATION,
        ok: false,
        error: mapped,
      });
    }
    return true;
  };
}

function runComparison(
  request: D5cCompareRequest,
  context: PreviewContext,
): D5cCompareResult {
  context.requireInitialized();
  const authorizedVaultId = context.getInitializedVaultId();
  const target = context.resolveTarget(request.generation, request.revision);
  if (target === null) throw new D5cPreviewFailure("index_changed");

  const signalPlan = prepareSignalPlan(BALANCED_PROFILE);
  const prepared = prepareQueryWithRust(request.query);
  const observation = target.index.observeQuery(prepared.probes);
  const finalized = finalizeQueryWithRust(request.query, observation);
  const candidates = collectCandidates(target.index, finalized.execution_plan, signalPlan);
  if (candidates.some((candidate) => candidate.hit.vault_id !== authorizedVaultId)) {
    throw new D5cPreviewFailure("authorization_refused");
  }
  hydrateCandidateIdentities(target.index, candidates, authorizedVaultId, signalPlan);
  const sourceSignals = hydrateSourceSignals(target.index, candidates, signalPlan);
  const finalizedRanking = finalizeRanking(
    BALANCED_PROFILE,
    request.query_time_epoch_seconds,
    candidates,
    sourceSignals,
  );
  const displayedCount = Math.min(request.limit, candidates.length);
  const textOrder = candidates.slice(0, displayedCount).map((candidate) => candidate.lexical_ordinal);
  const balancedOrder = finalizedRanking.ordered_candidate_ordinals.slice(0, displayedCount);
  if (balancedOrder.length !== displayedCount) {
    throw new D5cPreviewFailure("invalid_rerank_result");
  }
  const displayedOrdinals = [...new Set([...textOrder, ...balancedOrder])].sort((left, right) =>
    left - right);
  const displayCandidates = displayedOrdinals.map((ordinal) => {
    const candidate = candidates[ordinal];
    if (!candidate) throw new D5cPreviewFailure("invalid_rerank_result");
    return {
      ordinal,
      hit: {
        path: candidate.hit.path,
        heading_path: candidate.hit.heading_path,
        frontmatter: candidate.hit.frontmatter,
      },
    };
  });
  const textRanks = new Map(textOrder.map((ordinal, rank) => [ordinal, rank]));
  const balancedRanks = new Map(balancedOrder.map((ordinal, rank) => [ordinal, rank]));
  const movedCandidateCount = displayedOrdinals.filter((ordinal) =>
    textRanks.get(ordinal) !== balancedRanks.get(ordinal)).length;
  const topNOverlap = textOrder.filter((ordinal) => balancedRanks.has(ordinal)).length;
  return {
    schema_version: D5C_COMPARE_SCHEMA_VERSION,
    generation: target.id,
    publication: target.publication,
    revision: target.revision,
    candidate_pool_count: candidates.length,
    display_candidates: displayCandidates,
    text_order: textOrder,
    balanced_order: balancedOrder,
    aggregate: {
      moved_candidate_count: movedCandidateCount,
      top_n_overlap: topNOverlap,
    },
  };
}

function prepareSignalPlan(profile: unknown): SignalPlan {
  const result = parseAdapterResponse(prepare_d5c_preview(stringifyBounded({
    abi_version: ABI_VERSION,
    operation: "prepare_d5c_preview",
    profile,
  }, MAX_PROFILE_BYTES + 1_024)), "prepare_d5c_preview");
  if (!isRecord(result) || !hasExactKeys(result, ["signal_plan"]) || !isSignalPlan(result.signal_plan)) {
    throw new D5cPreviewFailure("invalid_preview_plan");
  }
  return result.signal_plan;
}

function collectCandidates(
  index: Fts5GenerationIndex,
  plan: ExecutionPlan,
  signalPlan: SignalPlan,
): InternalCandidate[] {
  if (plan.disposition === "empty_no_evidence") return [];
  const candidates: InternalCandidate[] = [];
  const seen = new Set<string>();
  let collectedRows = 0;
  for (const stage of plan.stages) {
    if (candidates.length === signalPlan.max_candidates
      || collectedRows === signalPlan.max_candidates) break;
    const stageLimit = Math.min(
      stage.max_candidates,
      signalPlan.max_candidates_per_stage,
      signalPlan.max_candidates - collectedRows,
    );
    if (stageLimit < 1) break;
    const stagePlan = singleStagePlan(plan, stage, stageLimit);
    const rows = index.search(stagePlan, stageLimit);
    collectedRows += rows.length;
    for (const hit of rows) {
      const identity = JSON.stringify([hit.vault_id, hit.chunk_id, hit.path]);
      if (seen.has(identity)) continue;
      seen.add(identity);
      candidates.push({
        hit,
        source_key: "",
        mtime_nanos: "",
        evidence_tier: evidenceTier(stage),
        lexical_ordinal: candidates.length,
      });
      if (candidates.length === signalPlan.max_candidates) break;
    }
  }
  return candidates;
}

function singleStagePlan(
  plan: ExecutionPlan,
  stage: StagePlan,
  limit: number,
): ExecutionPlan {
  return {
    schema_version: 3,
    profile_id: "lexical-v1",
    disposition: plan.disposition === "explicit_bypass" ? "explicit_bypass" : "ready",
    max_total_candidates: 512,
    stages: [{ ...stage, ordinal: 0, max_candidates: limit }],
  };
}

function evidenceTier(stage: StagePlan): EvidenceTier {
  switch (stage.plan_id) {
    case "lexical_explicit_v3": return "explicit";
    case "lexical_exact_metadata_v3": return "exact_metadata";
    case "lexical_exact_phrase_v3": return "exact_phrase";
    case "lexical_all_terms_v3": return "all_terms";
    case "lexical_partial_coverage_v3": return "partial_coverage";
    case "lexical_prefix_v3": return "prefix";
  }
}

function hydrateCandidateIdentities(
  index: Fts5GenerationIndex,
  candidates: InternalCandidate[],
  vaultId: string,
  signalPlan: SignalPlan,
): void {
  if (candidates.length === 0) return;
  const db = database(index);
  const requested = candidates.map((candidate) => ({
    ordinal: candidate.lexical_ordinal,
    chunk_id: candidate.hit.chunk_id,
  }));
  const rows = db.selectObjects(CANDIDATE_IDENTITIES_SQL, [
    JSON.stringify(requested),
    vaultId,
    vaultId,
    signalPlan.max_candidates + 1,
  ]);
  if (rows.length !== candidates.length || rows.length > signalPlan.max_candidates) {
    throw new D5cPreviewFailure("incomplete_rerank_input");
  }
  for (let ordinal = 0; ordinal < rows.length; ordinal += 1) {
    const row = rows[ordinal];
    const candidate = candidates[ordinal];
    if (!row || !candidate
      || row.ordinal !== ordinal
      || row.chunk_id !== candidate.hit.chunk_id
      || row.vault_id !== vaultId
      || row.path !== candidate.hit.path
      || !isBoundedString(row.source_key, 256)
      || typeof row.mtime_nanos !== "string"
      || !/^[0-9]{1,39}$/u.test(row.mtime_nanos)) {
      throw new D5cPreviewFailure("incomplete_rerank_input");
    }
    candidate.source_key = row.source_key;
    candidate.mtime_nanos = row.mtime_nanos;
  }
}

function hydrateSourceSignals(
  index: Fts5GenerationIndex,
  candidates: readonly InternalCandidate[],
  signalPlan: SignalPlan,
): SourceSignalWire[] {
  const sources = new Map<string, { source_key: string; mtime_nanos: string }>();
  for (const candidate of candidates) {
    const existing = sources.get(candidate.source_key);
    if (existing && existing.mtime_nanos !== candidate.mtime_nanos) {
      throw new D5cPreviewFailure("incomplete_rerank_input");
    }
    sources.set(candidate.source_key, {
      source_key: candidate.source_key,
      mtime_nanos: candidate.mtime_nanos,
    });
  }
  const propertyNamesJson = JSON.stringify(signalPlan.property_names);
  const db = database(index);
  const observations: SourceSignalWire[] = [];
  let hydratedSignalBytes = 2;
  for (const source of [...sources.values()].sort((left, right) =>
    left.source_key < right.source_key ? -1 : left.source_key > right.source_key ? 1 : 0)) {
    const rows = signalPlan.property_names.length === 0
      ? []
      : db.selectObjects(SOURCE_SIGNAL_SQL, [
          propertyNamesJson,
          source.source_key,
          signalPlan.max_property_values_per_source + signalPlan.property_names.length + 1,
        ]);
    const present = new Set<string>();
    const propertyValues: SourceSignalWire["property_values"] = [];
    for (const row of rows) {
      if (!isBoundedString(row.property_name, 256)
        || !signalPlan.property_names.includes(row.property_name)) {
        throw new D5cPreviewFailure("invalid_rerank_input");
      }
      present.add(row.property_name);
      if (row.json_pointer === null
        && row.scalar_type === null
        && row.exact_value === null) continue;
      if (typeof row.json_pointer !== "string" || row.json_pointer.length > 1_024) {
        throw new D5cPreviewFailure("invalid_rerank_input");
      }
      propertyValues.push({
        property: row.property_name,
        pointer: row.json_pointer,
        value: rankingScalar(row.scalar_type, row.exact_value),
      });
      if (propertyValues.length > signalPlan.max_property_values_per_source) {
        throw new D5cPreviewFailure("ranking_work_limit_exceeded");
      }
    }
    const observation: SourceSignalWire = {
      source: { authorization_scope: AUTHORIZATION_SCOPE, source_key: source.source_key },
      present_properties: [...present].sort(),
      property_values: propertyValues,
    };
    if (signalPlan.requires_source_mtime) {
      observation.source_mtime_epoch_seconds = epochSeconds(source.mtime_nanos);
    }
    hydratedSignalBytes = chargeHydratedSignalBytes(hydratedSignalBytes, observation);
    observations.push(observation);
  }
  return observations;
}

function chargeHydratedSignalBytes(current: number, observation: SourceSignalWire): number {
  let serialized: string;
  try {
    serialized = JSON.stringify(observation);
  } catch {
    throw new D5cPreviewFailure("invalid_rerank_input");
  }
  const next = current + new TextEncoder().encode(serialized).byteLength + 1;
  if (next > MAX_HYDRATED_SIGNAL_BYTES) {
    throw new D5cPreviewFailure("ranking_work_limit_exceeded");
  }
  return next;
}

function rankingScalar(type: unknown, exactValue: unknown): RankingScalar {
  if (typeof type !== "string" || typeof exactValue !== "string") {
    throw new D5cPreviewFailure("invalid_rerank_input");
  }
  switch (type) {
    case "null":
      if (exactValue !== "null") throw new D5cPreviewFailure("invalid_rerank_input");
      return { type: "null" };
    case "boolean":
      if (exactValue === "true") return { type: "boolean", value: true };
      if (exactValue === "false") return { type: "boolean", value: false };
      throw new D5cPreviewFailure("invalid_rerank_input");
    case "i64":
    case "u64":
      if (!/^(0|-?[1-9][0-9]*)$/u.test(exactValue)) {
        throw new D5cPreviewFailure("invalid_rerank_input");
      }
      return { type, value: exactValue };
    case "real":
      if (!/^[0-9a-f]{16}$/u.test(exactValue)) {
        throw new D5cPreviewFailure("invalid_rerank_input");
      }
      return { type: "f64", value: exactValue };
    case "string":
    case "date":
      if (new TextEncoder().encode(exactValue).byteLength > 4_096) {
        throw new D5cPreviewFailure("invalid_rerank_input");
      }
      return { type, value: exactValue };
    default:
      throw new D5cPreviewFailure("invalid_rerank_input");
  }
}

function epochSeconds(mtimeNanos: string): string {
  const nanos = BigInt(mtimeNanos);
  const seconds = nanos / 1_000_000_000n;
  if (seconds > U64_MAX) throw new D5cPreviewFailure("invalid_rerank_input");
  return seconds.toString();
}

function finalizeRanking(
  profile: unknown,
  queryTimeEpochSeconds: string,
  candidates: readonly InternalCandidate[],
  sourceSignals: readonly SourceSignalWire[],
): { ordered_candidate_ordinals: number[]; evidence: RankingEvidence } {
  const candidateWires: CandidateWire[] = candidates.map((candidate) => ({
    source: { authorization_scope: AUTHORIZATION_SCOPE, source_key: candidate.source_key },
    chunk_id: candidate.hit.chunk_id,
    path: candidate.hit.path,
    evidence_tier: candidate.evidence_tier,
    lexical_score: candidate.hit.score,
    lexical_ordinal: candidate.lexical_ordinal,
  }));
  const result = parseAdapterResponse(finalize_d5c_preview(stringifyBounded({
    abi_version: ABI_VERSION,
    operation: "finalize_d5c_preview",
    profile,
    query_time_epoch_seconds: queryTimeEpochSeconds,
    candidates: candidateWires,
    source_signals: sourceSignals,
  }, MAX_ADAPTER_REQUEST_BYTES)), "finalize_d5c_preview");
  if (!isFinalizedRanking(result, candidateWires)) {
    throw new D5cPreviewFailure("invalid_rerank_result");
  }
  return result;
}

function parseAdapterResponse(source: string, operation: string): unknown {
  let value: unknown;
  try {
    value = JSON.parse(source);
  } catch {
    throw new D5cPreviewFailure("invalid_adapter_response");
  }
  if (!isRecord(value)
    || value.abi_version !== ABI_VERSION
    || value.operation !== operation
    || (value.status !== "ok" && value.status !== "error")) {
    throw new D5cPreviewFailure("invalid_adapter_response");
  }
  if (value.status === "error") {
    if (!isRecord(value.error) || typeof value.error.code !== "string") {
      throw new D5cPreviewFailure("invalid_adapter_response");
    }
    throw new D5cPreviewFailure(value.error.code);
  }
  if (!("result" in value)) throw new D5cPreviewFailure("invalid_adapter_response");
  return value.result;
}

function stringifyBounded(value: unknown, maximumBytes: number): string {
  let serialized: string;
  try {
    serialized = JSON.stringify(value);
  } catch {
    throw new D5cPreviewFailure("invalid_request");
  }
  if (new TextEncoder().encode(serialized).byteLength > maximumBytes) {
    throw new D5cPreviewFailure("invalid_request");
  }
  return serialized;
}

function isSignalPlan(value: unknown): value is SignalPlan {
  if (!isRecord(value)
    || !hasExactKeys(value, [
      "schema_version", "requires_source_mtime", "property_names", "max_candidates",
      "max_candidates_per_stage", "max_property_values_per_source",
    ])
    || value.schema_version !== 1
    || typeof value.requires_source_mtime !== "boolean"
    || !Array.isArray(value.property_names)
    || value.property_names.length > 8
    || value.max_candidates !== 512
    || value.max_candidates_per_stage !== 256
    || value.max_property_values_per_source !== 256) {
    return false;
  }
  const names = value.property_names;
  return names.every((name, index) => isBoundedString(name, 256)
    && (index === 0 || String(names[index - 1]) < name));
}

function isFinalizedRanking(
  value: unknown,
  candidates: readonly CandidateWire[],
): value is { ordered_candidate_ordinals: number[]; evidence: RankingEvidence } {
  if (!isRecord(value)
    || !hasExactKeys(value, ["ordered_candidate_ordinals", "evidence"])
    || !Array.isArray(value.ordered_candidate_ordinals)
    || value.ordered_candidate_ordinals.length !== candidates.length
    || !isRankingEvidence(value.evidence, candidates)) {
    return false;
  }
  const ordinals = value.ordered_candidate_ordinals;
  return ordinals.every((ordinal) => Number.isSafeInteger(ordinal)
    && Number(ordinal) >= 0
    && Number(ordinal) < candidates.length)
    && new Set(ordinals).size === candidates.length
    && value.evidence.entries.every((entry, index) =>
      entry.ordinal === ordinals[index]
      && entry.tier === candidates[entry.ordinal]?.evidence_tier);
}

function isRankingEvidence(value: unknown, candidates: readonly CandidateWire[]): value is RankingEvidence {
  if (!isRecord(value)
    || !hasExactKeys(value, ["schema_version", "candidate_count", "source_count", "entries"])
    || value.schema_version !== 1
    || value.candidate_count !== candidates.length
    || !Number.isSafeInteger(value.source_count)
    || Number(value.source_count) < 0
    || Number(value.source_count) > candidates.length
    || !Array.isArray(value.entries)
    || value.entries.length !== candidates.length) {
    return false;
  }
  return value.entries.every((entry) => isRecord(entry)
    && hasExactKeys(entry, ["tier", "ordinal", "points"])
    && isEvidenceTier(entry.tier)
    && Number.isSafeInteger(entry.ordinal)
    && Number(entry.ordinal) >= 0
    && Number(entry.ordinal) < candidates.length
    && Number.isSafeInteger(entry.points)
    && Math.abs(Number(entry.points)) <= 32);
}

function isEvidenceTier(value: unknown): value is EvidenceTier {
  return value === "explicit"
    || value === "exact_metadata"
    || value === "exact_phrase"
    || value === "all_terms"
    || value === "partial_coverage"
    || value === "prefix";
}

function database(index: Fts5GenerationIndex): SQLiteDatabase {
  return (index as unknown as { db: SQLiteDatabase }).db;
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function hasExactKeys(value: Record<string, unknown>, keys: readonly string[]): boolean {
  const actual = Object.keys(value);
  return actual.length === keys.length && actual.every((key) => keys.includes(key));
}

function isBoundedString(value: unknown, maximum: number): value is string {
  return typeof value === "string" && value.length > 0 && value.length <= maximum;
}
