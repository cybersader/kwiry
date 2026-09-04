// SPDX-FileCopyrightText: 2026 cybersader
// SPDX-License-Identifier: GPL-3.0-only
//
// Diagnostics are intentionally independent of Obsidian so the privacy boundary
// and bounded-storage behavior can be exercised without a plugin runtime.

export const DEFAULT_DIAGNOSTIC_CAPACITY = 512;

const MAX_DIAGNOSTIC_CAPACITY = 10_000;
const MAX_DETAIL_FIELDS = 48;
// Mirrors the fixed source-discovery response window grouping uses
// (`GROUPED_SEARCH_HIT_LIMIT` in grouped-search.ts). Diagnostics stays
// independent of that module, so the bound is restated here rather than
// imported.
const SOURCE_DISCOVERY_WINDOW_LIMIT = 100;

export type DiagnosticLevel = "debug" | "info" | "warn" | "error";

export type DiagnosticEventCode =
  | "plugin.load"
  | "plugin.unload"
  | "startup.lifecycle"
  | "backend.activate"
  | "backend.dispose"
  | "worker.lifecycle"
  | "cache.lifecycle"
  | "index.lifecycle"
  | "vault.event"
  | "search.lifecycle"
  | "failure.caught"
  | "promise.rejected";

export type DiagnosticTextValue =
  | "sqlite"
  | "plan_rejected"
  | "bounds_exceeded"
  | "internal"
  | "daemon"
  | "in_plugin"
  | "connecting"
  | "starting"
  | "building"
  | "ready"
  | "degraded"
  | "unavailable"
  | "disposed"
  | "unknown"
  | "alive"
  | "unreachable"
  | "terminated"
  | "lexical"
  | "semantic"
  | "hybrid"
  | "configuration"
  | "transport"
  | "protocol"
  | "index"
  | "query"
  | "lifecycle"
  | "inventory"
  | "read"
  | "prepare"
  | "apply"
  | "source_read_timeout"
  | "source_read_capacity"
  | "source_inspect_failed"
  | "source_read_rejected"
  | "source_snapshot_unstable"
  | "worker_timeout"
  | "snapshot"
  | "replay"
  | "rebuild"
  | "create"
  | "modify"
  | "delete"
  | "rename"
  | "activate"
  | "dispose"
  | "initialize"
  | "status"
  | "search"
  | "open"
  | "load"
  | "restore"
  | "export"
  | "discard"
  | "build"
  | "reconcile"
  | "update"
  | "poll"
  | "save"
  | "copy"
  | "clear"
  | "requested"
  | "started"
  | "succeeded"
  | "failed"
  | "cancelled"
  | "superseded"
  | "hit"
  | "miss"
  | "scheduled"
  | "already_building"
  | "skipped"
  | "aborted"
  | "restored"
  | "discarded"
  | "exported"
  | "plugin"
  | "backend_manager"
  | "daemon_backend"
  | "in_plugin_backend"
  | "index_controller"
  | "cache_store"
  | "vault_source"
  | "search_session"
  | "worker"
  | "rpc"
  | "vfs"
  | "settings"
  | "ui"
  | "vault_read_failed"
  | "index_build_failed"
  | "index_update_failed"
  | "index_limit_exceeded"
  | "index_reconciling"
  | "cache_partially_reused"
  | "index_building"
  | "worker_recovering"
  | "worker_failed"
  | "cache_absent"
  | "cache_unavailable"
  | "cache_corrupt"
  | "cache_incompatible"
  | "cache_restore_unavailable"
  | "cache_discard_failed"
  | "cache_save_failed"
  | "absent"
  | "pointer_unreadable"
  | "pointer_corrupt"
  | "identity_mismatch"
  | "image_absent"
  | "image_unreadable"
  | "image_length_mismatch"
  | "unsupported_platform"
  | "no_machine_local_root"
  | "root_not_absolute"
  | "root_not_machine_local"
  | "root_inside_vault"
  | "root_not_a_directory"
  | "root_not_writable"
  | "root_probe_failed"
  | "vault_location_unavailable"
  | "invalid_generation_id"
  | "invalid_identity"
  | "invalid_blob"
  | "write_failed"
  | "discard_failed"
  | "unsafe_path"
  | "locked"
  | "daemon_unreachable"
  | "daemon_upgrade_required"
  | "mode_unavailable"
  | "invalid_field_control"
  | "diagnostic_annotation_rejected"
  | "internal_error"
  | "BlockVfsUnavailableError"
  | "IndexCapacityError"
  | "IndexIntegrityError"
  | "CacheImageInvalidError"
  | "CacheVersionMismatchError"
  | "VaultSourceReadError"
  | "WorkerRpcError"
  | "RustAdapterError"
  | "TypeError"
  | "RangeError"
  | "ReferenceError"
  | "SyntaxError"
  | "Error"
  | "other"
  | "rust"
  | "sqlite"
  | "artifact"
  | "fts5_unavailable"
  | "rust_init_failed"
  | "sqlite_init_failed"
  | "artifact_mismatch"
  | "protocol_mismatch"
  | "invalid_request"
  | "invalid_state"
  | "source_rejected"
  | "explicit_query_unsupported"
  | "invalid_query"
  | "invalid_query_plan"
  | "query_execution_failed"
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
  | "fully_current"
  | "sources_omitted"
  | "vault_unavailable"
  | "index_capacity"
  | "backend_unavailable"
  | "plugin_load_failed"
  | "activation_failed"
  | "plugin_unloaded";

declare const diagnosticHashBrand: unique symbol;
declare const diagnosticGenerationBrand: unique symbol;

/** A hash produced before data crosses into the diagnostics recorder. */
export type DiagnosticHash = string & { readonly [diagnosticHashBrand]: true };

/** A generation identifier in one of Kwiry's machine-generated formats. */
export type DiagnosticGenerationId = string & { readonly [diagnosticGenerationBrand]: true };

export type DiagnosticSourceFormat =
  | "markdown"
  | "text"
  | "base"
  | "canvas"
  | "docx"
  | "pdf"
  | "excalidraw"
  | "excel"
  | "html";

export type DiagnosticSourceFormatPolicy = "enabled" | "disabled" | "unknown";

export interface DiagnosticSourceFormatSnapshot {
  policy: DiagnosticSourceFormatPolicy;
  indexedComplete: number;
  indexedPartial: number;
  skippedNoExtractableText: number;
  unreadable: number;
  quarantined: number;
}

export interface DiagnosticAvailableSourceGeneration {
  schemaVersion: 1;
  availability: "available";
  documents: number;
  chunks: number;
  zeroChunkSources: number;
  formats: Record<DiagnosticSourceFormat, DiagnosticSourceFormatSnapshot>;
}

export type DiagnosticSourceGeneration =
  | DiagnosticAvailableSourceGeneration
  | { schemaVersion: 1; availability: "unavailable" | "not_applicable" };

export type DiagnosticLexicalLaneKind =
  | "lexical_explicit_v3"
  | "lexical_exact_metadata_v3"
  | "lexical_exact_phrase_v3"
  | "lexical_all_terms_v3"
  | "lexical_partial_coverage_v3"
  | "lexical_prefix_metadata_v3"
  | "lexical_prefix_v3";

export type DiagnosticQueryPublicField =
  | "name"
  | "filename"
  | "title"
  | "alias"
  | "heading"
  | "tag"
  | "body";

export type DiagnosticLexicalProofField =
  | "filename"
  | "title"
  | "alias"
  | "heading"
  | "tag"
  | "body"
  | "cross_field";

export interface DiagnosticLexicalLaneAggregate {
  kind: DiagnosticLexicalLaneKind;
  plannedLaneCount: number;
  executedLaneCount: number;
  zeroObservationLaneCount: number;
  saturatedLaneCount: number;
  observationCount: number;
  addedUniqueCount: number;
}

export interface DiagnosticLexicalFieldAggregate {
  field: DiagnosticLexicalProofField;
  plannedLaneCount: number;
  executedLaneCount: number;
  zeroObservationLaneCount: number;
  saturatedLaneCount: number;
  observationCount: number;
  addedUniqueCount: number;
}

export interface DiagnosticAvailableLexicalExecution {
  schemaVersion: 1;
  availability: "available";
  disposition: "ready" | "empty_no_evidence" | "explicit_bypass";
  evidenceProbeCount: number;
  matchedEvidenceProbeCount: number;
  prefixProbeCount: number;
  prefixExpansionCount: number;
  plannedLaneCount: number;
  executedLaneCount: number;
  zeroObservationLaneCount: number;
  saturatedLaneCount: number;
  observationCount: number;
  uniqueCandidateCount: number;
  duplicateObservationCount: number;
  collectionCapDiscardedObservationCount: number;
  returnedCount: number;
  resultLimit: number;
  retainedCandidateTruncationCount: number;
  candidateLimit: number;
  candidateLimitReached: boolean;
  lanes: readonly DiagnosticLexicalLaneAggregate[];
  fields: readonly DiagnosticLexicalFieldAggregate[];
}

