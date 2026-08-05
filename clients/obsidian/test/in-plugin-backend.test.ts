// SPDX-FileCopyrightText: 2026 cybersader
// SPDX-License-Identifier: GPL-3.0-only

import { describe, expect, it, vi } from "vitest";

import {
  type ActiveVaultSource,
  type ExcerptRead,
  type SourceInspection,
  type SourceReadOutcome,
  type VaultSourceEvent,
} from "../src/active-vault-source";
import {
  INITIAL_BUILD_CHECKPOINT_ORDERING_VERSION,
  INITIAL_BUILD_CHECKPOINT_RECORD_KIND,
  INITIAL_BUILD_CHECKPOINT_RECORD_VERSION,
  type CacheLoad,
  type CacheStoreBundlePort,
  type CacheWrite,
  type InitialBuildCheckpointLoad,
  type InitialBuildCheckpointToken,
  type InitialBuildCheckpointWrite,
} from "../src/cache/cache-store";
import { InPluginLexicalBackend } from "../src/backends/in-plugin-lexical-backend";
import {
  CACHE_SCHEMA_VERSION,
  INITIAL_BUILD_CHECKPOINT_IMAGE_VERSION,
  WORKER_PROTOCOL_VERSION,
  emptySourceFormatCounts,
  type ExportGenerationResult,
  type InitialBuildCheckpointCursor,
  type InitialBuildCheckpointExportResult,
} from "../src/worker/protocol";
import { WorkerRpcError } from "../src/worker/rpc-client";
import type { InPluginWorkerSession } from "../src/worker/session";

class FakeSource implements ActiveVaultSource {
  listener: ((event: VaultSourceEvent) => void) | null = null;
  subscriptions = 0;
  unsubscriptions = 0;
  readonly excerptTexts = new Map<string, string>();
  readonly excerptReads: string[] = [];
  readonly records = new Map<string, { bytes: Uint8Array; mtime: number }>();
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

  listSourcePaths(): readonly string[] {
    return [...this.records.keys()];
  }

  inspectSource(path: string): SourceInspection {
    const record = this.records.get(path);
    if (!record) return { kind: "missing", path };
    return {
      kind: "candidate",
      path,
      format: "markdown",
      size: record.bytes.byteLength,
      mtime: record.mtime,
    };
  }

  async readSource(
    inspection: Extract<SourceInspection, { kind: "candidate" }>,
  ): Promise<SourceReadOutcome> {
    const record = this.records.get(inspection.path);
    if (!record) return { kind: "missing", path: inspection.path };
    return {
      kind: "source",
      source: {
        descriptor: {
          vault_id: "active-vault",
          path: inspection.path,
          format: "markdown",
          byte_length: record.bytes.byteLength,
          mtime: Math.floor(record.mtime / 1_000),
          mtime_nanos: (BigInt(record.mtime) * 1_000_000n).toString(),
        },
        bytes: record.bytes,
      },
    };
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

  set(path: string, text: string, mtime = 1): void {
    this.records.set(path, { bytes: new TextEncoder().encode(text), mtime });
  }
}

const CACHE_IDENTITY = "0123456789abcdef".repeat(4);
const SOURCE_POLICY_HASH = "e".repeat(64);

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
        source_policy_hash: SOURCE_POLICY_HASH,
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
    source_policy_hash: SOURCE_POLICY_HASH,
  };
}

