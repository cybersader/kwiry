// SPDX-FileCopyrightText: 2026 cybersader
// SPDX-License-Identifier: GPL-3.0-only

import {
  finalize_typo_suggestion_probe,
  prepare_typo_suggestion_probe,
} from "virtual:kwiry-rust-wasm-bindings";

import type { Fts5GenerationIndex, SQLiteDatabase } from "./fts5-index";
import {
  MAX_QUERY_CHARACTERS,
  MAX_SEARCH_HITS,
  WORKER_PROTOCOL_VERSION,
  fixedWorkerError,
  type SearchResult,
  type WorkerError,
} from "./protocol";

const ABI_VERSION = 2;
const PREFIX_LIMITATION = "Bounded prefix vocabulary only considers terms sharing the first four ASCII characters; it cannot catch early-character errors such as rettrieval." as const;
const BOUNDS = Object.freeze({
  prefix_chars: 4,
  max_term_bytes: 48,
  max_vocabulary_candidates: 40,
  max_candidate_bytes: 96,
  max_edit_distance: 1,
  max_output_suggestions: 1,
  max_work_units: 3_840,
} as const);
const VOCABULARY_SQL = `
SELECT term, SUM(doc) AS document_frequency
FROM chunks_fts_vocab
WHERE col IN ('filename', 'stem', 'aliases', 'title', 'heading_text', 'content')
  AND term LIKE ? ESCAPE '\\'
  AND length(CAST(term AS blob)) <= ?
GROUP BY term
ORDER BY document_frequency DESC, term ASC
LIMIT ?
`;

interface SuggestionPlan {
  schema_version: 1;
  disposition: "probe" | "explicit_syntax_bypass" | "ineligible";
  query: string;
  term: string | null;
  prefix_pattern: string | null;
  bounds: typeof BOUNDS;
  limitation: typeof PREFIX_LIMITATION;
}

interface VocabularyCandidate {
  term: string;
  document_frequency: number;
}

interface SuggestionResult {
  schema_version: 1;
  disposition: "explicit_syntax_bypass" | "ineligible" | "suggestion" | "no_candidate";
  original_query: string;
  suggested_query: string | null;
  candidates_examined: number;
  work_units: number;
  bounds: typeof BOUNDS;
  limitation: typeof PREFIX_LIMITATION;
}

interface PrototypeResult {
  schema_version: 1;
  generation: string;
  disposition:
    | "literal_results"
    | "explicit_syntax_bypass"
    | "ineligible"
    | "suggestion"
    | "no_candidate";
  literal_hits: SearchResult["hits"];
  suggested_query: string | null;
  candidates_examined: number;
  work_units: number;
  bounds: typeof BOUNDS;
  limitation: typeof PREFIX_LIMITATION;
  total_duration_ms: number;
  vocabulary_duration_ms: number;
}

interface PrototypeContext {
  scope: DedicatedWorkerGlobalScope;
  getActive(): { id: string; index: Fts5GenerationIndex } | null;
  requireInitialized(): void;
  search(query: string, limit: number): SearchResult;
  getLastRequestId(): number;
  setLastRequestId(id: number): void;
  mapError(error: unknown): WorkerError;
}

export function createInternalPrototypeHandler(
  context: PrototypeContext,
): (value: unknown) => Promise<boolean> {
  return async (value: unknown): Promise<boolean> => {
    if (!isRecord(value) || value.operation !== "internal_typo_prototype") return false;
    const keys = Object.keys(value);
    const valid = keys.length === 4
      && keys.every((key) => ["version", "id", "operation", "query"].includes(key))
      && value.version === WORKER_PROTOCOL_VERSION
      && Number.isSafeInteger(value.id)
      && Number(value.id) > 0
      && typeof value.query === "string"
      && value.query.trim().length > 0
      && value.query.length <= MAX_QUERY_CHARACTERS;
    const id = Number.isSafeInteger(value.id) && Number(value.id) > 0 ? Number(value.id) : 1;
    if (!valid || id <= context.getLastRequestId()) {
      context.scope.postMessage({
        version: WORKER_PROTOCOL_VERSION,
        id,
        operation: "internal_typo_prototype",
        ok: false,
        error: fixedWorkerError("invalid_request", "protocol", "Invalid Worker request.", false),
      });
      return true;
    }
    context.setLastRequestId(id);
    try {
      context.scope.postMessage({
        version: WORKER_PROTOCOL_VERSION,
        id,
        operation: "internal_typo_prototype",
        ok: true,
        result: runPrototype(String(value.query), context),
      });
    } catch (error) {
      context.scope.postMessage({
        version: WORKER_PROTOCOL_VERSION,
        id,
        operation: "internal_typo_prototype",
        ok: false,
        error: context.mapError(error),
      });
    }
    return true;
  };
}

