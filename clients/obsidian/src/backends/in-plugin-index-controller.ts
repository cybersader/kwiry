// SPDX-FileCopyrightText: 2026 cybersader
// SPDX-License-Identifier: GPL-3.0-only

import {
  ACTIVE_VAULT_ID,
  canonicalMtimeNanos,
  type ActiveVaultSource,
  type SourceInspection,
  type StableSourceRead,
  type VaultSourceEvent,
} from "../active-vault-source";
import type {
  CacheLoad,
  CacheStoreAvailability,
  CacheStorePort,
} from "../cache/cache-store";
import type {
  BuildResult,
  ExportGenerationResult,
  ReconciliationPlanResult,
  ReconciliationSourceMetadata,
  SourceRemoval,
  SourceUpsert,
} from "../worker/protocol";

export type IndexCounts = BuildResult;
export type { SourceRemoval } from "../worker/protocol";

export interface IndexWorkerPort {
  initialize(): Promise<unknown>;
  beginBuild(generation: string): Promise<IndexCounts>;
  addSourceBatch(generation: string, sources: SourceUpsert[]): Promise<IndexCounts>;
  applySourceChanges(
    generation: string,
    nextGeneration: string | null,
    upserts: SourceUpsert[],
    removals: SourceRemoval[],
  ): Promise<IndexCounts>;
  commitBuild(generation: string): Promise<IndexCounts>;
  abortBuild(generation: string): Promise<IndexCounts>;
}

export interface CacheIndexWorkerPort extends IndexWorkerPort {
  restoreGeneration(
    hit: Extract<CacheLoad, { kind: "hit" }>,
    expectedCacheIdentity: string,
  ): Promise<IndexCounts>;
  planReconciliation(
    generation: string,
    vaultId: string,
    currentSources: ReconciliationSourceMetadata[],
  ): Promise<ReconciliationPlanResult>;
  exportGeneration(generation: string, cacheIdentity: string): Promise<ExportGenerationResult>;
}

export type IndexControllerStage =
  | "starting"
  | "snapshot"
  | "replay"
  | "ready"
  | "rebuild"
  | "degraded"
  | "failed"
  | "disposed";

export type IndexControllerIssue =
  | "vault_read_failed"
  | "index_build_failed"
  | "index_update_failed"
  | "index_limit_exceeded"
  | "index_reconciling"
  | "cache_absent"
  | "cache_unavailable"
  | "cache_corrupt"
  | "cache_incompatible"
  | "cache_restore_unavailable"
  | "cache_discard_failed"
  | "cache_save_failed";

export interface IndexControllerStatus {
  stage: IndexControllerStage;
  searchable: boolean;
  generation: string | null;
  documents: number;
  chunks: number;
  dirty: boolean;
  rebuilding: boolean;
  progress?: {
    completed: number;
    total: number | null;
  };
  issue?: IndexControllerIssue;
}

export interface IndexControllerLimits {
  maxConcurrentReads: number;
  maxBatchSources: number;
  maxBatchBytes: number;
  maxPendingPaths: number;
  maxStableReadAttempts: number;
}

export interface IndexControllerCacheOptions {
  openStore: () => Promise<CacheStoreAvailability>;
  idleExportMs?: number;
  setTimer?: (callback: () => void, delayMs: number) => ReturnType<typeof setTimeout>;
  clearTimer?: (timer: ReturnType<typeof setTimeout>) => void;
}

export interface InPluginIndexControllerOptions {
  source: ActiveVaultSource;
  worker: IndexWorkerPort;
  nextGeneration: () => string;
  onStatus: (status: IndexControllerStatus) => void;
  onFailure?: (error: unknown) => void;
  yieldControl?: () => Promise<void>;
  limits?: Partial<IndexControllerLimits>;
  cache?: IndexControllerCacheOptions;
}

const DEFAULT_LIMITS: IndexControllerLimits = {
  maxConcurrentReads: 4,
  maxBatchSources: 16,
  maxBatchBytes: 16 * 1024 * 1024,
  maxPendingPaths: 2_048,
  maxStableReadAttempts: 3,
};
const DEFAULT_IDLE_EXPORT_MS = 2_000;
const MAX_GENERATION_ALLOCATION_ATTEMPTS = 32;

interface SnapshotEntry {
  path: string;
  inspection: SourceInspection;
}

interface Snapshot {
  entries: SnapshotEntry[];
  cut: number;
}

