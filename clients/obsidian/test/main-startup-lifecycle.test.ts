// SPDX-FileCopyrightText: 2026 cybersader
// SPDX-License-Identifier: GPL-3.0-only

import { createRequire } from "node:module";
import { fileURLToPath } from "node:url";

import { build, type Plugin as EsbuildPlugin } from "esbuild";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { HOST_AVAILABLE_EVENT, HOST_SLOT_KEY } from "../src/host-services";
import type { SearchProfile } from "../src/backend";
import type { KwirySearchApiV1 } from "../src/public-api";

interface SettingRow {
  name?: string;
  description?: string;
  options?: Record<string, { label: string; disabled: boolean }>;
  value?: string;
  disabled?: boolean;
  onChange?: (value: string) => Promise<void>;
}

class HostElement {
  children: HostElement[] = [];
  attrs: Record<string, string>;
  textContent: string;
  cls: string;
  value = "";
  disabled = false;
  hidden = false;
  private readonly handlers = new Map<string, (event: { preventDefault(): void }) => void>();

  constructor(readonly tag: string, options: { attr?: Record<string, string>; text?: string; cls?: string } = {}) {
    this.attrs = options.attr ?? {};
    this.textContent = options.text ?? "";
    this.cls = options.cls ?? "";
  }

  get text(): string { return this.textContent + this.children.map((child) => child.text).join(""); }

  find(tag: string, text?: string): HostElement {
    const element = this.all(tag).find((element) => text === undefined || text === element.textContent);
    if (!element) throw new Error(`Missing test element: ${tag}`);
    return element;
  }

  all(tag: string): HostElement[] {
    return [...(this.tag === tag ? [this] : []), ...this.children.flatMap((child) => child.all(tag))];
  }

  createEl(tag: string, options?: ConstructorParameters<typeof HostElement>[1]): HostElement {
    const child = new HostElement(tag, options);
    this.children.push(child);
    return child;
  }

  createDiv(options: ConstructorParameters<typeof HostElement>[1]): HostElement {
    return this.createEl("div", options);
  }

  addClass(cls: string): void { this.cls = [this.cls, cls].filter(Boolean).join(" "); }

  addEventListener(type: string, callback: (event: { preventDefault(): void }) => void): void {
    this.handlers.set(type, callback);
  }

  fire(type: string): void { this.handlers.get(type)?.({ preventDefault() {} }); }
  empty(): void { this.children.length = 0; this.textContent = ""; }
  focus(): void { /* Focus is not simulated. */ }
}

interface HostModalDouble { contentEl: HostElement; close(): void }

interface LifecycleHarness {
  HostElement: typeof HostElement;
  hostModals: HostModalDouble[];
  commands: Map<string, () => void>;
  forbidden: string[];
  vaultFiles: Map<string, "file" | "folder" | { alias: string }>;
  openedPaths: string[];
  pathLookups: string[];
  openError: unknown;
  beforeSave?: () => Promise<void>;
  beforeInitialize?: () => void;
  afterLoad?: () => void;
  exactSettings: boolean;
  hoverSources: Array<{ id: string; display: string; defaultMod: boolean }>;
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
  storedData: Record<string, unknown> | null;
  desktopApp: boolean;
  nodeFilesystemAvailable: boolean;
  desktopTransportAvailable: boolean;
  displaySettings: (() => void) | null;
  settingRows: SettingRow[];
  rebuildResult: "scheduled" | "already_building";
  statusBarEvents: string[];
  statusBarText: string;
  statusBarAttributes: Record<string, string>;
}

const harness: LifecycleHarness = {
  HostElement,
  hostModals: [],
  commands: new Map(),
  forbidden: [],
  vaultFiles: new Map(),
  openedPaths: [],
  pathLookups: [],
  openError: undefined,
  exactSettings: false,
  hoverSources: [],
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
  desktopApp: true,
  nodeFilesystemAvailable: true,
  desktopTransportAvailable: true,
  displaySettings: null,
  settingRows: [],
  rebuildResult: "scheduled",
  statusBarEvents: [],
  statusBarText: "",
  statusBarAttributes: {},
};

const MAIN_PATH = fileURLToPath(new URL("../src/main.ts", import.meta.url));
const require = createRequire(import.meta.url);

async function loadProductionPlugin(): Promise<new () => {
  settings: {
    backendProfile: SearchProfile;
    enabledSourceFormats: Record<string, boolean>;
    diagnosticsReportLevel: "debug" | "info" | "warn" | "error";
    diagnosticsReportScope: "all" | "indexing" | "search" | "startup" | "failures";
    resultLimit: number;
  };
  readonly api: KwirySearchApiV1;
  onload(): Promise<void>;
  onunload(): void;
  saveSettings(): Promise<void>;
  changeBackendProfile(profile: SearchProfile): Promise<void>;
  refreshStatus(): Promise<void>;
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
  evaluate(module, module.exports, (id: string): unknown => {
    if (id !== "fs") return require(id);
    if (!harness.nodeFilesystemAvailable) return {};
    return {
      ...(require("node:fs") as typeof import("node:fs")),
      lstatSync() {
        harness.forbidden.push("token");
        throw new Error("unexpected filesystem read");
      },
    };
  });
  const plugin = module.exports.default;
  if (typeof plugin !== "function") throw new Error("main.ts test bundle has no default plugin");
  return plugin as Awaited<ReturnType<typeof loadProductionPlugin>>;
}

