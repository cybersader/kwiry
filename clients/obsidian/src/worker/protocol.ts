// SPDX-FileCopyrightText: 2026 cybersader
// SPDX-License-Identifier: GPL-3.0-only

export const WORKER_PROTOCOL_VERSION = 12 as const;
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
export const SOURCE_QUARANTINE_WARNING_CODE = "source_rejected" as const;
export const SOURCE_PREPARATION_DEFECT_FIELDS = [
  "not_a_record",
  "preparation_fields",
  "schema_version",
  "source_key",
  "vault_id",
  "room",
  "path",
  "format",
  "extraction_profile",
  "coverage",
  "content_hash",
  "byte_length",
  "mtime",
  "mtime_nanos",
  "retrieval",
  "normalized_exact",
  "chunks_shape",
  "chunks_contents",
  "chunks_source_correlation",
  "chunks_content_role",
  "chunks_source_locator",
  "frontmatter_not_a_record",
  "frontmatter_property_value",
  "frontmatter_property_nesting",
  "frontmatter_property_cycle",
  "kind",
  "warning",
  "skipped_has_chunks",
  "indexed_missing_hash",
] as const;
export type SourcePreparationDefectField = typeof SOURCE_PREPARATION_DEFECT_FIELDS[number];

/**
 * Version of the cache image format the Worker produces. It covers the SQLite
 * schema in `./fts5-index` (which stamps the same number into
 * `PRAGMA user_version`). Any schema edit must bump it: an image whose value
 * differs from the running build's is not restorable.
 */
export const CACHE_SCHEMA_VERSION = 11;
export const INITIAL_BUILD_CHECKPOINT_RECORD_VERSION = 1 as const;
export const INITIAL_BUILD_CHECKPOINT_IMAGE_VERSION = 1 as const;
export const INITIAL_BUILD_CHECKPOINT_RECORD_KIND = "initial_build_checkpoint" as const;

/**
 * Independent ceiling on a transported generation image. The SQLite adapter's
 * durable page budget is configured below this value, so a compliant compact
 * database fits without truncation; this remains a defense at the RPC boundary.
 */
export const MAX_EXPORT_BLOB_BYTES = 384 * 1024 * 1024;

export const MAX_PLUGIN_ID_CHARACTERS = 128;
export const MAX_PLUGIN_VERSION_CHARACTERS = 64;

const SHA256_HEX_PATTERN = /^[0-9a-f]{64}$/u;

export const SOURCE_FORMATS = [
  "markdown",
  "text",
  "base",
  "canvas",
  "docx",
  "pdf",
  "excalidraw",
  "excel",
] as const;
export type SourceFormat = typeof SOURCE_FORMATS[number];

export const EXTRACTION_COVERAGES = [
  "indexed-complete",
  "indexed-partial",
  "skipped-no-extractable-text",
  "unreadable",
  "quarantined",
] as const;
export type ExtractionCoverage = typeof EXTRACTION_COVERAGES[number];

/**
 * Mirrors `kwiry_core::policy::ExtractionProfile`: which extractor set produced
 * a preparation. `none` means no extractor was compiled for the format at all.
 * This plugin only ever produces `portable`.
 */
export const EXTRACTION_PROFILES = ["none", "portable", "enhanced"] as const;
export type ExtractionProfile = typeof EXTRACTION_PROFILES[number];

/**
 * Mirrors `kwiry_core::extract::SourceLocator`. Non-ranking navigation metadata:
 * it is stored on the chunk row but is not a `chunk_search` column, so it can
 * never reach MATCH or BM25. Each variant pairs with exactly one source format;
 * see `locatorMatchesFormat`.
 */
export type SourceLocator =
  | { kind: "base_view"; view: string }
  | { kind: "pdf_page"; page: number }
  | { kind: "excel_cell"; sheet: string; cell: string };
export type SourceFormatCounts = Record<
  SourceFormat,
  Record<ExtractionCoverage, number>
>;

export function emptySourceFormatCounts(): SourceFormatCounts {
  return Object.fromEntries(SOURCE_FORMATS.map((format) => [
    format,
    Object.fromEntries(EXTRACTION_COVERAGES.map((coverage) => [coverage, 0])),
  ])) as SourceFormatCounts;
}

