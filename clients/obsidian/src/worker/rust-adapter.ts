// SPDX-FileCopyrightText: 2026 cybersader
// SPDX-License-Identifier: GPL-3.0-only

import rustWasmBytes from "virtual:kwiry-rust-wasm-bytes";
import {
  abi_identity,
  finalize_query,
  initSync,
  prepare_oversized_source,
  prepare_query,
  prepare_source,
} from "virtual:kwiry-rust-wasm-bindings";

import type { PropertyBag, SourceDescriptorInput } from "./protocol";

const ABI_VERSION = 1;
const SOURCE_SCHEMA_VERSION = 2;
const QUERY_SCHEMA_VERSION = 2;
const MATCH_PLAN_SCHEMA_VERSION = 1;

export interface RustIdentity {
  abi_version: 1;
  adapter: "kwiry-obsidian-wasm";
  adapter_version: string;
  source_preparation_schema_version: 2;
  lexical_query_plan_schema_version: 2;
  fts5_match_plan_schema_version: 1;
  /**
   * The chunking contract the adapter applies. Chunk rows carry it per chunk,
   * but a generation with no chunks still has to name the contract its cached
   * image was produced under, so the identity is the source of truth.
   */
  chunking_version: number;
  max_request_bytes: number;
  max_source_buffer_bytes: number;
  operations: ["prepare_source", "prepare_oversized_source", "prepare_query", "finalize_query"];
}

export type PreparedPropertyValue =
  | { type: "null" }
  | { type: "boolean"; value: boolean }
  | { type: "i64" | "u64"; value: string }
  | { type: "f64"; value: string }
  | { type: "string"; value: string }
  | { type: "sequence"; value: PreparedPropertyValue[] }
  | { type: "map"; value: PreparedFrontmatter };

export interface PreparedFrontmatter {
  [name: string]: PreparedPropertyValue;
}

export interface PreparedChunk {
  chunk: {
    chunk_id: string;
    vault_id: string;
    room: string | null;
    path: string;
    heading_path: string[];
    content: string;
    frontmatter: PropertyBag;
    links_out: string[];
    mtime: number;
    content_hash: string;
    chunking_version: number;
  };
  heading_text: string;
  technical_identifiers: string[];
}

export interface SourcePreparation {
  schema_version: 2;
  source_key: string;
  vault_id: string;
  room?: string;
  path: string;
  format: "markdown" | "text";
  content_hash: string | null;
  byte_length: number;
  mtime: number;
  mtime_nanos: string;
  retrieval: {
    filename: string;
    stem: string;
    aliases: string[];
  };
  frontmatter: PreparedFrontmatter;
  chunks: PreparedChunk[];
  kind: "indexed" | "skipped";
  warning?: string;
}

export interface LexicalQueryPlan {
  schema_version: 2;
  query: string;
  kind: "explicit" | "ordinary" | "identifier";
  match_operator: "explicit" | "any" | "all";
  terms: string[];
  normalized_exact: string | null;
  phrase_boost: boolean;
  metadata_probe?: {
    query: string;
    fields: ["filename", "stem", "aliases", "title", "heading"];
    conjunction: true;
  };
}

export interface MetadataProbePlan {
  schema_version: 1;
  plan_id: "metadata_probe_v1";
  match_value: string;
}

export interface MatchPlan {
  schema_version: 1;
  plan_id: "lexical_any_v1" | "lexical_all_v1" | "lexical_explicit_v1";
  match_value: string;
}

export interface PreparedQuery {
  plan: LexicalQueryPlan;
  metadata_probe?: MetadataProbePlan;
}

export interface FinalizedQuery {
  plan: LexicalQueryPlan;
  match_plan: MatchPlan;
}

export class RustAdapterError extends Error {
  constructor(
    public readonly code: string,
    message: string,
  ) {
    super(message);
    this.name = "RustAdapterError";
  }
}

export function initializeRustAdapter(): RustIdentity {
  try {
    initSync({ module: rustWasmBytes.slice() });
  } catch {
    throw new RustAdapterError("rust_init_failed", "Portable Rust initialization failed.");
  }
  const identity = parseJson(abi_identity());
  if (!isRustIdentity(identity)) {
    throw new RustAdapterError("artifact_mismatch", "Portable Rust artifact identity mismatch.");
  }
  return identity;
}

import { sourcePreparationDefect } from "./source-defect";
export { sourcePreparationDefect } from "./source-defect";

export function prepareSourceWithRust(
  descriptor: SourceDescriptorInput,
  bytes: Uint8Array,
): SourcePreparation {
  const response = parseResponse(
    prepare_source(JSON.stringify({
      abi_version: ABI_VERSION,
      operation: "prepare_source",
      descriptor,
    }), bytes),
    "prepare_source",
  );
  if (!isRecord(response.result) || !hasExactKeys(response.result, ["preparation"])) {
    throw new RustAdapterError("invalid_response", "Portable Rust returned invalid source data.");
  }
  return requireSourcePreparation(response.result.preparation);
}

