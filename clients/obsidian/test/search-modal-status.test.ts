// SPDX-FileCopyrightText: 2026 cybersader
// SPDX-License-Identifier: GPL-3.0-only

import { createRequire } from "node:module";
import { fileURLToPath } from "node:url";

import { build, type Plugin as EsbuildPlugin } from "esbuild";
import { afterEach, beforeAll, beforeEach, describe, expect, it, vi } from "vitest";

import type { SearchRequest } from "../src/api";
import {
  type BackendIdentity,
  type BackendSearchHit,
  type BackendStatus,
  type SearchBackend,
  type SearchExecution,
} from "../src/backend";

interface FakeClassList {
  contains(name: string): boolean;
}

class FakeClassListImpl implements FakeClassList {
  private readonly values = new Set<string>();

  add(...names: string[]): void {
    for (const name of names) this.values.add(name);
  }

  remove(...names: string[]): void {
    for (const name of names) this.values.delete(name);
  }

  toggle(name: string, force?: boolean): boolean {
    const enabled = force ?? !this.values.has(name);
    if (enabled) this.values.add(name);
    else this.values.delete(name);
    return enabled;
  }

  contains(name: string): boolean {
    return this.values.has(name);
  }
}

class FakeElement {
  readonly attributes = new Map<string, string>();
  readonly classList = new FakeClassListImpl();
  readonly children: FakeElement[] = [];
  parent: FakeElement | null = null;
  textContent = "";
  textSetCount = 0;
  value = "";
  focusCount = 0;
  readonly dispatchedEvents: Event[] = [];
  private readonly listeners = new Map<string, Array<(event: Event) => void>>();

  constructor(className?: string) {
    if (className) this.classList.add(...className.split(/\s+/u));
  }

  createDiv(options: { cls?: string; text?: string } = {}): FakeElement {
    return this.append(new FakeElement(options.cls), options.text);
  }

  createSpan(options: { cls?: string; text?: string } = {}): FakeElement {
    return this.append(new FakeElement(options.cls), options.text);
  }

  createEl(
    _tag: string,
    options: { cls?: string; text?: string; attr?: Record<string, string> } = {},
  ): FakeElement {
    const child = this.append(new FakeElement(options.cls), options.text);
    for (const [name, value] of Object.entries(options.attr ?? {})) {
      child.setAttribute(name, value);
    }
    return child;
  }

  private append(child: FakeElement, text?: string): FakeElement {
    child.parent = this;
    this.children.push(child);
    if (text !== undefined) child.setText(text);
    return child;
  }

  before(child: FakeElement): void {
    if (!this.parent) return;
    if (child.parent) {
      const previousIndex = child.parent.children.indexOf(child);
      if (previousIndex >= 0) child.parent.children.splice(previousIndex, 1);
    }
    const index = this.parent.children.indexOf(this);
    child.parent = this.parent;
    this.parent.children.splice(index < 0 ? this.parent.children.length : index, 0, child);
  }

  setText(text: string): void {
    this.textContent = text;
    this.textSetCount += 1;
  }

  empty(): void {
    this.children.splice(0);
    this.textContent = "";
  }

  setAttribute(name: string, value: string): void {
    this.attributes.set(name, value);
  }

  addClass(name: string): void {
    this.classList.add(name);
  }

  toggleClass(name: string, force: boolean): void {
    this.classList.toggle(name, force);
  }

  appendText(text: string): void {
    this.textContent += text;
  }

  addEventListener(type: string, listener: (event: Event) => void): void {
    const listeners = this.listeners.get(type) ?? [];
    listeners.push(listener);
    this.listeners.set(type, listeners);
  }

  focus(): void {
    this.focusCount += 1;
  }

  dispatchEvent(event: Event): boolean {
    this.dispatchedEvents.push(event);
    for (const listener of this.listeners.get(event.type) ?? []) listener(event);
    return true;
  }
}

interface FakeScopeRegistration {
  modifiers: string[];
  key: string;
  callback: (event: KeyboardEvent) => boolean;
}

class FakeSuggestModal<T> {
  readonly app: unknown;
  readonly contentEl = new FakeElement("modal-content");
  readonly resultContainerEl = this.contentEl.createDiv({ cls: "suggestion-container" });
  readonly inputEl = new FakeElement("prompt-input");
  readonly scopeRegistrations: FakeScopeRegistration[] = [];
  readonly scope = {
    register: (
      modifiers: string[],
      key: string,
      callback: (event: KeyboardEvent) => boolean,
    ): void => {
      this.scopeRegistrations.push({ modifiers, key, callback });
    },
  };
  suggestions: T[] = [];
  activeIndex = 0;
  closed = false;
  emptyStateText = "";
  private suggestionUpdate: Promise<void> = Promise.resolve();

  constructor(app: unknown) {
    this.app = app;
    this.inputEl.addEventListener("input", () => {
      this.suggestionUpdate = Promise.resolve(this.getSuggestions(this.inputEl.value)).then(
        (suggestions) => {
          this.suggestions = suggestions;
          this.activeIndex = 0;
          this.resultContainerEl.children.splice(0);
          for (const suggestion of suggestions) {
            const row = this.resultContainerEl.createDiv();
            this.renderSuggestion(suggestion, row as unknown as HTMLElement);
          }
        },
      );
    });
    this.inputEl.addEventListener("keydown", (event) => {
      const key = (event as KeyboardEvent).key;
      if (key === "ArrowDown") {
        this.activeIndex = Math.min(this.activeIndex + 1, Math.max(0, this.suggestions.length - 1));
      } else if (key === "ArrowUp") {
        this.activeIndex = Math.max(0, this.activeIndex - 1);
      }
    });
  }

  setPlaceholder(): void {}
  setInstructions(): void {}

  getSuggestions(_query: string): T[] | Promise<T[]> {
    throw new Error("FakeSuggestModal.getSuggestions must be overridden");
  }

  renderSuggestion(_result: T, _el: HTMLElement): void {
    throw new Error("FakeSuggestModal.renderSuggestion must be overridden");
  }

  onChooseSuggestion(_result: T, _event: MouseEvent | KeyboardEvent): void {
    throw new Error("FakeSuggestModal.onChooseSuggestion must be overridden");
  }

  selectSuggestion(result: T, event: MouseEvent | KeyboardEvent): void {
    this.close();
    this.onChooseSuggestion(result, event);
  }

  close(): void {
    this.closed = true;
    this.onClose();
  }

  selectActiveSuggestion(event: MouseEvent | KeyboardEvent): void {
    const active = this.suggestions[this.activeIndex];
    if (active !== undefined) this.selectSuggestion(active, event);
  }

  async flushSuggestions(): Promise<void> {
    await this.suggestionUpdate;
  }

  triggerScope(
    modifiers: string[],
    key: string,
    event: KeyboardEvent,
  ): boolean {
    const registration = this.scopeRegistrations.find((candidate) =>
      candidate.key.toLowerCase() === key.toLowerCase()
      && candidate.modifiers.join("+") === modifiers.join("+"));
    if (!registration) throw new Error(`Missing scope registration for ${modifiers.join("+")} ${key}`);
    return registration.callback(event);
  }

  onClose(): void {
    this.closed = true;
  }
}

const notices: string[] = [];
class FakeNotice {
  constructor(message: string) {
    notices.push(message);
  }
}
class FakeTFile {
  constructor(readonly path: string) {}
}

class FakeKeyboardEvent extends Event {
  readonly key: string;
  readonly ctrlKey: boolean;
  readonly metaKey: boolean;
  readonly altKey: boolean;
  readonly shiftKey: boolean;

  constructor(type: string, init: KeyboardEventInit = {}) {
    super(type, init);
    this.key = init.key ?? "";
    this.ctrlKey = init.ctrlKey ?? false;
    this.metaKey = init.metaKey ?? false;
    this.altKey = init.altKey ?? false;
    this.shiftKey = init.shiftKey ?? false;
  }
}

