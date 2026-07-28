// SPDX-FileCopyrightText: 2026 cybersader
// SPDX-License-Identifier: GPL-3.0-only
//
// Maps a thrown value to a safe, fixed-vocabulary cause so a field report can
// say WHICH subsystem failed. Kept free of any Obsidian import so it is
// directly testable.
//
// The first real field report showed why this is needed: the controller
// reported `index_build_failed`, which is its catch-all branch -- everything
// that is not a vault read error and not a size limit -- and the raw cause was
// discarded before it could be recorded. That is enough to know something
// broke and nothing at all about what.
//
// The output is deliberately a closed enum rather than the error message.
// Messages routinely embed paths and identifiers, and this log is meant to be
// pasted into a chat, so a message would defeat the redaction boundary the
// rest of the diagnostics layer enforces.

export type FailureSubsystem =
  | "worker"
  | "rpc"
  | "vfs"
  | "vault_source"
  | "cache_store"
  | "index_controller"
  | "unknown";

export type FailureReason =
  | "worker_failed"
  | "index_limit_exceeded"
  | "vault_read_failed"
  | "unsafe_path"
  | "locked"
  | "write_failed"
  | "invalid_blob"
  | "internal_error";

/// Exact constructor names, matched before any pattern. A field report that
/// says only "unknown" costs another release round-trip, and the first one did
/// exactly that: the thrown value was a real Error whose name and message
/// matched none of the patterns below. A class name is a fixed identifier
/// chosen by this codebase, not caller text, so echoing it leaks nothing.
export type FailureErrorName =
  | (string & {})
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
  | "other";

/// An identifier-shaped name is echoed verbatim when it is not on the known
/// list. Three releases were spent testing the name against lists that kept
/// missing it, each round-trip reporting only "other" -- so the list was the
/// wrong instrument. A JavaScript error class name is a bare identifier
/// (`DataCloneError`, `QuotaExceededError`), never a path, query, or sentence:
/// this pattern admits nothing that could carry vault content, and anything
/// failing it is still reported as "other".
const IDENTIFIER_NAME = /^[A-Za-z][A-Za-z0-9_]{0,63}$/u;

const KNOWN_ERROR_NAMES = new Set<FailureErrorName>([
  "BlockVfsUnavailableError", "IndexCapacityError", "IndexIntegrityError",
  "CacheImageInvalidError", "CacheVersionMismatchError", "VaultSourceReadError",
  "WorkerRpcError", "RustAdapterError", "TypeError", "RangeError",
  "ReferenceError", "SyntaxError", "Error",
]);

/// The Worker protocol's own error vocabulary, mirrored here rather than
/// imported so this module stays free of Worker types. A WorkerError is a
/// plain `{code, stage, message, retryable}` object, not an Error subclass, so
/// it carries no constructor name -- which is why the first three field
/// reports could only say "other". Its code is the precise answer.
export type WorkerFailureCode =
  | "protocol_mismatch" | "invalid_request" | "invalid_state" | "artifact_mismatch"
  | "rust_init_failed" | "sqlite_init_failed" | "fts5_unavailable" | "source_rejected"
  | "query_rejected" | "index_building" | "index_limit_exceeded" | "integrity_failed"
  | "cache_identity_mismatch" | "cache_version_mismatch" | "cache_digest_mismatch"
  | "cache_image_invalid" | "cache_blob_too_large" | "worker_crashed" | "timeout"
  | "disposed" | "internal_error";

const WORKER_ERROR_CODES = new Set<WorkerFailureCode>([
  "protocol_mismatch", "invalid_request", "invalid_state", "artifact_mismatch",
  "rust_init_failed", "sqlite_init_failed", "fts5_unavailable", "source_rejected",
  "query_rejected", "index_building", "index_limit_exceeded", "integrity_failed",
  "cache_identity_mismatch", "cache_version_mismatch", "cache_digest_mismatch",
  "cache_image_invalid", "cache_blob_too_large", "worker_crashed", "timeout",
  "disposed", "internal_error",
]);

function workerSubsystem(code: WorkerFailureCode): FailureSubsystem {
  if (code === "rust_init_failed") return "worker";
  if (code === "sqlite_init_failed" || code === "fts5_unavailable") return "vfs";
  if (code.startsWith("cache_")) return "cache_store";
  if (code === "worker_crashed" || code === "timeout") return "rpc";
  return "worker";
}

