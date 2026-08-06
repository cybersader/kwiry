// SPDX-FileCopyrightText: 2026 cybersader
// SPDX-License-Identifier: GPL-3.0-only

import { createRequire } from "node:module";

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
  notices: string[];
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
  notices: [],
  rebuildResult: "scheduled",
};

const MAIN_PATH = new URL("../src/main.ts", import.meta.url).pathname;
const require = createRequire(import.meta.url);

async function loadProductionPlugin(): Promise<new () => {
  settings: {
    enabledSourceFormats: Record<string, boolean>;
    diagnosticsReportDetail: "compact" | "full";
  };
  onload(): Promise<void>;
  onSourcePolicyChanged(): Promise<void>;
  rebuildInPluginIndex(): Promise<void>;
  copyDiagnostics(): Promise<void>;
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
  return plugin as new () => {
    settings: {
    enabledSourceFormats: Record<string, boolean>;
    diagnosticsReportDetail: "compact" | "full";
  };
    onload(): Promise<void>;
    onSourcePolicyChanged(): Promise<void>;
    rebuildInPluginIndex(): Promise<void>;
    copyDiagnostics(): Promise<void>;
  };
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
            vault: {},
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
            };
          }
          async saveData() {}
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
    harness.notices.length = 0;
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

    // The structured records are what this assertion inspects, so ask for the
    // full report explicitly; the shipped default is compact so a field report
    // stays small enough to send.
    plugin.settings.diagnosticsReportDetail = "full";
    await plugin.copyDiagnostics();
    const report = harness.clipboardWrites.at(-1) ?? "";
    const jsonText = report.split("Structured records (JSON):\n")[1];
    expect(jsonText).toBeDefined();

    plugin.settings.diagnosticsReportDetail = "compact";
    await plugin.copyDiagnostics();
    const compact = harness.clipboardWrites.at(-1) ?? "";
    expect(compact).toContain("structured_records: omitted");
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

  it("reactivates with an awaited new policy hash and a fresh source policy snapshot", async () => {
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
    expect(harness.cachePolicyHashes[1]).not.toBe(initialHash);
    expect(harness.sourcePolicies[1]?.text).toBe(false);
    expect(harness.sourcePolicies[0]?.text).toBe(true);
    expect(harness.sourcePolicies[1]?.pdf).toBe(false);
  });
});