export type DiagnosticLexicalExecution =
  | DiagnosticAvailableLexicalExecution
  | { schemaVersion: 1; availability: "unavailable" | "not_applicable" };

export interface DiagnosticDetails {
  profile?: "daemon" | "in_plugin";
  phase?: DiagnosticTextValue;
  stage?: DiagnosticTextValue;
  activity?: "inventory" | "read" | "prepare" | "apply";
  stallCategory?: "source_read_timeout" | "source_read_capacity" | "worker_timeout";
  liveness?: "unknown" | "alive" | "unreachable" | "terminated";
  mode?: "lexical" | "semantic" | "hybrid";
  requestedMode?: "lexical" | "semantic" | "hybrid";
  effectiveMode?: "lexical" | "semantic" | "hybrid";
  lexicalProfile?: "none" | "lexical-v1" | "lexical-v2" | "unknown";
  lexicalMatchQuality?:
    | "standard_only"
    | "mixed"
    | "partial_only"
    | "none"
    | "unavailable"
    | "not_applicable";
  fieldScope?: DiagnosticQueryPublicField;
  fieldEmphasis?: DiagnosticQueryPublicField;
  candidateWindowState?: "exhausted" | "more_available" | "candidate_limit_reached" | "unknown";
  outcome?: DiagnosticTextValue;
  code?: DiagnosticTextValue;
  reason?: DiagnosticTextValue;
  /// A JavaScript error class name, the one field admitting a value not on the
  /// fixed vocabulary. Constrained to a bare identifier at runtime: an error
  /// class name is always one (`DataCloneError`), and that shape cannot carry a
  /// path, a query, or a sentence. Repeated field reports proved a closed list
  /// could not keep up with names thrown by the platform, and "other" cost a
  /// release each time it appeared.
  errorName?: string;
  operation?: DiagnosticTextValue;
  subsystem?: DiagnosticTextValue;
  generationId?: DiagnosticGenerationId;
  pathHash?: DiagnosticHash;
  pluginEpoch?: number;
  activationEpoch?: number;
  mutationEpoch?: number;
  count?: number;
  limit?: number;
  documents?: number;
  chunks?: number;
  completed?: number;
  total?: number | null;
  inFlight?: number;
  warningCount?: number;
  pending?: number;
  sourcesEnumerated?: number;
  sourcesRead?: number;
  sourcesSkipped?: number;
  sourcesOversized?: number;
  sourcesFailed?: number;
  bytesRead?: number;
  batchCount?: number;
  upserts?: number;
  removals?: number;
  renames?: number;
  rescans?: number;
  /// Paths created again within the resurrection window of their own deletion.
  /// A non-zero value means something is restoring files behind the user.
  resurrected?: number;
  resultCount?: number;
  candidateCount?: number;
  candidateLimit?: number;
  returnedSectionCount?: number;
  displayedSourceCount?: number;
  omittedObservedSourceCount?: number;
  /// Closed fact: the fixed 100-section source-discovery window came back
  /// full for this search. Independent of `omittedObservedSourceCount` (an
  /// exact, locally-known truncation by the source-row limit) and of
  /// `candidateWindowState` (the backend's own candidate-scan completeness).
  /// A saturated window means sources ranked below it were never observed,
  /// not that none exist.
  sourceWindowSaturated?: boolean;
  annotationRejectedCount?: number;
  sourceGeneration?: DiagnosticSourceGeneration;
  lexicalExecution?: DiagnosticLexicalExecution;
  cacheBytes?: number;
  /// Fixed classification of a failure, drawn from the closed text vocabulary.
  /// `query_execution_failed` is a catch-all that discards the thrown value on
  /// purpose; without this a report cannot name its own cause.
  failureCause?: DiagnosticTextValue;
  pluginLoadCompleteMs?: number | null;
  layoutReadyMs?: number | null;
  firstProgressMs?: number | null;
  firstCacheSearchableMs?: number | null;
  fullyCurrentMs?: number | null;
  retryable?: boolean;
  recoverable?: boolean;
  searchable?: boolean;
  dirty?: boolean;
  rebuilding?: boolean;
  cacheHit?: boolean;
  recovery?: boolean;
}

export type DiagnosticCounter =
  | "count"
  | "documents"
  | "chunks"
  | "completed"
  | "warningCount"
  | "pending"
  | "sourcesEnumerated"
  | "sourcesRead"
  | "sourcesSkipped"
  | "sourcesOversized"
  | "sourcesFailed"
  | "bytesRead"
  | "batchCount"
  | "upserts"
  | "removals"
  | "renames"
  | "rescans"
  | "resurrected"
  | "resultCount"
  | "cacheBytes";

export interface DiagnosticEventBuilder {
  set(details: Readonly<DiagnosticDetails>): void;
  increment(counter: DiagnosticCounter, amount?: number): void;
  complete(level: DiagnosticLevel, details: Readonly<DiagnosticDetails>): void;
  rejectAnnotation(): void;
}

export interface DiagnosticEntry {
  readonly sequence: number;
  readonly startedAtMs: number;
  readonly durationMs: number;
  readonly level: DiagnosticLevel;
  readonly code: DiagnosticEventCode;
  readonly details: Readonly<DiagnosticDetails>;
}

export interface DiagnosticSnapshot {
  readonly capacity: number;
  readonly dropped: number;
  readonly entries: readonly DiagnosticEntry[];
}

export type DiagnosticPlatform = "android" | "ios" | "linux" | "macos" | "windows" | "unknown";

export interface DiagnosticExportContext {
  readonly pluginVersion: string;
  readonly obsidianVersion: string;
  readonly platform: DiagnosticPlatform;
  readonly backendProfile: "daemon" | "in_plugin";
}

