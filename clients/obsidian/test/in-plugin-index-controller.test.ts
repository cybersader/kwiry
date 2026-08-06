// SPDX-FileCopyrightText: 2026 cybersader
// SPDX-License-Identifier: GPL-3.0-only

import { describe, expect, it, vi } from "vitest";

import {
  ACTIVE_VAULT_ID,
  type ActiveVaultSource,
  type ExcerptRead,
  type SourceInspection,
  type SourceReadOutcome,
  type StableSourceRead,
  type VaultSourceEvent,
} from "../src/active-vault-source";
import {
  INITIAL_BUILD_CHECKPOINT_ORDERING_VERSION,
  INITIAL_BUILD_CHECKPOINT_RECORD_KIND,
  INITIAL_BUILD_CHECKPOINT_RECORD_VERSION,
  type CacheLoad,
  type CacheStoreBundlePort,
  type CacheStorePort,
  type CacheWrite,
  type InitialBuildCheckpointLoad,
  type InitialBuildCheckpointToken,
  type InitialBuildCheckpointWrite,
} from "../src/cache/cache-store";
import { classifyFailure } from "../src/diagnostics/classify-failure";
import { classifySourcePath, type SourceFormat } from "../src/source-formats";
import {
  InPluginIndexController,
  type VaultActivityReport,
  type IndexControllerCacheOptions,
  type IndexControllerStatus,
  type IndexCounts,
  type IndexWorkerPort,
  type SourceRemoval,
} from "../src/backends/in-plugin-index-controller";
import {
  CACHE_SCHEMA_VERSION,
  INITIAL_BUILD_CHECKPOINT_IMAGE_VERSION,
  WORKER_PROTOCOL_VERSION,
  emptySourceFormatCounts,
  parseWorkerRequest,
  type ExportGenerationResult,
  type InitialBuildCheckpointCursor,
  type InitialBuildCheckpointExportResult,
  type InitialBuildCheckpointReconciliationPlanResult,
  type ReconciliationPlanResult,
  type RestoreInitialBuildCheckpointResult,
  type ReconciliationSourceMetadata,
  type SourceInput,
  type SourcePreparationDefectField,
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
  readonly remainingReadFailures = new Map<string, number>();
  readonly readAttempts = new Map<string, number>();
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

  listSourcePaths(): readonly string[] {
    this.log.push("list");
    return [...this.records.keys()].reverse();
  }

  // Indexing never hydrates excerpts; this exists only to satisfy the port.
  async readExcerptText(path: string): Promise<ExcerptRead> {
    return { kind: "missing", path };
  }

  inspectSource(path: string): SourceInspection {
    const record = this.records.get(path);
    const format = classifySourcePath(path);
    if (!record || format === null) return { kind: "missing", path };
    if (this.oversizedPaths.has(path)) {
      return {
        kind: "oversized",
        path,
        format,
        size: record.bytes.byteLength,
        mtime: record.mtime,
      };
    }
    return {
      kind: "candidate",
      path,
      format,
      size: record.bytes.byteLength,
      mtime: record.mtime,
    };
  }

  async readSource(
    inspection: Extract<SourceInspection, { kind: "candidate" }>,
  ): Promise<SourceReadOutcome> {
    this.log.push(`read:${inspection.path}`);
    this.readAttempts.set(inspection.path, (this.readAttempts.get(inspection.path) ?? 0) + 1);
    this.onRead?.(inspection.path);
    const failures = this.remainingReadFailures.get(inspection.path) ?? 0;
    if (failures > 0) {
      this.remainingReadFailures.set(inspection.path, failures - 1);
      throw new Error("simulated read failure");
    }
    if (this.staleReads.has(inspection.path)) return { kind: "stale", path: inspection.path };
    const record = this.records.get(inspection.path);
    if (!record) return { kind: "missing", path: inspection.path };
    if (record.mtime !== inspection.mtime || record.bytes.byteLength !== inspection.size) {
      return { kind: "stale", path: inspection.path };
    }
    return { kind: "source", source: sourceInput(inspection.path, record.bytes, record.mtime, inspection.format) };
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
  readonly applyUpsertFormats: SourceFormat[][] = [];
  readonly appliedUpsertBytes = new Map<string, Uint8Array>();
  activeGeneration: string | null = null;
  stagingGeneration: string | null = null;
  activePaths = new Set<string>();
  stagingPaths = new Set<string>();
  quarantinedSources = 0;
  quarantineFields: SourcePreparationDefectField[] = [];

  async initialize(_vaultId: string, _sourcePolicyHash: string): Promise<void> {
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
    this.applyUpsertFormats.push(upserts.map((source) => source.descriptor.format));
    for (const source of upserts) {
      if ("bytes" in source) this.appliedUpsertBytes.set(source.descriptor.path, source.bytes.slice());
    }
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

  protected counts(generation: string, paths: Set<string>): IndexCounts {
    const sourceFormatCounts = emptySourceFormatCounts();
    for (const path of paths) {
      const format = classifySourcePath(path);
      if (format !== null) sourceFormatCounts[format]["indexed-complete"] += 1;
    }
    return {
      generation,
      documents: paths.size,
      chunks: paths.size,
      database_bytes: paths.size,
      database_byte_limit: 1_000_000,
      quarantined_sources: this.quarantinedSources,
      quarantine_fields: [...this.quarantineFields],
      source_format_counts: sourceFormatCounts,
    };
  }
}

const CACHE_IDENTITY = "0123456789abcdef".repeat(4);
const OLD_SOURCE_POLICY_HASH = "c32007f375c07577ac536ca290a078525a6f2f125405a803f584216daf1dad97";
const SOURCE_POLICY_HASH = "c414b56f31d22f8e1fbe69f5074bc8862337d1c8ee6065b6ad0da441b4f63860";

async function sha256Text(value: string): Promise<string> {
  const digest = await crypto.subtle.digest("SHA-256", new TextEncoder().encode(value));
  return [...new Uint8Array(digest)]
    .map((byte) => byte.toString(16).padStart(2, "0"))
    .join("");
}

class FakeCacheWorker extends FakeWorker {
  readonly restoredLedger = new Map<
    string,
    ReconciliationSourceMetadata & { content_hash?: string }
  >();
  readonly planCalls: ReconciliationSourceMetadata[][] = [];
  readonly exportCalls: string[] = [];
  restoreCalls = 0;
  exportGate: Promise<void> | null = null;
  sourcePolicyHash = SOURCE_POLICY_HASH;

  override async initialize(vaultId: string, sourcePolicyHash: string): Promise<void> {
    await super.initialize(vaultId, sourcePolicyHash);
    this.sourcePolicyHash = sourcePolicyHash;
  }

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
    const audit: Array<{ path: string; content_hash: string }> = [];
    const refresh: string[] = [];
    for (const source of currentSources) {
      const previous = stored.get(source.path);
      if (previous) matchedSourceCount += 1;
      stored.delete(source.path);
      if (previous
        && previous.byte_length === source.byte_length
        && previous.mtime_nanos === source.mtime_nanos
        && previous.indexable === source.indexable) {
        if (source.indexable) {
          audit.push({
            path: source.path,
            content_hash: previous.content_hash ?? "0".repeat(64),
          });
        } else {
          unchanged.push(source.path);
        }
      } else {
        refresh.push(source.path);
      }
    }
    return {
      generation,
      unchanged,
      audit,
      refresh,
      remove: [...stored.keys()].sort(),
      stored_source_count: storedSourceCount,
      matched_source_count: matchedSourceCount,
    };
  }

  async exportGeneration(generation: string): Promise<ExportGenerationResult> {
    this.exportCalls.push(generation);
    if (this.exportGate) await this.exportGate;
    return exportResult(generation, this.sourcePolicyHash);
  }

  private countsFor(generation: string, paths: Set<string>): IndexCounts {
    const sourceFormatCounts = emptySourceFormatCounts();
    for (const path of paths) {
      const format = classifySourcePath(path);
      if (format !== null) sourceFormatCounts[format]["indexed-complete"] += 1;
    }
    return {
      generation,
      documents: paths.size,
      chunks: paths.size,
      database_bytes: paths.size,
      database_byte_limit: 1_000_000,
      quarantined_sources: 0,
      quarantine_fields: [],
      source_format_counts: sourceFormatCounts,
    };
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

class FakeCheckpointStore extends FakeCacheStore implements CacheStoreBundlePort {
  readonly checkpointPuts: InitialBuildCheckpointWrite[] = [];
  readonly checkpointDiscards: string[] = [];
  readonly checkpointDiscardTokens: InitialBuildCheckpointToken[] = [];
  checkpointLoadCalls = 0;
  checkpointPutGate: Promise<void> | null = null;

  constructor(
    loaded: CacheLoad,
    readonly checkpointLoaded: InitialBuildCheckpointLoad,
    onLoad: (() => void) | null = null,
    private readonly onCheckpointLoad: (() => void) | null = null,
  ) {
    super(loaded, onLoad);
  }

  async loadInitialBuildCheckpoint(): Promise<InitialBuildCheckpointLoad> {
    this.checkpointLoadCalls += 1;
    this.onCheckpointLoad?.();
    return this.checkpointLoaded;
  }

  async putInitialBuildCheckpoint(write: InitialBuildCheckpointWrite) {
    this.checkpointPuts.push(write);
    if (this.checkpointPutGate) await this.checkpointPutGate;
    const { bytes: _bytes, ...record } = write;
    return record;
  }

  async discardInitialBuildCheckpoint(
    reason: "corrupt" | "incompatible" | "completed" | "requested",
    expected: InitialBuildCheckpointToken,
  ): Promise<void> {
    this.checkpointDiscards.push(reason);
    this.checkpointDiscardTokens.push(expected);
  }
}

class FakeCheckpointWorker extends FakeCacheWorker {
  readonly checkpointExportCursors: InitialBuildCheckpointCursor[] = [];
  checkpointRestoreCalls = 0;
  checkpointPlanCalls = 0;
  onCheckpointPlan: (() => void) | null = null;
  checkpointExportGate: Promise<void> | null = null;

  async exportInitialBuildCheckpoint(
    generation: string,
    _cacheIdentity: string,
    cursor: InitialBuildCheckpointCursor,
  ): Promise<InitialBuildCheckpointExportResult> {
    const parsed = parseWorkerRequest({
      version: WORKER_PROTOCOL_VERSION,
      id: 1,
      operation: "export_initial_build_checkpoint",
      generation,
      cache_identity: CACHE_IDENTITY,
      cursor,
    });
    if ("code" in parsed) {
      throw new Error("checkpoint export cursor was rejected by the Worker protocol");
    }
    this.checkpointExportCursors.push({ ...cursor });
    if (this.checkpointExportGate) await this.checkpointExportGate;
    return {
      ...this.counts(generation, this.stagingPaths),
      record_kind: INITIAL_BUILD_CHECKPOINT_RECORD_KIND,
      checkpoint_record_version: INITIAL_BUILD_CHECKPOINT_RECORD_VERSION,
      checkpoint_image_version: INITIAL_BUILD_CHECKPOINT_IMAGE_VERSION,
      publication: "initial_staging",
      searchable: false,
      cursor: { ...cursor },
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
      source_policy_hash: this.sourcePolicyHash,
    };
  }

  async restoreInitialBuildCheckpoint(
    hit: Extract<InitialBuildCheckpointLoad, { kind: "hit" }>,
  ): Promise<RestoreInitialBuildCheckpointResult> {
    this.checkpointRestoreCalls += 1;
    this.stagingGeneration = hit.record.generationId;
    this.stagingPaths = new Set(this.restoredLedger.keys());
    return {
      ...this.counts(hit.record.generationId, this.stagingPaths),
      record_kind: INITIAL_BUILD_CHECKPOINT_RECORD_KIND,
      publication: "initial_staging",
      searchable: false,
      cursor: { ...hit.record.cursor },
    };
  }

  async planInitialBuildCheckpointReconciliation(
    generation: string,
    vaultId: string,
    currentSources: ReconciliationSourceMetadata[],
  ): Promise<InitialBuildCheckpointReconciliationPlanResult> {
    this.checkpointPlanCalls += 1;
    this.onCheckpointPlan?.();
    return {
      ...await super.planReconciliation(generation, vaultId, currentSources),
      publication: "initial_staging",
      searchable: false,
    };
  }
}

function cacheHit(
  generationId = "cached-generation",
  sourcePolicyHash = SOURCE_POLICY_HASH,
): Extract<CacheLoad, { kind: "hit" }> {
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
        source_policy_hash: sourcePolicyHash,
      },
    },
    bytes: new Uint8Array([1, 2, 3, 4]),
    digestVerified: false,
  };
}