interface SearchModalLike {
  readonly contentEl: FakeElement;
  readonly inputEl: FakeElement;
  readonly resultContainerEl: FakeElement;
  readonly suggestions: unknown[];
  readonly activeIndex: number;
  readonly closed: boolean;
  getSuggestions(query: string): Promise<unknown[]>;
  renderSuggestion(result: unknown, el: FakeElement): void;
  selectSuggestion(result: unknown, event: MouseEvent | KeyboardEvent): void;
  flushSuggestions(): Promise<void>;
  triggerScope(modifiers: string[], key: string, event: KeyboardEvent): boolean;
  onClose(): void;
}

interface SearchModalModule {
  KwirySearchModal: new (
    plugin: unknown,
    backend: SearchBackend,
    status: BackendStatus,
  ) => SearchModalLike;
  KwiryBackendError: new (
    code: string,
    profile: "daemon" | "in_plugin",
    stage: "configuration" | "transport" | "protocol" | "index" | "query" | "lifecycle",
    retryable: boolean,
    safeMessage: string,
  ) => Error;
  SEARCH_STATUS_ANIMATION_DELAY_MS: number;
  FORMAT_CHIP_PRESENTATIONS: Record<string, { label: string; accessibleLabel: string }>;
}

const SEARCH_MODAL_PATH = fileURLToPath(new URL("../src/search-modal.ts", import.meta.url));
const BACKEND_PATH = fileURLToPath(new URL("../src/backend.ts", import.meta.url));
const require = createRequire(import.meta.url);
let searchModalModule: SearchModalModule;

beforeAll(async () => {
  Object.assign(globalThis, {
    __kwirySearchModalHarness: {
      SuggestModal: FakeSuggestModal,
      Notice: FakeNotice,
      TFile: FakeTFile,
      Platform: { isMacOS: false },
    },
  });
  searchModalModule = await loadSearchModal();
});

async function loadSearchModal(): Promise<SearchModalModule> {
  const bundle = await build({
    stdin: {
      contents: `
        export * from ${JSON.stringify(SEARCH_MODAL_PATH)};
        export { KwiryBackendError } from ${JSON.stringify(BACKEND_PATH)};
      `,
      resolveDir: "/",
      sourcefile: "search-modal-status-entry.ts",
      loader: "ts",
    },
    bundle: true,
    format: "cjs",
    platform: "node",
    target: "node22",
    write: false,
    logLevel: "silent",
    plugins: [obsidianStubPlugin()],
  });
  const output = bundle.outputFiles[0];
  if (!output) throw new Error("search-modal.ts test bundle emitted no output");
  const module = { exports: {} as Record<string, unknown> };
  const evaluate = new Function("module", "exports", "require", output.text);
  evaluate(module, module.exports, require);
  return module.exports as unknown as SearchModalModule;
}

function obsidianStubPlugin(): EsbuildPlugin {
  return {
    name: "kwiry-search-modal-status-harness",
    setup(pluginBuild) {
      pluginBuild.onResolve({ filter: /^obsidian$/ }, () => ({
        path: "obsidian",
        namespace: "kwiry-search-modal-status-harness",
      }));
      pluginBuild.onLoad(
        { filter: /.*/, namespace: "kwiry-search-modal-status-harness" },
        () => ({
          loader: "js",
          contents: `
            const harness = globalThis.__kwirySearchModalHarness;
            export const SuggestModal = harness.SuggestModal;
            export const Notice = harness.Notice;
            export const TFile = harness.TFile;
            export const Platform = harness.Platform;
          `,
        }),
      );
    },
  };
}

interface Deferred<T> {
  promise: Promise<T>;
  resolve(value: T): void;
  reject(error: unknown): void;
}

function deferred<T>(): Deferred<T> {
  let resolve!: (value: T) => void;
  let reject!: (error: unknown) => void;
  const promise = new Promise<T>((accept, deny) => {
    resolve = accept;
    reject = deny;
  });
  return { promise, resolve, reject };
}

class DeferredBackend implements SearchBackend {
  readonly identity: BackendIdentity;

  constructor(identity: BackendIdentity = {
    profile: "in_plugin",
    instanceId: "in-plugin-1",
    label: "In-plugin",
    boundVaultId: "active-vault",
  }) {
    this.identity = identity;
  }
  readonly requests: SearchRequest[] = [];
  readonly searches: Deferred<SearchExecution>[] = [];
  readonly statuses: Deferred<BackendStatus>[] = [];
  statusValue = status();
  statusCalls = 0;
  statusInFlight = 0;
  maxStatusInFlight = 0;

  async initialize(): Promise<void> {}

  async status(): Promise<BackendStatus> {
    this.statusCalls += 1;
    const pending = this.statuses.shift();
    if (!pending) return this.statusValue;
    this.statusInFlight += 1;
    this.maxStatusInFlight = Math.max(this.maxStatusInFlight, this.statusInFlight);
    try {
      return await pending.promise;
    } finally {
      this.statusInFlight -= 1;
    }
  }

  search(request: SearchRequest): Promise<SearchExecution> {
    this.requests.push(request);
    const search = deferred<SearchExecution>();
    this.searches.push(search);
    return search.promise;
  }

  async dispose(): Promise<void> {}
}

function status(overrides: Partial<BackendStatus> = {}): BackendStatus {
  return {
    identity: {
      profile: "in_plugin",
      instanceId: "in-plugin-1",
      label: "In-plugin",
      boundVaultId: "active-vault",
    },
    phase: "ready",
    liveness: "alive",
    searchable: true,
    generation: "g1",
    capabilities: {
      supportedModes: ["lexical"],
      sourceScope: "active_vault",
      manualRebuild: true,
    },
    documents: 10,
    chunks: 20,
    dirty: false,
    rebuilding: false,
    ...overrides,
  };
}

function hit(
  chunkId: string,
  path: string,
  headingPath: string[] = [],
  overrides: Partial<BackendSearchHit> = {},
): BackendSearchHit {
  return {
    chunk_id: chunkId,
    vault_id: "active-vault",
    path,
    format: "markdown",
    coverage: "indexed-complete",
    locator: null,
    heading_path: headingPath,
    excerpt: [{ text: chunkId, highlighted: true }],
    score: 1,
    frontmatter: { title: `Title ${path}` },
    origin: {
      profile: "in_plugin",
      backendInstanceId: "in-plugin-1",
      vaultId: "active-vault",
    },
    ...overrides,
  };
}

function executionWithHits(
  hits: BackendSearchHit[],
  state: SearchExecution["candidateWindow"]["state"] = "exhausted",
  overrides: Partial<Pick<
    SearchExecution,
    "generation" | "backend" | "requestedMode" | "effectiveMode"
  >> = {},
): SearchExecution {
  return {
    backend: overrides.backend ?? {
      profile: "in_plugin",
      instanceId: "in-plugin-1",
      label: "In-plugin",
      boundVaultId: "active-vault",
    },
    requestedMode: overrides.requestedMode ?? "lexical",
    effectiveMode: overrides.effectiveMode ?? "lexical",
    generation: overrides.generation ?? "g1",
    candidateWindow: {
      state,
      candidateCount: state === "unknown" ? null : 24,
      candidateLimit: state === "candidate_limit_reached" ? 24 : null,
    },
    response: { hits, next_cursor: null },
  };
}

function execution(
  resultCount: number,
  state: SearchExecution["candidateWindow"]["state"],
): SearchExecution {
  return executionWithHits(
    Array.from({ length: resultCount }, (_, index) => hit(
      `chunk-${index}`,
      `Note-${index}.md`,
    )),
    state,
  );
}

