// SPDX-FileCopyrightText: 2026 cybersader
// SPDX-License-Identifier: GPL-3.0-only

export const WORKER_PROTOCOL_VERSION = 1 as const;
export const WORKER_REQUEST_TIMEOUT_MS = 30_000;
export const MAX_PENDING_REQUESTS = 16;
export const MAX_BATCH_SOURCES = 16;
export const MAX_SOURCE_BYTES = 10 * 1024 * 1024 + 1;
export const MAX_BATCH_BYTES = 16 * 1024 * 1024;
export const MAX_GENERATION_CHARACTERS = 128;
export const MAX_QUERY_CHARACTERS = 4_096;
export const MAX_SEARCH_HITS = 100;

export type WorkerOperation =
  | "initialize"
  | "begin_build"
  | "add_source_batch"
  | "commit_build"
  | "abort_build"
  | "search"
  | "status"
  | "dispose";

export type WorkerErrorCode =
  | "protocol_mismatch"
  | "invalid_request"
  | "invalid_state"
  | "artifact_mismatch"
  | "rust_init_failed"
  | "sqlite_init_failed"
  | "fts5_unavailable"
  | "source_rejected"
  | "query_rejected"
  | "index_building"
  | "integrity_failed"
  | "worker_crashed"
  | "timeout"
  | "disposed"
  | "internal_error";

export interface WorkerError {
  code: WorkerErrorCode;
  stage: "protocol" | "artifact" | "rust" | "sqlite" | "index" | "query" | "lifecycle";
  message: string;
  retryable: boolean;
}

export interface SourceDescriptorInput {
  vault_id: string;
  room?: string;
  path: string;
  format: "markdown" | "text";
  byte_length: number;
  mtime: number;
  mtime_nanos: string;
}

export interface SourceInput {
  descriptor: SourceDescriptorInput;
  bytes: Uint8Array;
}

interface RequestBase {
  version: typeof WORKER_PROTOCOL_VERSION;
  id: number;
}

export type WorkerRequest =
  | (RequestBase & { operation: "initialize" })
  | (RequestBase & { operation: "begin_build"; generation: string })
  | (RequestBase & {
      operation: "add_source_batch";
      generation: string;
      sources: SourceInput[];
    })
  | (RequestBase & { operation: "commit_build"; generation: string })
  | (RequestBase & { operation: "abort_build"; generation: string })
  | (RequestBase & {
      operation: "search";
      query: string;
      limit: number;
    })
  | (RequestBase & { operation: "status" })
  | (RequestBase & { operation: "dispose" });

export interface InitializeResult {
  rustAbiVersion: 1;
  sourceSchemaVersion: number;
  querySchemaVersion: number;
  matchPlanSchemaVersion: 1;
  sqliteVersion: "3.53.0";
  fts5Enabled: 1;
}

export interface BuildResult {
  generation: string;
  documents: number;
  chunks: number;
}

export interface StatusResult {
  phase: "ready" | "building" | "disposed" | "failed";
  searchable: boolean;
  active_generation: string | null;
  staging_generation: string | null;
  documents: number;
  chunks: number;
  dirty: boolean;
  rebuilding: boolean;
}

export interface WorkerFrontmatter {
  title?: string;
  description?: string;
  tags?: string[];
  status?: string;
  date?: string;
}

export interface WorkerSearchHit {
  chunk_id: string;
  vault_id: string;
  path: string;
  heading_path: string[];
  score: number;
  excerpt: string;
  frontmatter: WorkerFrontmatter;
}

export interface SearchResult {
  generation: string;
  hits: WorkerSearchHit[];
}

export interface DisposeResult {
  closed: true;
}

export type WorkerResult =
  | InitializeResult
  | BuildResult
  | StatusResult
  | SearchResult
  | DisposeResult;

export type WorkerResponse =
  | {
      version: typeof WORKER_PROTOCOL_VERSION;
      id: number;
      operation: WorkerOperation;
      ok: true;
      result: WorkerResult;
    }
  | {
      version: typeof WORKER_PROTOCOL_VERSION;
      id: number;
      operation: WorkerOperation;
      ok: false;
      error: WorkerError;
    };