const LEVELS: readonly DiagnosticLevel[] = ["debug", "info", "warn", "error"];
const EVENT_CODES: readonly DiagnosticEventCode[] = [
  "plugin.load",
  "plugin.unload",
  "startup.lifecycle",
  "backend.activate",
  "backend.dispose",
  "worker.lifecycle",
  "cache.lifecycle",
  "index.lifecycle",
  "vault.event",
  "search.lifecycle",
  "failure.caught",
  "promise.rejected",
];
const TEXT_VALUES: readonly DiagnosticTextValue[] = [
  "daemon", "in_plugin", "connecting", "starting", "building", "ready", "degraded",
  "unavailable", "disposed", "unknown", "alive", "unreachable", "terminated", "lexical",
  "semantic", "hybrid", "configuration", "transport", "protocol", "index", "query",
  "lifecycle", "inventory", "read", "prepare", "apply", "source_read_timeout",
  "source_read_capacity", "source_inspect_failed", "source_read_rejected",
  "source_snapshot_unstable", "worker_timeout", "snapshot", "replay", "rebuild", "create",
  "modify", "delete", "rename",
  "activate", "dispose", "initialize", "status", "search", "open", "load", "restore",
  "export", "discard", "build", "reconcile", "update", "poll", "save", "copy", "clear",
  "requested", "started", "succeeded", "failed", "cancelled", "superseded", "hit", "miss",
  "scheduled", "already_building", "skipped", "aborted", "restored", "discarded", "exported",
  "plugin",
  "backend_manager", "daemon_backend", "in_plugin_backend", "index_controller", "cache_store",
  "vault_source", "search_session", "worker", "rpc", "vfs", "settings", "ui",
  "vault_read_failed", "index_build_failed", "index_update_failed", "index_limit_exceeded",
  "index_reconciling", "cache_partially_reused", "index_building", "worker_recovering",
  "worker_failed", "cache_absent",
  "cache_unavailable", "cache_corrupt", "cache_incompatible", "cache_restore_unavailable",
  "cache_discard_failed", "cache_save_failed", "absent", "pointer_unreadable", "pointer_corrupt",
  "identity_mismatch", "image_absent", "image_unreadable", "image_length_mismatch",
  "unsupported_platform", "no_machine_local_root", "root_not_absolute", "root_not_machine_local",
  "root_inside_vault", "root_not_a_directory", "root_not_writable", "root_probe_failed",
  "vault_location_unavailable", "invalid_generation_id", "invalid_identity", "invalid_blob",
  "write_failed", "discard_failed", "unsafe_path", "locked", "daemon_unreachable",
  "daemon_upgrade_required", "mode_unavailable", "invalid_field_control",
  "diagnostic_annotation_rejected", "internal_error",
  // Constructor names of errors this codebase and the JS runtime define.
  // These are fixed identifiers chosen here, not caller-supplied text, so
  // recording one leaks nothing while turning an "unknown" report into a
  // named fault.
  "BlockVfsUnavailableError", "IndexCapacityError", "IndexIntegrityError",
  "CacheImageInvalidError", "CacheVersionMismatchError", "VaultSourceReadError",
  "WorkerRpcError", "RustAdapterError", "TypeError", "RangeError",
  "ReferenceError", "SyntaxError", "Error", "other", "rust", "sqlite", "plan_rejected",
  "bounds_exceeded", "internal", "artifact",
  "fts5_unavailable", "rust_init_failed", "sqlite_init_failed", "artifact_mismatch", "protocol_mismatch", "invalid_request", "invalid_state", "source_rejected", "explicit_query_unsupported", "invalid_query", "invalid_query_plan", "query_execution_failed", "integrity_failed", "cache_identity_mismatch", "cache_version_mismatch", "cache_digest_mismatch", "cache_image_invalid", "cache_blob_too_large", "checkpoint_kind_mismatch", "checkpoint_identity_mismatch", "checkpoint_version_mismatch", "checkpoint_digest_mismatch", "checkpoint_image_invalid", "checkpoint_blob_too_large", "worker_crashed", "timeout",
  "fully_current", "sources_omitted", "vault_unavailable", "index_capacity", "backend_unavailable",
  "plugin_load_failed", "activation_failed", "plugin_unloaded",
];
const DETAIL_KEYS: readonly (keyof DiagnosticDetails)[] = [
  "profile", "phase", "stage", "activity", "stallCategory", "liveness", "mode",
  "requestedMode", "effectiveMode", "lexicalProfile", "lexicalMatchQuality", "fieldScope",
  "fieldEmphasis", "candidateWindowState", "outcome", "code", "reason", "errorName", "operation",
  "subsystem", "generationId", "pathHash", "pluginEpoch", "activationEpoch", "mutationEpoch",
  "count", "limit", "documents", "chunks", "completed", "total", "inFlight", "warningCount",
  "pending",
  "sourcesEnumerated", "sourcesRead", "sourcesSkipped", "sourcesOversized", "sourcesFailed",
  "bytesRead", "batchCount", "upserts", "removals", "renames", "rescans", "resurrected",
  "resultCount", "candidateCount", "candidateLimit", "returnedSectionCount",
  "displayedSourceCount", "omittedObservedSourceCount", "sourceWindowSaturated",
  "annotationRejectedCount",
  "sourceGeneration", "lexicalExecution", "cacheBytes", "failureCause",
  "pluginLoadCompleteMs", "layoutReadyMs", "firstProgressMs", "firstCacheSearchableMs",
  "fullyCurrentMs", "retryable",
  "recoverable", "searchable", "dirty", "rebuilding", "cacheHit", "recovery",
];
const NUMERIC_DETAIL_KEYS = new Set<keyof DiagnosticDetails>([
  "pluginEpoch", "activationEpoch", "mutationEpoch", "count", "limit", "documents", "chunks",
  "completed", "inFlight", "warningCount", "pending", "sourcesEnumerated", "sourcesRead",
  "sourcesSkipped",
  "sourcesOversized", "sourcesFailed", "bytesRead", "batchCount", "upserts", "removals",
  "renames", "rescans", "resurrected",
  "resultCount", "candidateCount", "candidateLimit", "returnedSectionCount",
  "displayedSourceCount", "omittedObservedSourceCount", "annotationRejectedCount", "cacheBytes",
]);
const NULLABLE_NUMERIC_DETAIL_KEYS = new Set<keyof DiagnosticDetails>([
  "pluginLoadCompleteMs", "layoutReadyMs", "firstProgressMs", "firstCacheSearchableMs",
  "fullyCurrentMs",
]);
const BOOLEAN_DETAIL_KEYS = new Set<keyof DiagnosticDetails>([
  "retryable", "recoverable", "searchable", "dirty", "rebuilding", "cacheHit", "recovery",
  "sourceWindowSaturated",
]);
const STARTUP_DETAIL_KEYS = new Set<keyof DiagnosticDetails>([
  "profile", "outcome", "reason", "pluginEpoch", "activationEpoch", "pluginLoadCompleteMs",
  "layoutReadyMs", "firstProgressMs", "firstCacheSearchableMs", "fullyCurrentMs", "cacheHit",
  "cacheBytes",
]);
const REQUIRED_STARTUP_DETAIL_KEYS: readonly (keyof DiagnosticDetails)[] = [
  "profile", "outcome", "reason", "pluginEpoch", "activationEpoch", "pluginLoadCompleteMs",
  "layoutReadyMs", "firstProgressMs", "firstCacheSearchableMs", "fullyCurrentMs", "cacheHit",
];
const STARTUP_OUTCOMES = new Set<DiagnosticTextValue>([
  "succeeded", "degraded", "failed", "cancelled",
]);
const STARTUP_REASONS = new Set<DiagnosticTextValue>([
  "fully_current", "sources_omitted", "vault_unavailable", "index_capacity",
  "backend_unavailable", "plugin_load_failed", "activation_failed", "plugin_unloaded",
]);
const COUNTERS = new Set<DiagnosticCounter>([
  "count", "documents", "chunks", "completed", "warningCount", "pending", "sourcesEnumerated",
  "sourcesRead", "sourcesSkipped", "sourcesOversized", "sourcesFailed", "bytesRead", "batchCount",
  "upserts", "removals", "renames", "rescans", "resurrected", "resultCount", "cacheBytes",
]);
const TEXT_VALUE_SET = new Set<DiagnosticTextValue>(TEXT_VALUES);
const INDEX_ACTIVITY_SET = new Set<NonNullable<DiagnosticDetails["activity"]>>([
  "inventory", "read", "prepare", "apply",
]);
const INDEX_STALL_CATEGORY_SET = new Set<NonNullable<DiagnosticDetails["stallCategory"]>>([
  "source_read_timeout", "source_read_capacity", "worker_timeout",
]);
const SEARCH_MODE_SET = new Set<"lexical" | "semantic" | "hybrid">([
  "lexical", "semantic", "hybrid",
]);
const LEXICAL_PROFILE_SET = new Set<NonNullable<DiagnosticDetails["lexicalProfile"]>>([
  "none", "lexical-v1", "lexical-v2", "unknown",
]);
const LEXICAL_MATCH_QUALITY_SET = new Set<
  NonNullable<DiagnosticDetails["lexicalMatchQuality"]>
>([
  "standard_only", "mixed", "partial_only", "none", "unavailable", "not_applicable",
]);
const QUERY_FIELD_SET = new Set<DiagnosticQueryPublicField>([
  "name", "filename", "title", "alias", "heading", "tag", "body",
]);
const CANDIDATE_WINDOW_STATE_SET = new Set<NonNullable<DiagnosticDetails["candidateWindowState"]>>([
  "exhausted", "more_available", "candidate_limit_reached", "unknown",
]);
const EVENT_CODE_SET = new Set<DiagnosticEventCode>(EVENT_CODES);
const LEVEL_SET = new Set<DiagnosticLevel>(LEVELS);
const PLATFORM_SET = new Set<DiagnosticPlatform>([
  "android", "ios", "linux", "macos", "windows", "unknown",
]);
const PROFILE_SET = new Set<DiagnosticExportContext["backendProfile"]>(["daemon", "in_plugin"]);
const HASH_PATTERN = /^sha256:[0-9a-f]{64}$/u;
const IN_PLUGIN_GENERATION_PATTERN = /^(?:daemon|in_plugin)-[1-9][0-9]*-generation-[1-9][0-9]*$/u;
const DAEMON_GENERATION_PATTERN = /^g-[0-9]{39}-[0-9]{10}-[0-9]{20}-[0-9]{20}$/u;

// Wide events deliberately carry many safe, high-cardinality dimensions because
// one completed operation is more useful than a diary of disconnected steps.
// The schema still excludes arbitrary strings: query text, note content,
// excerpts, tokens, and literal paths cannot be added in the name of richer
// context. Runtime validation repeats that type boundary for JavaScript and
// `any`, and unknown keys cannot become a side channel.
export function diagnosticHash(value: string): DiagnosticHash {
  if (!HASH_PATTERN.test(value)) throw new TypeError("Invalid diagnostic hash");
  return value as DiagnosticHash;
}

