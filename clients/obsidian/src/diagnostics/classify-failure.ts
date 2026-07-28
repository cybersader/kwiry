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

export interface FailureClassification {
  readonly subsystem: FailureSubsystem;
  readonly reason: FailureReason;
  /// True when the value thrown was not an Error at all. A non-Error rejection
  /// is itself a defect worth seeing in a report, and it is invisible once the
  /// cause is reduced to an enum.
  readonly nonError: boolean;
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
  const nonError = !(error instanceof Error);
  const name = safeRead(error, "name") ?? "";
  const code = safeRead(error, "code") ?? "";
  const message = safeRead(error, "message") ?? "";
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

  return { subsystem, reason, nonError };
}