function plugin(options: {
  activeEditor?: unknown;
  activeBackend?: BackendIdentity | null;
  resultLimit?: number;
  /**
   * View type the leaf reports after the open. `undefined` leaves the leaf with
   * no observable view at all, which is what every non-PDF assertion here has
   * always exercised.
   */
  leafViewType?: string;
  openFileError?: unknown;
} = {}) {
  const openFile = vi.fn(async () => {
    if (options.openFileError !== undefined) throw options.openFileError;
  });
  const leaf = options.leafViewType === undefined
    ? { openFile }
    : { openFile, view: { getViewType: () => options.leafViewType } };
  const getLeaf = vi.fn(() => leaf);
  const getAbstractFileByPath = vi.fn((path: string) => new FakeTFile(path));
  const generateMarkdownLink = vi.fn(
    (_file: FakeTFile, _sourcePath: string, subpath?: string) => `[[target${subpath ?? ""}]]`,
  );
  const activeBackend = options.activeBackend === undefined
    ? {
      profile: "in_plugin" as const,
      instanceId: "in-plugin-1",
      label: "In-plugin" as const,
      boundVaultId: "active-vault",
    }
    : options.activeBackend;
  const instance = {
    app: {
      workspace: { activeEditor: options.activeEditor ?? null, getLeaf },
      vault: { getAbstractFileByPath },
      fileManager: { generateMarkdownLink },
    },
    settings: {
      defaultMode: "lexical",
      resultLimit: options.resultLimit ?? 20,
      vaultId: "",
      daemonCurrentVaultId: "",
    },
    captureDiagnostic: async (
      _level: string,
      _eventName: string,
      _details: unknown,
      operation: (event: {
        set(values: unknown): void;
        setLevel(level: string): void;
      }) => Promise<unknown>,
    ) => operation({ set(): void {}, setLevel(): void {} }),
    diagnosticErrorDetails: () => ({}),
    recordCaughtFailure: vi.fn(),
    getActiveBackendIdentity: () => activeBackend,
  };
  return {
    instance,
    openFile,
    getLeaf,
    getAbstractFileByPath,
    generateMarkdownLink,
  };
}

function modalElements(modal: SearchModalLike): {
  query: FakeElement;
  index: FakeElement;
  results: FakeElement;
} {
  const query = findByClass(modal.contentEl, "kwiry-query-status");
  const index = findByClass(modal.contentEl, "kwiry-index-status");
  const results = findByClass(modal.contentEl, "suggestion-container");
  if (!query || !index || !results) throw new Error("Status rail test elements are missing");
  return { query, index, results };
}

function findByClass(root: FakeElement, className: string): FakeElement | null {
  if (root.classList.contains(className)) return root;
  for (const child of root.children) {
    const found = findByClass(child, className);
    if (found) return found;
  }
  return null;
}

function createModal(
  backend: DeferredBackend,
  initialStatus: BackendStatus = status(),
  pluginHarness = plugin(),
): SearchModalLike {
  return new searchModalModule.KwirySearchModal(
    pluginHarness.instance,
    backend,
    initialStatus,
  );
}

function keyboard(
  key: string,
  overrides: Partial<KeyboardEventInit> = {},
): KeyboardEvent {
  return new FakeKeyboardEvent("keydown", {
    key,
    bubbles: true,
    cancelable: true,
    ...overrides,
  }) as unknown as KeyboardEvent;
}

async function settleInputSearch(
  modal: SearchModalLike,
  backend: DeferredBackend,
  query: string,
  result: SearchExecution,
): Promise<void> {
  modal.inputEl.value = query;
  modal.inputEl.dispatchEvent(new Event("input", { bubbles: true }));
  backend.searches.at(-1)!.resolve(result);
  await modal.flushSuggestions();
}

function renderedRows(modal: SearchModalLike): FakeElement[] {
  return [...modal.resultContainerEl.children];
}

beforeEach(() => {
  vi.useFakeTimers();
  vi.stubGlobal("window", globalThis);
  vi.stubGlobal("KeyboardEvent", FakeKeyboardEvent);
  notices.length = 0;
});

afterEach(() => {
  vi.clearAllTimers();
  vi.useRealTimers();
  vi.unstubAllGlobals();
});

