// SPDX-FileCopyrightText: 2026 cybersader
// SPDX-License-Identifier: GPL-3.0-only

import { describe, expect, it, vi } from "vitest";

import {
  ACTIVE_VAULT_ID,
  type ActiveVaultSource,
  type ExcerptRead,
  type SourceInspection,
  type StableSourceRead,
  type VaultSourceEvent,
} from "../src/active-vault-source";
import type { CacheLoad, CacheStorePort, CacheWrite } from "../src/cache/cache-store";
import {
  InPluginIndexController,
  type IndexControllerCacheOptions,
  type IndexControllerStatus,
  type IndexCounts,
  type IndexWorkerPort,
  type SourceRemoval,
} from "../src/backends/in-plugin-index-controller";
import {
  CACHE_SCHEMA_VERSION,
  WORKER_PROTOCOL_VERSION,
  type ExportGenerationResult,
  type ReconciliationPlanResult,
  type ReconciliationSourceMetadata,
  type SourceInput,
  type SourceUpsert,
} from "../src/worker/protocol";
import { WorkerRpcError } from "../src/worker/rpc-client";

class FakeSource implements ActiveVaultSource {
  readonly log: string[] = [];
  readonly records = new Map<string, { bytes: Uint8Array; mtime: number }>();
  listener: ((event: VaultSourceEvent) => void) | null = null;
  onRead: ((path: string) => void) | null = null;
  staleReads = new Set<string>();
  oversizedPaths = new Set<string>();
  unsubscribed = 0;

  subscribe(listener: (event: VaultSourceEvent) => void): () => void {
    this.log.push("subscribe");
    this.listener = listener;
    return () => {
      if (this.listener === null) return;
      this.listener = null;
      this.unsubscribed += 1;
    };
  }

  listMarkdownPaths(): readonly string[] {
    this.log.push("list");
    return [...this.records.keys()].reverse();
  }

  // Indexing never hydrates excerpts; this exists only to satisfy the port.
  async readExcerptText(path: string): Promise<ExcerptRead> {
    return { kind: "missing", path };
  }

  inspectMarkdown(path: string): SourceInspection {
    const record = this.records.get(path);
    if (!record) return { kind: "missing", path };
    if (this.oversizedPaths.has(path)) {
      return { kind: "oversized", path, size: record.bytes.byteLength, mtime: record.mtime };
    }
    return {
      kind: "candidate",
      path,
      size: record.bytes.byteLength,
      mtime: record.mtime,
    };
  }

  async readMarkdown(
    inspection: Extract<SourceInspection, { kind: "candidate" }>,
  ): Promise<StableSourceRead> {
    this.log.push(`read:${inspection.path}`);
    this.onRead?.(inspection.path);
    if (this.staleReads.has(inspection.path)) return { kind: "stale", path: inspection.path };
    const record = this.records.get(inspection.path);
    if (!record) return { kind: "missing", path: inspection.path };
    if (record.mtime !== inspection.mtime || record.bytes.byteLength !== inspection.size) {
      return { kind: "stale", path: inspection.path };
    }
    return { kind: "source", source: sourceInput(inspection.path, record.bytes, record.mtime) };
  }

  emit(event: VaultSourceEvent): void {
    this.listener?.(event);
  }

  set(path: string, text: string, mtime = 1): void {
    this.records.set(path, { bytes: new TextEncoder().encode(text), mtime });
  }

  rename(oldPath: string, path: string): void {
    const record = this.records.get(oldPath);
    this.records.delete(oldPath);
    if (record) this.records.set(path, record);
    this.emit({ kind: "rename", oldPath, path });
  }
}

interface ApplyCall {
  generation: string;
  nextGeneration: string | null;
  upserts: string[];
  removals: string[];
}

class FakeWorker implements IndexWorkerPort {
  readonly calls: string[] = [];
  readonly applyCalls: ApplyCall[] = [];
  activeGeneration: string | null = null;
  stagingGeneration: string | null = null;
  activePaths = new Set<string>();
  stagingPaths = new Set<string>();

  async initialize(): Promise<void> {
    this.calls.push("initialize");
  }

  async beginBuild(generation: string): Promise<IndexCounts> {
    this.calls.push(`begin:${generation}`);
    this.stagingGeneration = generation;
    this.stagingPaths = new Set();
    return this.counts(generation, this.stagingPaths);
  }

  async addSourceBatch(generation: string, sources: SourceUpsert[]): Promise<IndexCounts> {
    this.calls.push(`add:${sources.map((source) => source.descriptor.path).join(",")}`);
    if (generation !== this.stagingGeneration) throw new Error("wrong staging generation");
    for (const source of sources) this.stagingPaths.add(source.descriptor.path);
    return this.counts(generation, this.stagingPaths);
  }