export class InPluginIndexController {
  private readonly source: ActiveVaultSource;
  private readonly worker: IndexWorkerPort;
  private readonly nextGeneration: () => string;
  private readonly onStatus: (status: IndexControllerStatus) => void;
  private readonly onFailure: (error: unknown) => void;
  private readonly yieldControl: () => Promise<void>;
  private readonly limits: IndexControllerLimits;
  private readonly cache: IndexControllerCacheOptions | null;
  private readonly idleExportMs: number;
  private readonly setTimer: NonNullable<IndexControllerCacheOptions["setTimer"]>;
  private readonly clearTimer: NonNullable<IndexControllerCacheOptions["clearTimer"]>;
  private readonly pendingUpserts = new Set<string>();
  private readonly pendingRemovals = new Set<string>();
  private readonly pendingRenames = new Map<string, string>();
  private readonly lastTouchedSequence = new Map<string, number>();
  private readonly attemptedExportGenerations = new Set<string>();
  private readonly issuedGenerationIds = new Set<string>();
  private unsubscribe: (() => void) | null = null;
  private running: Promise<void> | null = null;
  private exportRunning: Promise<void> | null = null;
  private disposal: Promise<void> = Promise.resolve();
  private exportTimer: ReturnType<typeof setTimeout> | null = null;
  private started = false;
  private disposed = false;
  private blocked = false;
  private workerInitialized = false;
  private startupDecided = false;
  private startupReconciling = false;
  private rebuildRequested = false;
  private rescanRequested = false;
  private rescanSequence = 0;
  private eventSequence = 0;
  private mutationEpoch = 0;
  private activeGeneration: string | null = null;
  private documents = 0;
  private chunks = 0;
  private stage: IndexControllerStage = "starting";
  private completed = 0;
  private total: number | null = null;
  private cacheStore: CacheStorePort | null = null;
  private cacheIssue: IndexControllerIssue | null = null;
  private lastPersistedGeneration: string | null = null;

  constructor(options: InPluginIndexControllerOptions) {
    this.source = options.source;
    this.worker = options.worker;
    this.nextGeneration = options.nextGeneration;
    this.onStatus = options.onStatus;
    this.onFailure = options.onFailure ?? (() => undefined);
    this.yieldControl = options.yieldControl ?? (() => Promise.resolve());
    this.limits = { ...DEFAULT_LIMITS, ...options.limits };
    this.cache = options.cache ?? null;
    this.idleExportMs = options.cache?.idleExportMs ?? DEFAULT_IDLE_EXPORT_MS;
    this.setTimer = options.cache?.setTimer ?? ((callback, delayMs) => setTimeout(callback, delayMs));
    this.clearTimer = options.cache?.clearTimer ?? ((timer) => clearTimeout(timer));
    validateLimits(this.limits);
    if (!Number.isSafeInteger(this.idleExportMs) || this.idleExportMs < 1) {
      throw new Error("idle export delay must be a positive integer");
    }
  }

  start(): void {
    if (this.started) return;
    if (this.disposed) throw new Error("in-plugin index controller is disposed");
    this.started = true;
    // Bind first. No Worker or cache await is allowed ahead of this line.
    this.unsubscribe = this.source.subscribe((event) => this.handleEvent(event));
    this.emit("starting");
    this.scheduleWork();
  }

  requestRebuild(): void {
    if (this.disposed) return;
    this.blocked = false;
    this.rebuildRequested = true;
    this.mutationEpoch += 1;
    if (this.cacheIssue === "cache_save_failed") this.cacheIssue = null;
    this.cancelExportTimer();
    this.completed = 0;
    this.total = null;
    this.emit(this.activeGeneration === null ? "starting" : "rebuild");
    this.scheduleWork();
  }

  async whenIdle(): Promise<void> {
    while (this.running) await this.running;
  }

  async whenDisposed(): Promise<void> {
    await this.disposal;
  }

  dispose(): void {
    if (this.disposed) return;
    this.disposed = true;
    this.blocked = true;
    this.mutationEpoch += 1;
    this.cancelExportTimer();
    this.unsubscribe?.();
    this.unsubscribe = null;
    this.pendingUpserts.clear();
    this.pendingRemovals.clear();
    this.pendingRenames.clear();
    this.rebuildRequested = false;
    this.rescanRequested = false;
    this.emit("disposed");
    const store = this.cacheStore;
    this.cacheStore = null;
    this.disposal = (async () => {
      try {
        await this.exportRunning;
      } catch {
        // A durability operation cannot delay or overturn disposal.
      }
      await store?.dispose();
    })();
  }

  private handleEvent(event: VaultSourceEvent): void {
    if (this.disposed) return;
    const sequence = ++this.eventSequence;
    this.mutationEpoch += 1;
    if (this.cacheIssue === "cache_save_failed") this.cacheIssue = null;
    this.cancelExportTimer();
    switch (event.kind) {
      case "upsert":
        this.lastTouchedSequence.set(event.path, sequence);
        this.queueUpsert(event.path);
        break;
      case "remove":
        this.lastTouchedSequence.set(event.path, sequence);
        this.queueRemoval(event.path);
        break;
      case "rename":
        this.lastTouchedSequence.set(event.oldPath, sequence);
        this.lastTouchedSequence.set(event.path, sequence);
        this.queueRename(event.oldPath, event.path);
        break;
      case "rescan":
        this.rescanSequence = sequence;
        this.requestAuthoritativeRescan();
        break;
    }
    this.enforcePendingBound();
    this.emit(this.stage);
    if (!this.blocked) this.scheduleWork();
  }

