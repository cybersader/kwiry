// SPDX-FileCopyrightText: 2026 cybersader
// SPDX-License-Identifier: GPL-3.0-only

export const WORKER_PROTOCOL_VERSION = 3 as const;
export const WORKER_REQUEST_TIMEOUT_MS = 30_000;
export const MAX_PENDING_REQUESTS = 16;
export const MAX_BATCH_SOURCES = 16;
export const MAX_SOURCE_CHANGES = 16;
export const MAX_SOURCE_BYTES = 10 * 1024 * 1024 + 1;
export const MAX_BATCH_BYTES = 16 * 1024 * 1024;
export const MAX_GENERATION_CHARACTERS = 128;
export const MAX_QUERY_CHARACTERS = 4_096;
export const MAX_SEARCH_HITS = 100;
export const MAX_RECONCILIATION_SOURCES = 200_000;
export const MAX_RECONCILIATION_PLAN_PATHS = MAX_RECONCILIATION_SOURCES * 2;

/**
 * Version of the cache image format the Worker produces. It covers the SQLite
 * schema in `./fts5-index` (which stamps the same number into
 * `PRAGMA user_version`). Any schema edit must bump it: an image whose value
 * differs from the running build's is not restorable.
 */
export const CACHE_SCHEMA_VERSION = 1;

/**
 * Ceiling on a single exported generation image. Derived from the corpus
 * bounds the index already enforces: 256 MiB of indexed text in a contentless
 * FTS5 index is roughly 230 MiB of image, so an image above this means the
 * corpus invariants were already violated, and the correct outcome is a
 * refusal carrying no bytes rather than a truncated export.
 */
export const MAX_EXPORT_BLOB_BYTES = 384 * 1024 * 1024;

export const MAX_PLUGIN_ID_CHARACTERS = 128;
export const MAX_PLUGIN_VERSION_CHARACTERS = 64;

const SHA256_HEX_PATTERN = /^[0-9a-f]{64}$/u;

export type WorkerOperation =
  | "initialize"
  | "begin_build"
  | "add_source_batch"
  | "apply_source_changes"
  | "commit_build"
  | "abort_build"
  | "export_generation"
  | "restore_generation"
  | "plan_reconciliation"
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
  | "index_limit_exceeded"
  | "integrity_failed"
  | "cache_identity_mismatch"
  | "cache_version_mismatch"
  | "cache_digest_mismatch"
  | "cache_image_invalid"
  | "cache_blob_too_large"
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

/** Rust-authored skipped preparation without reading or transporting contents. */
export interface OversizedSourceInput {
  descriptor: SourceDescriptorInput;
  oversized: true;
}

export type SourceUpsert = SourceInput | OversizedSourceInput;

export interface ReconciliationSourceMetadata {
  path: string;
  byte_length: number;
  mtime_nanos: string;
  indexable: boolean;
}

export interface ReconciliationPlanResult {
  generation: string;
  unchanged: string[];
  refresh: string[];
  remove: string[];
  /** Number of rows in the restored freshness ledger before planning. */
  stored_source_count: number;
  /** Current paths that were present in that restored ledger. */
  matched_source_count: number;
}

export interface SourceRemoval {
  vault_id: string;
  path: string;
}

export interface RestoreGenerationInput {
  generation: string;
  bytes: Uint8Array;
  blob_byte_length: number;
  blob_sha256: string;
  /** B6.2 deliberately has not verified this digest; no other value is valid. */
  digest_verified: false;
  protocol_version: number;
  cache_schema_version: number;
  chunking_version: number;
  sqlite_version: string;
  sqlite_wasm_sha256: string;
  rust_wasm_sha256: string;
  plugin_id: string;
  plugin_version: string;
  cache_identity: string;
  /** Independently derived for the currently open vault, never copied from the record. */
  expected_cache_identity: string;
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
      sources: SourceUpsert[];
    })
  | (RequestBase & {
      operation: "apply_source_changes";
      generation: string;
      next_generation: string | null;
      upserts: SourceUpsert[];
      removals: SourceRemoval[];
    })
  | (RequestBase & { operation: "commit_build"; generation: string })
  | (RequestBase & { operation: "abort_build"; generation: string })
  | (RequestBase & {
      operation: "export_generation";
      generation: string;
      // Derived on the host from the canonical vault location and passed in as
      // an opaque digest. The Worker is persistence-blind: it never learns the
      // vault path, and bounding this to exactly 64 hex characters makes
      // passing a path here structurally impossible.
      cache_identity: string;
    })
  | (RequestBase & RestoreGenerationInput & { operation: "restore_generation" })
  | (RequestBase & {
      operation: "plan_reconciliation";
      generation: string;
      vault_id: string;
      current_sources: ReconciliationSourceMetadata[];
    })
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

