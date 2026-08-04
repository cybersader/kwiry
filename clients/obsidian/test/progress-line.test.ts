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

function progress(
  activity: NonNullable<BackendStatus["progress"]>["activity"],
  overrides: Partial<NonNullable<BackendStatus["progress"]>> = {},
): NonNullable<BackendStatus["progress"]> {
  return {
    stage: "snapshot",
    activity,
    completed: 250,
    total: 1_000,
    inFlight: 0,
    ...overrides,
  };
}

describe("progressLine", () => {
  it("is hidden when nothing is in flight", () => {
    expect(progressLine(status())).toBeNull();
  });

  it("renders each privacy-safe activity distinctly", () => {
    expect(progressLine(status(progress("inventory"))))
      .toBe("Inventory 250/1000 (25%)");
    expect(progressLine(status(progress("read", { inFlight: 4 }))))
      .toBe("Reading 250/1000 (25%) · 4 in flight");
    expect(progressLine(status(progress("prepare", { inFlight: 1 }))))
      .toBe("Preparing 250/1000 (25%) · 1 in flight");
    expect(progressLine(status(progress("apply"))))
      .toBe("Applying 250/1000 (25%)");
  });

  it("shows activity-specific copy until the total is known", () => {
    expect(progressLine(status(progress("inventory", { total: null }))))
      .toBe("Inventorying sources…");
    expect(progressLine(status(progress("read", { total: null, inFlight: 2 }))))
      .toBe("Reading sources… · 2 in flight");
    expect(progressLine(status(progress("prepare", { total: null }))))
      .toBe("Preparing index batches…");
    expect(progressLine(status(progress("apply", { total: null }))))
      .toBe("Applying index…");
  });

  it("names reconciliation application phases distinctly", () => {
    expect(progressLine(status(progress("apply", {
      stage: "replay",
      subphase: "planning",
      completed: 0,
      total: null,
    })))).toBe("Planning reconciliation…");
    expect(progressLine(status(progress("apply", {
      stage: "replay",
      subphase: "verifying",
      completed: 3,
      total: 4,
    })))).toBe("Verifying 3/4 (75%)");
    expect(progressLine(status(progress("apply", {
      stage: "replay",
      subphase: "applying",
      completed: 1,
      total: 2,
    })))).toBe("Applying changes 1/2 (50%)");
  });

  it("uses only the closed stall vocabulary and aggregate counts", () => {
    const privatePath = "Clients/Private/Target.md";
    const lines = [
      progressLine(status(progress("read", { stallCategory: "source_read_timeout" }))),
      progressLine(status(progress("read", { stallCategory: "source_read_capacity" }))),
      progressLine(status(progress("apply", { stallCategory: "worker_timeout" }))),
    ];

    expect(lines).toEqual([
      "Reading 250/1000 (25%) · source reads timed out",
      "Reading 250/1000 (25%) · source read capacity reached",
      "Applying 250/1000 (25%) · index worker timed out",
    ]);
    expect(JSON.stringify(lines)).not.toContain(privatePath);
  });

  it("does not divide by zero or render unknown totals as question marks", () => {
    expect(progressLine(status(progress("inventory", { completed: 0, total: 0 }))))
      .toBe("Inventory 0/0 (0%)");
    expect(progressLine(status(progress("read", { total: null })))).not.toContain("?");
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
      ...status(progress("read", { completed: 7, total: 10, inFlight: 2 })),
      quarantinedSources: 1,
      unreadableSources: 0,
      quarantineValidatorFields: ["mtime_nanos"],
    })).toBe(
      "Reading 7/10 (70%) · 2 in flight · 1 note may be missing from search (1 quarantined)",
    );
  });
});