export function diagnosticGenerationId(value: string): DiagnosticGenerationId {
  if (!IN_PLUGIN_GENERATION_PATTERN.test(value) && !DAEMON_GENERATION_PATTERN.test(value)) {
    throw new TypeError("Invalid diagnostic generation ID");
  }
  return value as DiagnosticGenerationId;
}

export class DiagnosticLog {
  private readonly slots: Array<DiagnosticEntry | undefined>;
  private start = 0;
  private size = 0;
  private nextSequence = 1;
  private dropped = 0;

  // 512 wide events retain multiple indexing/recovery cycles while keeping the
  // worst case bounded. The power-of-two default stays near the requested 500
  // entries without pretending an evicted prefix is still present.
  constructor(
    public readonly capacity = DEFAULT_DIAGNOSTIC_CAPACITY,
    private readonly wallNow: () => number = Date.now,
    private readonly monotonicNow: () => number = defaultMonotonicNow,
  ) {
    if (!Number.isSafeInteger(capacity) || capacity <= 0 || capacity > MAX_DIAGNOSTIC_CAPACITY) {
      throw new RangeError("Diagnostic capacity is out of bounds");
    }
    this.slots = new Array<DiagnosticEntry | undefined>(capacity);
  }

  async capture<T>(
    level: DiagnosticLevel,
    code: DiagnosticEventCode,
    initialDetails: Readonly<DiagnosticDetails>,
    operation: (event: DiagnosticEventBuilder) => T | Promise<T>,
  ): Promise<T> {
    if (!LEVEL_SET.has(level) || !EVENT_CODE_SET.has(code)) {
      throw new TypeError("Invalid diagnostic event");
    }
    const startedAtMs = this.readWallClock();
    const monotonicStartedAtMs = this.readMonotonicClock();
    const event = new MutableDiagnosticEvent(level, code, initialDetails);
    try {
      const result = await operation(event);
      event.completeDefault();
      return result;
    } catch (error) {
      event.fail();
      throw error;
    } finally {
      // A broken monotonic clock must not erase the operation record that the
      // wrapper exists to guarantee; zero duration is safer than wall-clock skew.
      const endedAtMs = this.readMonotonicClockOr(monotonicStartedAtMs);
      const durationMs = Math.max(0, Math.round(endedAtMs - monotonicStartedAtMs));
      this.append(event.finish(startedAtMs, durationMs));
    }
  }

  record(
    level: DiagnosticLevel,
    code: DiagnosticEventCode,
    startedAtMs: number,
    durationMs: number,
    details: Readonly<DiagnosticDetails>,
  ): void {
    if (!LEVEL_SET.has(level) || !EVENT_CODE_SET.has(code)) {
      throw new TypeError("Invalid diagnostic event");
    }
    const event = new MutableDiagnosticEvent(level, code, details);
    event.completeDefault();
    this.append(event.finish(
      validDiagnosticTimestamp(startedAtMs),
      nonNegativeSafeInteger(durationMs, "Invalid diagnostic duration"),
    ));
  }

  snapshot(minimumLevel: DiagnosticLevel = "debug"): DiagnosticSnapshot {
    if (!LEVEL_SET.has(minimumLevel)) throw new TypeError("Invalid diagnostic level");
    const minimum = LEVELS.indexOf(minimumLevel);
    const entries: DiagnosticEntry[] = [];
    for (let offset = 0; offset < this.size; offset += 1) {
      const entry = this.slots[(this.start + offset) % this.capacity];
      if (entry && LEVELS.indexOf(entry.level) >= minimum) entries.push(entry);
    }
    return Object.freeze({
      capacity: this.capacity,
      dropped: this.dropped,
      entries: Object.freeze(entries),
    });
  }

  clear(): void {
    this.slots.fill(undefined);
    this.start = 0;
    this.size = 0;
    this.dropped = 0;
  }

  private append(event: Omit<DiagnosticEntry, "sequence">): void {
    const entry = Object.freeze({ sequence: this.nextSequence, ...event });
    this.nextSequence += 1;
    if (this.size < this.capacity) {
      this.slots[(this.start + this.size) % this.capacity] = entry;
      this.size += 1;
    } else {
      this.slots[this.start] = entry;
      this.start = (this.start + 1) % this.capacity;
      this.dropped += 1;
    }
  }

  private readWallClock(): number {
    return validDiagnosticTimestamp(this.wallNow());
  }

  private readMonotonicClock(): number {
    const timestamp = this.monotonicNow();
    if (!Number.isFinite(timestamp) || timestamp < 0) {
      throw new TypeError("Invalid diagnostic monotonic timestamp");
    }
    return timestamp;
  }

  private readMonotonicClockOr(fallback: number): number {
    try {
      return this.readMonotonicClock();
    } catch {
      return fallback;
    }
  }
}

/// Export shaping. Filtering never changes what was captured, only which
/// frozen retained entries are selected for this point-in-time report.
export interface DiagnosticReportOptions {
  readonly minimumLevel?: DiagnosticLevel;
  readonly categories?: readonly DiagnosticEventCode[];
  readonly includeStructuredRecords?: boolean;
}

export interface DiagnosticExportPlan {
  readonly schemaVersion: 2;
  readonly reportUnavailable: boolean;
  readonly context: Readonly<DiagnosticExportContext>;
  readonly capacity: number;
  readonly retainedEntries: number;
  readonly selectedEntries: number;
  readonly droppedEntries: number;
  readonly filteredOutEntries: number;
  readonly minimumLevel: DiagnosticLevel;
  readonly categories: readonly DiagnosticEventCode[] | null;
  readonly entries: readonly DiagnosticEntry[];
}

export interface DiagnosticExportSerializationOptions {
  readonly includeStructuredRecords?: boolean;
  readonly maxChunkBytes?: number;
}

export interface BoundedDiagnosticSummary {
  readonly text: string;
  readonly byteLength: number;
  readonly retainedEntries: number;
  readonly selectedEntries: number;
  readonly droppedEntries: number;
  readonly filteredOutEntries: number;
  readonly emittedEntries: number;
  readonly omittedEntries: number;
}

export const DIAGNOSTIC_EXPORT_CHUNK_BYTES = 16 * 1_024;
export const DIAGNOSTIC_CLIPBOARD_MAX_BYTES = 64 * 1_024;

const MIN_DIAGNOSTIC_SUMMARY_BYTES = 1_024;

export const DIAGNOSTIC_CATEGORY_GROUPS = {
  all: null,
  indexing: ["index.lifecycle", "cache.lifecycle", "worker.lifecycle", "vault.event"],
  search: ["search.lifecycle"],
  startup: [
    "plugin.load",
    "plugin.unload",
    "startup.lifecycle",
    "backend.activate",
    "backend.dispose",
  ],
  failures: ["failure.caught", "promise.rejected"],
} as const satisfies Record<string, readonly DiagnosticEventCode[] | null>;

export type DiagnosticCategoryGroup = keyof typeof DIAGNOSTIC_CATEGORY_GROUPS;

export function diagnosticCategoryGroup(
  group: DiagnosticCategoryGroup,
): readonly DiagnosticEventCode[] | undefined {
  return DIAGNOSTIC_CATEGORY_GROUPS[group] ?? undefined;
}

export function createDiagnosticExportPlan(
  log: DiagnosticLog,
  context: DiagnosticExportContext,
  options: DiagnosticReportOptions = {},
): DiagnosticExportPlan {
  const minimumLevel = options.minimumLevel ?? "debug";
  if (!LEVEL_SET.has(minimumLevel)) throw new TypeError("Invalid diagnostic level");
  const categories = validateCategoryFilter(options.categories);
  const safeContext = validateExportContext(context);

  // Always take one complete retained snapshot. Level and category filtering is
  // derived from this frozen view so counts and records cannot come from
  // different moments in a busy indexing session.
  const snapshot = log.snapshot("debug");
  const minimum = LEVELS.indexOf(minimumLevel);
  const selected = snapshot.entries.filter((entry) =>
    LEVELS.indexOf(entry.level) >= minimum
      && (categories === null || categories.includes(entry.code)));
  const entries = Object.freeze(selected);
  return Object.freeze({
    schemaVersion: 2,
    reportUnavailable: false,
    context: safeContext,
    capacity: snapshot.capacity,
    retainedEntries: snapshot.entries.length,
    selectedEntries: entries.length,
    droppedEntries: snapshot.dropped,
    filteredOutEntries: snapshot.entries.length - entries.length,
    minimumLevel,
    categories,
    entries,
  });
}

export function createUnavailableDiagnosticExportPlan(): DiagnosticExportPlan {
  return Object.freeze({
    schemaVersion: 2,
    reportUnavailable: true,
    context: Object.freeze({
      pluginVersion: "unknown",
      obsidianVersion: "unknown",
      platform: "unknown",
      backendProfile: "in_plugin",
    }),
    capacity: 0,
    retainedEntries: 0,
    selectedEntries: 0,
    droppedEntries: 0,
    filteredOutEntries: 0,
    minimumLevel: "error",
    categories: null,
    entries: Object.freeze([]),
  });
}