describe("KwirySearchModal status rail", () => {
  it("exposes an atomic polite query region and delays only its animation", async () => {
    const backend = new DeferredBackend();
    const modal = createModal(backend);
    const { query, results } = modalElements(modal);

    expect(query.attributes.get("role")).toBe("status");
    expect(query.attributes.get("aria-live")).toBe("polite");
    expect(query.attributes.get("aria-atomic")).toBe("true");
    expect(query.textContent).toBe("Type to search your notes.");
    expect(results.attributes.get("aria-busy")).toBe("false");

    const pending = modal.getSuggestions("query");
    expect(query.textContent).toBe("Searching…");
    expect(results.attributes.get("aria-busy")).toBe("true");
    expect(query.classList.contains("is-animation-ready")).toBe(false);

    await vi.advanceTimersByTimeAsync(searchModalModule.SEARCH_STATUS_ANIMATION_DELAY_MS - 1);
    expect(query.classList.contains("is-animation-ready")).toBe(false);
    await vi.advanceTimersByTimeAsync(1);
    expect(query.classList.contains("is-animation-ready")).toBe(true);

    backend.searches[0]!.resolve(execution(1, "exhausted"));
    await expect(pending).resolves.toHaveLength(1);
    expect(query.textContent).toBe("1 returned section — 1 source shown; search window complete.");
    expect(query.classList.contains("is-animation-ready")).toBe(false);
    expect(results.attributes.get("aria-busy")).toBe("false");
    modal.onClose();
  });

  it.each([
    { state: "exhausted", disclosure: "search window complete." },
    { state: "more_available", disclosure: "more candidates are available." },
    { state: "candidate_limit_reached", disclosure: "candidate window limit reached." },
    { state: "unknown", disclosure: "window completeness is unknown." },
  ] as const)("preserves exact $state candidate-window disclosure at the modal seam", async ({
    state,
    disclosure,
  }) => {
    const backend = new DeferredBackend();
    const modal = createModal(backend);
    const { query } = modalElements(modal);

    const pending = modal.getSuggestions("candidate truth");
    backend.searches[0]!.resolve(execution(1, state));
    await expect(pending).resolves.toHaveLength(1);

    expect(query.textContent).toBe(`1 returned section — 1 source shown; ${disclosure}`);
    modal.onClose();
  });

  it("does not let a stale request replace the current request status", async () => {
    const backend = new DeferredBackend();
    const modal = createModal(backend);
    const { query } = modalElements(modal);

    const older = modal.getSuggestions("older");
    const newer = modal.getSuggestions("newer");
    backend.searches[1]!.resolve(execution(7, "more_available"));
    await expect(newer).resolves.toHaveLength(7);
    expect(query.textContent).toBe("7 returned sections — 7 sources shown; more candidates are available.");

    backend.searches[0]!.resolve(execution(0, "exhausted"));
    await expect(older).resolves.toEqual([]);
    expect(query.textContent).toBe("7 returned sections — 7 sources shown; more candidates are available.");
    modal.onClose();
  });

  it("reports returned sections separately from grouped source rows", async () => {
    const backend = new DeferredBackend();
    const modal = createModal(backend);
    const { query } = modalElements(modal);

    const pending = modal.getSuggestions("grouped counts");
    backend.searches[0]!.resolve(executionWithHits([
      hit("a-1", "A.md", ["One"]),
      hit("b-1", "B.md"),
      hit("a-2", "A.md", ["Two"]),
      hit("c-1", "C.md"),
      hit("d-1", "D.md"),
      hit("e-1", "E.md", ["One"]),
      hit("e-2", "E.md", ["Two"]),
    ]));

    await expect(pending).resolves.toHaveLength(5);
    expect(query.textContent).toBe(
      "7 returned sections — 5 sources shown; search window complete.",
    );
    modal.onClose();
  });

  it("keeps successive no-match live announcements bounded and query-free", async () => {
    const backend = new DeferredBackend();
    const modal = createModal(backend);
    const { query } = modalElements(modal);
    const rawQueries = ["private phrase", "x".repeat(4_096)];

    for (const rawQuery of rawQueries) {
      const pending = modal.getSuggestions(rawQuery);
      backend.searches.at(-1)!.resolve(execution(0, "exhausted"));
      await expect(pending).resolves.toEqual([]);
      expect(query.textContent).toBe("No matches — search window complete.");
      expect(query.textContent.length).toBeLessThan(64);
      expect(query.textContent).not.toContain(rawQuery);
    }
    modal.onClose();
  });

  it("shows user-correctable query guidance inline without a Notice", async () => {
    const backend = new DeferredBackend();
    const modal = createModal(backend);
    const { query } = modalElements(modal);

    const pending = modal.getSuggestions("title:(");
    backend.searches[0]!.reject(new searchModalModule.KwiryBackendError(
      "invalid_query",
      "in_plugin",
      "query",
      false,
      "The query is invalid.",
    ));

    await expect(pending).resolves.toEqual([]);
    expect(query.textContent).toBe("The query is invalid or exceeds the supported limits.");
    expect(notices).toEqual([]);
    modal.onClose();
  });

  it("reruns the retained query once when a cold index becomes searchable", async () => {
    const backend = new DeferredBackend();
    const building = status({
      phase: "building",
      searchable: false,
      generation: null,
      dirty: true,
      rebuilding: true,
    });
    backend.statusValue = building;
    const modal = createModal(backend, building);
    modal.inputEl.value = "retained query";

    const blocked = modal.getSuggestions(modal.inputEl.value);
    backend.searches[0]!.reject(new searchModalModule.KwiryBackendError(
      "index_building",
      "in_plugin",
      "index",
      true,
      "In-plugin lexical index is still building.",
    ));
    await expect(blocked).resolves.toEqual([]);
    expect(modal.inputEl.dispatchedEvents).toEqual([]);

    backend.statusValue = status();
    await vi.advanceTimersByTimeAsync(400);
    expect(modal.inputEl.dispatchedEvents.map((event) => event.type)).toEqual(["input"]);

    await vi.advanceTimersByTimeAsync(400);
    expect(modal.inputEl.dispatchedEvents).toHaveLength(1);

    backend.statusValue = building;
    await vi.advanceTimersByTimeAsync(400);
    backend.statusValue = status();
    await vi.advanceTimersByTimeAsync(400);
    expect(modal.inputEl.dispatchedEvents).toHaveLength(1);
    modal.onClose();
  });

  it("does not rerun a cleared query when the cold index becomes searchable", async () => {
    const backend = new DeferredBackend();
    const building = status({
      phase: "building",
      searchable: false,
      generation: null,
      dirty: true,
      rebuilding: true,
    });
    backend.statusValue = building;
    const modal = createModal(backend, building);
    modal.inputEl.value = "temporary query";

    const blocked = modal.getSuggestions(modal.inputEl.value);
    backend.searches[0]!.reject(new searchModalModule.KwiryBackendError(
      "index_building",
      "in_plugin",
      "index",
      true,
      "In-plugin lexical index is still building.",
    ));
    await expect(blocked).resolves.toEqual([]);
    modal.inputEl.value = "";

    backend.statusValue = status();
    await vi.advanceTimersByTimeAsync(400);
    expect(modal.inputEl.dispatchedEvents).toEqual([]);
    modal.onClose();
  });

  it("updates background indexing without mutating the query live region", async () => {
    const backend = new DeferredBackend();
    const modal = createModal(backend);
    const { query, index } = modalElements(modal);
    const queryMutations = query.textSetCount;

    backend.statusValue = status({
      phase: "building",
      searchable: false,
      dirty: true,
      progress: {
        stage: "snapshot",
        activity: "read",
        completed: 4,
        total: 10,
        inFlight: 1,
      },
    });
    await vi.advanceTimersByTimeAsync(400);

    expect(index.textContent).toBe("Index · Reading 4/10 (40%) ·  1 in flight");
    expect(index.attributes.has("aria-live")).toBe(false);
    expect(query.textSetCount).toBe(queryMutations);
    expect(query.textContent).toBe("Type to search your notes.");
    modal.onClose();
  });

  it("single-flights slow status polls, eventually renders, and invalidates on close", async () => {
    const backend = new DeferredBackend();
    const first = deferred<BackendStatus>();
    const afterClose = deferred<BackendStatus>();
    backend.statuses.push(first, afterClose);
    const modal = createModal(backend);
    const { index } = modalElements(modal);

    await vi.advanceTimersByTimeAsync(1_200);
    expect(backend.statusCalls).toBe(1);
    expect(backend.statusInFlight).toBe(1);
    expect(backend.maxStatusInFlight).toBe(1);

    first.resolve(status({
      phase: "building",
      searchable: false,
      dirty: true,
      progress: {
        stage: "snapshot",
        activity: "read",
        completed: 6,
        total: 10,
        inFlight: 1,
      },
    }));
    await vi.advanceTimersByTimeAsync(0);
    expect(index.textContent).toBe("Index · Reading 6/10 (60%) ·  1 in flight");

    await vi.advanceTimersByTimeAsync(400);
    expect(backend.statusCalls).toBe(2);
    expect(backend.statusInFlight).toBe(1);
    expect(backend.maxStatusInFlight).toBe(1);

    modal.onClose();
    afterClose.resolve(status({
      phase: "building",
      progress: {
        stage: "snapshot",
        activity: "read",
        completed: 9,
        total: 10,
        inFlight: 1,
      },
    }));
    await vi.advanceTimersByTimeAsync(0);
    expect(index.textContent).toBe("Index · Reading 6/10 (60%) ·  1 in flight");
    expect(vi.getTimerCount()).toBe(0);
  });

  it("invalidates delayed updates and clears timers when closed", async () => {
    const backend = new DeferredBackend();
    const modal = createModal(backend);
    const { query } = modalElements(modal);

    const pending = modal.getSuggestions("query");
    modal.onClose();
    await vi.advanceTimersByTimeAsync(searchModalModule.SEARCH_STATUS_ANIMATION_DELAY_MS);
    expect(query.classList.contains("is-animation-ready")).toBe(false);

    backend.searches[0]!.resolve(execution(1, "exhausted"));
    await expect(pending).resolves.toEqual([]);
    expect(query.textContent).toBe("Searching…");
    expect(vi.getTimerCount()).toBe(0);
  });
});