  async applySourceChanges(
    generation: string,
    nextGeneration: string | null,
    upserts: SourceUpsert[],
    removals: SourceRemoval[],
  ): Promise<IndexCounts> {
    this.applyCalls.push({
      generation,
      nextGeneration,
      upserts: upserts.map((source) => source.descriptor.path),
      removals: removals.map((removal) => removal.path),
    });
    const paths = nextGeneration === null ? this.stagingPaths : this.activePaths;
    for (const removal of removals) paths.delete(removal.path);
    for (const source of upserts) paths.add(source.descriptor.path);
    if (nextGeneration === null) return this.counts(generation, paths);
    this.activeGeneration = nextGeneration;
    return this.counts(nextGeneration, paths);
  }

  async commitBuild(generation: string): Promise<IndexCounts> {
    this.calls.push(`commit:${generation}`);
    if (generation !== this.stagingGeneration) throw new Error("wrong staging generation");
    this.activeGeneration = generation;
    this.activePaths = new Set(this.stagingPaths);
    this.stagingGeneration = null;
    this.stagingPaths = new Set();
    return this.counts(generation, this.activePaths);
  }

  async abortBuild(generation: string): Promise<IndexCounts> {
    this.calls.push(`abort:${generation}`);
    if (generation === this.stagingGeneration) {
      this.stagingGeneration = null;
      this.stagingPaths = new Set();
    }
    const active = this.activeGeneration ?? generation;
    return this.counts(active, this.activePaths);
  }

  private counts(generation: string, paths: Set<string>): IndexCounts {
    return { generation, documents: paths.size, chunks: paths.size };
  }
}

const CACHE_IDENTITY = "0123456789abcdef".repeat(4);

class FakeCacheWorker extends FakeWorker {
  readonly restoredLedger = new Map<string, ReconciliationSourceMetadata>();
  readonly planCalls: ReconciliationSourceMetadata[][] = [];
  readonly exportCalls: string[] = [];
  restoreCalls = 0;
  exportGate: Promise<void> | null = null;

  async restoreGeneration(hit: Extract<CacheLoad, { kind: "hit" }>): Promise<IndexCounts> {
    this.restoreCalls += 1;
    this.activeGeneration = hit.record.generationId;
    this.activePaths = new Set(this.restoredLedger.keys());
    return this.countsFor(hit.record.generationId, this.activePaths);
  }

  async planReconciliation(
    generation: string,
    _vaultId: string,
    currentSources: ReconciliationSourceMetadata[],
  ): Promise<ReconciliationPlanResult> {
    this.planCalls.push(currentSources);
    const stored = new Map(this.restoredLedger);
    const storedSourceCount = stored.size;
    let matchedSourceCount = 0;
    const unchanged: string[] = [];
    const refresh: string[] = [];
    for (const source of currentSources) {
      const previous = stored.get(source.path);
      if (previous) matchedSourceCount += 1;
      stored.delete(source.path);
      if (previous
        && previous.byte_length === source.byte_length
        && previous.mtime_nanos === source.mtime_nanos
        && previous.indexable === source.indexable) {
        unchanged.push(source.path);
      } else {
        refresh.push(source.path);
      }
    }
    return {
      generation,
      unchanged,
      refresh,
      remove: [...stored.keys()].sort(),
      stored_source_count: storedSourceCount,
      matched_source_count: matchedSourceCount,
    };
  }

  async exportGeneration(generation: string): Promise<ExportGenerationResult> {
    this.exportCalls.push(generation);
    if (this.exportGate) await this.exportGate;
    return exportResult(generation);
  }

  private countsFor(generation: string, paths: Set<string>): IndexCounts {
    return { generation, documents: paths.size, chunks: paths.size };
  }
}

class FakeCacheStore implements CacheStorePort {
  readonly vaultCacheIdentity = CACHE_IDENTITY;
  readonly puts: CacheWrite[] = [];
  readonly discards: string[] = [];
  disposed = 0;
  putError: unknown = null;

  constructor(
    readonly loaded: CacheLoad,
    private readonly onLoad: (() => void) | null = null,
  ) {}

  async load(): Promise<CacheLoad> {
    this.onLoad?.();
    return this.loaded;
  }

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
  }

  async dispose(): Promise<void> {
    this.disposed += 1;
  }
}

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
    documents: 1,
    chunks: 1,
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

function sourceInput(path: string, bytes: Uint8Array, mtime: number): SourceInput {
  return {
    descriptor: {
      vault_id: ACTIVE_VAULT_ID,
      path,
      format: "markdown",
      byte_length: bytes.byteLength,
      mtime: Math.floor(mtime / 1_000),
      mtime_nanos: (BigInt(mtime) * 1_000_000n).toString(),
    },
    bytes,
  };
}

