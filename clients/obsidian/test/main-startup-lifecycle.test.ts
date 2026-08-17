// SPDX-FileCopyrightText: 2026 cybersader
// SPDX-License-Identifier: GPL-3.0-only

import { createRequire } from "node:module";
import { fileURLToPath } from "node:url";

import { build, type Plugin as EsbuildPlugin } from "esbuild";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

interface LifecycleHarness {
  layoutReady: (() => void) | null;
  startupObserver: ((observation: unknown) => void) | null;
  statusListener: ((status: unknown) => void) | null;
  backendInitializations: number;
  backendDisposals: number;
  cachePolicyHashes: string[];
  sourcePolicies: Array<Record<string, boolean>>;
  clipboardWrites: string[];
  exportReports: string[];
  exportVaultRoots: string[];
  exportChunkIterations: number;
  exportResult: "saved" | "cancelled" | "inside_vault" | "unsafe_destination" | "unavailable" | "write_failed";
  notices: string[];
  savedData: unknown[];
  storedData: Record<string, unknown>;
  rebuildResult: "scheduled" | "already_building";
}

const harness: LifecycleHarness = {
  layoutReady: null,
  startupObserver: null,
  statusListener: null,
  backendInitializations: 0,
  backendDisposals: 0,
  cachePolicyHashes: [],
  sourcePolicies: [],
  clipboardWrites: [],
  exportReports: [],
  exportVaultRoots: [],
  exportChunkIterations: 0,
  exportResult: "saved",
  notices: [],
  savedData: [],
  storedData: {},
  rebuildResult: "scheduled",
};

const MAIN_PATH = fileURLToPath(new URL("../src/main.ts", import.meta.url));
const require = createRequire(import.meta.url);

async function loadProductionPlugin(): Promise<new () => {
  settings: {
    enabledSourceFormats: Record<string, boolean>;
    diagnosticsReportLevel: "debug" | "info" | "warn" | "error";
    diagnosticsReportScope: "all" | "indexing" | "search" | "startup" | "failures";
  };
  onload(): Promise<void>;
  saveSettings(): Promise<void>;
  onSourcePolicyChanged(): Promise<void>;
  rebuildInPluginIndex(): Promise<void>;
  copyDiagnostics(): Promise<void>;
  exportDiagnosticsFile(): Promise<void>;
  captureDiagnostic(
    level: "debug" | "info" | "warn" | "error",
    code: string,
    details: Record<string, unknown>,
    operation: () => unknown,
  ): Promise<unknown>;
}> {
  const bundle = await build({
    entryPoints: [MAIN_PATH],
    bundle: true,
    format: "cjs",
    platform: "node",
    target: "node22",
    write: false,
    logLevel: "silent",
    plugins: [lifecycleHarnessPlugin()],
  });
  const output = bundle.outputFiles[0];
  if (!output) throw new Error("main.ts test bundle emitted no output");

  const module = { exports: {} as Record<string, unknown> };
  const evaluate = new Function("module", "exports", "require", output.text);
  evaluate(module, module.exports, require);
  const plugin = module.exports.default;
  if (typeof plugin !== "function") throw new Error("main.ts test bundle has no default plugin");
  return plugin as Awaited<ReturnType<typeof loadProductionPlugin>>;
}

function lifecycleHarnessPlugin(): EsbuildPlugin {
  const stubs = new Map<string, string>([
    ["obsidian", "obsidian"],
    ["virtual:kwiry-worker-source", "worker-source"],
    ["./active-vault-source", "active-vault-source"],
    ["./backends/daemon-backend", "daemon-backend"],
    ["./backends/in-plugin-lexical-backend", "in-plugin-backend"],
    ["./cache/build-cache-options", "cache-options"],
    ["./credentials", "credentials"],
    ["./diagnostics/desktop-export-host", "desktop-export-host"],
    ["./internal/private-tools", "private-tools"],
    ["./search-modal", "search-modal"],
    ["./settings-tab", "settings-tab"],
  ]);

  return {
    name: "kwiry-startup-lifecycle-harness",
    setup(pluginBuild) {
      pluginBuild.onResolve({ filter: /.*/ }, (args) => {
        const stub = stubs.get(args.path);
        return stub === undefined
          ? undefined
          : { path: stub, namespace: "kwiry-startup-lifecycle-harness" };
      });
      pluginBuild.onLoad(
        { filter: /.*/, namespace: "kwiry-startup-lifecycle-harness" },
        (args) => ({ contents: stubSource(args.path), loader: "js" }),
      );
    },
  };
}