export function* serializeDiagnosticExport(
  plan: DiagnosticExportPlan,
  options: DiagnosticExportSerializationOptions = {},
): Iterable<Uint8Array> {
  const maxChunkBytes = options.maxChunkBytes ?? DIAGNOSTIC_EXPORT_CHUNK_BYTES;
  if (!Number.isSafeInteger(maxChunkBytes) || maxChunkBytes <= 0
    || maxChunkBytes > DIAGNOSTIC_EXPORT_CHUNK_BYTES) {
    throw new RangeError("Diagnostic export chunk size is out of bounds");
  }
  const encoder = new TextEncoder();
  for (const text of diagnosticTextSegments(
    plan,
    options.includeStructuredRecords ?? true,
  )) {
    const encoded = encoder.encode(text);
    for (let offset = 0; offset < encoded.byteLength; offset += maxChunkBytes) {
      yield encoded.subarray(offset, Math.min(encoded.byteLength, offset + maxChunkBytes));
    }
  }
}

export function collectBoundedDiagnosticSummary(
  plan: DiagnosticExportPlan,
  maxBytes = DIAGNOSTIC_CLIPBOARD_MAX_BYTES,
): BoundedDiagnosticSummary {
  if (!Number.isSafeInteger(maxBytes) || maxBytes < MIN_DIAGNOSTIC_SUMMARY_BYTES
    || maxBytes > DIAGNOSTIC_CLIPBOARD_MAX_BYTES) {
    throw new RangeError("Diagnostic summary byte limit is out of bounds");
  }
  const encoder = new TextEncoder();
  const header = `${diagnosticHeaderLines(plan).join("\n")}\n`;
  const eventLines: string[] = [];
  let emittedEntries = 0;
  let byteLengthWithoutFooter = encoder.encode(header).byteLength;

  for (const entry of plan.entries) {
    const candidate = `${formatDiagnosticEntry(entry)}\n`;
    const candidateBytes = encoder.encode(candidate).byteLength;
    const candidateFooterBytes = encoder.encode(clipboardFooter(
      emittedEntries + 1,
      plan.selectedEntries - emittedEntries - 1,
    )).byteLength;
    if (byteLengthWithoutFooter + candidateBytes + candidateFooterBytes > maxBytes) break;
    eventLines.push(candidate);
    emittedEntries += 1;
    byteLengthWithoutFooter += candidateBytes;
  }

  if (plan.selectedEntries === 0) {
    const emptyLine = "(no retained events at this level)\n";
    eventLines.push(emptyLine);
    byteLengthWithoutFooter += encoder.encode(emptyLine).byteLength;
  }
  const omittedEntries = plan.selectedEntries - emittedEntries;
  const text = `${header}${eventLines.join("")}${clipboardFooter(emittedEntries, omittedEntries)}`;
  const byteLength = encoder.encode(text).byteLength;
  if (byteLength > maxBytes) throw new RangeError("Diagnostic summary header exceeds byte limit");
  return Object.freeze({
    text,
    byteLength,
    retainedEntries: plan.retainedEntries,
    selectedEntries: plan.selectedEntries,
    droppedEntries: plan.droppedEntries,
    filteredOutEntries: plan.filteredOutEntries,
    emittedEntries,
    omittedEntries,
  });
}

export function formatDiagnosticLog(
  log: DiagnosticLog,
  context: DiagnosticExportContext,
  minimumLevelOrOptions: DiagnosticLevel | DiagnosticReportOptions = "debug",
): string {
  const options: DiagnosticReportOptions = typeof minimumLevelOrOptions === "string"
    ? { minimumLevel: minimumLevelOrOptions }
    : minimumLevelOrOptions;
  return formatDiagnosticExportPlan(createDiagnosticExportPlan(log, context, options), {
    includeStructuredRecords: options.includeStructuredRecords ?? true,
  });
}

export function formatDiagnosticExportPlan(
  plan: DiagnosticExportPlan,
  options: DiagnosticExportSerializationOptions = {},
): string {
  const decoder = new TextDecoder();
  const parts: string[] = [];
  for (const chunk of serializeDiagnosticExport(plan, options)) {
    parts.push(decoder.decode(chunk, { stream: true }));
  }
  parts.push(decoder.decode());
  return parts.join("");
}

function* diagnosticTextSegments(
  plan: DiagnosticExportPlan,
  includeStructuredRecords: boolean,
): Iterable<string> {
  yield `${diagnosticHeaderLines(plan).join("\n")}\n`;
  if (plan.entries.length === 0) {
    yield "(no retained events at this level)\n";
  } else {
    for (const entry of plan.entries) yield `${formatDiagnosticEntry(entry)}\n`;
  }
  if (!includeStructuredRecords) {
    yield "\nstructured_records: omitted\n";
    return;
  }

  yield "\nStructured records (JSON):\n";
  const prefix = JSON.stringify({
    schemaVersion: 2,
    context: plan.context,
    capacity: plan.capacity,
    storedEntries: plan.selectedEntries,
    droppedEntries: plan.droppedEntries,
    minimumLevel: plan.minimumLevel,
  }, null, 2);
  yield `${prefix.slice(0, -2)},\n  "records": [`;
  for (let index = 0; index < plan.entries.length; index += 1) {
    const record = JSON.stringify(plan.entries[index], null, 2)
      .split("\n")
      .map((line) => `    ${line}`)
      .join("\n");
    yield `${index === 0 ? "\n" : ",\n"}${record}`;
  }
  yield `${plan.entries.length === 0 ? "" : "\n"}  ]\n}\n`;
}

function diagnosticHeaderLines(plan: DiagnosticExportPlan): string[] {
  return [
    "Kwiry diagnostics log",
    ...(plan.reportUnavailable ? ["report_unavailable: true"] : []),
    `plugin_version: ${plan.context.pluginVersion}`,
    `obsidian_version: ${plan.context.obsidianVersion}`,
    `platform: ${plan.context.platform}`,
    `backend_profile: ${plan.context.backendProfile}`,
    `capacity: ${plan.capacity}`,
    `stored_entries: ${plan.selectedEntries}`,
    `dropped_entries: ${plan.droppedEntries}`,
    `minimum_level: ${plan.minimumLevel}`,
    `categories: ${plan.categories === null ? "all" : plan.categories.join(",")}`,
    `retained_entries: ${plan.retainedEntries}`,
    `filtered_out_entries: ${plan.filteredOutEntries}`,
    "",
    "Summary:",
  ];
}

function formatDiagnosticEntry(entry: DiagnosticEntry): string {
  const detail = DETAIL_KEYS.flatMap((key) => {
    const value = entry.details[key];
    return value === undefined ? [] : [`${key}=${formatDiagnosticDetail(key, value)}`];
  }).join(" ");
  const prefix = `${entry.sequence} ${new Date(entry.startedAtMs).toISOString()} +${entry.durationMs}ms ${entry.level.toUpperCase()} ${entry.code}`;
  return detail.length === 0 ? prefix : `${prefix} ${detail}`;
}

function formatDiagnosticDetail(key: keyof DiagnosticDetails, value: unknown): string {
  if (key === "sourceGeneration") {
    const generation = value as DiagnosticSourceGeneration;
    return generation.availability === "available"
      ? `available:${generation.documents}/${generation.chunks}/zero=${generation.zeroChunkSources}`
      : generation.availability;
  }
  if (key === "lexicalExecution") {
    const execution = value as DiagnosticLexicalExecution;
    return execution.availability === "available"
      ? `${execution.disposition}:lanes=${execution.executedLaneCount}/${execution.plannedLaneCount},candidates=${execution.uniqueCandidateCount},returned=${execution.returnedCount}`
      : execution.availability;
  }
  return String(value);
}

function clipboardFooter(emittedEntries: number, omittedEntries: number): string {
  return "\nstructured_records: omitted\n"
    + `clipboard_emitted_entries: ${emittedEntries}\n`
    + `clipboard_omitted_entries: ${omittedEntries}\n`;
}

function validateCategoryFilter(
  value: readonly DiagnosticEventCode[] | undefined,
): readonly DiagnosticEventCode[] | null {
  if (value === undefined) return null;
  if (value.length === 0) throw new TypeError("Invalid diagnostic category filter");
  for (const category of value) {
    if (!EVENT_CODE_SET.has(category)) throw new TypeError("Invalid diagnostic category filter");
  }
  return Object.freeze([...value]);
}

