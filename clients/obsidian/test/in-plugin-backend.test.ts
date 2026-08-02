// SPDX-FileCopyrightText: 2026 cybersader
// SPDX-License-Identifier: GPL-3.0-only

import { describe, expect, it, vi } from "vitest";

import {
  type ActiveVaultSource,
  type ExcerptRead,
  type SourceInspection,
  type StableSourceRead,
  type VaultSourceEvent,
} from "../src/active-vault-source";
import type { CacheLoad, CacheStorePort, CacheWrite } from "../src/cache/cache-store";
import { InPluginLexicalBackend } from "../src/backends/in-plugin-lexical-backend";
import {
  CACHE_SCHEMA_VERSION,
  WORKER_PROTOCOL_VERSION,
  type ExportGenerationResult,
} from "../src/worker/protocol";
import { WorkerRpcError } from "../src/worker/rpc-client";
import type { InPluginWorkerSession } from "../src/worker/session";

class FakeSource implements ActiveVaultSource {
  listener: ((event: VaultSourceEvent) => void) | null = null;
  subscriptions = 0;
  unsubscriptions = 0;
  readonly excerptTexts = new Map<string, string>();
  readonly excerptReads: string[] = [];
  excerptFailures = new Set<string>();

  subscribe(listener: (event: VaultSourceEvent) => void): () => void {
    if (this.listener) throw new Error("already subscribed");
    this.listener = listener;
    this.subscriptions += 1;
    return () => {
      if (!this.listener) return;
      this.listener = null;
      this.unsubscriptions += 1;
    };
  }

  listMarkdownPaths(): readonly string[] {
    return [];
  }

  inspectMarkdown(path: string): SourceInspection {
    return { kind: "missing", path };
  }

  async readMarkdown(
    inspection: Extract<SourceInspection, { kind: "candidate" }>,
  ): Promise<StableSourceRead> {
    return { kind: "missing", path: inspection.path };
  }

  async readExcerptText(path: string): Promise<ExcerptRead> {
    this.excerptReads.push(path);
    if (this.excerptFailures.has(path)) throw new Error("vault read failed");
    const text = this.excerptTexts.get(path);
    if (text === undefined) return { kind: "missing", path };
    return { kind: "text", path, text };
  }

  emit(event: VaultSourceEvent): void {
    this.listener?.(event);
  }
}

const CACHE_IDENTITY = "0123456789abcdef".repeat(4);

function cacheHit(generationId = "cached-generation"): Extract<CacheLoad, { kind: "hit" }> {
  return {
    kind: "hit",
    record: {
      generationId,
      byteLength: 4,
      sha256: "a".repeat(64),
      identity: {
        protocol_version: WORKER_PROTOCOL_VERSION,
        cache_schema_version: CACHE_SCHEMA_VERSION,
        chunking_version: 1,
        sqlite_version: "3.53.0",
        sqlite_wasm_sha256: "b".repeat(64),
        rust_wasm_sha256: "c".repeat(64),
        plugin_id: "kwiry-search",
        plugin_version: "0.1.0",
        cache_identity: CACHE_IDENTITY,
      },
    },
    bytes: new Uint8Array([1, 2, 3, 4]),
    digestVerified: false,
  };
}

function exportResult(generation: string): ExportGenerationResult {
  return {
    generation,
    documents: 0,
    chunks: 0,
    bytes: new Uint8Array([1, 2, 3, 4]),
    blob_byte_length: 4,
    blob_sha256: "d".repeat(64),
    protocol_version: WORKER_PROTOCOL_VERSION,
    cache_schema_version: CACHE_SCHEMA_VERSION,
    chunking_version: 1,
    sqlite_version: "3.53.0",
    sqlite_wasm_sha256: "b".repeat(64),
    rust_wasm_sha256: "c".repeat(64),
    plugin_id: "kwiry-search",
    plugin_version: "0.1.0",
    cache_identity: CACHE_IDENTITY,
  };
}

class FakeCacheStore implements CacheStorePort {
  readonly vaultCacheIdentity = CACHE_IDENTITY;
  readonly puts: CacheWrite[] = [];
  readonly discards: Array<"corrupt" | "incompatible" | "requested"> = [];
  putError: unknown = null;
  discardError: unknown = null;

  constructor(readonly loaded: CacheLoad) {}
  async load(): Promise<CacheLoad> { return this.loaded; }
  async put(write: CacheWrite) {
    this.puts.push(write);
    if (this.putError) throw this.putError;
    return {
      generationId: write.generationId,
      byteLength: write.byteLength,
      sha256: write.sha256,
      identity: write.identity,
    };
  }
  async discard(reason: "corrupt" | "incompatible" | "requested"): Promise<void> {
    this.discards.push(reason);
    if (this.discardError) throw this.discardError;
  }
  async dispose(): Promise<void> {}
}