function requireSourcePreparation(value: unknown): SourcePreparation {
  const defect = sourcePreparationDefect(value);
  if (defect !== null) {
    // Only the fixed structural identifier crosses the diagnostic boundary;
    // property names and values remain note content and are never reported.
    const failure = new RustAdapterError(
      "invalid_response",
      "Portable Rust returned invalid source data.",
    );
    (failure as { defectField?: string }).defectField = defect;
    throw failure;
  }
  return value as SourcePreparation;
}

export function prepareOversizedSourceWithRust(
  descriptor: SourceDescriptorInput,
): SourcePreparation {
  const response = parseResponse(
    prepare_oversized_source(JSON.stringify({
      abi_version: ABI_VERSION,
      operation: "prepare_oversized_source",
      descriptor,
    })),
    "prepare_oversized_source",
  );
  if (!isRecord(response.result) || !hasExactKeys(response.result, ["preparation"])) {
    throw new RustAdapterError("invalid_response", "Portable Rust returned invalid source data.");
  }
  const preparation = requireSourcePreparation(response.result.preparation);
  if (preparation.kind !== "skipped"
    || preparation.content_hash !== null
    || preparation.byte_length !== descriptor.byte_length) {
    throw new RustAdapterError("invalid_response", "Portable Rust returned invalid oversized source data.");
  }
  return preparation;
}

export function prepareQueryWithRust(query: string): PreparedQuery {
  const response = parseResponse(
    prepare_query(JSON.stringify({
      abi_version: ABI_VERSION,
      operation: "prepare_query",
      query,
    })),
    "prepare_query",
  );
  if (!isPreparedQuery(response.result)) {
    throw new RustAdapterError("invalid_response", "Portable Rust returned invalid query data.");
  }
  return response.result;
}

export function finalizeQueryWithRust(query: string, metadataProbeMatched: boolean): FinalizedQuery {
  const response = parseResponse(
    finalize_query(JSON.stringify({
      abi_version: ABI_VERSION,
      operation: "finalize_query",
      query,
      metadata_probe_matched: metadataProbeMatched,
    })),
    "finalize_query",
  );
  if (!isFinalizedQuery(response.result)) {
    throw new RustAdapterError("invalid_response", "Portable Rust returned invalid match data.");
  }
  return response.result;
}

interface SuccessResponse {
  result: unknown;
}

function parseResponse(source: string, operation: string): SuccessResponse {
  const value = parseJson(source);
  if (!isRecord(value)
    || value.abi_version !== ABI_VERSION
    || value.operation !== operation
    || (value.status !== "ok" && value.status !== "error")) {
    throw new RustAdapterError("invalid_response", "Portable Rust returned an invalid response.");
  }
  if (value.status === "error") {
    if (!hasExactKeys(value, ["status", "abi_version", "operation", "error"])
      || !isRecord(value.error)
      || !hasExactKeys(value.error, ["code", "message"])
      || typeof value.error.code !== "string"
      || typeof value.error.message !== "string"
      || value.error.message.length > 1_024) {
      throw new RustAdapterError("invalid_response", "Portable Rust returned an invalid error.");
    }
    throw new RustAdapterError(value.error.code, value.error.message);
  }
  if (!hasExactKeys(value, ["status", "abi_version", "operation", "result"])) {
    throw new RustAdapterError("invalid_response", "Portable Rust returned an invalid response.");
  }
  return { result: value.result };
}

function parseJson(source: string): unknown {
  // The in-process Rust call has already allocated the complete response before
  // this point. A second arbitrary length cap cannot protect allocation; it can
  // only reject valid large property bags, especially when note-level metadata
  // is repeated across several chunks.
  try {
    return JSON.parse(source);
  } catch {
    throw new RustAdapterError("invalid_response", "Portable Rust returned invalid JSON.");
  }
}

function isRustIdentity(value: unknown): value is RustIdentity {
  return isRecord(value)
    && hasExactKeys(value, [
      "abi_version",
      "adapter",
      "adapter_version",
      "source_preparation_schema_version",
      "lexical_query_plan_schema_version",
      "fts5_match_plan_schema_version",
      "chunking_version",
      "max_request_bytes",
      "max_source_buffer_bytes",
      "operations",
    ])
    && value.abi_version === ABI_VERSION
    && value.adapter === "kwiry-obsidian-wasm"
    && isBoundedString(value.adapter_version, 128)
    && value.source_preparation_schema_version === SOURCE_SCHEMA_VERSION
    && value.lexical_query_plan_schema_version === QUERY_SCHEMA_VERSION
    && value.fts5_match_plan_schema_version === MATCH_PLAN_SCHEMA_VERSION
    && isNonNegativeSafeInteger(value.chunking_version)
    && isPositiveSafeInteger(value.max_request_bytes)
    && isPositiveSafeInteger(value.max_source_buffer_bytes)
    && Array.isArray(value.operations)
    && JSON.stringify(value.operations) === JSON.stringify([
      "prepare_source",
      "prepare_oversized_source",
      "prepare_query",
      "finalize_query",
    ]);
}

