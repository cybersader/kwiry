// SPDX-FileCopyrightText: 2026 cybersader
// SPDX-License-Identifier: GPL-3.0-only

import { describe, expect, it } from "vitest";

import type { BackendStatus } from "../src/backend";
import { formatStatus } from "../src/status-format";

const base: BackendStatus = {
  identity: {
    profile: "in_plugin",
    instanceId: "in_plugin-1",
    label: "In-plugin",
    boundVaultId: "active-vault",
  },
  phase: "ready",
  liveness: "alive",
  searchable: true,
  generation: "generation-1",
  capabilities: {
    supportedModes: ["lexical"],
    sourceScope: "active_vault",
    manualRebuild: true,
  },
  documents: 2,
  chunks: 3,
  dirty: false,
  rebuilding: false,
};

describe("formatStatus", () => {
  it("uses concise ready and unavailable states", () => {
    expect(formatStatus(base)).toBe("Kwiry: Ready");
    expect(formatStatus({
      ...base,
      phase: "unavailable",
      liveness: "unreachable",
      searchable: false,
      generation: null,
    })).toBe("Kwiry: Index unavailable");
  });

  it("reports count and percentage with distinct indexing verbs", () => {
    const cases = [
      ["snapshot", "Kwiry: Indexing 42/900 (4%)"],
      ["rebuild", "Kwiry: Rebuilding 42/900 (4%)"],
    ] as const;

    for (const [stage, expected] of cases) {
      expect(formatStatus({
        ...base,
        phase: "building",
        searchable: false,
        dirty: true,
        progress: { stage, completed: 42, total: 900 },
      })).toBe(expected);
    }
  });

  it("reports reconciliation planning, verification, and application honestly", () => {
    const cases: Array<[NonNullable<BackendStatus["progress"]>, string]> = [
      [{ stage: "replay", subphase: "planning", completed: 0, total: null },
        "Kwiry: Planning reconciliation…"],
      [{ stage: "replay", subphase: "verifying", completed: 42, total: 900 },
        "Kwiry: Verifying 42/900 (4%)"],
      [{ stage: "replay", subphase: "applying", completed: 2, total: 5 },
        "Kwiry: Applying changes 2/5 (40%)"],
    ];

    for (const [progress, expected] of cases) {
      expect(formatStatus({
        ...base,
        phase: "building",
        searchable: true,
        dirty: true,
        progress,
      })).toBe(expected);
    }
  });

  it("uses a truthful starting state before the total is known", () => {
    expect(formatStatus({
      ...base,
      phase: "building",
      searchable: false,
      generation: null,
      dirty: true,
      progress: { stage: "snapshot", completed: 0, total: null },
    })).toBe("Kwiry: Starting index…");
  });

  it("keeps dirty searchable state distinct when no counter is available", () => {
    expect(formatStatus({ ...base, dirty: true })).toBe("Kwiry: Ready · Updating");
  });

  it("summarizes source omissions without exposing validation machinery", () => {
    expect(formatStatus({
      ...base,
      phase: "degraded",
      quarantinedSources: 2,
      unreadableSources: 1,
      quarantineValidatorFields: ["chunks_contents"],
      issue: {
        code: "sources_quarantined",
        safeMessage: "3 notes may be missing from search (2 quarantined, 1 unreadable).",
        recoverable: true,
      },
    })).toBe("Kwiry: Ready · 3 notes incomplete");
  });

  it("never exposes old profile, question-mark, or verbose issue copy", () => {
    const statuses: BackendStatus[] = [
      base,
      {
        ...base,
        phase: "building",
        searchable: false,
        generation: null,
        dirty: true,
        progress: { stage: "snapshot", completed: 0, total: null },
        issue: {
          code: "cache_absent",
          safeMessage: "No cached index; building a fresh index…",
          recoverable: true,
        },
      },
      {
        ...base,
        phase: "unavailable",
        searchable: false,
        issue: {
          code: "cache_unavailable",
          safeMessage: "Cache unavailable; building a fresh index…",
          recoverable: true,
        },
      },
    ];

    for (const status of statuses) {
      const line = formatStatus(status);
      expect(line).not.toContain("?");
      expect(line).not.toContain("In-plugin");
      expect(line).not.toContain("Lexical");
      expect(line).not.toContain("No cached index");
      expect(line).not.toContain("Cache unavailable; building");
    }
  });
});
