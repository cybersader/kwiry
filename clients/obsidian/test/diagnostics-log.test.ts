// SPDX-FileCopyrightText: 2026 cybersader
// SPDX-License-Identifier: GPL-3.0-only

import { describe, expect, it } from "vitest";

import {
  DiagnosticLog,
  diagnosticGenerationId,
  diagnosticHash,
  formatDiagnosticLog,
  type DiagnosticDetails,
  type DiagnosticEventCode,
  type DiagnosticLevel,
} from "../src/diagnostics/log";

function clock(...timestamps: number[]): () => number {
  let index = 0;
  return () => timestamps[index++] ?? timestamps.at(-1) ?? 0;
}

async function capture(
  log: DiagnosticLog,
  level: DiagnosticLevel,
  code: DiagnosticEventCode,
  details: Readonly<DiagnosticDetails> = {},
): Promise<void> {
  await log.capture(level, code, details, () => undefined);
}

describe("DiagnosticLog", () => {
  it("evicts the oldest wide events and reports every dropped entry", async () => {
    const log = new DiagnosticLog(
      2,
      clock(1, 2, 3, 4),
      clock(10, 11, 20, 21, 30, 31, 40, 41),
    );

    await capture(log, "debug", "plugin.load");
    await capture(log, "info", "backend.activate", { outcome: "started" });
    await capture(log, "warn", "failure.caught", { code: "worker_failed" });
    await capture(log, "error", "promise.rejected", { subsystem: "worker" });

    const snapshot = log.snapshot();
    expect(snapshot.entries.map((entry) => entry.sequence)).toEqual([3, 4]);
    expect(snapshot.dropped).toBe(2);
    expect(snapshot.capacity).toBe(2);
  });

  it("keeps insertion order after the ring wraps", async () => {
    const log = new DiagnosticLog(
      3,
      clock(10, 20, 30, 40, 50),
      clock(0, 1, 10, 11, 20, 21, 30, 31, 40, 41),
    );
    for (let count = 1; count <= 5; count += 1) {
      await capture(log, "info", "index.lifecycle", { count });
    }

    expect(log.snapshot().entries.map((entry) => ({
      sequence: entry.sequence,
      count: entry.details.count,
    }))).toEqual([
      { sequence: 3, count: 3 },
      { sequence: 4, count: 4 },
      { sequence: 5, count: 5 },
    ]);
  });

  it("filters by minimum level without hiding the truncation count", async () => {
    const log = new DiagnosticLog(
      3,
      clock(1, 2, 3, 4),
      clock(10, 11, 20, 21, 30, 31, 40, 41),
    );
    await capture(log, "debug", "plugin.load");
    await capture(log, "info", "backend.activate");
    await capture(log, "warn", "failure.caught");
    await capture(log, "error", "promise.rejected");

    const snapshot = log.snapshot("warn");
    expect(snapshot.entries.map((entry) => entry.level)).toEqual(["warn", "error"]);
    expect(snapshot.dropped).toBe(1);
  });

  it("commits one context-rich event on success and on failure", async () => {
    const log = new DiagnosticLog(
      4,
      clock(100, 200),
      clock(0, 45, 100, 180),
    );
    await log.capture("info", "index.lifecycle", {
      operation: "build",
      generationId: diagnosticGenerationId("in_plugin-1-generation-1"),
      sourcesEnumerated: 12,
    }, (event) => {
      event.increment("sourcesRead", 9);
      event.increment("sourcesSkipped", 2);
      event.increment("sourcesFailed");
      event.increment("bytesRead", 4_096);
      event.increment("batchCount", 3);
    });

    const failure = new Error("raw failure text that must not enter diagnostics");
    await expect(log.capture("info", "cache.lifecycle", {
      operation: "restore",
      cacheBytes: 1_024,
    }, (event) => {
      event.set({ code: "cache_corrupt" });
      throw failure;
    })).rejects.toBe(failure);

    const [success, failed] = log.snapshot().entries;
    expect(success).toMatchObject({ durationMs: 45, level: "info" });
    expect(success?.details).toMatchObject({
      outcome: "succeeded",
      sourcesEnumerated: 12,
      sourcesRead: 9,
      sourcesSkipped: 2,
      sourcesFailed: 1,
      bytesRead: 4_096,
      batchCount: 3,
    });
    expect(failed).toMatchObject({ durationMs: 80, level: "error" });
    expect(failed?.details).toMatchObject({
      outcome: "failed",
      operation: "restore",
      cacheBytes: 1_024,
      code: "cache_corrupt",
    });
    expect(log.snapshot().entries).toHaveLength(2);
    expect(JSON.stringify(failed)).not.toContain(failure.message);
  });

  it("uses wall time only for display metadata and monotonic time for duration", async () => {
    const log = new DiagnosticLog(
      2,
      clock(1_700_000_000_000),
      clock(500, 575),
    );

    await capture(log, "info", "plugin.load");

    expect(log.snapshot().entries[0]).toMatchObject({
      startedAtMs: 1_700_000_000_000,
      durationMs: 75,
    });
  });

  it("records the fixed-schema startup aggregate without arbitrary text", () => {
    const log = new DiagnosticLog(2);
    log.record("info", "startup.lifecycle", 1_700_000_000_000, 321, {
      profile: "in_plugin",
      outcome: "succeeded",
      reason: "fully_current",
      pluginEpoch: 2,
      activationEpoch: 4,
      pluginLoadCompleteMs: 10,
      layoutReadyMs: 20,
      firstProgressMs: 30,
      firstCacheSearchableMs: 100,
      fullyCurrentMs: 320,
      cacheHit: true,
      cacheBytes: 8_192,
    });

    expect(log.snapshot().entries[0]).toMatchObject({
      startedAtMs: 1_700_000_000_000,
      durationMs: 321,
      code: "startup.lifecycle",
      details: {
        outcome: "succeeded",
        firstProgressMs: 30,
        firstCacheSearchableMs: 100,
        fullyCurrentMs: 320,
        cacheHit: true,
        cacheBytes: 8_192,
      },
    });
    expect(() => log.record("info", "startup.lifecycle", 0, 1, {
      // @ts-expect-error Startup diagnostics cannot carry vault paths.
      vaultPath: "smb://server/private-vault",
    })).toThrow("Invalid diagnostic details");
    expect(() => log.record("info", "startup.lifecycle", 0, 1, {
      profile: "in_plugin",
      outcome: "failed",
      reason: "activation_failed",
      pluginEpoch: 1,
      activationEpoch: 1,
      pluginLoadCompleteMs: null,
      layoutReadyMs: null,
      firstProgressMs: null,
      firstCacheSearchableMs: null,
      fullyCurrentMs: null,
      cacheHit: false,
      pathHash: diagnosticHash(`sha256:${"b".repeat(64)}`),
    })).toThrow("Invalid startup diagnostic details");
  });

  it("records only closed aggregate progress and stall details", async () => {
    const log = new DiagnosticLog(4, clock(0), clock(0, 1));
    await capture(log, "warn", "index.lifecycle", {
      phase: "building",
      stage: "snapshot",
      activity: "read",
      completed: 32,
      total: 100,
      inFlight: 4,
      stallCategory: "source_read_timeout",
    });

    const serialized = JSON.stringify(log.snapshot());
    expect(log.snapshot().entries[0]?.details).toMatchObject({
      activity: "read",
      completed: 32,
      total: 100,
      inFlight: 4,
      stallCategory: "source_read_timeout",
    });
    expect(serialized).not.toContain("Clients/Private/Target.md");
    await expect(log.capture("warn", "index.lifecycle", {
      // @ts-expect-error Source paths are not diagnostic progress fields.
      path: "Clients/Private/Target.md",
    }, () => undefined)).rejects.toThrow("Invalid diagnostic details");
    await expect(log.capture("warn", "index.lifecycle", {
      activity: "daemon",
    } as never, () => undefined)).rejects.toThrow("Invalid diagnostic details");
    await expect(log.capture("warn", "index.lifecycle", {
      stallCategory: "timeout",
    } as never, () => undefined)).rejects.toThrow("Invalid diagnostic details");
  });

  it("renders both a pasteable summary and the structured JSON records", async () => {
    const log = new DiagnosticLog(4, clock(0), clock(0, 1_000));
    await log.capture("error", "failure.caught", {
      profile: "in_plugin",
      subsystem: "cache_store",
      code: "cache_unavailable",
      pathHash: diagnosticHash(`sha256:${"a".repeat(64)}`),
    }, () => undefined);

    const output = formatDiagnosticLog(log, {
      pluginVersion: "0.2.2",
      obsidianVersion: "1.8.10",
      platform: "linux",
      backendProfile: "in_plugin",
    });

    expect(output).toContain("Kwiry diagnostics log\nplugin_version: 0.2.2");
    expect(output).toContain("dropped_entries: 0\nminimum_level: debug\ncategories: all\n\nSummary:");
    expect(output).toContain(
      "1 1970-01-01T00:00:00.000Z +1000ms ERROR failure.caught profile=in_plugin outcome=succeeded",
    );
    const jsonText = output.split("Structured records (JSON):\n")[1];
    expect(jsonText).toBeDefined();
    const structured = JSON.parse(jsonText!);
    expect(structured).toMatchObject({
      schemaVersion: 1,
      context: {
        pluginVersion: "0.2.2",
        obsidianVersion: "1.8.10",
        platform: "linux",
        backendProfile: "in_plugin",
      },
      capacity: 4,
      storedEntries: 1,
      droppedEntries: 0,
      minimumLevel: "debug",
    });
    expect(structured.records).toHaveLength(1);
    expect(structured.records[0]).toMatchObject({
      sequence: 1,
      startedAtMs: 0,
      durationMs: 1_000,
      code: "failure.caught",
    });
  });

  it("rejects unstructured text at both the type and runtime boundaries", async () => {
    type CaptureDetails = Parameters<DiagnosticLog["capture"]>[2];
    const unsafeDetails: CaptureDetails = {
      // @ts-expect-error Free-form text is not a diagnostic detail value.
      code: "the search query or note body",
    };
    void unsafeDetails;

    const log = new DiagnosticLog(4, clock(0), clock(0, 1));
    const structurallySmuggled = {
      profile: "in_plugin" as const,
      queryText: "confidential acquisition notes",
    };
    await expect(log.capture("info", "search.lifecycle", {}, (event) => {
      event.set(structurallySmuggled);
    })).rejects.toThrow("Invalid diagnostic details");

    const [entry] = log.snapshot().entries;
    expect(entry?.details).toMatchObject({ outcome: "failed", code: "internal_error" });
    expect(JSON.stringify(entry)).not.toContain(structurallySmuggled.queryText);
  });

  it("rejects arbitrary text carried by an allow-listed field", async () => {
    // The unknown-key case above is the easy half. The dangerous half is
    // secret text smuggled through a field that IS on the allow list: a
    // caller reaching for `code` or `reason` to describe a failure is the
    // most natural way this leaks in practice. Covering it only with a
    // compile-time @ts-expect-error leaves the runtime free to accept any
    // string, which a single cast through `any` then exploits.
    const secret = "Clients/Acme — Q3 layoffs, confidential";
    for (const field of ["code", "reason", "outcome", "phase", "stage", "operation", "subsystem"]) {
      const log = new DiagnosticLog(4, clock(0), clock(0, 1));
      await expect(
        log.capture("info", "search.lifecycle", { [field]: secret } as never, () => undefined),
      ).rejects.toThrow("Invalid diagnostic details");
      expect(JSON.stringify(log.snapshot())).not.toContain(secret);
    }
  });
});
