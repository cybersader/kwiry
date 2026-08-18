// SPDX-FileCopyrightText: 2026 cybersader
// SPDX-License-Identifier: GPL-3.0-only

import { describe, expect, it } from "vitest";

import type { BackendStatus } from "../src/backend";
import { formatStatus } from "../src/status-format";
import { emptySourceFormatCounts } from "../src/worker/protocol";

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

  // The Obsidian status bar sits in a row of other plugins' items, so an item
  // that changes width shoves its neighbours sideways. This is the surface the
  // padding was missing from while the modal rail already had it.
  it("keeps the status bar item a stable width as the in-flight count grows", () => {
    const progress = (inFlight: number) => formatStatus({
      ...base,
      phase: "building",
      searchable: false,
      dirty: true,
      progress: {
        stage: "snapshot",
        activity: "read",
        completed: 42,
        total: 900,
        inFlight,
      },
    });

    expect(progress(9)).toContain("\u20079 in flight");
    expect(progress(10)).toContain(" 10 in flight");
    expect(progress(9).length).toBe(progress(10).length);
  });

  it("reports count and percentage with distinct aggregate activities", () => {
    const cases = [
      ["inventory", "Kwiry: Inventory 42/900 (4%)"],
      ["read", "Kwiry: Reading 42/900 (4%) ·  4 in flight"],
      ["prepare", "Kwiry: Preparing 42/900 (4%) ·  4 in flight"],
      ["apply", "Kwiry: Applying 42/900 (4%) ·  4 in flight"],
    ] as const;

    for (const [activity, expected] of cases) {
      expect(formatStatus({
        ...base,
        phase: "building",
        searchable: false,
        dirty: true,
        progress: {
          stage: "snapshot",
          activity,
          completed: 42,
          total: 900,
          inFlight: activity === "inventory" ? 0 : 4,
        },
      })).toBe(expected);
    }
  });

  it("reports reconciliation planning, verification, and application honestly", () => {
    const cases: Array<[NonNullable<BackendStatus["progress"]>, string]> = [
      [{ stage: "replay", activity: "apply", subphase: "planning", completed: 0, total: null, inFlight: 0 },
        "Kwiry: Planning reconciliation…"],
      [{ stage: "replay", activity: "apply", subphase: "verifying", completed: 42, total: 900, inFlight: 0 },
        "Kwiry: Verifying 42/900 (4%)"],
      [{ stage: "replay", activity: "apply", subphase: "applying", completed: 2, total: 5, inFlight: 0 },
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
      progress: { stage: "snapshot", activity: "inventory", completed: 0, total: null, inFlight: 0 },
    })).toBe("Kwiry: Inventorying sources…");
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
    })).toBe("Kwiry: Ready · 3 sources incomplete");
  });

  it("reports partial extraction separately from omitted sources", () => {
    const counts = emptySourceFormatCounts();
    counts.pdf["indexed-partial"] = 2;
    counts.base["indexed-partial"] = 1;

    expect(formatStatus({
      ...base,
      phase: "degraded",
      sourceFormatCounts: counts,
      quarantinedSources: 1,
      unreadableSources: 1,
    })).toBe("Kwiry: Ready · 3 sources partial · 2 sources incomplete");
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
        progress: { stage: "snapshot", activity: "inventory", completed: 0, total: null, inFlight: 0 },
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