  private queueUpsert(path: string): void {
    this.pendingRemovals.delete(path);
    if ([...this.pendingRenames.values()].includes(path)) return;
    this.pendingUpserts.add(path);
  }

  private queueRemoval(path: string): void {
    const chained = [...this.pendingRenames.entries()]
      .find(([, destination]) => destination === path);
    if (chained) {
      const [root] = chained;
      const rootIsReused = [...this.pendingRenames.entries()]
        .some(([otherRoot, destination]) => otherRoot !== root && destination === root);
      if (rootIsReused) {
        this.requestAuthoritativeRescan();
        return;
      }
      this.pendingRenames.delete(root);
      this.pendingRemovals.add(root);
    }
    this.pendingUpserts.delete(path);
    this.pendingRemovals.add(path);
  }

  private queueRename(oldPath: string, path: string): void {
    if (this.pendingRenames.has(oldPath)) {
      this.requestAuthoritativeRescan();
      return;
    }
    const chained = [...this.pendingRenames.entries()]
      .find(([, destination]) => destination === oldPath);
    const root = chained?.[0] ?? oldPath;
    this.pendingRenames.set(root, path);
    if (this.hasRenameCycle()) {
      this.requestAuthoritativeRescan();
      return;
    }
    this.pendingUpserts.delete(root);
    this.pendingUpserts.delete(oldPath);
    this.pendingUpserts.delete(path);
    this.pendingRemovals.delete(path);
  }

  private hasRenameCycle(): boolean {
    for (const root of this.pendingRenames.keys()) {
      const seen = new Set<string>();
      let path: string | undefined = root;
      while (path !== undefined) {
        if (seen.has(path)) return true;
        seen.add(path);
        path = this.pendingRenames.get(path);
      }
    }
    return false;
  }

  private requestAuthoritativeRescan(): void {
    this.pendingUpserts.clear();
    this.pendingRemovals.clear();
    this.pendingRenames.clear();
    this.rescanRequested = true;
  }

  private enforcePendingBound(): void {
    const paths = new Set([...this.pendingUpserts, ...this.pendingRemovals]);
    for (const [oldPath, path] of this.pendingRenames) {
      paths.add(oldPath);
      paths.add(path);
    }
    if (paths.size <= this.limits.maxPendingPaths) return;
    this.requestAuthoritativeRescan();
    this.rescanSequence = this.eventSequence;
  }

  private scheduleWork(): void {
    if (this.disposed || this.blocked || this.running) return;
    const task = Promise.resolve()
      .then(() => this.runLoop())
      .catch((error: unknown) => this.handleFailure(error))
      .finally(() => {
        if (this.running === task) this.running = null;
        if (this.hasWork() && !this.blocked && !this.disposed) this.scheduleWork();
      });
    this.running = task;
  }

  private async runLoop(): Promise<void> {
    if (!this.workerInitialized) {
      await this.worker.initialize();
      this.requireActive();
      this.workerInitialized = true;
    }
    if (!this.startupDecided) {
      this.startupDecided = true;
      await this.decideStartup();
      this.requireActive();
    }

    while (this.hasMutationWork()) {
      this.requireActive();
      if (this.activeGeneration === null || this.rebuildRequested || this.rescanRequested) {
        const rebuilding = this.activeGeneration !== null;
        this.rebuildRequested = false;
        this.rescanRequested = false;
        await this.buildGeneration(rebuilding);
      } else if (this.hasPendingChanges()) {
        await this.flushActiveChanges();
      }
    }

    if (this.startupReconciling) {
      this.startupReconciling = false;
      if (this.cacheIssue === "index_reconciling") this.cacheIssue = null;
      this.emit("ready");
    }
  }

  private hasWork(): boolean {
    return !this.workerInitialized || !this.startupDecided || this.hasMutationWork();
  }

  private hasMutationWork(): boolean {
    return this.activeGeneration === null
      || this.rebuildRequested
      || this.rescanRequested
      || this.hasPendingChanges();
  }

  private hasPendingChanges(): boolean {
    return this.pendingUpserts.size > 0
      || this.pendingRemovals.size > 0
      || this.pendingRenames.size > 0;
  }

