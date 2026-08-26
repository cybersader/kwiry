// SPDX-FileCopyrightText: 2026 cybersader
// SPDX-License-Identifier: GPL-3.0-only

import { describe, expect, it } from "vitest";

import { DiagnosticLog } from "../src/diagnostics/log";
import { PluginDiagnostics } from "../src/diagnostics/plugin-diagnostics";

const CONTEXT = {
  pluginVersion: "0.2.2",
  obsidianVersion: "1.8.10",
  platform: "linux" as const,
  backendProfile: "in_plugin" as const,
};
const UNAVAILABLE_SOURCE_GENERATION = {
  schemaVersion: 1,
  availability: "unavailable",
} as const;
const UNAVAILABLE_LEXICAL_EXECUTION = {
  schemaVersion: 1,
  availability: "unavailable",
} as const;

describe("PluginDiagnostics", () => {
  it("captures one completed wide event", async () => {
    const diagnostics = new PluginDiagnostics("info");

    await diagnostics.capture("info", "search.lifecycle", {
      profile: "in_plugin",
      operation: "search",
      mode: "lexical",
    }, (event) => {
      event.complete("info", {
        outcome: "succeeded",
        resultCount: 3,
        sourceGeneration: UNAVAILABLE_SOURCE_GENERATION,
        lexicalExecution: UNAVAILABLE_LEXICAL_EXECUTION,
      });
    });

    const report = diagnostics.format(CONTEXT);
    expect(report).toContain("stored_entries: 1");
    expect(report).toContain("search.lifecycle");
    expect(report).toContain("resultCount=3");
  });

  it("rethrows operation failures after committing the safe event", async () => {
    const diagnostics = new PluginDiagnostics("info");
    const failure = new Error("private transport detail");

    await expect(diagnostics.capture("info", "backend.activate", {
      profile: "daemon",
      operation: "activate",
    }, (event) => {
      event.set({ stage: "transport", code: "daemon_unreachable" });
      throw failure;
    })).rejects.toBe(failure);

    const report = diagnostics.format(CONTEXT);
    expect(report).toContain("ERROR backend.activate");
    expect(report).toContain("code=daemon_unreachable");
    expect(report).not.toContain("private transport detail");
  });

  it("records a bounded startup aggregate and isolates invalid diagnostics", () => {
    const diagnostics = new PluginDiagnostics(
      "info",
      () => new DiagnosticLog(4, () => 0, () => 0),
    );

    diagnostics.recordStartup({
      startedAtMs: 1_700_000_000_000,
      durationMs: 250,
      details: {
        profile: "in_plugin",
        outcome: "degraded",
        reason: "sources_omitted",
        pluginEpoch: 1,
        activationEpoch: 2,
        pluginLoadCompleteMs: 5,
        layoutReadyMs: 15,
        firstProgressMs: 20,
        firstCacheSearchableMs: 40,
        fullyCurrentMs: null,
        cacheHit: true,
        cacheBytes: 4_096,
      },
    });
    diagnostics.recordStartup({
      startedAtMs: 0,
      durationMs: 1,
      details: {
        profile: "in_plugin",
        // Deliberately bypass the type boundary to exercise safe runtime rejection.
        vaultPath: "smb://server/private-vault",
      } as never,
    });

    const report = diagnostics.format(CONTEXT);
    expect(report).toContain("stored_entries: 1");
    expect(report).toContain("startup.lifecycle");
    expect(report).toContain("firstCacheSearchableMs=40");
    expect(report).toContain("fullyCurrentMs=null");
    expect(report).not.toContain("private-vault");
  });

  it("records rejected annotations without changing the observed operation", async () => {
    const diagnostics = new PluginDiagnostics("info");
    const privateValue = "Clients/Private/Target.md";
    let completed = false;

    await diagnostics.capture("info", "search.lifecycle", {
      operation: "search",
    }, (event) => {
      event.set({ resultCount: 1 });
      event.complete("error", {
        outcome: "failed",
        code: privateValue,
      } as never);
      completed = true;
    });

    expect(completed).toBe(true);
    const report = diagnostics.format(CONTEXT);
    expect(report).toContain("ERROR search.lifecycle");
    expect(report).toContain("outcome=failed");
    expect(report).toContain("code=diagnostic_annotation_rejected");
    expect(report).toContain("annotationRejectedCount=1");
    expect(report).not.toContain(privateValue);
  });

  it("runs operations without retaining events when disabled", async () => {
    const diagnostics = new PluginDiagnostics("off");
    let ran = false;

    await diagnostics.capture("info", "plugin.load", {}, () => {
      ran = true;
    });

    expect(ran).toBe(true);
    expect(diagnostics.format(CONTEXT)).toContain("stored_entries: 0");
  });

  it("keeps the capture floor and report narrowing in the frozen plan", async () => {
    const diagnostics = new PluginDiagnostics("error");
    await diagnostics.capture("info", "search.lifecycle", {
      operation: "search",
      mode: "lexical",
    }, (event) => {
      event.complete("info", { outcome: "skipped" });
    });
    await expect(diagnostics.capture("info", "backend.activate", {
      operation: "activate",
    }, () => {
      throw new Error("private failure");
    })).rejects.toThrow("private failure");

    const plan = diagnostics.createExportPlan(CONTEXT, { minimumLevel: "debug" });
    expect(plan).toMatchObject({
      retainedEntries: 2,
      selectedEntries: 1,
      filteredOutEntries: 1,
      minimumLevel: "error",
    });
    expect(plan.entries.map((entry) => entry.code)).toEqual(["backend.activate"]);
  });

  it("returns a fixed unavailable plan when export planning fails", () => {
    const diagnostics = new PluginDiagnostics("info");
    const privateValue = "private/path/to/vault";
    const plan = diagnostics.createExportPlan({
      ...CONTEXT,
      pluginVersion: privateValue,
    });

    expect(plan.reportUnavailable).toBe(true);
    expect(plan).toMatchObject({ retainedEntries: 0, selectedEntries: 0, droppedEntries: 0 });
    const report = diagnostics.format({ ...CONTEXT, pluginVersion: privateValue });
    expect(report).toContain("report_unavailable: true");
    expect(report).not.toContain(privateValue);
    expect(JSON.stringify(plan)).not.toContain(privateValue);
  });

  it("excludes captures completed after export planning", async () => {
    const diagnostics = new PluginDiagnostics("info");
    await diagnostics.capture("info", "plugin.load", {}, () => undefined);
    const plan = diagnostics.createExportPlan(CONTEXT);
    await diagnostics.capture("error", "failure.caught", { code: "timeout" }, () => undefined);

    expect(plan.selectedEntries).toBe(1);
    expect(plan.entries.map((entry) => entry.code)).toEqual(["plugin.load"]);
    expect(diagnostics.createExportPlan(CONTEXT).selectedEntries).toBe(2);
  });

  it("keeps late operations from repopulating a cleared buffer", async () => {
    const diagnostics = new PluginDiagnostics("info");
    let release!: () => void;
    const gate = new Promise<void>((resolve) => {
      release = resolve;
    });
    const pending = diagnostics.capture("info", "index.lifecycle", {
      operation: "build",
    }, async () => gate);

    diagnostics.clear();
    release();
    await pending;

    expect(diagnostics.format(CONTEXT)).toContain("stored_entries: 0");
  });
});