/**
 * A sealed cache image plus the identity every field of which is authored by
 * its producer. The shape is flat on purpose: `generation` has to stay at the
 * top level so the existing RPC generation correlation applies unchanged.
 *
 * `bytes` is transferred, never copied, so `blob_byte_length` and
 * `blob_sha256` must be computed before the response leaves the Worker.
 */
export interface ExportGenerationResult {
  generation: string;
  documents: number;
  chunks: number;
  bytes: Uint8Array;
  blob_byte_length: number;
  blob_sha256: string;
  protocol_version: typeof WORKER_PROTOCOL_VERSION;
  cache_schema_version: typeof CACHE_SCHEMA_VERSION;
  chunking_version: number;
  sqlite_version: "3.53.0";
  sqlite_wasm_sha256: string;
  rust_wasm_sha256: string;
  plugin_id: string;
  plugin_version: string;
  cache_identity: string;
}

export type WorkerResult =
  | InitializeResult
  | BuildResult
  | StatusResult
  | SearchResult
  | ExportGenerationResult
  | ReconciliationPlanResult
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
    case "export_generation":
      return hasExactKeys(value, [...base, "generation", "cache_identity"])
        && isGeneration(value.generation)
        && isSha256Hex(value.cache_identity)
        ? value as unknown as WorkerRequest
        : fixedWorkerError("invalid_request", "protocol", "Invalid Worker request.", false);
    case "restore_generation":
      return parseRestoreGenerationRequest(value, base);
    case "plan_reconciliation":
      return hasExactKeys(value, [...base, "generation", "vault_id", "current_sources"])
        && isGeneration(value.generation)
        && isBoundedString(value.vault_id, 1_024)
        && isReconciliationSources(value.current_sources)
        ? value as unknown as WorkerRequest
        : fixedWorkerError("invalid_request", "protocol", "Invalid Worker request.", false);
    case "add_source_batch":
      return hasExactKeys(value, [...base, "generation", "sources"])
        && isGeneration(value.generation)
        && isSourceBatch(value.sources)
        ? value as unknown as WorkerRequest
        : fixedWorkerError("invalid_request", "protocol", "Invalid Worker request.", false);
    case "apply_source_changes":
      return hasExactKeys(value, [
        ...base,
        "generation",
        "next_generation",
        "upserts",
        "removals",
      ])
        && isGeneration(value.generation)
        && (value.next_generation === null || isGeneration(value.next_generation))
        && value.next_generation !== value.generation
        && isSourceChanges(value.upserts, value.removals)
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