/** One count per compiled format. Absent formats are `0`, never missing. */
export type SourceFormatTally = Record<SourceFormat, number>;

export function emptySourceFormatTally(): SourceFormatTally {
  return Object.fromEntries(SOURCE_FORMATS.map((format) => [format, 0])) as SourceFormatTally;
}

export function isSourceFormatTally(value: unknown): value is SourceFormatTally {
  return isRecord(value)
    && hasExactKeys(value, [...SOURCE_FORMATS])
    && SOURCE_FORMATS.every((format) => isNonNegativeSafeInteger(value[format]));
}

/**
 * What a restore refused to reuse, per format and per reason.
 *
 * This is what makes a partial reuse reportable truthfully. A restore that
 * evicts only PDF rows is not a rebuild, and the two reasons are different
 * statements to the user: `stale_identity` rows will be read again, while
 * `disabled_format` rows are simply gone because the user asked for that.
 */
export interface RestoreEvictionReport {
  stale_identity: SourceFormatTally;
  disabled_format: SourceFormatTally;
}

export function emptyRestoreEvictionReport(): RestoreEvictionReport {
  return { stale_identity: emptySourceFormatTally(), disabled_format: emptySourceFormatTally() };
}

export function isRestoreEvictionReport(value: unknown): value is RestoreEvictionReport {
  return isRecord(value)
    && hasExactKeys(value, ["stale_identity", "disabled_format"])
    && isSourceFormatTally(value.stale_identity)
    && isSourceFormatTally(value.disabled_format);
}

/** The formats with at least one evicted row, in compiled order. */
export function evictedFormats(tally: SourceFormatTally): SourceFormat[] {
  return SOURCE_FORMATS.filter((format) => tally[format] > 0);
}

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
  | "export_initial_build_checkpoint"
  | "restore_initial_build_checkpoint"
  | "plan_initial_build_checkpoint_reconciliation"
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
  | "explicit_query_unsupported"
  | "invalid_query"
  | "invalid_query_plan"
  | "query_execution_failed"
  | "index_building"
  | "index_limit_exceeded"
  | "integrity_failed"
  | "cache_identity_mismatch"
  | "cache_version_mismatch"
  | "cache_digest_mismatch"
  | "cache_image_invalid"
  | "cache_blob_too_large"
  | "checkpoint_kind_mismatch"
  | "checkpoint_identity_mismatch"
  | "checkpoint_version_mismatch"
  | "checkpoint_digest_mismatch"
  | "checkpoint_image_invalid"
  | "checkpoint_blob_too_large"
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
  format: SourceFormat;
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
  /** Metadata-identical oversized skips; no content exists to hash. */
  unchanged: string[];
  /** Metadata-identical indexed sources that require raw-byte hash audit. */
  audit: Array<{ path: string; content_hash: string }>;
  refresh: string[];
  remove: string[];
  /** Number of rows in the restored freshness ledger before planning. */
  stored_source_count: number;
  /** Current paths that were present in that restored ledger. */
  matched_source_count: number;
}

export interface InitialBuildCheckpointCursor {
  snapshot_source_count: number;
  acknowledged_add_batches: number;
  acknowledged_prefix_sources: number;
  last_acknowledged_path: string | null;
}

export interface InitialBuildCheckpointReconciliationPlanResult
  extends ReconciliationPlanResult {
  publication: "initial_staging";
  searchable: false;
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
  source_policy_hash: string;
  /** Independently derived for the currently open vault, never copied from the record. */
  expected_cache_identity: string;
  /** Independently computed from the currently enabled source formats and source schema. */
  expected_source_policy_hash: string;
}

export interface RestoreInitialBuildCheckpointInput extends RestoreGenerationInput {
  record_kind: string;
  checkpoint_record_version: number;
  checkpoint_image_version: number;
  cursor: InitialBuildCheckpointCursor;
}

interface RequestBase {
  version: typeof WORKER_PROTOCOL_VERSION;
  id: number;
}

