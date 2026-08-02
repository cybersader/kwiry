// SPDX-FileCopyrightText: 2026 cybersader
// SPDX-License-Identifier: GPL-3.0-only

import { describe, expect, it, vi } from "vitest";

import {
  StartupTimeline,
  type StartupTimelineRecord,
} from "../src/diagnostics/startup-timeline";

function clock(...timestamps: number[]): () => number {
  let index = 0;
  return () => timestamps[index++] ?? timestamps.at(-1) ?? 0;
}

describe("StartupTimeline", () => {
  it("records one cache-hit startup with monotonic integer milestones", () => {
    const records: StartupTimelineRecord[] = [];
    const timeline = new StartupTimeline({
      profile: "daemon",
      pluginEpoch: 3,
      wallNow: () => 1_700_000_000_000,
      monotonicNow: clock(100, 110.2, 120.4, 150.1, 200.3, 201.1),
      record: (record) => records.push(record),
    });

    timeline.setProfile("in_plugin");
    timeline.beginActivation("in_plugin", 7);
    timeline.markPluginLoadComplete();
    timeline.markPluginLoadComplete();
    timeline.markLayoutReady();
    timeline.markCacheSearchable(4_096);
    timeline.markCacheSearchable(8_192);
    timeline.markFullyCurrent();
    timeline.finish("failed", "backend_unavailable");

    expect(records).toEqual([{
      startedAtMs: 1_700_000_000_000,
      durationMs: 101,
      details: {
        profile: "in_plugin",
        outcome: "succeeded",
        reason: "fully_current",
        pluginEpoch: 3,
        activationEpoch: 7,
        pluginLoadCompleteMs: 10,
        layoutReadyMs: 20,
        firstCacheSearchableMs: 50,
        fullyCurrentMs: 100,
        cacheHit: true,
        cacheBytes: 4_096,
      },
    }]);
  });

  it.each([
    ["degraded", "sources_omitted"],
    ["failed", "activation_failed"],
    ["cancelled", "plugin_unloaded"],
  ] as const)("records a cache-miss %s terminal outcome", (outcome, reason) => {
    const record = vi.fn();
    const timeline = new StartupTimeline({
      profile: "in_plugin",
      pluginEpoch: 1,
      activationEpoch: 2,
      wallNow: () => 500,
      monotonicNow: clock(10, 25),
      record,
    });

    timeline.finish(outcome, reason);
    timeline.finish("failed", "backend_unavailable");

    expect(record).toHaveBeenCalledTimes(1);
    expect(record).toHaveBeenCalledWith({
      startedAtMs: 500,
      durationMs: 15,
      details: {
        profile: "in_plugin",
        outcome,
        reason,
        pluginEpoch: 1,
        activationEpoch: 2,
        pluginLoadCompleteMs: null,
        layoutReadyMs: null,
        firstCacheSearchableMs: null,
        fullyCurrentMs: null,
        cacheHit: false,
      },
    });
  });

  it("never lets a backward or broken monotonic clock reduce elapsed time", () => {
    const records: StartupTimelineRecord[] = [];
    const timeline = new StartupTimeline({
      profile: "in_plugin",
      pluginEpoch: 1,
      wallNow: () => 100,
      monotonicNow: clock(50, 80, 60, Number.NaN, 90),
      record: (record) => records.push(record),
    });

    timeline.markPluginLoadComplete();
    timeline.markLayoutReady();
    timeline.markCacheSearchable(1);
    timeline.finish("degraded", "vault_unavailable");

    expect(records[0]).toMatchObject({
      durationMs: 40,
      details: {
        pluginLoadCompleteMs: 30,
        layoutReadyMs: 30,
        firstCacheSearchableMs: 30,
      },
    });
  });

  it("swallows recorder failures", () => {
    const timeline = new StartupTimeline({
      profile: "in_plugin",
      pluginEpoch: 1,
      wallNow: () => 100,
      monotonicNow: clock(0, 1),
      record: () => {
        throw new Error("diagnostic sink failed");
      },
    });

    expect(() => timeline.finish("failed", "backend_unavailable")).not.toThrow();
  });
});