  private async decideStartup(): Promise<void> {
    if (!this.cache) return;
    let availability: CacheStoreAvailability;
    try {
      availability = await this.cache.openStore();
    } catch {
      this.requireActive();
      this.cacheIssue = "cache_unavailable";
      this.emit("starting");
      return;
    }
    if (this.disposed) {
      if (availability.kind === "available") await availability.store.dispose();
      this.requireActive();
    }
    if (availability.kind === "unavailable") {
      this.cacheIssue = "cache_unavailable";
      this.emit("starting");
      return;
    }

    this.cacheStore = availability.store;
    let loaded: CacheLoad;
    try {
      loaded = await availability.store.load();
    } catch {
      this.requireActive();
      this.cacheIssue = "cache_unavailable";
      this.emit("starting");
      return;
    }
    this.requireActive();

    if (loaded.kind === "miss") {
      await this.classifyCacheMissReason(loaded.reason);
      this.emit("starting");
      return;
    }
    if (!isCacheIndexWorker(this.worker)) {
      this.cacheIssue = "cache_restore_unavailable";
      this.emit("starting");
      return;
    }

    let counts: IndexCounts;
    try {
      counts = await this.worker.restoreGeneration(
        loaded,
        availability.store.vaultCacheIdentity,
      );
    } catch (error) {
      this.requireActive();
      const code = errorCode(error);
      if (code === "cache_digest_mismatch"
        || code === "cache_image_invalid"
        || code === "cache_blob_too_large") {
        await this.discardCache("corrupt", "cache_corrupt");
      } else if (code === "cache_version_mismatch" || code === "cache_identity_mismatch") {
        await this.discardCache("incompatible", "cache_incompatible");
      } else if (code === "internal_error" || code === "invalid_state") {
        this.cacheIssue = "cache_restore_unavailable";
      } else {
        throw error;
      }
      this.emit("starting");
      return;
    }

    this.requireActive();
    this.setActiveCounts(counts);
    this.lastPersistedGeneration = counts.generation;
    this.startupReconciling = true;
    this.cacheIssue = "index_reconciling";
    this.emit("replay");
    await this.reconcileRestoredGeneration();
  }

  private async classifyCacheMissReason(
    reason: Extract<CacheLoad, { kind: "miss" }>["reason"],
  ): Promise<void> {
    if (reason === "absent") {
      this.cacheIssue = "cache_absent";
      return;
    }
    if (reason === "identity_mismatch") {
      this.cacheIssue = "cache_incompatible";
      return;
    }
    await this.discardCache("corrupt", "cache_corrupt");
  }

  private async discardCache(
    reason: "corrupt" | "incompatible",
    successIssue: IndexControllerIssue,
  ): Promise<void> {
    const store = this.cacheStore;
    if (!store) {
      this.cacheIssue = "cache_unavailable";
      return;
    }
    try {
      await store.discard(reason);
      this.requireActive();
      this.cacheIssue = successIssue;
    } catch {
      this.requireActive();
      this.cacheIssue = "cache_discard_failed";
    }
  }

  private async reconcileRestoredGeneration(): Promise<void> {
    const worker = isCacheIndexWorker(this.worker) ? this.worker : null;
    const generation = this.activeGeneration;
    if (!worker || generation === null) throw new Error("cache reconciliation is unavailable");
    const snapshot = this.captureSnapshot();
    this.completed = 0;
    this.total = snapshot.entries.length;
    this.emit("replay");
    const current = snapshot.entries.map(({ inspection }) => inspectionMetadata(inspection));
    const plan = await worker.planReconciliation(generation, ACTIVE_VAULT_ID, current);
    this.requireActive();
    if (plan.generation !== this.activeGeneration) {
      throw new Error("reconciliation plan generation changed");
    }
    assertCompleteReconciliationPlan(plan, snapshot.entries.map((entry) => entry.path));

    const refresh = new Set(plan.refresh);
    const remove = new Set(plan.remove);
    for (const entry of snapshot.entries) {
      this.completed += 1;
      if (refresh.has(entry.path) && !this.wasTouchedAfter(entry.path, snapshot.cut)) {
        await this.applySnapshotRefresh(entry, snapshot.cut);
      }
      this.emit("replay");
      await this.yieldControl();
      this.requireActive();
    }
    for (const path of [...remove].sort(comparePaths)) {
      if (!this.wasTouchedAfter(path, snapshot.cut)) {
        await this.applyReconciliationChange([], [{ vault_id: ACTIVE_VAULT_ID, path }]);
      }
      this.emit("replay");
    }
  }

  private async applySnapshotRefresh(entry: SnapshotEntry, cut: number): Promise<void> {
    const read = await this.readSnapshot(entry.inspection);
    this.requireActive();
    if (this.wasTouchedAfter(entry.path, cut)) return;
    if (read.kind === "source") {
      await this.applyReconciliationChange([read.source], []);
      return;
    }
    if (read.kind === "oversized") {
      await this.applyReconciliationChange([oversizedInput(read)], []);
      return;
    }
    if (read.kind === "missing") this.queueRemoval(entry.path);
    else this.queueUpsert(entry.path);
  }

  private async applyReconciliationChange(
    upserts: SourceUpsert[],
    removals: SourceRemoval[],
  ): Promise<void> {
    const generation = this.activeGeneration;
    if (generation === null || (upserts.length === 0 && removals.length === 0)) return;
    const counts = await this.worker.applySourceChanges(
      generation,
      this.allocateFreshGeneration(),
      upserts,
      removals,
    );
    this.requireActive();
    this.setActiveCounts(counts);
  }

