// SPDX-FileCopyrightText: 2026 cybersader
// SPDX-License-Identifier: GPL-3.0-only

import {
  MAX_QUERY_CHARACTERS,
  MAX_SEARCH_HITS,
  WORKER_PROTOCOL_VERSION,
  type WorkerError,
  type WorkerFrontmatter,
} from "./protocol";

export const INTERNAL_D5C_COMPARE_OPERATION = "internal_d5c_compare" as const;
export const D5C_COMPARE_SCHEMA_VERSION = 2 as const;
const MAX_PATH_CHARACTERS = 16_384;
const MAX_HEADING_DEPTH = 64;
const MAX_HEADING_CHARACTERS = 4_096;
const MAX_TITLE_BYTES = 1_048_576;
const U64_MAX = 18_446_744_073_709_551_615n;

export interface D5cDisplayHit {
  path: string;
  heading_path: string[];
  frontmatter: WorkerFrontmatter;
}

export interface D5cDisplayCandidate {
  ordinal: number;
  hit: D5cDisplayHit;
}

export interface D5cCompareResult {
  schema_version: typeof D5C_COMPARE_SCHEMA_VERSION;
  generation: string;
  publication: "active" | "initial_staging";
  revision: number | null;
  candidate_pool_count: number;
  display_candidates: D5cDisplayCandidate[];
  text_order: number[];
  balanced_order: number[];
  aggregate: {
    moved_candidate_count: number;
    top_n_overlap: number;
  };
}

export interface D5cCompareCommand {
  operation: typeof INTERNAL_D5C_COMPARE_OPERATION;
  generation: string;
  revision: number | null;
  query: string;
  limit: number;
  query_time_epoch_seconds: string;
}

export type D5cCompareRequest = D5cCompareCommand & {
  version: typeof WORKER_PROTOCOL_VERSION;
  id: number;
};

export type D5cCompareResponse =
  | {
      version: typeof WORKER_PROTOCOL_VERSION;
      id: number;
      operation: typeof INTERNAL_D5C_COMPARE_OPERATION;
      ok: true;
      result: D5cCompareResult;
    }
  | {
      version: typeof WORKER_PROTOCOL_VERSION;
      id: number;
      operation: typeof INTERNAL_D5C_COMPARE_OPERATION;
      ok: false;
      error: WorkerError;
    };

export function isD5cCompareRequest(value: unknown): value is D5cCompareRequest {
  if (!isRecord(value)
    || !hasExactKeys(value, [
      "version", "id", "operation", "generation", "revision", "query", "limit",
      "query_time_epoch_seconds",
    ])
    || value.version !== WORKER_PROTOCOL_VERSION
    || value.operation !== INTERNAL_D5C_COMPARE_OPERATION
    || !isPositiveSafeInteger(value.id)
    || !isBoundedNonemptyString(value.generation, 1_024)
    || !(value.revision === null || isPositiveSafeInteger(value.revision))
    || typeof value.query !== "string"
    || value.query.trim().length === 0
    || value.query.length > MAX_QUERY_CHARACTERS
    || !isPositiveSafeInteger(value.limit)
    || value.limit > MAX_SEARCH_HITS
    || typeof value.query_time_epoch_seconds !== "string"
    || !/^(0|[1-9][0-9]{0,19})$/u.test(value.query_time_epoch_seconds)) {
    return false;
  }
  return BigInt(value.query_time_epoch_seconds) <= U64_MAX;
}

export function isD5cCompareResponse(value: unknown): value is D5cCompareResponse {
  if (!isRecord(value)
    || value.version !== WORKER_PROTOCOL_VERSION
    || !isPositiveSafeInteger(value.id)
    || value.operation !== INTERNAL_D5C_COMPARE_OPERATION
    || typeof value.ok !== "boolean") {
    return false;
  }
  if (value.ok) {
    return hasExactKeys(value, ["version", "id", "operation", "ok", "result"])
      && isD5cCompareResult(value.result);
  }
  return hasExactKeys(value, ["version", "id", "operation", "ok", "error"])
    && isWorkerError(value.error);
}

