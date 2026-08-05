// SPDX-FileCopyrightText: 2026 cybersader
// SPDX-License-Identifier: GPL-3.0-only
//
// The message shown where search results would appear. Kept free of any
// Obsidian import so it is directly testable: a completed search that
// matched nothing must be distinguishable from an untyped query and from a
// failure, which is not true when all three render an empty list.

export type EmptyStateKind = "prompt" | "no-matches" | "error";

const USER_CORRECTABLE_QUERY_ERRORS = new Set([
  "explicit_query_unsupported",
  "invalid_query",
]);

/// Builds the empty-state text. The query is echoed verbatim so a user can
/// see exactly what was searched — including a typo, which is the whole
/// point of distinguishing this state.
export function emptyStateMessage(kind: EmptyStateKind, query = ""): string {
  switch (kind) {
    case "no-matches": {
      const trimmed = query.trim();
      return trimmed.length > 0 ? `No matches for “${trimmed}”.` : "No matches.";
    }
    case "error":
      return "Search is unavailable. See the notice for details.";
    case "prompt":
      return "Type to search your notes.";
  }
}

export function searchErrorEmptyState(code: string): string {
  switch (code) {
    case "explicit_query_unsupported":
      return "This explicit query is not supported by the in-plugin backend.";
    case "invalid_query":
      return "The query is invalid or exceeds the supported limits.";
    default:
      return emptyStateMessage("error");
  }
}

export function shouldNoticeSearchError(code: string): boolean {
  return !USER_CORRECTABLE_QUERY_ERRORS.has(code);
}