function isPreparedQuery(value: unknown): value is PreparedQuery {
  if (!isRecord(value)
    || !hasRequiredAndOptionalKeys(value, ["plan"], ["metadata_probe"])
    || !isLexicalQueryPlan(value.plan)) {
    return false;
  }
  return value.metadata_probe === undefined || isMetadataProbePlan(value.metadata_probe);
}

function isFinalizedQuery(value: unknown): value is FinalizedQuery {
  return isRecord(value)
    && hasExactKeys(value, ["plan", "match_plan"])
    && isLexicalQueryPlan(value.plan)
    && isMatchPlan(value.match_plan);
}

function isLexicalQueryPlan(value: unknown): value is LexicalQueryPlan {
  if (!isRecord(value)) return false;
  const required = [
    "schema_version",
    "query",
    "kind",
    "match_operator",
    "terms",
    "normalized_exact",
    "phrase_boost",
  ];
  if (!hasRequiredAndOptionalKeys(value, required, ["metadata_probe"])
    || value.schema_version !== QUERY_SCHEMA_VERSION
    || !isBoundedString(value.query, 4_096)
    || (value.kind !== "explicit" && value.kind !== "ordinary" && value.kind !== "identifier")
    || (value.match_operator !== "explicit"
      && value.match_operator !== "any"
      && value.match_operator !== "all")
    || !isBoundedStrings(value.terms, 128, 4_096)
    || (value.normalized_exact !== null
      && !isBoundedString(value.normalized_exact, 4_096, true))
    || typeof value.phrase_boost !== "boolean") {
    return false;
  }
  if (value.metadata_probe === undefined) return true;
  return isRecord(value.metadata_probe)
    && hasExactKeys(value.metadata_probe, ["query", "fields", "conjunction"])
    && isBoundedString(value.metadata_probe.query, 4_096)
    && JSON.stringify(value.metadata_probe.fields) === JSON.stringify([
      "filename",
      "stem",
      "aliases",
      "title",
      "heading",
    ])
    && value.metadata_probe.conjunction === true;
}

function isMetadataProbePlan(value: unknown): value is MetadataProbePlan {
  return isRecord(value)
    && hasExactKeys(value, ["schema_version", "plan_id", "match_value"])
    && value.schema_version === MATCH_PLAN_SCHEMA_VERSION
    && value.plan_id === "metadata_probe_v1"
    && isBoundedString(value.match_value, 16_384);
}

function isMatchPlan(value: unknown): value is MatchPlan {
  return isRecord(value)
    && hasExactKeys(value, ["schema_version", "plan_id", "match_value"])
    && value.schema_version === MATCH_PLAN_SCHEMA_VERSION
    && (value.plan_id === "lexical_any_v1"
      || value.plan_id === "lexical_all_v1"
      || value.plan_id === "lexical_explicit_v1")
    && isBoundedString(value.match_value, 16_384);
}

function isBoundedStrings(
  value: unknown,
  maximumItems: number,
  maximumCharacters: number,
  allowEmpty = false,
): value is string[] {
  return Array.isArray(value)
    && value.length <= maximumItems
    && value.every((item) => isBoundedString(item, maximumCharacters, allowEmpty));
}

function isBoundedString(value: unknown, maximum: number, allowEmpty = false): value is string {
  return typeof value === "string"
    && value.length <= maximum
    && (allowEmpty || value.length > 0);
}

function isPositiveSafeInteger(value: unknown): value is number {
  return typeof value === "number" && Number.isSafeInteger(value) && value > 0;
}

function isNonNegativeSafeInteger(value: unknown): value is number {
  return typeof value === "number" && Number.isSafeInteger(value) && value >= 0;
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function hasExactKeys(value: Record<string, unknown>, keys: readonly string[]): boolean {
  const actual = Object.keys(value);
  return actual.length === keys.length && actual.every((key) => keys.includes(key));
}

function hasRequiredAndOptionalKeys(
  value: Record<string, unknown>,
  required: readonly string[],
  optional: readonly string[],
): boolean {
  const actual = Object.keys(value);
  return required.every((key) => actual.includes(key))
    && actual.every((key) => required.includes(key) || optional.includes(key));
}