  private async buildGeneration(rebuilding: boolean): Promise<void> {
    const generation = this.allocateFreshGeneration();
    let began = false;
    try {
      this.emit(rebuilding ? "rebuild" : "snapshot");
      let counts = await this.worker.beginBuild(generation);
      began = true;
      this.requireActive();

      const snapshot = this.captureSnapshot();
      this.completed = 0;
      this.total = snapshot.entries.length;
      this.emit(rebuilding ? "rebuild" : "snapshot");
      counts = await this.addSnapshotSources(generation, snapshot, counts, rebuilding);

      if (this.rescanRequested) {
        await this.worker.abortBuild(generation);
        return;
      }

      this.emit(rebuilding ? "rebuild" : "replay");
      while (this.hasPendingChanges()) {
        counts = await this.applyPendingChanges(generation, null, counts);
        if (this.rescanRequested) {
          await this.worker.abortBuild(generation);
          return;
        }
      }

      counts = await this.worker.commitBuild(generation);
      this.requireActive();
      this.setActiveCounts(counts);
      this.completed = this.total ?? 0;
      this.emit(this.hasPendingChanges() ? "replay" : "ready");
    } catch (error) {
      if (began) {
        try {
          await this.worker.abortBuild(generation);
        } catch (abortError) {
          throw new AggregateError(
            [error, abortError],
            "index build failed and staging abort did not complete",
          );
        }
      }
      throw error;
    }
  }

  private captureSnapshot(): Snapshot {
    const paths = this.listMarkdownPaths();
    const entries = paths.flatMap((path) => {
      const inspection = this.inspectMarkdown(path);
      return inspection.kind === "missing" ? [] : [{ path, inspection }];
    });
    const cut = this.eventSequence;
    // The enumeration, inspection, cut, and acknowledgement are one synchronous
    // turn. Every event at or before the cut is represented by this snapshot;
    // events after it remain in the ordinary pending queues.
    this.pendingUpserts.clear();
    this.pendingRemovals.clear();
    this.pendingRenames.clear();
    if (this.rescanRequested && this.rescanSequence <= cut) this.rescanRequested = false;
    return { entries, cut };
  }

  private async addSnapshotSources(
    generation: string,
    snapshot: Snapshot,
    initialCounts: IndexCounts,
    rebuilding: boolean,
  ): Promise<IndexCounts> {
    let counts = initialCounts;
    let batch: SourceUpsert[] = [];
    let batchBytes = 0;

    const flush = async (): Promise<void> => {
      if (batch.length === 0) return;
      counts = await this.worker.addSourceBatch(generation, batch);
      this.requireActive();
      batch = [];
      batchBytes = 0;
      this.emit(rebuilding ? "rebuild" : "snapshot");
    };

    let cursor = 0;
    while (cursor < snapshot.entries.length) {
      let reservedBytes = 0;
      const window: Array<{ entry: SnapshotEntry; read: Promise<StableSourceRead> }> = [];
      while (cursor < snapshot.entries.length && window.length < this.limits.maxConcurrentReads) {
        const entry = snapshot.entries[cursor]!;
        const inspection = entry.inspection;
        if (inspection.kind === "candidate") {
          if (inspection.size > this.limits.maxBatchBytes) {
            throw new Error("source exceeds the configured batch byte limit");
          }
          if (batchBytes + reservedBytes + inspection.size > this.limits.maxBatchBytes) {
            if (window.length > 0) break;
            await flush();
            continue;
          }
          reservedBytes += inspection.size;
          window.push({ entry, read: this.readSnapshot(inspection) });
        } else {
          window.push({ entry, read: Promise.resolve(inspection) });
        }
        cursor += 1;
      }

      const settled = await Promise.allSettled(window.map((entry) => entry.read));
      this.requireActive();
      for (let index = 0; index < window.length; index += 1) {
        const result = settled[index]!;
        if (result.status === "rejected") throw result.reason;
        const { entry } = window[index]!;
        const read = result.value;
        this.completed += 1;
        if (!this.wasTouchedAfter(entry.path, snapshot.cut)) {
          const upsert = sourceUpsert(read);
          if (upsert) {
            const sourceBytes = "bytes" in upsert ? upsert.bytes.byteLength : 0;
            if (sourceBytes > this.limits.maxBatchBytes) {
              throw new Error("source exceeds the configured batch byte limit");
            }
            if (batch.length >= this.limits.maxBatchSources
              || batchBytes + sourceBytes > this.limits.maxBatchBytes) {
              await flush();
            }
            batch.push(upsert);
            batchBytes += sourceBytes;
          } else if (read.kind === "stale") {
            this.queueUpsert(entry.path);
          } else if (read.kind === "missing") {
            this.queueRemoval(entry.path);
          }
        }
        this.emit(rebuilding ? "rebuild" : "snapshot");
        if (batch.length >= this.limits.maxBatchSources) await flush();
      }
      await this.yieldControl();
      this.requireActive();
    }

    await flush();
    return counts;
  }

