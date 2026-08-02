// SPDX-FileCopyrightText: 2026 cybersader
// SPDX-License-Identifier: GPL-3.0-only

import { createRequire } from "node:module";

import { build, type Plugin as EsbuildPlugin } from "esbuild";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

interface LifecycleHarness {
  layoutReady: (() => void) | null;
  startupObserver: ((observation: unknown) => void) | null;
  backendInitializations: number;
  clipboardWrites: string[];
}

const harness: LifecycleHarness = {
  layoutReady: null,
  startupObserver: null,
  backendInitializations: 0,
  clipboardWrites: [],
};

const MAIN_PATH = new URL("../src/main.ts", import.meta.url).pathname;
const require = createRequire(import.meta.url);

async function loadProductionPlugin(): Promise<new () => {
  onload(): Promise<void>;
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
    onload(): Promise<void>;
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
        export class Notice {}
        export const Platform = { isAndroidApp: false, isIosApp: false };
        export const apiVersion = "1.8.10";
        export async function requestUrl() { throw new Error("unexpected request"); }
      `;
    case "worker-source":
      return `export default "worker source";`;
    case "active-vault-source":
      return `
        export const ACTIVE_VAULT_ID = "active-vault";
        export class ObsidianActiveVaultSource {}
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
            harness.startupObserver?.({ kind: "cache_searchable", cacheBytes: 4096 });
            harness.startupObserver?.({ kind: "fully_current" });
          }
          async status() { return this.statusValue; }
          subscribeStatus() { return () => undefined; }
          async dispose() {}
        }
      `;
    case "cache-options":
      return `export function createInPluginCacheOptions() { return undefined; }`;
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
    harness.backendInitializations = 0;
    harness.clipboardWrites.length = 0;
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
    expect(startupRecords[0]).toMatch(/firstCacheSearchableMs=\d+/u);
    expect(startupRecords[0]).toMatch(/fullyCurrentMs=\d+/u);
    expect(startupRecords[0]).toContain("cacheHit=true");
    expect(startupRecords[0]).toContain("cacheBytes=4096");
  });
});