function runPrototype(query: string, context: PrototypeContext): PrototypeResult {
  context.requireInitialized();
  const active = context.getActive();
  if (active === null) {
    throw fixedWorkerError(
      "index_building",
      "query",
      "In-plugin lexical index is not ready.",
      true,
    );
  }
  const started = performance.now();
  const plan = prepareSuggestion(query);
  const base = {
    schema_version: 1 as const,
    generation: active.id,
    literal_hits: [] as SearchResult["hits"],
    suggested_query: null,
    candidates_examined: 0,
    work_units: 0,
    bounds: plan.bounds,
    limitation: plan.limitation,
    total_duration_ms: 0,
    vocabulary_duration_ms: 0,
  };
  if (plan.disposition === "explicit_syntax_bypass" || plan.disposition === "ineligible") {
    return {
      ...base,
      disposition: plan.disposition,
      total_duration_ms: elapsedMilliseconds(started),
    };
  }

  const literal = context.search(query, MAX_SEARCH_HITS);
  if (literal.hits.length > 0) {
    return {
      ...base,
      disposition: "literal_results",
      literal_hits: literal.hits,
      total_duration_ms: elapsedMilliseconds(started),
    };
  }

  const vocabularyStarted = performance.now();
  const candidates = vocabulary(active.index, plan);
  const suggestion = finalizeSuggestion(query, candidates);
  return {
    ...base,
    disposition: suggestion.disposition,
    suggested_query: suggestion.suggested_query,
    candidates_examined: suggestion.candidates_examined,
    work_units: suggestion.work_units,
    bounds: suggestion.bounds,
    limitation: suggestion.limitation,
    total_duration_ms: elapsedMilliseconds(started),
    vocabulary_duration_ms: elapsedMilliseconds(vocabularyStarted),
  };
}

function prepareSuggestion(query: string): SuggestionPlan {
  const response = parseResponse(prepare_typo_suggestion_probe(JSON.stringify({
    abi_version: ABI_VERSION,
    operation: "prepare_typo_suggestion",
    query,
  })), "prepare_typo_suggestion");
  if (!isRecord(response) || !hasExactKeys(response, ["plan"]) || !isSuggestionPlan(response.plan)) {
    throw new Error("Portable Rust returned invalid prototype data.");
  }
  return response.plan;
}

function finalizeSuggestion(
  query: string,
  candidates: readonly VocabularyCandidate[],
): SuggestionResult {
  const response = parseResponse(finalize_typo_suggestion_probe(JSON.stringify({
    abi_version: ABI_VERSION,
    operation: "finalize_typo_suggestion",
    query,
    candidates,
  })), "finalize_typo_suggestion");
  if (!isRecord(response)
    || !hasExactKeys(response, ["suggestion"])
    || !isSuggestionResult(response.suggestion)) {
    throw new Error("Portable Rust returned invalid prototype data.");
  }
  return response.suggestion;
}

