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
  desktopExportAvailable: boolean;
  adapterBasePath: string | null;
  adapterBasePathThrows: boolean;
  fullClipboardAvailable: boolean;
  fullClipboardCalls: number;
  fullClipboardReports: string[];
  fullClipboardResult: "saved" | "cancelled" | "inside_vault" | "unsafe_destination" | "unavailable" | "write_failed";
  notices: string[];
  savedData: unknown[];
  storedData: Record<string, unknown>;
  rebuildResult: "scheduled" | "already_building";
  statusBarEvents: string[];
  statusBarText: string;
  statusBarAttributes: Record<string, string>;
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
  desktopExportAvailable: true,
  adapterBasePath: "/synthetic-vault",
  adapterBasePathThrows: false,
  fullClipboardAvailable: true,
  fullClipboardCalls: 0,
  fullClipboardReports: [],
  fullClipboardResult: "saved",
  notices: [],
  savedData: [],
  storedData: {},
  rebuildResult: "scheduled",
  statusBarEvents: [],
  statusBarText: "",
  statusBarAttributes: {},
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
    ["./diagnostics/full-report-clipboard-host", "full-report-clipboard-host"],
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
              adapter: harness.adapterBasePath === null ? {} : {
                getBasePath() {
                  if (harness.adapterBasePathThrows) throw new Error("synthetic adapter failure");
                  return harness.adapterBasePath;
                },
              },
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
          addStatusBarItem() {
            return {
              addClass(name) { harness.statusBarEvents.push('class:' + name); },
              createSpan(options) {
                harness.statusBarEvents.push('label:' + options.cls);
                return {
                  setText(text) {
                    harness.statusBarText = text;
                    harness.statusBarEvents.push('text:' + text);
                  },
                };
              },
              setAttribute(name, value) {
                harness.statusBarAttributes[name] = value;
                harness.statusBarEvents.push('attribute:' + name);
              },
            };
          }
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
            isAvailable() { return harness.desktopExportAvailable; },
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
    case "full-report-clipboard-host":
      return `
        const harness = globalThis.__kwiryStartupLifecycleHarness;
        export function createProductionFullReportClipboardHost() {
          return {
            isAvailable() { return harness.fullClipboardAvailable; },
            copy({ chunks }) {
              harness.fullClipboardCalls += 1;
              if (harness.fullClipboardResult === "saved") {
                const decoder = new TextDecoder();
                const parts = [];
                for (const chunk of chunks) {
                  parts.push(decoder.decode(chunk, { stream: true }));
                }
                parts.push(decoder.decode());
                harness.fullClipboardReports.push(parts.join(""));
              }
              return Promise.resolve({ kind: harness.fullClipboardResult });
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
    harness.desktopExportAvailable = true;
    harness.adapterBasePath = "/synthetic-vault";
    harness.adapterBasePathThrows = false;
    harness.fullClipboardAvailable = true;
    harness.fullClipboardCalls = 0;
    harness.fullClipboardReports.length = 0;
    harness.fullClipboardResult = "saved";
    harness.notices.length = 0;
    harness.savedData.length = 0;
    harness.storedData = {};
    harness.rebuildResult = "scheduled";
    harness.statusBarEvents.length = 0;
    harness.statusBarText = "";
    harness.statusBarAttributes = {};
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

  it("creates stable status geometry before the initial status render", async () => {
    const KwiryPlugin = await loadProductionPlugin();
    const plugin = new KwiryPlugin();

    await plugin.onload();

    expect(harness.statusBarEvents.slice(0, 3)).toEqual([
      "class:kwiry-status-bar",
      "label:kwiry-status-bar__label",
      "text:kwiry: starting…",
    ]);
    expect(harness.statusBarText).toBe("kwiry: starting…");
    expect(harness.statusBarAttributes).toEqual({
      title: "kwiry: starting…",
      "aria-label": "kwiry: starting…",
    });
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
      phase: "degraded",
      liveness: "alive",
      searchable: true,
      generation: "generation-1",
      dirty: true,
      rebuilding: false,
      documents: 999,
      chunks: 2_000,
      unreadableSources: 1,
      unreadableSourceCauses: [{ cause: "source_read_rejected", count: 1 }],
      issue: {
        code: "sources_unreadable",
        safeMessage: privateText,
        recoverable: true,
      },
    });
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

    const unreadableCauseRecords = structured.records.filter((record: {
      code: string;
      details: { failureCause?: string; sourcesFailed?: number };
    }) => record.code === "index.lifecycle"
      && record.details.failureCause === "source_read_rejected");
    expect(progressRecords.length).toBeGreaterThanOrEqual(20);
    expect(progressRecords.length).toBeLessThanOrEqual(21);
    expect(unreadableCauseRecords).toHaveLength(1);
    expect(unreadableCauseRecords[0]?.details.sourcesFailed).toBe(1);
    expect(report).not.toContain(privateText);
    expect(progressRecords.every((record: { details: Record<string, unknown> }) =>
      !("path" in record.details))).toBe(true);
  });

  it.each([
    { label: "missing base path", basePath: null, throws: false },
    { label: "throwing base path", basePath: "/synthetic-vault", throws: true },
    { label: "invalid base path", basePath: "", throws: false },
  ])("copies the full report synchronously when the adapter has a $label", async ({
    basePath,
    throws,
  }) => {
    harness.adapterBasePath = basePath;
    harness.adapterBasePathThrows = throws;
    const KwiryPlugin = await loadProductionPlugin();
    const plugin = new KwiryPlugin();
    await plugin.onload();

    const exportPromise = plugin.exportDiagnosticsFile();
    expect(harness.fullClipboardCalls).toBe(1);
    await exportPromise;

    expect(harness.exportVaultRoots).toEqual([]);
    expect(harness.fullClipboardReports).toHaveLength(1);
    expect(harness.fullClipboardReports[0]).toContain("Structured records (JSON)");
    expect(harness.notices.at(-1)).toBe(
      "Kwiry: full diagnostics report copied. Paste it into a text file outside the active vault, then clear the clipboard.",
    );
  });

  it("uses the full clipboard fallback when Electron file authority is unavailable", async () => {
    harness.desktopExportAvailable = false;
    const KwiryPlugin = await loadProductionPlugin();
    const plugin = new KwiryPlugin();
    await plugin.onload();

    await plugin.exportDiagnosticsFile();

    expect(harness.exportVaultRoots).toEqual([]);
    expect(harness.fullClipboardCalls).toBe(1);
    expect(harness.fullClipboardReports).toHaveLength(1);
  });

  it("fails closed without either full-export authority", async () => {
    harness.adapterBasePath = null;
    harness.fullClipboardAvailable = false;
    const KwiryPlugin = await loadProductionPlugin();
    const plugin = new KwiryPlugin();
    await plugin.onload();

    await plugin.exportDiagnosticsFile();

    expect(harness.fullClipboardCalls).toBe(0);
    expect(harness.exportChunkIterations).toBe(0);
    expect(harness.notices.at(-1)).toBe(
      "Kwiry: full diagnostics export is unavailable on this device.",
    );
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
    expect(harness.fullClipboardCalls).toBe(0);

    harness.exportResult = "inside_vault";
    await plugin.exportDiagnosticsFile();
    expect(harness.notices.at(-1)).toBe(
      "Kwiry: choose a diagnostics export location outside the active vault.",
    );
    expect(harness.notices.join("\n")).not.toContain("/synthetic-vault");
    expect(harness.exportChunkIterations).toBe(0);
    expect(harness.fullClipboardCalls).toBe(0);

    harness.exportResult = "saved";
    await plugin.exportDiagnosticsFile();
    expect(harness.exportReports).toHaveLength(1);
    expect(harness.exportChunkIterations).toBeGreaterThan(0);
    expect(harness.notices.at(-1)).toBe("Kwiry: full diagnostics report exported.");
    expect(harness.notices.join("\n")).not.toContain("/synthetic-vault");
    expect(harness.fullClipboardCalls).toBe(0);
  });

  it.each([
    ["unsafe_destination", "Kwiry: the diagnostics export destination was refused."],
    ["unavailable", "Kwiry: full diagnostics export is unavailable on this device."],
    ["write_failed", "Kwiry: diagnostics could not be exported."],
  ] as const)("does not bypass a selected desktop %s result", async (result, notice) => {
    harness.exportResult = result;
    const KwiryPlugin = await loadProductionPlugin();
    const plugin = new KwiryPlugin();
    await plugin.onload();

    await plugin.exportDiagnosticsFile();

    expect(harness.fullClipboardCalls).toBe(0);
    expect(harness.notices.at(-1)).toBe(notice);
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
    expect(harness.sourcePolicies[0]?.excel).toBe(false);
    expect(harness.sourcePolicies[0]?.html).toBe(true);
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