function checkpointExportResult(
  generation: string,
  cursor: InitialBuildCheckpointCursor,
): InitialBuildCheckpointExportResult {
  return {
    generation,
    documents: cursor.acknowledged_prefix_sources,
    chunks: cursor.acknowledged_prefix_sources,
    database_bytes: 1,
    database_byte_limit: 1_000_000,
    quarantined_sources: 0,
    quarantine_fields: [],
    source_format_counts: emptySourceFormatCounts(),
    record_kind: INITIAL_BUILD_CHECKPOINT_RECORD_KIND,
    checkpoint_record_version: INITIAL_BUILD_CHECKPOINT_RECORD_VERSION,
    checkpoint_image_version: INITIAL_BUILD_CHECKPOINT_IMAGE_VERSION,
    publication: "initial_staging",
    searchable: false,
    cursor,
    bytes: new Uint8Array([5, 6, 7, 8]),
    blob_byte_length: 4,
    blob_sha256: "f".repeat(64),
    protocol_version: WORKER_PROTOCOL_VERSION,
    cache_schema_version: CACHE_SCHEMA_VERSION,
    chunking_version: 1,
    sqlite_version: "3.53.0",
    sqlite_wasm_sha256: "b".repeat(64),
    rust_wasm_sha256: "c".repeat(64),
    plugin_id: "kwiry-search",
    plugin_version: "0.1.0",
    cache_identity: CACHE_IDENTITY,
    source_policy_hash: SOURCE_POLICY_HASH,
  };
}

class FakeCacheStore implements CacheStoreBundlePort {
  readonly vaultCacheIdentity = CACHE_IDENTITY;
  readonly puts: CacheWrite[] = [];
  readonly discards: Array<"corrupt" | "incompatible" | "requested"> = [];
  readonly checkpointPuts: InitialBuildCheckpointWrite[] = [];
  putError: unknown = null;
  discardError: unknown = null;

  constructor(
    readonly loaded: CacheLoad,
    private readonly events: string[] | null = null,
  ) {}
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
  async loadInitialBuildCheckpoint(): Promise<InitialBuildCheckpointLoad> {
    return { kind: "miss", reason: "absent" };
  }
  async putInitialBuildCheckpoint(write: InitialBuildCheckpointWrite) {
    this.events?.push("store-put");
    this.checkpointPuts.push(write);
    const { bytes: _bytes, ...record } = write;
    return record;
  }
  async discardInitialBuildCheckpoint(
    _reason: "corrupt" | "incompatible" | "completed" | "requested",
    _expected: InitialBuildCheckpointToken,
  ): Promise<void> {}
  async dispose(): Promise<void> {
    this.events?.push("store-dispose");
  }
}

