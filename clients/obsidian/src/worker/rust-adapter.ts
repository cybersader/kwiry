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

import type { SourceDescriptorInput } from "./protocol";

const ABI_VERSION = 1;
const SOURCE_SCHEMA_VERSION = 1;
const QUERY_SCHEMA_VERSION = 2;
const MATCH_PLAN_SCHEMA_VERSION = 1;

export interface RustIdentity {
  abi_version: 1;
  adapter: "kwiry-obsidian-wasm";
  adapter_version: string;
  source_preparation_schema_version: 1;
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

export interface PreparedFrontmatter {
  title?: string;
  description?: string;
  tags?: string[];
  status?: string;
  date?: string;
}

export interface PreparedChunk {
  chunk: {
    chunk_id: string;
    vault_id: string;
    room: string | null;
    path: string;
    heading_path: string[];
    content: string;
    frontmatter: PreparedFrontmatter;
    links_out: string[];
    mtime: number;
    content_hash: string;
    chunking_version: number;
  };
  heading_text: string;
  technical_identifiers: string[];
}

export interface SourcePreparation {
  schema_version: 1;
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
  if (!isRecord(response.result)
    || !hasExactKeys(response.result, ["preparation"])
    || !isSourcePreparation(response.result.preparation)) {
    throw new RustAdapterError("invalid_response", "Portable Rust returned invalid source data.");
  }
  return response.result.preparation;
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
  if (!isRecord(response.result)
    || !hasExactKeys(response.result, ["preparation"])
    || !isSourcePreparation(response.result.preparation)) {
    throw new RustAdapterError("invalid_response", "Portable Rust returned invalid source data.");
  }
  const preparation = response.result.preparation;
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
  if (source.length > 32 * 1024 * 1024) {
    throw new RustAdapterError("invalid_response", "Portable Rust response exceeded its limit.");
  }
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

function isSourcePreparation(value: unknown): value is SourcePreparation {
  if (!isRecord(value)) return false;
  const required = [
    "schema_version",
    "source_key",
    "vault_id",
    "path",
    "format",
    "content_hash",
    "byte_length",
    "mtime",
    "mtime_nanos",
    "retrieval",
    "chunks",
    "kind",
  ];
  const optional = ["room", "warning"];
  if (!hasRequiredAndOptionalKeys(value, required, optional)
    || value.schema_version !== SOURCE_SCHEMA_VERSION
    || !isBoundedString(value.source_key, 128)
    || !isBoundedString(value.vault_id, 1_024)
    || (value.room !== undefined && !isBoundedString(value.room, 1_024))
    || !isBoundedString(value.path, 4_096)
    || (value.format !== "markdown" && value.format !== "text")
    || (value.content_hash !== null && !isBoundedString(value.content_hash, 128))
    || !isNonNegativeSafeInteger(value.byte_length)
    || !isNonNegativeSafeInteger(value.mtime)
    || typeof value.mtime_nanos !== "string"
    || !/^[0-9]{1,39}$/u.test(value.mtime_nanos)
    || !isRetrieval(value.retrieval)
    || !Array.isArray(value.chunks)
    || value.chunks.length > 100_000
    || !value.chunks.every(isPreparedChunk)
    || (value.kind !== "indexed" && value.kind !== "skipped")
    || (value.warning !== undefined && !isBoundedString(value.warning, 4_096, true))) {
    return false;
  }
  // A skipped source never carries chunks, and an indexed source always
  // carries the hash its chunks were derived from. Both halves are enforced
  // here so a malformed preparation is rejected at the ABI boundary — as
  // `source_rejected` — rather than surfacing later as a storage constraint
  // violation the Worker would have to blame on the index.
  if (value.kind === "skipped") return value.chunks.length === 0;
  return value.content_hash !== null;
}

function isRetrieval(value: unknown): boolean {
  return isRecord(value)
    && hasExactKeys(value, ["filename", "stem", "aliases"])
    && isBoundedString(value.filename, 4_096)
    && isBoundedString(value.stem, 4_096)
    && isBoundedStrings(value.aliases, 256, 1_024);
}

function isPreparedChunk(value: unknown): boolean {
  return isRecord(value)
    && hasExactKeys(value, ["chunk", "heading_text", "technical_identifiers"])
    && isChunk(value.chunk)
    && isBoundedString(value.heading_text, 8_192, true)
    && isBoundedStrings(value.technical_identifiers, 1_024, 1_024);
}

function isChunk(value: unknown): boolean {
  return isRecord(value)
    && hasExactKeys(value, [
      "chunk_id",
      "vault_id",
      "room",
      "path",
      "heading_path",
      "content",
      "frontmatter",
      "links_out",
      "mtime",
      "content_hash",
      "chunking_version",
    ])
    && isBoundedString(value.chunk_id, 128)
    && isBoundedString(value.vault_id, 1_024)
    && (value.room === null || isBoundedString(value.room, 1_024))
    && isBoundedString(value.path, 4_096)
    && isBoundedStrings(value.heading_path, 64, 1_024)
    && isBoundedString(value.content, 16_384)
    && isFrontmatter(value.frontmatter)
    && isBoundedStrings(value.links_out, 4_096, 4_096)
    && isNonNegativeSafeInteger(value.mtime)
    && isBoundedString(value.content_hash, 128)
    && isNonNegativeSafeInteger(value.chunking_version);
}

function isFrontmatter(value: unknown): boolean {
  if (!isRecord(value)) return false;
  const allowed = ["title", "description", "tags", "status", "date"];
  if (Object.keys(value).some((key) => !allowed.includes(key))) return false;
  for (const key of ["title", "description", "status", "date"] as const) {
    if (value[key] !== undefined && !isBoundedString(value[key], 1_024, true)) return false;
  }
  return value.tags === undefined || isBoundedStrings(value.tags, 256, 1_024, true);
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