export type WorkerRequest =
  | (RequestBase & {
      operation: "initialize";
      vault_id: string;
      /**
       * The **core** policy identity: the facts for which no row of any format
       * is reusable. Per-format identities are compiled into the Worker and
       * never travel here.
       */
      source_policy_hash: string;
      /**
       * The formats the user currently wants indexed, sorted and deduplicated.
       *
       * Configuration, not identity: it is never digested and never persisted.
       * The Worker holds it so a restored image can be projected down to the
       * enabled set before it is published as searchable — otherwise disabling
       * a format would leave its rows answering queries for the whole of the
       * following reconciliation.
       */
      enabled_source_formats: SourceFormat[];
    })
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
      operation: "export_initial_build_checkpoint";
      generation: string;
      cache_identity: string;
      cursor: InitialBuildCheckpointCursor;
    })
  | (RequestBase & RestoreInitialBuildCheckpointInput & {
      operation: "restore_initial_build_checkpoint";
    })
  | (RequestBase & {
      operation: "plan_initial_build_checkpoint_reconciliation";
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
  rustAbiVersion: 3;
  sourceSchemaVersion: 9;
  querySchemaVersion: 6;
  matchPlanSchemaVersion: 5;
  sqliteVersion: "3.53.0";
  fts5Enabled: 1;
}

export interface BuildResult {
  generation: string;
  documents: number;
  chunks: number;
  database_bytes: number;
  database_byte_limit: number;
  quarantined_sources: number;
  quarantine_fields: SourcePreparationDefectField[];
  source_format_counts: SourceFormatCounts;
}

export interface StatusResult {
  phase: "ready" | "building" | "disposed" | "failed";
  searchable: boolean;
  active_generation: string | null;
  staging_generation: string | null;
  documents: number;
  chunks: number;
  active_database_bytes: number;
  staging_database_bytes: number;
  database_byte_limit: number;
  source_format_counts: SourceFormatCounts;
  dirty: boolean;
  rebuilding: boolean;
}

export type PropertyValue =
  | null
  | boolean
  | number
  | string
  | PropertyValue[]
  | PropertyBag;

/**
 * The complete note-authored property projection. Property policy is applied at
 * query time; narrowing this shape would make a later policy change require a
 * second rebuild of the vault.
 */
export interface PropertyBag {
  [name: string]: PropertyValue;
}

/** Compact ordinary-search metadata. The complete source-owned bag stays in SQLite. */
export interface WorkerFrontmatter {
  title?: string;
}

export interface WorkerSearchHit {
  chunk_id: string;
  vault_id: string;
  path: string;
  format: SourceFormat;
  coverage: ExtractionCoverage;
  locator: SourceLocator | null;
  heading_path: string[];
  score: number;
  excerpt: string;
  frontmatter: WorkerFrontmatter;
}

export const CANDIDATE_WINDOW_STATES = [
  "exhausted",
  "more_available",
  "candidate_limit_reached",
  "unknown",
] as const;
export type CandidateWindowState = typeof CANDIDATE_WINDOW_STATES[number];

/**
 * Closed evidence from the bounded in-plugin collector. `candidate_count` is the
 * number of candidate rows actually inspected, never a corpus/result total.
 */
export interface WorkerCandidateWindow {
  state: CandidateWindowState;
  candidate_count: number;
  candidate_limit: 512;
}

export interface SearchResult {
  generation: string;
  hits: WorkerSearchHit[];
  candidate_window: WorkerCandidateWindow;
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
  source_policy_hash: string;
}

export interface InitialBuildCheckpointExportResult extends BuildResult {
  record_kind: typeof INITIAL_BUILD_CHECKPOINT_RECORD_KIND;
  checkpoint_record_version: typeof INITIAL_BUILD_CHECKPOINT_RECORD_VERSION;
  checkpoint_image_version: typeof INITIAL_BUILD_CHECKPOINT_IMAGE_VERSION;
  publication: "initial_staging";
  searchable: false;
  cursor: InitialBuildCheckpointCursor;
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
  source_policy_hash: string;
}

/**
 * A restored generation, plus what the restore refused to reuse.
 *
 * The report is part of the result rather than a later query because the host
 * has to describe the restore honestly at the moment it publishes it: "cached
 * index searchable, reindexing PDF" and "rebuilding the index" are different
 * claims, and only the Worker knows which one happened.
 */
export interface RestoreGenerationResult extends BuildResult {
  evictions: RestoreEvictionReport;
}

export interface RestoreInitialBuildCheckpointResult extends BuildResult {
  record_kind: typeof INITIAL_BUILD_CHECKPOINT_RECORD_KIND;
  publication: "initial_staging";
  searchable: false;
  cursor: InitialBuildCheckpointCursor;
  evictions: RestoreEvictionReport;
}

export type WorkerResult =
  | InitializeResult
  | BuildResult
  | RestoreGenerationResult
  | StatusResult
  | SearchResult
  | ExportGenerationResult
  | InitialBuildCheckpointExportResult
  | RestoreInitialBuildCheckpointResult
  | ReconciliationPlanResult
  | InitialBuildCheckpointReconciliationPlanResult
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
      return hasExactKeys(value, [
        ...base,
        "vault_id",
        "source_policy_hash",
        "enabled_source_formats",
      ])
        && isBoundedString(value.vault_id, 1_024)
        && value.vault_id.trim().length > 0
        && isSha256Hex(value.source_policy_hash)
        && isSortedSourceFormatSet(value.enabled_source_formats)
        ? value as unknown as WorkerRequest
        : fixedWorkerError("invalid_request", "protocol", "Invalid Worker request.", false);
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
    case "export_initial_build_checkpoint":
      return hasExactKeys(value, [...base, "generation", "cache_identity", "cursor"])
        && isGeneration(value.generation)
        && isSha256Hex(value.cache_identity)
        && isInitialBuildCheckpointCursor(value.cursor)
        ? value as unknown as WorkerRequest
        : fixedWorkerError("invalid_request", "protocol", "Invalid Worker request.", false);
    case "restore_generation":
      return parseRestoreGenerationRequest(value, base);
    case "restore_initial_build_checkpoint":
      return parseRestoreInitialBuildCheckpointRequest(value, base);
    case "plan_reconciliation":
    case "plan_initial_build_checkpoint_reconciliation":
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
      if (!hasExactKeys(value, [...base, "query", "limit"])
        || typeof value.query !== "string"
        || !Number.isSafeInteger(value.limit)
        || Number(value.limit) < 1
        || Number(value.limit) > MAX_SEARCH_HITS) {
        return fixedWorkerError("invalid_request", "protocol", "Invalid Worker request.", false);
      }
      return value.query.trim().length > 0 && value.query.length <= MAX_QUERY_CHARACTERS
        ? value as unknown as WorkerRequest
        : fixedWorkerError(
            "invalid_query",
            "query",
            "The query is invalid or exceeds the supported limits.",
            false,
          );
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
    "source_policy_hash",
    "expected_cache_identity",
    "expected_source_policy_hash",
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
    || !isSha256Hex(value.source_policy_hash)
    || !isSha256Hex(value.expected_cache_identity)
    || !isSha256Hex(value.expected_source_policy_hash)) {
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

function parseRestoreInitialBuildCheckpointRequest(
  value: Record<string, unknown>,
  base: readonly string[],
): WorkerRequest | WorkerError {
  const keys = [
    ...base,
    "record_kind",
    "checkpoint_record_version",
    "checkpoint_image_version",
    "generation",
    "cursor",
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
    "source_policy_hash",
    "expected_cache_identity",
    "expected_source_policy_hash",
  ];
  if (!hasExactKeys(value, keys)
    || !isBoundedString(value.record_kind, 64)
    || !isNonNegativeSafeInteger(value.checkpoint_record_version)
    || !isNonNegativeSafeInteger(value.checkpoint_image_version)
    || !isGeneration(value.generation)
    || !isInitialBuildCheckpointCursor(value.cursor)
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
    || !isSha256Hex(value.source_policy_hash)
    || !isSha256Hex(value.expected_cache_identity)
    || !isSha256Hex(value.expected_source_policy_hash)) {
    return fixedWorkerError("invalid_request", "protocol", "Invalid Worker request.", false);
  }
  if (value.bytes.byteLength > MAX_EXPORT_BLOB_BYTES
    || value.blob_byte_length > MAX_EXPORT_BLOB_BYTES) {
    return fixedWorkerError(
      "checkpoint_blob_too_large",
      "protocol",
      "Initial-build checkpoint exceeds the restore limit.",
      false,
    );
  }
  if (value.bytes.byteLength === 0 || value.blob_byte_length !== value.bytes.byteLength) {
    return fixedWorkerError(
      "checkpoint_image_invalid",
      "protocol",
      "Initial-build checkpoint has an invalid length.",
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

export function isSourceBatch(value: unknown, allowEmpty = false): value is SourceUpsert[] {
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

function isInitialBuildCheckpointCursor(
  value: unknown,
): value is InitialBuildCheckpointCursor {
  if (!isRecord(value)
    || !hasExactKeys(value, [
      "snapshot_source_count",
      "acknowledged_add_batches",
      "acknowledged_prefix_sources",
      "last_acknowledged_path",
    ])
    || !isNonNegativeSafeInteger(value.snapshot_source_count)
    || value.snapshot_source_count > MAX_RECONCILIATION_SOURCES
    || !isNonNegativeSafeInteger(value.acknowledged_add_batches)
    || value.acknowledged_add_batches > value.snapshot_source_count
    || !isNonNegativeSafeInteger(value.acknowledged_prefix_sources)
    || value.acknowledged_prefix_sources > value.snapshot_source_count) {
    return false;
  }
  return value.acknowledged_prefix_sources === 0
    ? value.last_acknowledged_path === null
    : value.acknowledged_add_batches > 0
      && isNormalizedVaultRelativePath(value.last_acknowledged_path);
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

export function isSourceChanges(upserts: unknown, removals: unknown): boolean {
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
    && isSourceFormat(value.format)
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
      return isBuildResult(value);
    case "restore_generation":
      return isRestoreGenerationResult(value);
    case "restore_initial_build_checkpoint":
      return isRestoreInitialBuildCheckpointResult(value);
    case "export_generation":
      return isExportGenerationResult(value);
    case "export_initial_build_checkpoint":
      return isInitialBuildCheckpointExportResult(value);
    case "plan_reconciliation":
      return isReconciliationPlanResult(value);
    case "plan_initial_build_checkpoint_reconciliation":
      return isInitialBuildCheckpointReconciliationPlanResult(value);
    case "status":
      return isStatusResult(value);
    case "search":
      return isSearchResult(value);
    case "dispose":
      return isRecord(value) && hasExactKeys(value, ["closed"]) && value.closed === true;
  }
}

export function isInitializeResult(value: unknown): value is InitializeResult {
  return isRecord(value)
    && hasExactKeys(value, [
      "rustAbiVersion",
      "sourceSchemaVersion",
      "querySchemaVersion",
      "matchPlanSchemaVersion",
      "sqliteVersion",
      "fts5Enabled",
    ])
    && value.rustAbiVersion === 3
    && value.sourceSchemaVersion === 9
    && value.querySchemaVersion === 6
    && value.matchPlanSchemaVersion === 5
    && value.sqliteVersion === "3.53.0"
    && value.fts5Enabled === 1;
}

const BUILD_RESULT_KEYS = [
  "generation",
  "documents",
  "chunks",
  "database_bytes",
  "database_byte_limit",
  "quarantined_sources",
  "quarantine_fields",
  "source_format_counts",
] as const;

export function isBuildResult(value: unknown): value is BuildResult {
  return isRecord(value)
    && hasExactKeys(value, BUILD_RESULT_KEYS)
    && hasValidBuildResultFields(value);
}

export function isRestoreGenerationResult(value: unknown): value is RestoreGenerationResult {
  return isRecord(value)
    && hasExactKeys(value, [...BUILD_RESULT_KEYS, "evictions"])
    && hasValidBuildResultFields(value)
    && isRestoreEvictionReport(value.evictions);
}

function hasValidBuildResultFields(value: Record<string, unknown>): boolean {
  return isGeneration(value.generation)
    && isNonNegativeSafeInteger(value.documents)
    && isNonNegativeSafeInteger(value.chunks)
    && isNonNegativeSafeInteger(value.database_bytes)
    && isPositiveSafeInteger(value.database_byte_limit)
    && value.database_bytes <= value.database_byte_limit
    && isNonNegativeSafeInteger(value.quarantined_sources)
    && Array.isArray(value.quarantine_fields)
    && value.quarantine_fields.length <= SOURCE_PREPARATION_DEFECT_FIELDS.length
    && value.quarantine_fields.every(isSourcePreparationDefectField)
    && new Set(value.quarantine_fields).size === value.quarantine_fields.length
    && isSourceFormatCounts(value.source_format_counts)
    && indexedSourceCount(value.source_format_counts) === value.documents;
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
      "source_policy_hash",
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
    && isSha256Hex(value.cache_identity)
    && isSha256Hex(value.source_policy_hash);
}

function isInitialBuildCheckpointExportResult(
  value: unknown,
): value is InitialBuildCheckpointExportResult {
  return isRecord(value)
    && hasExactKeys(value, [
      ...BUILD_RESULT_KEYS,
      "record_kind",
      "checkpoint_record_version",
      "checkpoint_image_version",
      "publication",
      "searchable",
      "cursor",
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
      "source_policy_hash",
    ])
    && hasValidBuildResultFields(value)
    && value.record_kind === INITIAL_BUILD_CHECKPOINT_RECORD_KIND
    && value.checkpoint_record_version === INITIAL_BUILD_CHECKPOINT_RECORD_VERSION
    && value.checkpoint_image_version === INITIAL_BUILD_CHECKPOINT_IMAGE_VERSION
    && value.publication === "initial_staging"
    && value.searchable === false
    && isInitialBuildCheckpointCursor(value.cursor)
    && value.bytes instanceof Uint8Array
    && value.bytes.byteLength > 0
    && value.bytes.byteLength <= MAX_EXPORT_BLOB_BYTES
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
    && isSha256Hex(value.cache_identity)
    && isSha256Hex(value.source_policy_hash);
}

function isRestoreInitialBuildCheckpointResult(
  value: unknown,
): value is RestoreInitialBuildCheckpointResult {
  return isRecord(value)
    && hasExactKeys(value, [
      ...BUILD_RESULT_KEYS,
      "record_kind",
      "publication",
      "searchable",
      "cursor",
      "evictions",
    ])
    && hasValidBuildResultFields(value)
    && value.record_kind === INITIAL_BUILD_CHECKPOINT_RECORD_KIND
    && value.publication === "initial_staging"
    && value.searchable === false
    && isInitialBuildCheckpointCursor(value.cursor)
    && isRestoreEvictionReport(value.evictions);
}

function isInitialBuildCheckpointReconciliationPlanResult(
  value: unknown,
): value is InitialBuildCheckpointReconciliationPlanResult {
  if (!isRecord(value)
    || value.publication !== "initial_staging"
    || value.searchable !== false) {
    return false;
  }
  const { publication: _publication, searchable: _searchable, ...plan } = value;
  return isReconciliationPlanResult(plan);
}

function isReconciliationPlanResult(value: unknown): value is ReconciliationPlanResult {
  if (!isRecord(value)
    || !hasExactKeys(value, [
      "generation",
      "unchanged",
      "audit",
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
    && group.every(isNormalizedVaultRelativePath))
    || !Array.isArray(value.audit)
    || value.audit.length > MAX_RECONCILIATION_SOURCES
    || !value.audit.every((entry) => isRecord(entry)
      && hasExactKeys(entry, ["path", "content_hash"])
      && isNormalizedVaultRelativePath(entry.path)
      && isSha256Hex(entry.content_hash))) {
    return false;
  }
  const unchanged = value.unchanged as string[];
  const audit = value.audit as Array<{ path: string; content_hash: string }>;
  const refresh = value.refresh as string[];
  const remove = value.remove as string[];
  const paths = [...groups.flat() as string[], ...audit.map((entry) => entry.path)];
  const currentCount = unchanged.length + audit.length + refresh.length;
  return currentCount <= MAX_RECONCILIATION_SOURCES
    && paths.length <= MAX_RECONCILIATION_PLAN_PATHS
    && new Set(paths).size === paths.length
    && unchanged.length + audit.length <= value.matched_source_count
    && value.matched_source_count <= currentCount
    && value.matched_source_count + remove.length === value.stored_source_count;
}

export function isStatusResult(value: unknown): value is StatusResult {
  return isRecord(value)
    && hasExactKeys(value, [
      "phase",
      "searchable",
      "active_generation",
      "staging_generation",
      "documents",
      "chunks",
      "active_database_bytes",
      "staging_database_bytes",
      "database_byte_limit",
      "source_format_counts",
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
    && isNonNegativeSafeInteger(value.active_database_bytes)
    && isNonNegativeSafeInteger(value.staging_database_bytes)
    && isPositiveSafeInteger(value.database_byte_limit)
    && value.active_database_bytes <= value.database_byte_limit
    && value.staging_database_bytes <= value.database_byte_limit
    && isSourceFormatCounts(value.source_format_counts)
    && indexedSourceCount(value.source_format_counts) === value.documents
    && typeof value.dirty === "boolean"
    && typeof value.rebuilding === "boolean";
}

export function isSearchResult(value: unknown): value is SearchResult {
  return isRecord(value)
    && hasExactKeys(value, ["generation", "hits", "candidate_window"])
    && isGeneration(value.generation)
    && Array.isArray(value.hits)
    && value.hits.length <= MAX_SEARCH_HITS
    && value.hits.every(isSearchHit)
    && isWorkerCandidateWindow(value.candidate_window)
    && value.candidate_window.candidate_count >= value.hits.length
    && (value.candidate_window.state !== "candidate_limit_reached"
      || value.candidate_window.candidate_count === value.candidate_window.candidate_limit)
    && (value.candidate_window.state !== "more_available"
      || value.candidate_window.candidate_count > value.hits.length);
}

function isWorkerCandidateWindow(value: unknown): value is WorkerCandidateWindow {
  return isRecord(value)
    && hasExactKeys(value, ["state", "candidate_count", "candidate_limit"])
    && CANDIDATE_WINDOW_STATES.includes(value.state as CandidateWindowState)
    && isNonNegativeSafeInteger(value.candidate_count)
    && value.candidate_limit === 512
    && value.candidate_count <= value.candidate_limit;
}

function isSearchHit(value: unknown): value is WorkerSearchHit {
  return isRecord(value)
    && hasExactKeys(value, [
      "chunk_id",
      "vault_id",
      "path",
      "format",
      "coverage",
      "locator",
      "heading_path",
      "score",
      "excerpt",
      "frontmatter",
    ])
    && isBoundedString(value.chunk_id, 1_024)
    && isBoundedString(value.vault_id, 1_024)
    && isBoundedString(value.path, 4_096)
    && isSourceFormat(value.format)
    && (value.coverage === "indexed-complete" || value.coverage === "indexed-partial")
    && (value.locator === null || isSourceLocator(value.locator))
    && locatorMatchesFormat(value.locator as SourceLocator | null, value.format)
    && Array.isArray(value.heading_path)
    && value.heading_path.length <= 64
    && value.heading_path.every((heading) => isBoundedString(heading, 1_024))
    && typeof value.score === "number"
    && Number.isFinite(value.score)
    // Excerpts are hydrated only after final hit selection from the stored
    // canonical chunk content. They never participate in MATCH, BM25, or ordering.
    && isBoundedString(value.excerpt, 16_384, true)
    && isFrontmatter(value.frontmatter);
}

function isFrontmatter(value: unknown): value is WorkerFrontmatter {
  return isRecord(value)
    && Object.keys(value).every((key) => key === "title")
    && (value.title === undefined || isBoundedString(value.title, 1_024, true));
}

export function isSourceFormat(value: unknown): value is SourceFormat {
  return typeof value === "string" && (SOURCE_FORMATS as readonly string[]).includes(value);
}

/**
 * A sorted, duplicate-free subset of the compiled formats.
 *
 * Sortedness is required rather than merely tolerated: the enabled set decides
 * which restored rows are deleted before publication, and an ordering the
 * sender chose freely is an ordering a test can disagree with silently. The
 * empty set is legal — a user may disable every format.
 */
export function isSortedSourceFormatSet(value: unknown): value is SourceFormat[] {
  if (!Array.isArray(value) || value.length > SOURCE_FORMATS.length) return false;
  let previous: string | null = null;
  for (const entry of value) {
    if (!isSourceFormat(entry)) return false;
    if (previous !== null && entry <= previous) return false;
    previous = entry;
  }
  return true;
}

export function isExtractionCoverage(value: unknown): value is ExtractionCoverage {
  return typeof value === "string"
    && (EXTRACTION_COVERAGES as readonly string[]).includes(value);
}

export function isSourceLocator(value: unknown): value is SourceLocator {
  if (!isRecord(value)) return false;
  if (hasExactKeys(value, ["kind", "view"])) {
    return value.kind === "base_view" && isBoundedString(value.view, 4_096);
  }
  if (hasExactKeys(value, ["kind", "page"])) {
    return value.kind === "pdf_page" && isPdfPageNumber(value.page);
  }
  if (hasExactKeys(value, ["kind", "sheet", "cell"])) {
    return value.kind === "excel_cell"
      && isBoundedString(value.sheet, 4_096)
      && isBoundedString(value.cell, 32);
  }
  return false;
}

/**
 * Bounded by the Rust field's `u32` domain, not by the extractor's page ceiling.
 * The ceiling is extraction policy and may move; the type domain cannot.
 */
function isPdfPageNumber(value: unknown): value is number {
  return typeof value === "number"
    && Number.isSafeInteger(value)
    && value >= 1
    && value <= 4_294_967_295;
}

/**
 * The single format-to-locator pairing rule for the in-plugin profile. A chunk
 * may carry no locator at all; when it carries one, the kind must be the one
 * its format produces. Anything else is a corrupt row or a foreign preparation,
 * never a hit to publish.
 */
export function locatorMatchesFormat(
  locator: SourceLocator | null,
  format: SourceFormat,
): boolean {
  if (locator === null) return true;
  switch (locator.kind) {
    case "base_view": return format === "base";
    case "pdf_page": return format === "pdf";
    case "excel_cell": return format === "excel";
  }
}

export function isSourceFormatCounts(value: unknown): value is SourceFormatCounts {
  if (!isRecord(value) || !hasExactKeys(value, SOURCE_FORMATS)) return false;
  return SOURCE_FORMATS.every((format) => {
    const counts = value[format];
    return isRecord(counts)
      && hasExactKeys(counts, EXTRACTION_COVERAGES)
      && EXTRACTION_COVERAGES.every((coverage) => isNonNegativeSafeInteger(counts[coverage]));
  });
}

function indexedSourceCount(counts: SourceFormatCounts): number {
  return SOURCE_FORMATS.reduce((total, format) => total
    + counts[format]["indexed-complete"]
    + counts[format]["indexed-partial"], 0);
}

export function isWorkerError(value: unknown): value is WorkerError {
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
    "explicit_query_unsupported",
    "invalid_query",
    "invalid_query_plan",
    "query_execution_failed",
    "index_building",
    "index_limit_exceeded",
    "integrity_failed",
    "cache_identity_mismatch",
    "cache_version_mismatch",
    "cache_digest_mismatch",
    "cache_image_invalid",
    "cache_blob_too_large",
    "checkpoint_kind_mismatch",
    "checkpoint_identity_mismatch",
    "checkpoint_version_mismatch",
    "checkpoint_digest_mismatch",
    "checkpoint_image_invalid",
    "checkpoint_blob_too_large",
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
    || value === "export_initial_build_checkpoint"
    || value === "restore_initial_build_checkpoint"
    || value === "plan_initial_build_checkpoint_reconciliation"
    || value === "search"
    || value === "status"
    || value === "dispose";
}

export function isSha256Hex(value: unknown): value is string {
  return typeof value === "string" && SHA256_HEX_PATTERN.test(value);
}

export function isSourcePreparationDefectField(
  value: unknown,
): value is SourcePreparationDefectField {
  return typeof value === "string"
    && (SOURCE_PREPARATION_DEFECT_FIELDS as readonly string[]).includes(value);
}

export function isRequestId(value: unknown): value is number {
  return typeof value === "number" && Number.isSafeInteger(value) && value >= 1;
}

function isNonNegativeSafeInteger(value: unknown): value is number {
  return typeof value === "number" && Number.isSafeInteger(value) && value >= 0;
}

function isPositiveSafeInteger(value: unknown): value is number {
  return typeof value === "number" && Number.isSafeInteger(value) && value > 0;
}

export function isBoundedString(value: unknown, maximum: number, allowEmpty = false): value is string {
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

export function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

export function hasExactKeys(value: Record<string, unknown>, keys: readonly string[]): boolean {
  const actual = Object.keys(value);
  return actual.length === keys.length && actual.every((key) => keys.includes(key));
}