  private async flushActiveChanges(): Promise<void> {
    const generation = this.activeGeneration;
    if (generation === null) return;
    const counts = await this.applyPendingChanges(generation, this.allocateFreshGeneration(), {
      generation,
      documents: this.documents,
      chunks: this.chunks,
    });
    this.requireActive();
    this.setActiveCounts(counts);
    this.emit(this.startupReconciling || this.hasPendingChanges() ? "replay" : "ready");
  }

  private async applyPendingChanges(
    generation: string,
    nextGeneration: string | null,
    previousCounts: IndexCounts,
  ): Promise<IndexCounts> {
    const changes = this.takePendingChanges();
    if (changes.paths.length === 0) return previousCounts;

    try {
      const upserts: SourceUpsert[] = [];
      const removals = new Map<string, SourceRemoval>();
      for (const path of changes.removals) {
        removals.set(path, { vault_id: ACTIVE_VAULT_ID, path });
      }
      for (const path of changes.upserts) {
        const read = await this.readStable(path);
        this.requireActive();
        const upsert = sourceUpsert(read);
        if (upsert) {
          if ("bytes" in upsert && upsert.bytes.byteLength > this.limits.maxBatchBytes) {
            throw new Error("source exceeds the configured batch byte limit");
          }
          upserts.push(upsert);
          removals.delete(path);
        } else {
          removals.set(path, { vault_id: ACTIVE_VAULT_ID, path });
        }
      }

      if (upserts.length === 0 && removals.size === 0) return previousCounts;
      const counts = await this.worker.applySourceChanges(
        generation,
        nextGeneration,
        upserts,
        [...removals.values()],
      );
      this.requireActive();
      return counts;
    } catch (error) {
      if (changes.rename) {
        this.queueRename(changes.rename.oldPath, changes.rename.path);
      } else {
        for (const path of changes.removals) this.queueRemoval(path);
        for (const path of changes.upserts) this.queueUpsert(path);
      }
      throw error;
    }
  }

  private takePendingChanges(): {
    paths: string[];
    upserts: string[];
    removals: string[];
    rename?: { oldPath: string; path: string };
  } {
    const rename = this.pendingRenames.entries().next().value;
    if (rename) {
      const [oldPath, path] = rename;
      this.pendingRenames.delete(oldPath);
      return {
        paths: [oldPath, path],
        upserts: [path],
        removals: [oldPath],
        rename: { oldPath, path },
      };
    }

    const candidates = [...new Set([...this.pendingRemovals, ...this.pendingUpserts])]
      .sort(comparePaths);
    const paths: string[] = [];
    const upserts: string[] = [];
    const removals: string[] = [];
    let bytes = 0;

    for (const path of candidates) {
      if (paths.length >= 1) break;
      const isUpsert = this.pendingUpserts.has(path);
      if (isUpsert) {
        const inspection = this.inspectMarkdown(path);
        const estimated = inspection.kind === "candidate" ? inspection.size : 0;
        if (paths.length > 0 && bytes + estimated > this.limits.maxBatchBytes) continue;
        bytes += estimated;
      }
      paths.push(path);
      if (this.pendingUpserts.delete(path)) upserts.push(path);
      if (this.pendingRemovals.delete(path)) removals.push(path);
    }
    return { paths, upserts, removals };
  }

  private inspectMarkdown(path: string): SourceInspection {
    try {
      return this.source.inspectMarkdown(path);
    } catch (error) {
      throw new VaultSourceReadError(error);
    }
  }

  private listMarkdownPaths(): string[] {
    try {
      return [...this.source.listMarkdownPaths()].sort(comparePaths);
    } catch (error) {
      throw new VaultSourceReadError(error);
    }
  }

  private async readSnapshot(inspection: SourceInspection): Promise<StableSourceRead> {
    if (inspection.kind !== "candidate") return inspection;
    try {
      return await this.source.readMarkdown(inspection);
    } catch (error) {
      if (this.disposed) throw error;
      throw new VaultSourceReadError(error);
    }
  }

  private async readStable(path: string): Promise<StableSourceRead> {
    try {
      for (let attempt = 0; attempt < this.limits.maxStableReadAttempts; attempt += 1) {
        const inspection = this.inspectMarkdown(path);
        if (inspection.kind !== "candidate") return inspection;
        const read = await this.source.readMarkdown(inspection);
        this.requireActive();
        if (read.kind !== "stale") return read;
      }
    } catch (error) {
      if (this.disposed) throw error;
      throw new VaultSourceReadError(error);
    }
    throw new VaultSourceReadError(new Error("vault source did not become stable"));
  }

  private wasTouchedAfter(path: string, cut: number): boolean {
    return (this.lastTouchedSequence.get(path) ?? 0) > cut
      || this.rescanSequence > cut;
  }