export function parseWorkerRequest(value: unknown): WorkerRequest | WorkerError {
  if (!isRecord(value) || !isRequestId(value.id) || !isWorkerOperation(value.operation)) {
    return fixedWorkerError("invalid_request", "protocol", "Invalid Worker request.", false);
  }
  if (value.version !== WORKER_PROTOCOL_VERSION) {
    return fixedWorkerError(
      "protocol_mismatch",
      "protocol",
      "Unsupported Worker protocol.",
      false,
    );
  }

  const base = ["version", "id", "operation"];
  switch (value.operation) {
    case "initialize":
    case "status":
    case "dispose":
      return hasExactKeys(value, base)
        ? value as unknown as WorkerRequest
        : fixedWorkerError("invalid_request", "protocol", "Invalid Worker request.", false);
    case "begin_build":
    case "commit_build":
    case "abort_build":
      return hasExactKeys(value, [...base, "generation"]) && isGeneration(value.generation)
        ? value as unknown as WorkerRequest
        : fixedWorkerError("invalid_request", "protocol", "Invalid Worker request.", false);
    case "add_source_batch":
      return hasExactKeys(value, [...base, "generation", "sources"])
        && isGeneration(value.generation)
        && isSourceBatch(value.sources)
        ? value as unknown as WorkerRequest
        : fixedWorkerError("invalid_request", "protocol", "Invalid Worker request.", false);
    case "search":
      return hasExactKeys(value, [...base, "query", "limit"])
        && typeof value.query === "string"
        && value.query.trim().length > 0
        && value.query.length <= MAX_QUERY_CHARACTERS
        && Number.isSafeInteger(value.limit)
        && Number(value.limit) >= 1
        && Number(value.limit) <= MAX_SEARCH_HITS
        ? value as unknown as WorkerRequest
        : fixedWorkerError("invalid_request", "protocol", "Invalid Worker request.", false);
  }
}

export function isWorkerResponse(value: unknown): value is WorkerResponse {
  if (!isRecord(value)
    || value.version !== WORKER_PROTOCOL_VERSION
    || !isRequestId(value.id)
    || !isWorkerOperation(value.operation)
    || typeof value.ok !== "boolean") {
    return false;
  }
  if (value.ok) {
    return hasExactKeys(value, ["version", "id", "operation", "ok", "result"])
      && isResultForOperation(value.operation, value.result);
  }
  return hasExactKeys(value, ["version", "id", "operation", "ok", "error"])
    && isWorkerError(value.error);
}

export function fixedWorkerError(
  code: WorkerErrorCode,
  stage: WorkerError["stage"],
  message: string,
  retryable: boolean,
): WorkerError {
  return { code, stage, message, retryable };
}

export function isGeneration(value: unknown): value is string {
  return typeof value === "string"
    && value.length >= 1
    && value.length <= MAX_GENERATION_CHARACTERS
    && /^[A-Za-z0-9][A-Za-z0-9._-]*$/u.test(value);
}

function isSourceBatch(value: unknown): value is SourceInput[] {
  if (!Array.isArray(value) || value.length < 1 || value.length > MAX_BATCH_SOURCES) return false;
  let totalBytes = 0;
  for (const source of value) {
    if (!isRecord(source)
      || !hasExactKeys(source, ["descriptor", "bytes"])
      || !(source.bytes instanceof Uint8Array)
      || source.bytes.byteLength > MAX_SOURCE_BYTES
      || !isSourceDescriptor(source.descriptor)
      || source.descriptor.byte_length !== source.bytes.byteLength) {
      return false;
    }
    totalBytes += source.bytes.byteLength;
    if (totalBytes > MAX_BATCH_BYTES) return false;
  }
  return true;
}

function isSourceDescriptor(value: unknown): value is SourceDescriptorInput {
  if (!isRecord(value)) return false;
  const required = ["vault_id", "path", "format", "byte_length", "mtime", "mtime_nanos"];
  const allowed = value.room === undefined ? required : [...required, "room"];
  return hasExactKeys(value, allowed)
    && isBoundedString(value.vault_id, 1_024)
    && (value.room === undefined || isBoundedString(value.room, 1_024))
    && isBoundedString(value.path, 4_096)
    && (value.format === "markdown" || value.format === "text")
    && isNonNegativeSafeInteger(value.byte_length)
    && Number(value.byte_length) <= MAX_SOURCE_BYTES
    && isNonNegativeSafeInteger(value.mtime)
    && typeof value.mtime_nanos === "string"
    && /^[0-9]{1,39}$/u.test(value.mtime_nanos);
}

function isResultForOperation(operation: WorkerOperation, value: unknown): boolean {
  switch (operation) {
    case "initialize":
      return isInitializeResult(value);
    case "begin_build":
    case "add_source_batch":
    case "commit_build":
    case "abort_build":
      return isBuildResult(value);
    case "status":
      return isStatusResult(value);
    case "search":
      return isSearchResult(value);
    case "dispose":
      return isRecord(value) && hasExactKeys(value, ["closed"]) && value.closed === true;
  }
}

function isInitializeResult(value: unknown): value is InitializeResult {
  return isRecord(value)
    && hasExactKeys(value, [
      "rustAbiVersion",
      "sourceSchemaVersion",
      "querySchemaVersion",
      "matchPlanSchemaVersion",
      "sqliteVersion",
      "fts5Enabled",
    ])
    && value.rustAbiVersion === 1
    && isNonNegativeSafeInteger(value.sourceSchemaVersion)
    && isNonNegativeSafeInteger(value.querySchemaVersion)
    && value.matchPlanSchemaVersion === 1
    && value.sqliteVersion === "3.53.0"
    && value.fts5Enabled === 1;
}