function vocabulary(index: Fts5GenerationIndex, plan: SuggestionPlan): VocabularyCandidate[] {
  if (plan.disposition !== "probe" || plan.prefix_pattern === null || plan.term === null) {
    throw new Error("invalid internal suggestion plan");
  }
  const db = (index as unknown as { db: SQLiteDatabase }).db;
  const rows = db.selectObjects(VOCABULARY_SQL, [
    plan.prefix_pattern,
    plan.bounds.max_candidate_bytes,
    plan.bounds.max_vocabulary_candidates,
  ]);
  if (rows.length > plan.bounds.max_vocabulary_candidates) {
    throw new Error("prototype vocabulary exceeded its candidate bound");
  }
  let bytes = 0;
  return rows.map((row) => {
    if (typeof row.term !== "string"
      || !/^[a-z]{1,96}$/u.test(row.term)
      || !Number.isSafeInteger(row.document_frequency)
      || Number(row.document_frequency) < 1) {
      throw new Error("SQLite returned an invalid prototype candidate");
    }
    bytes += new TextEncoder().encode(row.term).byteLength;
    if (bytes > plan.bounds.max_vocabulary_candidates * plan.bounds.max_candidate_bytes) {
      throw new Error("prototype vocabulary exceeded its memory bound");
    }
    return { term: row.term, document_frequency: Number(row.document_frequency) };
  });
}

function parseResponse(source: string, operation: string): unknown {
  let value: unknown;
  try {
    value = JSON.parse(source);
  } catch {
    throw new Error("Portable Rust returned invalid prototype JSON.");
  }
  if (!isRecord(value)
    || value.abi_version !== ABI_VERSION
    || value.operation !== operation
    || value.status !== "ok"
    || !("result" in value)) {
    throw new Error("Portable Rust returned an invalid prototype response.");
  }
  return value.result;
}

function isSuggestionPlan(value: unknown): value is SuggestionPlan {
  if (!isRecord(value)
    || !hasExactKeys(value, [
      "schema_version", "disposition", "query", "term", "prefix_pattern", "bounds", "limitation",
    ])
    || value.schema_version !== 1
    || (value.disposition !== "probe"
      && value.disposition !== "explicit_syntax_bypass"
      && value.disposition !== "ineligible")
    || !isBoundedString(value.query, 4_096)
    || !isBounds(value.bounds)
    || value.limitation !== PREFIX_LIMITATION) {
    return false;
  }
  if (value.disposition !== "probe") return value.term === null && value.prefix_pattern === null;
  return typeof value.term === "string"
    && /^[a-z]{5,48}$/u.test(value.term)
    && typeof value.prefix_pattern === "string"
    && /^[a-z]{4}%$/u.test(value.prefix_pattern)
    && value.term.startsWith(value.prefix_pattern.slice(0, -1));
}

function isSuggestionResult(value: unknown): value is SuggestionResult {
  if (!isRecord(value)
    || !hasExactKeys(value, [
      "schema_version", "disposition", "original_query", "suggested_query",
      "candidates_examined", "work_units", "bounds", "limitation",
    ])
    || value.schema_version !== 1
    || (value.disposition !== "explicit_syntax_bypass"
      && value.disposition !== "ineligible"
      && value.disposition !== "suggestion"
      && value.disposition !== "no_candidate")
    || !isBoundedString(value.original_query, 4_096)
    || !isNonNegativeInteger(value.candidates_examined)
    || value.candidates_examined > BOUNDS.max_vocabulary_candidates
    || !isNonNegativeInteger(value.work_units)
    || value.work_units > BOUNDS.max_work_units
    || !isBounds(value.bounds)
    || value.limitation !== PREFIX_LIMITATION) {
    return false;
  }
  return value.disposition === "suggestion"
    ? typeof value.suggested_query === "string" && /^[a-z]{1,96}$/u.test(value.suggested_query)
    : value.suggested_query === null;
}

function isBounds(value: unknown): value is typeof BOUNDS {
  return isRecord(value)
    && hasExactKeys(value, Object.keys(BOUNDS))
    && Object.entries(BOUNDS).every(([key, expected]) => value[key] === expected);
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

function isNonNegativeInteger(value: unknown): value is number {
  return typeof value === "number" && Number.isSafeInteger(value) && value >= 0;
}

function elapsedMilliseconds(started: number): number {
  return Math.round(Math.max(0, performance.now() - started) * 1_000) / 1_000;
}