function fakeSession(options: {
  initialize?: () => Promise<unknown>;
  add?: (generation: string) => Promise<unknown>;
  commit?: (generation: string) => Promise<unknown>;
  search?: () => Promise<unknown>;
  restore?: (hit: Extract<CacheLoad, { kind: "hit" }>) => Promise<unknown>;
  plan?: () => Promise<unknown>;
  export?: (generation: string) => Promise<unknown>;
  checkpointExport?: (
    generation: string,
    cursor: InitialBuildCheckpointCursor,
  ) => Promise<unknown>;
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
      source_format_counts: emptySourceFormatCounts(),
    })),
    addSourceBatch: vi.fn(options.add ?? (async (generation: string) => ({
      generation,
      documents: 0,
      chunks: 0,
      database_bytes: 0,
      database_byte_limit: 1_000_000,
      quarantined_sources: 0,
      quarantine_fields: [],
      source_format_counts: emptySourceFormatCounts(),
    }))),
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
      source_format_counts: emptySourceFormatCounts(),
    })),
    commitBuild: vi.fn(options.commit ?? (async (generation: string) => ({
      generation,
      documents: 0,
      chunks: 0,
      database_bytes: 0,
      database_byte_limit: 1_000_000,
      quarantined_sources: 0,
      quarantine_fields: [],
      source_format_counts: emptySourceFormatCounts(),
    }))),
    abortBuild: vi.fn(async (generation: string) => ({
      generation,
      documents: 0,
      chunks: 0,
      database_bytes: 0,
      database_byte_limit: 1_000_000,
      quarantined_sources: 0,
      quarantine_fields: [],
      source_format_counts: emptySourceFormatCounts(),
    })),
    restoreGeneration: vi.fn(options.restore ?? (async (hit: Extract<CacheLoad, { kind: "hit" }>) => ({
      generation: hit.record.generationId,
      documents: 0,
      chunks: 0,
      database_bytes: 0,
      database_byte_limit: 1_000_000,
      quarantined_sources: 0,
      quarantine_fields: [],
      source_format_counts: emptySourceFormatCounts(),
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
    exportInitialBuildCheckpoint: vi.fn(async (
      generation: string,
      _cacheIdentity: string,
      cursor: InitialBuildCheckpointCursor,
    ) => options.checkpointExport
      ? options.checkpointExport(generation, cursor)
      : checkpointExportResult(generation, cursor)),
    restoreInitialBuildCheckpoint: vi.fn(),
    planInitialBuildCheckpointReconciliation: vi.fn(),
    search: vi.fn(async () => {
      const result = await (options.search ?? (async () => ({
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
      })))();
      if (typeof result !== "object" || result === null || "candidate_window" in result) {
        return result;
      }
      return {
        ...result,
        candidate_window: {
          state: "unknown",
          candidate_count: 0,
          candidate_limit: 512,
        },
      };
    }),
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
    ...(cache ? { cache: { ...cache, sourcePolicyHash: SOURCE_POLICY_HASH } } : {}),
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

  it("records first progress only when the empty-vault inventory total is known", async () => {
    let release!: () => void;
    const initializing = new Promise<void>((resolve) => {
      release = resolve;
    });
    const source = new FakeSource();
    const observations: unknown[] = [];
    const inPlugin = backend(
      source,
      [fakeSession({ initialize: () => initializing })],
      undefined,
      (observation) => observations.push(observation),
    );

    await inPlugin.initialize();
    expect(observations).toEqual([]);
    release();
    await vi.waitFor(async () => {
      await expect(inPlugin.status()).resolves.toMatchObject({
        phase: "ready",
        searchable: true,
      });
    });
    expect(observations).toEqual([
      { kind: "first_progress" },
      { kind: "fully_current" },
    ]);
  });

  it("reports a manual request during a healthy initial build as already building", async () => {
    let release!: () => void;
    const initializing = new Promise<void>((resolve) => {
      release = resolve;
    });
    const source = new FakeSource();
    const session = fakeSession({ initialize: () => initializing });
    const inPlugin = backend(source, [session]);

    await inPlugin.initialize();
    await expect(inPlugin.rebuild()).resolves.toBe("already_building");
    expect(session.beginBuild).not.toHaveBeenCalled();

    release();
    await vi.waitFor(() => expect(session.commitBuild).toHaveBeenCalledTimes(1));
    expect(session.beginBuild).toHaveBeenCalledTimes(1);
  });

  it("publishes a path-free timeout stall while pre-ready search stays index_building", async () => {
    const source = new FakeSource();
    source.set("private-looking-name.md", "body");
    source.readSource = vi.fn(async () => ({
      kind: "timeout" as const,
      underlyingSettled: Promise.resolve(),
    }));
    const inPlugin = backend(source, [fakeSession()]);

    await inPlugin.initialize();
    await vi.waitFor(async () => {
      await expect(inPlugin.status()).resolves.toMatchObject({
        phase: "unavailable",
        searchable: false,
        generation: null,
        issue: { code: "vault_read_failed", recoverable: true },
        progress: {
          stage: "failed",
          activity: "read",
          completed: 0,
          total: 1,
          inFlight: 0,
          stallCategory: "source_read_timeout",
        },
      });
    });
    const status = await inPlugin.status();
    expect(JSON.stringify(status)).not.toContain("private-looking-name.md");
    await expect(inPlugin.search({ q: "query", mode: "lexical" })).rejects.toMatchObject({
      code: "index_building",
      safeMessage: "In-plugin lexical index is still building.",
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
        source_format_counts: emptySourceFormatCounts(),
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
    expect(startupObservations).toEqual([
      { kind: "first_progress" },
      {
        kind: "terminal",
        outcome: "degraded",
        reason: "sources_omitted",
      },
    ]);
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
    expect(startupObservations).toEqual([
      { kind: "cache_searchable", cacheBytes: 4 },
      { kind: "first_progress" },
    ]);
    await expect(inPlugin.status()).resolves.toMatchObject({
      phase: "building",
      searchable: true,
      generation: "cached-generation",
      dirty: true,
      progress: {
        stage: "replay",
        subphase: "planning",
        completed: 0,
        total: 0,
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
      { kind: "first_progress" },
      { kind: "fully_current" },
    ]);
  });

  it.each([
    [null, "cache_absent"],
    ["cache_digest_mismatch", "cache_corrupt"],
    ["cache_version_mismatch", "cache_incompatible"],
  ] as const)(
    "keeps %s support detail in status while pre-ready search returns public index_building",
    async (restoreCode, expectedIssue) => {
      let releaseAdd!: () => void;
      const addGate = new Promise<void>((resolve) => {
        releaseAdd = resolve;
      });
      const source = new FakeSource();
      source.set("private-looking-name.md", "body");
      const session = fakeSession({
        add: async (generation) => {
          await addGate;
          return {
            generation,
            documents: 1,
            chunks: 1,
            database_bytes: 1,
            database_byte_limit: 1_000_000,
            quarantined_sources: 0,
            quarantine_fields: [],
            source_format_counts: emptySourceFormatCounts(),
          };
        },
        ...(restoreCode === null
          ? {}
          : {
              restore: async () => Promise.reject(Object.assign(new Error(restoreCode), {
                code: restoreCode,
              })),
            }),
      });
      const store = new FakeCacheStore(restoreCode === null
        ? { kind: "miss", reason: "absent" }
        : cacheHit());
      const inPlugin = backend(source, [session], {
        openStore: async () => ({ kind: "available", store }),
      });

      await inPlugin.initialize();
      await vi.waitFor(async () => {
        await expect(inPlugin.status()).resolves.toMatchObject({
          searchable: false,
          issue: { code: expectedIssue },
        });
      });
      await expect(inPlugin.search({ q: "query", mode: "lexical" })).rejects.toMatchObject({
        code: "index_building",
        profile: "in_plugin",
        stage: "index",
        retryable: true,
        safeMessage: "In-plugin lexical index is still building.",
      });
      await expect(inPlugin.status()).resolves.toMatchObject({
        issue: { code: expectedIssue },
      });

      releaseAdd();
      await vi.waitFor(async () => {
        await expect(inPlugin.status()).resolves.toMatchObject({ searchable: true });
      });
    },
  );

  it("keeps a clean initial publication ready while cache persistence finishes", async () => {
    vi.useFakeTimers();
    try {
      const source = new FakeSource();
      const session = fakeSession();
      const store = new FakeCacheStore({ kind: "miss", reason: "absent" });
      const inPlugin = backend(source, [session], {
        openStore: async () => ({ kind: "available", store }),
        idleExportMs: 10,
      });

      await inPlugin.initialize();
      await vi.waitFor(() => expect(session.commitBuild).toHaveBeenCalledTimes(1));
      await expect(inPlugin.status()).resolves.toMatchObject({
        phase: "ready",
        searchable: true,
        dirty: false,
        issue: {
          code: "cache_absent",
          safeMessage: "Index is current; cached durability is pending.",
        },
      });
      await expect(inPlugin.search({ q: "query", mode: "lexical" })).resolves.toMatchObject({
        generation: "generation-1",
      });

      await vi.advanceTimersByTimeAsync(10);
      await vi.waitFor(() => expect(store.puts).toHaveLength(1));
      await expect(inPlugin.status()).resolves.toMatchObject({
        phase: "ready",
        searchable: true,
        dirty: false,
      });
    } finally {
      vi.useRealTimers();
    }
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
          phase: "ready",
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
          phase: "ready",
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
    await expect(inPlugin.rebuild()).resolves.toBe("scheduled");
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

    await expect(inPlugin.rebuild()).resolves.toBe("scheduled");
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

  it("hydrates excerpts from stored chunk content and attaches in-plugin result origin", async () => {
    const source = new FakeSource();
    const inPlugin = backend(source, [fakeSession({
      search: async () => ({
        generation: "generation-1",
        hits: [{
          chunk_id: "chunk-1",
          vault_id: "active-vault",
          path: "note.md",
          format: "markdown",
          coverage: "indexed-complete",
          locator: null,
          heading_path: ["Heading"],
          score: 1,
          excerpt: "before match after",
          frontmatter: {},
        }],
        candidate_window: {
          state: "more_available",
          candidate_count: 11,
          candidate_limit: 512,
        },
      }),
    })]);
    await inPlugin.initialize();
    await vi.waitFor(async () => {
      await expect(inPlugin.status()).resolves.toMatchObject({ searchable: true });
    });

    const execution = await inPlugin.search({ q: "match", mode: "lexical", limit: 10 });
    expect(execution).toMatchObject({
      requestedMode: "lexical",
      effectiveMode: "lexical",
      generation: "generation-1",
      candidateWindow: {
        state: "more_available",
        candidateCount: 11,
        candidateLimit: 512,
      },
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
    expect(source.excerptReads).toEqual([]);
  });

  it("hydrates each matching chunk without rereading the vault path", async () => {
    const source = new FakeSource();
    const session = fakeSession({
      search: async () => ({
        generation: "generation-1",
        hits: [1, 2, 3].map((index) => ({
          chunk_id: `chunk-${index}`,
          vault_id: "active-vault",
          path: "note.md",
          format: "markdown" as const,
          coverage: "indexed-complete" as const,
          locator: null,
          heading_path: [],
          score: index,
          excerpt: `alpha ${index} match beta`,
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
    expect(source.excerptReads).toEqual([]);
    expect(execution.response.hits).toHaveLength(3);
    for (const hit of execution.response.hits) {
      expect(hit.excerpt.filter((segment) => segment.highlighted)).toEqual([
        { text: "match", highlighted: true },
      ]);
    }
    const [first, second, third] = execution.response.hits;
    expect(first!.excerpt).not.toBe(second!.excerpt);
    expect(second!.excerpt).not.toBe(third!.excerpt);
  });

  // Stored chunks are authoritative for presentation after final hit selection;
  // current vault readability cannot erase a valid indexed excerpt.
  it("uses stored excerpts even when current vault files are unreadable or missing", async () => {
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
            format: "markdown",
            coverage: "indexed-complete",
            locator: null,
            heading_path: [],
            score: 3,
            excerpt: "broken stored match",
            frontmatter: {},
          },
          {
            chunk_id: "chunk-gone",
            vault_id: "active-vault",
            path: "deleted.md",
            format: "markdown",
            coverage: "indexed-complete",
            locator: null,
            heading_path: [],
            score: 2,
            excerpt: "deleted stored match",
            frontmatter: {},
          },
          {
            chunk_id: "chunk-ok",
            vault_id: "active-vault",
            path: "readable.md",
            format: "markdown",
            coverage: "indexed-complete",
            locator: null,
            heading_path: [],
            score: 1,
            excerpt: "readable match here",
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
    expect(source.excerptReads).toEqual([]);
    for (const hit of hits) {
      expect(hit.excerpt.filter((segment) => segment.highlighted)).toEqual([
        { text: "match", highlighted: true },
      ]);
    }
  });

  it("rejects a search whose hydration finishes after disposal", async () => {
    const source = new FakeSource();
    let release!: () => void;
    const gate = new Promise<void>((resolve) => {
      release = resolve;
    });
    const inPlugin = backend(source, [fakeSession({
      search: async () => {
        await gate;
        return {
          generation: "generation-1",
          hits: [{
            chunk_id: "chunk-1",
            vault_id: "active-vault",
            path: "note.md",
            format: "markdown",
            coverage: "indexed-complete",
            locator: null,
            heading_path: [],
            score: 1,
            excerpt: "stored match",
            frontmatter: {},
          }],
        };
      },
    })]);
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

  it.each([
    [
      "explicit_query_unsupported",
      "query",
      false,
      "This explicit query is unavailable in the in-plugin backend.",
    ],
    [
      "invalid_query",
      "query",
      false,
      "The query is invalid or exceeds the supported limits.",
    ],
    [
      "invalid_query_plan",
      "protocol",
      false,
      "The in-plugin backend produced an invalid query plan.",
    ],
    [
      "query_execution_failed",
      "query",
      true,
      "In-plugin lexical search could not complete.",
    ],
  ] as const)(
    "preserves the structured %s Worker query failure",
    async (code, stage, retryable, safeMessage) => {
      const source = new FakeSource();
      const session = fakeSession({
        search: async () => {
          throw new WorkerRpcError({
            code,
            stage: code === "invalid_query_plan" ? "rust" : "query",
            message: "private query, SQL, or Rust detail",
            retryable,
          });
        },
      });
      const inPlugin = backend(source, [session]);
      await inPlugin.initialize();
      await vi.waitFor(async () => {
        await expect(inPlugin.status()).resolves.toMatchObject({ searchable: true });
      });

      const rejected = await inPlugin.search({ q: "private query", mode: "lexical" })
        .catch((error: unknown) => error);
      expect(rejected).toMatchObject({ code, stage, retryable, safeMessage });
      expect(JSON.stringify(rejected)).not.toMatch(/private query|SQL|Rust detail/u);
    },
  );

  it("rejects an unavailable mode without entering the Worker query lane", async () => {
    const source = new FakeSource();
    const session = fakeSession();
    const inPlugin = backend(source, [session]);
    await inPlugin.initialize();
    await vi.waitFor(async () => {
      await expect(inPlugin.status()).resolves.toMatchObject({ searchable: true });
    });

    await expect(inPlugin.search({ q: "query", mode: "semantic" })).rejects.toMatchObject({
      code: "mode_unavailable",
      stage: "query",
      retryable: false,
    });
    expect(session.search).not.toHaveBeenCalled();
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

  it("persists the last acknowledged initial-build cursor before Worker termination and store disposal", async () => {
    const events: string[] = [];
    let reportAddEntered!: () => void;
    const addEntered = new Promise<void>((resolve) => {
      reportAddEntered = resolve;
    });
    let releaseAdd!: () => void;
    const addGate = new Promise<void>((resolve) => {
      releaseAdd = resolve;
    });
    const source = new FakeSource();
    source.set("note.md", "note", 1);
    const session = fakeSession({
      add: async (generation) => {
        events.push("add-start");
        reportAddEntered();
        await addGate;
        events.push("add-ack");
        return {
          generation,
          documents: 1,
          chunks: 1,
          database_bytes: 1,
          database_byte_limit: 1_000_000,
          quarantined_sources: 0,
          quarantine_fields: [],
          source_format_counts: emptySourceFormatCounts(),
        };
      },
      checkpointExport: async (generation, cursor) => {
        events.push("checkpoint-export");
        return checkpointExportResult(generation, cursor);
      },
    });
    vi.mocked(session.forceDispose).mockImplementation(() => {
      events.push("worker-force-dispose");
    });
    const store = new FakeCacheStore({ kind: "miss", reason: "absent" }, events);
    const inPlugin = backend(source, [session], {
      openStore: async () => ({ kind: "available", store }),
    });

    await inPlugin.initialize();
    await addEntered;
    const disposing = inPlugin.dispose();
    releaseAdd();
    await disposing;

    expect(store.checkpointPuts).toHaveLength(1);
    expect(store.checkpointPuts[0]?.cursor).toEqual({
      snapshot_source_count: 1,
      acknowledged_add_batches: 1,
      acknowledged_prefix_sources: 1,
      last_acknowledged_path: "note.md",
    });
    expect(events).toEqual([
      "add-start",
      "add-ack",
      "checkpoint-export",
      "store-put",
      "worker-force-dispose",
      "store-dispose",
    ]);
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