function harness(
  source: FakeSource,
  worker = new FakeWorker(),
  limits: ConstructorParameters<typeof InPluginIndexController>[0]["limits"] = {},
  cache?: IndexControllerCacheOptions,
): {
    controller: InPluginIndexController;
    worker: FakeWorker;
    statuses: IndexControllerStatus[];
    failures: unknown[];
  } {
  const statuses: IndexControllerStatus[] = [];
  const failures: unknown[] = [];
  let generation = 0;
  const controller = new InPluginIndexController({
    source,
    worker,
    nextGeneration: () => `generation-${++generation}`,
    onStatus: (status) => statuses.push(status),
    onFailure: (error) => failures.push(error),
    yieldControl: () => Promise.resolve(),
    limits,
    ...(cache ? { cache } : {}),
  });
  return { controller, worker, statuses, failures };
}

describe("InPluginIndexController", () => {
  it("subscribes before the snapshot, sorts paths, and publishes only after commit", async () => {
    const source = new FakeSource();
    source.set("z.md", "z");
    source.set("a.md", "a");
    const { controller, worker, statuses } = harness(source);

    controller.start();
    await controller.whenIdle();

    expect(source.log[0]).toBe("subscribe");
    expect(source.log.indexOf("subscribe")).toBeLessThan(source.log.indexOf("list"));
    expect(worker.calls).toEqual([
      "initialize",
      "begin:generation-1",
      "add:a.md,z.md",
      "commit:generation-1",
    ]);
    expect(statuses.filter((status) => status.searchable).map((status) => status.stage)).toEqual([
      "ready",
    ]);
    expect(statuses.at(-1)).toMatchObject({
      stage: "ready",
      generation: "generation-1",
      documents: 2,
      dirty: false,
    });
  });

  it("binds before Worker and cache awaits, restores searchable-stale, and reads only changed sources", async () => {
    const source = new FakeSource();
    source.set("unchanged.md", "same", 1);
    source.set("changed.md", "new value", 2);
    source.set("new.md", "brand new", 1);
    const worker = new FakeCacheWorker();
    worker.restoredLedger.set("unchanged.md", {
      path: "unchanged.md",
      byte_length: 4,
      mtime_nanos: "1000000",
      indexable: true,
    });
    worker.restoredLedger.set("changed.md", {
      path: "changed.md",
      byte_length: 3,
      mtime_nanos: "1000000",
      indexable: true,
    });
    worker.restoredLedger.set("deleted.md", {
      path: "deleted.md",
      byte_length: 7,
      mtime_nanos: "1000000",
      indexable: true,
    });
    let releasePlan!: () => void;
    const planGate = new Promise<void>((resolve) => {
      releasePlan = resolve;
    });
    const originalPlan = worker.planReconciliation.bind(worker);
    worker.planReconciliation = vi.fn(async (generation, vaultId, currentSources) => {
      await planGate;
      return originalPlan(generation, vaultId, currentSources);
    });
    worker.initialize = vi.fn(async () => {
      source.log.push("initialize");
    });
    const store = new FakeCacheStore(cacheHit());
    const { controller, statuses } = harness(source, worker, {}, {
      openStore: async () => {
        source.log.push("open-store");
        return { kind: "available", store };
      },
    });

    controller.start();
    await vi.waitFor(() => expect(worker.planReconciliation).toHaveBeenCalledTimes(1));

    expect(source.log[0]).toBe("subscribe");
    expect(source.log.indexOf("subscribe")).toBeLessThan(source.log.indexOf("initialize"));
    expect(source.log.indexOf("initialize")).toBeLessThan(source.log.indexOf("open-store"));
    expect(statuses.at(-1)).toMatchObject({
      stage: "replay",
      searchable: true,
      generation: "cached-generation",
      dirty: true,
      issue: "index_reconciling",
    });
    expect(source.log.filter((entry) => entry.startsWith("read:"))).toEqual([]);

    releasePlan();
    await controller.whenIdle();

    expect(source.log.filter((entry) => entry.startsWith("read:"))).toEqual([
      "read:changed.md",
      "read:new.md",
    ]);
    expect(worker.applyCalls).toEqual([
      {
        generation: "cached-generation",
        nextGeneration: "generation-1",
        upserts: ["changed.md"],
        removals: [],
      },
      {
        generation: "generation-1",
        nextGeneration: "generation-2",
        upserts: ["new.md"],
        removals: [],
      },
      {
        generation: "generation-2",
        nextGeneration: "generation-3",
        upserts: [],
        removals: ["deleted.md"],
      },
    ]);
    expect(statuses.at(-1)).toMatchObject({
      stage: "ready",
      generation: "generation-3",
      dirty: false,
    });
  });

  it("never reuses a restored generation when the restarted allocator collides", async () => {
    const source = new FakeSource();
    source.set("changed.md", "new", 2);
    const worker = new FakeCacheWorker();
    worker.restoredLedger.set("changed.md", {
      path: "changed.md",
      byte_length: 3,
      mtime_nanos: "1000000",
      indexable: true,
    });
    const store = new FakeCacheStore(cacheHit("generation-1"));
    const { controller } = harness(source, worker, {}, {
      openStore: async () => ({ kind: "available", store }),
    });

    controller.start();
    await controller.whenIdle();
    expect(worker.applyCalls[0]).toMatchObject({
      generation: "generation-1",
      nextGeneration: "generation-2",
      upserts: ["changed.md"],
    });

    source.set("later.md", "later", 3);
    source.emit({ kind: "upsert", path: "later.md" });
    await controller.whenIdle();
    expect(worker.applyCalls.at(-1)).toMatchObject({
      generation: "generation-2",
      nextGeneration: "generation-3",
      upserts: ["later.md"],
    });

    controller.requestRebuild();
    await controller.whenIdle();
    expect(worker.calls).toContain("begin:generation-4");
    expect(worker.activeGeneration).toBe("generation-4");
  });

  it.each([
    [
      "an omitted current path",
      {
        generation: "cached-generation",
        unchanged: [],
        refresh: [],
        remove: [],
        stored_source_count: 1,
        matched_source_count: 1,
      },
    ],
    [
      "an omitted stored deletion",
      {
        generation: "cached-generation",
        unchanged: ["current.md"],
        refresh: [],
        remove: [],
        stored_source_count: 2,
        matched_source_count: 1,
      },
    ],
  ] as const)("fails closed on a reconciliation plan with %s", async (_name, invalidPlan) => {
    const source = new FakeSource();
    source.set("current.md", "current", 1);
    const worker = new FakeCacheWorker();
    worker.restoredLedger.set("current.md", {
      path: "current.md",
      byte_length: 7,
      mtime_nanos: "1000000",
      indexable: true,
    });
    worker.planReconciliation = vi.fn(
      async () => invalidPlan as unknown as ReconciliationPlanResult,
    );
    const store = new FakeCacheStore(cacheHit());
    const { controller, statuses, failures } = harness(source, worker, {}, {
      openStore: async () => ({ kind: "available", store }),
    });

    controller.start();
    await controller.whenIdle();

    expect(failures).toHaveLength(1);
    expect(worker.applyCalls).toEqual([]);
    expect(statuses.at(-1)).toMatchObject({
      stage: "degraded",
      searchable: true,
      generation: "cached-generation",
      dirty: true,
      issue: "index_update_failed",
    });
    expect(statuses.some((status) => status.stage === "ready" && !status.dirty)).toBe(false);
  });

  it("subsumes an event captured during cache load into the snapshot exactly once", async () => {
    const source = new FakeSource();
    const worker = new FakeCacheWorker();
    const store = new FakeCacheStore(cacheHit(), () => {
      source.set("during-load.md", "captured", 1);
      source.emit({ kind: "upsert", path: "during-load.md" });
    });
    const { controller } = harness(source, worker, {}, {
      openStore: async () => ({ kind: "available", store }),
    });

    controller.start();
    await controller.whenIdle();

    expect(worker.applyCalls).toEqual([{
      generation: "cached-generation",
      nextGeneration: "generation-1",
      upserts: ["during-load.md"],
      removals: [],
    }]);
    expect(worker.activePaths).toEqual(new Set(["during-load.md"]));
  });

  it("exports only after two clean idle seconds and degrades durability without dirtying search", async () => {
    vi.useFakeTimers();
    try {
      const source = new FakeSource();
      source.set("note.md", "value", 1);
      const worker = new FakeCacheWorker();
      const store = new FakeCacheStore({ kind: "miss", reason: "absent" });
      store.putError = new Error("quota");
      const { controller, statuses } = harness(source, worker, {}, {
        openStore: async () => ({ kind: "available", store }),
      });

      controller.start();
      await controller.whenIdle();
      await vi.advanceTimersByTimeAsync(1_999);
      expect(worker.exportCalls).toEqual([]);
      await vi.advanceTimersByTimeAsync(1);
      await vi.waitFor(() => expect(worker.exportCalls).toEqual(["generation-1"]));
      await vi.waitFor(() => expect(statuses.at(-1)).toMatchObject({
        stage: "ready",
        searchable: true,
        generation: "generation-1",
        dirty: false,
        issue: "cache_save_failed",
      }));
      expect(store.puts).toHaveLength(1);
      await vi.advanceTimersByTimeAsync(10_000);
      expect(worker.exportCalls).toEqual(["generation-1"]);
    } finally {
      vi.useRealTimers();
    }
  });

  it("drops an export made stale by a vault event and persists the later clean generation", async () => {
    vi.useFakeTimers();
    try {
      const source = new FakeSource();
      source.set("base.md", "base", 1);
      const worker = new FakeCacheWorker();
      let releaseExport!: () => void;
      worker.exportGate = new Promise<void>((resolve) => {
        releaseExport = resolve;
      });
      const store = new FakeCacheStore({ kind: "miss", reason: "absent" });
      const { controller } = harness(source, worker, {}, {
        openStore: async () => ({ kind: "available", store }),
      });

      controller.start();
      await controller.whenIdle();
      await vi.advanceTimersByTimeAsync(2_000);
      await vi.waitFor(() => expect(worker.exportCalls).toEqual(["generation-1"]));
      source.set("late.md", "late", 2);
      source.emit({ kind: "upsert", path: "late.md" });
      releaseExport();
      await controller.whenIdle();
      await Promise.resolve();
      expect(store.puts).toHaveLength(0);

      worker.exportGate = null;
      await vi.advanceTimersByTimeAsync(2_000);
      await vi.waitFor(() => expect(store.puts).toHaveLength(1));
      expect(store.puts[0]!.generationId).toBe("generation-2");
    } finally {
      vi.useRealTimers();
    }
  });

  it("reads up to four sources concurrently but emits deterministic sorted batches", async () => {
    const source = new FakeSource();
    for (const path of ["f.md", "e.md", "d.md", "c.md", "b.md", "a.md"]) {
      source.set(path, path.slice(0, 1));
    }
    const releases = new Map<string, () => void>();
    const started: string[] = [];
    let activeReads = 0;
    let peakReads = 0;
    const originalRead = source.readMarkdown.bind(source);
    source.readMarkdown = vi.fn(async (inspection) => {
      started.push(inspection.path);
      activeReads += 1;
      peakReads = Math.max(peakReads, activeReads);
      await new Promise<void>((resolve) => releases.set(inspection.path, resolve));
      activeReads -= 1;
      return originalRead(inspection);
    });
    const { controller, worker } = harness(source);

    controller.start();
    await vi.waitFor(() => expect(started).toEqual(["a.md", "b.md", "c.md", "d.md"]));
    for (const path of ["d.md", "a.md", "c.md", "b.md"]) releases.get(path)?.();
    await vi.waitFor(() => expect(started).toHaveLength(6));
    releases.get("f.md")?.();
    releases.get("e.md")?.();
    await controller.whenIdle();

    expect(peakReads).toBe(4);
    expect(worker.calls).toContain("add:a.md,b.md,c.md,d.md,e.md,f.md");
  });

  it("flushes retained bytes before starting a read that would exceed the batch budget", async () => {
    const source = new FakeSource();
    for (const path of ["a.md", "b.md", "c.md"]) source.set(path, "123456");
    const releases = new Map<string, () => void>();
    const started: string[] = [];
    const originalRead = source.readMarkdown.bind(source);
    source.readMarkdown = vi.fn(async (inspection) => {
      started.push(inspection.path);
      await new Promise<void>((resolve) => releases.set(inspection.path, resolve));
      return originalRead(inspection);
    });
    const { controller, worker } = harness(source, new FakeWorker(), {
      maxConcurrentReads: 4,
      maxBatchBytes: 10,
    });

    controller.start();
    await vi.waitFor(() => expect(started).toEqual(["a.md"]));
    releases.get("a.md")?.();
    await vi.waitFor(() => {
      expect(worker.calls).toContain("add:a.md");
      expect(started).toEqual(["a.md", "b.md"]);
    });
    releases.get("b.md")?.();
    await vi.waitFor(() => {
      expect(worker.calls).toContain("add:b.md");
      expect(started).toEqual(["a.md", "b.md", "c.md"]);
    });
    releases.get("c.md")?.();
    await controller.whenIdle();

    expect(worker.calls.filter((call) => call.startsWith("add:"))).toEqual([
      "add:a.md",
      "add:b.md",
      "add:c.md",
    ]);
  });

  it("replays a rename emitted during snapshot reads before publication", async () => {
    const source = new FakeSource();
    source.set("a.md", "a");
    source.set("b.md", "b");
    let renamed = false;
    source.onRead = () => {
      if (renamed) return;
      renamed = true;
      source.rename("b.md", "c.md");
    };
    const { controller, worker } = harness(source);

    controller.start();
    await controller.whenIdle();

    expect(worker.applyCalls).toContainEqual({
      generation: "generation-1",
      nextGeneration: null,
      upserts: ["c.md"],
      removals: ["b.md"],
    });
    expect([...worker.activePaths].sort()).toEqual(["a.md", "c.md"]);
  });

  it("continues replay when another event arrives during a staging mutation", async () => {
    const source = new FakeSource();
    source.set("a.md", "a");
    source.set("b.md", "b");
    let renamed = false;
    source.onRead = () => {
      if (renamed) return;
      renamed = true;
      source.rename("b.md", "c.md");
    };
    const worker = new FakeWorker();
    const originalApply = worker.applySourceChanges.bind(worker);
    let emitted = false;
    worker.applySourceChanges = vi.fn(async (
      generation,
      nextGeneration,
      upserts,
      removals,
    ) => {
      if (nextGeneration === null && !emitted) {
        emitted = true;
        source.set("late.md", "late");
        source.emit({ kind: "upsert", path: "late.md" });
      }
      return originalApply(generation, nextGeneration, upserts, removals);
    });
    const { controller } = harness(source, worker);

    controller.start();
    await controller.whenIdle();

    expect([...worker.activePaths].sort()).toEqual(["a.md", "c.md", "late.md"]);
    expect(worker.applyCalls.filter((call) => call.nextGeneration === null)).toHaveLength(2);
  });

  it("drains an event emitted during initial commit after publication", async () => {
    const source = new FakeSource();
    source.set("initial.md", "initial");
    const worker = new FakeWorker();
    const originalCommit = worker.commitBuild.bind(worker);
    let emitted = false;
    worker.commitBuild = vi.fn(async (generation) => {
      if (!emitted) {
        emitted = true;
        source.set("late.md", "late");
        source.emit({ kind: "upsert", path: "late.md" });
      }
      return originalCommit(generation);
    });
    const { controller } = harness(source, worker);

    controller.start();
    await controller.whenIdle();

    expect([...worker.activePaths].sort()).toEqual(["initial.md", "late.md"]);
    expect(worker.activeGeneration).toBe("generation-2");
  });

  it("coalesces modify bursts and advances the active generation once", async () => {
    const source = new FakeSource();
    source.set("note.md", "first", 1);
    const { controller, worker } = harness(source);
    controller.start();
    await controller.whenIdle();

    source.set("note.md", "final", 2);
    source.emit({ kind: "upsert", path: "note.md" });
    source.emit({ kind: "upsert", path: "note.md" });
    source.emit({ kind: "upsert", path: "note.md" });
    await controller.whenIdle();

    expect(worker.applyCalls.at(-1)).toEqual({
      generation: "generation-1",
      nextGeneration: "generation-2",
      upserts: ["note.md"],
      removals: [],
    });
  });

  it("retries a transient stale read and publishes the stable source", async () => {
    const source = new FakeSource();
    source.set("note.md", "stable");
    const originalRead = source.readMarkdown.bind(source);
    let attempts = 0;
    source.readMarkdown = vi.fn(async (inspection) => {
      attempts += 1;
      if (attempts === 1) return { kind: "stale" as const, path: inspection.path };
      return originalRead(inspection);
    });
    const { controller, worker } = harness(source);

    controller.start();
    await controller.whenIdle();

    expect(attempts).toBe(2);
    expect([...worker.activePaths]).toEqual(["note.md"]);
  });

  it("records an oversized skip and removes only sources that vanish", async () => {
    const source = new FakeSource();
    source.set("oversized.md", "old");
    source.set("vanished.md", "old");
    const { controller, worker } = harness(source);
    controller.start();
    await controller.whenIdle();

    source.oversizedPaths.add("oversized.md");
    source.records.delete("vanished.md");
    source.emit({ kind: "upsert", path: "oversized.md" });
    source.emit({ kind: "upsert", path: "vanished.md" });
    await controller.whenIdle();

    expect([...worker.activePaths]).toEqual(["oversized.md"]);
    expect(worker.applyCalls.slice(-2)).toEqual([
      {
        generation: "generation-1",
        nextGeneration: "generation-2",
        upserts: ["oversized.md"],
        removals: [],
      },
      {
        generation: "generation-2",
        nextGeneration: "generation-3",
        upserts: [],
        removals: ["vanished.md"],
      },
    ]);
  });

  it("keeps exactly one active mutation RPC in flight", async () => {
    const source = new FakeSource();
    source.set("base.md", "base");
    const worker = new FakeWorker();
    const { controller } = harness(source, worker);
    controller.start();
    await controller.whenIdle();

    let release!: () => void;
    const gate = new Promise<void>((resolve) => {
      release = resolve;
    });
    const originalApply = worker.applySourceChanges.bind(worker);
    let activeCalls = 0;
    let peakCalls = 0;
    let first = true;
    worker.applySourceChanges = vi.fn(async (
      generation,
      nextGeneration,
      upserts,
      removals,
    ) => {
      activeCalls += 1;
      peakCalls = Math.max(peakCalls, activeCalls);
      try {
        if (first) {
          first = false;
          await gate;
        }
        return await originalApply(generation, nextGeneration, upserts, removals);
      } finally {
        activeCalls -= 1;
      }
    });

    source.set("x.md", "x");
    source.set("y.md", "y");
    source.emit({ kind: "upsert", path: "x.md" });
    source.emit({ kind: "upsert", path: "y.md" });
    await vi.waitFor(() => expect(worker.applySourceChanges).toHaveBeenCalledTimes(1));
    expect(activeCalls).toBe(1);
    release();
    await controller.whenIdle();

    expect(peakCalls).toBe(1);
    expect(worker.applySourceChanges).toHaveBeenCalledTimes(2);
  });

  it("collapses rename chains into one atomic old-path removal and final upsert", async () => {
    const source = new FakeSource();
    source.set("a.md", "a");
    const { controller, worker } = harness(source);
    controller.start();
    await controller.whenIdle();

    source.rename("a.md", "b.md");
    source.rename("b.md", "c.md");
    await controller.whenIdle();

    expect(worker.applyCalls.at(-1)).toEqual({
      generation: "generation-1",
      nextGeneration: "generation-2",
      upserts: ["c.md"],
      removals: ["a.md"],
    });
    expect([...worker.activePaths]).toEqual(["c.md"]);
  });

  it("preserves causal order for dependent renames", async () => {
    const source = new FakeSource();
    source.set("a.md", "a");
    source.set("b.md", "b");
    const { controller, worker } = harness(source);
    controller.start();
    await controller.whenIdle();

    source.rename("b.md", "c.md");
    source.rename("a.md", "b.md");
    await controller.whenIdle();

    expect(worker.applyCalls.slice(-2)).toEqual([
      {
        generation: "generation-1",
        nextGeneration: "generation-2",
        upserts: ["c.md"],
        removals: ["b.md"],
      },
      {
        generation: "generation-2",
        nextGeneration: "generation-3",
        upserts: ["b.md"],
        removals: ["a.md"],
      },
    ]);
    expect([...worker.activePaths].sort()).toEqual(["b.md", "c.md"]);
  });

  it("rebuilds authoritative state for a live rename swap cycle", async () => {
    const source = new FakeSource();
    source.set("a.md", "a");
    source.set("b.md", "b");
    const { controller, worker } = harness(source);
    controller.start();
    await controller.whenIdle();

    source.rename("a.md", "temporary.md");
    source.rename("b.md", "a.md");
    source.rename("temporary.md", "b.md");
    await controller.whenIdle();

    expect(worker.calls.filter((call) => call.startsWith("begin:"))).toHaveLength(2);
    expect([...worker.activePaths].sort()).toEqual(["a.md", "b.md"]);
  });

  it("restarts an initial snapshot when replay contains a rename round trip", async () => {
    const source = new FakeSource();
    source.set("a.md", "a");
    source.set("b.md", "b");
    let renamed = false;
    source.onRead = () => {
      if (renamed) return;
      renamed = true;
      source.rename("a.md", "temporary.md");
      source.rename("temporary.md", "a.md");
    };
    const { controller, worker } = harness(source);

    controller.start();
    await controller.whenIdle();

    expect(worker.calls.filter((call) => call.startsWith("begin:"))).toHaveLength(2);
    expect([...worker.activePaths].sort()).toEqual(["a.md", "b.md"]);
  });

  it("rebuilds when a rename root is reused and its destination is then deleted", async () => {
    const source = new FakeSource();
    source.set("a.md", "a");
    source.set("b.md", "b");
    const { controller, worker } = harness(source);
    controller.start();
    await controller.whenIdle();

    source.rename("b.md", "c.md");
    source.rename("a.md", "b.md");
    source.records.delete("c.md");
    source.emit({ kind: "remove", path: "c.md" });
    await controller.whenIdle();

    expect(worker.calls.filter((call) => call.startsWith("begin:"))).toHaveLength(2);
    expect([...worker.activePaths]).toEqual(["b.md"]);
  });

  it("keeps chain updates in their original causal position", async () => {
    const source = new FakeSource();
    source.set("a.md", "a");
    source.set("b.md", "b");
    const { controller, worker } = harness(source);
    controller.start();
    await controller.whenIdle();

    source.rename("b.md", "c.md");
    source.rename("a.md", "b.md");
    source.rename("c.md", "d.md");
    await controller.whenIdle();

    expect(worker.applyCalls.slice(-2)).toEqual([
      {
        generation: "generation-1",
        nextGeneration: "generation-2",
        upserts: ["d.md"],
        removals: ["b.md"],
      },
      {
        generation: "generation-2",
        nextGeneration: "generation-3",
        upserts: ["b.md"],
        removals: ["a.md"],
      },
    ]);
    expect([...worker.activePaths].sort()).toEqual(["b.md", "d.md"]);
  });

  it("falls back to an authoritative rebuild when a renamed path is reused", async () => {
    const source = new FakeSource();
    source.set("a.md", "old");
    const { controller, worker } = harness(source);
    controller.start();
    await controller.whenIdle();

    source.rename("a.md", "b.md");
    source.set("a.md", "new");
    source.emit({ kind: "upsert", path: "a.md" });
    source.rename("a.md", "c.md");
    await controller.whenIdle();

    expect(worker.calls.filter((call) => call.startsWith("begin:"))).toHaveLength(2);
    expect([...worker.activePaths].sort()).toEqual(["b.md", "c.md"]);
  });

  it("coalesces repeated manual rebuild requests and resets progress immediately", async () => {
    const source = new FakeSource();
    source.set("a.md", "a");
    const { controller, worker, statuses } = harness(source);
    controller.start();
    await controller.whenIdle();

    controller.requestRebuild();
    controller.requestRebuild();
    controller.requestRebuild();
    expect(statuses.at(-1)).toMatchObject({
      stage: "rebuild",
      progress: { completed: 0, total: null },
    });
    await controller.whenIdle();

    expect(worker.calls.filter((call) => call.startsWith("begin:"))).toHaveLength(2);
  });

  it("turns pending-path overflow into one authoritative rebuild", async () => {
    const source = new FakeSource();
    source.set("base.md", "base");
    const { controller, worker } = harness(source, new FakeWorker(), { maxPendingPaths: 1 });
    controller.start();
    await controller.whenIdle();

    source.set("x.md", "x");
    source.set("y.md", "y");
    source.emit({ kind: "upsert", path: "x.md" });
    source.emit({ kind: "upsert", path: "y.md" });
    await controller.whenIdle();

    expect(worker.calls.filter((call) => call.startsWith("begin:"))).toHaveLength(2);
    expect([...worker.activePaths].sort()).toEqual(["base.md", "x.md", "y.md"]);
  });

  it("fails closed when an initial source never becomes stable", async () => {
    const source = new FakeSource();
    source.set("changing.md", "value");
    source.staleReads.add("changing.md");
    const { controller, worker, statuses } = harness(source, new FakeWorker(), {
      maxStableReadAttempts: 2,
    });

    controller.start();
    await controller.whenIdle();

    expect(worker.calls.some((call) => call.startsWith("commit:"))).toBe(false);
    expect(statuses.at(-1)).toMatchObject({
      stage: "failed",
      searchable: false,
      issue: "vault_read_failed",
    });
  });

  it("surfaces an uncertain abort failure instead of reusing that Worker", async () => {
    const source = new FakeSource();
    source.set("changing.md", "value");
    source.staleReads.add("changing.md");
    const worker = new FakeWorker();
    worker.abortBuild = vi.fn(async () => {
      throw new WorkerRpcError({
        code: "timeout",
        stage: "lifecycle",
        message: "private detail",
        retryable: true,
      });
    });
    const { controller, failures } = harness(source, worker, { maxStableReadAttempts: 1 });

    controller.start();
    await controller.whenIdle();

    const failure = failures.at(-1);
    expect(failure).toBeInstanceOf(AggregateError);
    expect((failure as AggregateError).errors).toEqual(expect.arrayContaining([
      expect.objectContaining({ code: "timeout" }),
    ]));
  });

  it("prevents a late active mutation from publishing after disposal", async () => {
    const source = new FakeSource();
    source.set("base.md", "base");
    const worker = new FakeWorker();
    const { controller, statuses } = harness(source, worker);
    controller.start();
    await controller.whenIdle();

    let release!: () => void;
    const gate = new Promise<void>((resolve) => {
      release = resolve;
    });
    const originalApply = worker.applySourceChanges.bind(worker);
    worker.applySourceChanges = vi.fn(async (
      generation,
      nextGeneration,
      upserts,
      removals,
    ) => {
      await gate;
      return originalApply(generation, nextGeneration, upserts, removals);
    });
    source.set("late.md", "late");
    source.emit({ kind: "upsert", path: "late.md" });
    await vi.waitFor(() => expect(worker.applySourceChanges).toHaveBeenCalledTimes(1));

    controller.dispose();
    release();
    await controller.whenIdle();

    expect(source.unsubscribed).toBe(1);
    expect(statuses.at(-1)).toMatchObject({ stage: "disposed", searchable: false });
  });

  it("detaches events and prevents late reads from committing after disposal", async () => {
    const source = new FakeSource();
    source.set("blocked.md", "value");
    let release!: () => void;
    const gate = new Promise<void>((resolve) => {
      release = resolve;
    });
    const originalRead = source.readMarkdown.bind(source);
    source.readMarkdown = vi.fn(async (inspection) => {
      await gate;
      return originalRead(inspection);
    });
    const { controller, worker, statuses } = harness(source);

    controller.start();
    await vi.waitFor(() => expect(source.readMarkdown).toHaveBeenCalled());
    controller.dispose();
    release();
    await controller.whenIdle();

    expect(source.unsubscribed).toBe(1);
    expect(worker.calls.some((call) => call.startsWith("commit:"))).toBe(false);
    expect(statuses.at(-1)).toMatchObject({ stage: "disposed", searchable: false });
  });
});