  private allocateFreshGeneration(): string {
    for (let attempt = 0; attempt < MAX_GENERATION_ALLOCATION_ATTEMPTS; attempt += 1) {
      const generation = this.nextGeneration();
      if (generation === this.activeGeneration || this.issuedGenerationIds.has(generation)) continue;
      this.issuedGenerationIds.add(generation);
      return generation;
    }
    throw new Error("generation allocator did not produce a fresh identifier");
  }

  private setActiveCounts(counts: IndexCounts): void {
    const changed = this.activeGeneration !== counts.generation;
    this.activeGeneration = counts.generation;
    this.issuedGenerationIds.add(counts.generation);
    this.documents = counts.documents;
    this.chunks = counts.chunks;
    if (changed) {
      this.mutationEpoch += 1;
      this.cancelExportTimer();
    }
  }

  private handleFailure(error: unknown): void {
    if (this.disposed) return;
    this.blocked = true;
    this.cancelExportTimer();
    const hasActive = this.activeGeneration !== null;
    const issue = containsIndexLimitError(error)
      ? "index_limit_exceeded"
      : containsVaultSourceReadError(error)
        ? "vault_read_failed"
        : hasActive
          ? "index_update_failed"
          : "index_build_failed";
    this.emit(hasActive ? "degraded" : "failed", issue);
    this.onFailure(error);
  }

  private emit(stage: IndexControllerStage, explicitIssue?: IndexControllerIssue): void {
    this.stage = stage;
    const dirty = this.hasPendingChanges()
      || this.rebuildRequested
      || this.rescanRequested
      || this.startupReconciling
      || stage === "snapshot"
      || stage === "replay"
      || stage === "rebuild"
      || stage === "degraded"
      || stage === "failed";
    const progress = stage === "snapshot" || stage === "replay" || stage === "rebuild"
      ? { completed: this.completed, total: this.total }
      : undefined;
    const issue = explicitIssue
      ?? (this.startupReconciling ? "index_reconciling" : this.cacheIssue ?? undefined);
    const status: IndexControllerStatus = {
      stage,
      searchable: this.activeGeneration !== null && stage !== "disposed",
      generation: this.activeGeneration,
      documents: this.documents,
      chunks: this.chunks,
      dirty,
      rebuilding: stage === "rebuild",
      ...(progress ? { progress } : {}),
      ...(issue ? { issue } : {}),
    };
    this.onStatus(status);
    this.updateExportSchedule(status);
  }

  private updateExportSchedule(status: IndexControllerStatus): void {
    if (!this.cacheStore
      || !isCacheIndexWorker(this.worker)
      || !isCleanStatus(status)
      || status.generation === this.lastPersistedGeneration
      || this.attemptedExportGenerations.has(status.generation)) {
      this.cancelExportTimer();
      return;
    }
    if (this.exportTimer !== null || this.exportRunning !== null) return;
    const generation = status.generation;
    const epoch = this.mutationEpoch;
    this.exportTimer = this.setTimer(() => {
      this.exportTimer = null;
      const task = this.runIdleExport(generation, epoch).finally(() => {
        if (this.exportRunning === task) this.exportRunning = null;
      });
      this.exportRunning = task;
    }, this.idleExportMs);
  }

  private cancelExportTimer(): void {
    if (this.exportTimer === null) return;
    this.clearTimer(this.exportTimer);
    this.exportTimer = null;
  }

  private async runIdleExport(generation: string, epoch: number): Promise<void> {
    const store = this.cacheStore;
    if (!store || !isCacheIndexWorker(this.worker) || this.disposed) return;
    await this.whenIdle();
    if (!this.exportTupleIsCurrent(generation, epoch)) return;
    this.attemptedExportGenerations.add(generation);

    let exported: ExportGenerationResult;
    try {
      exported = await this.worker.exportGeneration(generation, store.vaultCacheIdentity);
    } catch {
      if (this.exportTupleIsCurrent(generation, epoch)) {
        this.cacheIssue = "cache_save_failed";
        this.emit("ready");
      }
      return;
    }
    if (!this.exportTupleIsCurrent(generation, epoch)) return;

    try {
      await store.put({
        generationId: exported.generation,
        byteLength: exported.blob_byte_length,
        sha256: exported.blob_sha256,
        bytes: exported.bytes,
        identity: {
          protocol_version: exported.protocol_version,
          cache_schema_version: exported.cache_schema_version,
          chunking_version: exported.chunking_version,
          sqlite_version: exported.sqlite_version,
          sqlite_wasm_sha256: exported.sqlite_wasm_sha256,
          rust_wasm_sha256: exported.rust_wasm_sha256,
          plugin_id: exported.plugin_id,
          plugin_version: exported.plugin_version,
          cache_identity: exported.cache_identity,
        },
      });
      this.lastPersistedGeneration = generation;
      if (this.exportTupleIsCurrent(generation, epoch)) {
        this.cacheIssue = null;
        this.emit("ready");
      }
    } catch {
      if (this.exportTupleIsCurrent(generation, epoch)) {
        this.cacheIssue = "cache_save_failed";
        this.emit("ready");
      }
    }
  }