function fakeSession(options: {
  initialize?: () => Promise<unknown>;
  commit?: (generation: string) => Promise<unknown>;
  search?: () => Promise<unknown>;
  restore?: (hit: Extract<CacheLoad, { kind: "hit" }>) => Promise<unknown>;
  plan?: () => Promise<unknown>;
  export?: (generation: string) => Promise<unknown>;
} = {}): InPluginWorkerSession {
  return {
    initialize: vi.fn(options.initialize ?? (async () => ({}))),
    beginBuild: vi.fn(async (generation: string) => ({
      generation,
      documents: 0,
      chunks: 0,
      database_bytes: 0,
      database_byte_limit: 1_000_000,
      quarantined_sources: 0,
      quarantine_fields: [],
    })),
    addSourceBatch: vi.fn(async (generation: string) => ({
      generation,
      documents: 0,
      chunks: 0,
      database_bytes: 0,
      database_byte_limit: 1_000_000,
      quarantined_sources: 0,
      quarantine_fields: [],
    })),
    applySourceChanges: vi.fn(async (
      generation: string,
      nextGeneration: string | null,
    ) => ({
      generation: nextGeneration ?? generation,
      documents: 0,
      chunks: 0,
      database_bytes: 0,
      database_byte_limit: 1_000_000,
      quarantined_sources: 0,
      quarantine_fields: [],
    })),
    commitBuild: vi.fn(options.commit ?? (async (generation: string) => ({
      generation,
      documents: 0,
      chunks: 0,
      database_bytes: 0,
      database_byte_limit: 1_000_000,
      quarantined_sources: 0,
      quarantine_fields: [],
    }))),
    abortBuild: vi.fn(async (generation: string) => ({
      generation,
      documents: 0,
      chunks: 0,
      database_bytes: 0,
      database_byte_limit: 1_000_000,
      quarantined_sources: 0,
      quarantine_fields: [],
    })),
    restoreGeneration: vi.fn(options.restore ?? (async (hit: Extract<CacheLoad, { kind: "hit" }>) => ({
      generation: hit.record.generationId,
      documents: 0,
      chunks: 0,
      database_bytes: 0,
      database_byte_limit: 1_000_000,
      quarantined_sources: 0,
      quarantine_fields: [],
    }))),
    planReconciliation: vi.fn(options.plan ?? (async () => ({
      generation: "cached-generation",
      unchanged: [],
      refresh: [],
      remove: [],
      audit: [],
      stored_source_count: 0,
      matched_source_count: 0,
    }))),
    exportGeneration: vi.fn(options.export ?? (async (generation: string) => exportResult(generation))),
    search: vi.fn(options.search ?? (async () => ({
      generation: "generation-1",
      hits: [{
        chunk_id: "chunk-1",
        vault_id: "active-vault",
        path: "note.md",
        heading_path: ["Heading"],
        score: 1,
        excerpt: "",
        frontmatter: {},
      }],
    }))),
    dispose: vi.fn(async () => ({ closed: true as const })),
    forceDispose: vi.fn(),
  } as unknown as InPluginWorkerSession;
}

function backend(
  source: FakeSource,
  sessions: InPluginWorkerSession[],
  cache?: ConstructorParameters<typeof InPluginLexicalBackend>[0]["cache"],
  onStartupObservation?: ConstructorParameters<typeof InPluginLexicalBackend>[0]["onStartupObservation"],
): InPluginLexicalBackend {
  let generation = 0;
  return new InPluginLexicalBackend({
    instanceId: "in_plugin-2",
    activeVaultId: "active-vault",
    source,
    workerSource: "worker source",
    createSession: () => {
      const session = sessions.shift();
      if (!session) throw new Error("missing fake session");
      return session;
    },
    nextGeneration: () => `generation-${++generation}`,
    yieldControl: () => Promise.resolve(),
    ...(cache ? { cache } : {}),
    ...(onStartupObservation ? { onStartupObservation } : {}),
  });
}