function validateExportContext(
  context: DiagnosticExportContext,
): Readonly<DiagnosticExportContext> {
  if (!PLATFORM_SET.has(context.platform) || !PROFILE_SET.has(context.backendProfile)) {
    throw new TypeError("Invalid diagnostic header");
  }
  return Object.freeze({
    pluginVersion: headerToken(context.pluginVersion),
    obsidianVersion: headerToken(context.obsidianVersion),
    platform: context.platform,
    backendProfile: context.backendProfile,
  });
}

class MutableDiagnosticEvent implements DiagnosticEventBuilder {
  private level: DiagnosticLevel;
  private details: Partial<DiagnosticDetails>;
  private terminal = false;
  private finished = false;

  constructor(
    level: DiagnosticLevel,
    private readonly code: DiagnosticEventCode,
    initialDetails: Readonly<DiagnosticDetails>,
  ) {
    this.level = level;
    this.details = { ...validateDetails(initialDetails) };
  }

  set(details: Readonly<DiagnosticDetails>): void {
    this.requireMutable();
    const validated = validateDetails(details);
    const merged = { ...this.details, ...validated };
    if (Object.keys(merged).length > MAX_DETAIL_FIELDS) {
      throw new TypeError("Invalid diagnostic details");
    }
    this.details = { ...merged };
  }

  increment(counter: DiagnosticCounter, amount = 1): void {
    this.requireMutable();
    if (!COUNTERS.has(counter) || !isNonNegativeInteger(amount)) {
      throw new TypeError("Invalid diagnostic counter");
    }
    const current = this.details[counter] ?? 0;
    const next = current + amount;
    if (!Number.isSafeInteger(next)) throw new TypeError("Invalid diagnostic counter");
    this.details[counter] = next;
  }

  complete(level: DiagnosticLevel, details: Readonly<DiagnosticDetails>): void {
    this.requireMutable();
    if (!LEVEL_SET.has(level)) throw new TypeError("Invalid diagnostic level");
    const merged = { ...this.details, ...validateDetails(details) };
    if (Object.keys(merged).length > MAX_DETAIL_FIELDS) {
      throw new TypeError("Invalid diagnostic details");
    }
    const validated = validateEventDetails(this.code, level, merged);
    this.level = level;
    this.details = { ...validated };
    this.terminal = true;
  }

  rejectAnnotation(): void {
    this.requireOpen();
    const previous = this.details.annotationRejectedCount ?? 0;
    this.level = "error";
    this.details = {
      outcome: "failed",
      code: "diagnostic_annotation_rejected",
      annotationRejectedCount: Math.min(previous + 1, Number.MAX_SAFE_INTEGER),
    };
    this.terminal = true;
  }

  completeDefault(): void {
    if (this.terminal) return;
    if (this.details.outcome !== undefined) {
      if (this.level === "error"
        && (this.details.outcome !== "failed"
          || (this.code !== "startup.lifecycle" && this.details.code === undefined))) {
        this.fail();
        return;
      }
      this.complete(this.level, {});
      return;
    }
    if (this.level === "error") {
      this.fail();
      return;
    }
    this.complete(this.level, { outcome: "succeeded" });
  }

  fail(): void {
    this.requireOpen();
    const merged: DiagnosticDetails = {
      ...this.details,
      outcome: "failed",
      code: this.details.code ?? "internal_error",
    };
    try {
      const validated = validateEventDetails(this.code, "error", merged);
      this.level = "error";
      this.details = { ...validated };
      this.terminal = true;
    } catch {
      this.rejectAnnotation();
    }
  }

  finish(
    startedAtMs: number,
    durationMs: number,
  ): Omit<DiagnosticEntry, "sequence"> {
    this.requireOpen();
    if (!this.terminal) this.completeDefault();
    this.finished = true;
    return Object.freeze({
      startedAtMs,
      durationMs,
      level: this.level,
      code: this.code,
      details: validateEventDetails(this.code, this.level, this.details),
    });
  }

  private requireMutable(): void {
    this.requireOpen();
    if (this.terminal) throw new Error("Diagnostic event is already completed");
  }

  private requireOpen(): void {
    if (this.finished) throw new Error("Diagnostic event is already committed");
  }
}

function validateEventDetails(
  code: DiagnosticEventCode,
  level: DiagnosticLevel,
  details: Readonly<DiagnosticDetails>,
): Readonly<DiagnosticDetails> {
  const validated = validateDetails(details);
  if (validated.outcome === undefined) throw new TypeError("Invalid diagnostic terminal state");
  const failed = validated.outcome === "failed";
  if (code === "startup.lifecycle") {
    if ((level === "error") !== failed) throw new TypeError("Invalid diagnostic terminal state");
    validateStartupDetails(validated);
    return validated;
  }
  if ((level === "error") !== failed || (failed && validated.code === undefined)) {
    throw new TypeError("Invalid diagnostic terminal state");
  }
  if (validated.outcome === "succeeded" && validated.failureCause !== undefined) {
    throw new TypeError("Invalid diagnostic terminal state");
  }
  if (code === "search.lifecycle" && validated.operation === "search") {
    validateSearchDetails(level, validated);
  }
  return validated;
}

function validateStartupDetails(validated: Readonly<DiagnosticDetails>): void {
  const keys = Object.keys(validated) as Array<keyof DiagnosticDetails>;
  if (keys.some((key) => !STARTUP_DETAIL_KEYS.has(key))
    || REQUIRED_STARTUP_DETAIL_KEYS.some((key) => validated[key] === undefined)) {
    throw new TypeError("Invalid startup diagnostic details");
  }
  if (!STARTUP_OUTCOMES.has(validated.outcome as DiagnosticTextValue)
    || !STARTUP_REASONS.has(validated.reason as DiagnosticTextValue)) {
    throw new TypeError("Invalid startup diagnostic details");
  }
  if (validated.cacheHit === true) {
    if (validated.firstCacheSearchableMs === null || validated.cacheBytes === undefined) {
      throw new TypeError("Invalid startup diagnostic details");
    }
  } else if (validated.firstCacheSearchableMs !== null || validated.cacheBytes !== undefined) {
    throw new TypeError("Invalid startup diagnostic details");
  }
  if (validated.outcome === "succeeded") {
    if (validated.reason !== "fully_current" || validated.fullyCurrentMs === null) {
      throw new TypeError("Invalid startup diagnostic details");
    }
  } else if (validated.reason === "fully_current" || validated.fullyCurrentMs !== null) {
    throw new TypeError("Invalid startup diagnostic details");
  }
}

function validateSearchDetails(
  level: DiagnosticLevel,
  validated: Readonly<DiagnosticDetails>,
): void {
  // `sourceWindowSaturated` is a closed fact derived from a single other
  // field (`returnedSectionCount` against the fixed 100-section discovery
  // window), so it cannot be recorded on its own or drift from that count.
  if (validated.sourceWindowSaturated !== undefined
    && (validated.returnedSectionCount === undefined
      || validated.sourceWindowSaturated
        !== (validated.returnedSectionCount >= SOURCE_DISCOVERY_WINDOW_LIMIT))) {
    throw new TypeError("Invalid search diagnostic details");
  }
  if (validated.outcome === "succeeded") {
    if (level === "error"
      || validated.resultCount === undefined
      || validated.lexicalMatchQuality === undefined
      || validated.sourceGeneration === undefined
      || validated.lexicalExecution === undefined
      || validated.code !== undefined
      || validated.failureCause !== undefined) {
      throw new TypeError("Invalid search diagnostic details");
    }
    if (validated.returnedSectionCount !== undefined
      && validated.returnedSectionCount !== validated.resultCount) {
      throw new TypeError("Invalid search diagnostic details");
    }
    return;
  }
  if (validated.outcome === "failed") {
    if (level !== "error" || validated.code === undefined || validated.resultCount !== undefined
      || validated.sourceWindowSaturated !== undefined) {
      throw new TypeError("Invalid search diagnostic details");
    }
    return;
  }
  if (validated.outcome === "superseded" || validated.outcome === "skipped") {
    if (level === "error" || validated.resultCount !== undefined || validated.failureCause !== undefined) {
      throw new TypeError("Invalid search diagnostic details");
    }
    return;
  }
  throw new TypeError("Invalid search diagnostic details");
}

