// SPDX-FileCopyrightText: 2026 cybersader
// SPDX-License-Identifier: GPL-3.0-only

import { describe, expect, it } from "vitest";

import type { BackendStatus, CandidateWindowFacts } from "../src/backend";
import {
  presentBackgroundIndex,
  presentQueryStatus,
  type QueryStatusFacts,
} from "../src/search-status-presenter";

function window(state: CandidateWindowFacts["state"]): CandidateWindowFacts {
  return {
    state,
    candidateCount: state === "unknown" ? null : 32,
    candidateLimit: state === "candidate_limit_reached" ? 32 : null,
  };
}

function status(overrides: Partial<BackendStatus> = {}): BackendStatus {
  return {
    identity: {
      profile: "in_plugin",
      label: "In-plugin",
      instanceId: "i",
      boundVaultId: "vault",
    },
    phase: "ready",
    liveness: "alive",
    searchable: true,
    generation: "g1",
    capabilities: {
      supportedModes: ["lexical"],
      sourceScope: "active_vault",
      manualRebuild: true,
    },
    documents: 10,
    chunks: 20,
    dirty: false,
    rebuilding: false,
    ...overrides,
  };
}

describe("presentQueryStatus", () => {
  it.each<{
    name: string;
    facts: QueryStatusFacts;
    expected: ReturnType<typeof presentQueryStatus>;
  }>([
    {
      name: "prompt",
      facts: { phase: "prompt" },
      expected: { state: "prompt", text: "Type to search your notes.", busy: false },
    },
    {
      name: "current request searching",
      facts: { phase: "searching" },
      expected: { state: "searching", text: "Searching…", busy: true },
    },
    {
      name: "one settled result with an exhausted window",
      facts: {
        phase: "settled",
        resultCount: 1,
        candidateWindow: window("exhausted"),
      },
      expected: {
        state: "results",
        text: "1 result returned — search window complete.",
        busy: false,
      },
    },
    {
      name: "settled results with observed extra candidates",
      facts: {
        phase: "settled",
        resultCount: 7,
        candidateWindow: window("more_available"),
      },
      expected: {
        state: "results",
        text: "7 results returned — more candidates are available.",
        busy: false,
      },
    },
    {
      name: "settled results at the candidate limit",
      facts: {
        phase: "settled",
        resultCount: 20,
        candidateWindow: window("candidate_limit_reached"),
      },
      expected: {
        state: "results",
        text: "20 results returned — candidate window limit reached.",
        busy: false,
      },
    },
    {
      name: "settled results with unknown completeness",
      facts: {
        phase: "settled",
        resultCount: 20,
        candidateWindow: window("unknown"),
      },
      expected: {
        state: "results",
        text: "20 results returned — window completeness is unknown.",
        busy: false,
      },
    },
    {
      name: "no match with a complete window",
      facts: {
        phase: "settled",
        resultCount: 0,
        candidateWindow: window("exhausted"),
      },
      expected: {
        state: "no-match",
        text: "No matches — search window complete.",
        busy: false,
      },
    },
    {
      name: "no match at a bounded candidate limit",
      facts: {
        phase: "settled",
        resultCount: 0,
        candidateWindow: window("candidate_limit_reached"),
      },
      expected: {
        state: "no-match",
        text: "No matches — candidate window limit reached.",
        busy: false,
      },
    },
    {
      name: "user-correctable query guidance",
      facts: {
        phase: "error",
        code: "invalid_query",
        safeMessage: "Ignored backend wording.",
      },
      expected: {
        state: "error",
        text: "The query is invalid or exceeds the supported limits.",
        busy: false,
      },
    },
    {
      name: "specific safe backend error",
      facts: {
        phase: "error",
        code: "worker_recovering",
        safeMessage: "The index worker is recovering. Try again shortly.",
      },
      expected: {
        state: "error",
        text: "The index worker is recovering. Try again shortly.",
        busy: false,
      },
    },
  ])("derives $name", ({ facts, expected }) => {
    expect(presentQueryStatus(facts)).toEqual(expected);
  });

  it("never promotes candidate counts into result or corpus totals", () => {
    const facts: QueryStatusFacts = {
      phase: "settled",
      resultCount: 3,
      candidateWindow: {
        state: "candidate_limit_reached",
        candidateCount: 512,
        candidateLimit: 512,
      },
    };

    expect(presentQueryStatus(facts).text).toBe(
      "3 results returned — candidate window limit reached.",
    );
  });
});

describe("presentBackgroundIndex", () => {
  it("keeps ready indexing status quiet", () => {
    expect(presentBackgroundIndex(status())).toEqual({ state: "quiet", text: "" });
  });

  it("presents aggregate progress separately from query status", () => {
    expect(presentBackgroundIndex(status({
      phase: "building",
      searchable: false,
      dirty: true,
      progress: {
        stage: "snapshot",
        activity: "read",
        completed: 8,
        total: 10,
        inFlight: 2,
      },
    }))).toEqual({
      state: "indexing",
      text: "Index · Reading 8/10 (80%) · 2 in flight",
    });
  });

  it("keeps aggregate omissions visible without exposing paths", () => {
    const presentation = presentBackgroundIndex(status({
      phase: "degraded",
      quarantinedSources: 1,
      unreadableSources: 2,
    }));
    expect(presentation).toEqual({
      state: "attention",
      text: "Index · 3 notes may be missing from search (1 quarantined, 2 unreadable)",
    });
    expect(presentation.text).not.toContain("/");
  });
});