function isD5cCompareResult(value: unknown): value is D5cCompareResult {
  if (!isRecord(value)
    || !hasExactKeys(value, [
      "schema_version", "generation", "publication", "revision", "candidate_pool_count",
      "display_candidates", "text_order", "balanced_order", "aggregate",
    ])
    || value.schema_version !== D5C_COMPARE_SCHEMA_VERSION
    || !isBoundedNonemptyString(value.generation, 1_024)
    || (value.publication !== "active" && value.publication !== "initial_staging")
    || !(value.revision === null || isPositiveSafeInteger(value.revision))
    || (value.publication === "active" && value.revision !== null)
    || (value.publication === "initial_staging" && value.revision === null)
    || !isNonnegativeSafeInteger(value.candidate_pool_count)
    || value.candidate_pool_count > 512
    || !Array.isArray(value.display_candidates)
    || value.display_candidates.length > MAX_SEARCH_HITS * 2
    || !Array.isArray(value.text_order)
    || value.text_order.length > MAX_SEARCH_HITS
    || !Array.isArray(value.balanced_order)
    || value.balanced_order.length !== value.text_order.length
    || !isRecord(value.aggregate)
    || !hasExactKeys(value.aggregate, ["moved_candidate_count", "top_n_overlap"])
    || !isNonnegativeSafeInteger(value.aggregate.moved_candidate_count)
    || value.aggregate.moved_candidate_count > value.text_order.length
    || !isNonnegativeSafeInteger(value.aggregate.top_n_overlap)
    || value.aggregate.top_n_overlap > value.text_order.length) {
    return false;
  }

  const candidatePoolCount = value.candidate_pool_count;
  const displayOrdinals = new Set<number>();
  for (const candidate of value.display_candidates) {
    if (!isRecord(candidate)
      || !hasExactKeys(candidate, ["ordinal", "hit"])
      || !isNonnegativeSafeInteger(candidate.ordinal)
      || candidate.ordinal >= candidatePoolCount
      || displayOrdinals.has(candidate.ordinal)
      || !isDisplayHit(candidate.hit)) {
      return false;
    }
    displayOrdinals.add(candidate.ordinal);
  }

  const validOrder = (order: unknown[]): order is number[] => {
    const seen = new Set<number>();
    for (const ordinal of order) {
      if (!isNonnegativeSafeInteger(ordinal)
        || ordinal >= candidatePoolCount
        || !displayOrdinals.has(ordinal)
        || seen.has(ordinal)) return false;
      seen.add(ordinal);
    }
    return true;
  };
  return validOrder(value.text_order) && validOrder(value.balanced_order);
}

function isDisplayHit(value: unknown): value is D5cDisplayHit {
  if (!isRecord(value)
    || !hasExactKeys(value, ["path", "heading_path", "frontmatter"])
    || !isBoundedNonemptyString(value.path, MAX_PATH_CHARACTERS)
    || !Array.isArray(value.heading_path)
    || value.heading_path.length > MAX_HEADING_DEPTH
    || !value.heading_path.every((heading) =>
      typeof heading === "string" && heading.length <= MAX_HEADING_CHARACTERS)
    || !isRecord(value.frontmatter)) {
    return false;
  }
  const keys = Object.keys(value.frontmatter);
  return keys.length <= 1
    && keys.every((key) => key === "title")
    && (value.frontmatter.title === undefined
      || (typeof value.frontmatter.title === "string"
        && new TextEncoder().encode(value.frontmatter.title).byteLength <= MAX_TITLE_BYTES));
}

function isWorkerError(value: unknown): value is WorkerError {
  return isRecord(value)
    && hasExactKeys(value, ["code", "stage", "message", "retryable"])
    && typeof value.code === "string"
    && typeof value.stage === "string"
    && typeof value.message === "string"
    && typeof value.retryable === "boolean";
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function hasExactKeys(value: Record<string, unknown>, keys: readonly string[]): boolean {
  const actual = Object.keys(value);
  return actual.length === keys.length && actual.every((key) => keys.includes(key));
}

function isPositiveSafeInteger(value: unknown): value is number {
  return Number.isSafeInteger(value) && Number(value) > 0;
}

function isNonnegativeSafeInteger(value: unknown): value is number {
  return Number.isSafeInteger(value) && Number(value) >= 0;
}

function isBoundedNonemptyString(value: unknown, maximum: number): value is string {
  return typeof value === "string" && value.length > 0 && value.length <= maximum;
}