function lifecycleHarnessPlugin(): EsbuildPlugin {
  const stubs = new Map<string, string>([
    ["obsidian", "obsidian"],
    ["virtual:kwiry-worker-source", "worker-source"],
    ["./active-vault-source", "active-vault-source"],
    ["./backends/in-plugin-lexical-backend", "in-plugin-backend"],
    ["./cache/build-cache-options", "cache-options"],
    ["./diagnostics/desktop-export-host", "desktop-export-host"],
    ["./diagnostics/full-report-clipboard-host", "full-report-clipboard-host"],
    ["./internal/private-tools", "private-tools"],
    ["./search-modal", "search-modal"],
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
        export class Modal {
          contentEl = new harness.HostElement("modal");
          constructor(app) { this.app = app; }
          setTitle() {}
          open() { harness.hostModals.push(this); this.onOpen(); }
          close() { this.onClose(); }
        }
        export class TFile { constructor(path) { this.path = path; } }
        export class PluginSettingTab {
          containerEl = { empty() { harness.settingRows.length = 0; } };
          hide() {}
        }
        export class Setting {
          row = {};
          constructor() { harness.settingRows.push(this.row); }
          setName(name) { this.row.name = name; return this; }
          setDesc(description) { this.row.description = description; return this; }
          setHeading() { return this; }
          addText(configure) {
            const row = this.row;
            configure?.({
              setPlaceholder() { return this; },
              setValue() { return this; },
              onChange(callback) { row.onChange = callback; return this; },
            });
            return this;
          }
          addToggle() { return this; }
          addButton() { return this; }
          addDropdown(configure) {
            const row = this.row;
            configure({
              selectEl: {
                querySelector(selector) {
                  return selector === 'option[value="daemon"]' ? row.options.daemon ?? null : null;
                },
              },
              addOptions(options) {
                row.options = Object.fromEntries(Object.entries(options).map(([value, label]) =>
                  [value, { label, disabled: false }]));
                return this;
              },
              setValue(value) { row.value = value; return this; },
              setDisabled(disabled) { row.disabled = disabled; return this; },
              onChange(callback) { row.onChange = callback; return this; },
            });
            return this;
          }
        }
        export class Plugin {
          app = {
            vault: {
              getFiles() { harness.forbidden.push("enumeration"); return []; },
              getMarkdownFiles() { harness.forbidden.push("enumeration"); return []; },
              getAllLoadedFiles() { harness.forbidden.push("enumeration"); return []; },
              read() { harness.forbidden.push("body"); },
              cachedRead() { harness.forbidden.push("body"); },
              readBinary() { harness.forbidden.push("body"); },
              getAbstractFileByPath(path) {
                harness.pathLookups.push(path);
                const file = harness.vaultFiles.get(path);
                if (file === "file") return new TFile(path);
                if (file === "folder") return { path };
                if (file) return new TFile(file.alias);
                return null;
              },
              adapter: harness.adapterBasePath === null ? {} : {
                getBasePath() {
                  if (harness.adapterBasePathThrows) throw new Error("synthetic adapter failure");
                  return harness.adapterBasePath;
                },
              },
            },
            workspace: {
              onLayoutReady(callback) { harness.layoutReady = callback; },
              getLeaf(explicitNewLeaf) {
                if (explicitNewLeaf !== false) throw new Error("unexpected leaf request");
                return { openFile: async (file) => {
                  if (harness.openError !== undefined) throw harness.openError;
                  harness.openedPaths.push(file.path);
                } };
              },
            },
          };
          manifest = { version: "0.5.2", name: "Kwiry Search" };
          registerHoverLinkSource(id, info) { harness.hoverSources.push({ id, ...info }); }
          async loadData() {
            if (harness.exactSettings || harness.storedData === null) {
              const snapshot = harness.storedData;
              harness.afterLoad?.();
              return snapshot;
            }
            return {
              backendProfile: "in_plugin",
              diagnosticsLogLevel: "info",
              showRibbonIcon: false,
              ...harness.storedData,
            };
          }
          async saveData(value) {
            await harness.beforeSave?.();
            harness.savedData.push(value);
          }
          addSettingTab(tab) { harness.displaySettings = () => tab.display(); }
          addCommand({ id, callback }) { harness.commands.set(id, callback); }
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
        export const Platform = {
          isDesktopApp: harness.desktopApp, isAndroidApp: false, isIosApp: false,
        };
        export const apiVersion = "1.8.10";
        export const requestUrl = harness.desktopTransportAvailable
          ? async () => { harness.forbidden.push("daemon"); throw new Error("unexpected request"); }
          : undefined;
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
    case "in-plugin-backend":
      return `
        const harness = globalThis.__kwiryStartupLifecycleHarness;
        export class InPluginLexicalBackend {
          constructor(options) {
            this.identity = {
              profile: "in_plugin",
              instanceId: options.instanceId,
              label: "In-plugin",
              boundVaultId: options.activeVaultId,
            };
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
            harness.beforeInitialize?.();
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
          async search(request) {
            return {
              requestedMode: request.mode,
              effectiveMode: request.mode,
              response: {
                hits: [{
                  path: "Notes/Orchard.md",
                  format: "markdown",
                  heading_path: ["Pears"],
                  score: 2,
                  vault_id: this.identity.boundVaultId,
                  origin: {
                    profile: this.identity.profile,
                    backendInstanceId: this.identity.instanceId,
                    vaultId: this.identity.boundVaultId,
                  },
                }],
              },
            };
          }
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
          return { sourcePolicyHash, openStore: async () => ({ kind: "unavailable" }) };
        }
      `;
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
    default:
      throw new Error(`missing lifecycle test stub: ${path}`);
  }
}

const NOTICE = "Results may be incomplete.";
const HOST_LABEL = "Host search · Lexical";
const NO_HOST = "Host search is unavailable: no host is providing it here.";
const NOT_OFFERED = "Host search is unavailable: this host does not offer search version 1.";
const QUERY = "  cobalt marmot  ";
const CURSOR = "opaque-continuation-marmot";
const REF = "opaque-note-marmot";
const PATH = "Saffron/Field Log.md";
const EXCERPT = "<img src=x> **cobalt marmot**";
const RAW = "private-stub-error-marmot";
const HIT = { ref: REF, path: PATH, title: "Field Log", excerpt: EXCERPT };
const firstRoom = { ref: "room-saffron", label: "<Saffron>" };
const secondRoom = { ref: "room-indigo", label: "Indigo" };
const hitTwin = { ref: "opaque-note-lark", path: "Indigo/Field Log.md", title: "Field Log", excerpt: "Lark" };

type PageRequest = { query: string; cursor?: string };

function page(overrides: {
  room?: typeof firstRoom | null;
  state?: string;
  items?: unknown[];
  next?: string | null;
  notice?: string;
  decidedBy?: string;
} = {}) {
  return { room: firstRoom, state: "ready", items: [HIT], next: null, notice: NOTICE, ...overrides };
}

interface PendingTimer { id: number; at: number; callback: () => void }

/** Deterministic window timers: nothing fires until a test advances the clock. */
const clock = { now: 0, nextId: 1, pending: [] as PendingTimer[], set: [] as number[] };
/** Window event listeners, so tests can count them and dispatch the provider-arrived event. */
const windowEvents = new Map<string, Set<(event: unknown) => void>>();

function fakeWindow() {
  clock.now = 0;
  clock.nextId = 1;
  clock.pending.length = 0;
  clock.set.length = 0;
  windowEvents.clear();
  return {
    setInterval: () => 1,
    setTimeout(callback: () => void, ms: number) {
      const id = clock.nextId++;
      clock.pending.push({ id, at: clock.now + ms, callback });
      clock.set.push(ms);
      return id;
    },
    clearTimeout(id: number) {
      const index = clock.pending.findIndex((timer) => timer.id === id);
      if (index >= 0) clock.pending.splice(index, 1);
    },
    addEventListener(type: string, callback: (event: unknown) => void, options?: { signal?: AbortSignal }) {
      if (options?.signal?.aborted) return;
      const listeners = windowEvents.get(type) ?? new Set();
      windowEvents.set(type, listeners);
      listeners.add(callback);
      options?.signal?.addEventListener("abort", () => listeners.delete(callback), { once: true });
    },
    removeEventListener(type: string, callback: (event: unknown) => void) {
      windowEvents.get(type)?.delete(callback);
    },
    dispatchEvent(event: { type: string }) {
      for (const callback of [...windowEvents.get(event.type) ?? []]) callback(event);
      return true;
    },
  };
}

function advance(ms: number): void {
  const target = clock.now + ms;
  for (;;) {
    clock.pending.sort((left, right) => left.at - right.at || left.id - right.id);
    const due = clock.pending[0];
    if (!due || due.at > target) break;
    clock.pending.shift();
    clock.now = due.at;
    due.callback();
  }
  clock.now = target;
}

function availableListeners(): number {
  return windowEvents.get(HOST_AVAILABLE_EVENT)?.size ?? 0;
}

function announceProvider(): void {
  (window as unknown as { dispatchEvent(event: { type: string }): boolean })
    .dispatchEvent({ type: HOST_AVAILABLE_EVENT });
}

function installSlot(slot: unknown): void {
  Reflect.set(window, HOST_SLOT_KEY, slot);
}

function removeSlot(): void {
  Reflect.deleteProperty(window, HOST_SLOT_KEY);
}

type PolicyScript =
  | "absent"
  | { value: unknown; decidedBy?: string; limit?: unknown; hang?: boolean };

/** A scripted provider: search v1 and, optionally, policy v1, plus an invalidation hook. */
function scriptedHost(
  kind: "present" | "absent" | "late" | "wrong major" | "malformed" | "without search" = "present",
  policyScript: PolicyScript = "absent",
) {
  const search = vi.fn<(request: PageRequest, options: { signal: AbortSignal }) => Promise<unknown>>();
  const open = vi.fn<(ref: string, options: { signal: AbortSignal }) => Promise<unknown>>();
  search.mockResolvedValue(page());
  open.mockResolvedValue(undefined);
  const listeners = new Set<(event: { reason: string }) => void>();
  const onInvalidated = vi.fn((listener: (event: { reason: string }) => void) => {
    listeners.add(listener);
    return () => { listeners.delete(listener); };
  });
  const service = Object.freeze({
    major: kind === "wrong major" ? 2 : 1,
    minor: 0,
    page: search,
    open: kind === "malformed" ? 1 : open,
    onInvalidated,
  });
  const policyListeners = new Set<(event: unknown) => void>();
  const snapshot = (answer: { value: unknown; decidedBy?: string; limit?: unknown }) => ({
    localIndexing: { value: answer.value, decidedBy: answer.decidedBy ?? "protocol",
      ...(answer.limit === undefined ? {} : { limit: answer.limit }) },
  });
  const policy = policyScript === "absent" ? undefined : Object.freeze({
    major: 1,
    minor: 0,
    current: vi.fn(async () => policyScript.hang ? new Promise(() => undefined) : snapshot(policyScript)),
    subscribe: vi.fn((listener: (event: unknown) => void, options?: { signal?: AbortSignal }) => {
      policyListeners.add(listener);
      options?.signal?.addEventListener("abort", () => policyListeners.delete(listener), { once: true });
      if (!policyScript.hang) queueMicrotask(() => listener({ type: "snapshot", snapshot: snapshot(policyScript) }));
      return () => { policyListeners.delete(listener); };
    }),
    onInvalidated,
  });
  const get = vi.fn((name: string, major: number) => {
    if (name === "search" && major === 1 && kind !== "without search") return service;
    if (name === "policy" && major === 1) return policy;
    return undefined;
  });
  const slot = Object.freeze({ get });
  if (kind !== "absent" && kind !== "late") installSlot(slot);
  return {
    search,
    open,
    get,
    listeners,
    policyListeners,
    install: () => installSlot(slot),
    invalidate(reason: string) {
      for (const listener of [...listeners]) listener({ reason });
    },
    changePolicy(answer: { value: unknown; decidedBy?: string }) {
      for (const listener of [...policyListeners]) listener({ type: "change", snapshot: snapshot(answer) });
    },
  };
}

function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (error: unknown) => void;
  const promise = new Promise<T>((done, fail) => { resolve = done; reject = fail; });
  return { promise, resolve, reject };
}

async function settled(): Promise<void> {
  await new Promise<void>((resolve) => setImmediate(resolve));
}

async function startedHost() {
  const KwiryPlugin = await loadProductionPlugin();
  harness.storedData = null;
  const plugin = new KwiryPlugin();
  await plugin.onload();
  expect(plugin.settings.backendProfile).toBe("host");
  harness.layoutReady?.();
  await vi.waitFor(() => expect(harness.statusBarText).toBe(`Kwiry: ${HOST_LABEL}`), { interval: 5 });
  await settled();
  return plugin;
}

async function openHost(): Promise<HostModalDouble> {
  const index = harness.hostModals.length;
  harness.commands.get("open-search")?.();
  await settled();
  const modal = harness.hostModals[index];
  if (!modal) throw new Error("Host search modal did not open.");
  return modal;
}

function inputText(modal: HostModalDouble, text: string): void {
  const input = modal.contentEl.find("input");
  input.value = text;
  input.fire("input");
}

function submit(modal: HostModalDouble, text: string): void {
  inputText(modal, text);
  modal.contentEl.find("form").fire("submit");
}

function storedProfiles(): unknown[] {
  return harness.savedData.map((write) => Reflect.get(write as object, "backendProfile"));
}

describe("KwiryPlugin startup lifecycle wiring", () => {
  beforeEach(() => {
    harness.hostModals.length = 0;
    harness.commands.clear();
    harness.forbidden.length = 0;
    harness.vaultFiles.clear();
    harness.openedPaths.length = 0;
    harness.pathLookups.length = 0;
    harness.openError = undefined;
    delete harness.beforeSave;
    delete harness.beforeInitialize;
    delete harness.afterLoad;
    harness.exactSettings = false;
    harness.hoverSources.length = 0;
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
    harness.desktopApp = true;
    harness.nodeFilesystemAvailable = true;
    harness.desktopTransportAvailable = true;
    harness.displaySettings = null;
    harness.settingRows.length = 0;
    harness.rebuildResult = "scheduled";
    harness.statusBarEvents.length = 0;
    harness.statusBarText = "";
    harness.statusBarAttributes = {};
    vi.stubGlobal("__kwiryStartupLifecycleHarness", harness);
    vi.stubGlobal("window", fakeWindow());
    vi.stubGlobal("navigator", {
      clipboard: {
        writeText: async (value: string) => {
          harness.clipboardWrites.push(value);
        },
      },
    });
  });

  afterEach(() => {
    vi.restoreAllMocks();
    vi.unstubAllGlobals();
  });

  const selectionCases = [true, false].flatMap((desktop) =>
    (["present", "absent", "wrong major", "malformed", "without search"] as const).flatMap((kind) =>
      (["fresh", "daemon", "in_plugin", "host", "legacy"] as const).map((stored) => ({ desktop, kind, stored }))));

  it.each(selectionCases)("selects before activation: desktop=$desktop, host=$kind, settings=$stored", async ({ desktop, kind, stored }) => {
    const host = scriptedHost(kind);
    const worker = vi.fn();
    const fetch = vi.fn();
    vi.stubGlobal("Worker", worker);
    vi.stubGlobal("fetch", fetch);
    harness.exactSettings = true;
    harness.desktopApp = desktop;
    harness.storedData = stored === "fresh" ? null : {
      ...(stored === "legacy" ? {} : { backendProfile: stored }),
      tokenFilePath: "/synthetic/token-file",
    };
    const expectedProfile = stored === "fresh"
      ? (kind === "present" ? "host" : desktop ? "daemon" : "in_plugin")
      : stored === "legacy" ? "daemon" : stored;
    const slotPresent = kind !== "absent";
    const KwiryPlugin = await loadProductionPlugin();
    const plugin = new KwiryPlugin();
    await plugin.onload();
    expect(plugin.settings.backendProfile).toBe(expectedProfile);
    expect(harness.backendInitializations).toBe(0);
    expect(harness.forbidden).toEqual([]);
    expect(harness.cachePolicyHashes).toEqual([]);
    expect(harness.sourcePolicies).toEqual([]);
    if (expectedProfile === "host") {
      await expect(plugin.api.ready()).rejects.toMatchObject({ code: "backend_unavailable" });
    }
    harness.displaySettings?.();
    const choice = harness.settingRows.find((row) => row.name === "Search backend");
    expect(choice?.options?.host?.label).toBe(expectedProfile === "host" || kind === "present" ? HOST_LABEL : undefined);
    expect(choice?.value).toBe(expectedProfile);
    expect(host.get.mock.calls.every(([name, major]) => (name === "search" || name === "policy") && major === 1)).toBe(true);
    harness.layoutReady?.();
    // Nothing waits: there is no polling window.
    expect(clock.set).toEqual([]);
    if (expectedProfile === "host") {
      const status = kind === "present" ? HOST_LABEL : kind === "absent" ? NO_HOST : NOT_OFFERED;
      await vi.waitFor(() => expect(harness.statusBarText).toBe(`Kwiry: ${status}`), { interval: 5 });
      await settled();
      await plugin.refreshStatus();
      await plugin.rebuildInPluginIndex();
      await expect(plugin.api.search("purple tern")).rejects.toMatchObject({ code: "backend_unavailable" });
      const modal = await openHost();
      expect(host.search).not.toHaveBeenCalled();
      expect(host.open).not.toHaveBeenCalled();
      if (kind === "present") {
        host.search.mockRejectedValueOnce({ code: "unavailable", family: "unavailable", message: RAW });
        submit(modal, QUERY);
        await settled();
        expect(host.search.mock.calls).toEqual([[{ query: QUERY }, { signal: expect.any(AbortSignal) }]]);
      } else {
        expect(modal.contentEl.text).toContain(status);
        submit(modal, QUERY);
        await settled();
        expect(host.search).not.toHaveBeenCalled();
        expect(modal.contentEl.text).toContain(status);
      }
      expect(harness.backendInitializations).toBe(0);
      expect(harness.cachePolicyHashes).toEqual([]);
      expect(harness.sourcePolicies).toEqual([]);
      expect(harness.forbidden).toEqual([]);
      expect(worker).not.toHaveBeenCalled();
      expect(fetch).not.toHaveBeenCalled();
      expect(harness.pathLookups).toEqual([]);
    } else if (expectedProfile === "in_plugin") {
      await vi.waitFor(() => expect(harness.backendInitializations).toBe(1), { interval: 5 });
      // Under a host slot the persistent cache is switched off explicitly.
      expect(harness.cachePolicyHashes).toHaveLength(slotPresent ? 0 : 1);
    } else {
      await vi.waitFor(() => expect(harness.statusBarText).not.toBe("kwiry: starting…"), { interval: 5 });
    }
    // Presence and absence are never saved.
    expect(harness.savedData).toEqual([]);
    plugin.onunload();
    expect(availableListeners()).toBe(0);
  });

  it("submits exactly, preserves order and plain text, keeps every row, and replaces room groups only on Next", async () => {
    const host = scriptedHost();
    host.search.mockResolvedValueOnce(page({ items: [], next: CURSOR }))
      .mockResolvedValueOnce(page({
        items: [HIT, { ...HIT, ref: "opaque-second", path: "Saffron/Alder.md", title: "Alder" }, HIT],
        next: "advance",
      }))
      .mockResolvedValueOnce(page({ room: secondRoom, items: [hitTwin] }));
    const plugin = await startedHost();
    const modal = await openHost();
    const { contentEl } = modal;
    expect(contentEl.cls.split(" ")).toContain("kwiry-host-search");
    expect(contentEl.find("button", "Next page").hidden).toBe(true);
    inputText(modal, QUERY);
    expect(host.search).not.toHaveBeenCalled();
    expect(contentEl.text).not.toContain(NOTICE);
    contentEl.find("form").fire("submit");
    await settled();
    expect(contentEl.find("h3").text).toBe(firstRoom.label);
    expect(contentEl.all("li")).toEqual([]);
    expect(contentEl.find("p", NOTICE).attrs.role).toBe("status");
    expect(contentEl.find("button", "Next page").hidden).toBe(false);
    expect(host.search).toHaveBeenCalledTimes(1);
    contentEl.find("button", "Next page").fire("click");
    await settled();
    expect(contentEl.find("h3").text).toBe(firstRoom.label);
    // Reference equality is not meaningful, so nothing is deduplicated.
    expect(contentEl.all("li").map((li) => li.find("button").text)).toEqual(["Field Log", "Alder", "Field Log"]);
    expect(contentEl.find("div", EXCERPT).children).toHaveLength(0);
    expect(contentEl.all("img")).toHaveLength(0);
    contentEl.find("button", "Next page").fire("click");
    await settled();
    expect(contentEl.find("h3").text).toBe(secondRoom.label);
    expect(contentEl.all("li")).toHaveLength(1);
    expect(contentEl.text).toContain(hitTwin.path);
    expect(contentEl.text).not.toContain(PATH);
    expect(contentEl.find("p", NOTICE)).toBeDefined();
    expect(contentEl.find("button", "Next page").hidden).toBe(true);
    expect(host.search.mock.calls).toEqual([
      [{ query: QUERY }, { signal: expect.any(AbortSignal) }],
      [{ query: QUERY, cursor: CURSOR }, { signal: expect.any(AbortSignal) }],
      [{ query: QUERY, cursor: "advance" }, { signal: expect.any(AbortSignal) }],
    ]);
    expect(host.open).not.toHaveBeenCalled();
    plugin.onunload();
  });

  it("renders hostile host strings literally as text", async () => {
    const host = scriptedHost();
    const hostile = {
      ref: "opaque-hostile",
      path: "Blue/Budget.md",
      title: "<img src=x onerror=\"window.__pwned=1\">",
      excerpt: "![](https://beacon.invalid/p) [x](javascript:alert(1)) ‮evil‬</h4><script>1</script>",
    };
    host.search.mockResolvedValueOnce(page({ room: { ref: "room-x", label: "</h3><b>Blue</b>" }, items: [hostile] }));
    const plugin = await startedHost();
    const modal = await openHost();
    submit(modal, "hostile");
    await settled();
    const { contentEl } = modal;
    expect(contentEl.find("h3").textContent).toBe("</h3><b>Blue</b>");
    expect(contentEl.find("button", hostile.title).children).toHaveLength(0);
    expect(contentEl.find("div", hostile.excerpt).children).toHaveLength(0);
    for (const tag of ["img", "script", "b", "a"]) expect(contentEl.all(tag)).toHaveLength(0);
    plugin.onunload();
  });

  it.each([
    [page({ state: "ready", items: [], next: CURSOR }), "Results from the host"],
    [page({ state: "building", items: [], next: CURSOR }), "Host search is being prepared."],
    [page({ state: "unavailable", items: [], next: CURSOR }), "Host search is unavailable right now."],
    [page({ state: "not_enabled", items: [], next: CURSOR }), "Host search is turned off by policy."],
    [page({ state: "not_enabled", items: [], next: CURSOR, decidedBy: "organization" }), "Host search is turned off by your organization."],
    [page({ state: "not_enabled", items: [], next: CURSOR, decidedBy: "galaxy" }), "Host search is turned off by policy."],
    [page({ state: "paused-by-future-minor", items: [HIT], next: CURSOR }), "Host search is unavailable right now."],
  ])("shows a host state, the notice, and only a supplied continuation: %j", async (first, label) => {
    const host = scriptedHost();
    host.search.mockResolvedValueOnce(first)
      .mockResolvedValueOnce(page({ state: first.state, items: [], room: first.state === "ready" ? secondRoom : null }));
    const plugin = await startedHost();
    const modal = await openHost();
    submit(modal, QUERY);
    await settled();
    expect(modal.contentEl.find("p", label)).toBeDefined();
    expect(modal.contentEl.find("p", NOTICE)).toBeDefined();
    // An unknown state is treated as unavailable: its rows are never shown.
    expect(modal.contentEl.all("li")).toHaveLength(0);
    expect(host.search).toHaveBeenCalledTimes(1);
    modal.contentEl.find("button", "Next page").fire("click");
    await settled();
    expect(modal.contentEl.find("p", NOTICE)).toBeDefined();
    expect(modal.contentEl.find("button", "Next page").hidden).toBe(true);
    expect(host.search).toHaveBeenCalledTimes(2);
    plugin.onunload();
  });

  it.each([
    ["invalid_query", undefined, "Use words separated by spaces, without spaces at the ends."],
    ["unsupported_mode", undefined, "This search request is not supported by the host."],
    ["invalid_request", undefined, "This search request is not supported by the host."],
    ["access_denied", undefined, "You no longer have access to these results."],
    ["invalid_cursor", undefined, "This page can no longer be continued. Submit a new search."],
    ["result_unavailable", undefined, "That note is no longer available."],
    ["limited", undefined, "Host search reached a limit. Maximum: 11 query bytes, set by this workspace. Try again in 3 seconds."],
    ["unavailable", undefined, "Host search is unavailable right now."],
    ["invalidated", undefined, "These results expired. Search again."],
    ["not_enabled", undefined, "Host search is turned off by your organization."],
    ["conflict", undefined, "Changed elsewhere. Search again."],
    ["read_only", undefined, "Read-only right now."],
    ["declined", undefined, "Results from the host"],
    ["cancelled", undefined, "Results from the host"],
    // Unknown codes are handled by their family, then as unavailable.
    ["search_unavailable", undefined, "Host search is unavailable right now."],
    ["future_cursor_code", "invalidated", "These results expired. Search again."],
    ["future_shape_code", "invalid_request", "This search request is not supported by the host."],
    ["future_denial", "access_denied", "You no longer have access to these results."],
    ["future_code", "made_up_family", "Host search is unavailable right now."],
  ])("handles %s (family %s) safely without a replacement request", async (code, family, label) => {
    const host = scriptedHost();
    const failure = Object.assign(new Error(RAW), {
      code, ...(family === undefined ? {} : { family }), decidedBy: "organization",
      limit: { dimension: "queryBytes", maximum: 11, origin: "workspace" }, retryAfterMs: 2_500,
    });
    host.search.mockResolvedValueOnce(page({ next: CURSOR }));
    if (code === "result_unavailable") host.open.mockRejectedValueOnce(failure);
    else host.search.mockRejectedValueOnce(failure);
    const plugin = await startedHost();
    const modal = await openHost();
    submit(modal, QUERY);
    await settled();
    modal.contentEl.find("button", code === "result_unavailable" ? HIT.title : "Next page").fire("click");
    await settled();
    expect(modal.contentEl.find("p", label)).toBeDefined();
    expect(modal.contentEl.text).not.toContain(RAW);
    expect(host.search).toHaveBeenCalledTimes(code === "result_unavailable" ? 1 : 2);
    expect(harness.openedPaths).toEqual([]);
    if (code === "access_denied" || family === "access_denied") {
      expect(modal.contentEl.text).not.toContain(firstRoom.label);
      expect(modal.contentEl.text).not.toContain(PATH);
      expect(modal.contentEl.find("button", "Next page").hidden).toBe(true);
    }
    if (code === "result_unavailable") {
      expect(modal.contentEl.all("li")).toHaveLength(0);
      expect(modal.contentEl.find("p", NOTICE)).toBeDefined();
    }
    if (code === "invalid_cursor") {
      const next = modal.contentEl.find("button", "Next page");
      expect(next.hidden).toBe(true);
      next.fire("click");
      expect(host.search).toHaveBeenCalledTimes(2);
      modal.contentEl.find("form").fire("submit");
      await settled();
      expect(host.search.mock.calls.at(-1)?.[0]).toEqual({ query: QUERY });
    }
    plugin.onunload();
  });

  it("shows no unsafe limit values or partial rows from a malformed page", async () => {
    const host = scriptedHost();
    host.search.mockRejectedValueOnce({ code: "limited", limit: { dimension: RAW, maximum: 55, origin: RAW }, retryAfterMs: Infinity })
      .mockResolvedValueOnce(page({ items: [HIT, { ref: "malformed" }] }));
    const plugin = await startedHost();
    const modal = await openHost();
    submit(modal, QUERY);
    await settled();
    expect(modal.contentEl.find("p", "Host search reached a limit. The limit is set by policy.")).toBeDefined();
    expect(modal.contentEl.text).not.toContain(RAW);
    submit(modal, QUERY);
    await settled();
    expect(modal.contentEl.find("p", "Host search is unavailable right now.")).toBeDefined();
    expect(modal.contentEl.all("li")).toHaveLength(0);
    expect(modal.contentEl.text).not.toContain(PATH);
    plugin.onunload();
  });

  it("aborts superseded queries and unloads, and ignores host output even if the host does not cancel", async () => {
    const host = scriptedHost();
    const first = deferred<unknown>();
    const last = deferred<unknown>();
    host.search.mockReturnValueOnce(first.promise).mockResolvedValueOnce(page({ room: secondRoom, items: [hitTwin] }))
      .mockReturnValueOnce(last.promise);
    const plugin = await startedHost();
    const modal = await openHost();
    submit(modal, QUERY);
    const firstSignal = host.search.mock.calls[0]?.[1].signal;
    expect(firstSignal?.aborted).toBe(false);
    inputText(modal, "lark");
    expect(firstSignal?.aborted).toBe(true);
    expect(host.search).toHaveBeenCalledTimes(1);
    modal.contentEl.find("form").fire("submit");
    await settled();
    first.resolve(page());
    await settled();
    expect(modal.contentEl.text).toContain(hitTwin.path);
    expect(modal.contentEl.text).not.toContain(PATH);
    submit(modal, "marten");
    const lastSignal = host.search.mock.calls[2]?.[1].signal;
    const input = modal.contentEl.find("input");
    plugin.onunload();
    expect(lastSignal?.aborted).toBe(true);
    expect(input.value).toBe("");
    last.resolve(page());
    await settled();
    expect(modal.contentEl.text).toBe("");
    expect(host.listeners.size).toBe(0);
  });

  it("opens through the host with the reference and a signal, and touches no vault files", async () => {
    const host = scriptedHost();
    host.search.mockResolvedValueOnce(page({ items: [HIT, hitTwin] }));
    harness.vaultFiles.set(hitTwin.path, "file");
    const plugin = await startedHost();
    const modal = await openHost();
    submit(modal, QUERY);
    await settled();
    expect(host.open).not.toHaveBeenCalled();
    modal.contentEl.all("li")[1]?.find("button", hitTwin.title).fire("click");
    await settled();
    expect(host.open.mock.calls).toEqual([[hitTwin.ref, { signal: expect.any(AbortSignal) }]]);
    expect(harness.pathLookups).toEqual([]);
    expect(harness.openedPaths).toEqual([]);
    expect(harness.forbidden).toEqual([]);
    // A successful open closes the modal and releases its invalidation listener.
    expect(modal.contentEl.text).toBe("");
    expect(host.listeners.size).toBe(0);
    plugin.onunload();
  });

  it.each(["dismiss", "switch profile", "unload", "replace query"] as const)(
    "aborts an open before the host's open step on %s; it rejects cancelled and nothing opens", async (action) => {
      const host = scriptedHost();
      const opening = deferred<unknown>();
      let openedByHost = false;
      host.open.mockImplementationOnce((_ref, { signal }) => {
        // A conforming host: an abort before the open step means nothing opens.
        signal.addEventListener("abort", () => opening.reject({ code: "cancelled", family: "cancelled" }), { once: true });
        return opening.promise.then(() => { openedByHost = true; });
      });
      const plugin = await startedHost();
      const modal = await openHost();
      submit(modal, QUERY);
      await settled();
      modal.contentEl.find("button", HIT.title).fire("click");
      const signal = host.open.mock.calls[0]?.[1].signal;
      expect(host.open.mock.calls).toEqual([[REF, { signal: expect.any(AbortSignal) }]]);
      expect(signal?.aborted).toBe(false);
      let change: Promise<void> | undefined;
      if (action === "dismiss") modal.close();
      if (action === "unload") plugin.onunload();
      if (action === "replace query") inputText(modal, "fern");
      if (action === "switch profile") {
        harness.displaySettings?.();
        change = harness.settingRows.find((row) => row.name === "Search backend")?.onChange?.("in_plugin");
        expect(change).toBeDefined();
      }
      expect(signal?.aborted).toBe(true);
      await settled();
      await change;
      expect(openedByHost).toBe(false);
      expect(harness.pathLookups).toEqual([]);
      expect(modal.contentEl.text).not.toContain(PATH);
      if (action !== "unload") plugin.onunload();
    },
  );

  it("an open the host already handed off completes, and a later abort changes nothing", async () => {
    const host = scriptedHost();
    const handedOff = deferred<unknown>();
    host.open.mockReturnValueOnce(handedOff.promise);
    const plugin = await startedHost();
    const modal = await openHost();
    submit(modal, QUERY);
    await settled();
    modal.contentEl.find("button", HIT.title).fire("click");
    // The host ignores aborts once it has begun opening; the result still arrives.
    handedOff.resolve(undefined);
    await settled();
    expect(modal.contentEl.text).toBe("");
    expect(host.open).toHaveBeenCalledTimes(1);
    plugin.onunload();
  });

  it.each([
    [{ code: "result_unavailable", family: "invalidated" }, "That note is no longer available.", 1],
    [{ code: "unavailable", family: "unavailable" }, "Host search is unavailable right now.", 0],
  ])("reports a refused open %j without opening anything locally", async (failure, message, rowsLeft) => {
    const host = scriptedHost();
    host.search.mockResolvedValueOnce(page({ items: [HIT, hitTwin], next: CURSOR }));
    host.open.mockRejectedValueOnce(Object.assign(new Error(RAW), failure));
    const plugin = await startedHost();
    const modal = await openHost();
    submit(modal, QUERY);
    await settled();
    modal.contentEl.all("li")[0]?.find("button").fire("click");
    await settled();
    expect(modal.contentEl.find("p", message)).toBeDefined();
    expect(modal.contentEl.all("li")).toHaveLength(rowsLeft);
    expect(modal.contentEl.text).not.toContain(RAW);
    expect(harness.pathLookups).toEqual([]);
    expect(harness.openedPaths).toEqual([]);
    plugin.onunload();
  });

  it("an ordinary access denial clears only the modal that received it", async () => {
    const host = scriptedHost();
    const resolution = deferred<unknown>();
    host.open.mockReturnValueOnce(resolution.promise);
    host.search.mockResolvedValueOnce(page({ next: CURSOR }))
      .mockResolvedValueOnce(page({ room: secondRoom, items: [hitTwin], next: "opaque-other-page" }))
      .mockRejectedValueOnce({ code: "access_denied", family: "access_denied", message: RAW });
    const plugin = await startedHost();
    const one = await openHost();
    const two = await openHost();
    submit(one, QUERY);
    submit(two, "lark");
    await settled();
    two.contentEl.find("button", hitTwin.title).fire("click");
    const signal = host.open.mock.calls[0]?.[1].signal;
    one.contentEl.find("button", "Next page").fire("click");
    await settled();
    expect(one.contentEl.all("li")).toHaveLength(0);
    expect(one.contentEl.find("p", "You no longer have access to these results.")).toBeDefined();
    // The sibling keeps its rows and its open in flight.
    expect(signal?.aborted).toBe(false);
    expect(two.contentEl.text).toContain(hitTwin.path);
    resolution.resolve(undefined);
    await settled();
    expect(two.contentEl.text).toBe("");
    plugin.onunload();
  });

  it.each([
    ["access", "You no longer have access to these results.", 2],
    ["workspace", "The workspace changed. Search again.", 2],
    ["lifecycle", "These results expired. Search again.", 2],
    ["withdrawn", "Host search is no longer offered here.", 0],
    ["a-future-reason", "These results expired. Search again.", 0],
  ])("an invalidation (%s) clears every open modal and aborts calls in flight", async (reason, message, listenersAfter) => {
    const host = scriptedHost();
    const waiting = deferred<unknown>();
    host.search.mockResolvedValueOnce(page({ next: CURSOR })).mockReturnValueOnce(waiting.promise);
    const plugin = await startedHost();
    const one = await openHost();
    const two = await openHost();
    // Each modal registers its listener when it opens, before the first page.
    expect(host.listeners.size).toBe(2);
    expect(host.search).not.toHaveBeenCalled();
    submit(one, QUERY);
    await settled();
    submit(two, "lark");
    const signal = host.search.mock.calls[1]?.[1].signal;
    host.invalidate(reason);
    expect(signal?.aborted).toBe(true);
    waiting.resolve(page({ items: [hitTwin] }));
    await settled();
    for (const modal of [one, two]) {
      expect(modal.contentEl.all("li")).toHaveLength(0);
      expect(modal.contentEl.all("h3")[0]?.textContent).toBe("");
      expect(modal.contentEl.text).not.toContain(NOTICE);
      expect(modal.contentEl.find("button", "Next page").hidden).toBe(true);
      expect(modal.contentEl.find("p", message)).toBeDefined();
    }
    // A withdrawn or unknown invalidation releases the service object.
    expect(host.listeners.size).toBe(listenersAfter);
    one.close();
    two.close();
    expect(host.listeners.size).toBe(0);
    plugin.onunload();
  });

  it("nothing is saved from presence or absence across two launches", async () => {
    scriptedHost();
    harness.desktopApp = false;
    harness.exactSettings = true;
    const first = await startedHost();
    first.onunload();
    expect(harness.savedData).toEqual([]);
    harness.storedData = null;
    removeSlot();
    const KwiryPlugin = await loadProductionPlugin();
    const second = new KwiryPlugin();
    await second.onload();
    expect(second.settings.backendProfile).toBe("in_plugin");
    harness.layoutReady?.();
    await vi.waitFor(() => expect(harness.statusBarText).toBe("Kwiry: Ready"), { interval: 5 });
    expect({ saved: harness.savedData, timers: clock.set, inits: harness.backendInitializations })
      .toEqual({ saved: [], timers: [], inits: 1 });
    second.onunload();
  });

  it("an automatic choice saved alongside other settings is stored as automatic and chosen again next start", async () => {
    scriptedHost();
    harness.desktopApp = false;
    harness.exactSettings = true;
    const first = await startedHost();
    first.settings.resultLimit = 37;
    await first.saveSettings();
    first.onunload();
    expect(storedProfiles()).toEqual(["automatic"]);
    harness.storedData = harness.savedData[0] as Record<string, unknown>;
    removeSlot();
    const KwiryPlugin = await loadProductionPlugin();
    const second = new KwiryPlugin();
    await second.onload();
    expect({ profile: second.settings.backendProfile, limit: second.settings.resultLimit })
      .toEqual({ profile: "in_plugin", limit: 37 });
    second.onunload();
  });

  it("treats a bare host record saved by an earlier build as automatic", async () => {
    harness.desktopApp = false;
    harness.exactSettings = true;
    harness.storedData = { backendProfile: "host" };
    const KwiryPlugin = await loadProductionPlugin();
    const plugin = new KwiryPlugin();
    await plugin.onload();
    expect(plugin.settings.backendProfile).toBe("in_plugin");
    plugin.onunload();
  });

  it("an explicit choice is still saved and kept when the host is gone", async () => {
    harness.desktopApp = false;
    harness.exactSettings = true;
    harness.storedData = null;
    const host = scriptedHost();
    const KwiryPlugin = await loadProductionPlugin();
    const first = new KwiryPlugin();
    await first.onload();
    harness.layoutReady?.();
    await vi.waitFor(() => expect(harness.statusBarText).toBe(`Kwiry: ${HOST_LABEL}`), { interval: 5 });
    await first.changeBackendProfile("in_plugin");
    await first.changeBackendProfile("host");
    expect(storedProfiles()).toEqual(["in_plugin", "host"]);
    first.onunload();
    harness.storedData = harness.savedData.at(-1) as Record<string, unknown>;
    removeSlot();
    const second = new KwiryPlugin();
    await second.onload();
    harness.layoutReady?.();
    await vi.waitFor(() => expect(harness.statusBarText).toBe(`Kwiry: ${NO_HOST}`), { interval: 5 });
    expect(second.settings.backendProfile).toBe("host");
    expect(harness.backendInitializations).toBe(1);
    expect(host.search).not.toHaveBeenCalled();
    second.onunload();
  });

  it("records one supported-host startup without reporting index readiness", async () => {
    scriptedHost();
    const plugin = await startedHost();
    await plugin.refreshStatus();
    await plugin.copyDiagnostics();
    const records = harness.clipboardWrites.join("\n").split("\n").filter((line) => line.includes("startup.lifecycle"));
    expect(records).toHaveLength(1);
    expect(records[0]).toContain("profile=host");
    expect(records[0]).toContain("outcome=succeeded");
    expect(records[0]).toContain("reason=host_search_supported");
    expect(records[0]).not.toMatch(/fullyCurrentMs=\d/u);
    plugin.onunload();
  });

  it.each([true, false])("waits for persistence before activating a replacement (save succeeds=%s)", async (succeeds) => {
    const host = scriptedHost();
    const resolution = deferred<unknown>();
    host.open.mockReturnValueOnce(resolution.promise);
    const plugin = await startedHost();
    const modal = await openHost();
    submit(modal, QUERY);
    await settled();
    modal.contentEl.find("button", HIT.title).fire("click");
    const gate = deferred<void>();
    harness.beforeSave = async () => {
      await gate.promise;
      if (!succeeds) throw new Error(RAW);
    };
    harness.displaySettings?.();
    const switchProfile = harness.settingRows.find((row) => row.name === "Search backend")?.onChange;
    if (!switchProfile) throw new Error("missing profile control");
    const changing = switchProfile("in_plugin");
    const outcome = changing.catch(() => "rejected");
    expect(host.open.mock.calls[0]?.[1].signal.aborted).toBe(true);
    await settled();
    const initializedWhileSaving = harness.backendInitializations;
    harness.layoutReady?.();
    await settled();
    const cacheWhileSaving = harness.cachePolicyHashes.length;
    gate.resolve();
    expect(await outcome).toBeUndefined();
    expect({ initializedWhileSaving, cacheWhileSaving }).toEqual({ initializedWhileSaving: 0, cacheWhileSaving: 0 });
    expect(harness.backendInitializations).toBe(succeeds ? 1 : 0);
    expect(plugin.settings.backendProfile).toBe(succeeds ? "in_plugin" : "host");
    if (!succeeds) expect(harness.notices.at(-1)).toContain("Could not save the search backend selection");
    expect(harness.notices.join(" ")).not.toContain(RAW);
    resolution.resolve(undefined);
    await settled();
    expect(harness.pathLookups).toEqual([]);
    plugin.onunload();
  });

  it.each([true, false])("final regression: locks the dropdown until the saved profile is shown (save succeeds=%s)", async (succeeds) => {
    scriptedHost();
    const KwiryPlugin = await loadProductionPlugin();
    const plugin = new KwiryPlugin();
    await plugin.onload();
    harness.layoutReady?.();
    await vi.waitFor(() => expect(harness.backendInitializations).toBe(1));
    const gate = deferred<void>();
    harness.beforeSave = async () => {
      await gate.promise;
      if (!succeeds) throw new Error(RAW);
    };
    const control = () => harness.settingRows.find((row) => row.name === "Search backend");
    const choose = async (value: string) => {
      const row = control();
      if (!row || row.disabled) return;
      row.value = value;
      await row.onChange?.(value);
    };
    harness.displaySettings?.();
    const switching = choose("host");
    const latched = control()?.disabled === true;
    harness.displaySettings?.();
    const latchedAfterRedraw = control()?.disabled === true;
    const attempted = choose("in_plugin");
    gate.resolve();
    await Promise.all([switching, attempted]);
    const selected = succeeds ? "host" : "in_plugin";
    expect({ latched, latchedAfterRedraw, disabled: control()?.disabled, displayed: control()?.value,
      runtime: plugin.settings.backendProfile, writes: storedProfiles(),
    }).toEqual({ latched: true, latchedAfterRedraw: true, disabled: false, displayed: selected,
      runtime: selected, writes: succeeds ? ["host"] : [] });
    if (succeeds) await openHost();
    else await expect(plugin.api.ready()).resolves.toBeUndefined();
    plugin.onunload();
  });

  it("shows neutral host startup copy before layout readiness", async () => {
    const host = scriptedHost();
    harness.storedData = null;
    const KwiryPlugin = await loadProductionPlugin();
    const plugin = new KwiryPlugin();
    await plugin.onload();
    harness.commands.get("open-search")?.();
    await settled();
    expect(harness.notices.at(-1)).toBe("Kwiry: Host search is starting…");
    expect(host.search).not.toHaveBeenCalled();
    plugin.onunload();
  });

  it("keeps two modals independent and never saves, logs or exports host search data", async () => {
    const host = scriptedHost();
    const waiting = deferred<unknown>();
    host.search.mockReturnValueOnce(waiting.promise).mockResolvedValueOnce(page({ next: CURSOR }))
      .mockRejectedValueOnce({ code: "access_denied", family: "access_denied", message: RAW });
    const consoleCalls = ["debug", "info", "log", "warn", "error"].map((method) =>
      vi.spyOn(console, method as "log").mockImplementation(() => undefined));
    const plugin = await startedHost();
    const one = await openHost();
    const two = await openHost();
    submit(one, "marten");
    submit(two, QUERY);
    await settled();
    expect(two.contentEl.text).toContain(EXCERPT);
    one.close();
    expect(host.search.mock.calls.map(([, { signal }]) => signal.aborted)).toEqual([true, false]);
    waiting.resolve(page({ room: secondRoom, items: [hitTwin] }));
    await settled();
    expect(one.contentEl.text).toBe("");
    expect(two.contentEl.text).toContain(PATH);
    two.contentEl.find("button", "Next page").fire("click");
    await settled();
    expect(two.contentEl.text).not.toContain(PATH);
    await plugin.saveSettings();
    await plugin.copyDiagnostics();
    await plugin.exportDiagnosticsFile();
    harness.desktopExportAvailable = false;
    await plugin.exportDiagnosticsFile();
    expect(storedProfiles()).toEqual(["automatic"]);
    expect(harness.clipboardWrites).toHaveLength(1);
    expect(harness.exportReports).toHaveLength(1);
    expect(harness.fullClipboardReports).toHaveLength(1);
    const emitted = JSON.stringify({
      settings: harness.savedData,
      logs: consoleCalls.flatMap((spy) => spy.mock.calls),
      clipboard: harness.clipboardWrites,
      exports: [...harness.exportReports, ...harness.fullClipboardReports],
    });
    for (const secret of [QUERY, QUERY.trim(), "marten", PATH, REF, CURSOR, EXCERPT, RAW, firstRoom.label]) {
      expect(emitted).not.toContain(secret);
    }
    for (const spy of consoleCalls) spy.mockRestore();
    plugin.onunload();
  });

  describe("late provider and the provider-arrived event", () => {
    async function freshWithoutHost(desktop = false) {
      const host = scriptedHost("late");
      const worker = vi.fn();
      vi.stubGlobal("Worker", worker);
      harness.desktopApp = desktop;
      harness.exactSettings = true;
      harness.storedData = null;
      const KwiryPlugin = await loadProductionPlugin();
      const plugin = new KwiryPlugin();
      await plugin.onload();
      return { host, worker, plugin };
    }

    it.each([true, false])("starts the fallback at layout-ready with no polling and saves nothing (desktop=%s)", async (desktop) => {
      const { host, plugin } = await freshWithoutHost(desktop);
      expect(availableListeners()).toBe(1);
      harness.layoutReady?.();
      if (desktop) {
        await vi.waitFor(() => expect(harness.statusBarText).toBe("Kwiry: Index unavailable"), { interval: 5 });
      } else {
        await vi.waitFor(() => expect(harness.statusBarText).toBe("Kwiry: Ready"), { interval: 5 });
        expect(harness.backendInitializations).toBe(1);
        // No slot: ordinary desktop behaviour, persistent cache included.
        expect(harness.cachePolicyHashes).toHaveLength(1);
      }
      advance(60_000);
      await settled();
      expect({ timers: clock.set, pending: clock.pending, saved: harness.savedData, lookups: host.get.mock.calls.length })
        .toEqual({ timers: [], pending: [], saved: [], lookups: 0 });
      plugin.onunload();
    });

    it("looks at start, listens, and looks again only when the event fires", async () => {
      const { host, plugin } = await freshWithoutHost();
      harness.layoutReady?.();
      await vi.waitFor(() => expect(harness.statusBarText).toBe("Kwiry: Ready"), { interval: 5 });
      expect(host.get).not.toHaveBeenCalled();
      // A spoofed or early event with no slot only causes a fresh look.
      announceProvider();
      await settled();
      expect(plugin.settings.backendProfile).toBe("in_plugin");
      host.install();
      advance(60_000);
      await settled();
      expect(plugin.settings.backendProfile).toBe("in_plugin");
      expect(host.get).not.toHaveBeenCalled();
      announceProvider();
      await vi.waitFor(() => expect(harness.statusBarText).toBe(`Kwiry: ${HOST_LABEL}`), { interval: 5 });
      expect(plugin.settings.backendProfile).toBe("host");
      // Switching to host search stops the local index; nothing is saved.
      expect(harness.backendDisposals).toBe(1);
      expect({ saved: harness.savedData, timers: clock.set }).toEqual({ saved: [], timers: [] });
      const modal = await openHost();
      submit(modal, QUERY);
      await settled();
      expect(host.search.mock.calls).toEqual([[{ query: QUERY }, { signal: expect.any(AbortSignal) }]]);
      plugin.onunload();
    });

    it("an event before layout-ready selects host search for the first activation", async () => {
      const { host, plugin } = await freshWithoutHost();
      host.install();
      announceProvider();
      expect(plugin.settings.backendProfile).toBe("host");
      harness.layoutReady?.();
      await vi.waitFor(() => expect(harness.statusBarText).toBe(`Kwiry: ${HOST_LABEL}`), { interval: 5 });
      expect({ inits: harness.backendInitializations, saved: harness.savedData }).toEqual({ inits: 0, saved: [] });
      plugin.onunload();
    });

    it("removes its listener on unload; a later event does nothing", async () => {
      const { host, plugin } = await freshWithoutHost();
      harness.layoutReady?.();
      await vi.waitFor(() => expect(harness.statusBarText).toBe("Kwiry: Ready"), { interval: 5 });
      plugin.onunload();
      expect(availableListeners()).toBe(0);
      host.install();
      announceProvider();
      await settled();
      expect({ lookups: host.get.mock.calls.length, saved: harness.savedData }).toEqual({ lookups: 0, saved: [] });
    });

    it.each(["daemon", "in_plugin", "legacy"] as const)("never overrides a saved %s choice when a provider arrives", async (stored) => {
      const host = scriptedHost("late");
      harness.desktopApp = false;
      harness.exactSettings = true;
      harness.storedData = stored === "legacy" ? { tokenFilePath: "/synthetic/token-file" } : { backendProfile: stored };
      const KwiryPlugin = await loadProductionPlugin();
      const plugin = new KwiryPlugin();
      await plugin.onload();
      harness.layoutReady?.();
      await vi.waitFor(() => expect(harness.statusBarText).not.toBe("kwiry: starting…"), { interval: 5 });
      host.install();
      announceProvider();
      await settled();
      expect({ timers: clock.set, saved: harness.savedData, profile: plugin.settings.backendProfile })
        .toEqual({ timers: [], saved: [], profile: stored === "legacy" ? "daemon" : stored });
      plugin.onunload();
    });

    it("a saved host choice with no provider shows the no-host wording and switches to available on the event", async () => {
      const host = scriptedHost("late");
      harness.desktopApp = false;
      harness.exactSettings = true;
      harness.storedData = { backendProfile: "host", resultLimit: 20 };
      const KwiryPlugin = await loadProductionPlugin();
      const plugin = new KwiryPlugin();
      await plugin.onload();
      harness.layoutReady?.();
      await vi.waitFor(() => expect(harness.statusBarText).toBe(`Kwiry: ${NO_HOST}`), { interval: 5 });
      const modal = await openHost();
      submit(modal, QUERY);
      await settled();
      expect(modal.contentEl.text).toContain(NO_HOST);
      host.install();
      announceProvider();
      await vi.waitFor(() => expect(harness.statusBarText).toBe(`Kwiry: ${HOST_LABEL}`), { interval: 5 });
      // The open modal looks again at its next use.
      submit(modal, QUERY);
      await settled();
      expect(host.search).toHaveBeenCalledTimes(1);
      expect(harness.backendInitializations).toBe(0);
      plugin.onunload();
    });

    it("withdrawal never switches to local computation", async () => {
      const { host, plugin } = await freshWithoutHost();
      host.install();
      announceProvider();
      harness.layoutReady?.();
      await vi.waitFor(() => expect(harness.statusBarText).toBe(`Kwiry: ${HOST_LABEL}`), { interval: 5 });
      const modal = await openHost();
      submit(modal, QUERY);
      await settled();
      removeSlot();
      host.invalidate("withdrawn");
      await settled();
      expect(modal.contentEl.find("p", "Host search is no longer offered here.")).toBeDefined();
      expect(host.listeners.size).toBe(0);
      submit(modal, QUERY);
      await settled();
      expect(modal.contentEl.text).toContain(NO_HOST);
      await plugin.refreshStatus();
      expect({ profile: plugin.settings.backendProfile, inits: harness.backendInitializations, status: harness.statusBarText })
        .toEqual({ profile: "host", inits: 0, status: `Kwiry: ${NO_HOST}` });
      modal.close();
      plugin.onunload();
    });
  });

  describe("local indexing policy under a host slot", () => {
    async function inPluginUnder(policy: PolicyScript, kind: "without search" | "late" = "without search") {
      const host = scriptedHost(kind, policy);
      const worker = vi.fn();
      vi.stubGlobal("Worker", worker);
      harness.desktopApp = false;
      harness.exactSettings = true;
      harness.storedData = { backendProfile: "in_plugin" };
      const KwiryPlugin = await loadProductionPlugin();
      const plugin = new KwiryPlugin();
      await plugin.onload();
      harness.layoutReady?.();
      await settled();
      return { host, plugin };
    }

    it.each([
      ["no policy service", "absent" as const, "Kwiry: Ready"],
      ["memory_only", { value: "memory_only", decidedBy: "organization" }, "Kwiry: Ready"],
    ])("%s builds in memory with the persistent cache off", async (_name, policy, status) => {
      const { plugin } = await inPluginUnder(policy);
      await vi.waitFor(() => expect(harness.statusBarText).toBe(status), { interval: 5 });
      expect({ inits: harness.backendInitializations, cache: harness.cachePolicyHashes, saved: harness.savedData })
        .toEqual({ inits: 1, cache: [], saved: [] });
      harness.displaySettings?.();
      expect(harness.settingRows.find(({ name }) => name === "Local indexing")?.description)
        .toMatch(/^Local indexing: memory only \((default|your organization)\)\. Nothing is kept after this session\.$/u);
      plugin.onunload();
    });

    it.each([
      [{ value: "refuse", decidedBy: "organization" }, "Local indexing is turned off by your organization."],
      [{ value: "persistent_custodied", decidedBy: "workspace" }, "Local indexing is turned off by this workspace."],
      [{ value: 7, decidedBy: "galaxy" }, "Local indexing is turned off by policy."],
      [{ value: "memory_only", decidedBy: "deployment", limit: { dimension: "memoryBytes", maximum: 1, origin: "organization" } },
        "Local indexing has a limit set by your organization that this plugin cannot check, so it builds no index."],
    ])("%j never starts the in-plugin backend, also from a saved profile", async (policy, message) => {
      const { plugin } = await inPluginUnder(policy);
      await vi.waitFor(() => expect(harness.statusBarText).toBe(`Kwiry: ${message}`), { interval: 5 });
      harness.commands.get("open-search")?.();
      await plugin.rebuildInPluginIndex();
      await expect(plugin.api.ready()).rejects.toMatchObject({ code: "backend_unavailable" });
      expect({ inits: harness.backendInitializations, cache: harness.cachePolicyHashes, sources: harness.sourcePolicies })
        .toEqual({ inits: 0, cache: [], sources: [] });
      expect(harness.notices).toEqual([`Kwiry: ${message}`, `Kwiry: ${message}`]);
      plugin.onunload();
    });

    it("starts nothing while the policy answer is still unknown", async () => {
      const { plugin } = await inPluginUnder({ value: "memory_only", hang: true });
      await settled();
      expect(harness.statusBarText).toBe("Kwiry: Waiting for the local indexing policy…");
      expect(harness.backendInitializations).toBe(0);
      plugin.onunload();
    });

    it("a change from memory_only to refuse tears down the running backend and its in-memory index", async () => {
      const { host, plugin } = await inPluginUnder({ value: "memory_only" });
      await vi.waitFor(() => expect(harness.backendInitializations).toBe(1), { interval: 5 });
      expect(host.policyListeners.size).toBe(1);
      host.changePolicy({ value: "refuse", decidedBy: "folder" });
      await vi.waitFor(() => expect(harness.backendDisposals).toBe(1), { interval: 5 });
      expect(harness.statusBarText).toBe("Kwiry: Local indexing is turned off by this folder.");
      await expect(plugin.api.ready()).rejects.toMatchObject({ code: "backend_unavailable" });
      host.changePolicy({ value: "memory_only" });
      await vi.waitFor(() => expect(harness.backendInitializations).toBe(2), { interval: 5 });
      expect(harness.cachePolicyHashes).toEqual([]);
      plugin.onunload();
      expect(host.policyListeners.size).toBe(0);
      expect(host.listeners.size).toBe(0);
    });

    it("a late provider on plain desktop restarts a cached in-plugin index without its persistent cache", async () => {
      const { host, plugin } = await inPluginUnder("absent", "late");
      await vi.waitFor(() => expect(harness.backendInitializations).toBe(1), { interval: 5 });
      expect(harness.cachePolicyHashes).toHaveLength(1);
      host.install();
      announceProvider();
      await vi.waitFor(() => expect(harness.backendInitializations).toBe(2), { interval: 5 });
      expect(harness.backendDisposals).toBe(1);
      expect(harness.cachePolicyHashes).toHaveLength(1);
      expect(plugin.settings.backendProfile).toBe("in_plugin");
      plugin.onunload();
    });

    it("review regression: a refuse that arrives while a switch to in-plugin is starting stops it", async () => {
      const host = scriptedHost("without search", { value: "memory_only" });
      harness.desktopApp = false;
      harness.exactSettings = true;
      harness.storedData = { backendProfile: "daemon" };
      const KwiryPlugin = await loadProductionPlugin();
      const plugin = new KwiryPlugin();
      await plugin.onload();
      harness.layoutReady?.();
      await vi.waitFor(() => expect(harness.statusBarText).not.toBe("kwiry: starting…"), { interval: 5 });
      harness.beforeInitialize = () => {
        delete harness.beforeInitialize;
        host.changePolicy({ value: "refuse", decidedBy: "organization" });
      };
      await plugin.changeBackendProfile("in_plugin");
      await settled();
      await settled();
      expect(plugin.settings.backendProfile).toBe("in_plugin");
      expect(harness.statusBarText).toBe("Kwiry: Local indexing is turned off by your organization.");
      expect(harness.backendDisposals).toBe(1);
      await expect(plugin.api.ready()).rejects.toMatchObject({ code: "backend_unavailable" });
      plugin.onunload();
    });

    it("review regression: a refuse during a failed switch away from in-plugin still stops it", async () => {
      const { host, plugin } = await inPluginUnder({ value: "memory_only" });
      await vi.waitFor(() => expect(harness.statusBarText).toBe("Kwiry: Ready"), { interval: 5 });
      harness.beforeSave = async () => {
        host.changePolicy({ value: "refuse", decidedBy: "folder" });
        throw new Error(RAW);
      };
      await plugin.changeBackendProfile("daemon");
      await settled();
      expect(plugin.settings.backendProfile).toBe("in_plugin");
      expect(harness.notices.at(-1)).toContain("Could not save the search backend selection");
      expect(harness.statusBarText).toBe("Kwiry: Local indexing is turned off by this folder.");
      expect(harness.backendDisposals).toBe(1);
      await expect(plugin.api.ready()).rejects.toMatchObject({ code: "backend_unavailable" });
      plugin.onunload();
    });

    it("review regression: a late provider whose policy has not answered stops a cached in-plugin index", async () => {
      const { host, plugin } = await inPluginUnder({ value: "memory_only", hang: true }, "late");
      await vi.waitFor(() => expect(harness.backendInitializations).toBe(1), { interval: 5 });
      expect(harness.cachePolicyHashes).toHaveLength(1);
      host.install();
      announceProvider();
      await vi.waitFor(() => expect(harness.backendDisposals).toBe(1), { interval: 5 });
      await settled();
      expect(harness.statusBarText).toBe("Kwiry: Waiting for the local indexing policy…");
      expect(harness.backendInitializations).toBe(1);
      await expect(plugin.api.ready()).rejects.toMatchObject({ code: "backend_unavailable" });
      plugin.onunload();
    });

    it("review regression: withdrawal while the policy answer is pending applies the memory-only default", async () => {
      const { host, plugin } = await inPluginUnder({ value: "memory_only", hang: true });
      await settled();
      expect(harness.statusBarText).toBe("Kwiry: Waiting for the local indexing policy…");
      removeSlot();
      host.invalidate("withdrawn");
      await vi.waitFor(() => expect(harness.statusBarText).toBe("Kwiry: Ready"), { interval: 5 });
      expect({ inits: harness.backendInitializations, cache: harness.cachePolicyHashes }).toEqual({ inits: 1, cache: [] });
      await expect(plugin.api.ready()).resolves.toBeUndefined();
      plugin.onunload();
    });
  });

  describe("daemon connection settings", () => {
    it.each(["Token file path", "Current vault daemon ID", "Search vault ID filter"])(
      "review regression: configuring %s on a fresh install records the daemon choice", async (rowName) => {
        harness.desktopApp = true;
        harness.exactSettings = true;
        harness.storedData = null;
        const KwiryPlugin = await loadProductionPlugin();
        const first = new KwiryPlugin();
        await first.onload();
        expect(first.settings.backendProfile).toBe("daemon");
        harness.layoutReady?.();
        harness.displaySettings?.();
        const row = harness.settingRows.find(({ name }) => name === rowName);
        expect(row?.onChange).toBeTypeOf("function");
        await row?.onChange?.("/synthetic/daemon-value");
        expect(storedProfiles().at(-1)).toBe("daemon");
        // A provider arriving later in the same session does not switch a configured daemon.
        const host = scriptedHost("late");
        host.install();
        announceProvider();
        await settled();
        expect(first.settings.backendProfile).toBe("daemon");
        first.onunload();
        harness.storedData = harness.savedData.at(-1) as Record<string, unknown>;
        const second = new KwiryPlugin();
        await second.onload();
        expect(second.settings.backendProfile).toBe("daemon");
        second.onunload();
      },
    );
  });

  it("keeps the fresh desktop daemon default, settings controls, and status", async () => {
    harness.storedData = null;
    harness.adapterBasePath = null;
    const KwiryPlugin = await loadProductionPlugin();
    const plugin = new KwiryPlugin();
    await plugin.onload();
    expect(plugin.settings.backendProfile).toBe("daemon");
    harness.layoutReady?.();
    expect(clock.set).toEqual([]);
    harness.displaySettings?.();
    expect(harness.settingRows.find(({ name }) => name === "Search backend")).toMatchObject({
      value: "daemon",
      description: "Backend selection is explicit. A daemon failure never activates in-plugin search.",
      options: {
        daemon: { label: "Daemon", disabled: false },
        in_plugin: { label: "In-plugin · Lexical", disabled: false },
      },
    });
    expect(harness.settingRows).toContainEqual(expect.objectContaining({ name: "Daemon URL" }));
    await vi.waitFor(() => expect(harness.statusBarText).toBe("Kwiry: Index unavailable"));
    expect(harness.savedData).toEqual([]);
    plugin.onunload();
  });

  it.each(["desktopApp", "nodeFilesystemAvailable", "desktopTransportAvailable"] as const)(
    "starts in-plugin indexing on a fresh install without %s",
    async (missingCapability) => {
      harness[missingCapability] = false;
      harness.storedData = null;
      const KwiryPlugin = await loadProductionPlugin();
      const plugin = new KwiryPlugin();
      await plugin.onload();
      expect(plugin.settings.backendProfile).toBe("in_plugin");
      harness.layoutReady?.();
      expect(clock.set).toEqual([]);
      await vi.waitFor(() => expect(harness.statusBarText).toBe("Kwiry: Ready"));
      expect(harness.backendInitializations).toBe(1);
      expect(harness.savedData).toEqual([]);
      plugin.onunload();
    },
  );

  it("keeps an unavailable saved daemon selection until the user switches in settings", async () => {
    harness.desktopApp = false;
    harness.nodeFilesystemAvailable = false;
    harness.storedData = { backendProfile: "daemon", daemonUrl: "invalid-url" };
    const KwiryPlugin = await loadProductionPlugin();
    const plugin = new KwiryPlugin();
    await plugin.onload();
    harness.layoutReady?.();
    const guidance = "The daemon backend is not available in this host. "
      + "Choose In-plugin · Lexical to search without a daemon.";
    await vi.waitFor(() => expect(harness.statusBarText).toBe(`Kwiry: ${guidance}`));
    expect(plugin.settings.backendProfile).toBe("daemon");
    expect(harness.backendInitializations).toBe(0);
    expect(harness.savedData).toEqual([]);

    harness.displaySettings?.();
    const backendRow = harness.settingRows.find(({ name }) => name === "Search backend");
    expect(backendRow).toMatchObject({
      value: "daemon",
      description: guidance,
      options: {
        daemon: { label: "Daemon (unavailable)", disabled: true },
        in_plugin: { label: "In-plugin · Lexical", disabled: false },
      },
    });
    await backendRow?.onChange?.("in_plugin");
    expect(plugin.settings.backendProfile).toBe("in_plugin");
    expect(harness.savedData).toEqual([expect.objectContaining({ backendProfile: "in_plugin" })]);
    expect(harness.backendInitializations).toBe(1);
    expect(harness.statusBarText).toBe("Kwiry: Ready");
    expect(harness.settingRows.find(({ name }) => name === "Search backend")).toMatchObject({
      value: "in_plugin",
      options: { daemon: { disabled: true } },
    });
    plugin.onunload();
  });

  it("publishes the API on the plugin and revokes cached references synchronously on unload", async () => {
    const KwiryPlugin = await loadProductionPlugin();
    const plugin = new KwiryPlugin();
    const api = plugin.api;
    expect(api.version).toBe(1);
    await expect(api.ready()).rejects.toMatchObject({ code: "backend_unavailable" });
    await plugin.onload();
    await expect(api.ready()).rejects.toMatchObject({ code: "backend_unavailable" });
    harness.layoutReady?.();
    await vi.waitFor(() => expect(harness.backendInitializations).toBe(1));
    await expect(api.ready()).resolves.toBeUndefined();
    await expect(api.search("pear")).resolves.toEqual([
      { path: "Notes/Orchard.md", score: 2, heading: "Pears" },
    ]);
    expect(plugin.api).toBe(api);
    expect(harness.backendInitializations).toBe(1);
    expect(harness.savedData).toEqual([]);
    plugin.onunload();
    await expect(api.ready()).rejects.toMatchObject({ name: "KwiryApiError", code: "plugin_unloaded" });
    await expect(api.search("pear")).rejects.toMatchObject({ code: "plugin_unloaded" });
  });

  it("hover preview: registers the native source with Mod required by default", async () => {
    const KwiryPlugin = await loadProductionPlugin();
    const plugin = new KwiryPlugin();
    await plugin.onload();
    expect(harness.hoverSources).toEqual([{ id: "kwiry-search", display: "Kwiry Search", defaultMod: true }]);
    expect(Reflect.get(plugin, "hoverPreviewSource")).toBe("kwiry-search");
  });

  it("hover preview: missing public registration disables previews, not startup", async () => {
    const KwiryPlugin = await loadProductionPlugin();
    const plugin = new KwiryPlugin();
    Reflect.set(plugin, "registerHoverLinkSource", undefined);
    await plugin.onload();
    expect(harness.hoverSources).toEqual([]);
    expect(Reflect.get(plugin, "hoverPreviewSource")).toBeNull();
    expect(harness.layoutReady).toEqual(expect.any(Function));
    expect(harness.notices).toEqual([]);
  });

  it("hover preview: backend replacement notifies promptly and callback disposal is effective", async () => {
    const KwiryPlugin = await loadProductionPlugin();
    const plugin = new KwiryPlugin();
    await plugin.onload();
    harness.layoutReady?.();
    await vi.waitFor(() => expect(harness.backendInitializations).toBe(1));
    const register = Reflect.get(plugin, "onBackendInvalidated") as (callback: () => void) => () => void;
    expect(register).toEqual(expect.any(Function));
    const invalidated = vi.fn();
    const dispose = register.call(plugin, invalidated);
    await plugin.onSourcePolicyChanged();
    expect(invalidated).toHaveBeenCalled();
    dispose();
    invalidated.mockClear();
    await plugin.onSourcePolicyChanged();
    expect(invalidated).not.toHaveBeenCalled();
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

  it("treats a status poll before layout-ready activation as starting, not as a failure", async () => {
    const KwiryPlugin = await loadProductionPlugin();
    const plugin = new KwiryPlugin();
    await plugin.onload();
    expect(harness.backendInitializations).toBe(0);

    // A slow layout on a large vault lets the 30 s status interval fire before
    // any backend is bound. The field report logged that tick as an
    // internal_error; nothing had failed.
    const refreshStatus = Reflect.get(plugin, "refreshStatus") as () => Promise<void>;
    await refreshStatus.call(plugin);
    expect(harness.statusBarText).toBe("kwiry: starting…");

    await plugin.copyDiagnostics();
    const report = harness.clipboardWrites.at(-1) ?? "";
    expect(report).not.toMatch(/failure\.caught[^\n]*operation=poll/u);

    // Once a backend is bound, a genuine poll failure is still reported.
    harness.layoutReady?.();
    await vi.waitFor(() => expect(harness.backendInitializations).toBe(1));
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
