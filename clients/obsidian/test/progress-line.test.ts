// SPDX-FileCopyrightText: 2026 cybersader
// SPDX-License-Identifier: GPL-3.0-only

import { describe, expect, it } from "vitest";

import type { BackendStatus } from "../src/backend";
import { progressLine } from "../src/progress-line";

function status(progress?: BackendStatus["progress"]): BackendStatus {
  return {
    identity: {
      profile: "in_plugin",
      label: "In-plugin",
      instanceId: "i",
      boundVaultId: "vault",
    },
    phase: "building",
    liveness: "alive",
    searchable: false,
    generation: "g1",
    capabilities: {
      supportedModes: ["lexical"],
      sourceScope: "active_vault",
      manualRebuild: true,
    },
    documents: 0,
    chunks: 0,
    dirty: true,
    rebuilding: false,
    ...(progress ? { progress } : {}),
  };
}

describe("progressLine", () => {
  it("is hidden when nothing is in flight", () => {
    // A ready index must show no chrome at all, so absent progress is null
    // rather than an empty string that would still occupy a row.
    expect(progressLine(status())).toBeNull();
  });

  it("reports count, percentage, and the current file", () => {
    const line = progressLine(status({
      stage: "snapshot",
      completed: 250,
      total: 1000,
      path: "Projects/Alpha.md",
    }));

    expect(line).toBe("Indexing 250/1000 (25%) · Projects/Alpha");
  });

  it("shows a starting state until the total is known", () => {
    const line = progressLine(status({ stage: "snapshot", completed: 7, total: null }));

    expect(line).toBe("Starting index…");
    expect(line).not.toContain("?");
  });

  it("names reconciliation distinctly from a first build", () => {
    const line = progressLine(status({ stage: "replay", completed: 3, total: 4 }));

    expect(line).toBe("Reconciling 3/4 (75%)");
  });

  it("truncates a long path from the head so the filename stays visible", () => {
    const line = progressLine(status({
      stage: "snapshot",
      completed: 1,
      total: 2,
      path: `${"deeply/nested/".repeat(6)}Target.md`,
    }));

    expect(line).toContain("Target");
    expect(line).toContain("…");
    // One row only: the whole point is an unobtrusive line.
    expect(line!.length).toBeLessThan(80);
  });

  it("does not divide by zero on an empty vault", () => {
    const line = progressLine(status({ stage: "snapshot", completed: 0, total: 0 }));

    expect(line).toBe("Indexing 0/0 (0%)");
  });

  it("renders nothing for explicit zero omission counts", () => {
    expect(progressLine({
      ...status(),
      quarantinedSources: 0,
      unreadableSources: 0,
      quarantineValidatorFields: [],
    })).toBeNull();
  });

  it("keeps omissions visible after indexing finishes", () => {
    expect(progressLine({
      ...status(),
      phase: "degraded",
      searchable: true,
      dirty: false,
      quarantinedSources: 2,
      unreadableSources: 1,
      quarantineValidatorFields: ["chunks_contents"],
    })).toBe(
      "3 notes may be missing from search (2 quarantined, 1 unreadable)",
    );
  });

  it("keeps omissions visible while indexing is in flight", () => {
    expect(progressLine({
      ...status({ stage: "snapshot", completed: 7, total: 10 }),
      quarantinedSources: 1,
      unreadableSources: 0,
      quarantineValidatorFields: ["mtime_nanos"],
    })).toBe(
      "Indexing 7/10 (70%) · 1 note may be missing from search (1 quarantined)",
    );
  });
});
