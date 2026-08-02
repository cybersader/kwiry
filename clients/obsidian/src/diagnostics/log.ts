// SPDX-FileCopyrightText: 2026 cybersader
// SPDX-License-Identifier: GPL-3.0-only
//
// Diagnostics are intentionally independent of Obsidian so the privacy boundary
// and bounded-storage behavior can be exercised without a plugin runtime.

export const DEFAULT_DIAGNOSTIC_CAPACITY = 512;

const MAX_DIAGNOSTIC_CAPACITY = 10_000;
const MAX_DETAIL_FIELDS = 48;

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
  | "mode_unavailable"
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
  | "query_rejected"
  | "integrity_failed"
  | "cache_identity_mismatch"
  | "cache_version_mismatch"
  | "cache_digest_mismatch"
  | "cache_image_invalid"
  | "cache_blob_too_large"
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

export interface DiagnosticDetails {
  profile?: "daemon" | "in_plugin";
  phase?: DiagnosticTextValue;
  stage?: DiagnosticTextValue;
  liveness?: "unknown" | "alive" | "unreachable" | "terminated";
  mode?: "lexical" | "semantic" | "hybrid";
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
  resultCount?: number;
  cacheBytes?: number;
  pluginLoadCompleteMs?: number | null;
  layoutReadyMs?: number | null;
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
  | "resultCount"
  | "cacheBytes";

export interface DiagnosticEventBuilder {
  set(details: Readonly<DiagnosticDetails>): void;
  increment(counter: DiagnosticCounter, amount?: number): void;
  setLevel(level: DiagnosticLevel): void;
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
  "lifecycle", "snapshot", "replay", "rebuild", "create", "modify", "delete", "rename",
  "activate", "dispose", "initialize", "status", "search", "open", "load", "restore",
  "export", "discard", "build", "reconcile", "update", "poll", "save", "copy", "clear",
  "requested", "started", "succeeded", "failed", "cancelled", "superseded", "hit", "miss",
  "scheduled", "skipped", "aborted", "restored", "discarded", "exported", "plugin",
  "backend_manager", "daemon_backend", "in_plugin_backend", "index_controller", "cache_store",
  "vault_source", "search_session", "worker", "rpc", "vfs", "settings", "ui",
  "vault_read_failed", "index_build_failed", "index_update_failed", "index_limit_exceeded",
  "index_reconciling", "index_building", "worker_recovering", "worker_failed", "cache_absent",
  "cache_unavailable", "cache_corrupt", "cache_incompatible", "cache_restore_unavailable",
  "cache_discard_failed", "cache_save_failed", "absent", "pointer_unreadable", "pointer_corrupt",
  "identity_mismatch", "image_absent", "image_unreadable", "image_length_mismatch",
  "unsupported_platform", "no_machine_local_root", "root_not_absolute", "root_not_machine_local",
  "root_inside_vault", "root_not_a_directory", "root_not_writable", "root_probe_failed",
  "vault_location_unavailable", "invalid_generation_id", "invalid_identity", "invalid_blob",
  "write_failed", "discard_failed", "unsafe_path", "locked", "daemon_unreachable",
  "mode_unavailable", "internal_error",
  // Constructor names of errors this codebase and the JS runtime define.
  // These are fixed identifiers chosen here, not caller-supplied text, so
  // recording one leaks nothing while turning an "unknown" report into a
  // named fault.
  "BlockVfsUnavailableError", "IndexCapacityError", "IndexIntegrityError",
  "CacheImageInvalidError", "CacheVersionMismatchError", "VaultSourceReadError",
  "WorkerRpcError", "RustAdapterError", "TypeError", "RangeError",
  "ReferenceError", "SyntaxError", "Error", "other", "rust", "sqlite", "artifact",
  "fts5_unavailable", "rust_init_failed", "sqlite_init_failed", "artifact_mismatch", "protocol_mismatch", "invalid_request", "invalid_state", "source_rejected", "query_rejected", "integrity_failed", "cache_identity_mismatch", "cache_version_mismatch", "cache_digest_mismatch", "cache_image_invalid", "cache_blob_too_large", "worker_crashed", "timeout",
  "fully_current", "sources_omitted", "vault_unavailable", "index_capacity", "backend_unavailable",
  "plugin_load_failed", "activation_failed", "plugin_unloaded",
];
const DETAIL_KEYS: readonly (keyof DiagnosticDetails)[] = [
  "profile", "phase", "stage", "liveness", "mode", "outcome", "code", "reason", "errorName", "operation",
  "subsystem", "generationId", "pathHash", "pluginEpoch", "activationEpoch", "mutationEpoch",
  "count", "limit", "documents", "chunks", "completed", "total", "warningCount", "pending",
  "sourcesEnumerated", "sourcesRead", "sourcesSkipped", "sourcesOversized", "sourcesFailed",
  "bytesRead", "batchCount", "upserts", "removals", "resultCount", "cacheBytes",
  "pluginLoadCompleteMs", "layoutReadyMs", "firstCacheSearchableMs", "fullyCurrentMs", "retryable",
  "recoverable", "searchable", "dirty", "rebuilding", "cacheHit", "recovery",
];
const NUMERIC_DETAIL_KEYS = new Set<keyof DiagnosticDetails>([
  "pluginEpoch", "activationEpoch", "mutationEpoch", "count", "limit", "documents", "chunks",
  "completed", "warningCount", "pending", "sourcesEnumerated", "sourcesRead", "sourcesSkipped",
  "sourcesOversized", "sourcesFailed", "bytesRead", "batchCount", "upserts", "removals",
  "resultCount", "cacheBytes",
]);
const NULLABLE_NUMERIC_DETAIL_KEYS = new Set<keyof DiagnosticDetails>([
  "pluginLoadCompleteMs", "layoutReadyMs", "firstCacheSearchableMs", "fullyCurrentMs",
]);
const BOOLEAN_DETAIL_KEYS = new Set<keyof DiagnosticDetails>([
  "retryable", "recoverable", "searchable", "dirty", "rebuilding", "cacheHit", "recovery",
]);
const STARTUP_DETAIL_KEYS = new Set<keyof DiagnosticDetails>([
  "profile", "outcome", "reason", "pluginEpoch", "activationEpoch", "pluginLoadCompleteMs",
  "layoutReadyMs", "firstCacheSearchableMs", "fullyCurrentMs", "cacheHit", "cacheBytes",
]);
const REQUIRED_STARTUP_DETAIL_KEYS: readonly (keyof DiagnosticDetails)[] = [
  "profile", "outcome", "reason", "pluginEpoch", "activationEpoch", "pluginLoadCompleteMs",
  "layoutReadyMs", "firstCacheSearchableMs", "fullyCurrentMs", "cacheHit",
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
  "upserts", "removals", "resultCount", "cacheBytes",
]);
const TEXT_VALUE_SET = new Set<DiagnosticTextValue>(TEXT_VALUES);
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
    const event = new MutableDiagnosticEvent(level, initialDetails);
    try {
      const result = await operation(event);
      event.defaultOutcome("succeeded");
      return result;
    } catch (error) {
      event.setLevel("error");
      event.set({ outcome: "failed" });
      event.defaultCode("internal_error");
      throw error;
    } finally {
      // A broken monotonic clock must not erase the operation record that the
      // wrapper exists to guarantee; zero duration is safer than wall-clock skew.
      const endedAtMs = this.readMonotonicClockOr(monotonicStartedAtMs);
      const durationMs = Math.max(0, Math.round(endedAtMs - monotonicStartedAtMs));
      this.append(event.finish(code, startedAtMs, durationMs));
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
    const event = new MutableDiagnosticEvent(level, details);
    event.defaultOutcome("succeeded");
    this.append(event.finish(
      code,
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

export function formatDiagnosticLog(
  log: DiagnosticLog,
  context: DiagnosticExportContext,
  minimumLevel: DiagnosticLevel = "debug",
): string {
  if (!PLATFORM_SET.has(context.platform) || !PROFILE_SET.has(context.backendProfile)) {
    throw new TypeError("Invalid diagnostic header");
  }
  const snapshot = log.snapshot(minimumLevel);
  const safeContext = {
    pluginVersion: headerToken(context.pluginVersion),
    obsidianVersion: headerToken(context.obsidianVersion),
    platform: context.platform,
    backendProfile: context.backendProfile,
  };
  const lines = [
    "Kwiry diagnostics log",
    `plugin_version: ${safeContext.pluginVersion}`,
    `obsidian_version: ${safeContext.obsidianVersion}`,
    `platform: ${safeContext.platform}`,
    `backend_profile: ${safeContext.backendProfile}`,
    `capacity: ${snapshot.capacity}`,
    `stored_entries: ${snapshot.entries.length}`,
    `dropped_entries: ${snapshot.dropped}`,
    `minimum_level: ${minimumLevel}`,
    "",
    "Summary:",
  ];
  for (const entry of snapshot.entries) {
    const detail = DETAIL_KEYS.flatMap((key) => {
      const value = entry.details[key];
      return value === undefined ? [] : [`${key}=${String(value)}`];
    }).join(" ");
    const prefix = `${entry.sequence} ${new Date(entry.startedAtMs).toISOString()} +${entry.durationMs}ms ${entry.level.toUpperCase()} ${entry.code}`;
    lines.push(detail.length === 0 ? prefix : `${prefix} ${detail}`);
  }
  if (snapshot.entries.length === 0) lines.push("(no retained events at this level)");
  lines.push("", "Structured records (JSON):");
  lines.push(JSON.stringify({
    schemaVersion: 1,
    context: safeContext,
    capacity: snapshot.capacity,
    storedEntries: snapshot.entries.length,
    droppedEntries: snapshot.dropped,
    minimumLevel,
    records: snapshot.entries,
  }, null, 2));
  return `${lines.join("\n")}\n`;
}

class MutableDiagnosticEvent implements DiagnosticEventBuilder {
  private level: DiagnosticLevel;
  private readonly details: Partial<DiagnosticDetails>;
  private finished = false;

  constructor(level: DiagnosticLevel, initialDetails: Readonly<DiagnosticDetails>) {
    this.level = level;
    this.details = { ...validateDetails(initialDetails) };
  }

  set(details: Readonly<DiagnosticDetails>): void {
    this.requireOpen();
    const validated = validateDetails(details);
    if (new Set([...Object.keys(this.details), ...Object.keys(validated)]).size > MAX_DETAIL_FIELDS) {
      throw new TypeError("Invalid diagnostic details");
    }
    Object.assign(this.details, validated);
  }

  increment(counter: DiagnosticCounter, amount = 1): void {
    this.requireOpen();
    if (!COUNTERS.has(counter) || !isNonNegativeInteger(amount)) {
      throw new TypeError("Invalid diagnostic counter");
    }
    const current = this.details[counter] ?? 0;
    const next = current + amount;
    if (!Number.isSafeInteger(next)) throw new TypeError("Invalid diagnostic counter");
    this.details[counter] = next;
  }

  setLevel(level: DiagnosticLevel): void {
    this.requireOpen();
    if (!LEVEL_SET.has(level)) throw new TypeError("Invalid diagnostic level");
    this.level = level;
  }

  defaultOutcome(outcome: DiagnosticTextValue): void {
    if (this.details.outcome === undefined) this.details.outcome = outcome;
  }

  defaultCode(code: DiagnosticTextValue): void {
    if (this.details.code === undefined) this.details.code = code;
  }

  finish(
    code: DiagnosticEventCode,
    startedAtMs: number,
    durationMs: number,
  ): Omit<DiagnosticEntry, "sequence"> {
    this.requireOpen();
    this.finished = true;
    return Object.freeze({
      startedAtMs,
      durationMs,
      level: this.level,
      code,
      details: validateEventDetails(code, this.details),
    });
  }

  private requireOpen(): void {
    if (this.finished) throw new Error("Diagnostic event is already committed");
  }
}

function validateEventDetails(
  code: DiagnosticEventCode,
  details: Readonly<DiagnosticDetails>,
): Readonly<DiagnosticDetails> {
  const validated = validateDetails(details);
  if (code !== "startup.lifecycle") return validated;
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
  return validated;
}

function validateDetails(details: Readonly<DiagnosticDetails>): Readonly<DiagnosticDetails> {
  const entries = Object.entries(details);
  if (entries.length > MAX_DETAIL_FIELDS) throw new TypeError("Invalid diagnostic details");
  const validated: Partial<DiagnosticDetails> = Object.create(null) as Partial<DiagnosticDetails>;
  for (const [rawKey, value] of entries) {
    const key = DETAIL_KEYS.find((candidate) => candidate === rawKey);
    if (!key || !isValidDetailValue(key, value)) {
      throw new TypeError("Invalid diagnostic details");
    }
    Object.defineProperty(validated, key, {
      value,
      enumerable: true,
      configurable: false,
      writable: false,
    });
  }
  return Object.freeze(validated);
}

const ERROR_NAME_PATTERN = /^[A-Za-z][A-Za-z0-9_]{0,63}$/u;

function isValidDetailValue(key: keyof DiagnosticDetails, value: unknown): boolean {
  // The single field that accepts a value outside the fixed vocabulary, and
  // therefore the one that has to justify itself. A JavaScript error class name
  // is a bare identifier; the pattern rejects anything containing a space,
  // slash, dot, or quote, so a path, query, or message cannot pass through it.
  if (key === "errorName") return typeof value === "string" && ERROR_NAME_PATTERN.test(value);
  if (key === "pathHash") return typeof value === "string" && HASH_PATTERN.test(value);
  if (key === "generationId") {
    return typeof value === "string"
      && (IN_PLUGIN_GENERATION_PATTERN.test(value) || DAEMON_GENERATION_PATTERN.test(value));
  }
  if (key === "total" || NULLABLE_NUMERIC_DETAIL_KEYS.has(key)) {
    return value === null || isNonNegativeInteger(value);
  }
  if (NUMERIC_DETAIL_KEYS.has(key)) return isNonNegativeInteger(value);
  if (BOOLEAN_DETAIL_KEYS.has(key)) return typeof value === "boolean";
  return typeof value === "string" && TEXT_VALUE_SET.has(value as DiagnosticTextValue);
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