export interface FailureClassification {
  readonly subsystem: FailureSubsystem;
  readonly reason: FailureReason;
  /// Present only when the thrown value was a Worker protocol error. This is
  /// the most specific field in the classification; prefer it when reading a
  /// report.
  readonly workerCode?: WorkerFailureCode;
  /// The thrown value's constructor name when it is one this codebase or the
  /// JS runtime defines, otherwise "other". A raw RangeError or TypeError
  /// here means an unhandled programming fault rather than a handled
  /// subsystem failure, which is the single most useful thing to know first.
  readonly errorName: FailureErrorName;
  /// True when the value thrown was not an Error at all. A non-Error rejection
  /// is itself a defect worth seeing in a report, and it is invisible once the
  /// cause is reduced to an enum.
  readonly nonError: boolean;
}

/// Returns the first cause of an AggregateError, or the value unchanged.
/// Bounded to three levels so a self-referential or deeply nested chain cannot
/// spin here; diagnostics must never hang the failure path they observe.
function unwrapAggregate(error: unknown, depth = 0): unknown {
  if (depth >= 3) return error;
  if (typeof error !== "object" || error === null) return error;
  let nested: unknown;
  try {
    nested = (error as { errors?: unknown }).errors;
  } catch {
    return error;
  }
  if (!Array.isArray(nested) || nested.length === 0) return error;
  return unwrapAggregate(nested[0], depth + 1);
}

/// Reads a property without trusting the object. A rejected value may be a
/// proxy or carry a throwing getter, in which case reading `name` or `message`
/// would replace the real failure with the getter's failure -- turning
/// diagnostics into the bug it is meant to diagnose.
function safeRead(value: unknown, key: string): string | null {
  if (typeof value !== "object" && typeof value !== "function") return null;
  if (value === null) return null;
  try {
    const read: unknown = (value as Record<string, unknown>)[key];
    return typeof read === "string" ? read : null;
  } catch {
    return null;
  }
}

const SUBSYSTEM_PATTERNS: readonly (readonly [RegExp, FailureSubsystem])[] = [
  [/\bvfs\b|blockvfs/iu, "vfs"],
  [/\brpc\b|postmessage|worker_crashed/iu, "rpc"],
  [/worker/iu, "worker"],
  [/cache|image|pointer/iu, "cache_store"],
  [/vault|source|read/iu, "vault_source"],
];

const REASON_PATTERNS: readonly (readonly [RegExp, FailureReason])[] = [
  [/limit|capacity|too large|oversized/iu, "index_limit_exceeded"],
  [/unsafe path|escape|traversal/iu, "unsafe_path"],
  [/lock/iu, "locked"],
  [/write|persist|save/iu, "write_failed"],
  [/invalid|malformed|corrupt|mismatch/iu, "invalid_blob"],
  [/vault|read/iu, "vault_read_failed"],
  [/worker|rpc|vfs|crash|terminate/iu, "worker_failed"],
];

/// Classifies a thrown value. Never throws, and never returns caller-supplied
/// text: the name and message are matched against fixed patterns and then
/// discarded.
export function classifyFailure(error: unknown): FailureClassification {
  // An AggregateError carries its real causes in `errors` and leaves `code`
  // absent and `message` a generic wrapper sentence. Five field reports read
  // only the outer shell and learned nothing, because the controller wraps a
  // failed build together with a failed staging abort. Unwrap to the first
  // cause before classifying: the original failure is what a report needs.
  const unwrapped = unwrapAggregate(error);
  const nonError = !(unwrapped instanceof Error);
  const name = safeRead(unwrapped, "name") ?? "";
  const code = safeRead(unwrapped, "code") ?? "";
  const message = safeRead(unwrapped, "message") ?? "";
  const haystack = `${name} ${code} ${message}`;

  let subsystem: FailureSubsystem = "unknown";
  for (const [pattern, candidate] of SUBSYSTEM_PATTERNS) {
    if (pattern.test(haystack)) {
      subsystem = candidate;
      break;
    }
  }

  let reason: FailureReason = "internal_error";
  for (const [pattern, candidate] of REASON_PATTERNS) {
    if (pattern.test(haystack)) {
      reason = candidate;
      break;
    }
  }

  const errorName: FailureErrorName = KNOWN_ERROR_NAMES.has(name as FailureErrorName)
    ? (name as FailureErrorName)
    : IDENTIFIER_NAME.test(name)
      ? name
      : "other";

  // A WorkerError is a plain object, not an Error subclass, so it has no name
  // and falls through everything above as "other" -- which is exactly what the
  // first three field reports said. Its `code` is a closed protocol vocabulary
  // and names the real fault precisely, so prefer it over any inference.
  const workerCode = WORKER_ERROR_CODES.has(code as WorkerFailureCode)
    ? (code as WorkerFailureCode)
    : null;

  return {
    subsystem: workerCode === null ? subsystem : workerSubsystem(workerCode),
    reason,
    errorName,
    ...(workerCode === null ? {} : { workerCode }),
    nonError,
  };
}
