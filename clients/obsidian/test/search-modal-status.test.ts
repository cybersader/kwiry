// SPDX-FileCopyrightText: 2026 cybersader
// SPDX-License-Identifier: GPL-3.0-only

import { createRequire } from "node:module";

import { build, type Plugin as EsbuildPlugin } from "esbuild";
import { afterEach, beforeAll, beforeEach, describe, expect, it, vi } from "vitest";

import type { SearchRequest } from "../src/api";
import {
  type BackendIdentity,
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

  addEventListener(): void {}
  focus(): void {}
  dispatchEvent(): boolean { return true; }
}

class FakeSuggestModal<T> {
  readonly app: unknown;
  readonly contentEl = new FakeElement("modal-content");
  readonly resultContainerEl = this.contentEl.createDiv({ cls: "suggestion-container" });
  readonly inputEl = new FakeElement("prompt-input");
  readonly scope = { register(): void {} };
  emptyStateText = "";

  constructor(app: unknown) {
    this.app = app;
  }

  setPlaceholder(): void {}
  setInstructions(): void {}
  selectSuggestion(_result: T, _event: unknown): void {}
  selectActiveSuggestion(): void {}
  onClose(): void {}
}

const notices: string[] = [];
class FakeNotice {
  constructor(message: string) {
    notices.push(message);
  }
}
class FakeTFile {}

interface SearchModalLike {
  readonly contentEl: FakeElement;
  getSuggestions(query: string): Promise<unknown[]>;
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
}

const SEARCH_MODAL_PATH = new URL("../src/search-modal.ts", import.meta.url).pathname;
const BACKEND_PATH = new URL("../src/backend.ts", import.meta.url).pathname;
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
  readonly identity: BackendIdentity = {
    profile: "in_plugin",
    instanceId: "in-plugin-1",
    label: "In-plugin",
    boundVaultId: "active-vault",
  };
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

function execution(
  resultCount: number,
  state: SearchExecution["candidateWindow"]["state"],
): SearchExecution {
  return {
    backend: {
      profile: "in_plugin",
      instanceId: "in-plugin-1",
      label: "In-plugin",
      boundVaultId: "active-vault",
    },
    requestedMode: "lexical",
    effectiveMode: "lexical",
    generation: "g1",
    candidateWindow: {
      state,
      candidateCount: state === "unknown" ? null : 24,
      candidateLimit: state === "candidate_limit_reached" ? 24 : null,
    },
    response: {
      hits: Array.from({ length: resultCount }, (_, index) => ({
        chunk_id: `chunk-${index}`,
        vault_id: "active-vault",
        path: `Note-${index}.md`,
        format: "markdown" as const,
        coverage: "indexed-complete" as const,
        locator: null,
        heading_path: [],
        excerpt: [],
        score: 1,
        frontmatter: {},
        origin: {
          profile: "in_plugin" as const,
          backendInstanceId: "in-plugin-1",
          vaultId: "active-vault",
        },
      })),
      next_cursor: null,
    },
  };
}

function plugin() {
  return {
    app: {
      workspace: { activeEditor: null, getLeaf: vi.fn() },
      vault: { getAbstractFileByPath: vi.fn() },
      fileManager: {},
    },
    settings: {
      defaultMode: "lexical",
      resultLimit: 20,
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
    getActiveBackendIdentity: () => null,
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

function createModal(backend: DeferredBackend): SearchModalLike {
  return new searchModalModule.KwirySearchModal(plugin(), backend, status());
}

beforeEach(() => {
  vi.useFakeTimers();
  vi.stubGlobal("window", globalThis);
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
    expect(query.textContent).toBe("1 result returned — search window complete.");
    expect(query.classList.contains("is-animation-ready")).toBe(false);
    expect(results.attributes.get("aria-busy")).toBe("false");
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
    expect(query.textContent).toBe("7 results returned — more candidates are available.");

    backend.searches[0]!.resolve(execution(0, "exhausted"));
    await expect(older).resolves.toEqual([]);
    expect(query.textContent).toBe("7 results returned — more candidates are available.");
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

    expect(index.textContent).toBe("Index · Reading 4/10 (40%) · 1 in flight");
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
    expect(index.textContent).toBe("Index · Reading 6/10 (60%) · 1 in flight");

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
    expect(index.textContent).toBe("Index · Reading 6/10 (60%) · 1 in flight");
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