describe("KwirySearchModal grouped interactions", () => {
  it("renders one source row in first-occurrence order with bounded returned-section counts", async () => {
    const backend = new DeferredBackend();
    const modal = createModal(backend);
    const a1 = hit("a1", "Folder/A.md", ["Highest ranked"]);
    const b1 = hit("b1", "B.md", ["Only section"]);
    const a2 = hit("a2", "Folder/A.md", ["Lower ranked"]);

    await settleInputSearch(modal, backend, "ranked", executionWithHits([a1, b1, a2]));

    expect(backend.requests).toEqual([{
      q: "ranked",
      mode: "lexical",
      limit: 100,
      filters: undefined,
    }]);
    expect(modal.suggestions).toHaveLength(2);
    const rows = renderedRows(modal);
    expect(rows).toHaveLength(2);
    expect(rows[0]?.classList.contains("kwiry-source-result")).toBe(true);
    expect(findByClass(rows[0]!, "kwiry-result-title")?.textContent).toBe("Title Folder/A.md");
    expect(findByClass(rows[0]!, "kwiry-result-meta")?.textContent).toBe(
      "Folder/A.md › Highest ranked",
    );
    expect(findByClass(rows[0]!, "kwiry-result-context")?.textContent).toBe(
      "2 returned sections",
    );
    expect(findByClass(rows[1]!, "kwiry-result-context")?.textContent).toBe(
      "1 returned section",
    );
    modal.onClose();
  });

  it("uses the HTML filename as the source-row title instead of the document title", async () => {
    const backend = new DeferredBackend();
    const modal = createModal(backend);

    await settleInputSearch(modal, backend, "portal", executionWithHits([
      hit("html", "site/index.html", ["Overview"], {
        format: "html",
        frontmatter: { title: "Canonical Portal" },
      }),
    ]));

    const sourceRow = renderedRows(modal)[0]!;
    expect(findByClass(sourceRow, "kwiry-result-title")?.textContent).toBe("index.html");
    expect(findByClass(sourceRow, "kwiry-result-meta")?.textContent)
      .toBe("site/index.html › Overview");
    modal.onClose();
  });

  it("discloses source-row truncation independently from an exhausted candidate window", async () => {
    const backend = new DeferredBackend();
    const modal = createModal(backend, status(), plugin({ resultLimit: 1 }));
    const { query } = modalElements(modal);

    await settleInputSearch(modal, backend, "bounded sources", executionWithHits([
      hit("a1", "A.md", ["A"]),
      hit("b1", "B.md", ["B"]),
    ], "exhausted"));

    expect(modal.suggestions).toHaveLength(1);
    expect(findByClass(renderedRows(modal)[0]!, "kwiry-result-meta")?.textContent).toBe("A.md › A");
    expect(query.textContent).toBe(
      "2 returned sections — 1 source shown; 1 observed source omitted by the source-row limit; search window complete.",
    );
    modal.onClose();
  });

  it("renders visible, accessible format labels for current and future source vocabulary", async () => {
    const backend = new DeferredBackend();
    const modal = createModal(backend);
    const formatCases = [
      { format: "markdown", path: "Note.md", label: "MD", accessibleLabel: "Markdown source format" },
      { format: "text", path: "Notes.txt", label: "TXT", accessibleLabel: "Plain text source format" },
      { format: "base", path: "Projects.base", label: "BASE", accessibleLabel: "Obsidian Base source format" },
      { format: "canvas", path: "Map.canvas", label: "CANVAS", accessibleLabel: "Obsidian Canvas source format" },
      { format: "docx", path: "Draft.docx", label: "DOCX", accessibleLabel: "Word document source format" },
      { format: "pdf", path: "Paper.pdf", label: "PDF", accessibleLabel: "PDF source format" },
      { format: "excalidraw", path: "Sketch.excalidraw", label: "EXCA", accessibleLabel: "Excalidraw drawing source format" },
      { format: "excel", path: "Budget.xlsx", label: "XLSX", accessibleLabel: "Excel workbook source format" },
      { format: "html", path: "Portal.htm", label: "HTML", accessibleLabel: "HTML document source format" },
    ] as const;

    await settleInputSearch(modal, backend, "formats", executionWithHits(
      formatCases.map(({ format, path }, index) => hit(`format-${index}`, path, [], { format })),
    ));

    expect(renderedRows(modal).map((row) => {
      const chip = findByClass(row, "kwiry-result-format");
      return {
        label: chip?.textContent,
        accessibleLabel: chip?.attributes.get("aria-label"),
      };
    })).toEqual(formatCases.map(({ label, accessibleLabel }) => ({ label, accessibleLabel })));
    modal.onClose();
  });

  it("uses the exact drilled hit when rendering its format chip", async () => {
    const backend = new DeferredBackend();
    const modal = createModal(backend);
    await settleInputSearch(modal, backend, "format", executionWithHits([
      hit("rep", "Projects.base", ["Overview"], {
        format: "base",
        locator: { kind: "base_view", view: "Overview" },
      }),
      hit("exact", "Projects.base", ["Details"], {
        format: "base",
        locator: { kind: "base_view", view: "Details" },
      }),
    ]));

    expect(findByClass(renderedRows(modal)[0]!, "kwiry-result-format")?.textContent).toBe("BASE");
    modal.triggerScope(["Ctrl"], "l", keyboard("l", { ctrlKey: true }));
    await modal.flushSuggestions();
    expect(renderedRows(modal).map((row) => ({
      title: findByClass(row, "kwiry-result-title")?.textContent,
      format: findByClass(row, "kwiry-result-format")?.textContent,
    }))).toEqual([
      { title: "Overview", format: "BASE" },
      { title: "Details", format: "BASE" },
    ]);
    modal.onClose();
  });

  it("navigates the visible view and drills/backtracks without searching or live-region churn", async () => {
    const backend = new DeferredBackend();
    const modal = createModal(backend);
    const { query } = modalElements(modal);
    await settleInputSearch(modal, backend, "ranked", executionWithHits([
      hit("a1", "A.md", ["A"]),
      hit("b1", "B.md", []),
      hit("b2", "B.md", ["Second"]),
      hit("b3", "B.md", ["Third"]),
    ], "more_available"));
    const settledText = query.textContent;
    const settledMutations = query.textSetCount;

    modal.triggerScope(["Ctrl"], "j", keyboard("j", { ctrlKey: true }));
    expect(modal.activeIndex).toBe(1);
    modal.triggerScope(["Ctrl"], "j", keyboard("j", { ctrlKey: true }));
    expect(modal.activeIndex).toBe(1);

    modal.triggerScope(["Ctrl"], "l", keyboard("l", { ctrlKey: true }));
    await modal.flushSuggestions();
    expect(backend.requests).toHaveLength(1);
    expect(modal.suggestions).toHaveLength(3);
    expect(renderedRows(modal).map((row) =>
      findByClass(row, "kwiry-result-title")?.textContent)).toEqual([
      "Match 1",
      "Second",
      "Third",
    ]);
    expect(renderedRows(modal).every((row) =>
      row.classList.contains("kwiry-section-result"))).toBe(true);

    modal.triggerScope(["Ctrl"], "j", keyboard("j", { ctrlKey: true }));
    modal.triggerScope(["Ctrl"], "j", keyboard("j", { ctrlKey: true }));
    modal.triggerScope(["Ctrl"], "j", keyboard("j", { ctrlKey: true }));
    expect(modal.activeIndex).toBe(2);
    modal.triggerScope(["Ctrl"], "k", keyboard("k", { ctrlKey: true }));
    expect(modal.activeIndex).toBe(1);

    modal.triggerScope(["Ctrl"], "h", keyboard("h", { ctrlKey: true }));
    await modal.flushSuggestions();
    await vi.advanceTimersByTimeAsync(0);
    expect(backend.requests).toHaveLength(1);
    expect(modal.suggestions).toHaveLength(2);
    expect(modal.activeIndex).toBe(1);
    expect(modal.inputEl.focusCount).toBe(1);
    expect(query.textContent).toBe(settledText);
    expect(query.textSetCount).toBe(settledMutations);
    modal.onClose();
  });

  it("resets drill state for query and mode changes", async () => {
    const backend = new DeferredBackend();
    const multiModeStatus = status({
      capabilities: {
        supportedModes: ["lexical", "semantic"],
        sourceScope: "active_vault",
        manualRebuild: true,
      },
    });
    const modal = createModal(backend, multiModeStatus);
    await settleInputSearch(modal, backend, "first", executionWithHits([
      hit("a1", "A.md", ["A1"]),
      hit("a2", "A.md", ["A2"]),
    ]));
    modal.triggerScope(["Ctrl"], "l", keyboard("l", { ctrlKey: true }));
    await modal.flushSuggestions();
    expect(modal.suggestions).toHaveLength(2);

    modal.inputEl.value = "second";
    modal.inputEl.dispatchEvent(new Event("input", { bubbles: true }));
    backend.searches.at(-1)!.resolve(executionWithHits([
      hit("b1", "B.md", ["B1"]),
      hit("b2", "B.md", ["B2"]),
    ]));
    await modal.flushSuggestions();
    expect(modal.suggestions).toHaveLength(1);
    expect(renderedRows(modal)[0]?.classList.contains("kwiry-source-result")).toBe(true);

    modal.triggerScope(["Ctrl"], "l", keyboard("l", { ctrlKey: true }));
    await modal.flushSuggestions();
    const requestsBeforeMode = backend.requests.length;
    modal.triggerScope([], "Tab", keyboard("Tab"));
    backend.searches.at(-1)!.resolve(executionWithHits(
      [hit("c1", "C.md", ["C1"]), hit("c2", "C.md", ["C2"])],
      "exhausted",
      { requestedMode: "semantic", effectiveMode: "semantic", generation: "g2" },
    ));
    await modal.flushSuggestions();
    expect(backend.requests).toHaveLength(requestsBeforeMode + 1);
    expect(backend.requests.at(-1)?.mode).toBe("semantic");
    expect(modal.suggestions).toHaveLength(1);
    expect(renderedRows(modal)[0]?.classList.contains("kwiry-source-result")).toBe(true);
    modal.onClose();
  });

  it("opens results from a generation the search observes before the status poll", async () => {
    const backend = new DeferredBackend();
    const pluginHarness = plugin();
    const modal = createModal(backend, status({ generation: "g1" }), pluginHarness);
    await settleInputSearch(modal, backend, "macro boundary", executionWithHits([
      hit("excel", "Spreadsheets/03-macro-boundary.xlsm", ["Budget"], {
        format: "excel",
        locator: { kind: "excel_cell", sheet: "Budget", cell: "A1" },
      }),
    ], "exhausted", { generation: "g2" }));

    expect(modal.suggestions).toHaveLength(1);
    modal.selectSuggestion(modal.suggestions[0]!, keyboard("Enter"));
    await Promise.resolve();

    expect(pluginHarness.openFile).toHaveBeenCalledOnce();
    expect(notices).not.toContain(
      "Kwiry: these search results are out of date. Wait for the refreshed results.",
    );
    modal.onClose();
  });

  it("reports a safe notice when Obsidian cannot open an Excel workbook", async () => {
    const backend = new DeferredBackend();
    const rawError = new Error("private workbook provider failure");
    const pluginHarness = plugin({ openFileError: rawError });
    const modal = createModal(backend, status(), pluginHarness);
    await settleInputSearch(modal, backend, "macro boundary", executionWithHits([
      hit("excel", "Spreadsheets/03-macro-boundary.xlsm", ["Budget"], {
        format: "excel",
        locator: { kind: "excel_cell", sheet: "Budget", cell: "A1" },
      }),
    ]));

    modal.selectSuggestion(modal.suggestions[0]!, keyboard("Enter"));
    await Promise.resolve();
    await Promise.resolve();

    expect(pluginHarness.openFile).toHaveBeenCalledWith(
      expect.objectContaining({ path: "Spreadsheets/03-macro-boundary.xlsm" }),
      { eState: undefined },
    );
    expect(pluginHarness.instance.recordCaughtFailure).toHaveBeenCalledWith(
      "ui",
      "open",
      rawError,
      { profile: "in_plugin" },
    );
    expect(notices).toContain("Kwiry: Obsidian could not open this file.");
    expect(notices.join("\n")).not.toContain("private workbook provider failure");
    modal.onClose();
  });

  it("opens a newer in-plugin execution after an intermediate status generation", async () => {
    const backend = new DeferredBackend();
    const pluginHarness = plugin();
    const modal = createModal(backend, status({ generation: "g1" }), pluginHarness);

    modal.inputEl.value = "new generation";
    modal.inputEl.dispatchEvent(new Event("input", { bubbles: true }));
    backend.statusValue = status({ generation: "g2" });
    await vi.advanceTimersByTimeAsync(400);
    backend.searches[0]!.resolve(executionWithHits([
      hit("new", "New.md", ["New"]),
    ], "exhausted", { generation: "g3" }));
    await modal.flushSuggestions();

    modal.selectSuggestion(modal.suggestions[0]!, keyboard("Enter"));
    await Promise.resolve();
    expect(pluginHarness.openFile).toHaveBeenCalledOnce();
    expect(notices).not.toContain(
      "Kwiry: these search results are out of date. Wait for the refreshed results.",
    );
    modal.onClose();
  });

  it("does not adopt a daemon execution generation over newer status", async () => {
    const daemon: BackendIdentity = {
      profile: "daemon",
      instanceId: "daemon-1",
      label: "Daemon",
      boundVaultId: "active-vault",
    };
    const backend = new DeferredBackend(daemon);
    const pluginHarness = plugin({ activeBackend: daemon });
    pluginHarness.instance.settings.daemonCurrentVaultId = "active-vault";
    const modal = createModal(backend, status({ identity: daemon, generation: "g1" }), pluginHarness);

    modal.inputEl.value = "old generation";
    modal.inputEl.dispatchEvent(new Event("input", { bubbles: true }));
    backend.statusValue = status({ identity: daemon, generation: "g3" });
    await vi.advanceTimersByTimeAsync(400);
    backend.searches[0]!.resolve(executionWithHits([
      hit("old", "Old.md", ["Old"], {
        origin: {
          profile: "daemon",
          backendInstanceId: "daemon-1",
          vaultId: "active-vault",
        },
      }),
    ], "unknown", { backend: daemon, generation: "g2" }));
    await modal.flushSuggestions();

    expect(modal.suggestions).toHaveLength(1);
    modal.selectSuggestion(modal.suggestions[0]!, keyboard("Enter"));
    expect(pluginHarness.openFile).not.toHaveBeenCalled();
    expect(notices).toContain(
      "Kwiry: these search results are out of date. Wait for the refreshed results.",
    );
    modal.onClose();
  });

  it("invalidates drilled results and reruns once when status observes a new generation", async () => {
    const backend = new DeferredBackend();
    const pluginHarness = plugin();
    const modal = createModal(backend, status(), pluginHarness);
    await settleInputSearch(modal, backend, "generation", executionWithHits([
      hit("a1", "A.md", ["A1"]),
      hit("a2", "A.md", ["A2"]),
    ], "exhausted", { generation: "g1" }));
    modal.triggerScope(["Ctrl"], "l", keyboard("l", { ctrlKey: true }));
    await modal.flushSuggestions();
    expect(renderedRows(modal).every((row) =>
      row.classList.contains("kwiry-section-result"))).toBe(true);
    const staleSection = modal.suggestions[0];

    backend.statusValue = status({ generation: "g2" });
    await vi.advanceTimersByTimeAsync(400);

    expect(backend.requests).toHaveLength(2);
    expect(backend.requests[1]).toEqual(backend.requests[0]);
    expect(renderedRows(modal)).toEqual([]);
    modal.triggerScope(["Ctrl"], "h", keyboard("h", { ctrlKey: true }));
    await Promise.resolve();
    expect(backend.requests).toHaveLength(2);
    expect(renderedRows(modal)).toEqual([]);

    modal.selectSuggestion(staleSection, keyboard("Enter"));
    expect(pluginHarness.openFile).not.toHaveBeenCalled();
    expect(notices).toContain(
      "Kwiry: these search results are out of date. Wait for the refreshed results.",
    );

    // Choosing a genuinely stale row closes the real SuggestModal. Its
    // already-started refresh must therefore remain unable to repopulate it.
    backend.searches[1]!.resolve(executionWithHits(
      [hit("b1", "B.md", ["B1"])],
      "exhausted",
      { generation: "g2" },
    ));
    await modal.flushSuggestions();
    expect(modal.suggestions).toEqual([]);
    expect(renderedRows(modal)).toEqual([]);
    await vi.advanceTimersByTimeAsync(400);
    expect(backend.requests).toHaveLength(2);
  });

  it("invalidates local drill projection when the backend identity changes", async () => {
    const backend = new DeferredBackend();
    const modal = createModal(backend);
    await settleInputSearch(modal, backend, "identity", executionWithHits([
      hit("a1", "A.md", ["A1"]),
      hit("a2", "A.md", ["A2"]),
    ]));
    modal.triggerScope(["Ctrl"], "l", keyboard("l", { ctrlKey: true }));
    await modal.flushSuggestions();

    (backend.identity as { instanceId: string }).instanceId = "in-plugin-2";
    modal.triggerScope(["Ctrl"], "h", keyboard("h", { ctrlKey: true }));
    const replacementOrigin = {
      profile: "in_plugin" as const,
      backendInstanceId: "in-plugin-2",
      vaultId: "active-vault",
    };
    backend.searches.at(-1)!.resolve(executionWithHits(
      [hit("b1", "B.md", ["B1"], { origin: replacementOrigin })],
      "exhausted",
      {
        backend: {
          profile: "in_plugin",
          instanceId: "in-plugin-2",
          label: "In-plugin",
          boundVaultId: "active-vault",
        },
        generation: "g2",
      },
    ));
    await modal.flushSuggestions();

    expect(backend.requests).toHaveLength(2);
    expect(modal.suggestions).toHaveLength(1);
    expect(findByClass(renderedRows(modal)[0]!, "kwiry-result-meta")?.textContent).toBe(
      "B.md › B1",
    );
    modal.onClose();
  });

  it("does not let an older input response erase a newer grouped projection", async () => {
    const backend = new DeferredBackend();
    const modal = createModal(backend);

    modal.inputEl.value = "older";
    modal.inputEl.dispatchEvent(new Event("input", { bubbles: true }));
    modal.inputEl.value = "newer";
    modal.inputEl.dispatchEvent(new Event("input", { bubbles: true }));

    backend.searches[1]!.resolve(executionWithHits([
      hit("new-1", "New.md", ["New 1"]),
      hit("new-2", "New.md", ["New 2"]),
    ], "exhausted", { generation: "g3" }));
    await modal.flushSuggestions();
    expect(modal.suggestions).toHaveLength(1);
    expect(findByClass(renderedRows(modal)[0]!, "kwiry-result-meta")?.textContent).toBe(
      "New.md › New 1",
    );

    backend.searches[0]!.resolve(executionWithHits(
      [hit("old", "Old.md", ["Old"])],
      "exhausted",
      { generation: "g9" },
    ));
    await Promise.resolve();
    await Promise.resolve();
    expect(modal.suggestions).toHaveLength(1);
    expect(findByClass(renderedRows(modal)[0]!, "kwiry-result-meta")?.textContent).toBe(
      "New.md › New 1",
    );
    modal.selectSuggestion(modal.suggestions[0]!, keyboard("Enter"));
    await Promise.resolve();
    expect(notices).not.toContain(
      "Kwiry: these search results are out of date. Wait for the refreshed results.",
    );
    modal.onClose();
  });

  it.each([
    {
      format: "markdown",
      path: "A.md",
      representativeLocator: null,
      exactLocator: null,
      representativeSubpath: "#Representative",
      exactSubpath: "#Exact",
    },
    {
      format: "text",
      path: "A.txt",
      representativeLocator: null,
      exactLocator: null,
      representativeSubpath: undefined,
      exactSubpath: undefined,
    },
    {
      format: "base",
      path: "A.base",
      representativeLocator: { kind: "base_view" as const, view: "Representative" },
      exactLocator: { kind: "base_view" as const, view: "Exact" },
      representativeSubpath: "#Representative",
      exactSubpath: "#Exact",
    },
    {
      format: "canvas",
      path: "A.canvas",
      representativeLocator: null,
      exactLocator: null,
      representativeSubpath: undefined,
      exactSubpath: undefined,
    },
    {
      format: "html",
      path: "A.htm",
      representativeLocator: null,
      exactLocator: null,
      representativeSubpath: undefined,
      exactSubpath: undefined,
    },
    {
      // The heading paths below are ignored for a PDF: only the page locator
      // decides where each row opens, so the source row lands on the
      // representative's page and the drilled row on its own.
      format: "pdf",
      path: "A.pdf",
      representativeLocator: { kind: "pdf_page" as const, page: 4 },
      exactLocator: { kind: "pdf_page" as const, page: 19 },
      representativeSubpath: "#page=4",
      exactSubpath: "#page=19",
    },
  ] as const)("opens representative and drilled exact $format hits correctly", async ({
    format,
    path,
    representativeLocator,
    exactLocator,
    representativeSubpath,
    exactSubpath,
  }) => {
    for (const view of ["source", "section"] as const) {
      const backend = new DeferredBackend();
      const pluginHarness = plugin(format === "pdf" ? { leafViewType: "pdf" } : {});
      const modal = createModal(backend, status(), pluginHarness);
      await settleInputSearch(modal, backend, "format open", executionWithHits([
        hit("rep", path, ["Representative"], {
          format,
          locator: representativeLocator,
        }),
        hit("exact", path, ["Exact"], {
          format,
          locator: exactLocator,
        }),
      ]));
      if (view === "section") {
        modal.triggerScope(["Ctrl"], "l", keyboard("l", { ctrlKey: true }));
        await modal.flushSuggestions();
      }

      const selected = modal.suggestions[view === "source" ? 0 : 1];
      modal.selectSuggestion(selected, keyboard("Enter"));

      expect(pluginHarness.getAbstractFileByPath).toHaveBeenCalledWith(path);
      expect(pluginHarness.getLeaf).toHaveBeenCalledWith(false);
      expect(pluginHarness.openFile).toHaveBeenCalledWith(
        expect.any(FakeTFile),
        {
          eState: (view === "source" ? representativeSubpath : exactSubpath) === undefined
            ? undefined
            : { subpath: view === "source" ? representativeSubpath : exactSubpath },
        },
      );
      await vi.advanceTimersByTimeAsync(0);
      expect(notices).toEqual([]);
      modal.onClose();
    }
  });

  it("says so when the view a PDF opened in cannot take a page", async () => {
    for (const leafViewType of ["markdown", "third-party-pdf"]) {
      notices.length = 0;
      const backend = new DeferredBackend();
      const pluginHarness = plugin({ leafViewType });
      const modal = createModal(backend, status(), pluginHarness);
      await settleInputSearch(modal, backend, "page open", executionWithHits([
        hit("page-31", "A.pdf", [], { format: "pdf", locator: { kind: "pdf_page", page: 31 } }),
      ]));

      modal.selectSuggestion(modal.suggestions[0], keyboard("Enter"));

      // The file still opens: a page jump is an enhancement, and losing it must
      // never cost the user the document.
      expect(pluginHarness.openFile).toHaveBeenCalledWith(
        expect.any(FakeTFile),
        { eState: { subpath: "#page=31" } },
      );
      await vi.advanceTimersByTimeAsync(0);
      expect(notices).toEqual([
        "Kwiry: opened this PDF, but the view showing it cannot jump to page 31.",
      ]);
      modal.onClose();
    }
  });

  it("stays silent when a PDF opens in the view that understands the page", async () => {
    const backend = new DeferredBackend();
    const pluginHarness = plugin({ leafViewType: "pdf" });
    const modal = createModal(backend, status(), pluginHarness);
    await settleInputSearch(modal, backend, "page open", executionWithHits([
      hit("page-31", "A.pdf", [], { format: "pdf", locator: { kind: "pdf_page", page: 31 } }),
    ]));

    modal.selectSuggestion(modal.suggestions[0], keyboard("Enter"));

    expect(pluginHarness.openFile).toHaveBeenCalledWith(
      expect.any(FakeTFile),
      { eState: { subpath: "#page=31" } },
    );
    await vi.advanceTimersByTimeAsync(0);
    expect(notices).toEqual([]);
    modal.onClose();
  });

  it("names the page on a drilled PDF row and chips every admitted format", async () => {
    const backend = new DeferredBackend();
    const pluginHarness = plugin({ leafViewType: "pdf" });
    const modal = createModal(backend, status(), pluginHarness);
    await settleInputSearch(modal, backend, "page rows", executionWithHits([
      hit("p4", "A.pdf", [], { format: "pdf", locator: { kind: "pdf_page", page: 4 } }),
      hit("p19", "A.pdf", [], { format: "pdf", locator: { kind: "pdf_page", page: 19 } }),
      hit("nowhere", "A.pdf", [], { format: "pdf", locator: null }),
    ]));

    const sourceRow = renderedRows(modal)[0]!;
    expect(findByClass(sourceRow, "kwiry-result-format")?.textContent).toBe("PDF");
    expect(findByClass(sourceRow, "kwiry-result-format")?.attributes.get("aria-label"))
      .toBe("PDF source format");

    modal.triggerScope(["Ctrl"], "l", keyboard("l", { ctrlKey: true }));
    await modal.flushSuggestions();

    // Without the locator every drilled PDF row would read "Match 1", "Match 2"
    // — a PDF section has no heading path by construction. The page is a label
    // only; it is never added to heading_path and never queried.
    const titles = renderedRows(modal).map(
      (row) => findByClass(row, "kwiry-result-title")?.textContent,
    );
    expect(titles).toEqual(["Page 4", "Page 19", "Match 3"]);
    modal.onClose();
  });

  it("presents a chip for every source format the backend can return", () => {
    expect(Object.keys(searchModalModule.FORMAT_CHIP_PRESENTATIONS).sort()).toEqual([
      "base",
      "canvas",
      "docx",
      "excalidraw",
      "excel",
      "html",
      "markdown",
      "pdf",
      "text",
    ]);
    expect(searchModalModule.FORMAT_CHIP_PRESENTATIONS.html).toEqual({
      label: "HTML",
      accessibleLabel: "HTML document source format",
    });
  });

  it.each([
    { name: "current open", modifiers: null, key: "Enter", placement: false, background: false },
    { name: "new tab", modifiers: ["Mod"], key: "Enter", placement: "tab", background: false },
    { name: "new split", modifiers: ["Mod", "Alt"], key: "Enter", placement: "split", background: false },
    { name: "background tab", modifiers: ["Mod"], key: "o", placement: "tab", background: true },
  ] as const)("routes representative and exact hits for $name", async ({
    modifiers,
    key,
    placement,
    background,
  }) => {
    for (const view of ["source", "section"] as const) {
      const backend = new DeferredBackend();
      const pluginHarness = plugin();
      const modal = createModal(backend, status(), pluginHarness);
      await settleInputSearch(modal, backend, "open", executionWithHits([
        hit("rep", "A.md", ["Representative"]),
        hit("exact", "A.md", ["Exact"]),
      ]));
      if (view === "section") {
        modal.triggerScope(["Ctrl"], "l", keyboard("l", { ctrlKey: true }));
        await modal.flushSuggestions();
        modal.triggerScope(["Ctrl"], "j", keyboard("j", { ctrlKey: true }));
      }
      const modifierList: readonly string[] = modifiers ?? [];
      const event = keyboard(key, {
        ctrlKey: modifierList.includes("Mod"),
        altKey: modifierList.includes("Alt"),
      });
      if (modifiers === null) {
        modal.selectSuggestion(modal.suggestions[modal.activeIndex], event);
      } else {
        modal.triggerScope([...modifierList], key, event);
      }

      expect(pluginHarness.getLeaf).toHaveBeenCalledWith(placement);
      expect(pluginHarness.openFile).toHaveBeenCalledWith(
        expect.any(FakeTFile),
        { eState: { subpath: view === "source" ? "#Representative" : "#Exact" } },
      );
      expect(modal.activeIndex).toBe(view === "source" ? 0 : 1);
      expect(modal.closed).toBe(!background);
      modal.onClose();
    }
  });

  it.each([
    { name: "note link", modifiers: ["Alt"], sectionLink: false },
    { name: "section link", modifiers: ["Alt", "Shift"], sectionLink: true },
  ] as const)("routes representative and exact hits for $name insertion", async ({
    modifiers,
    sectionLink,
  }) => {
    for (const view of ["source", "section"] as const) {
      const replaceRange = vi.fn();
      const setCursor = vi.fn();
      const pluginHarness = plugin({
        activeEditor: {
          file: { path: "Source.md" },
          editor: {
            getCursor: () => ({ line: 0, ch: 0 }),
            getRange: () => "",
            replaceRange,
            setCursor,
          },
        },
      });
      const backend = new DeferredBackend();
      const modal = createModal(backend, status(), pluginHarness);
      await settleInputSearch(modal, backend, "insert", executionWithHits([
        hit("rep", "A.md", ["Representative"]),
        hit("exact", "A.md", ["Exact"]),
      ]));
      if (view === "section") {
        modal.triggerScope(["Ctrl"], "l", keyboard("l", { ctrlKey: true }));
        await modal.flushSuggestions();
        modal.triggerScope(["Ctrl"], "j", keyboard("j", { ctrlKey: true }));
      }
      const modifierList: readonly string[] = modifiers;
      modal.triggerScope([...modifierList], "Enter", keyboard("Enter", {
        altKey: true,
        shiftKey: modifierList.includes("Shift"),
      }));

      const expectedSubpath = sectionLink
        ? view === "source" ? "#Representative" : "#Exact"
        : undefined;
      expect(pluginHarness.generateMarkdownLink).toHaveBeenCalledWith(
        expect.any(FakeTFile),
        "Source.md",
        expectedSubpath,
        undefined,
      );
      expect(replaceRange).toHaveBeenCalledOnce();
      // The caret lands after the inserted link, not in front of it.
      expect(setCursor).toHaveBeenCalledOnce();
      modal.onClose();
    }
  });

  it("note-links any format and refuses a section link only where headings are not anchors", async () => {
    const replaceRange = vi.fn();
    const setCursor = vi.fn();
    const pluginHarness = plugin({
      activeEditor: {
        file: { path: "Source.md" },
        editor: {
          getCursor: () => ({ line: 0, ch: 0 }),
          getRange: () => "",
          replaceRange,
          setCursor,
        },
      },
    });
    const backend = new DeferredBackend();
    const modal = createModal(backend, status(), pluginHarness);
    await settleInputSearch(modal, backend, "insert", executionWithHits([
      hit("workbook", "Book.xlsx", ["Sheet1"], { format: "excel" }),
    ]));

    // A note link reaches any file in the vault, whatever its format.
    modal.triggerScope(["Alt"], "Enter", keyboard("Enter", { altKey: true }));
    expect(pluginHarness.generateMarkdownLink).toHaveBeenCalledWith(
      expect.any(FakeTFile),
      "Source.md",
      undefined,
      undefined,
    );
    expect(replaceRange).toHaveBeenCalledOnce();

    // A workbook sheet is a region of the extraction, not a link anchor.
    pluginHarness.generateMarkdownLink.mockClear();
    modal.triggerScope(["Alt", "Shift"], "Enter", keyboard("Enter", {
      altKey: true,
      shiftKey: true,
    }));
    expect(pluginHarness.generateMarkdownLink).not.toHaveBeenCalled();
    expect(replaceRange).toHaveBeenCalledOnce();
    modal.onClose();
  });

  it("keeps background-open selection and resolves mouse source/drill rows consistently", async () => {
    const backend = new DeferredBackend();
    const backgroundHarness = plugin();
    const modal = createModal(backend, status(), backgroundHarness);
    await settleInputSearch(modal, backend, "open", executionWithHits([
      hit("a", "A.md", ["A"]),
      hit("b", "B.md", ["B"]),
    ]));
    modal.triggerScope(["Ctrl"], "j", keyboard("j", { ctrlKey: true }));
    modal.triggerScope(["Mod"], "o", keyboard("o", { ctrlKey: true }));
    expect(modal.activeIndex).toBe(1);
    expect(modal.closed).toBe(false);
    expect(backgroundHarness.openFile).toHaveBeenCalledWith(
      expect.any(FakeTFile),
      { eState: { subpath: "#B" } },
    );
    modal.onClose();

    for (const view of ["source", "section"] as const) {
      const mouseBackend = new DeferredBackend();
      const mouseHarness = plugin();
      const mouseModal = createModal(mouseBackend, status(), mouseHarness);
      await settleInputSearch(mouseModal, mouseBackend, "mouse", executionWithHits([
        hit("rep", "Mouse.md", ["Representative"]),
        hit("exact", "Mouse.md", ["Exact"]),
      ]));
      if (view === "section") {
        mouseModal.triggerScope(["Ctrl"], "l", keyboard("l", { ctrlKey: true }));
        await mouseModal.flushSuggestions();
      }
      const selected = mouseModal.suggestions[view === "source" ? 0 : 1];
      mouseModal.selectSuggestion(selected, {
        ctrlKey: false,
        metaKey: false,
      } as MouseEvent);
      expect(mouseHarness.openFile).toHaveBeenCalledWith(
        expect.any(FakeTFile),
        { eState: { subpath: view === "source" ? "#Representative" : "#Exact" } },
      );
      mouseModal.onClose();
    }
  });
});
