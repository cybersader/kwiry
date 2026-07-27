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
  it("renders controller-authored cache status messages verbatim", () => {
    expect(formatStatus({
      ...base,
      phase: "building",
      dirty: true,
      issue: {
        code: "index_reconciling",
        safeMessage: "Cached index searchable; reconciling vault changes…",
        recoverable: true,
      },
      progress: { stage: "replay", completed: 1, total: 2 },
      // The counter accompanies the issue rather than being suppressed by it:
      // during a long first build on a network vault, an issue-only line is
      // indistinguishable from a stall for the entire build.
    })).toBe(
      "kwiry: In-plugin · Lexical · Cached index searchable; reconciling vault changes… replay 1/2",
    );

    expect(formatStatus(base)).toBe("kwiry: In-plugin · Lexical · ready (3 chunks, lexical)");

    expect(formatStatus({
      ...base,
      phase: "building",
      searchable: false,
      generation: null,
      documents: 0,
      chunks: 0,
      dirty: true,
      issue: {
        code: "cache_unavailable",
        safeMessage: "Cache unavailable; building a fresh index…",
        recoverable: true,
      },
    })).toBe("kwiry: In-plugin · Lexical · Cache unavailable; building a fresh index…");

    expect(formatStatus({
      ...base,
      phase: "building",
      searchable: false,
      generation: null,
      documents: 0,
      chunks: 0,
      dirty: true,
      issue: {
        code: "cache_corrupt",
        safeMessage: "Cached index rejected and discarded; building fresh…",
        recoverable: true,
      },
    })).toBe("kwiry: In-plugin · Lexical · Cached index rejected and discarded; building fresh…");

    expect(formatStatus({
      ...base,
      phase: "degraded",
      issue: {
        code: "cache_save_failed",
        safeMessage: "Search ready; cache save failed",
        recoverable: true,
      },
    })).toBe("kwiry: In-plugin · Lexical · Search ready; cache save failed");
  });

  it("never renders a dirty ready-shaped status as ready", () => {
    expect(formatStatus({ ...base, dirty: true })).toBe(
      "kwiry: In-plugin · Lexical · building (3 chunks, lexical)",
    );
  });
});

describe("formatStatus during a first build", () => {
  it("shows the counter alongside a cache-absent issue", () => {
    // Regression: the issue branch used to return early, so a first build --
    // exactly when "no cached index" is expected and long-running -- showed a
    // static message with no evidence of progress until it finished.
    const line = formatStatus({
      ...base,
      phase: "building",
      searchable: false,
      dirty: true,
      issue: {
        code: "cache_absent",
        safeMessage: "No cached index; building a fresh index…",
        recoverable: true,
      },
      progress: { stage: "snapshot", completed: 42, total: 900 },
    });

    expect(line).toContain("No cached index");
    expect(line).toContain("snapshot 42/900");
  });

  it("shows the issue alone when no work is in flight", () => {
    const line = formatStatus({
      ...base,
      phase: "unavailable",
      searchable: false,
      issue: {
        code: "cache_unavailable",
        safeMessage: "Cache unavailable; building a fresh index…",
        recoverable: true,
      },
    });

    expect(line).toBe("kwiry: In-plugin · Lexical · Cache unavailable; building a fresh index…");
  });
});