function stubSource(path: string): string {
  switch (path) {
    case "obsidian":
      return `
        const harness = globalThis.__kwiryStartupLifecycleHarness;
        export class Plugin {
          app = {
            vault: {
              adapter: { getBasePath() { return "/synthetic-vault"; } },
            },
            workspace: {
              onLayoutReady(callback) { harness.layoutReady = callback; },
            },
          };
          manifest = { version: "0.5.2" };
          async loadData() {
            return {
              backendProfile: "in_plugin",
              diagnosticsLogLevel: "info",
              showRibbonIcon: false,
              ...harness.storedData,
            };
          }
          async saveData(value) { harness.savedData.push(value); }
          addSettingTab() {}
          addCommand() {}
          addRibbonIcon() {}
          addStatusBarItem() { return { setText() {} }; }
          registerInterval() {}
        }
        export class Notice {
          constructor(message) { harness.notices.push(message); }
        }
        export const Platform = { isAndroidApp: false, isIosApp: false };
        export const apiVersion = "1.8.10";
        export async function requestUrl() { throw new Error("unexpected request"); }
      `;
    case "worker-source":
      return `export default "worker source";`;
    case "active-vault-source":
      return `
        const harness = globalThis.__kwiryStartupLifecycleHarness;
        export const ACTIVE_VAULT_ID = "active-vault";
        export class ObsidianActiveVaultSource {
          constructor(_vault, enabledFormats) {
            harness.sourcePolicies.push({ ...enabledFormats });
          }
        }
      `;
    case "daemon-backend":
      return `export class DaemonBackend {}`;
    case "in-plugin-backend":
      return `
        const harness = globalThis.__kwiryStartupLifecycleHarness;
        export class InPluginLexicalBackend {
          constructor(options) {
            this.identity = { profile: "in_plugin", instanceId: options.instanceId };
            this.statusValue = {
              identity: this.identity,
              capabilities: {
                supportedModes: ["lexical"],
                sourceScope: "active_vault",
                manualRebuild: true,
              },
              phase: "ready",
              liveness: "alive",
              searchable: true,
              dirty: false,
              rebuilding: false,
              documents: 1,
              chunks: 1,
            };
            harness.startupObserver = options.onStartupObservation ?? null;
          }
          async initialize() {
            harness.backendInitializations += 1;
            harness.statusListener?.({
              ...this.statusValue,
              phase: "building",
              searchable: false,
              dirty: true,
              progress: {
                stage: "snapshot",
                activity: "inventory",
                completed: 0,
                total: 0,
                inFlight: 0,
              },
            });
            harness.startupObserver?.({ kind: "first_progress" });
            harness.startupObserver?.({ kind: "cache_searchable", cacheBytes: 4096 });
            harness.startupObserver?.({ kind: "fully_current" });
            harness.statusListener?.(this.statusValue);
          }
          async status() { return this.statusValue; }
          subscribeStatus(listener) {
            harness.statusListener = listener;
            listener(this.statusValue);
            return () => {
              if (harness.statusListener === listener) harness.statusListener = null;
            };
          }
          async rebuild() { return harness.rebuildResult; }
          async dispose() { harness.backendDisposals += 1; }
        }
      `;
    case "cache-options":
      return `
        const harness = globalThis.__kwiryStartupLifecycleHarness;
        export function createInPluginCacheOptions(_vault, sourcePolicyHash) {
          harness.cachePolicyHashes.push(sourcePolicyHash);
          return undefined;
        }
      `;
    case "credentials":
      return `export async function readDaemonToken() { throw new Error("unexpected token read"); }`;
    case "desktop-export-host":
      return `
        const harness = globalThis.__kwiryStartupLifecycleHarness;
        export function createProductionDesktopDiagnosticsExportHost() {
          return {
            async save({ vaultRoot, chunks }) {
              harness.exportVaultRoots.push(vaultRoot);
              if (harness.exportResult === "saved") {
                const decoder = new TextDecoder();
                const parts = [];
                for await (const chunk of chunks) {
                  harness.exportChunkIterations += 1;
                  parts.push(decoder.decode(chunk, { stream: true }));
                }
                parts.push(decoder.decode());
                harness.exportReports.push(parts.join(""));
              }
              return { kind: harness.exportResult };
            },
          };
        }
      `;
    case "private-tools":
      return `
        export function createPrivateTools() {
          return {
            register() {},
            dispose() {},
            prepareStoredData(settings) { return settings; },
            renderSettings() {},
          };
        }
      `;
    case "search-modal":
      return `export class KwirySearchModal {}`;
    case "settings-tab":
      return `export class KwirySettingTab {}`;
    default:
      throw new Error(`missing lifecycle test stub: ${path}`);
  }
}