function validateDetails(details: Readonly<DiagnosticDetails>): Readonly<DiagnosticDetails> {
  const entries = Object.entries(details);
  if (entries.length > MAX_DETAIL_FIELDS) throw new TypeError("Invalid diagnostic details");
  const validated: Partial<DiagnosticDetails> = Object.create(null) as Partial<DiagnosticDetails>;
  for (const [rawKey, value] of entries) {
    const key = DETAIL_KEYS.find((candidate) => candidate === rawKey);
    if (!key) throw new TypeError("Invalid diagnostic details");
    const normalized = validatedDetailValue(key, value);
    Object.defineProperty(validated, key, {
      value: normalized,
      enumerable: true,
      configurable: false,
      writable: false,
    });
  }
  return Object.freeze(validated);
}

const ERROR_NAME_PATTERN = /^[A-Za-z][A-Za-z0-9_]{0,63}$/u;

function validatedDetailValue(key: keyof DiagnosticDetails, value: unknown): unknown {
  // The single field that accepts a value outside the fixed vocabulary, and
  // therefore the one that has to justify itself. A JavaScript error class name
  // is a bare identifier; the pattern rejects anything containing a space,
  // slash, dot, or quote, so a path, query, or message cannot pass through it.
  if (key === "errorName") {
    if (typeof value === "string" && ERROR_NAME_PATTERN.test(value)) return value;
  } else if (key === "pathHash") {
    if (typeof value === "string" && HASH_PATTERN.test(value)) return value;
  } else if (key === "generationId") {
    if (typeof value === "string"
      && (IN_PLUGIN_GENERATION_PATTERN.test(value) || DAEMON_GENERATION_PATTERN.test(value))) {
      return value;
    }
  } else if (key === "activity") {
    if (typeof value === "string"
      && INDEX_ACTIVITY_SET.has(value as NonNullable<DiagnosticDetails["activity"]>)) return value;
  } else if (key === "stallCategory") {
    if (typeof value === "string"
      && INDEX_STALL_CATEGORY_SET.has(
        value as NonNullable<DiagnosticDetails["stallCategory"]>,
      )) return value;
  } else if (key === "mode" || key === "requestedMode" || key === "effectiveMode") {
    if (typeof value === "string" && SEARCH_MODE_SET.has(value as never)) return value;
  } else if (key === "lexicalProfile") {
    if (typeof value === "string" && LEXICAL_PROFILE_SET.has(value as never)) return value;
  } else if (key === "lexicalMatchQuality") {
    if (typeof value === "string" && LEXICAL_MATCH_QUALITY_SET.has(value as never)) return value;
  } else if (key === "fieldScope" || key === "fieldEmphasis") {
    if (typeof value === "string" && QUERY_FIELD_SET.has(value as never)) return value;
  } else if (key === "candidateWindowState") {
    if (typeof value === "string" && CANDIDATE_WINDOW_STATE_SET.has(value as never)) return value;
  } else if (key === "sourceGeneration") {
    return validateSourceGeneration(value);
  } else if (key === "lexicalExecution") {
    return validateLexicalExecution(value);
  } else if (key === "total" || NULLABLE_NUMERIC_DETAIL_KEYS.has(key)) {
    if (value === null || isNonNegativeInteger(value)) return value;
  } else if (NUMERIC_DETAIL_KEYS.has(key)) {
    if (isNonNegativeInteger(value)) return value;
  } else if (BOOLEAN_DETAIL_KEYS.has(key)) {
    if (typeof value === "boolean") return value;
  } else if (typeof value === "string" && TEXT_VALUE_SET.has(value as DiagnosticTextValue)) {
    return value;
  }
  throw new TypeError("Invalid diagnostic details");
}

const DIAGNOSTIC_SOURCE_FORMATS: readonly DiagnosticSourceFormat[] = [
  "markdown", "text", "base", "canvas", "docx", "pdf", "excalidraw", "excel", "html",
];
const DIAGNOSTIC_LANE_KINDS: readonly DiagnosticLexicalLaneKind[] = [
  "lexical_explicit_v3", "lexical_exact_metadata_v3", "lexical_exact_phrase_v3",
  "lexical_all_terms_v3", "lexical_partial_coverage_v3", "lexical_prefix_metadata_v3",
  "lexical_prefix_v3",
];
const DIAGNOSTIC_PROOF_FIELDS: readonly DiagnosticLexicalProofField[] = [
  "filename", "title", "alias", "heading", "tag", "body", "cross_field",
];
const SOURCE_FORMAT_SNAPSHOT_KEYS = [
  "policy", "indexedComplete", "indexedPartial", "skippedNoExtractableText",
  "unreadable", "quarantined",
] as const;
const LEXICAL_AGGREGATE_COUNT_KEYS = [
  "plannedLaneCount", "executedLaneCount", "zeroObservationLaneCount", "saturatedLaneCount",
  "observationCount", "addedUniqueCount",
] as const;

function validateSourceGeneration(value: unknown): DiagnosticSourceGeneration {
  const record = requireExactRecord(value, ["schemaVersion", "availability"], [
    "schemaVersion", "availability", "documents", "chunks", "zeroChunkSources", "formats",
  ]);
  if (record.schemaVersion !== 1) throw new TypeError("Invalid diagnostic details");
  if (record.availability === "unavailable" || record.availability === "not_applicable") {
    if (!hasExactKeys(record, ["schemaVersion", "availability"])) {
      throw new TypeError("Invalid diagnostic details");
    }
    return Object.freeze({ schemaVersion: 1, availability: record.availability });
  }
  if (record.availability !== "available"
    || !hasExactKeys(record, [
      "schemaVersion", "availability", "documents", "chunks", "zeroChunkSources", "formats",
    ])
    || !isNonNegativeInteger(record.documents)
    || !isNonNegativeInteger(record.chunks)
    || !isNonNegativeInteger(record.zeroChunkSources)
    || record.zeroChunkSources > record.documents) {
    throw new TypeError("Invalid diagnostic details");
  }
  const formatsRecord = requireExactRecord(record.formats, DIAGNOSTIC_SOURCE_FORMATS);
  const formats = Object.create(null) as Record<DiagnosticSourceFormat, DiagnosticSourceFormatSnapshot>;
  let indexedSources = 0;
  for (const format of DIAGNOSTIC_SOURCE_FORMATS) {
    const snapshot = requireExactRecord(formatsRecord[format], SOURCE_FORMAT_SNAPSHOT_KEYS);
    if (snapshot.policy !== "enabled" && snapshot.policy !== "disabled" && snapshot.policy !== "unknown") {
      throw new TypeError("Invalid diagnostic details");
    }
    for (const key of SOURCE_FORMAT_SNAPSHOT_KEYS.slice(1)) {
      if (!isNonNegativeInteger(snapshot[key])) throw new TypeError("Invalid diagnostic details");
    }
    if (snapshot.policy === "disabled"
      && SOURCE_FORMAT_SNAPSHOT_KEYS.slice(1).some((key) => Number(snapshot[key]) !== 0)) {
      throw new TypeError("Invalid diagnostic details");
    }
    indexedSources += Number(snapshot.indexedComplete) + Number(snapshot.indexedPartial);
    formats[format] = Object.freeze({
      policy: snapshot.policy,
      indexedComplete: Number(snapshot.indexedComplete),
      indexedPartial: Number(snapshot.indexedPartial),
      skippedNoExtractableText: Number(snapshot.skippedNoExtractableText),
      unreadable: Number(snapshot.unreadable),
      quarantined: Number(snapshot.quarantined),
    });
  }
  if (indexedSources !== record.documents) throw new TypeError("Invalid diagnostic details");
  return Object.freeze({
    schemaVersion: 1,
    availability: "available",
    documents: record.documents,
    chunks: record.chunks,
    zeroChunkSources: record.zeroChunkSources,
    formats: Object.freeze(formats),
  });
}

