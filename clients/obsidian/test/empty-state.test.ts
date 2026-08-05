// SPDX-FileCopyrightText: 2026 cybersader
// SPDX-License-Identifier: GPL-3.0-only

import { describe, expect, it } from "vitest";

import {
  emptyStateMessage,
  searchErrorEmptyState,
  shouldNoticeSearchError,
} from "../src/empty-state";

describe("search modal empty state", () => {
  it("distinguishes a completed no-match search from an untyped query", () => {
    const noMatches = emptyStateMessage("no-matches", "retieval");
    const prompt = emptyStateMessage("prompt");
    expect(noMatches).not.toBe(prompt);
    // The typed text is echoed so a typo is visible to the user.
    expect(noMatches).toContain("retieval");
    expect(prompt).not.toContain("retieval");
  });

  it("echoes nothing when the query is only whitespace", () => {
    expect(emptyStateMessage("no-matches", "   ")).toBe("No matches.");
  });

  it("reports failure separately from an empty result set", () => {
    expect(emptyStateMessage("error")).not.toBe(emptyStateMessage("no-matches", "x"));
  });

  it("keeps user-correctable query errors inline instead of repeating notices", () => {
    expect(searchErrorEmptyState("explicit_query_unsupported"))
      .toBe("This explicit query is not supported by the in-plugin backend.");
    expect(searchErrorEmptyState("invalid_query"))
      .toBe("The query is invalid or exceeds the supported limits.");
    expect(shouldNoticeSearchError("explicit_query_unsupported")).toBe(false);
    expect(shouldNoticeSearchError("invalid_query")).toBe(false);
    expect(shouldNoticeSearchError("invalid_query_plan")).toBe(true);
  });

  it("fails closed for unknown error codes without echoing them", () => {
    const code = "Clients/Acme secret query";
    const message = searchErrorEmptyState(code);
    expect(message).toBe("Search is unavailable. See the notice for details.");
    expect(message).not.toContain("Acme");
  });

  it("never renders note content or a path in the message", () => {
    const message = emptyStateMessage("no-matches", "secret/private note.md");
    // The query is echoed verbatim by design; the message must not add any
    // other vault-derived text around it.
    expect(message).toBe("No matches for “secret/private note.md”.");
  });
});