function parseRestoreGenerationRequest(
  value: Record<string, unknown>,
  base: readonly string[],
): WorkerRequest | WorkerError {
  const keys = [
    ...base,
    "generation",
    "bytes",
    "blob_byte_length",
    "blob_sha256",
    "digest_verified",
    "protocol_version",
    "cache_schema_version",
    "chunking_version",
    "sqlite_version",
    "sqlite_wasm_sha256",
    "rust_wasm_sha256",
    "plugin_id",
    "plugin_version",
    "cache_identity",
    "expected_cache_identity",
  ];
  if (!hasExactKeys(value, keys)
    || !isGeneration(value.generation)
    || !(value.bytes instanceof Uint8Array)
    || !isNonNegativeSafeInteger(value.blob_byte_length)
    || !isSha256Hex(value.blob_sha256)
    || value.digest_verified !== false
    || !isNonNegativeSafeInteger(value.protocol_version)
    || !isNonNegativeSafeInteger(value.cache_schema_version)
    || !isNonNegativeSafeInteger(value.chunking_version)
    || !isBoundedString(value.sqlite_version, 64)
    || !isSha256Hex(value.sqlite_wasm_sha256)
    || !isSha256Hex(value.rust_wasm_sha256)
    || !isBoundedString(value.plugin_id, MAX_PLUGIN_ID_CHARACTERS)
    || !isBoundedString(value.plugin_version, MAX_PLUGIN_VERSION_CHARACTERS)
    || !isSha256Hex(value.cache_identity)
    || !isSha256Hex(value.expected_cache_identity)) {
    return fixedWorkerError("invalid_request", "protocol", "Invalid Worker request.", false);
  }
  if (value.bytes.byteLength > MAX_EXPORT_BLOB_BYTES
    || value.blob_byte_length > MAX_EXPORT_BLOB_BYTES) {
    return fixedWorkerError(
      "cache_blob_too_large",
      "protocol",
      "Cached generation exceeds the restore limit.",
      false,
    );
  }
  if (value.bytes.byteLength === 0 || value.blob_byte_length !== value.bytes.byteLength) {
    return fixedWorkerError(
      "cache_image_invalid",
      "protocol",
      "Cached generation has an invalid length.",
      false,
    );
  }
  return value as unknown as WorkerRequest;
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

function isSourceBatch(value: unknown, allowEmpty = false): value is SourceUpsert[] {
  if (!Array.isArray(value)
    || (!allowEmpty && value.length < 1)
    || value.length > MAX_BATCH_SOURCES) {
    return false;
  }
  let totalBytes = 0;
  for (const source of value) {
    if (!isRecord(source)) return false;
    if (hasExactKeys(source, ["descriptor", "bytes"])) {
      if (!isSourceDescriptor(source.descriptor)
        || !(source.bytes instanceof Uint8Array)
        || source.bytes.byteLength > MAX_SOURCE_BYTES
        || source.descriptor.byte_length !== source.bytes.byteLength) {
        return false;
      }
      totalBytes += source.bytes.byteLength;
      if (totalBytes > MAX_BATCH_BYTES) return false;
      continue;
    }
    if (!hasExactKeys(source, ["descriptor", "oversized"])
      || source.oversized !== true
      || !isSourceDescriptor(source.descriptor, true)
      || source.descriptor.byte_length < MAX_SOURCE_BYTES) {
      return false;
    }
  }
  return true;
}

function isReconciliationSources(value: unknown): value is ReconciliationSourceMetadata[] {
  if (!Array.isArray(value) || value.length > MAX_RECONCILIATION_SOURCES) return false;
  const paths = new Set<string>();
  for (const source of value) {
    if (!isRecord(source)
      || !hasExactKeys(source, ["path", "byte_length", "mtime_nanos", "indexable"])
      || !isNormalizedVaultRelativePath(source.path)
      || !isNonNegativeSafeInteger(source.byte_length)
      || typeof source.mtime_nanos !== "string"
      || !/^[0-9]{1,39}$/u.test(source.mtime_nanos)
      || typeof source.indexable !== "boolean"
      || paths.has(source.path)) {
      return false;
    }
    paths.add(source.path);
  }
  return true;
}

function isSourceChanges(upserts: unknown, removals: unknown): boolean {
  if (!isSourceBatch(upserts, true)
    || !Array.isArray(removals)
    || !removals.every(isSourceRemoval)
    || upserts.length + removals.length < 1
    || upserts.length + removals.length > MAX_SOURCE_CHANGES) {
    return false;
  }

  const upsertIdentities = new Set<string>();
  for (const source of upserts) {
    const identity = sourceIdentity(source.descriptor.vault_id, source.descriptor.path);
    if (upsertIdentities.has(identity)) return false;
    upsertIdentities.add(identity);
  }
  const removalIdentities = new Set<string>();
  for (const removal of removals) {
    const identity = sourceIdentity(removal.vault_id, removal.path);
    if (removalIdentities.has(identity) || upsertIdentities.has(identity)) return false;
    removalIdentities.add(identity);
  }
  return true;
}

function isSourceRemoval(value: unknown): value is SourceRemoval {
  return isRecord(value)
    && hasExactKeys(value, ["vault_id", "path"])
    && isBoundedString(value.vault_id, 1_024)
    && value.vault_id.trim().length > 0
    && isNormalizedVaultRelativePath(value.path);
}

function sourceIdentity(vaultId: string, path: string): string {
  return JSON.stringify([vaultId, path]);
}

function isSourceDescriptor(value: unknown, allowOversized = false): value is SourceDescriptorInput {
  if (!isRecord(value)) return false;
  const required = ["vault_id", "path", "format", "byte_length", "mtime", "mtime_nanos"];
  const allowed = value.room === undefined ? required : [...required, "room"];
  return hasExactKeys(value, allowed)
    && isBoundedString(value.vault_id, 1_024)
    && (value.room === undefined || isBoundedString(value.room, 1_024))
    && isBoundedString(value.path, 4_096)
    && (value.format === "markdown" || value.format === "text")
    && isNonNegativeSafeInteger(value.byte_length)
    && (allowOversized || Number(value.byte_length) <= MAX_SOURCE_BYTES)
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
    case "apply_source_changes":
    case "commit_build":
    case "abort_build":
    case "restore_generation":
      return isBuildResult(value);
    case "export_generation":
      return isExportGenerationResult(value);
    case "plan_reconciliation":
      return isReconciliationPlanResult(value);
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

function isExportGenerationResult(value: unknown): value is ExportGenerationResult {
  return isRecord(value)
    && hasExactKeys(value, [
      "generation",
      "documents",
      "chunks",
      "bytes",
      "blob_byte_length",
      "blob_sha256",
      "protocol_version",
      "cache_schema_version",
      "chunking_version",
      "sqlite_version",
      "sqlite_wasm_sha256",
      "rust_wasm_sha256",
      "plugin_id",
      "plugin_version",
      "cache_identity",
    ])
    && isGeneration(value.generation)
    && isNonNegativeSafeInteger(value.documents)
    && isNonNegativeSafeInteger(value.chunks)
    && value.bytes instanceof Uint8Array
    && value.bytes.byteLength > 0
    && value.bytes.byteLength <= MAX_EXPORT_BLOB_BYTES
    // The declared length must equal the buffer that actually arrived, the
    // same cross-check the inbound source descriptors get.
    && value.blob_byte_length === value.bytes.byteLength
    && isSha256Hex(value.blob_sha256)
    && value.protocol_version === WORKER_PROTOCOL_VERSION
    && value.cache_schema_version === CACHE_SCHEMA_VERSION
    && isNonNegativeSafeInteger(value.chunking_version)
    && value.sqlite_version === "3.53.0"
    && isSha256Hex(value.sqlite_wasm_sha256)
    && isSha256Hex(value.rust_wasm_sha256)
    && isBoundedString(value.plugin_id, MAX_PLUGIN_ID_CHARACTERS)
    && isBoundedString(value.plugin_version, MAX_PLUGIN_VERSION_CHARACTERS)
    && isSha256Hex(value.cache_identity);
}

function isReconciliationPlanResult(value: unknown): value is ReconciliationPlanResult {
  if (!isRecord(value)
    || !hasExactKeys(value, [
      "generation",
      "unchanged",
      "refresh",
      "remove",
      "stored_source_count",
      "matched_source_count",
    ])
    || !isGeneration(value.generation)
    || !isNonNegativeSafeInteger(value.stored_source_count)
    || value.stored_source_count > MAX_RECONCILIATION_SOURCES
    || !isNonNegativeSafeInteger(value.matched_source_count)
    || value.matched_source_count > MAX_RECONCILIATION_SOURCES) {
    return false;
  }
  const groups = [value.unchanged, value.refresh, value.remove];
  if (!groups.every((group) => Array.isArray(group)
    && group.length <= MAX_RECONCILIATION_SOURCES
    && group.every(isNormalizedVaultRelativePath))) {
    return false;
  }
  const unchanged = value.unchanged as string[];
  const refresh = value.refresh as string[];
  const remove = value.remove as string[];
  const paths = groups.flat() as string[];
  const currentCount = unchanged.length + refresh.length;
  return currentCount <= MAX_RECONCILIATION_SOURCES
    && paths.length <= MAX_RECONCILIATION_PLAN_PATHS
    && new Set(paths).size === paths.length
    && unchanged.length <= value.matched_source_count
    && value.matched_source_count <= currentCount
    && value.matched_source_count + remove.length === value.stored_source_count;
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
    // The index is contentless, so the Worker has no text to snippet and the
    // host hydrates the excerpt from the vault file. The frozen hit shape keeps
    // the field; a Worker that filled it again would be regressing the ruling,
    // so the empty string is enforced rather than merely length-bounded.
    && value.excerpt === ""
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
    "index_limit_exceeded",
    "integrity_failed",
    "cache_identity_mismatch",
    "cache_version_mismatch",
    "cache_digest_mismatch",
    "cache_image_invalid",
    "cache_blob_too_large",
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
    || value === "apply_source_changes"
    || value === "commit_build"
    || value === "abort_build"
    || value === "export_generation"
    || value === "restore_generation"
    || value === "plan_reconciliation"
    || value === "search"
    || value === "status"
    || value === "dispose";
}

export function isSha256Hex(value: unknown): value is string {
  return typeof value === "string" && SHA256_HEX_PATTERN.test(value);
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

function isNormalizedVaultRelativePath(value: unknown): value is string {
  if (!isBoundedString(value, 4_096)
    || value.startsWith("/")
    || value.endsWith("/")
    || value.includes("\\")
    || value.includes("\0")) {
    return false;
  }
  return value.split("/").every(
    (component) => component.length > 0 && component !== "." && component !== "..",
  );
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function hasExactKeys(value: Record<string, unknown>, keys: readonly string[]): boolean {
  const actual = Object.keys(value);
  return actual.length === keys.length && actual.every((key) => keys.includes(key));
}