function isBuildResult(value: unknown): value is BuildResult {
  return isRecord(value)
    && hasExactKeys(value, ["generation", "documents", "chunks"])
    && isGeneration(value.generation)
    && isNonNegativeSafeInteger(value.documents)
    && isNonNegativeSafeInteger(value.chunks);
}

function isStatusResult(value: unknown): value is StatusResult {
  return isRecord(value)
    && hasExactKeys(value, [
      "phase",
      "searchable",
      "active_generation",
      "staging_generation",
      "documents",
      "chunks",
      "dirty",
      "rebuilding",
    ])
    && (value.phase === "ready"
      || value.phase === "building"
      || value.phase === "disposed"
      || value.phase === "failed")
    && typeof value.searchable === "boolean"
    && (value.active_generation === null || isGeneration(value.active_generation))
    && (value.staging_generation === null || isGeneration(value.staging_generation))
    && isNonNegativeSafeInteger(value.documents)
    && isNonNegativeSafeInteger(value.chunks)
    && typeof value.dirty === "boolean"
    && typeof value.rebuilding === "boolean";
}

function isSearchResult(value: unknown): value is SearchResult {
  return isRecord(value)
    && hasExactKeys(value, ["generation", "hits"])
    && isGeneration(value.generation)
    && Array.isArray(value.hits)
    && value.hits.length <= MAX_SEARCH_HITS
    && value.hits.every(isSearchHit);
}

function isSearchHit(value: unknown): value is WorkerSearchHit {
  return isRecord(value)
    && hasExactKeys(value, [
      "chunk_id",
      "vault_id",
      "path",
      "heading_path",
      "score",
      "excerpt",
      "frontmatter",
    ])
    && isBoundedString(value.chunk_id, 1_024)
    && isBoundedString(value.vault_id, 1_024)
    && isBoundedString(value.path, 4_096)
    && Array.isArray(value.heading_path)
    && value.heading_path.length <= 64
    && value.heading_path.every((heading) => isBoundedString(heading, 1_024))
    && typeof value.score === "number"
    && Number.isFinite(value.score)
    && typeof value.excerpt === "string"
    && value.excerpt.length <= 262_144
    && isFrontmatter(value.frontmatter);
}

function isFrontmatter(value: unknown): value is WorkerFrontmatter {
  if (!isRecord(value)) return false;
  const allowed = ["title", "description", "tags", "status", "date"];
  if (Object.keys(value).some((key) => !allowed.includes(key))) return false;
  for (const key of ["title", "description", "status", "date"] as const) {
    if (value[key] !== undefined && !isBoundedString(value[key], 1_024, true)) return false;
  }
  return value.tags === undefined
    || (Array.isArray(value.tags)
      && value.tags.length <= 256
      && value.tags.every((tag) => isBoundedString(tag, 1_024, true)));
}

function isWorkerError(value: unknown): value is WorkerError {
  if (!isRecord(value)
    || !hasExactKeys(value, ["code", "stage", "message", "retryable"])
    || typeof value.code !== "string"
    || typeof value.stage !== "string"
    || typeof value.message !== "string"
    || value.message.length > 1_024
    || typeof value.retryable !== "boolean") {
    return false;
  }
  return [
    "protocol_mismatch",
    "invalid_request",
    "invalid_state",
    "artifact_mismatch",
    "rust_init_failed",
    "sqlite_init_failed",
    "fts5_unavailable",
    "source_rejected",
    "query_rejected",
    "index_building",
    "integrity_failed",
    "worker_crashed",
    "timeout",
    "disposed",
    "internal_error",
  ].includes(value.code)
    && ["protocol", "artifact", "rust", "sqlite", "index", "query", "lifecycle"].includes(value.stage);
}

function isWorkerOperation(value: unknown): value is WorkerOperation {
  return value === "initialize"
    || value === "begin_build"
    || value === "add_source_batch"
    || value === "commit_build"
    || value === "abort_build"
    || value === "search"
    || value === "status"
    || value === "dispose";
}

function isRequestId(value: unknown): value is number {
  return typeof value === "number" && Number.isSafeInteger(value) && value >= 1;
}

function isNonNegativeSafeInteger(value: unknown): value is number {
  return typeof value === "number" && Number.isSafeInteger(value) && value >= 0;
}

function isBoundedString(value: unknown, maximum: number, allowEmpty = false): value is string {
  return typeof value === "string"
    && value.length <= maximum
    && (allowEmpty || value.length > 0);
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function hasExactKeys(value: Record<string, unknown>, keys: readonly string[]): boolean {
  const actual = Object.keys(value);
  return actual.length === keys.length && actual.every((key) => keys.includes(key));
}