describe("InPluginLexicalBackend", () => {
  it("subscribes before returning and builds a complete initial generation in the background", async () => {
    let release!: () => void;
    const initializing = new Promise<void>((resolve) => {
      release = resolve;
    });
    const source = new FakeSource();
    const session = fakeSession({ initialize: () => initializing });
    const inPlugin = backend(source, [session]);

    await inPlugin.initialize();
    expect(source.subscriptions).toBe(1);
    await expect(inPlugin.status()).resolves.toMatchObject({
      phase: "starting",
      searchable: false,
      capabilities: {
        supportedModes: ["lexical"],
        sourceScope: "active_vault",
        manualRebuild: true,
      },
      issue: { code: "index_building" },
    });

    release();
    await vi.waitFor(async () => {
      await expect(inPlugin.status()).resolves.toMatchObject({
        phase: "ready",
        searchable: true,
        generation: "generation-1",
      });
    });
  });

  it("publishes quarantined counts and validator fields as a degraded searchable status", async () => {
    const source = new FakeSource();
    const session = fakeSession({
      commit: async (generation) => ({
        generation,
        documents: 9,
        chunks: 12,
        database_bytes: 12,
        database_byte_limit: 1_000_000,
        quarantined_sources: 1,
        quarantine_fields: ["chunks_contents"],
      }),
    });
    const startupObservations: unknown[] = [];
    const inPlugin = backend(
      source,
      [session],
      undefined,
      (observation) => startupObservations.push(observation),
    );

    await inPlugin.initialize();
    await vi.waitFor(async () => {
      await expect(inPlugin.status()).resolves.toMatchObject({
        phase: "degraded",
        searchable: true,
        generation: "generation-1",
        documents: 9,
        chunks: 12,
        quarantinedSources: 1,
        unreadableSources: 0,
        quarantineValidatorFields: ["chunks_contents"],
        issue: {
          code: "sources_quarantined",
          safeMessage: "1 note may be missing from search (1 quarantined).",
        },
      });
    });
    expect(startupObservations).toEqual([{
      kind: "terminal",
      outcome: "degraded",
      reason: "sources_omitted",
    }]);
  });

  it("publishes a restored generation as searchable but stale until reconciliation completes", async () => {
    let releasePlan!: () => void;
    const gate = new Promise<void>((resolve) => {
      releasePlan = resolve;
    });
    const source = new FakeSource();
    const session = fakeSession({
      plan: async () => {
        await gate;
        return {
          generation: "cached-generation",
          unchanged: [],
          refresh: [],
          remove: [],
          audit: [],
          stored_source_count: 0,
          matched_source_count: 0,
        };
      },
      search: async () => ({ generation: "cached-generation", hits: [] }),
    });
    const store = new FakeCacheStore(cacheHit());
    const startupObservations: unknown[] = [];
    const inPlugin = backend(source, [session], {
      openStore: async () => ({ kind: "available", store }),
    }, (observation) => startupObservations.push(observation));

    await inPlugin.initialize();
    await vi.waitFor(() => expect(session.planReconciliation).toHaveBeenCalledTimes(1));
    expect(startupObservations).toEqual([{ kind: "cache_searchable", cacheBytes: 4 }]);
    await expect(inPlugin.status()).resolves.toMatchObject({
      phase: "building",
      searchable: true,
      generation: "cached-generation",
      dirty: true,
      progress: {
        stage: "replay",
        subphase: "planning",
        completed: 0,
        total: null,
      },
      issue: {
        code: "index_reconciling",
        safeMessage: "Cached index searchable; reconciling vault changes…",
      },
    });
    await expect(inPlugin.search({ q: "query", mode: "lexical" })).resolves.toMatchObject({
      generation: "cached-generation",
    });

    releasePlan();
    await vi.waitFor(async () => {
      await expect(inPlugin.status()).resolves.toMatchObject({
        phase: "ready",
        searchable: true,
        generation: "cached-generation",
        dirty: false,
      });
    });
    expect(startupObservations).toEqual([
      { kind: "cache_searchable", cacheBytes: 4 },
      { kind: "fully_current" },
    ]);
  });

  it("keeps cache unavailability observable after an explicit clean build", async () => {
    const source = new FakeSource();
    const session = fakeSession();
    const inPlugin = backend(source, [session], {
      openStore: async () => ({ kind: "unavailable", reason: "root_not_writable" }),
    });

    await inPlugin.initialize();
    await vi.waitFor(async () => {
      await expect(inPlugin.status()).resolves.toMatchObject({
        phase: "degraded",
        searchable: true,
        generation: "generation-1",
        dirty: false,
        issue: {
          code: "cache_unavailable",
          safeMessage: "Index is current, but cache durability is unavailable.",
        },
      });
    });
    expect(session.restoreGeneration).not.toHaveBeenCalled();
  });

  it("turns a thrown cache-open failure into the same explicit clean build", async () => {
    const source = new FakeSource();
    const session = fakeSession();
    const inPlugin = backend(source, [session], {
      openStore: async () => {
        throw new Error("platform probe failed");
      },
    });

    await inPlugin.initialize();
    await vi.waitFor(async () => {
      await expect(inPlugin.status()).resolves.toMatchObject({
        phase: "degraded",
        searchable: true,
        generation: "generation-1",
        dirty: false,
        issue: { code: "cache_unavailable" },
      });
    });
  });

  it.each([
    ["cache_digest_mismatch", "corrupt", "cache_corrupt", "Cached index rejected and discarded; building fresh…", "Fresh index is current; replacing the discarded cache…"],
    ["cache_image_invalid", "corrupt", "cache_corrupt", "Cached index rejected and discarded; building fresh…", "Fresh index is current; replacing the discarded cache…"],
    ["cache_blob_too_large", "corrupt", "cache_corrupt", "Cached index rejected and discarded; building fresh…", "Fresh index is current; replacing the discarded cache…"],
    ["cache_version_mismatch", "incompatible", "cache_incompatible", "Cached index is incompatible; building fresh…", "Fresh index is current; replacing the incompatible cache…"],
    ["cache_identity_mismatch", "incompatible", "cache_incompatible", "Cached index is incompatible; building fresh…", "Fresh index is current; replacing the incompatible cache…"],
  ] as const)(
    "discards a %s restore refusal explicitly and completes a clean build",
    async (code, discardReason, issue, buildingMessage, safeMessage) => {
      const source = new FakeSource();
      const refusal = Object.assign(new Error(code), { code });
      const session = fakeSession({ restore: async () => Promise.reject(refusal) });
      const store = new FakeCacheStore(cacheHit());
      const inPlugin = backend(source, [session], {
        openStore: async () => ({ kind: "available", store }),
      });
      const statuses: Array<Awaited<ReturnType<typeof inPlugin.status>>> = [];
      inPlugin.subscribeStatus((status) => statuses.push(status));

      await inPlugin.initialize();
      await vi.waitFor(async () => {
        await expect(inPlugin.status()).resolves.toMatchObject({
          phase: "degraded",
          searchable: true,
          generation: "generation-1",
          dirty: false,
          issue: { code: issue, safeMessage },
        });
      });
      expect(statuses).toContainEqual(expect.objectContaining({
        searchable: false,
        dirty: true,
        issue: expect.objectContaining({ code: issue, safeMessage: buildingMessage }),
      }));
      expect(store.discards).toEqual([discardReason]);
      expect(session.restoreGeneration).toHaveBeenCalledTimes(1);
      expect(session.beginBuild).toHaveBeenCalledWith("generation-1");
      expect(session.commitBuild).toHaveBeenCalledWith("generation-1");
      expect(session.planReconciliation).not.toHaveBeenCalled();
    },
  );

  it.each(["internal_error", "invalid_state"] as const)(
    "reports a %s restore failure as cache-restore-unavailable and builds clean",
    async (code) => {
      const source = new FakeSource();
      const refusal = Object.assign(new Error(code), { code });
      const session = fakeSession({ restore: async () => Promise.reject(refusal) });
      const store = new FakeCacheStore(cacheHit());
      const inPlugin = backend(source, [session], {
        openStore: async () => ({ kind: "available", store }),
      });

      await inPlugin.initialize();
      await vi.waitFor(async () => {
        await expect(inPlugin.status()).resolves.toMatchObject({
          phase: "degraded",
          searchable: true,
          generation: "generation-1",
          dirty: false,
          issue: {
            code: "cache_restore_unavailable",
            safeMessage: "Fresh index is current; replacing the unrestorable cache…",
          },
        });
      });
      expect(store.discards).toEqual([]);
      expect(session.commitBuild).toHaveBeenCalledWith("generation-1");
    },
  );

  it("reports discard failure distinctly while still completing the explicit clean build", async () => {
    const source = new FakeSource();
    const refusal = Object.assign(new Error("digest mismatch"), {
      code: "cache_digest_mismatch",
    });
    const session = fakeSession({ restore: async () => Promise.reject(refusal) });
    const store = new FakeCacheStore(cacheHit());
    store.discardError = new Error("permission denied");
    const inPlugin = backend(source, [session], {
      openStore: async () => ({ kind: "available", store }),
    });

    await inPlugin.initialize();
    await vi.waitFor(async () => {
      await expect(inPlugin.status()).resolves.toMatchObject({
        phase: "degraded",
        searchable: true,
        generation: "generation-1",
        dirty: false,
        issue: {
          code: "cache_discard_failed",
          safeMessage: "Fresh index is current; rejected cache could not be discarded.",
        },
      });
    });
    expect(store.discards).toEqual(["corrupt"]);
    expect(session.commitBuild).toHaveBeenCalledWith("generation-1");
  });

  it("reports cache save failure without changing index freshness or searchability", async () => {
    vi.useFakeTimers();
    try {
      const source = new FakeSource();
      const session = fakeSession();
      const store = new FakeCacheStore({ kind: "miss", reason: "absent" });
      store.putError = new Error("quota");
      const inPlugin = backend(source, [session], {
        openStore: async () => ({ kind: "available", store }),
        idleExportMs: 1,
      });

      await inPlugin.initialize();
      await vi.waitFor(() => expect(session.commitBuild).toHaveBeenCalledTimes(1));
      await vi.advanceTimersByTimeAsync(1);
      await vi.waitFor(async () => {
        await expect(inPlugin.status()).resolves.toMatchObject({
          phase: "degraded",
          searchable: true,
          generation: "generation-1",
          dirty: false,
          issue: { code: "cache_save_failed", safeMessage: "Search ready; cache save failed" },
        });
      });
      await expect(inPlugin.search({ q: "query", mode: "lexical" })).resolves.toMatchObject({
        generation: "generation-1",
      });
    } finally {
      vi.useRealTimers();
    }
  });

  it("emits cached status immediately and starts a manual replacement build", async () => {
    const source = new FakeSource();
    const session = fakeSession();
    const inPlugin = backend(source, [session]);
    const phases: string[] = [];
    const unsubscribe = inPlugin.subscribeStatus((status) => phases.push(status.phase));

    await inPlugin.initialize();
    await vi.waitFor(() => expect(session.commitBuild).toHaveBeenCalledTimes(1));
    await inPlugin.rebuild();
    await vi.waitFor(() => expect(session.commitBuild).toHaveBeenCalledTimes(2));

    expect(phases[0]).toBe("starting");
    expect(phases).toContain("building");
    expect(phases.at(-1)).toBe("ready");
    unsubscribe();
  });

  it("keeps the old active generation searchable and drains events during rebuild", async () => {
    let release!: () => void;
    const gate = new Promise<void>((resolve) => {
      release = resolve;
    });
    const source = new FakeSource();
    const session = fakeSession();
    const originalCommit = session.commitBuild.bind(session);
    let commits = 0;
    session.commitBuild = vi.fn(async (generation: string) => {
      commits += 1;
      if (commits === 2) await gate;
      return originalCommit(generation);
    });
    const inPlugin = backend(source, [session]);
    await inPlugin.initialize();
    await vi.waitFor(async () => {
      await expect(inPlugin.status()).resolves.toMatchObject({
        searchable: true,
        generation: "generation-1",
      });
    });

    await inPlugin.rebuild();
    await vi.waitFor(async () => {
      await expect(inPlugin.status()).resolves.toMatchObject({
        phase: "building",
        searchable: true,
        generation: "generation-1",
        rebuilding: true,
      });
    });
    await expect(inPlugin.search({ q: "query", mode: "lexical" })).resolves.toMatchObject({
      generation: "generation-1",
    });
    source.emit({ kind: "remove", path: "deleted-during-rebuild.md" });
    release();
    await vi.waitFor(async () => {
      await expect(inPlugin.status()).resolves.toMatchObject({
        phase: "ready",
        generation: "generation-3",
        dirty: false,
      });
    });
  });

  it("never accepts semantic or hybrid requests", async () => {
    const source = new FakeSource();
    const inPlugin = backend(source, [fakeSession()]);
    await inPlugin.initialize();
    await vi.waitFor(async () => {
      await expect(inPlugin.status()).resolves.toMatchObject({ searchable: true });
    });

    await expect(inPlugin.search({ q: "query", mode: "semantic" })).rejects.toMatchObject({
      code: "mode_unavailable",
    });
    await expect(inPlugin.search({ q: "query", mode: "hybrid" })).rejects.toMatchObject({
      code: "mode_unavailable",
    });
  });

  it("hydrates excerpts from the vault file and attaches in-plugin result origin", async () => {
    const source = new FakeSource();
    source.excerptTexts.set("note.md", "# Heading\nbefore match after");
    const inPlugin = backend(source, [fakeSession()]);
    await inPlugin.initialize();
    await vi.waitFor(async () => {
      await expect(inPlugin.status()).resolves.toMatchObject({ searchable: true });
    });

    const execution = await inPlugin.search({ q: "match", mode: "lexical", limit: 10 });
    expect(execution).toMatchObject({
      requestedMode: "lexical",
      effectiveMode: "lexical",
      generation: "generation-1",
      response: {
        hits: [{
          excerpt: [
            { text: "before ", highlighted: false },
            { text: "match", highlighted: true },
            { text: " after", highlighted: false },
          ],
          origin: {
            profile: "in_plugin",
            backendInstanceId: "in_plugin-2",
            vaultId: "active-vault",
          },
        }],
      },
    });
    expect(source.excerptReads).toEqual(["note.md"]);
  });

  it("reads each hit path once even when several chunks of a note match", async () => {
    const source = new FakeSource();
    source.excerptTexts.set("note.md", "alpha match beta");
    const session = fakeSession({
      search: async () => ({
        generation: "generation-1",
        hits: [1, 2, 3].map((index) => ({
          chunk_id: `chunk-${index}`,
          vault_id: "active-vault",
          path: "note.md",
          heading_path: [],
          score: index,
          excerpt: "",
          frontmatter: {},
        })),
      }),
    });
    const inPlugin = backend(source, [session]);
    await inPlugin.initialize();
    await vi.waitFor(async () => {
      await expect(inPlugin.status()).resolves.toMatchObject({ searchable: true });
    });

    const execution = await inPlugin.search({ q: "match", mode: "lexical" });
    expect(source.excerptReads).toEqual(["note.md"]);
    expect(execution.response.hits).toHaveLength(3);
    for (const hit of execution.response.hits) {
      expect(hit.excerpt.filter((segment) => segment.highlighted)).toEqual([
        { text: "match", highlighted: true },
      ]);
    }
    // Reading once is not enough: the file must also be located and folded
    // once. Hits sharing a path and heading path share the hydrated result.
    const [first, second, third] = execution.response.hits;
    expect(second!.excerpt).toBe(first!.excerpt);
    expect(third!.excerpt).toBe(first!.excerpt);
  });

  // Files are authoritative. If the file behind a hit cannot be read, the hit
  // stays — with an empty excerpt — rather than the search failing or the
  // excerpt being invented.
  it("degrades a single unreadable or missing file to an empty excerpt", async () => {
    const source = new FakeSource();
    source.excerptTexts.set("readable.md", "readable match here");
    source.excerptFailures.add("broken.md");
    const session = fakeSession({
      search: async () => ({
        generation: "generation-1",
        hits: [
          {
            chunk_id: "chunk-broken",
            vault_id: "active-vault",
            path: "broken.md",
            heading_path: [],
            score: 3,
            excerpt: "",
            frontmatter: {},
          },
          {
            chunk_id: "chunk-gone",
            vault_id: "active-vault",
            path: "deleted.md",
            heading_path: [],
            score: 2,
            excerpt: "",
            frontmatter: {},
          },
          {
            chunk_id: "chunk-ok",
            vault_id: "active-vault",
            path: "readable.md",
            heading_path: [],
            score: 1,
            excerpt: "",
            frontmatter: {},
          },
        ],
      }),
    });
    const inPlugin = backend(source, [session]);
    await inPlugin.initialize();
    await vi.waitFor(async () => {
      await expect(inPlugin.status()).resolves.toMatchObject({ searchable: true });
    });

    const execution = await inPlugin.search({ q: "match", mode: "lexical" });
    const hits = execution.response.hits;
    expect(hits.map((hit) => hit.chunk_id)).toEqual(["chunk-broken", "chunk-gone", "chunk-ok"]);
    expect(hits[0]!.excerpt).toEqual([]);
    expect(hits[1]!.excerpt).toEqual([]);
    expect(hits[2]!.excerpt).toEqual([
      { text: "readable ", highlighted: false },
      { text: "match", highlighted: true },
      { text: " here", highlighted: false },
    ]);
  });

  it("rejects a search whose hydration finishes after disposal", async () => {
    const source = new FakeSource();
    let release!: () => void;
    const gate = new Promise<void>((resolve) => {
      release = resolve;
    });
    const inPlugin = backend(source, [fakeSession()]);
    source.readExcerptText = async (path: string) => {
      await gate;
      return { kind: "missing", path };
    };
    await inPlugin.initialize();
    await vi.waitFor(async () => {
      await expect(inPlugin.status()).resolves.toMatchObject({ searchable: true });
    });

    const pending = inPlugin.search({ q: "match", mode: "lexical" });
    await inPlugin.dispose();
    release();
    await expect(pending).rejects.toMatchObject({ code: "disposed" });
  });

  it("keeps the active generation searchable after a definitive live update failure", async () => {
    const source = new FakeSource();
    const session = fakeSession();
    session.applySourceChanges = vi.fn(async () => {
      throw new WorkerRpcError({
        code: "source_rejected",
        stage: "index",
        message: "private detail",
        retryable: false,
      });
    });
    const inPlugin = backend(source, [session]);
    await inPlugin.initialize();
    await vi.waitFor(async () => {
      await expect(inPlugin.status()).resolves.toMatchObject({ searchable: true });
    });

    source.emit({ kind: "remove", path: "deleted.md" });
    await vi.waitFor(async () => {
      await expect(inPlugin.status()).resolves.toMatchObject({
        phase: "degraded",
        searchable: true,
        generation: "generation-1",
        dirty: true,
        issue: {
          code: "index_update_failed",
          safeMessage: "The in-plugin lexical index could not be updated.",
        },
      });
    });
    await expect(inPlugin.search({ q: "query", mode: "lexical" })).resolves.toMatchObject({
      generation: "generation-1",
    });
  });

  it("reports capacity exhaustion while preserving the active generation", async () => {
    const source = new FakeSource();
    const session = fakeSession();
    session.applySourceChanges = vi.fn(async () => {
      throw new WorkerRpcError({
        code: "index_limit_exceeded",
        stage: "index",
        message: "private detail",
        retryable: false,
      });
    });
    const inPlugin = backend(source, [session]);
    await inPlugin.initialize();
    await vi.waitFor(async () => {
      await expect(inPlugin.status()).resolves.toMatchObject({ searchable: true });
    });

    source.emit({ kind: "remove", path: "deleted.md" });
    await vi.waitFor(async () => {
      await expect(inPlugin.status()).resolves.toMatchObject({
        phase: "degraded",
        searchable: true,
        generation: "generation-1",
        issue: {
          code: "index_limit_exceeded",
          safeMessage: "The in-plugin lexical index reached its capacity limit.",
        },
      });
    });
  });

  it("replaces the Worker after an uncertain active mutation", async () => {
    const source = new FakeSource();
    const failed = fakeSession();
    failed.applySourceChanges = vi.fn(async () => {
      throw new WorkerRpcError({
        code: "timeout",
        stage: "lifecycle",
        message: "private detail",
        retryable: true,
      });
    });
    const replacement = fakeSession();
    const inPlugin = backend(source, [failed, replacement]);
    await inPlugin.initialize();
    await vi.waitFor(async () => {
      await expect(inPlugin.status()).resolves.toMatchObject({ generation: "generation-1" });
    });

    source.emit({ kind: "remove", path: "deleted.md" });
    await vi.waitFor(async () => {
      await expect(inPlugin.status()).resolves.toMatchObject({
        phase: "ready",
        searchable: true,
        generation: "generation-3",
      });
    });

    expect(failed.forceDispose).toHaveBeenCalledTimes(1);
    expect(source.unsubscriptions).toBe(1);
    expect(source.subscriptions).toBe(2);
  });

  it("recovers an uncertain initial Worker failure through one fresh complete build", async () => {
    const source = new FakeSource();
    const failed = fakeSession({
      initialize: async () => {
        throw new WorkerRpcError({
          code: "timeout",
          stage: "lifecycle",
          message: "private detail",
          retryable: true,
        });
      },
    });
    const replacement = fakeSession();
    const inPlugin = backend(source, [failed, replacement]);

    await inPlugin.initialize();
    await vi.waitFor(async () => {
      await expect(inPlugin.status()).resolves.toMatchObject({
        phase: "ready",
        searchable: true,
        generation: "generation-1",
      });
    });

    expect(failed.forceDispose).toHaveBeenCalledTimes(1);
    expect(source.unsubscriptions).toBe(1);
    expect(source.subscriptions).toBe(2);
  });

  it("recovers when an uncertain build failure is followed by a definitive abort error", async () => {
    const source = new FakeSource();
    const failed = fakeSession();
    failed.commitBuild = vi.fn(async () => {
      throw new WorkerRpcError({
        code: "timeout",
        stage: "lifecycle",
        message: "private detail",
        retryable: true,
      });
    });
    failed.abortBuild = vi.fn(async () => {
      throw new WorkerRpcError({
        code: "invalid_state",
        stage: "index",
        message: "private detail",
        retryable: false,
      });
    });
    const replacement = fakeSession();
    const inPlugin = backend(source, [failed, replacement]);

    await inPlugin.initialize();
    await vi.waitFor(async () => {
      await expect(inPlugin.status()).resolves.toMatchObject({
        phase: "ready",
        searchable: true,
        generation: "generation-2",
      });
    });

    expect(failed.forceDispose).toHaveBeenCalledTimes(1);
  });

  it("reports a definitive replacement-build failure instead of claiming endless recovery", async () => {
    const source = new FakeSource();
    const failed = fakeSession({
      initialize: async () => {
        throw new WorkerRpcError({
          code: "worker_crashed",
          stage: "lifecycle",
          message: "private detail",
          retryable: true,
        });
      },
    });
    const replacement = fakeSession();
    replacement.beginBuild = vi.fn(async () => {
      throw new WorkerRpcError({
        code: "source_rejected",
        stage: "index",
        message: "private detail",
        retryable: false,
      });
    });
    const inPlugin = backend(source, [failed, replacement]);

    await inPlugin.initialize();
    await vi.waitFor(async () => {
      await expect(inPlugin.status()).resolves.toMatchObject({
        phase: "unavailable",
        searchable: false,
        issue: {
          code: "index_build_failed",
          safeMessage: "The in-plugin lexical index could not be built.",
        },
      });
    });
  });

  it("terminates an uncertain Worker before starting one fresh recovery build", async () => {
    const source = new FakeSource();
    const failed = fakeSession({
      search: async () => {
        throw new WorkerRpcError({
          code: "worker_crashed",
          stage: "lifecycle",
          message: "private detail",
          retryable: true,
        });
      },
    });
    const replacement = fakeSession();
    const inPlugin = backend(source, [failed, replacement]);
    await inPlugin.initialize();
    await vi.waitFor(async () => {
      await expect(inPlugin.status()).resolves.toMatchObject({ searchable: true });
    });

    await expect(inPlugin.search({ q: "query", mode: "lexical" })).rejects.toMatchObject({
      code: "worker_recovering",
      safeMessage: "In-plugin search Worker is recovering.",
    });
    expect(failed.forceDispose).toHaveBeenCalledTimes(1);
    expect(source.unsubscriptions).toBe(1);
    expect(source.subscriptions).toBe(2);
    await vi.waitFor(async () => {
      await expect(inPlugin.status()).resolves.toMatchObject({
        phase: "ready",
        searchable: true,
        generation: "generation-2",
      });
    });
  });

  it("prevents a held initial commit from publishing after disposal", async () => {
    let release!: () => void;
    const gate = new Promise<void>((resolve) => {
      release = resolve;
    });
    const source = new FakeSource();
    const session = fakeSession();
    const originalCommit = session.commitBuild.bind(session);
    session.commitBuild = vi.fn(async (generation: string) => {
      await gate;
      return originalCommit(generation);
    });
    const inPlugin = backend(source, [session]);

    await inPlugin.initialize();
    await vi.waitFor(() => expect(session.commitBuild).toHaveBeenCalledTimes(1));
    await inPlugin.dispose();
    release();
    await Promise.resolve();
    await Promise.resolve();

    expect(source.unsubscriptions).toBe(1);
    expect(session.forceDispose).toHaveBeenCalledTimes(1);
    await expect(inPlugin.status()).resolves.toMatchObject({
      phase: "disposed",
      searchable: false,
    });
  });

  it("rejects a search that completes after backend disposal", async () => {
    let release!: () => void;
    const pendingSearch = new Promise<unknown>((resolve) => {
      release = () => resolve({ generation: "generation-1", hits: [] });
    });
    const source = new FakeSource();
    const session = fakeSession({ search: () => pendingSearch });
    const inPlugin = backend(source, [session]);
    await inPlugin.initialize();
    await vi.waitFor(async () => {
      await expect(inPlugin.status()).resolves.toMatchObject({ searchable: true });
    });

    const pending = inPlugin.search({ q: "query", mode: "lexical" });
    await inPlugin.dispose();
    release();

    await expect(pending).rejects.toMatchObject({ code: "disposed" });
  });

  it("returns listener and Worker counts to zero across repeated lifecycles", async () => {
    for (let cycle = 0; cycle < 10; cycle += 1) {
      const source = new FakeSource();
      const session = fakeSession();
      const inPlugin = backend(source, [session]);
      await inPlugin.initialize();
      await vi.waitFor(async () => {
        await expect(inPlugin.status()).resolves.toMatchObject({ phase: "ready" });
      });
      await inPlugin.dispose();
      expect(source.listener).toBeNull();
      expect(source.subscriptions).toBe(1);
      expect(source.unsubscriptions).toBe(1);
      expect(session.forceDispose).toHaveBeenCalledTimes(1);
    }
  });

  it("detaches the vault and force-disposes the Worker exactly once", async () => {
    const source = new FakeSource();
    const session = fakeSession();
    const inPlugin = backend(source, [session]);
    await inPlugin.initialize();
    await inPlugin.dispose();
    await inPlugin.dispose();

    expect(source.unsubscriptions).toBe(1);
    expect(session.forceDispose).toHaveBeenCalledTimes(1);
    await expect(inPlugin.status()).resolves.toMatchObject({ phase: "disposed" });
  });
});