function validateLexicalExecution(value: unknown): DiagnosticLexicalExecution {
  const minimalKeys = ["schemaVersion", "availability"] as const;
  const fullKeys = [
    "schemaVersion", "availability", "disposition", "evidenceProbeCount",
    "matchedEvidenceProbeCount", "prefixProbeCount", "prefixExpansionCount",
    "plannedLaneCount", "executedLaneCount", "zeroObservationLaneCount",
    "saturatedLaneCount", "observationCount", "uniqueCandidateCount",
    "duplicateObservationCount", "collectionCapDiscardedObservationCount", "returnedCount",
    "resultLimit", "retainedCandidateTruncationCount", "candidateLimit",
    "candidateLimitReached", "lanes", "fields",
  ] as const;
  const record = requireExactRecord(value, minimalKeys, fullKeys);
  if (record.schemaVersion !== 1) throw new TypeError("Invalid diagnostic details");
  if (record.availability === "unavailable" || record.availability === "not_applicable") {
    if (!hasExactKeys(record, minimalKeys)) throw new TypeError("Invalid diagnostic details");
    return Object.freeze({ schemaVersion: 1, availability: record.availability });
  }
  if (record.availability !== "available"
    || !hasExactKeys(record, fullKeys)
    || (record.disposition !== "ready"
      && record.disposition !== "empty_no_evidence"
      && record.disposition !== "explicit_bypass")
    || typeof record.candidateLimitReached !== "boolean"
    || !Array.isArray(record.lanes)
    || !Array.isArray(record.fields)
    || record.lanes.length > DIAGNOSTIC_LANE_KINDS.length
    || record.fields.length > DIAGNOSTIC_PROOF_FIELDS.length) {
    throw new TypeError("Invalid diagnostic details");
  }
  const countKeys = [
    "evidenceProbeCount", "matchedEvidenceProbeCount", "prefixProbeCount",
    "prefixExpansionCount", "plannedLaneCount", "executedLaneCount",
    "zeroObservationLaneCount", "saturatedLaneCount", "observationCount",
    "uniqueCandidateCount", "duplicateObservationCount",
    "collectionCapDiscardedObservationCount", "returnedCount", "resultLimit",
    "retainedCandidateTruncationCount", "candidateLimit",
  ] as const;
  for (const key of countKeys) {
    if (!isNonNegativeInteger(record[key])) throw new TypeError("Invalid diagnostic details");
  }
  if (Number(record.matchedEvidenceProbeCount) > Number(record.evidenceProbeCount)
    || Number(record.executedLaneCount) > Number(record.plannedLaneCount)
    || Number(record.zeroObservationLaneCount) > Number(record.executedLaneCount)
    || Number(record.saturatedLaneCount) > Number(record.executedLaneCount)
    || Number(record.uniqueCandidateCount) > Number(record.candidateLimit)
    || Number(record.returnedCount) > Number(record.resultLimit)
    || Number(record.returnedCount) + Number(record.retainedCandidateTruncationCount)
      !== Number(record.uniqueCandidateCount)
    || Number(record.observationCount) !== Number(record.uniqueCandidateCount)
      + Number(record.duplicateObservationCount)
      + Number(record.collectionCapDiscardedObservationCount)
    || Number(record.plannedLaneCount) > 42
    || Number(record.executedLaneCount) > 42
    || Number(record.candidateLimit) > 512
    || Number(record.resultLimit) < 1
    || Number(record.resultLimit) > 100) {
    throw new TypeError("Invalid diagnostic details");
  }
  const lanes = validateLexicalAggregates(
    record.lanes,
    "kind",
    DIAGNOSTIC_LANE_KINDS,
  ) as DiagnosticLexicalLaneAggregate[];
  const fields = validateLexicalAggregates(
    record.fields,
    "field",
    DIAGNOSTIC_PROOF_FIELDS,
  ) as DiagnosticLexicalFieldAggregate[];
  validateAggregateTotals(record, lanes);
  validateAggregateTotals(record, fields);
  if (record.disposition === "empty_no_evidence"
    && (Number(record.plannedLaneCount) !== 0
      || Number(record.executedLaneCount) !== 0
      || Number(record.observationCount) !== 0
      || Number(record.uniqueCandidateCount) !== 0
      || Number(record.returnedCount) !== 0
      || lanes.length !== 0
      || fields.length !== 0)) {
    throw new TypeError("Invalid diagnostic details");
  }
  return Object.freeze({
    schemaVersion: 1,
    availability: "available",
    disposition: record.disposition,
    evidenceProbeCount: Number(record.evidenceProbeCount),
    matchedEvidenceProbeCount: Number(record.matchedEvidenceProbeCount),
    prefixProbeCount: Number(record.prefixProbeCount),
    prefixExpansionCount: Number(record.prefixExpansionCount),
    plannedLaneCount: Number(record.plannedLaneCount),
    executedLaneCount: Number(record.executedLaneCount),
    zeroObservationLaneCount: Number(record.zeroObservationLaneCount),
    saturatedLaneCount: Number(record.saturatedLaneCount),
    observationCount: Number(record.observationCount),
    uniqueCandidateCount: Number(record.uniqueCandidateCount),
    duplicateObservationCount: Number(record.duplicateObservationCount),
    collectionCapDiscardedObservationCount: Number(record.collectionCapDiscardedObservationCount),
    returnedCount: Number(record.returnedCount),
    resultLimit: Number(record.resultLimit),
    retainedCandidateTruncationCount: Number(record.retainedCandidateTruncationCount),
    candidateLimit: Number(record.candidateLimit),
    candidateLimitReached: record.candidateLimitReached,
    lanes: Object.freeze(lanes),
    fields: Object.freeze(fields),
  });
}

function validateLexicalAggregates(
  value: unknown[],
  identityKey: "kind" | "field",
  identities: readonly string[],
): Array<DiagnosticLexicalLaneAggregate | DiagnosticLexicalFieldAggregate> {
  const seen = new Set<string>();
  return value.map((candidate) => {
    const aggregate = requireExactRecord(candidate, [identityKey, ...LEXICAL_AGGREGATE_COUNT_KEYS]);
    const identity = aggregate[identityKey];
    if (typeof identity !== "string" || !identities.includes(identity) || seen.has(identity)) {
      throw new TypeError("Invalid diagnostic details");
    }
    seen.add(identity);
    for (const key of LEXICAL_AGGREGATE_COUNT_KEYS) {
      if (!isNonNegativeInteger(aggregate[key])) throw new TypeError("Invalid diagnostic details");
    }
    if (Number(aggregate.executedLaneCount) > Number(aggregate.plannedLaneCount)
      || Number(aggregate.zeroObservationLaneCount) > Number(aggregate.executedLaneCount)
      || Number(aggregate.saturatedLaneCount) > Number(aggregate.executedLaneCount)
      || Number(aggregate.addedUniqueCount) > Number(aggregate.observationCount)) {
      throw new TypeError("Invalid diagnostic details");
    }
    const counts = {
      plannedLaneCount: Number(aggregate.plannedLaneCount),
      executedLaneCount: Number(aggregate.executedLaneCount),
      zeroObservationLaneCount: Number(aggregate.zeroObservationLaneCount),
      saturatedLaneCount: Number(aggregate.saturatedLaneCount),
      observationCount: Number(aggregate.observationCount),
      addedUniqueCount: Number(aggregate.addedUniqueCount),
    };
    return identityKey === "kind"
      ? Object.freeze({ kind: identity as DiagnosticLexicalLaneKind, ...counts })
      : Object.freeze({ field: identity as DiagnosticLexicalProofField, ...counts });
  });
}

function validateAggregateTotals(
  record: Record<string, unknown>,
  aggregates: Array<DiagnosticLexicalLaneAggregate | DiagnosticLexicalFieldAggregate>,
): void {
  for (const key of LEXICAL_AGGREGATE_COUNT_KEYS) {
    const topLevelKey = key === "addedUniqueCount" ? "uniqueCandidateCount" : key;
    const total = aggregates.reduce((sum, aggregate) => sum + aggregate[key], 0);
    if (total !== Number(record[topLevelKey])) throw new TypeError("Invalid diagnostic details");
  }
}

function requireExactRecord(
  value: unknown,
  requiredKeys: readonly string[],
  allowedKeys: readonly string[] = requiredKeys,
): Record<string, unknown> {
  if (typeof value !== "object" || value === null || Array.isArray(value)) {
    throw new TypeError("Invalid diagnostic details");
  }
  const record = value as Record<string, unknown>;
  const keys = Object.keys(record);
  if (requiredKeys.some((key) => !keys.includes(key))
    || keys.some((key) => !allowedKeys.includes(key))) {
    throw new TypeError("Invalid diagnostic details");
  }
  return record;
}

function hasExactKeys(value: Record<string, unknown>, keys: readonly string[]): boolean {
  const actual = Object.keys(value);
  return actual.length === keys.length && keys.every((key) => actual.includes(key));
}

function isNonNegativeInteger(value: unknown): value is number {
  return typeof value === "number" && Number.isSafeInteger(value) && value >= 0;
}

function validDiagnosticTimestamp(value: number): number {
  if (!Number.isFinite(value) || value < 0 || value > 8.64e15) {
    throw new TypeError("Invalid diagnostic timestamp");
  }
  return value;
}

function nonNegativeSafeInteger(value: number, message: string): number {
  if (!Number.isSafeInteger(value) || value < 0) throw new TypeError(message);
  return value;
}

function defaultMonotonicNow(): number {
  return typeof performance === "undefined" ? Date.now() : performance.now();
}

function headerToken(value: string): string {
  if (!/^[0-9A-Za-z][0-9A-Za-z._+-]{0,127}$/u.test(value)) {
    throw new TypeError("Invalid diagnostic header");
  }
  return value;
}