function checkpointHit(
  generationId = "checkpoint-generation",
  cursor: InitialBuildCheckpointCursor = {
    snapshot_source_count: 1,
    acknowledged_add_batches: 1,
    acknowledged_prefix_sources: 1,
    last_acknowledged_path: "a.md",
  },
  sourcePolicyHash = SOURCE_POLICY_HASH,
): Extract<InitialBuildCheckpointLoad, { kind: "hit" }> {
  return {
    kind: "hit",
    record: {
      recordKind: INITIAL_BUILD_CHECKPOINT_RECORD_KIND,
      recordVersion: INITIAL_BUILD_CHECKPOINT_RECORD_VERSION,
      imageVersion: INITIAL_BUILD_CHECKPOINT_IMAGE_VERSION,
      orderingVersion: INITIAL_BUILD_CHECKPOINT_ORDERING_VERSION,
      generationId,
      byteLength: 4,
      sha256: "f".repeat(64),
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
        source_policy_hash: sourcePolicyHash,
      },
      cursor,
    },
    bytes: new Uint8Array([5, 6, 7, 8]),
    digestVerified: false,
  };
}

function exportResult(
  generation: string,
  sourcePolicyHash = SOURCE_POLICY_HASH,
): ExportGenerationResult {
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
    source_policy_hash: sourcePolicyHash,
  };
}

function sourceFormatCountsForPaths(paths: readonly string[]) {
  const counts = emptySourceFormatCounts();
  for (const path of paths) {
    const format = classifySourcePath(path);
    if (format !== null) counts[format]["indexed-complete"] += 1;
  }
  return counts;
}

function deferred<T>(): {
  promise: Promise<T>;
  resolve: (value: T) => void;
  reject: (error: unknown) => void;
} {
  let resolve!: (value: T) => void;
  let reject!: (error: unknown) => void;
  const promise = new Promise<T>((resolvePromise, rejectPromise) => {
    resolve = resolvePromise;
    reject = rejectPromise;
  });
  return { promise, resolve, reject };
}

async function flushAsyncWork(turns = 12): Promise<void> {
  for (let turn = 0; turn < turns; turn += 1) await Promise.resolve();
}

function sourceInput(
  path: string,
  bytes: Uint8Array,
  mtime: number,
  format: SourceFormat = "markdown",
): SourceInput {
  return {
    descriptor: {
      vault_id: ACTIVE_VAULT_ID,
      path,
      format,
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
  initialColdPreview = false,
  sourceReadTimeoutMs?: number,
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
    ...(sourceReadTimeoutMs === undefined ? {} : { sourceReadTimeoutMs }),
    ...(cache ? { cache } : {}),
    ...(initialColdPreview ? { initialColdPreview: { enabled: true as const } } : {}),
  });
  return { controller, worker, statuses, failures };
}