  private exportTupleIsCurrent(generation: string, epoch: number): boolean {
    return !this.disposed
      && this.mutationEpoch === epoch
      && this.activeGeneration === generation
      && this.stage === "ready"
      && !this.hasPendingChanges()
      && !this.rebuildRequested
      && !this.rescanRequested
      && !this.startupReconciling;
  }

  private requireActive(): void {
    if (this.disposed) throw new Error("in-plugin index controller is disposed");
  }
}

class VaultSourceReadError extends Error {
  constructor(cause: unknown) {
    super("active vault source could not be read", { cause });
    this.name = "VaultSourceReadError";
  }
}

function isCacheIndexWorker(worker: IndexWorkerPort): worker is CacheIndexWorkerPort {
  const candidate = worker as Partial<CacheIndexWorkerPort>;
  return typeof candidate.restoreGeneration === "function"
    && typeof candidate.planReconciliation === "function"
    && typeof candidate.exportGeneration === "function";
}

function inspectionMetadata(inspection: SourceInspection): ReconciliationSourceMetadata {
  if (inspection.kind === "missing") {
    throw new VaultSourceReadError(new Error("enumerated source disappeared during inspection"));
  }
  return {
    path: inspection.path,
    byte_length: inspection.size,
    mtime_nanos: canonicalMtimeNanos(inspection.mtime),
    indexable: inspection.kind === "candidate",
  };
}

function oversizedInput(
  inspection: Extract<SourceInspection | StableSourceRead, { kind: "oversized" }>,
): SourceUpsert {
  return {
    oversized: true,
    descriptor: {
      vault_id: ACTIVE_VAULT_ID,
      path: inspection.path,
      format: "markdown",
      byte_length: inspection.size,
      mtime: Math.floor(inspection.mtime / 1_000),
      mtime_nanos: canonicalMtimeNanos(inspection.mtime),
    },
  };
}

function sourceUpsert(read: StableSourceRead): SourceUpsert | null {
  if (read.kind === "source") return read.source;
  if (read.kind === "oversized") return oversizedInput(read);
  return null;
}

function isCleanStatus(status: IndexControllerStatus): status is IndexControllerStatus & {
  generation: string;
} {
  return status.stage === "ready"
    && status.searchable
    && status.generation !== null
    && !status.dirty
    && !status.rebuilding;
}

function errorCode(error: unknown): string | null {
  return typeof error === "object" && error !== null && "code" in error
    && typeof error.code === "string"
    ? error.code
    : null;
}

function containsIndexLimitError(error: unknown): boolean {
  if (error instanceof AggregateError) {
    return error.errors.some((nested) => containsIndexLimitError(nested));
  }
  return errorCode(error) === "index_limit_exceeded";
}

function containsVaultSourceReadError(error: unknown): boolean {
  return error instanceof VaultSourceReadError
    || (error instanceof AggregateError
      && error.errors.some((nested) => containsVaultSourceReadError(nested)));
}

function validateLimits(limits: IndexControllerLimits): void {
  for (const value of Object.values(limits)) {
    if (!Number.isSafeInteger(value) || value < 1) {
      throw new Error("in-plugin index controller limits must be positive integers");
    }
  }
}

function assertCompleteReconciliationPlan(
  plan: ReconciliationPlanResult,
  currentPaths: readonly string[],
): void {
  const current = new Set(currentPaths);
  const unchanged = new Set(plan.unchanged);
  const refresh = new Set(plan.refresh);
  const remove = new Set(plan.remove);
  const uniqueCount = unchanged.size + refresh.size + remove.size;
  const declaredCount = plan.unchanged.length + plan.refresh.length + plan.remove.length;
  if (current.size !== currentPaths.length
    || uniqueCount !== declaredCount
    || plan.unchanged.some((path) => refresh.has(path) || remove.has(path))
    || plan.refresh.some((path) => remove.has(path))
    || plan.remove.some((path) => current.has(path))
    || plan.unchanged.some((path) => !current.has(path))
    || plan.refresh.some((path) => !current.has(path))
    || plan.unchanged.length + plan.refresh.length !== current.size
    || [...current].some((path) => !unchanged.has(path) && !refresh.has(path))
    || !Number.isSafeInteger(plan.stored_source_count)
    || !Number.isSafeInteger(plan.matched_source_count)
    || plan.stored_source_count < 0
    || plan.matched_source_count < plan.unchanged.length
    || plan.matched_source_count > current.size
    || plan.matched_source_count + plan.remove.length !== plan.stored_source_count) {
    throw new Error("reconciliation plan did not prove complete ledger coverage");
  }
}

function comparePaths(left: string, right: string): number {
  return left < right ? -1 : left > right ? 1 : 0;
}