describe("KwiryPlugin startup lifecycle wiring", () => {
  beforeEach(() => {
    harness.layoutReady = null;
    harness.startupObserver = null;
    harness.statusListener = null;
    harness.backendInitializations = 0;
    harness.backendDisposals = 0;
    harness.cachePolicyHashes.length = 0;
    harness.sourcePolicies.length = 0;
    harness.clipboardWrites.length = 0;
    harness.exportReports.length = 0;
    harness.exportVaultRoots.length = 0;
    harness.exportChunkIterations = 0;
    harness.exportResult = "saved";
    harness.notices.length = 0;
    harness.savedData.length = 0;
    harness.storedData = {};
    harness.rebuildResult = "scheduled";
    vi.stubGlobal("__kwiryStartupLifecycleHarness", harness);
    vi.stubGlobal("window", { setInterval: () => 1 });
    vi.stubGlobal("navigator", {
      clipboard: {
        writeText: async (value: string) => {
          harness.clipboardWrites.push(value);
        },
      },
    });
  });

  afterEach(() => {
    vi.unstubAllGlobals();
  });

  it("records production load, layout-ready, and backend startup milestones exactly once", async () => {
    const KwiryPlugin = await loadProductionPlugin();
    const plugin = new KwiryPlugin();

    await plugin.onload();

    expect(Reflect.get(plugin, "startupTimeline")).toEqual(expect.any(Object));
    expect(harness.layoutReady).toEqual(expect.any(Function));
    expect(harness.backendInitializations).toBe(0);

    harness.layoutReady?.();
    await vi.waitFor(() => expect(harness.backendInitializations).toBe(1));

    expect(harness.startupObserver).toEqual(expect.any(Function));
    harness.startupObserver?.({ kind: "fully_current" });
    harness.startupObserver?.({
      kind: "terminal",
      outcome: "failed",
      reason: "backend_unavailable",
    });

    await plugin.copyDiagnostics();
    const report = harness.clipboardWrites.at(-1) ?? "";
    const summary = report.split("\nStructured records (JSON):", 1)[0] ?? "";
    const startupRecords = summary
      .split("\n")
      .filter((line) => line.includes("startup.lifecycle"));

    expect(startupRecords).toHaveLength(1);
    expect(startupRecords[0]).toContain("profile=in_plugin");
    expect(startupRecords[0]).toContain("outcome=succeeded");
    expect(startupRecords[0]).toContain("reason=fully_current");
    expect(startupRecords[0]).toMatch(/pluginLoadCompleteMs=\d+/u);
    expect(startupRecords[0]).toMatch(/layoutReadyMs=\d+/u);
    expect(startupRecords[0]).toMatch(/firstProgressMs=\d+/u);
    expect(startupRecords[0]).toMatch(/firstCacheSearchableMs=\d+/u);
    expect(startupRecords[0]).toMatch(/fullyCurrentMs=\d+/u);
    expect(startupRecords[0]).toContain("cacheHit=true");
    expect(startupRecords[0]).toContain("cacheBytes=4096");
  });

  it("reports scheduled and already-building manual rebuild outcomes distinctly", async () => {
    const KwiryPlugin = await loadProductionPlugin();
    const plugin = new KwiryPlugin();

    await plugin.onload();
    harness.layoutReady?.();
    await vi.waitFor(() => expect(harness.backendInitializations).toBe(1));

    harness.rebuildResult = "scheduled";
    await plugin.rebuildInPluginIndex();
    expect(harness.notices.at(-1)).toBe("Kwiry: in-plugin lexical rebuild started.");

    harness.rebuildResult = "already_building";
    await plugin.rebuildInPluginIndex();
    expect(harness.notices.at(-1)).toBe(
      "Kwiry: the in-plugin lexical index is already building.",
    );

    await plugin.copyDiagnostics();
    const report = harness.clipboardWrites.at(-1) ?? "";
    expect(report).toContain("outcome=scheduled");
    expect(report).toContain("outcome=already_building");
  });

  it("deduplicates aggregate progress milestones without retaining private text", async () => {
    const KwiryPlugin = await loadProductionPlugin();
    const plugin = new KwiryPlugin();

    await plugin.onload();
    harness.layoutReady?.();
    await vi.waitFor(() => expect(harness.backendInitializations).toBe(1));
    const listener = harness.statusListener;
    expect(listener).toEqual(expect.any(Function));
    const privateText = "Clients/Private/Quarterly Plan.md";
    for (let completed = 0; completed <= 1_000; completed += 1) {
      listener?.({
        identity: {
          profile: "in_plugin",
          instanceId: "in_plugin-1",
          label: "In-plugin",
          boundVaultId: "active-vault",
        },
        capabilities: {
          supportedModes: ["lexical"],
          sourceScope: "active_vault",
          manualRebuild: true,
        },
        phase: "building",
        liveness: "alive",
        searchable: false,
        generation: null,
        dirty: true,
        rebuilding: false,
        documents: 0,
        chunks: 0,
        progress: {
          stage: "snapshot",
          activity: "read",
          completed,
          total: 1_000,
          inFlight: completed % 2 === 0 ? 4 : 0,
        },
        issue: {
          code: "cache_absent",
          safeMessage: privateText,
          recoverable: true,
        },
      });
    }
    await Promise.resolve();
    await Promise.resolve();

    await plugin.exportDiagnosticsFile();
    const report = harness.exportReports.at(-1) ?? "";
    const jsonText = report.split("Structured records (JSON):\n")[1];
    expect(jsonText).toBeDefined();
    expect(harness.exportVaultRoots).toEqual(["/synthetic-vault"]);

    await plugin.copyDiagnostics();
    const compact = harness.clipboardWrites.at(-1) ?? "";
    expect(compact).toContain("structured_records: omitted");
    expect(compact).not.toContain("Structured records (JSON)");
    expect(new TextEncoder().encode(compact).byteLength).toBeLessThanOrEqual(65_536);
    expect(compact.length).toBeLessThan(report.length);
    const structured = JSON.parse(jsonText!);
    const progressRecords = structured.records.filter((record: {
      code: string;
      details: { operation?: string; activity?: string };
    }) => record.code === "index.lifecycle"
      && record.details.operation === "status"
      && record.details.activity === "read");

    expect(progressRecords.length).toBeGreaterThanOrEqual(20);
    expect(progressRecords.length).toBeLessThanOrEqual(21);
    expect(report).not.toContain(privateText);
    expect(progressRecords.every((record: { details: Record<string, unknown> }) =>
      !("path" in record.details))).toBe(true);
  });

  it("never copies more than 64 KiB and reports retained events omitted by the clipboard cap", async () => {
    const KwiryPlugin = await loadProductionPlugin();
    const plugin = new KwiryPlugin();
    await plugin.onload();

    for (let index = 0; index < 512; index += 1) {
      await plugin.captureDiagnostic("error", "failure.caught", {
        profile: "in_plugin",
        phase: "building",
        stage: "snapshot",
        activity: "read",
        stallCategory: "worker_timeout",
        liveness: "alive",
        mode: "lexical",
        outcome: "failed",
        code: "worker_timeout",
        errorName: "A".repeat(64),
        operation: "build",
        subsystem: "worker",
        generationId: "in_plugin-1-generation-1",
        pathHash: `sha256:${"a".repeat(64)}`,
        completed: index,
        total: 512,
        inFlight: 4,
        bytesRead: index * 1_024,
        retryable: false,
        recoverable: true,
        searchable: true,
        dirty: false,
        rebuilding: false,
        cacheHit: true,
        recovery: false,
      }, () => undefined);
    }

    await plugin.copyDiagnostics();
    const summary = harness.clipboardWrites.at(-1) ?? "";
    expect(new TextEncoder().encode(summary).byteLength).toBeLessThanOrEqual(65_536);
    const omitted = /clipboard_omitted_entries: (\d+)/u.exec(summary);
    expect(omitted).not.toBeNull();
    expect(Number(omitted?.[1])).toBeGreaterThan(0);
    expect(summary).not.toContain("Structured records (JSON)");
    expect(harness.notices.at(-1)).toMatch(/retained events omitted/u);
  });

  it("keeps cancellation silent and all export notices path-free", async () => {
    const KwiryPlugin = await loadProductionPlugin();
    const plugin = new KwiryPlugin();
    await plugin.onload();

    harness.exportResult = "cancelled";
    const beforeCancellation = harness.notices.length;
    await plugin.exportDiagnosticsFile();
    expect(harness.notices).toHaveLength(beforeCancellation);
    expect(harness.exportChunkIterations).toBe(0);
    expect(harness.exportReports).toEqual([]);

    harness.exportResult = "inside_vault";
    await plugin.exportDiagnosticsFile();
    expect(harness.notices.at(-1)).toBe(
      "Kwiry: choose a diagnostics export location outside the active vault.",
    );
    expect(harness.notices.join("\n")).not.toContain("/synthetic-vault");
    expect(harness.exportChunkIterations).toBe(0);

    harness.exportResult = "saved";
    await plugin.exportDiagnosticsFile();
    expect(harness.exportReports).toHaveLength(1);
    expect(harness.exportChunkIterations).toBeGreaterThan(0);
    expect(harness.notices.at(-1)).toBe("Kwiry: full diagnostics report exported.");
    expect(harness.notices.join("\n")).not.toContain("/synthetic-vault");
  });

  it("ignores legacy report-detail values and omits them on the next save", async () => {
    const KwiryPlugin = await loadProductionPlugin();
    for (const legacy of ["compact", "full"] as const) {
      harness.storedData = { diagnosticsReportDetail: legacy };
      harness.savedData.length = 0;
      const plugin = new KwiryPlugin();
      await plugin.onload();

      expect(plugin.settings).not.toHaveProperty("diagnosticsReportDetail");
      await plugin.saveSettings();
      expect(harness.savedData.at(-1)).not.toHaveProperty("diagnosticsReportDetail");
    }
  });

  it("keeps the core policy hash across a format toggle and re-snapshots the enabled set", async () => {
    const KwiryPlugin = await loadProductionPlugin();
    const plugin = new KwiryPlugin();

    await plugin.onload();
    harness.layoutReady?.();
    await vi.waitFor(() => expect(harness.backendInitializations).toBe(1));

    const initialHash = harness.cachePolicyHashes[0];
    expect(initialHash).toMatch(/^[0-9a-f]{64}$/u);
    expect(harness.sourcePolicies[0]?.pdf).toBe(false);
    expect(harness.sourcePolicies[0]?.text).toBe(true);

    plugin.settings.enabledSourceFormats.text = false;
    await plugin.onSourcePolicyChanged();

    expect(harness.backendInitializations).toBe(2);
    expect(harness.backendDisposals).toBe(1);
    expect(harness.cachePolicyHashes).toHaveLength(2);
    expect(harness.cachePolicyHashes[1]).toMatch(/^[0-9a-f]{64}$/u);
    // The identity that decides whether the cache is even opened is core, and
    // enablement is not core. Under the old single fingerprint this assertion
    // was `not.toBe` and the toggle threw away the whole index; now the cache
    // is restored and projected down to the new enabled set instead.
    expect(harness.cachePolicyHashes[1]).toBe(initialHash);
    expect(harness.sourcePolicies[1]?.text).toBe(false);
    expect(harness.sourcePolicies[0]?.text).toBe(true);
    expect(harness.sourcePolicies[1]?.pdf).toBe(false);
  });
});