describe("InPluginIndexController vault activity", () => {
  it("counts a delete-then-recreate as a resurrection without recording any path", async () => {
    const source = new FakeSource();
    const reports: VaultActivityReport[] = [];
    const controller = new InPluginIndexController({
      source,
      worker: new FakeWorker(),
      nextGeneration: () => "generation-1",
      onStatus: () => undefined,
      yieldControl: () => Promise.resolve(),
      onVaultActivity: (activity) => reports.push(activity),
    });
    // The controller subscribes to the vault only once started.
    controller.start();
    await controller.whenIdle();

    // Ordinary editing: an edit to a file that was never deleted.
    source.emit({ kind: "upsert", path: "notes/keep.md" });
    expect(reports.at(-1)?.resurrected).toBe(0);

    // A sync layer restoring a file it still holds.
    source.emit({ kind: "remove", path: "notes/deleted.md" });
    source.emit({ kind: "upsert", path: "notes/deleted.md" });
    const latest = reports.at(-1);
    expect(latest?.resurrected).toBe(1);
    expect(latest?.removals).toBe(1);
    expect(latest?.upserts).toBe(2);

    // The report is counts only; a note name must never travel with it.
    expect(JSON.stringify(reports)).not.toContain("deleted.md");
    expect(JSON.stringify(reports)).not.toContain("keep.md");

    // Re-creating a path that was never deleted is not a resurrection.
    source.emit({ kind: "upsert", path: "notes/keep.md" });
    expect(reports.at(-1)?.resurrected).toBe(1);
    await controller.dispose();
  });
});

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

  it("keeps initial cold staging unavailable unless preview is explicitly enabled", async () => {
    const source = new FakeSource();
    source.set("a.md", "a");
    source.set("b.md", "b");
    const { controller, statuses } = harness(source, new FakeWorker(), { maxBatchSources: 1 });

    controller.start();
    await controller.whenIdle();

    expect(statuses.every((status) => status.initialColdPreview === undefined)).toBe(true);
  });

  it("offers revisioned initial cold preview only after completed source batches", async () => {
    const source = new FakeSource();
    source.set("a.md", "a");
    source.set("b.md", "b");
    const { controller, statuses } = harness(
      source,
      new FakeWorker(),
      { maxBatchSources: 1 },
      undefined,
      true,
    );

    controller.start();
    await controller.whenIdle();

    const leases = [...new Map(statuses.flatMap((status) =>
      status.initialColdPreview === undefined
        ? []
        : [[status.initialColdPreview.revision, status.initialColdPreview] as const])).values()];
    expect(leases).toEqual([
      {
        generation: "generation-1",
        revision: 1,
        processed: 1,
        total: 2,
        documents: 1,
        chunks: 1,
        quarantinedSources: 0,
        unreadableSources: 0,
      },
      {
        generation: "generation-1",
        revision: 2,
        processed: 2,
        total: 2,
        documents: 2,
        chunks: 2,
        quarantinedSources: 0,
        unreadableSources: 0,
      },
    ]);
    expect(statuses.at(-1)).toMatchObject({
      stage: "ready",
      searchable: true,
      generation: "generation-1",
    });
    expect(statuses.at(-1)).not.toHaveProperty("initialColdPreview");
  });

  it("reports only sources represented by an acknowledged byte-limited batch", async () => {
    const source = new FakeSource();
    source.set("a.md", "aaaa");
    source.set("b.md", "bbbb");
    const originalInspect = source.inspectSource.bind(source);
    source.inspectSource = vi.fn((path) => {
      const inspection = originalInspect(path);
      return inspection.kind === "candidate" ? { ...inspection, size: 2 } : inspection;
    });
    source.readSource = vi.fn(async (inspection) => {
      const record = source.records.get(inspection.path)!;
      return {
        kind: "source" as const,
        source: sourceInput(inspection.path, record.bytes, record.mtime, inspection.format),
      };
    });
    const { controller, statuses } = harness(
      source,
      new FakeWorker(),
      { maxBatchBytes: 6, maxConcurrentReads: 2 },
      undefined,
      true,
    );

    controller.start();
    await controller.whenIdle();

    const firstLease = statuses.find((status) => status.initialColdPreview?.revision === 1)
      ?.initialColdPreview;
    expect(firstLease).toMatchObject({ processed: 1, documents: 1, chunks: 1 });
  });

  it("never offers replacement staging through the initial cold preview seam", async () => {
    const source = new FakeSource();
    source.set("a.md", "a");
    const { controller, statuses } = harness(
      source,
      new FakeWorker(),
      { maxBatchSources: 1 },
      undefined,
      true,
    );
    controller.start();
    await controller.whenIdle();
    statuses.length = 0;

    source.set("b.md", "b", 2);
    controller.requestRebuild();
    await controller.whenIdle();

    expect(statuses.every((status) => status.initialColdPreview === undefined)).toBe(true);
    expect(statuses.at(-1)).toMatchObject({
      stage: "ready",
      searchable: true,
      generation: "generation-2",
      documents: 2,
    });
  });

  it("withdraws initial cold preview when a vault mutation arrives before publication", async () => {
    const source = new FakeSource();
    source.set("a.md", "a");
    source.set("b.md", "b");
    let emitted = false;
    source.onRead = (path) => {
      if (path !== "b.md" || emitted) return;
      emitted = true;
      source.set("c.md", "c", 2);
      source.emit({ kind: "upsert", path: "c.md" });
    };
    const { controller, statuses } = harness(
      source,
      new FakeWorker(),
      { maxBatchSources: 1, maxConcurrentReads: 1 },
      undefined,
      true,
    );

    controller.start();
    await controller.whenIdle();

    const firstPreviewIndex = statuses.findIndex((status) => status.initialColdPreview !== undefined);
    const mutationStatusIndex = statuses.findIndex((status, index) =>
      index > firstPreviewIndex
      && status.stage === "snapshot"
      && status.initialColdPreview === undefined);
    expect(mutationStatusIndex).toBeGreaterThanOrEqual(0);
    expect(statuses.slice(mutationStatusIndex).every((status) =>
      status.initialColdPreview === undefined)).toBe(true);
    expect(statuses.at(-1)).toMatchObject({
      stage: "ready",
      searchable: true,
      documents: 3,
    });
    expect(statuses.at(-1)).not.toHaveProperty("initialColdPreview");
  });

  it("omits a source that vanishes after enumeration and publishes the remaining counts", async () => {
    const source = new FakeSource();
    source.set("a.md", "a");
    source.set("b.md", "b");
    source.set("vanished.md", "gone");
    const originalInspect = source.inspectSource.bind(source);
    source.inspectSource = vi.fn((path) => {
      if (path === "vanished.md") source.records.delete(path);
      return originalInspect(path);
    });
    const { controller, worker, statuses, failures } = harness(source);

    controller.start();
    await controller.whenIdle();

    expect(failures).toEqual([]);
    expect(source.inspectSource).toHaveBeenCalledWith("vanished.md");
    expect(worker.calls).toEqual([
      "initialize",
      "begin:generation-1",
      "add:a.md,b.md",
      "commit:generation-1",
    ]);
    expect(worker.activePaths).toEqual(new Set(["a.md", "b.md"]));
    expect(statuses.at(-1)).toMatchObject({
      stage: "ready",
      searchable: true,
      generation: "generation-1",
      documents: 2,
      chunks: 2,
      quarantinedSources: 0,
      unreadableSources: 0,
      dirty: false,
    });
    expect(statuses.at(-1)).not.toHaveProperty("issue");
  });

  it("publishes a minority of snapshot inspection failures as source-local omissions", async () => {
    const source = new FakeSource();
    source.set("a.md", "a");
    source.set("b.md", "b");
    source.set("unreadable.md", "locked");
    const originalInspect = source.inspectSource.bind(source);
    source.inspectSource = vi.fn((path) => {
      if (path === "unreadable.md") throw new Error("simulated inspection failure");
      return originalInspect(path);
    });
    const { controller, worker, statuses, failures } = harness(source);

    controller.start();
    await controller.whenIdle();

    expect(failures).toEqual([]);
    expect(worker.calls).toEqual([
      "initialize",
      "begin:generation-1",
      "add:a.md,b.md",
      "commit:generation-1",
    ]);
    expect(worker.activePaths).toEqual(new Set(["a.md", "b.md"]));
    expect(statuses.at(-1)).toMatchObject({
      stage: "ready",
      searchable: true,
      generation: "generation-1",
      documents: 2,
      chunks: 2,
      unreadableSources: 1,
      issue: "sources_unreadable",
    });
  });

  it("aborts before committing when most snapshot inspections fail", async () => {
    const source = new FakeSource();
    source.set("a.md", "a");
    source.set("b.md", "b");
    source.set("healthy.md", "healthy");
    const originalInspect = source.inspectSource.bind(source);
    source.inspectSource = vi.fn((path) => {
      if (path === "a.md" || path === "b.md") {
        throw new Error("simulated inspection failure");
      }
      return originalInspect(path);
    });
    const { controller, worker, statuses, failures } = harness(source);

    controller.start();
    await controller.whenIdle();

    expect(failures).toHaveLength(1);
    expect(worker.calls).toEqual([
      "initialize",
      "begin:generation-1",
      "abort:generation-1",
    ]);
    expect(worker.activeGeneration).toBeNull();
    expect(worker.activePaths).toEqual(new Set());
    expect(statuses.at(-1)).toMatchObject({
      stage: "failed",
      searchable: false,
      generation: null,
      documents: 0,
      chunks: 0,
      unreadableSources: 2,
      issue: "vault_read_failed",
    });
  });

  it("classifies authoritative path enumeration failure as vault-wide", async () => {
    const source = new FakeSource();
    source.set("note.md", "value");
    source.listSourcePaths = vi.fn(() => {
      throw new Error("simulated enumeration failure");
    });
    const { controller, worker, statuses, failures } = harness(source);

    controller.start();
    await controller.whenIdle();

    expect(failures).toHaveLength(1);
    expect(worker.calls).toEqual([
      "initialize",
      "begin:generation-1",
      "abort:generation-1",
    ]);
    expect(worker.activeGeneration).toBeNull();
    expect(worker.activePaths).toEqual(new Set());
    expect(statuses.at(-1)).toMatchObject({
      stage: "failed",
      searchable: false,
      generation: null,
      issue: "vault_read_failed",
    });
  });

  it("mirrors cumulative Worker quarantines and clears them before a rebuild retries sources", async () => {
    const source = new FakeSource();
    source.set("a.md", "a");
    source.set("b.md", "b");
    let releaseReplacement!: () => void;
    const replacementGate = new Promise<void>((resolve) => {
      releaseReplacement = resolve;
    });
    let replacementEntered!: () => void;
    const enteredReplacement = new Promise<void>((resolve) => {
      replacementEntered = resolve;
    });
    class GatedReplacementWorker extends FakeWorker {
      override async addSourceBatch(
        generation: string,
        sources: SourceUpsert[],
      ): Promise<IndexCounts> {
        if (generation === "generation-2") {
          replacementEntered();
          await replacementGate;
        }
        return super.addSourceBatch(generation, sources);
      }
    }
    const worker = new GatedReplacementWorker();
    worker.quarantinedSources = 1;
    worker.quarantineFields = ["chunks_contents"];
    const { controller, statuses } = harness(source, worker);

    controller.start();
    await controller.whenIdle();

    expect(statuses.at(-1)).toMatchObject({
      stage: "ready",
      searchable: true,
      quarantinedSources: 1,
      unreadableSources: 0,
      quarantineValidatorFields: ["chunks_contents"],
      issue: "sources_quarantined",
    });

    worker.quarantinedSources = 0;
    worker.quarantineFields = [];
    controller.requestRebuild();
    // Requesting a rebuild does not replace the active generation: it stays
    // searchable, still missing the same notes. Dropping the warning here
    // would leave a partial index answering queries with no indication.
    expect(statuses.at(-1)).toMatchObject({
      quarantinedSources: 1,
      quarantineValidatorFields: ["chunks_contents"],
    });
    await enteredReplacement;
    expect(statuses.at(-1)).toMatchObject({
      stage: "rebuild",
      generation: "generation-1",
      quarantinedSources: 1,
      quarantineValidatorFields: ["chunks_contents"],
    });
    releaseReplacement();
    await controller.whenIdle();

    expect(statuses.at(-1)).toMatchObject({
      stage: "ready",
      quarantinedSources: 0,
      unreadableSources: 0,
      quarantineValidatorFields: [],
    });
    expect(statuses.at(-1)).not.toHaveProperty("issue");
    expect(source.readAttempts).toEqual(new Map([
      ["a.md", 2],
      ["b.md", 2],
    ]));
  });

  it("keeps active omissions visible through replacement replay until publication", async () => {
    const source = new FakeSource();
    source.set("a.md", "a");
    source.set("b.md", "b");
    let releaseReplay!: () => void;
    const replayGate = new Promise<void>((resolve) => {
      releaseReplay = resolve;
    });
    let replayEntered!: () => void;
    const enteredReplay = new Promise<void>((resolve) => {
      replayEntered = resolve;
    });
    class GatedReplayWorker extends FakeWorker {
      override async applySourceChanges(
        generation: string,
        nextGeneration: string | null,
        upserts: SourceUpsert[],
        removals: SourceRemoval[],
      ): Promise<IndexCounts> {
        if (generation === "generation-2" && nextGeneration === null) {
          replayEntered();
          await replayGate;
        }
        return super.applySourceChanges(generation, nextGeneration, upserts, removals);
      }
    }
    const worker = new GatedReplayWorker();
    worker.quarantinedSources = 1;
    worker.quarantineFields = ["chunks_contents"];
    const { controller, statuses } = harness(source, worker);

    controller.start();
    await controller.whenIdle();
    expect(statuses.at(-1)).toMatchObject({
      generation: "generation-1",
      quarantinedSources: 1,
      issue: "sources_quarantined",
    });

    worker.quarantinedSources = 0;
    worker.quarantineFields = [];
    let queuedReplay = false;
    source.onRead = () => {
      if (queuedReplay) return;
      queuedReplay = true;
      source.set("c.md", "c");
      source.emit({ kind: "upsert", path: "c.md" });
    };
    const replacementStatusStart = statuses.length;
    controller.requestRebuild();
    await enteredReplay;
    source.set("d.md", "d");
    source.emit({ kind: "upsert", path: "d.md" });

    const servingStatuses = statuses.slice(replacementStatusStart).filter(
      (status) => status.searchable && status.generation === "generation-1",
    );
    expect(statuses.at(-1)).toMatchObject({ stage: "rebuild" });
    expect(servingStatuses).not.toHaveLength(0);
    for (const status of servingStatuses) {
      expect(status).toMatchObject({
        quarantinedSources: 1,
        quarantineValidatorFields: ["chunks_contents"],
        issue: "sources_quarantined",
      });
    }

    releaseReplay();
    await controller.whenIdle();
    expect(statuses.at(-1)).toMatchObject({
      stage: "ready",
      generation: "generation-2",
      quarantinedSources: 0,
      unreadableSources: 0,
      quarantineValidatorFields: [],
    });
    expect(statuses.at(-1)).not.toHaveProperty("issue");
  });

  it("retries snapshot reads before quarantining one unreadable source", async () => {
    const source = new FakeSource();
    source.set("a.md", "a");
    source.set("b.md", "b");
    source.set("c.md", "c");
    source.remainingReadFailures.set("b.md", 99);
    const { controller, worker, statuses, failures } = harness(source);

    controller.start();
    await controller.whenIdle();

    expect(failures).toEqual([]);
    expect(source.readAttempts.get("b.md")).toBe(3);
    expect(worker.calls).toContain("add:a.md,c.md");
    expect(statuses.at(-1)).toMatchObject({
      stage: "ready",
      searchable: true,
      documents: 2,
      quarantinedSources: 0,
      unreadableSources: 1,
      issue: "sources_unreadable",
    });
  });

  it("does not quarantine a snapshot source that succeeds on retry", async () => {
    const source = new FakeSource();
    source.set("a.md", "a");
    source.set("b.md", "b");
    source.remainingReadFailures.set("a.md", 1);
    const { controller, statuses } = harness(source);

    controller.start();
    await controller.whenIdle();

    expect(source.readAttempts.get("a.md")).toBe(2);
    expect(statuses.at(-1)).toMatchObject({
      stage: "ready",
      documents: 2,
      quarantinedSources: 0,
      unreadableSources: 0,
    });
  });

  it("aborts early when a multi-source read window indicates a systemic outage", async () => {
    const source = new FakeSource();
    for (const path of ["a.md", "b.md", "c.md", "d.md"]) {
      source.set(path, path);
      source.remainingReadFailures.set(path, 99);
    }
    const { controller, worker, statuses, failures } = harness(source);

    controller.start();
    await controller.whenIdle();

    expect(failures).toHaveLength(1);
    expect(worker.calls).toEqual([
      "initialize",
      "begin:generation-1",
      "abort:generation-1",
    ]);
    expect([...source.readAttempts.values()].reduce((sum, count) => sum + count, 0)).toBe(12);
    expect(statuses.at(-1)).toMatchObject({
      stage: "failed",
      searchable: false,
      issue: "vault_read_failed",
    });
  });

  it("keeps the omission warning visible until a rebuild actually starts", async () => {
    const source = new FakeSource();
    source.set("note.md", "value");
    const worker = new FakeWorker();
    worker.quarantinedSources = 1;
    worker.quarantineFields = ["chunks_contents"];
    const { controller, statuses } = harness(source, worker);

    controller.start();
    await controller.whenIdle();
    expect(statuses.at(-1)).toMatchObject({
      stage: "ready",
      quarantinedSources: 1,
      quarantineValidatorFields: ["chunks_contents"],
      issue: "sources_quarantined",
    });

    worker.quarantinedSources = 0;
    worker.quarantineFields = [];
    controller.requestRebuild();
    expect(statuses.at(-1)).toMatchObject({
      stage: "rebuild",
      // The active generation is unchanged and still missing these notes.
      quarantinedSources: 1,
    });
    await controller.whenIdle();

    expect(statuses.at(-1)).toMatchObject({
      stage: "ready",
      quarantinedSources: 0,
      unreadableSources: 0,
    });
  });

  it("retries a snapshot read before marking the source unreadable", async () => {
    const source = new FakeSource();
    source.set("note.md", "value");
    source.remainingReadFailures.set("note.md", 1);
    const { controller, worker, statuses } = harness(source, new FakeWorker(), {
      maxStableReadAttempts: 3,
    });

    controller.start();
    await controller.whenIdle();

    expect(source.readAttempts.get("note.md")).toBe(2);
    expect(worker.activePaths).toEqual(new Set(["note.md"]));
    expect(statuses.at(-1)).toMatchObject({
      stage: "ready",
      unreadableSources: 0,
      quarantinedSources: 0,
    });
  });

  it("publishes a minority of unreadable sources as an explicit degraded generation", async () => {
    const source = new FakeSource();
    source.set("a.md", "a");
    source.set("b.md", "b");
    source.set("unreadable.md", "locked");
    source.remainingReadFailures.set("unreadable.md", 10);
    const { controller, worker, statuses } = harness(source, new FakeWorker(), {
      maxConcurrentReads: 3,
      maxStableReadAttempts: 2,
    });

    controller.start();
    await controller.whenIdle();

    expect(source.readAttempts.get("unreadable.md")).toBe(2);
    expect(worker.calls.filter((call) => call.startsWith("commit:"))).toEqual([
      "commit:generation-1",
    ]);
    expect(worker.calls.filter((call) => call.startsWith("abort:"))).toEqual([]);
    expect(worker.activeGeneration).toBe("generation-1");
    expect([...worker.activePaths].sort()).toEqual(["a.md", "b.md"]);
    expect(statuses.at(-1)).toMatchObject({
      stage: "ready",
      searchable: true,
      documents: 2,
      unreadableSources: 1,
      quarantinedSources: 0,
      issue: "sources_unreadable",
    });

    source.remainingReadFailures.set("unreadable.md", 0);
    controller.requestRebuild();
    expect(statuses.at(-1)).toMatchObject({
      stage: "rebuild",
      unreadableSources: 1,
    });
    await controller.whenIdle();

    expect([...worker.activePaths].sort()).toEqual(["a.md", "b.md", "unreadable.md"]);
    expect(statuses.at(-1)).toMatchObject({
      stage: "ready",
      documents: 3,
      unreadableSources: 0,
    });
  });

  it("aborts after one fully retried window proves source reads are systemically unavailable", async () => {
    const source = new FakeSource();
    for (const path of ["a.md", "b.md", "c.md", "d.md"]) {
      source.set(path, path);
      source.remainingReadFailures.set(path, 10);
    }
    const { controller, worker, statuses } = harness(source, new FakeWorker(), {
      maxConcurrentReads: 4,
      maxStableReadAttempts: 3,
    });

    controller.start();
    await controller.whenIdle();

    expect([...source.readAttempts.values()].reduce((sum, attempts) => sum + attempts, 0)).toBe(12);
    expect(worker.calls).toEqual([
      "initialize",
      "begin:generation-1",
      "abort:generation-1",
    ]);
    expect(worker.activeGeneration).toBeNull();
    expect(worker.activePaths).toEqual(new Set());
    expect(statuses.some((status) => status.searchable)).toBe(false);
    expect(statuses.at(-1)).toMatchObject({
      stage: "failed",
      searchable: false,
      generation: null,
      documents: 0,
      chunks: 0,
      quarantinedSources: 0,
      unreadableSources: 4,
      dirty: true,
      issue: "vault_read_failed",
    });
  });

  it.each(["inspection", "read"] as const)(
    "keeps the complete active generation when one replacement snapshot %s is unreadable",
    async (failureKind) => {
      const source = new FakeSource();
      source.set("a.md", "a");
      source.set("b.md", "b");
      source.set("unreadable.md", "last known good");
      const { controller, worker, statuses, failures } = harness(source, new FakeWorker(), {
        maxConcurrentReads: 3,
        maxStableReadAttempts: 2,
      });
      controller.start();
      await controller.whenIdle();
      expect(worker.activeGeneration).toBe("generation-1");
      expect(worker.activePaths).toEqual(new Set(["a.md", "b.md", "unreadable.md"]));

      if (failureKind === "inspection") {
        const originalInspect = source.inspectSource.bind(source);
        source.inspectSource = vi.fn((path) => {
          if (path === "unreadable.md") throw new Error("private SMB inspection detail");
          return originalInspect(path);
        });
      } else {
        source.remainingReadFailures.set("unreadable.md", 99);
      }
      controller.requestRebuild();
      await controller.whenIdle();

      expect(failures).toEqual([]);
      expect(worker.calls.filter((call) => call.startsWith("commit:"))).toEqual([
        "commit:generation-1",
      ]);
      expect(worker.calls).toContain("abort:generation-2");
      expect(worker.activeGeneration).toBe("generation-1");
      expect(worker.activePaths).toEqual(new Set(["a.md", "b.md", "unreadable.md"]));
      expect(worker.stagingGeneration).toBeNull();
      expect(worker.stagingPaths).toEqual(new Set());
      expect(statuses.at(-1)).toEqual({
        stage: "degraded",
        searchable: true,
        generation: "generation-1",
        documents: 3,
        chunks: 3,
        sourceFormatCounts: sourceFormatCountsForPaths(["a.md", "b.md", "unreadable.md"]),
        quarantinedSources: 0,
        unreadableSources: 1,
        quarantineValidatorFields: [],
        dirty: true,
        rebuilding: false,
        mutationEpoch: expect.any(Number),
        issue: "sources_unreadable",
      });
      expect(JSON.stringify(statuses.at(-1))).not.toContain("private SMB inspection detail");
    },
  );

  it("publishes a complete authoritative replacement that proves a source was deleted", async () => {
    const source = new FakeSource();
    source.set("deleted.md", "delete me");
    source.set("kept.md", "keep me");
    const { controller, worker, statuses, failures } = harness(source);
    controller.start();
    await controller.whenIdle();
    expect(worker.activeGeneration).toBe("generation-1");
    expect(worker.activePaths).toEqual(new Set(["deleted.md", "kept.md"]));

    source.records.delete("deleted.md");
    controller.requestRebuild();
    await controller.whenIdle();

    expect(failures).toEqual([]);
    expect(worker.calls.filter((call) => call.startsWith("commit:"))).toEqual([
      "commit:generation-1",
      "commit:generation-2",
    ]);
    expect(worker.calls.filter((call) => call.startsWith("abort:"))).toEqual([]);
    expect(worker.activeGeneration).toBe("generation-2");
    expect(worker.activePaths).toEqual(new Set(["kept.md"]));
    expect(statuses.at(-1)).toEqual({
      stage: "ready",
      searchable: true,
      generation: "generation-2",
      documents: 1,
      chunks: 1,
      sourceFormatCounts: sourceFormatCountsForPaths(["kept.md"]),
      quarantinedSources: 0,
      unreadableSources: 0,
      quarantineValidatorFields: [],
      dirty: false,
      rebuilding: false,
      mutationEpoch: expect.any(Number),
    });
  });

  it("aborts a systemically unreadable replacement without displacing the complete active generation", async () => {
    const source = new FakeSource();
    for (const path of ["Clients/Secret-A.md", "Clients/Secret-B.md", "healthy.md"]) {
      source.set(path, path);
    }
    const { controller, worker, statuses, failures } = harness(source, new FakeWorker(), {
      maxConcurrentReads: 3,
      maxStableReadAttempts: 1,
    });
    controller.start();
    await controller.whenIdle();
    expect(worker.activeGeneration).toBe("generation-1");
    expect(worker.activePaths).toEqual(new Set([
      "Clients/Secret-A.md",
      "Clients/Secret-B.md",
      "healthy.md",
    ]));

    source.remainingReadFailures.set("Clients/Secret-A.md", 99);
    source.remainingReadFailures.set("Clients/Secret-B.md", 99);
    controller.requestRebuild();
    await controller.whenIdle();

    expect(worker.calls.filter((call) => call.startsWith("commit:"))).toEqual([
      "commit:generation-1",
    ]);
    expect(worker.calls).toContain("abort:generation-2");
    expect(worker.activeGeneration).toBe("generation-1");
    expect(worker.activePaths).toEqual(new Set([
      "Clients/Secret-A.md",
      "Clients/Secret-B.md",
      "healthy.md",
    ]));
    expect(failures).toHaveLength(1);
    const diagnostic = classifyFailure(failures[0]);
    expect(diagnostic).toEqual({
      subsystem: "vault_source",
      reason: "vault_read_failed",
      errorName: "VaultSourceReadError",
      nonError: false,
    });
    expect(statuses.at(-1)).toEqual({
      stage: "degraded",
      searchable: true,
      generation: "generation-1",
      documents: 3,
      chunks: 3,
      sourceFormatCounts: sourceFormatCountsForPaths([
        "Clients/Secret-A.md",
        "Clients/Secret-B.md",
        "healthy.md",
      ]),
      quarantinedSources: 0,
      unreadableSources: 2,
      quarantineValidatorFields: [],
      dirty: true,
      rebuilding: false,
      mutationEpoch: expect.any(Number),
      issue: "vault_read_failed",
    });
    expect(JSON.stringify({ diagnostic, status: statuses.at(-1) })).not.toContain("Secret-");
  });

  it("checkpoints every 25 acknowledged initial-build batches with a conservative path cursor", async () => {
    const source = new FakeSource();
    for (let index = 0; index < 26; index += 1) {
      source.set(`${String(index).padStart(2, "0")}.md`, `source-${index}`, 1);
    }
    const worker = new FakeCheckpointWorker();
    const store = new FakeCheckpointStore(
      { kind: "miss", reason: "absent" },
      { kind: "miss", reason: "absent" },
    );
    const { controller } = harness(source, worker, {
      maxBatchSources: 1,
      maxConcurrentReads: 1,
    }, {
      openStore: async () => ({ kind: "available", store }),
    });

    controller.start();
    await controller.whenIdle();

    expect(worker.checkpointExportCursors).toEqual([{
      snapshot_source_count: 26,
      acknowledged_add_batches: 25,
      acknowledged_prefix_sources: 25,
      last_acknowledged_path: "24.md",
    }]);
    expect(store.checkpointPuts).toHaveLength(1);
    expect(store.checkpointDiscards).toContain("completed");
  });

  it("trusts store-side conditional cleanup and performs an explicit fresh initial build", async () => {
    const source = new FakeSource();
    source.set("fresh.md", "fresh", 1);
    const worker = new FakeCheckpointWorker();
    const store = new FakeCheckpointStore(
      { kind: "miss", reason: "absent" },
      { kind: "miss", reason: "pointer_incompatible" },
    );
    const { controller, statuses } = harness(source, worker, {}, {
      openStore: async () => ({ kind: "available", store }),
    });

    controller.start();
    await controller.whenIdle();

    expect(store.checkpointDiscards).toEqual([]);
    expect(worker.checkpointRestoreCalls).toBe(0);
    expect(worker.calls).toContain("begin:generation-1");
    expect(statuses.at(-1)).toMatchObject({
      stage: "ready",
      searchable: true,
      generation: "generation-1",
    });
  });

  it("never considers a partial checkpoint when a complete generation is restorable", async () => {
    const source = new FakeSource();
    const worker = new FakeCheckpointWorker();
    const store = new FakeCheckpointStore(cacheHit(), checkpointHit());
    const { controller } = harness(source, worker, {}, {
      openStore: async () => ({ kind: "available", store }),
    });

    controller.start();
    await controller.whenIdle();

    expect(worker.restoreCalls).toBe(1);
    expect(worker.checkpointRestoreCalls).toBe(0);
    expect(store.checkpointLoadCalls).toBe(0);
    expect(worker.activeGeneration).toBe("cached-generation");
  });

  it("reconciles only the checkpoint prefix without classifying an untouched suffix as removal", async () => {
    const source = new FakeSource();
    source.set("aa.md", "inserted", 2);
    source.set("b.md", "changed", 2);
    source.set("c.md", "untouched suffix", 1);
    source.set("y.md", "renamed prefix", 2);
    const worker = new FakeCheckpointWorker();
    for (const [path, text] of [
      ["a.md", "old a"],
      ["b.md", "old b"],
      ["c.md", "untouched suffix"],
      ["x.md", "deleted"],
      ["z.md", "old z"],
    ] as const) {
      worker.restoredLedger.set(path, {
        path,
        byte_length: text.length,
        mtime_nanos: "1000000",
        indexable: true,
        content_hash: await sha256Text(text),
      });
    }
    let releaseCommit!: () => void;
    const commitGate = new Promise<void>((resolve) => {
      releaseCommit = resolve;
    });
    const originalCommit = worker.commitBuild.bind(worker);
    worker.commitBuild = vi.fn(async (generation) => {
      await commitGate;
      return originalCommit(generation);
    });
    const store = new FakeCheckpointStore(
      { kind: "miss", reason: "absent" },
      checkpointHit("checkpoint-generation", {
        snapshot_source_count: 4,
        acknowledged_add_batches: 25,
        acknowledged_prefix_sources: 2,
        last_acknowledged_path: "b.md",
      }),
      null,
      () => {
        source.set("during.md", "during restore", 3);
        source.emit({ kind: "upsert", path: "during.md" });
      },
    );
    const { controller, statuses } = harness(source, worker, {
      maxBatchSources: 1,
      maxConcurrentReads: 1,
    }, {
      openStore: async () => ({ kind: "available", store }),
    });

    controller.start();
    await vi.waitFor(() => expect(worker.commitBuild).toHaveBeenCalledTimes(1));

    expect(worker.activeGeneration).toBeNull();
    expect(statuses.at(-1)).toMatchObject({ searchable: false, generation: null });
    expect(worker.planCalls[0]?.map((entry) => entry.path)).toEqual([
      "aa.md",
      "b.md",
      "c.md",
      "during.md",
      "y.md",
    ]);
    expect(worker.applyCalls.some((call) => call.removals.includes("c.md"))).toBe(false);
    expect(worker.applyCalls.flatMap((call) => call.upserts)).toContain("aa.md");
    expect(worker.appliedUpsertBytes.get("b.md")).toEqual(
      new TextEncoder().encode("changed"),
    );
    expect(worker.stagingPaths).toEqual(new Set(["aa.md", "b.md", "c.md", "during.md", "y.md"]));
    const statusBeforeRebuildRequest = statuses.at(-1);
    const statusCountBeforeRebuildRequest = statuses.length;
    expect(controller.requestRebuild()).toBe("already_building");
    expect(statuses).toHaveLength(statusCountBeforeRebuildRequest);
    expect(statuses.at(-1)).toEqual(statusBeforeRebuildRequest);
    expect(worker.calls.some((call) => call.startsWith("begin:"))).toBe(false);

    releaseCommit();
    await controller.whenIdle();

    expect(worker.activePaths).toEqual(new Set(["aa.md", "b.md", "c.md", "during.md", "y.md"]));
    expect(statuses.at(-1)).toMatchObject({
      stage: "ready",
      searchable: true,
      generation: "checkpoint-generation",
      dirty: false,
    });
    expect(store.checkpointDiscards).toEqual(["completed"]);
  });

  it("reconciles renames crossing the saved cursor without losing or duplicating either side", async () => {
    const source = new FakeSource();
    source.set("aa.md", "moved from suffix", 2);
    source.set("b.md", "stable prefix", 1);
    source.set("y.md", "moved from prefix", 2);
    const worker = new FakeCheckpointWorker();
    for (const [path, text] of [
      ["a.md", "moved from prefix"],
      ["b.md", "stable prefix"],
      ["z.md", "moved from suffix"],
    ] as const) {
      worker.restoredLedger.set(path, {
        path,
        byte_length: text.length,
        mtime_nanos: "1000000",
        indexable: true,
        content_hash: await sha256Text(text),
      });
    }
    const store = new FakeCheckpointStore(
      { kind: "miss", reason: "absent" },
      checkpointHit("checkpoint-generation", {
        snapshot_source_count: 3,
        acknowledged_add_batches: 2,
        acknowledged_prefix_sources: 2,
        last_acknowledged_path: "b.md",
      }),
    );
    const { controller } = harness(source, worker, {
      maxBatchSources: 1,
      maxConcurrentReads: 1,
    }, {
      openStore: async () => ({ kind: "available", store }),
    });

    controller.start();
    await controller.whenIdle();

    expect(worker.activePaths).toEqual(new Set(["aa.md", "b.md", "y.md"]));
    expect(worker.applyCalls.flatMap((call) => call.removals).sort()).toEqual(["a.md", "z.md"]);
    expect(worker.applyCalls.flatMap((call) => call.upserts)).toContain("aa.md");
    expect(worker.calls).toContain("add:y.md");
  });

  it("treats conservative cursor lag as replayable work rather than skipped work", async () => {
    const source = new FakeSource();
    const worker = new FakeCheckpointWorker();
    for (const path of ["a.md", "b.md", "c.md"]) {
      source.set(path, path, 1);
      worker.restoredLedger.set(path, {
        path,
        byte_length: path.length,
        mtime_nanos: "1000000",
        indexable: true,
        content_hash: await sha256Text(path),
      });
    }
    const store = new FakeCheckpointStore(
      { kind: "miss", reason: "absent" },
      checkpointHit("checkpoint-generation", {
        snapshot_source_count: 3,
        acknowledged_add_batches: 3,
        acknowledged_prefix_sources: 1,
        last_acknowledged_path: "a.md",
      }),
    );
    const { controller } = harness(source, worker, {
      maxBatchSources: 1,
      maxConcurrentReads: 1,
    }, {
      openStore: async () => ({ kind: "available", store }),
    });

    controller.start();
    await controller.whenIdle();

    expect(worker.calls).toContain("add:b.md");
    expect(worker.calls).toContain("add:c.md");
    expect(worker.applyCalls.flatMap((call) => call.removals)).toEqual([]);
    expect(worker.activePaths).toEqual(new Set(["a.md", "b.md", "c.md"]));
  });

  it("rebases checkpoint batch cadence after a resumed snapshot shrinks", async () => {
    const source = new FakeSource();
    source.set("a.md", "a.md", 1);
    source.set("b.md", "b.md", 1);
    const worker = new FakeCheckpointWorker();
    for (const path of ["a.md", "b.md", "c.md"]) {
      worker.restoredLedger.set(path, {
        path,
        byte_length: path.length,
        mtime_nanos: "1000000",
        indexable: true,
        content_hash: await sha256Text(path),
      });
    }
    let reportSuffixAcknowledged!: () => void;
    const suffixAcknowledged = new Promise<void>((resolve) => {
      reportSuffixAcknowledged = resolve;
    });
    let releaseSuffix!: () => void;
    const suffixGate = new Promise<void>((resolve) => {
      releaseSuffix = resolve;
    });
    const originalAdd = worker.addSourceBatch.bind(worker);
    worker.addSourceBatch = vi.fn(async (generation: string, sources: SourceUpsert[]) => {
      const counts = await originalAdd(generation, sources);
      if (sources.some((entry) => entry.descriptor.path === "b.md")) {
        reportSuffixAcknowledged();
        await suffixGate;
      }
      return counts;
    });
    const store = new FakeCheckpointStore(
      { kind: "miss", reason: "absent" },
      checkpointHit("checkpoint-generation", {
        snapshot_source_count: 3,
        acknowledged_add_batches: 3,
        acknowledged_prefix_sources: 1,
        last_acknowledged_path: "a.md",
      }),
    );
    const { controller } = harness(source, worker, {
      maxBatchSources: 1,
      maxConcurrentReads: 1,
    }, {
      openStore: async () => ({ kind: "available", store }),
    });

    controller.start();
    await suffixAcknowledged;
    const shutdown = controller.prepareForShutdown(1_000);
    releaseSuffix();
    await shutdown;

    expect(worker.checkpointExportCursors).toEqual([{
      snapshot_source_count: 2,
      acknowledged_add_batches: 2,
      acknowledged_prefix_sources: 2,
      last_acknowledged_path: "b.md",
    }]);
    expect(store.checkpointPuts).toHaveLength(1);
  });

  it("restarts checkpoint reconciliation when a post-cut event arrives and replays it", async () => {
    const source = new FakeSource();
    source.set("a.md", "a.md", 1);
    source.set("b.md", "b.md", 1);
    const worker = new FakeCheckpointWorker();
    worker.restoredLedger.set("a.md", {
      path: "a.md",
      byte_length: 4,
      mtime_nanos: "1000000",
      indexable: true,
      content_hash: await sha256Text("a.md"),
    });
    worker.onCheckpointPlan = () => {
      worker.onCheckpointPlan = null;
      source.set("post.md", "post-cut", 2);
      source.emit({ kind: "upsert", path: "post.md" });
    };
    const store = new FakeCheckpointStore(
      { kind: "miss", reason: "absent" },
      checkpointHit("checkpoint-generation", {
        snapshot_source_count: 2,
        acknowledged_add_batches: 1,
        acknowledged_prefix_sources: 1,
        last_acknowledged_path: "a.md",
      }),
    );
    const { controller } = harness(source, worker, {
      maxBatchSources: 1,
      maxConcurrentReads: 1,
    }, {
      openStore: async () => ({ kind: "available", store }),
    });

    controller.start();
    await controller.whenIdle();

    expect(worker.checkpointPlanCalls).toBe(2);
    expect(worker.planCalls[1]?.map((entry) => entry.path)).toEqual(["a.md", "b.md", "post.md"]);
    expect(worker.activePaths).toEqual(new Set(["a.md", "b.md", "post.md"]));
  });

  it("never checkpoints a replacement build even after 25 acknowledged batches", async () => {
    const source = new FakeSource();
    source.set("initial.md", "initial", 1);
    const worker = new FakeCheckpointWorker();
    const store = new FakeCheckpointStore(
      { kind: "miss", reason: "absent" },
      { kind: "miss", reason: "absent" },
    );
    const { controller } = harness(source, worker, {
      maxBatchSources: 1,
      maxConcurrentReads: 1,
    }, {
      openStore: async () => ({ kind: "available", store }),
    });
    controller.start();
    await controller.whenIdle();
    worker.checkpointExportCursors.length = 0;
    store.checkpointPuts.length = 0;

    for (let index = 0; index < 26; index += 1) {
      source.set(`replacement-${String(index).padStart(2, "0")}.md`, `r-${index}`, 2);
    }
    controller.requestRebuild();
    await controller.whenIdle();

    expect(worker.checkpointExportCursors).toEqual([]);
    expect(store.checkpointPuts).toEqual([]);
    expect(worker.activeGeneration).toBe("generation-2");
  });

  it("refuses an old source policy cache and exports the fresh generation under the new hash", async () => {
    vi.useFakeTimers();
    try {
      const source = new FakeSource();
      source.set("note.md", "fresh", 1);
      const worker = new FakeCacheWorker();
      const store = new FakeCacheStore(cacheHit("old-generation", OLD_SOURCE_POLICY_HASH));
      const { controller } = harness(source, worker, {}, {
        openStore: async () => ({ kind: "available", store }),
      });

      controller.start();
      await controller.whenIdle();

      expect(store.discards).toEqual(["incompatible"]);
      expect(worker.restoreCalls).toBe(0);
      expect(worker.activeGeneration).toBe("generation-1");
      expect(worker.sourcePolicyHash).toBe(SOURCE_POLICY_HASH);

      await vi.advanceTimersByTimeAsync(2_000);
      await vi.waitFor(() => expect(store.puts).toHaveLength(1));
      expect(store.puts[0]).toMatchObject({
        generationId: "generation-1",
        identity: { source_policy_hash: SOURCE_POLICY_HASH },
      });
    } finally {
      vi.useRealTimers();
    }
  });

  it("binds before awaits, audits metadata matches, and mutates only changed sources", async () => {
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
      content_hash: await sha256Text("same"),
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
      "read:unchanged.md",
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

  it("enters applying before a verified change mutates and never verifies at 100%", async () => {
    const source = new FakeSource();
    source.set("changed.md", "new value", 2);
    const worker = new FakeCacheWorker();
    worker.restoredLedger.set("changed.md", {
      path: "changed.md",
      byte_length: 3,
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
    let reportApplyEntered!: () => void;
    const applyEntered = new Promise<void>((resolve) => {
      reportApplyEntered = resolve;
    });
    let releaseApply!: () => void;
    const applyGate = new Promise<void>((resolve) => {
      releaseApply = resolve;
    });
    const originalApply = worker.applySourceChanges.bind(worker);
    worker.applySourceChanges = vi.fn(async (
      generation,
      nextGeneration,
      upserts,
      removals,
    ) => {
      reportApplyEntered();
      await applyGate;
      return originalApply(generation, nextGeneration, upserts, removals);
    });
    const { controller, statuses } = harness(source, worker, {}, {
      openStore: async () => ({
        kind: "available",
        store: new FakeCacheStore(cacheHit()),
      }),
    });

    controller.start();
    await vi.waitFor(() => expect(worker.planReconciliation).toHaveBeenCalledTimes(1));
    expect(statuses.at(-1)).toMatchObject({
      stage: "replay",
      progress: { subphase: "planning", completed: 0, total: 1 },
    });

    releasePlan();
    await applyEntered;

    expect(statuses.at(-1)).toMatchObject({
      stage: "replay",
      progress: { subphase: "applying", completed: 0, total: 1 },
    });
    expect(statuses).not.toContainEqual(expect.objectContaining({
      stage: "replay",
      progress: expect.objectContaining({
        subphase: "verifying",
        completed: 1,
        total: 1,
      }),
    }));

    releaseApply();
    await controller.whenIdle();
    expect(statuses.at(-1)).toMatchObject({
      stage: "ready",
      generation: "generation-1",
      dirty: false,
    });
  });

  it("refreshes a same-size same-mtime source when its authoritative hash changed", async () => {
    const source = new FakeSource();
    source.set("property.md", "008", 1);
    const worker = new FakeCacheWorker();
    worker.restoredLedger.set("property.md", {
      path: "property.md",
      byte_length: 3,
      mtime_nanos: "1000000",
      indexable: true,
      content_hash: await sha256Text("007"),
    });
    const { controller } = harness(source, worker, {}, {
      openStore: async () => ({ kind: "available", store: new FakeCacheStore(cacheHit()) }),
    });

    controller.start();
    await controller.whenIdle();

    expect(source.log).toContain("read:property.md");
    expect(worker.applyCalls).toEqual([{
      generation: "cached-generation",
      nextGeneration: "generation-1",
      upserts: ["property.md"],
      removals: [],
    }]);
  });

  it("keeps a complete restored generation when refresh reads prove a systemic outage", async () => {
    const source = new FakeSource();
    const worker = new FakeCacheWorker();
    for (const path of ["a.md", "b.md", "c.md"]) {
      source.set(path, "changed", 2);
      source.remainingReadFailures.set(path, 99);
      worker.restoredLedger.set(path, {
        path,
        byte_length: 3,
        mtime_nanos: "1000000",
        indexable: true,
      });
    }
    const store = new FakeCacheStore(cacheHit());
    const { controller, statuses, failures } = harness(source, worker, {
      maxConcurrentReads: 3,
      maxStableReadAttempts: 2,
    }, {
      openStore: async () => ({ kind: "available", store }),
    });

    controller.start();
    await controller.whenIdle();

    expect(failures).toHaveLength(1);
    expect(worker.applyCalls).toEqual([]);
    expect(worker.activeGeneration).toBe("cached-generation");
    expect(worker.activePaths).toEqual(new Set(["a.md", "b.md", "c.md"]));
    expect(statuses.at(-1)?.unreadableSources).toBe(3);
    expect(statuses.at(-1)).toMatchObject({
      stage: "degraded",
      searchable: true,
      generation: "cached-generation",
      documents: 3,
      chunks: 3,
      dirty: true,
      issue: "vault_read_failed",
    });
  });

  it("removes a genuinely deleted source from a restored generation", async () => {
    const source = new FakeSource();
    source.set("kept.md", "kept", 1);
    const worker = new FakeCacheWorker();
    worker.restoredLedger.set("kept.md", {
      path: "kept.md",
      byte_length: 4,
      mtime_nanos: "1000000",
      indexable: true,
      content_hash: await sha256Text("kept"),
    });
    worker.restoredLedger.set("deleted.md", {
      path: "deleted.md",
      byte_length: 7,
      mtime_nanos: "1000000",
      indexable: true,
    });
    const store = new FakeCacheStore(cacheHit());
    const { controller, statuses, failures } = harness(source, worker, {}, {
      openStore: async () => ({ kind: "available", store }),
    });

    controller.start();
    await controller.whenIdle();

    expect(failures).toEqual([]);
    expect(worker.applyCalls).toEqual([{
      generation: "cached-generation",
      nextGeneration: "generation-1",
      upserts: [],
      removals: ["deleted.md"],
    }]);
    expect(worker.activeGeneration).toBe("generation-1");
    expect(worker.activePaths).toEqual(new Set(["kept.md"]));
    expect(statuses.at(-1)).toMatchObject({
      stage: "ready",
      searchable: true,
      generation: "generation-1",
      documents: 1,
      chunks: 1,
      unreadableSources: 0,
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

  it.each(["unreadable", "quarantined"] as const)(
    "never exports a generation with %s sources as a clean cache",
    async (omission) => {
      vi.useFakeTimers();
      try {
        const source = new FakeSource();
        source.set("a.md", "a", 1);
        source.set("b.md", "b", 1);
        source.set("c.md", "c", 1);
        const worker = new FakeCacheWorker();
        if (omission === "unreadable") {
          source.remainingReadFailures.set("c.md", 99);
        } else {
          worker.quarantinedSources = 1;
          worker.quarantineFields = ["chunks_contents"];
        }
        const store = new FakeCacheStore({ kind: "miss", reason: "absent" });
        const { controller, statuses } = harness(source, worker, {
          maxConcurrentReads: 3,
          maxStableReadAttempts: 2,
        }, {
          openStore: async () => ({ kind: "available", store }),
        });

        controller.start();
        await controller.whenIdle();
        expect(statuses.at(-1)).toMatchObject({
          stage: "ready",
          searchable: true,
          ...(omission === "unreadable"
            ? { unreadableSources: 1 }
            : { quarantinedSources: 1 }),
        });

        await vi.advanceTimersByTimeAsync(10_000);
        expect(worker.exportCalls).toEqual([]);
        expect(store.puts).toEqual([]);
      } finally {
        vi.useRealTimers();
      }
    },
  );

  it("never exports an unreadable replacement and exports the later clean generation", async () => {
    vi.useFakeTimers();
    try {
      const source = new FakeSource();
      source.set("a.md", "a", 1);
      source.set("b.md", "b", 1);
      source.set("unreadable.md", "last known good", 1);
      const worker = new FakeCacheWorker();
      const store = new FakeCacheStore({ kind: "miss", reason: "absent" });
      const { controller, statuses } = harness(source, worker, {
        maxConcurrentReads: 3,
        maxStableReadAttempts: 1,
      }, {
        openStore: async () => ({ kind: "available", store }),
      });

      controller.start();
      await controller.whenIdle();
      source.remainingReadFailures.set("unreadable.md", 99);
      controller.requestRebuild();
      await controller.whenIdle();

      expect(worker.activeGeneration).toBe("generation-1");
      expect(worker.activePaths).toEqual(new Set(["a.md", "b.md", "unreadable.md"]));
      expect(statuses.at(-1)).toMatchObject({
        stage: "degraded",
        generation: "generation-1",
        unreadableSources: 1,
        dirty: true,
        issue: "sources_unreadable",
      });
      await vi.advanceTimersByTimeAsync(10_000);
      expect(worker.exportCalls).toEqual([]);
      expect(store.puts).toEqual([]);

      source.remainingReadFailures.set("unreadable.md", 0);
      controller.requestRebuild();
      await controller.whenIdle();
      expect(worker.activeGeneration).toBe("generation-3");
      expect(worker.activePaths).toEqual(new Set(["a.md", "b.md", "unreadable.md"]));
      await vi.advanceTimersByTimeAsync(2_000);
      await vi.waitFor(() => expect(worker.exportCalls).toEqual(["generation-3"]));
      await vi.waitFor(() => expect(store.puts).toHaveLength(1));
      expect(store.puts[0]!.generationId).toBe("generation-3");
    } finally {
      vi.useRealTimers();
    }
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

  it("times out a cold read window, starts no later window, and ignores late results", async () => {
    vi.useFakeTimers();
    try {
      const source = new FakeSource();
      for (const path of ["a.md", "b.md", "c.md", "d.md", "later.md"]) source.set(path, path);
      const reads = new Map<string, ReturnType<typeof deferred<StableSourceRead>>>();
      const started: string[] = [];
      source.readSource = vi.fn((inspection) => {
        started.push(inspection.path);
        const pending = deferred<StableSourceRead>();
        reads.set(inspection.path, pending);
        return pending.promise;
      });
      const { controller, worker, statuses, failures } = harness(
        source,
        new FakeWorker(),
        { maxConcurrentReads: 4 },
        undefined,
        false,
        1_000,
      );

      controller.start();
      await flushAsyncWork();
      expect(started).toEqual(["a.md", "b.md", "c.md", "d.md"]);
      expect(statuses.at(-1)).toMatchObject({
        stage: "snapshot",
        progress: { activity: "read", completed: 0, total: 5, inFlight: 4 },
      });

      await vi.advanceTimersByTimeAsync(1_000);
      await controller.whenIdle();

      expect(started).not.toContain("later.md");
      expect(worker.calls).toEqual([
        "initialize",
        "begin:generation-1",
        "abort:generation-1",
      ]);
      expect(failures).toHaveLength(1);
      expect(statuses.at(-1)).toMatchObject({
        stage: "failed",
        searchable: false,
        generation: null,
        issue: "vault_read_failed",
        progress: {
          activity: "read",
          completed: 0,
          total: 5,
          inFlight: 0,
          stallCategory: "source_read_timeout",
        },
      });

      for (const [path, pending] of reads) {
        const record = source.records.get(path)!;
        pending.resolve({
          kind: "source",
          source: sourceInput(path, record.bytes, record.mtime),
        });
      }
      await flushAsyncWork();

      expect(worker.calls).toEqual([
        "initialize",
        "begin:generation-1",
        "abort:generation-1",
      ]);
      expect(statuses.at(-1)).toMatchObject({ stage: "failed", searchable: false });
    } finally {
      vi.useRealTimers();
    }
  });

  it("checkpoints only the acknowledged cold prefix before a timed-out window aborts", async () => {
    vi.useFakeTimers();
    try {
      const source = new FakeSource();
      source.set("a.md", "a");
      source.set("b.md", "b");
      const blockedRead = deferred<StableSourceRead>();
      const originalRead = source.readSource.bind(source);
      source.readSource = vi.fn((inspection) => (
        inspection.path === "b.md" ? blockedRead.promise : originalRead(inspection)
      ));
      const worker = new FakeCheckpointWorker();
      const store = new FakeCheckpointStore(
        { kind: "miss", reason: "absent" },
        { kind: "miss", reason: "absent" },
      );
      const { controller } = harness(
        source,
        worker,
        { maxBatchSources: 1, maxConcurrentReads: 1 },
        { openStore: async () => ({ kind: "available", store }) },
        false,
        1_000,
      );

      controller.start();
      await flushAsyncWork(24);
      expect(worker.calls).toContain("add:a.md");
      expect(source.readSource).toHaveBeenCalledTimes(2);

      await vi.advanceTimersByTimeAsync(1_000);
      await controller.whenIdle();

      expect(worker.checkpointExportCursors).toEqual([{
        snapshot_source_count: 2,
        acknowledged_add_batches: 1,
        acknowledged_prefix_sources: 1,
        last_acknowledged_path: "a.md",
      }]);
      expect(store.checkpointPuts).toHaveLength(1);
      expect(worker.calls).toContain("abort:generation-1");
      expect(worker.calls.some((call) => call.startsWith("commit:"))).toBe(false);

      const record = source.records.get("b.md")!;
      blockedRead.resolve({
        kind: "source",
        source: sourceInput("b.md", record.bytes, record.mtime),
      });
      await flushAsyncWork();
      expect(worker.calls).not.toContain("add:b.md");
    } finally {
      vi.useRealTimers();
    }
  });

  it("bounds failure checkpoint export liveness before aborting cold staging", async () => {
    vi.useFakeTimers();
    const exportGate = deferred<void>();
    try {
      const source = new FakeSource();
      source.set("a.md", "a");
      source.set("b.md", "b");
      const blockedRead = deferred<StableSourceRead>();
      const originalRead = source.readSource.bind(source);
      source.readSource = vi.fn((inspection) => (
        inspection.path === "b.md" ? blockedRead.promise : originalRead(inspection)
      ));
      const worker = new FakeCheckpointWorker();
      worker.checkpointExportGate = exportGate.promise;
      const store = new FakeCheckpointStore(
        { kind: "miss", reason: "absent" },
        { kind: "miss", reason: "absent" },
      );
      const { controller, statuses, failures } = harness(
        source,
        worker,
        { maxBatchSources: 1, maxConcurrentReads: 1 },
        { openStore: async () => ({ kind: "available", store }) },
        false,
        1_000,
      );

      controller.start();
      await flushAsyncWork(24);
      expect(worker.calls).toContain("add:a.md");
      await vi.advanceTimersByTimeAsync(1_000);
      await flushAsyncWork(24);
      expect(worker.checkpointExportCursors).toHaveLength(1);

      let idle = false;
      void controller.whenIdle().then(() => {
        idle = true;
      });
      await vi.advanceTimersByTimeAsync(1_499);
      await flushAsyncWork();
      expect(idle).toBe(false);
      await vi.advanceTimersByTimeAsync(1);
      await flushAsyncWork(24);

      expect(idle).toBe(true);
      expect(worker.calls).toContain("abort:generation-1");
      expect(store.checkpointPuts).toHaveLength(0);
      expect(failures).toHaveLength(1);
      expect(statuses.at(-1)).toMatchObject({
        stage: "failed",
        searchable: false,
        issue: "vault_read_failed",
        progress: { stallCategory: "source_read_timeout" },
      });

      const record = source.records.get("b.md")!;
      blockedRead.resolve({
        kind: "source",
        source: sourceInput("b.md", record.bytes, record.mtime),
      });
      await flushAsyncWork();
      expect(controller.requestRebuild()).toBe("scheduled");
      await controller.whenIdle();
      expect(statuses.at(-1)).toMatchObject({
        stage: "ready",
        searchable: true,
        generation: "generation-2",
      });

      const statusCount = statuses.length;
      exportGate.resolve();
      await flushAsyncWork(24);
      expect(store.checkpointPuts).toHaveLength(0);
      expect(statuses).toHaveLength(statusCount);
      expect((controller as unknown as {
        initialBuildCheckpointToken: InitialBuildCheckpointToken | null;
      }).initialBuildCheckpointToken).toBeNull();

      controller.dispose();
      await controller.whenDisposed();
      expect(store.disposed).toBe(1);
    } finally {
      exportGate.resolve();
      await flushAsyncWork(24);
      vi.useRealTimers();
    }
  });

  it("bounds failure checkpoint store liveness and ignores its late token", async () => {
    vi.useFakeTimers();
    const putGate = deferred<void>();
    try {
      const source = new FakeSource();
      source.set("a.md", "a");
      source.set("b.md", "b");
      const blockedRead = deferred<StableSourceRead>();
      const originalRead = source.readSource.bind(source);
      source.readSource = vi.fn((inspection) => (
        inspection.path === "b.md" ? blockedRead.promise : originalRead(inspection)
      ));
      const worker = new FakeCheckpointWorker();
      const store = new FakeCheckpointStore(
        { kind: "miss", reason: "absent" },
        { kind: "miss", reason: "absent" },
      );
      store.checkpointPutGate = putGate.promise;
      const { controller, statuses, failures } = harness(
        source,
        worker,
        { maxBatchSources: 1, maxConcurrentReads: 1 },
        { openStore: async () => ({ kind: "available", store }) },
        false,
        1_000,
      );

      controller.start();
      await flushAsyncWork(24);
      expect(worker.calls).toContain("add:a.md");
      await vi.advanceTimersByTimeAsync(1_000);
      await flushAsyncWork(24);
      expect(store.checkpointPuts).toHaveLength(1);

      let idle = false;
      void controller.whenIdle().then(() => {
        idle = true;
      });
      await vi.advanceTimersByTimeAsync(1_499);
      await flushAsyncWork();
      expect(idle).toBe(false);
      await vi.advanceTimersByTimeAsync(1);
      await flushAsyncWork(24);

      expect(idle).toBe(true);
      expect(worker.calls).toContain("abort:generation-1");
      expect(failures).toHaveLength(1);
      expect(statuses.at(-1)).toMatchObject({
        stage: "failed",
        searchable: false,
        issue: "vault_read_failed",
        progress: { stallCategory: "source_read_timeout" },
      });

      const statusCount = statuses.length;
      controller.dispose();
      let disposed = false;
      void controller.whenDisposed().then(() => {
        disposed = true;
      });
      await flushAsyncWork();
      expect(disposed).toBe(true);
      expect(store.disposed).toBe(1);

      putGate.resolve();
      await flushAsyncWork(24);
      expect(statuses).toHaveLength(statusCount + 1);
      expect(statuses.at(-1)).toMatchObject({ stage: "disposed", generation: null });
      expect((controller as unknown as {
        initialBuildCheckpointToken: InitialBuildCheckpointToken | null;
      }).initialBuildCheckpointToken).toBeNull();
    } finally {
      putGate.resolve();
      await flushAsyncWork(24);
      vi.useRealTimers();
    }
  });

  it("keeps a healthy initial build singular when manual rebuild is requested mid-batch", async () => {
    const source = new FakeSource();
    source.set("a.md", "a");
    source.set("b.md", "b");
    const addGate = deferred<void>();
    const addEntered = deferred<void>();
    class GatedInitialWorker extends FakeWorker {
      private gated = false;

      override async addSourceBatch(
        generation: string,
        sources: SourceUpsert[],
      ): Promise<IndexCounts> {
        if (!this.gated) {
          this.gated = true;
          addEntered.resolve();
          await addGate.promise;
        }
        return super.addSourceBatch(generation, sources);
      }
    }
    const worker = new GatedInitialWorker();
    const { controller, statuses } = harness(source, worker, {
      maxBatchSources: 1,
      maxConcurrentReads: 1,
    });

    controller.start();
    await addEntered.promise;
    const beforeRequest = statuses.at(-1)!;
    const statusCount = statuses.length;
    expect(beforeRequest).toMatchObject({
      stage: "snapshot",
      mutationEpoch: 0,
      progress: { activity: "prepare", completed: 0, total: 2, inFlight: 1 },
    });

    expect(controller.requestRebuild()).toBe("already_building");
    expect(controller.requestRebuild()).toBe("already_building");
    expect(statuses).toHaveLength(statusCount);
    expect(statuses.at(-1)).toEqual(beforeRequest);

    addGate.resolve();
    await controller.whenIdle();

    expect(worker.calls.filter((call) => call.startsWith("begin:"))).toEqual([
      "begin:generation-1",
    ]);
    expect(worker.calls.filter((call) => call.startsWith("commit:"))).toEqual([
      "commit:generation-1",
    ]);
    const completed = statuses
      .flatMap((status) => status.progress ? [status.progress.completed] : []);
    expect(completed).toEqual([...completed].sort((left, right) => left - right));
  });

  it("restarts a blocked cold build after a typed read timeout", async () => {
    const source = new FakeSource();
    source.set("note.md", "value");
    const originalRead = source.readSource.bind(source);
    let timedOut = false;
    source.readSource = vi.fn(async (inspection) => {
      if (!timedOut) {
        timedOut = true;
        return { kind: "timeout" as const, underlyingSettled: Promise.resolve() };
      }
      return originalRead(inspection);
    });
    const { controller, worker, statuses } = harness(source);

    controller.start();
    await controller.whenIdle();
    expect(statuses.at(-1)).toMatchObject({
      stage: "failed",
      issue: "vault_read_failed",
      progress: { stallCategory: "source_read_timeout" },
    });

    expect(controller.requestRebuild()).toBe("scheduled");
    await controller.whenIdle();

    expect(worker.calls.filter((call) => call.startsWith("begin:"))).toEqual([
      "begin:generation-1",
      "begin:generation-2",
    ]);
    expect(worker.calls).toContain("abort:generation-1");
    expect(worker.calls).toContain("commit:generation-2");
    expect(statuses.at(-1)).toMatchObject({
      stage: "ready",
      searchable: true,
      generation: "generation-2",
      documents: 1,
    });
  });

  it("aborts a timed-out replacement and keeps the complete active generation immutable", async () => {
    vi.useFakeTimers();
    try {
      const source = new FakeSource();
      source.set("a.md", "a");
      source.set("b.md", "b");
      const { controller, worker, statuses, failures } = harness(
        source,
        new FakeWorker(),
        { maxConcurrentReads: 2 },
        undefined,
        false,
        1_000,
      );
      controller.start();
      await controller.whenIdle();
      const activePaths = new Set(worker.activePaths);
      const addCallsBeforeReplacement = worker.calls.filter((call) => call.startsWith("add:")).length;
      const lateReads = new Map<string, ReturnType<typeof deferred<StableSourceRead>>>();
      source.readSource = vi.fn((inspection) => {
        const pending = deferred<StableSourceRead>();
        lateReads.set(inspection.path, pending);
        return pending.promise;
      });

      expect(controller.requestRebuild()).toBe("scheduled");
      await flushAsyncWork();
      expect(source.readSource).toHaveBeenCalledTimes(2);
      await vi.advanceTimersByTimeAsync(1_000);
      await controller.whenIdle();

      expect(worker.calls.filter((call) => call.startsWith("commit:"))).toEqual([
        "commit:generation-1",
      ]);
      expect(worker.calls).toContain("abort:generation-2");
      expect(worker.activeGeneration).toBe("generation-1");
      expect(worker.activePaths).toEqual(activePaths);
      expect(failures).toHaveLength(1);
      expect(statuses.at(-1)).toMatchObject({
        stage: "degraded",
        searchable: true,
        generation: "generation-1",
        documents: 2,
        issue: "vault_read_failed",
        progress: {
          activity: "read",
          completed: 0,
          total: 2,
          inFlight: 0,
          stallCategory: "source_read_timeout",
        },
      });

      for (const [path, pending] of lateReads) {
        const record = source.records.get(path)!;
        pending.resolve({
          kind: "source",
          source: sourceInput(path, record.bytes, record.mtime),
        });
      }
      await flushAsyncWork();
      expect(worker.activeGeneration).toBe("generation-1");
      expect(worker.activePaths).toEqual(activePaths);
      expect(worker.calls.filter((call) => call.startsWith("add:"))).toHaveLength(
        addCallsBeforeReplacement,
      );
    } finally {
      vi.useRealTimers();
    }
  });

  it("caps abandoned source reads across repeated blocked cold restarts", async () => {
    vi.useFakeTimers();
    try {
      const source = new FakeSource();
      for (const path of ["a.md", "b.md", "c.md", "d.md"]) source.set(path, path);
      const blocked = new Map<string, ReturnType<typeof deferred<StableSourceRead>>>();
      source.readSource = vi.fn((inspection) => {
        const pending = deferred<StableSourceRead>();
        blocked.set(inspection.path, pending);
        return pending.promise;
      });
      const { controller, worker, statuses } = harness(
        source,
        new FakeWorker(),
        { maxConcurrentReads: 4 },
        undefined,
        false,
        1_000,
      );

      controller.start();
      await flushAsyncWork();
      expect(source.readSource).toHaveBeenCalledTimes(4);
      await vi.advanceTimersByTimeAsync(1_000);
      await controller.whenIdle();

      for (let attempt = 0; attempt < 2; attempt += 1) {
        expect(controller.requestRebuild()).toBe("scheduled");
        await controller.whenIdle();
        expect(source.readSource).toHaveBeenCalledTimes(4);
        expect(statuses.at(-1)).toMatchObject({
          stage: "failed",
          progress: { stallCategory: "source_read_capacity" },
        });
      }

      const released = blocked.get("a.md")!;
      const record = source.records.get("a.md")!;
      released.resolve({
        kind: "source",
        source: sourceInput("a.md", record.bytes, record.mtime),
      });
      await flushAsyncWork();

      expect(controller.requestRebuild()).toBe("scheduled");
      await flushAsyncWork();
      expect(source.readSource).toHaveBeenCalledTimes(5);
      expect(source.readSource).toHaveBeenLastCalledWith(
        expect.objectContaining({ path: "a.md" }),
      );
      await vi.advanceTimersByTimeAsync(1_000);
      await controller.whenIdle();
      expect(source.readSource).toHaveBeenCalledTimes(5);
      expect(statuses.at(-1)).toMatchObject({
        stage: "failed",
        progress: { stallCategory: "source_read_timeout" },
      });
      expect(worker.calls.filter((call) => call.startsWith("begin:"))).toHaveLength(4);
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
    const originalRead = source.readSource.bind(source);
    source.readSource = vi.fn(async (inspection) => {
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
    const originalRead = source.readSource.bind(source);
    source.readSource = vi.fn(async (inspection) => {
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

  it("applies an extension-changing rename as one atomic remove and typed upsert", async () => {
    const source = new FakeSource();
    source.set("note.md", "# Note");
    const { controller, worker } = harness(source);
    controller.start();
    await controller.whenIdle();

    source.rename("note.md", "note.base");
    await controller.whenIdle();

    expect(worker.applyCalls.at(-1)).toEqual({
      generation: "generation-1",
      nextGeneration: "generation-2",
      upserts: ["note.base"],
      removals: ["note.md"],
    });
    expect(worker.applyUpsertFormats.at(-1)).toEqual(["base"]);
    expect([...worker.activePaths]).toEqual(["note.base"]);
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

  it("defers a pending preflight inspection failure to the stable-read retries", async () => {
    const source = new FakeSource();
    source.set("note.md", "first", 1);
    const { controller, worker, statuses, failures } = harness(source);
    controller.start();
    await controller.whenIdle();

    const originalInspect = source.inspectSource.bind(source);
    let postEventInspections = 0;
    source.inspectSource = vi.fn((path) => {
      postEventInspections += 1;
      if (postEventInspections === 1) throw new Error("simulated inspection failure");
      return originalInspect(path);
    });
    source.set("note.md", "second", 2);
    source.emit({ kind: "upsert", path: "note.md" });
    await controller.whenIdle();

    expect(postEventInspections).toBe(2);
    expect(failures).toEqual([]);
    expect(worker.applyCalls.at(-1)).toEqual({
      generation: "generation-1",
      nextGeneration: "generation-2",
      upserts: ["note.md"],
      removals: [],
    });
    expect(worker.activeGeneration).toBe("generation-2");
    expect(worker.activePaths).toEqual(new Set(["note.md"]));
    expect(statuses.at(-1)).toMatchObject({
      stage: "ready",
      searchable: true,
      generation: "generation-2",
      documents: 1,
      unreadableSources: 0,
      dirty: false,
    });
  });

  it("retries a thrown pending read failure and publishes the recovered source", async () => {
    const source = new FakeSource();
    source.set("note.md", "first", 1);
    const { controller, worker, statuses, failures } = harness(source, new FakeWorker(), {
      maxStableReadAttempts: 3,
    });
    controller.start();
    await controller.whenIdle();
    const attemptsBeforeUpdate = source.readAttempts.get("note.md") ?? 0;

    source.set("note.md", "second", 2);
    source.remainingReadFailures.set("note.md", 1);
    source.emit({ kind: "upsert", path: "note.md" });
    await controller.whenIdle();

    expect((source.readAttempts.get("note.md") ?? 0) - attemptsBeforeUpdate).toBe(2);
    expect(failures).toEqual([]);
    expect(worker.applyCalls.at(-1)).toEqual({
      generation: "generation-1",
      nextGeneration: "generation-2",
      upserts: ["note.md"],
      removals: [],
    });
    expect(worker.activeGeneration).toBe("generation-2");
    expect(worker.activePaths).toEqual(new Set(["note.md"]));
    expect(statuses.at(-1)).toMatchObject({
      stage: "ready",
      searchable: true,
      generation: "generation-2",
      unreadableSources: 0,
      dirty: false,
    });
  });

  it("keeps the last known-good active source after pending read retries are exhausted", async () => {
    const source = new FakeSource();
    source.set("a.md", "a", 1);
    source.set("b.md", "b", 1);
    source.set("unreadable.md", "old", 1);
    const { controller, worker, statuses, failures } = harness(source, new FakeWorker(), {
      maxStableReadAttempts: 2,
    });
    controller.start();
    await controller.whenIdle();
    const attemptsBeforeUpdate = source.readAttempts.get("unreadable.md") ?? 0;

    source.set("unreadable.md", "new", 2);
    source.remainingReadFailures.set("unreadable.md", 99);
    source.emit({ kind: "upsert", path: "unreadable.md" });
    await controller.whenIdle();

    expect((source.readAttempts.get("unreadable.md") ?? 0) - attemptsBeforeUpdate).toBe(2);
    expect(failures).toEqual([]);
    expect(worker.applyCalls).toEqual([]);
    expect(worker.activeGeneration).toBe("generation-1");
    expect(worker.activePaths).toEqual(new Set(["a.md", "b.md", "unreadable.md"]));
    expect(statuses.at(-1)).toEqual({
      stage: "degraded",
      searchable: true,
      generation: "generation-1",
      documents: 3,
      chunks: 3,
      sourceFormatCounts: sourceFormatCountsForPaths(["a.md", "b.md", "unreadable.md"]),
      quarantinedSources: 0,
      unreadableSources: 1,
      quarantineValidatorFields: [],
      dirty: true,
      rebuilding: false,
      mutationEpoch: expect.any(Number),
      issue: "sources_unreadable",
    });
  });

  it("keeps an unreadable rename atomic and recovers on an authoritative rescan", async () => {
    const source = new FakeSource();
    source.set("old.md", "last known good", 1);
    const { controller, worker, statuses, failures } = harness(source, new FakeWorker(), {
      maxStableReadAttempts: 1,
    });
    controller.start();
    await controller.whenIdle();

    source.remainingReadFailures.set("new.md", 99);
    source.rename("old.md", "new.md");
    await controller.whenIdle();

    expect(failures).toEqual([]);
    expect(worker.applyCalls).toEqual([]);
    expect(worker.calls.filter((call) => call.startsWith("commit:"))).toEqual([
      "commit:generation-1",
    ]);
    expect(worker.calls).toContain("abort:generation-3");
    expect(worker.activeGeneration).toBe("generation-1");
    expect(worker.activePaths).toEqual(new Set(["old.md"]));
    expect(statuses.at(-1)).toMatchObject({
      stage: "degraded",
      generation: "generation-1",
      unreadableSources: 1,
      dirty: true,
      issue: "sources_unreadable",
    });

    source.remainingReadFailures.set("new.md", 0);
    source.emit({ kind: "rescan" });
    await controller.whenIdle();

    expect(worker.calls.filter((call) => call.startsWith("commit:"))).toEqual([
      "commit:generation-1",
      "commit:generation-4",
    ]);
    expect(worker.activeGeneration).toBe("generation-4");
    expect(worker.activePaths).toEqual(new Set(["new.md"]));
    expect(statuses.at(-1)).toMatchObject({
      stage: "ready",
      generation: "generation-4",
      unreadableSources: 0,
      dirty: false,
    });
  });

  it("retries a transient stale read and publishes the stable source", async () => {
    const source = new FakeSource();
    source.set("note.md", "stable");
    const originalRead = source.readSource.bind(source);
    let attempts = 0;
    source.readSource = vi.fn(async (inspection) => {
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

  it("coalesces repeated ready-state manual rebuild requests into one replacement", async () => {
    const source = new FakeSource();
    source.set("a.md", "a");
    const { controller, worker, statuses } = harness(source);
    controller.start();
    await controller.whenIdle();

    expect(controller.requestRebuild()).toBe("scheduled");
    expect(controller.requestRebuild()).toBe("scheduled");
    expect(controller.requestRebuild()).toBe("scheduled");
    expect(statuses.at(-1)).toMatchObject({
      stage: "rebuild",
      progress: {
        activity: "inventory",
        completed: 0,
        total: null,
        inFlight: 0,
      },
    });
    await controller.whenIdle();

    expect(worker.calls.filter((call) => call.startsWith("begin:"))).toHaveLength(2);
    expect(worker.calls.filter((call) => call.startsWith("commit:"))).toHaveLength(2);
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

  it("isolates one source that never stabilizes and publishes truthful counts", async () => {
    const source = new FakeSource();
    source.set("a.md", "a");
    source.set("b.md", "b");
    source.set("changing.md", "value");
    source.staleReads.add("changing.md");
    const { controller, worker, statuses, failures } = harness(source, new FakeWorker(), {
      maxConcurrentReads: 3,
      maxStableReadAttempts: 2,
    });

    controller.start();
    await controller.whenIdle();

    expect(source.readAttempts.get("changing.md")).toBeGreaterThanOrEqual(2);
    expect(statuses.at(-1)).toMatchObject({
      stage: "ready",
      searchable: true,
      generation: "generation-1",
      documents: 2,
      chunks: 2,
      quarantinedSources: 0,
      unreadableSources: 1,
      dirty: false,
      issue: "sources_unreadable",
    });
    expect(failures).toEqual([]);
    expect(worker.activePaths).toEqual(new Set(["a.md", "b.md"]));
    expect(worker.calls.some((call) => call.startsWith("commit:"))).toBe(true);
  });

  it("surfaces an uncertain abort failure instead of reusing that Worker", async () => {
    const source = new FakeSource();
    for (const path of ["a.md", "b.md"]) {
      source.set(path, path);
      source.remainingReadFailures.set(path, 99);
    }
    const worker = new FakeWorker();
    worker.abortBuild = vi.fn(async () => {
      throw new WorkerRpcError({
        code: "timeout",
        stage: "lifecycle",
        message: "private detail",
        retryable: true,
      });
    });
    const { controller, failures } = harness(source, worker, {
      maxConcurrentReads: 2,
      maxStableReadAttempts: 1,
    });

    controller.start();
    await controller.whenIdle();

    expect(source.readAttempts).toEqual(new Map([
      ["a.md", 1],
      ["b.md", 1],
    ]));
    expect(worker.abortBuild).toHaveBeenCalledTimes(1);
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
    const originalRead = source.readSource.bind(source);
    source.readSource = vi.fn(async (inspection) => {
      await gate;
      return originalRead(inspection);
    });
    const { controller, worker, statuses } = harness(source);

    controller.start();
    await vi.waitFor(() => expect(source.readSource).toHaveBeenCalled());
    controller.dispose();
    release();
    await controller.whenIdle();

    expect(source.unsubscribed).toBe(1);
    expect(worker.calls.some((call) => call.startsWith("commit:"))).toBe(false);
    expect(statuses.at(-1)).toMatchObject({ stage: "disposed", searchable: false });
  });
});
