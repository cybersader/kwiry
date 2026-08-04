// SPDX-FileCopyrightText: 2026 cybersader
// SPDX-License-Identifier: GPL-3.0-only

import {
  ACTIVE_VAULT_ID,
  canonicalMtimeNanos,
  type ActiveVaultSource,
  type SourceInspection,
  type SourceReadOutcome,
  type StableSourceRead,
  type VaultSourceEvent,
} from "../active-vault-source";
import {
  INITIAL_BUILD_CHECKPOINT_ORDERING_VERSION,
  type CacheLoad,
  type CacheStoreAvailability,
  type CacheStoreBundlePort,
  type CacheStorePort,
  type InitialBuildCheckpointCursor,
  type InitialBuildCheckpointLoad,
  type InitialBuildCheckpointToken,
} from "../cache/cache-store";
import {
  EXTRACTION_COVERAGES,
  SOURCE_FORMATS,
  emptySourceFormatCounts,
} from "../worker/protocol";
import type {
  BuildResult,
  ExportGenerationResult,
  InitialBuildCheckpointExportResult,
  InitialBuildCheckpointReconciliationPlanResult,
  RestoreInitialBuildCheckpointResult,
  ReconciliationPlanResult,
  ReconciliationSourceMetadata,
  SourceFormatCounts,
  SourcePreparationDefectField,
  SourceRemoval,
  SourceUpsert,
} from "../worker/protocol";

export type IndexCounts = BuildResult;
export type { SourceRemoval } from "../worker/protocol";

export interface IndexWorkerPort {
  initialize(vaultId: string, sourcePolicyHash: string): Promise<unknown>;
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
    expectedSourcePolicyHash: string,
  ): Promise<IndexCounts>;
  planReconciliation(
    generation: string,
    vaultId: string,
    currentSources: ReconciliationSourceMetadata[],
  ): Promise<ReconciliationPlanResult>;
  exportGeneration(generation: string, cacheIdentity: string): Promise<ExportGenerationResult>;
}

export interface CheckpointIndexWorkerPort extends CacheIndexWorkerPort {
  exportInitialBuildCheckpoint(
    generation: string,
    cacheIdentity: string,
    cursor: InitialBuildCheckpointCursor,
  ): Promise<InitialBuildCheckpointExportResult>;
  restoreInitialBuildCheckpoint(
    hit: Extract<InitialBuildCheckpointLoad, { kind: "hit" }>,
    expectedCacheIdentity: string,
    expectedSourcePolicyHash: string,
  ): Promise<RestoreInitialBuildCheckpointResult>;
  planInitialBuildCheckpointReconciliation(
    generation: string,
    vaultId: string,
    currentSources: ReconciliationSourceMetadata[],
  ): Promise<InitialBuildCheckpointReconciliationPlanResult>;
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

export type IndexControllerReplaySubphase = "planning" | "verifying" | "applying";
export type IndexControllerActivity = "inventory" | "read" | "prepare" | "apply";
export type IndexControllerStallCategory =
  | "source_read_timeout"
  | "source_read_capacity"
  | "worker_timeout";
export type IndexControllerRebuildResult = "scheduled" | "already_building";

export type IndexControllerIssue =
  | "vault_read_failed"
  | "index_build_failed"
  | "index_update_failed"
  | "index_limit_exceeded"
  | "sources_quarantined"
  | "sources_unreadable"
  | "index_reconciling"
  | "cache_absent"
  | "cache_unavailable"
  | "cache_corrupt"
  | "cache_incompatible"
  | "cache_restore_unavailable"
  | "cache_discard_failed"
  | "cache_save_failed"
  | "checkpoint_corrupt"
  | "checkpoint_incompatible"
  | "checkpoint_unavailable"
  | "checkpoint_discard_failed"
  | "checkpoint_save_failed";

export interface InitialColdPreviewLease {
  generation: string;
  revision: number;
  processed: number;
  total: number;
  documents: number;
  chunks: number;
  quarantinedSources: number;
  unreadableSources: number;
}

export interface IndexControllerStatus {
  stage: IndexControllerStage;
  searchable: boolean;
  generation: string | null;
  initialColdPreview?: InitialColdPreviewLease;
  documents: number;
  chunks: number;
  sourceFormatCounts: SourceFormatCounts;
  quarantinedSources: number;
  unreadableSources: number;
  quarantineValidatorFields: readonly SourcePreparationDefectField[];
  dirty: boolean;
  rebuilding: boolean;
  mutationEpoch?: number;
  progress?: {
    activity: IndexControllerActivity;
    completed: number;
    total: number | null;
    inFlight: number;
    subphase?: IndexControllerReplaySubphase;
    stallCategory?: IndexControllerStallCategory;
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
  sourcePolicyHash?: string;
  openStore: () => Promise<CacheStoreAvailability>;
  idleExportMs?: number;
  setTimer?: (callback: () => void, delayMs: number) => ReturnType<typeof setTimeout>;
  clearTimer?: (timer: ReturnType<typeof setTimeout>) => void;
}

export type IndexControllerStartupObservation =
  | { kind: "cache_searchable"; cacheBytes: number }
  | { kind: "fully_current" }
  | {
      kind: "terminal";
      outcome: "degraded" | "failed" | "cancelled";
      reason: "sources_omitted" | "vault_unavailable" | "index_capacity" | "backend_unavailable";
    };

export interface InPluginIndexControllerOptions {
  source: ActiveVaultSource;
  worker: IndexWorkerPort;
  nextGeneration: () => string;
  onStatus: (status: IndexControllerStatus) => void;
  onFailure?: (error: unknown) => void;
  onStartupObservation?: (observation: IndexControllerStartupObservation) => void;
  yieldControl?: () => Promise<void>;
  limits?: Partial<IndexControllerLimits>;
  sourceReadTimeoutMs?: number;
  cache?: IndexControllerCacheOptions;
  initialColdPreview?: { enabled: true };
}

const DEFAULT_LIMITS: IndexControllerLimits = {
  maxConcurrentReads: 4,
  maxBatchSources: 16,
  maxBatchBytes: 16 * 1024 * 1024,
  maxPendingPaths: 2_048,
  maxStableReadAttempts: 3,
};
const DEFAULT_IDLE_EXPORT_MS = 2_000;
const DEFAULT_SOURCE_READ_TIMEOUT_MS = 30_000;
const FAILURE_CHECKPOINT_DEADLINE_MS = 1_500;
const DEFAULT_SOURCE_POLICY_HASH = "c414b56f31d22f8e1fbe69f5074bc8862337d1c8ee6065b6ad0da441b4f63860";
const MAX_GENERATION_ALLOCATION_ATTEMPTS = 32;
// A network share that disappears midway can yield a plausible-looking partial
// index. Requiring more than half of attempted reads to fail avoids publishing
// that systemic outage while still isolating a minority of unreadable notes;
// requiring two failures keeps one independently locked note source-isolated.
// This is deliberately a judgement-call heuristic, not a measurement: at this
// layer, many independent unreadable notes are indistinguishable from an outage.
const SYSTEMIC_UNREADABLE_READ_RATIO = 0.5;
const MIN_SYSTEMIC_UNREADABLE_SOURCES = 2;

type PresentSourceInspection = Exclude<SourceInspection, { kind: "missing" }>;

interface SnapshotEntry {
  path: string;
  inspection: PresentSourceInspection;
}

interface Snapshot {
  entries: SnapshotEntry[];
  cut: number;
}

interface SourceOmissions {
  sourceFormatCounts: SourceFormatCounts;
  quarantinedSources: number;
  unreadableSources: string[];
  quarantineValidatorFields: SourcePreparationDefectField[];
}

type ReconciliationProbe =
  | { kind: "read"; inspection: SourceInspection; read: StableSourceRead }
  | { kind: "unreadable" };

interface InitialBuildProgress {
  generation: string;
  snapshot: Snapshot;
  represented: boolean[];
  acknowledgedAddBatches: number;
  acknowledgedPrefixSources: number;
  lastAcknowledgedPath: string | null;
  counts: IndexCounts;
}

interface SourceReadWindowLease {
  generation: string;
  epoch: number;
  active: boolean;
  failure: Error | null;
  deadline: Promise<never>;
  rejectDeadline: (error: Error) => void;
  timer: ReturnType<typeof setTimeout>;
}

const INITIAL_BUILD_CHECKPOINT_BATCH_CADENCE = 25;

export class InPluginIndexController {
  private readonly source: ActiveVaultSource;
  private readonly worker: IndexWorkerPort;
  private readonly nextGeneration: () => string;
  private readonly onStatus: (status: IndexControllerStatus) => void;
  private readonly onFailure: (error: unknown) => void;
  private readonly onStartupObservation: (observation: IndexControllerStartupObservation) => void;
  private readonly yieldControl: () => Promise<void>;
  private readonly limits: IndexControllerLimits;
  private readonly cache: IndexControllerCacheOptions | null;
  private readonly sourcePolicyHash: string;
  private readonly initialColdPreviewEnabled: boolean;
  private readonly idleExportMs: number;
  private readonly sourceReadTimeoutMs: number;
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
  private checkpointRunning: Promise<void> | null = null;
  private shutdownPreparation: Promise<void> | null = null;
  private disposal: Promise<void> = Promise.resolve();
  private exportTimer: ReturnType<typeof setTimeout> | null = null;
  private started = false;
  private disposed = false;
  private stoppingForCheckpoint = false;
  private blocked = false;
  private workerInitialized = false;
  private startupDecided = false;
  private startupReconciling = false;
  private startupObservationFinished = false;
  private cacheSearchableObserved = false;
  private rebuildRequested = false;
  private replacementBuildInProgress = false;
  private rescanRequested = false;
  private rescanSequence = 0;
  private eventSequence = 0;
  private mutationEpoch = 0;
  private readEpoch = 0;
  private currentReadWindow: SourceReadWindowLease | null = null;
  private readonly outstandingSourceReads = new Set<Promise<SourceReadOutcome>>();
  private candidateGeneration: string | null = null;
  private activeGeneration: string | null = null;
  private documents = 0;
  private chunks = 0;
  private databaseBytes = 0;
  private databaseByteLimit = 1;
  private sourceFormatCounts = emptySourceFormatCounts();
  private quarantinedSources = 0;
  private readonly unreadableSources = new Set<string>();
  private readonly quarantineValidatorFields = new Set<SourcePreparationDefectField>();
  private activeSourceFormatCounts = emptySourceFormatCounts();
  private activeQuarantinedSources = 0;
  private activeUnreadableSources = 0;
  private readonly activeQuarantineValidatorFields = new Set<SourcePreparationDefectField>();
  private stage: IndexControllerStage = "starting";
  private activity: IndexControllerActivity = "inventory";
  private completed = 0;
  private total: number | null = null;
  private inFlight = 0;
  private stallCategory: IndexControllerStallCategory | null = null;
  private currentPath: string | null = null;
  private replaySubphase: IndexControllerReplaySubphase | null = null;
  private initialColdPreview: InitialColdPreviewLease | null = null;
  private initialColdPreviewGeneration: string | null = null;
  private initialColdPreviewRevision = 0;
  private initialColdPreviewProcessed = 0;
  private initialBuildProgress: InitialBuildProgress | null = null;
  private initialBuildCheckpointToken: InitialBuildCheckpointToken | null = null;
  private cacheStore: CacheStorePort | null = null;
  private cacheIssue: IndexControllerIssue | null = null;
  private lastPersistedGeneration: string | null = null;

  constructor(options: InPluginIndexControllerOptions) {
    this.source = options.source;
    this.worker = options.worker;
    this.nextGeneration = options.nextGeneration;
    this.onStatus = options.onStatus;
    this.onFailure = options.onFailure ?? (() => undefined);
    this.onStartupObservation = options.onStartupObservation ?? (() => undefined);
    this.yieldControl = options.yieldControl ?? (() => Promise.resolve());
    this.limits = { ...DEFAULT_LIMITS, ...options.limits };
    this.cache = options.cache ?? null;
    this.sourcePolicyHash = options.cache?.sourcePolicyHash ?? DEFAULT_SOURCE_POLICY_HASH;
    this.initialColdPreviewEnabled = options.initialColdPreview?.enabled === true;
    this.idleExportMs = options.cache?.idleExportMs ?? DEFAULT_IDLE_EXPORT_MS;
    this.sourceReadTimeoutMs = options.sourceReadTimeoutMs ?? DEFAULT_SOURCE_READ_TIMEOUT_MS;
    this.setTimer = options.cache?.setTimer ?? ((callback, delayMs) => setTimeout(callback, delayMs));
    this.clearTimer = options.cache?.clearTimer ?? ((timer) => clearTimeout(timer));
    validateLimits(this.limits);
    if (!Number.isSafeInteger(this.idleExportMs) || this.idleExportMs < 1) {
      throw new Error("idle export delay must be a positive integer");
    }
    if (!Number.isSafeInteger(this.sourceReadTimeoutMs) || this.sourceReadTimeoutMs < 1) {
      throw new Error("source read timeout must be a positive integer");
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

  requestRebuild(): IndexControllerRebuildResult {
    if (this.disposed || this.stoppingForCheckpoint) return "already_building";
    if (this.activeGeneration === null && !this.blocked) {
      // A healthy cold build or checkpoint resume already owns the only staging
      // generation. Do not erase its acknowledged progress/checkpoint state or
      // queue the redundant second generation observed during field indexing.
      return "already_building";
    }
    if (this.activeGeneration !== null
      && (this.rebuildRequested || this.replacementBuildInProgress)) {
      return "scheduled";
    }

    this.blocked = false;
    this.rebuildRequested = true;
    this.mutationEpoch += 1;
    this.stallCategory = null;
    this.inFlight = 0;
    if (this.cacheIssue === "cache_save_failed") this.cacheIssue = null;
    this.cancelExportTimer();
    // Omissions are NOT cleared here. Requesting a rebuild does not replace the
    // active generation -- that one stays searchable, still missing the same
    // notes, until buildGeneration() actually starts and clears them there.
    // Clearing on request drops the warning while the partial index is still
    // the one answering queries, which is the silent-partial-index failure this
    // whole change exists to prevent.
    this.activity = "inventory";
    this.completed = 0;
    this.currentPath = null;
    this.total = null;
    this.replaySubphase = null;
    this.clearInitialColdPreview();
    this.emit(this.activeGeneration === null ? "starting" : "rebuild");
    this.scheduleWork();
    return "scheduled";
  }

  async whenIdle(): Promise<void> {
    while (this.running) await this.running;
  }

  async whenDisposed(): Promise<void> {
    await this.disposal;
  }

  prepareForShutdown(deadlineMs: number): Promise<void> {
    if (this.shutdownPreparation) return this.shutdownPreparation;
    if (!Number.isSafeInteger(deadlineMs) || deadlineMs < 1) {
      return Promise.reject(new Error("shutdown checkpoint deadline must be a positive integer"));
    }
    if (this.disposed) return Promise.resolve();

    this.stoppingForCheckpoint = true;
    this.blocked = true;
    this.mutationEpoch += 1;
    this.invalidateCurrentReadWindow(new ShutdownRequestedError());
    this.cancelExportTimer();
    this.unsubscribe?.();
    this.unsubscribe = null;
    this.pendingUpserts.clear();
    this.pendingRemovals.clear();
    this.pendingRenames.clear();
    this.rebuildRequested = false;
    this.rescanRequested = false;
    this.clearInitialColdPreview();

    const deadlineAt = Date.now() + deadlineMs;
    const attempt = async (): Promise<void> => {
      try {
        await this.running;
      } catch {
        // The run-loop records ordinary failures itself. Shutdown only needs the
        // last state whose Worker acknowledgement is certain.
      }
      if (this.disposed || Date.now() >= deadlineAt) return;
      await this.persistInitialBuildCheckpoint("shutdown");
    };
    this.shutdownPreparation = raceWithDeadline(attempt(), deadlineMs);
    return this.shutdownPreparation;
  }

  dispose(): void {
    if (this.disposed) return;
    this.disposed = true;
    this.stoppingForCheckpoint = false;
    this.blocked = true;
    this.mutationEpoch += 1;
    this.invalidateCurrentReadWindow(new Error("in-plugin index controller is disposed"));
    this.cancelExportTimer();
    this.unsubscribe?.();
    this.unsubscribe = null;
    this.pendingUpserts.clear();
    this.pendingRemovals.clear();
    this.pendingRenames.clear();
    this.rebuildRequested = false;
    this.replacementBuildInProgress = false;
    this.rescanRequested = false;
    this.candidateGeneration = null;
    this.initialBuildProgress = null;
    this.clearInitialColdPreview();
    this.emit("disposed");
    const store = this.cacheStore;
    this.cacheStore = null;
    this.disposal = (async () => {
      try {
        await Promise.allSettled([this.exportRunning, this.checkpointRunning]);
      } finally {
        await store?.dispose();
      }
    })();
  }

  private handleEvent(event: VaultSourceEvent): void {
    if (this.disposed || this.stoppingForCheckpoint) return;
    const sequence = ++this.eventSequence;
    this.mutationEpoch += 1;
    if (this.cacheIssue === "cache_save_failed") this.cacheIssue = null;
    this.cancelExportTimer();
    this.clearInitialColdPreview();
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
    this.blocked = false;
    this.rescanRequested = true;
    this.clearInitialColdPreview();
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
      .catch((error: unknown) => {
        if (!(error instanceof ShutdownRequestedError)) this.handleFailure(error);
      })
      .finally(() => {
        if (this.running === task) this.running = null;
        if (this.hasWork() && !this.blocked && !this.disposed) this.scheduleWork();
      });
    this.running = task;
  }

  private async runLoop(): Promise<void> {
    if (!this.workerInitialized) {
      await this.worker.initialize(ACTIVE_VAULT_ID, this.sourcePolicyHash);
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
      this.replaySubphase = null;
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

    let checkpointEligible = loaded.kind === "miss";
    if (loaded.kind === "miss") {
      await this.classifyCacheMissReason(loaded.reason);
    } else if (loaded.record.identity.source_policy_hash !== this.sourcePolicyHash) {
      await this.discardCache("incompatible", "cache_incompatible");
      checkpointEligible = true;
    } else if (!isCacheIndexWorker(this.worker)) {
      this.cacheIssue = "cache_restore_unavailable";
      this.emit("starting");
      return;
    } else {
      let counts: IndexCounts | null = null;
      try {
        counts = await this.worker.restoreGeneration(
          loaded,
          availability.store.vaultCacheIdentity,
          this.sourcePolicyHash,
        );
      } catch (error) {
        this.requireActive();
        const code = errorCode(error);
        if (code === "cache_digest_mismatch"
          || code === "cache_image_invalid"
          || code === "cache_blob_too_large") {
          await this.discardCache("corrupt", "cache_corrupt");
          checkpointEligible = true;
        } else if (code === "cache_version_mismatch"
          || code === "cache_identity_mismatch") {
          await this.discardCache("incompatible", "cache_incompatible");
          checkpointEligible = true;
        } else if (code === "internal_error" || code === "invalid_state") {
          this.cacheIssue = "cache_restore_unavailable";
          this.emit("starting");
          return;
        } else {
          throw error;
        }
      }
      if (counts !== null) {
        this.requireActive();
        this.setActiveCounts(counts);
        this.lastPersistedGeneration = counts.generation;
        this.startupReconciling = true;
        this.cacheIssue = "index_reconciling";
        this.replaySubphase = "planning";
        this.completed = 0;
        this.total = null;
        this.currentPath = null;
        this.emit("replay");
        if (!this.cacheSearchableObserved) {
          this.cacheSearchableObserved = true;
          this.observeStartup({ kind: "cache_searchable", cacheBytes: loaded.record.byteLength });
        }
        await this.reconcileRestoredGeneration();
        return;
      }
    }

    if (checkpointEligible && await this.tryResumeInitialBuildCheckpoint()) return;
    this.emit("starting");
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

  private async tryResumeInitialBuildCheckpoint(): Promise<boolean> {
    const store = isCheckpointStore(this.cacheStore) ? this.cacheStore : null;
    const worker = isCheckpointIndexWorker(this.worker) ? this.worker : null;
    if (!store || !worker || this.activeGeneration !== null) return false;

    let loaded: InitialBuildCheckpointLoad;
    try {
      loaded = await store.loadInitialBuildCheckpoint();
    } catch {
      this.requireActive();
      this.cacheIssue = "checkpoint_unavailable";
      return false;
    }
    this.requireActive();
    if (loaded.kind === "miss") {
      if (loaded.reason === "absent") return false;
      if (loaded.reason === "identity_mismatch") {
        this.cacheIssue = "checkpoint_incompatible";
        return false;
      }
      // The store observed and conditionally cleaned malformed or incomplete
      // pointer state under its writer lock. A second unqualified discard here
      // could delete a newer checkpoint committed after that observation.
      this.cacheIssue = loaded.reason === "pointer_incompatible"
        ? "checkpoint_incompatible"
        : "checkpoint_corrupt";
      return false;
    }
    this.initialBuildCheckpointToken = {
      generationId: loaded.record.generationId,
      sha256: loaded.record.sha256,
    };
    if (loaded.record.identity.source_policy_hash !== this.sourcePolicyHash) {
      await this.discardInitialBuildCheckpoint("incompatible", "checkpoint_incompatible");
      return false;
    }

    let restored: RestoreInitialBuildCheckpointResult;
    try {
      restored = await worker.restoreInitialBuildCheckpoint(
        loaded,
        store.vaultCacheIdentity,
        this.sourcePolicyHash,
      );
    } catch (error) {
      this.requireActive();
      const code = errorCode(error);
      if (code === "checkpoint_digest_mismatch"
        || code === "checkpoint_image_invalid"
        || code === "checkpoint_blob_too_large") {
        await this.discardInitialBuildCheckpoint("corrupt", "checkpoint_corrupt");
      } else if (code === "checkpoint_version_mismatch"
        || code === "checkpoint_identity_mismatch"
        || code === "checkpoint_kind_mismatch") {
        await this.discardInitialBuildCheckpoint("incompatible", "checkpoint_incompatible");
      } else if (code === "internal_error" || code === "invalid_state") {
        this.cacheIssue = "checkpoint_unavailable";
      } else {
        throw error;
      }
      return false;
    }

    this.requireActive();
    if (restored.searchable || restored.publication !== "initial_staging"
      || restored.generation !== loaded.record.generationId
      || !sameCheckpointCursor(restored.cursor, loaded.record.cursor)) {
      try {
        await worker.abortBuild(loaded.record.generationId);
      } finally {
        await this.discardInitialBuildCheckpoint("corrupt", "checkpoint_corrupt");
      }
      return false;
    }

    try {
      await this.resumeInitialBuildFromCheckpoint(
        worker,
        loaded.record.generationId,
        loaded.record.cursor,
        restored,
      );
      return true;
    } catch (error) {
      if (error instanceof ShutdownRequestedError) throw error;
      this.invalidateReadWindowForGeneration(loaded.record.generationId, error);
      await this.persistInitialBuildCheckpoint("failure");
      let failure = error;
      try {
        await worker.abortBuild(loaded.record.generationId);
      } catch (abortError) {
        failure = new AggregateError(
          [error, abortError],
          "checkpoint resume failed and staging abort did not complete",
        );
      }
      this.candidateGeneration = null;
      this.initialBuildProgress = null;
      if (error instanceof CheckpointResumeFallbackError) {
        this.cacheIssue = "checkpoint_unavailable";
        return false;
      }
      throw failure;
    }
  }

  private async discardInitialBuildCheckpoint(
    reason: "corrupt" | "incompatible" | "completed",
    successIssue: IndexControllerIssue | null,
  ): Promise<void> {
    const store = isCheckpointStore(this.cacheStore) ? this.cacheStore : null;
    const expected = this.initialBuildCheckpointToken;
    if (!store || !expected) return;
    try {
      await store.discardInitialBuildCheckpoint(reason, expected);
      if (sameCheckpointToken(this.initialBuildCheckpointToken, expected)) {
        this.initialBuildCheckpointToken = null;
      }
      if (!this.disposed && successIssue !== null) this.cacheIssue = successIssue;
    } catch {
      if (!this.disposed) this.cacheIssue = "checkpoint_discard_failed";
    }
  }

  private async resumeInitialBuildFromCheckpoint(
    worker: CheckpointIndexWorkerPort,
    generation: string,
    cursor: InitialBuildCheckpointCursor,
    restored: IndexCounts,
  ): Promise<void> {
    this.candidateGeneration = generation;
    this.clearSourceOmissions();
    this.clearInitialColdPreview();
    this.initialColdPreviewGeneration = null;
    this.replaySubphase = "planning";
    this.completed = 0;
    this.total = null;
    this.currentPath = null;
    this.cacheIssue = "index_reconciling";
    this.emit("replay");

    const reconciled = await this.reconcileCheckpointPrefix(worker, generation, cursor, restored);
    this.requireActive();
    const snapshot = reconciled.snapshot;
    const prefixLength = snapshot.entries.findIndex((entry) => (
      cursor.last_acknowledged_path !== null
        && comparePaths(entry.path, cursor.last_acknowledged_path) > 0
    ));
    const representedPrefix = prefixLength < 0 ? snapshot.entries.length : prefixLength;
    const progress: InitialBuildProgress = {
      generation,
      snapshot,
      represented: snapshot.entries.map((_entry, index) => index < representedPrefix),
      // A saved batch count describes the old snapshot. Rebase it to the fresh
      // represented prefix so later suffix acknowledgements can never construct
      // a cursor with more batches than sources in the current snapshot.
      acknowledgedAddBatches: Math.min(
        cursor.acknowledged_add_batches,
        representedPrefix,
      ),
      acknowledgedPrefixSources: representedPrefix,
      lastAcknowledgedPath: representedPrefix > 0
        ? snapshot.entries[representedPrefix - 1]!.path
        : null,
      counts: reconciled.counts,
    };
    this.initialBuildProgress = progress;
    this.completed = representedPrefix;
    this.total = snapshot.entries.length;
    this.currentPath = progress.lastAcknowledgedPath;
    this.replaySubphase = null;
    this.emit("snapshot");

    const suffix = snapshot.entries
      .map((entry, ordinal) => ({ entry, ordinal }))
      .filter(({ ordinal }) => ordinal >= representedPrefix);
    let counts = await this.addSnapshotSources(
      generation,
      snapshot,
      reconciled.counts,
      false,
      progress,
      suffix,
    );
    this.requireActive();

    this.emit("replay");
    while (this.hasPendingChanges()) {
      counts = await this.applyPendingChanges(generation, null, counts);
      this.syncWorkerQuarantines(counts);
      progress.counts = counts;
      this.requireActive();
    }

    counts = await worker.commitBuild(generation);
    this.requireActive();
    this.setActiveCounts(counts);
    this.candidateGeneration = null;
    this.initialBuildProgress = null;
    this.activity = "apply";
    this.completed = this.total ?? 0;
    this.inFlight = 0;
    this.stallCategory = null;
    this.cacheIssue = null;
    this.emit(this.hasPendingChanges() ? "replay" : "ready");
    await this.discardInitialBuildCheckpoint("completed", null);
    if (this.cacheIssue === "checkpoint_discard_failed") this.emit("ready");
  }

  private async reconcileCheckpointPrefix(
    worker: CheckpointIndexWorkerPort,
    generation: string,
    cursor: InitialBuildCheckpointCursor,
    initialCounts: IndexCounts,
  ): Promise<{ snapshot: Snapshot; counts: IndexCounts }> {
    let counts = initialCounts;
    for (;;) {
      this.requireActive();
      const snapshot = this.captureSnapshot();
      const expectedEpoch = this.mutationEpoch;
      const prefix = cursor.last_acknowledged_path === null
        ? []
        : snapshot.entries.filter((entry) => comparePaths(entry.path, cursor.last_acknowledged_path!) <= 0);
      const prefixPaths = new Set(prefix.map((entry) => entry.path));
      this.replaySubphase = "planning";
      this.completed = 0;
      this.total = snapshot.entries.length;
      this.currentPath = null;
      this.emit("replay");
      // Plan against the complete fresh snapshot so represented staging rows in
      // the untouched suffix are matched, never misclassified as deletions merely
      // because the conservative cursor stops before them. Only the proven prefix
      // classifications are applied here; the suffix is replayed ordinarily.
      const plan = await worker.planInitialBuildCheckpointReconciliation(
        generation,
        ACTIVE_VAULT_ID,
        snapshot.entries.map(({ inspection }) => inspectionMetadata(inspection)),
      );
      this.requireActive();
      if (this.mutationEpoch !== expectedEpoch) continue;
      if (plan.generation !== generation || plan.publication !== "initial_staging" || plan.searchable) {
        throw new Error("checkpoint reconciliation plan changed publication state");
      }
      assertCompleteReconciliationPlan(plan, snapshot.entries.map((entry) => entry.path));

      const refresh = new Set(plan.refresh.filter((path) => prefixPaths.has(path)));
      const audit = new Map(plan.audit
        .filter((entry) => prefixPaths.has(entry.path))
        .map((entry) => [entry.path, entry.content_hash]));
      const probedUpserts = new Map<string, SourceUpsert>();
      const removals = new Set<string>();
      let attemptedReads = 0;
      let unreadableReads = 0;
      this.replaySubphase = "verifying";
      this.completed = 0;
      this.total = new Set([...refresh, ...audit.keys(), ...plan.remove]).size;
      if (this.total > 0) this.emit("replay");

      for (const entry of prefix) {
        if (!refresh.has(entry.path) && !audit.has(entry.path)) continue;
        this.currentPath = entry.path;
        if (entry.inspection.kind === "candidate") attemptedReads += 1;
        const probe = await this.probeSnapshotRefresh(entry);
        this.requireActive();
        if (this.mutationEpoch !== expectedEpoch) break;
        if (probe.kind === "unreadable") {
          unreadableReads += 1;
        } else {
          const expectedHash = audit.get(entry.path);
          const unchanged = expectedHash !== undefined
            && probe.read.kind === "source"
            && await sha256Hex(probe.read.source.bytes) === expectedHash;
          if (!unchanged) {
            const upsert = sourceUpsert(probe.read);
            if (upsert) probedUpserts.set(entry.path, upsert);
            else throw new CheckpointResumeFallbackError();
          }
        }
        this.completed += 1;
        if (this.total !== null && this.completed < this.total) this.emit("replay");
        await this.yieldControl();
      }
      if (this.mutationEpoch !== expectedEpoch) continue;

      for (const path of [...plan.remove].sort(comparePaths)) {
        this.currentPath = path;
        // Planner removals remain hypotheses even outside the saved prefix. A
        // crossing rename or post-cut creation can make the path present again;
        // only an independent current inspection authorizes deleting its staged
        // row. Present paths are conservatively upserted instead.
        attemptedReads += 1;
        const probe = await this.probePotentialRemoval(path);
        this.requireActive();
        if (this.mutationEpoch !== expectedEpoch) break;
        if (probe.kind === "unreadable") {
          unreadableReads += 1;
        } else if (probe.inspection.kind === "missing") {
          removals.add(path);
        } else {
          const upsert = sourceUpsert(probe.read);
          if (upsert) probedUpserts.set(path, upsert);
          else throw new CheckpointResumeFallbackError();
        }
        this.completed += 1;
        if (this.total !== null && this.completed < this.total) this.emit("replay");
        await this.yieldControl();
      }
      if (this.mutationEpoch !== expectedEpoch) continue;
      if (isSystemicUnreadability(unreadableReads, attemptedReads)) {
        throw new VaultUnavailableError(new Error("active vault became unreadable"));
      }
      if (unreadableReads > 0) throw new CheckpointResumeFallbackError();

      this.replaySubphase = "applying";
      this.completed = 0;
      this.total = probedUpserts.size + removals.size;
      const paths = [...new Set([...probedUpserts.keys(), ...removals])].sort(comparePaths);
      for (const path of paths) {
        if (this.mutationEpoch !== expectedEpoch) break;
        this.currentPath = path;
        counts = await worker.applySourceChanges(
          generation,
          null,
          probedUpserts.has(path) ? [probedUpserts.get(path)!] : [],
          removals.has(path) ? [{ vault_id: ACTIVE_VAULT_ID, path }] : [],
        );
        this.requireActive();
        this.syncWorkerQuarantines(counts);
        this.completed += 1;
        if (this.total !== null && this.completed < this.total) this.emit("replay");
      }
      if (this.mutationEpoch !== expectedEpoch) continue;
      return { snapshot, counts };
    }
  }

  private async reconcileRestoredGeneration(): Promise<void> {
    const worker = isCacheIndexWorker(this.worker) ? this.worker : null;
    const generation = this.activeGeneration;
    if (!worker || generation === null) throw new Error("cache reconciliation is unavailable");
    let expectedGeneration = generation;
    let expectedEpoch = this.mutationEpoch;
    const snapshot = this.captureSnapshot();
    this.replaySubphase = "planning";
    this.completed = 0;
    this.currentPath = null;
    this.total = snapshot.entries.length;
    this.emit("replay");
    const current = snapshot.entries.map(({ inspection }) => inspectionMetadata(inspection));
    const plan = await worker.planReconciliation(generation, ACTIVE_VAULT_ID, current);
    this.requireActive();
    if (!this.reconciliationTupleIsCurrent(expectedGeneration, expectedEpoch)) {
      this.retrySupersededReconciliation();
      return;
    }
    if (plan.generation !== this.activeGeneration) {
      throw new Error("reconciliation plan generation changed");
    }
    assertCompleteReconciliationPlan(plan, snapshot.entries.map((entry) => entry.path));

    const refresh = new Set(plan.refresh);
    const audit = new Map(plan.audit.map((entry) => [entry.path, entry.content_hash]));
    plan.remove.sort(comparePaths);
    let attemptedRefreshChecks = 0;
    let unreadableRefreshChecks = 0;
    let attemptedRemovalChecks = 0;
    let unreadableRemovalChecks = 0;
    const bufferedProbes = new Map<string, Extract<ReconciliationProbe, { kind: "read" }>>();
    let bufferedProbeBytes = 0;
    const retainProbe = (path: string, probe: ReconciliationProbe): void => {
      if (probe.kind === "unreadable") return;
      const bytes = probe.read.kind === "source" ? probe.read.source.bytes.byteLength : 0;
      if (bufferedProbes.size >= this.limits.maxPendingPaths
        || bytes > this.limits.maxBatchBytes - bufferedProbeBytes) return;
      bufferedProbes.set(path, probe);
      bufferedProbeBytes += bytes;
    };

    this.replaySubphase = "verifying";
    this.completed = 0;
    this.currentPath = null;
    this.total = new Set([...refresh, ...audit.keys()]).size + plan.remove.length;
    if (this.total > 0) this.emit("replay");

    // Probe the whole pass before publishing. Successful reads are retained up to
    // the ordinary path and byte budgets; overflow is safely reread after verdict.
    for (const entry of snapshot.entries) {
      if ((refresh.has(entry.path) || audit.has(entry.path))
        && !this.wasTouchedAfter(entry.path, snapshot.cut)) {
        this.currentPath = entry.path;
        if (entry.inspection.kind === "candidate") attemptedRefreshChecks += 1;
        const probe = await this.probeSnapshotRefresh(entry);
        if (probe.kind === "unreadable") {
          unreadableRefreshChecks += 1;
        } else {
          const expectedHash = audit.get(entry.path);
          if (expectedHash !== undefined
            && probe.read.kind === "source"
            && await sha256Hex(probe.read.source.bytes) === expectedHash) {
            audit.delete(entry.path);
          } else {
            refresh.add(entry.path);
            retainProbe(entry.path, probe);
          }
        }
        this.completed += 1;
        this.requireActive();
        if (!this.reconciliationTupleIsCurrent(expectedGeneration, expectedEpoch)) {
          this.retrySupersededReconciliation();
          return;
        }
        if (this.total !== null && this.completed < this.total) this.emit("replay");
      }
      await this.yieldControl();
      this.requireActive();
      if (!this.reconciliationTupleIsCurrent(expectedGeneration, expectedEpoch)) {
        this.retrySupersededReconciliation();
        return;
      }
    }

    // A planner removal is only a hypothesis until the source API independently
    // confirms the path is gone. This also protects cached rows omitted by a
    // transiently incomplete inventory or failed first inspection.
    for (const path of plan.remove) {
      if (this.wasTouchedAfter(path, snapshot.cut)) continue;
      this.currentPath = path;
      attemptedRemovalChecks += 1;
      const probe = await this.probePotentialRemoval(path);
      if (probe.kind === "unreadable") unreadableRemovalChecks += 1;
      else retainProbe(path, probe);
      this.completed += 1;
      this.requireActive();
      if (!this.reconciliationTupleIsCurrent(expectedGeneration, expectedEpoch)) {
        this.retrySupersededReconciliation();
        return;
      }
      if (this.total !== null && this.completed < this.total) this.emit("replay");
      await this.yieldControl();
      this.requireActive();
      if (!this.reconciliationTupleIsCurrent(expectedGeneration, expectedEpoch)) {
        this.retrySupersededReconciliation();
        return;
      }
    }

    if (isSystemicUnreadability(unreadableRefreshChecks, attemptedRefreshChecks)
      || isSystemicUnreadability(unreadableRemovalChecks, attemptedRemovalChecks)) {
      // This is deliberately evaluated after the complete probe so early failures
      // cannot misclassify a pass whose remaining independent reads succeed.
      throw new VaultUnavailableError(new Error("active vault became unreadable"));
    }

    this.replaySubphase = "applying";
    this.completed = 0;
    this.currentPath = null;
    this.total = refresh.size + plan.remove.length;
    if (this.total > 0) this.emit("replay");

    // Publish only after the verdict. Retained reads avoid a second network trip;
    // probes beyond the configured budgets are reread one at a time here.
    for (const entry of snapshot.entries) {
      if (refresh.has(entry.path) && !this.wasTouchedAfter(entry.path, snapshot.cut)) {
        this.currentPath = entry.path;
        const generationBeforeApply: string | null = this.activeGeneration;
        if (!await this.applySnapshotRefresh(
          entry,
          snapshot.cut,
          expectedGeneration,
          expectedEpoch,
          bufferedProbes.get(entry.path)?.read,
        )) {
          this.retrySupersededReconciliation();
          return;
        }
        if (this.activeGeneration !== generationBeforeApply) this.completed += 1;
        expectedGeneration = this.activeGeneration ?? expectedGeneration;
        expectedEpoch = this.mutationEpoch;
        if (this.total !== null && this.completed < this.total) this.emit("replay");
      }
      await this.yieldControl();
      this.requireActive();
      if (!this.reconciliationTupleIsCurrent(expectedGeneration, expectedEpoch)) {
        this.retrySupersededReconciliation();
        return;
      }
    }

    for (const path of plan.remove) {
      if (this.wasTouchedAfter(path, snapshot.cut)) continue;
      this.currentPath = path;
      const buffered = bufferedProbes.get(path);
      let inspection: SourceInspection;
      if (buffered) {
        inspection = buffered.inspection;
      } else {
        try {
          inspection = this.inspectSource(path);
        } catch (error) {
          if (!(error instanceof UnreadableVaultSourceError)) throw error;
          this.unreadableSources.add(path);
          if (this.total !== null && this.completed < this.total) this.emit("replay");
          continue;
        }
      }
      this.requireActive();
      if (!this.reconciliationTupleIsCurrent(expectedGeneration, expectedEpoch)) {
        this.retrySupersededReconciliation();
        return;
      }
      const generationBeforeApply: string | null = this.activeGeneration;
      if (inspection.kind === "missing") {
        this.unreadableSources.delete(path);
        await this.applyReconciliationChange([], [{ vault_id: ACTIVE_VAULT_ID, path }]);
      } else if (!await this.applySnapshotRefresh(
        { path, inspection },
        snapshot.cut,
        expectedGeneration,
        expectedEpoch,
        buffered?.read,
      )) {
        this.retrySupersededReconciliation();
        return;
      }
      if (this.activeGeneration !== generationBeforeApply) this.completed += 1;
      expectedGeneration = this.activeGeneration ?? expectedGeneration;
      expectedEpoch = this.mutationEpoch;
      if (this.total !== null && this.completed < this.total) this.emit("replay");
      await this.yieldControl();
      this.requireActive();
      if (!this.reconciliationTupleIsCurrent(expectedGeneration, expectedEpoch)) {
        this.retrySupersededReconciliation();
        return;
      }
    }
    if (this.hasPendingChanges()) this.currentPath = null;
  }

  private async probeSnapshotRefresh(entry: SnapshotEntry): Promise<ReconciliationProbe> {
    try {
      const read = await this.readSnapshot(entry.inspection);
      this.requireActive();
      return { kind: "read", inspection: entry.inspection, read };
    } catch (error) {
      if (!(error instanceof UnreadableVaultSourceError)) throw error;
      this.unreadableSources.add(entry.path);
      return { kind: "unreadable" };
    }
  }

  private async probePotentialRemoval(path: string): Promise<ReconciliationProbe> {
    let inspection: SourceInspection;
    try {
      inspection = this.inspectSource(path);
    } catch (error) {
      if (!(error instanceof UnreadableVaultSourceError)) throw error;
      this.unreadableSources.add(path);
      return { kind: "unreadable" };
    }
    if (inspection.kind === "missing") {
      return { kind: "read", inspection, read: inspection };
    }
    return this.probeSnapshotRefresh({ path, inspection });
  }

  private async applySnapshotRefresh(
    entry: SnapshotEntry,
    cut: number,
    expectedGeneration: string,
    expectedEpoch: number,
    probedRead?: StableSourceRead,
  ): Promise<boolean> {
    let read: StableSourceRead;
    try {
      read = probedRead ?? await this.readSnapshot(entry.inspection);
    } catch (error) {
      if (!(error instanceof UnreadableVaultSourceError)) throw error;
      this.requireActive();
      if (!this.reconciliationTupleIsCurrent(expectedGeneration, expectedEpoch)) return false;
      // A read exception is evidence about availability, never proof of deletion.
      // Keep the last known-good cached row searchable and report the omission.
      this.unreadableSources.add(entry.path);
      return true;
    }
    this.requireActive();
    if (!this.reconciliationTupleIsCurrent(expectedGeneration, expectedEpoch)) return false;
    this.unreadableSources.delete(entry.path);
    if (this.wasTouchedAfter(entry.path, cut)) return true;
    if (read.kind === "source") {
      await this.applyReconciliationChange([read.source], []);
      return true;
    }
    if (read.kind === "oversized") {
      await this.applyReconciliationChange([oversizedInput(read)], []);
      return true;
    }
    if (read.kind === "missing") this.queueRemoval(entry.path);
    else this.queueUpsert(entry.path);
    return true;
  }

  private reconciliationTupleIsCurrent(generation: string, epoch: number): boolean {
    return this.activeGeneration === generation
      && this.mutationEpoch === epoch
      && !this.rebuildRequested
      && !this.rescanRequested;
  }

  private retrySupersededReconciliation(): void {
    if (this.disposed || this.rebuildRequested || this.rescanRequested) return;
    // An event invalidated the pass-wide evidence. A fresh authoritative snapshot
    // is safer than publishing decisions made against two different vault states.
    this.requestAuthoritativeRescan();
    this.rescanSequence = this.eventSequence;
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
    this.replaySubphase = null;
    this.activity = "inventory";
    this.completed = 0;
    this.total = null;
    this.inFlight = 0;
    this.stallCategory = null;
    this.currentPath = null;
    const generation = this.allocateFreshGeneration();
    this.candidateGeneration = generation;
    const activeOmissions = rebuilding ? this.captureSourceOmissions() : null;
    let began = false;
    this.replacementBuildInProgress = rebuilding;
    this.clearSourceOmissions();
    this.clearInitialColdPreview();
    this.initialColdPreviewGeneration = rebuilding ? null : generation;
    this.initialColdPreviewRevision = 0;
    this.initialColdPreviewProcessed = 0;
    try {
      this.emit(rebuilding ? "rebuild" : "snapshot");
      let counts = await this.worker.beginBuild(generation);
      began = true;
      this.requireActive();
      this.syncWorkerQuarantines(counts);

      const snapshot = this.captureSnapshot();
      // Publish the authoritative inventory denominator before source reads begin.
      // This is the first meaningful cold-build progress signal, including for an
      // empty vault; acknowledged completion still advances only after Worker RPCs.
      this.activity = "inventory";
      const progress: InitialBuildProgress | null = rebuilding ? null : {
        generation,
        snapshot,
        represented: snapshot.entries.map(() => false),
        acknowledgedAddBatches: 0,
        acknowledgedPrefixSources: 0,
        lastAcknowledgedPath: null,
        counts,
      };
      this.initialBuildProgress = progress;
      this.completed = 0;
      this.currentPath = null;
      this.total = snapshot.entries.length;
      this.emit(rebuilding ? "rebuild" : "snapshot");
      counts = await this.addSnapshotSources(
        generation,
        snapshot,
        counts,
        rebuilding,
        progress,
        snapshot.entries.map((entry, ordinal) => ({ entry, ordinal })),
      );

      if (this.rescanRequested) {
        this.clearInitialColdPreview();
        await this.worker.abortBuild(generation);
        this.initialBuildProgress = null;
        this.candidateGeneration = null;
        began = false;
        if (activeOmissions) this.restoreSourceOmissions(activeOmissions);
        this.replacementBuildInProgress = false;
        return;
      }

      this.clearInitialColdPreview();
      this.emit(rebuilding ? "rebuild" : "replay");
      while (this.hasPendingChanges()) {
        counts = await this.applyPendingChanges(generation, null, counts);
        this.syncWorkerQuarantines(counts);
        if (this.initialBuildProgress) this.initialBuildProgress.counts = counts;
        if (this.rescanRequested) {
          this.clearInitialColdPreview();
          await this.worker.abortBuild(generation);
          this.initialBuildProgress = null;
          this.candidateGeneration = null;
          began = false;
          if (activeOmissions) this.restoreSourceOmissions(activeOmissions);
          this.replacementBuildInProgress = false;
          return;
        }
      }

      if (rebuilding && this.unreadableSources.size > 0) {
        // A source-local read failure is not proof that the source disappeared.
        // Keep the complete active generation searchable rather than publishing
        // a replacement candidate known to omit at least one source. This is a
        // degraded freshness verdict, not a Worker failure, so it remains locally
        // recoverable through a later explicit rebuild without raising onFailure.
        const unreadableEvidence = [...this.unreadableSources];
        began = false;
        this.clearInitialColdPreview();
        await this.worker.abortBuild(generation);
        this.requireActive();
        if (activeOmissions) {
          this.restoreSourceOmissions(activeOmissions, unreadableEvidence);
        }
        this.blocked = !this.rebuildRequested && !this.rescanRequested;
        this.candidateGeneration = null;
        this.replacementBuildInProgress = false;
        this.emit("degraded", "sources_unreadable");
        return;
      }

      this.clearInitialColdPreview();
      counts = await this.worker.commitBuild(generation);
      this.requireActive();
      this.setActiveCounts(counts);
      this.candidateGeneration = null;
      this.initialBuildProgress = null;
      this.activity = "apply";
      this.completed = this.total ?? 0;
      this.inFlight = 0;
      this.stallCategory = null;
      this.replacementBuildInProgress = false;
      this.emit(this.hasPendingChanges() ? "replay" : "ready");
      if (!rebuilding) {
        await this.discardInitialBuildCheckpoint("completed", null);
        if (this.cacheIssue === "checkpoint_discard_failed") this.emit("ready");
      }
    } catch (error) {
      this.clearInitialColdPreview();
      this.invalidateReadWindowForGeneration(generation, error);
      if (error instanceof ShutdownRequestedError) throw error;
      const unreadableEvidence = rebuilding ? [...this.unreadableSources] : [];
      let failure = error;
      if (began) {
        if (!rebuilding) await this.persistInitialBuildCheckpoint("failure");
        try {
          await this.worker.abortBuild(generation);
        } catch (abortError) {
          failure = new AggregateError(
            [error, abortError],
            "index build failed and staging abort did not complete",
          );
        }
      }
      if (activeOmissions) {
        this.restoreSourceOmissions(activeOmissions, unreadableEvidence);
      }
      this.candidateGeneration = null;
      this.initialBuildProgress = null;
      this.replacementBuildInProgress = false;
      throw failure;
    }
  }

  private captureSnapshot(): Snapshot {
    const paths = this.listSourcePaths();
    const entries: SnapshotEntry[] = [];
    let unreadableInspections = 0;
    let firstUnreadableError: UnreadableVaultSourceError | null = null;
    for (const path of paths) {
      let inspection: SourceInspection;
      try {
        inspection = this.inspectSource(path);
      } catch (error) {
        if (!(error instanceof UnreadableVaultSourceError)) throw error;
        unreadableInspections += 1;
        firstUnreadableError ??= error;
        this.unreadableSources.add(path);
        continue;
      }
      if (inspection.kind === "missing") {
        // Enumeration only proved the path existed at an earlier instant. Once
        // it has vanished there is no source to quarantine, and omitting it from
        // reconciliation correctly removes any cached copy.
        this.unreadableSources.delete(path);
        continue;
      }
      this.unreadableSources.delete(path);
      entries.push({ path, inspection });
    }
    if (firstUnreadableError
      && unreadableInspections >= MIN_SYSTEMIC_UNREADABLE_SOURCES
      && unreadableInspections > paths.length * SYSTEMIC_UNREADABLE_READ_RATIO) {
      // A majority of independent per-path inspections failing is evidence that
      // the vault is unavailable, not permission to publish a mostly empty index.
      throw new VaultUnavailableError(firstUnreadableError);
    }
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
    progress: InitialBuildProgress | null,
    entries: Array<{ entry: SnapshotEntry; ordinal: number }>,
  ): Promise<IndexCounts> {
    let counts = initialCounts;
    let batch: SourceUpsert[] = [];
    let batchOrdinals: number[] = [];
    let batchBytes = 0;
    let attemptedCandidateReads = 0;
    let unreadableCandidateReads = 0;

    const flush = async (): Promise<void> => {
      if (batch.length === 0) return;
      const acknowledgedOrdinals = batchOrdinals;
      const previewWasAvailable = this.initialColdPreview !== null;
      this.clearInitialColdPreview();
      this.activity = "prepare";
      this.inFlight = batch.length;
      this.emit(rebuilding ? "rebuild" : "snapshot");
      counts = await this.worker.addSourceBatch(generation, batch);
      this.syncWorkerQuarantines(counts);
      this.completed += acknowledgedOrdinals.length;
      if (progress) {
        for (const ordinal of acknowledgedOrdinals) progress.represented[ordinal] = true;
        progress.acknowledgedAddBatches += 1;
        progress.counts = counts;
        this.advanceAcknowledgedPrefix(progress);
      }
      batch = [];
      batchOrdinals = [];
      batchBytes = 0;
      this.inFlight = 0;
      this.activity = "apply";
      if (this.stoppingForCheckpoint) throw new ShutdownRequestedError();
      if (progress
        && progress.acknowledgedAddBatches % INITIAL_BUILD_CHECKPOINT_BATCH_CADENCE === 0) {
        await this.persistInitialBuildCheckpoint("cadence");
      }
      if (!rebuilding) {
        this.initialColdPreviewRevision += 1;
        this.initialColdPreviewProcessed = progress?.acknowledgedPrefixSources ?? this.completed;
        this.offerInitialColdPreview(generation, counts);
      }
      this.emit(rebuilding ? "rebuild" : "snapshot");
    };

    let cursor = 0;
    while (cursor < entries.length) {
      let reservedBytes = 0;
      let reservedReads = 0;
      const availableReadSlots = this.limits.maxConcurrentReads
        - this.outstandingSourceReads.size;
      // Keep one candidate in the window when capacity is exhausted so
      // beginSourceReadWindow reports the bounded capacity stall instead of
      // spinning an empty cursor. Otherwise use every currently available slot,
      // but never exceed the established read-concurrency ceiling.
      const candidateReadLimit = Math.max(1, availableReadSlots);
      const window: Array<{ entry: SnapshotEntry; ordinal: number }> = [];
      while (cursor < entries.length && window.length < this.limits.maxConcurrentReads) {
        const indexed = entries[cursor]!;
        const inspection = indexed.entry.inspection;
        if (inspection.kind === "candidate") {
          if (reservedReads >= candidateReadLimit) break;
          if (inspection.size > this.limits.maxBatchBytes) {
            throw new Error("source exceeds the configured batch byte limit");
          }
          if (batchBytes + reservedBytes + inspection.size > this.limits.maxBatchBytes) {
            if (window.length > 0) break;
            await flush();
            continue;
          }
          reservedBytes += inspection.size;
          reservedReads += 1;
          attemptedCandidateReads += 1;
        }
        window.push(indexed);
        cursor += 1;
      }

      const candidateReads = window.filter(({ entry }) => entry.inspection.kind === "candidate").length;
      const lease = candidateReads > 0
        ? this.beginSourceReadWindow(generation, candidateReads)
        : null;
      this.activity = candidateReads > 0 ? "read" : "prepare";
      this.inFlight = candidateReads;
      this.emit(rebuilding ? "rebuild" : "snapshot");
      let settled: PromiseSettledResult<StableSourceRead>[];
      try {
        settled = await Promise.allSettled(window.map(({ entry }) => (
          entry.inspection.kind === "candidate"
            ? this.readSnapshot(entry.inspection, lease!)
            : Promise.resolve(entry.inspection)
        )));
        if (lease) this.assertReadWindowCurrent(lease);
      } finally {
        if (lease) this.finishSourceReadWindow(lease);
      }
      this.requireActive();
      this.inFlight = 0;
      this.activity = "prepare";
      let firstUnreadableError: UnreadableVaultSourceError | null = null;
      for (let index = 0; index < window.length; index += 1) {
        const result = settled[index]!;
        const { entry, ordinal } = window[index]!;
        this.currentPath = entry.path;
        if (result.status === "rejected") {
          if (!(result.reason instanceof UnreadableVaultSourceError)) throw result.reason;
          firstUnreadableError ??= result.reason;
          unreadableCandidateReads += 1;
          this.unreadableSources.add(entry.path);
          this.emit(rebuilding ? "rebuild" : "snapshot");
          continue;
        }
        const read = result.value;
        this.unreadableSources.delete(entry.path);
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
            batchOrdinals.push(ordinal);
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
      if (firstUnreadableError
        && unreadableCandidateReads >= MIN_SYSTEMIC_UNREADABLE_SOURCES
        && unreadableCandidateReads
          > attemptedCandidateReads * SYSTEMIC_UNREADABLE_READ_RATIO) {
        throw new VaultUnavailableError(firstUnreadableError);
      }
      await this.yieldControl();
      this.requireActive();
    }

    await flush();
    return counts;
  }

  private advanceAcknowledgedPrefix(progress: InitialBuildProgress): void {
    while (progress.acknowledgedPrefixSources < progress.represented.length
      && progress.represented[progress.acknowledgedPrefixSources]) {
      progress.acknowledgedPrefixSources += 1;
    }
    progress.lastAcknowledgedPath = progress.acknowledgedPrefixSources > 0
      ? progress.snapshot.entries[progress.acknowledgedPrefixSources - 1]!.path
      : null;
  }

  private async persistInitialBuildCheckpoint(
    reason: "cadence" | "shutdown" | "failure",
  ): Promise<void> {
    if (this.checkpointRunning) {
      const running = this.checkpointRunning;
      if (reason === "failure") {
        await raceWithDeadline(running, FAILURE_CHECKPOINT_DEADLINE_MS, () => {
          if (this.checkpointRunning === running) this.checkpointRunning = null;
        });
      } else {
        await running;
      }
      return;
    }
    const store = isCheckpointStore(this.cacheStore) ? this.cacheStore : null;
    const worker = isCheckpointIndexWorker(this.worker) ? this.worker : null;
    const progress = this.initialBuildProgress;
    if (!store
      || !worker
      || !progress
      || progress.acknowledgedPrefixSources === 0
      || this.activeGeneration !== null
      || this.replacementBuildInProgress
      || this.disposed) return;

    const cursor: InitialBuildCheckpointCursor = {
      snapshot_source_count: progress.snapshot.entries.length,
      acknowledged_add_batches: progress.acknowledgedAddBatches,
      acknowledged_prefix_sources: progress.acknowledgedPrefixSources,
      last_acknowledged_path: progress.lastAcknowledgedPath,
    };
    let attemptActive = true;
    const persistence = (async () => {
      try {
        const exported = await worker.exportInitialBuildCheckpoint(
          progress.generation,
          store.vaultCacheIdentity,
          cursor,
        );
        if (!attemptActive
          || this.disposed
          || this.activeGeneration !== null
          || this.initialBuildProgress !== progress
          || exported.generation !== progress.generation
          || exported.publication !== "initial_staging"
          || exported.searchable
          || !sameCheckpointCursor(exported.cursor, cursor)) return;
        const persisted = await store.putInitialBuildCheckpoint({
          recordKind: exported.record_kind,
          recordVersion: exported.checkpoint_record_version,
          imageVersion: exported.checkpoint_image_version,
          orderingVersion: INITIAL_BUILD_CHECKPOINT_ORDERING_VERSION,
          generationId: exported.generation,
          byteLength: exported.blob_byte_length,
          sha256: exported.blob_sha256,
          bytes: exported.bytes,
          cursor,
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
            source_policy_hash: exported.source_policy_hash,
          },
        });
        if (!attemptActive) return;
        if (persisted.generationId !== exported.generation
          || persisted.sha256 !== exported.blob_sha256) {
          throw new Error("checkpoint store returned a different record token");
        }
        if (!this.disposed && this.initialBuildProgress === progress) {
          this.initialBuildCheckpointToken = {
            generationId: persisted.generationId,
            sha256: persisted.sha256,
          };
          if (this.cacheIssue === "checkpoint_save_failed") this.cacheIssue = null;
          if (!this.stoppingForCheckpoint) this.emit(this.stage);
        }
      } catch {
        if (attemptActive && !this.disposed && this.initialBuildProgress === progress) {
          this.cacheIssue = "checkpoint_save_failed";
          if (!this.stoppingForCheckpoint) this.emit(this.stage);
        }
      }
    })();
    // A failure checkpoint is a bounded, best-effort durability opportunity.
    // Once abandoned, its uncancellable export/store continuation is detached and
    // may no longer publish a token, diagnostic, or disposal dependency.
    const task = (reason === "failure"
      ? raceWithDeadline(persistence, FAILURE_CHECKPOINT_DEADLINE_MS, () => {
        attemptActive = false;
      })
      : persistence
    ).finally(() => {
      if (this.checkpointRunning === task) this.checkpointRunning = null;
    });
    this.checkpointRunning = task;
    await task;
  }

  private async flushActiveChanges(): Promise<void> {
    const generation = this.activeGeneration;
    if (generation === null) return;
    this.currentPath = null;
    const counts = await this.applyPendingChanges(generation, this.allocateFreshGeneration(), {
      generation,
      documents: this.documents,
      chunks: this.chunks,
      database_bytes: this.databaseBytes,
      database_byte_limit: this.databaseByteLimit,
      quarantined_sources: this.quarantinedSources,
      quarantine_fields: [...this.quarantineValidatorFields],
      source_format_counts: cloneSourceFormatCounts(this.sourceFormatCounts),
    });
    this.requireActive();
    const applied = counts.generation !== generation;
    this.setActiveCounts(counts);
    if (this.startupReconciling && this.replaySubphase === "applying" && applied) {
      this.completed += 1;
    }
    if (this.unreadableSources.size > 0 && !this.hasPendingChanges()) {
      this.emit("degraded", "sources_unreadable");
      return;
    }
    if (this.startupReconciling
      && this.replaySubphase === "applying"
      && !this.hasPendingChanges()
      && this.total !== null
      && this.completed >= this.total) return;
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
        this.unreadableSources.delete(path);
      }
      for (const path of changes.upserts) {
        let read: StableSourceRead;
        try {
          read = await this.readStable(path, generation);
        } catch (error) {
          if (!(error instanceof UnreadableVaultSourceError)) throw error;
          this.unreadableSources.add(path);
          if (changes.rename && nextGeneration !== null) {
            // A rename must remain an atomic removal/reinsert. If the destination
            // cannot be read, keep the old path searchable and require a fresh
            // authoritative pass instead of publishing a generation with neither.
            this.requestAuthoritativeRescan();
            removals.clear();
          }
          continue;
        }
        this.requireActive();
        this.unreadableSources.delete(path);
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
        let estimated = 0;
        try {
          const inspection = this.inspectSource(path);
          estimated = inspection.kind === "candidate" ? inspection.size : 0;
        } catch (error) {
          if (!(error instanceof UnreadableVaultSourceError)) throw error;
          // This preflight only sizes one pending source. readStable owns its
          // retries and quarantine, so a local inspect failure cannot abort here.
        }
        if (paths.length > 0 && bytes + estimated > this.limits.maxBatchBytes) continue;
        bytes += estimated;
      }
      paths.push(path);
      if (this.pendingUpserts.delete(path)) upserts.push(path);
      if (this.pendingRemovals.delete(path)) removals.push(path);
    }
    return { paths, upserts, removals };
  }

  private inspectSource(path: string): SourceInspection {
    try {
      return this.source.inspectSource(path);
    } catch (error) {
      // Inspection names exactly one source, so failure proves nothing about the
      // rest of the vault and belongs on the per-source omission path.
      throw new UnreadableVaultSourceError(error);
    }
  }

  private listSourcePaths(): string[] {
    try {
      return [...this.source.listSourcePaths()].sort(comparePaths);
    } catch (error) {
      // Without the authoritative inventory there is no trustworthy source set
      // or denominator from which a partial generation could be published.
      throw new VaultUnavailableError(error);
    }
  }

  private async readSnapshot(
    inspection: SourceInspection,
    sharedLease?: SourceReadWindowLease,
  ): Promise<StableSourceRead> {
    if (inspection.kind !== "candidate") return inspection;
    const lease = sharedLease ?? this.beginSourceReadWindow(
      this.candidateGeneration ?? this.activeGeneration ?? "initial-staging",
      1,
    );
    try {
      let lastError: unknown = new Error("active-vault source could not be read");
      for (let attempt = 0; attempt < this.limits.maxStableReadAttempts; attempt += 1) {
        this.assertReadWindowCurrent(lease);
        try {
          const read = await this.readSourceWithinWindow(inspection, lease);
          this.assertReadWindowCurrent(lease);
          return read;
        } catch (error) {
          if (error instanceof SourceReadWindowError || error instanceof ShutdownRequestedError) {
            throw error;
          }
          if (this.disposed) throw error;
          lastError = error;
          this.assertReadWindowCurrent(lease);
        }
      }
      throw new UnreadableVaultSourceError(lastError);
    } finally {
      if (!sharedLease) this.finishSourceReadWindow(lease);
    }
  }

  private async readStable(path: string, generation: string): Promise<StableSourceRead> {
    const lease = this.beginSourceReadWindow(generation, 1);
    try {
      let readError: unknown = null;
      for (let attempt = 0; attempt < this.limits.maxStableReadAttempts; attempt += 1) {
        this.assertReadWindowCurrent(lease);
        try {
          const inspection = this.inspectSource(path);
          if (inspection.kind !== "candidate") return inspection;
          const read = await this.readSourceWithinWindow(inspection, lease);
          this.assertReadWindowCurrent(lease);
          if (read.kind !== "stale") return read;
        } catch (error) {
          if (error instanceof SourceReadWindowError || error instanceof ShutdownRequestedError) {
            throw error;
          }
          if (this.disposed) throw error;
          readError = error;
          this.assertReadWindowCurrent(lease);
        }
      }
      // Every attempt concerned this one path. Continuous churn or share-level
      // metadata lag therefore omits that source; it does not prove a dead vault.
      throw new UnreadableVaultSourceError(
        readError ?? new Error("vault source did not become stable"),
      );
    } finally {
      this.finishSourceReadWindow(lease);
    }
  }

  private beginSourceReadWindow(
    generation: string,
    requestedReads: number,
  ): SourceReadWindowLease {
    this.requireActive();
    if (this.currentReadWindow !== null) {
      throw new Error("source read windows must not overlap");
    }
    if (this.outstandingSourceReads.size + requestedReads > this.limits.maxConcurrentReads) {
      const error = new SourceReadWindowError("source_read_capacity");
      this.stallCategory = error.stallCategory;
      this.inFlight = 0;
      throw error;
    }

    let rejectDeadline!: (error: Error) => void;
    const deadline = new Promise<never>((_resolve, reject) => {
      rejectDeadline = reject;
    });
    void deadline.catch(() => undefined);
    const lease = {
      generation,
      epoch: ++this.readEpoch,
      active: true,
      failure: null,
      deadline,
      rejectDeadline,
      timer: undefined as unknown as ReturnType<typeof setTimeout>,
    } satisfies SourceReadWindowLease;
    lease.timer = setTimeout(() => {
      if (!lease.active || this.currentReadWindow !== lease) return;
      const error = new SourceReadWindowError("source_read_timeout");
      this.stallCategory = error.stallCategory;
      this.inFlight = 0;
      this.invalidateReadWindow(lease, error);
    }, this.sourceReadTimeoutMs);
    this.currentReadWindow = lease;
    return lease;
  }

  private readSourceWithinWindow(
    inspection: Extract<SourceInspection, { kind: "candidate" }>,
    lease: SourceReadWindowLease,
  ): Promise<StableSourceRead> {
    this.assertReadWindowCurrent(lease);
    if (this.outstandingSourceReads.size >= this.limits.maxConcurrentReads) {
      const error = new SourceReadWindowError("source_read_capacity");
      this.stallCategory = error.stallCategory;
      this.invalidateReadWindow(lease, error);
      return Promise.reject(error);
    }

    let underlying: Promise<SourceReadOutcome>;
    try {
      underlying = Promise.resolve(this.source.readSource(inspection));
    } catch (error) {
      underlying = Promise.reject(error);
    }
    this.outstandingSourceReads.add(underlying);
    void underlying.then(
      (outcome) => {
        if (outcome.kind !== "timeout") {
          this.outstandingSourceReads.delete(underlying);
          return;
        }
        void outcome.underlyingSettled.then(
          () => this.outstandingSourceReads.delete(underlying),
          () => this.outstandingSourceReads.delete(underlying),
        );
      },
      () => this.outstandingSourceReads.delete(underlying),
    );
    return Promise.race([underlying, lease.deadline]).then((read) => {
      this.assertReadWindowCurrent(lease);
      if (read.kind === "timeout") {
        const error = new SourceReadWindowError("source_read_timeout");
        this.stallCategory = error.stallCategory;
        this.invalidateReadWindow(lease, error);
        throw error;
      }
      return read;
    });
  }

  private assertReadWindowCurrent(lease: SourceReadWindowLease): void {
    if (!lease.active || this.currentReadWindow !== lease) {
      throw lease.failure ?? new SourceReadWindowError("source_read_timeout");
    }
    this.requireActive();
  }

  private finishSourceReadWindow(lease: SourceReadWindowLease): void {
    if (!lease.active || this.currentReadWindow !== lease) return;
    clearTimeout(lease.timer);
    lease.active = false;
    this.currentReadWindow = null;
    this.inFlight = 0;
  }

  private invalidateReadWindow(lease: SourceReadWindowLease, error: unknown): void {
    if (!lease.active) return;
    clearTimeout(lease.timer);
    lease.active = false;
    lease.failure = error instanceof Error ? error : new Error("source read window invalidated");
    if (this.currentReadWindow === lease) this.currentReadWindow = null;
    lease.rejectDeadline(lease.failure);
  }

  private invalidateCurrentReadWindow(error: unknown): void {
    if (this.currentReadWindow) this.invalidateReadWindow(this.currentReadWindow, error);
    this.inFlight = 0;
  }

  private invalidateReadWindowForGeneration(generation: string, error: unknown): void {
    if (this.currentReadWindow?.generation === generation) {
      this.invalidateReadWindow(this.currentReadWindow, error);
    }
    this.inFlight = 0;
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

  private captureSourceOmissions(): SourceOmissions {
    return {
      sourceFormatCounts: cloneSourceFormatCounts(this.sourceFormatCounts),
      quarantinedSources: this.quarantinedSources,
      unreadableSources: [...this.unreadableSources],
      quarantineValidatorFields: [...this.quarantineValidatorFields],
    };
  }

  private restoreSourceOmissions(
    omissions: SourceOmissions,
    unreadableEvidence: readonly string[] = [],
  ): void {
    this.sourceFormatCounts = cloneSourceFormatCounts(omissions.sourceFormatCounts);
    this.quarantinedSources = omissions.quarantinedSources;
    this.quarantineValidatorFields.clear();
    for (const field of omissions.quarantineValidatorFields) {
      this.quarantineValidatorFields.add(field);
    }
    this.unreadableSources.clear();
    for (const path of omissions.unreadableSources) this.unreadableSources.add(path);
    for (const path of unreadableEvidence) this.unreadableSources.add(path);
    this.syncActiveOmissionsFromCurrent();
  }

  private clearSourceOmissions(): void {
    this.sourceFormatCounts = emptySourceFormatCounts();
    this.quarantinedSources = 0;
    this.quarantineValidatorFields.clear();
    this.unreadableSources.clear();
  }

  private syncWorkerQuarantines(counts: IndexCounts): void {
    this.sourceFormatCounts = cloneSourceFormatCounts(counts.source_format_counts);
    this.quarantinedSources = counts.quarantined_sources;
    this.quarantineValidatorFields.clear();
    for (const field of counts.quarantine_fields) this.quarantineValidatorFields.add(field);
  }

  private offerInitialColdPreview(generation: string, counts: IndexCounts): void {
    if (!this.initialColdPreviewEnabled
      || this.activeGeneration !== null
      || this.initialColdPreviewGeneration !== generation
      || this.initialColdPreviewRevision < 1
      || counts.documents < 1
      || counts.chunks < 1
      || this.total === null
      || this.hasPendingChanges()
      || this.rebuildRequested
      || this.rescanRequested
      || this.disposed) {
      this.initialColdPreview = null;
      return;
    }
    this.initialColdPreview = Object.freeze({
      generation,
      revision: this.initialColdPreviewRevision,
      processed: this.initialColdPreviewProcessed,
      total: this.total,
      documents: counts.documents,
      chunks: counts.chunks,
      quarantinedSources: this.quarantinedSources,
      unreadableSources: this.unreadableSources.size,
    });
  }

  private clearInitialColdPreview(): void {
    this.initialColdPreview = null;
  }

  private syncActiveOmissionsFromCurrent(): void {
    this.activeSourceFormatCounts = cloneSourceFormatCounts(this.sourceFormatCounts);
    this.activeQuarantinedSources = this.quarantinedSources;
    this.activeUnreadableSources = this.unreadableSources.size;
    this.activeQuarantineValidatorFields.clear();
    for (const field of this.quarantineValidatorFields) {
      this.activeQuarantineValidatorFields.add(field);
    }
  }

  private setActiveCounts(counts: IndexCounts): void {
    const changed = this.activeGeneration !== counts.generation;
    this.activeGeneration = counts.generation;
    this.issuedGenerationIds.add(counts.generation);
    this.documents = counts.documents;
    this.chunks = counts.chunks;
    this.databaseBytes = counts.database_bytes;
    this.databaseByteLimit = counts.database_byte_limit;
    this.syncWorkerQuarantines(counts);
    this.syncActiveOmissionsFromCurrent();
    if (changed) {
      this.mutationEpoch += 1;
      this.cancelExportTimer();
    }
  }

  private handleFailure(error: unknown): void {
    if (this.disposed) return;
    this.blocked = true;
    this.cancelExportTimer();
    this.inFlight = 0;
    const sourceReadWindowError = findSourceReadWindowError(error);
    if (sourceReadWindowError) {
      this.activity = "read";
      this.stallCategory = sourceReadWindowError.stallCategory;
    } else if (containsWorkerTimeout(error)) {
      this.activity = "apply";
      this.stallCategory = "worker_timeout";
    }
    const hasActive = this.activeGeneration !== null;
    const issue = containsIndexLimitError(error)
      ? "index_limit_exceeded"
      : containsVaultUnavailableError(error)
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
    const progress = stage === "snapshot"
      || stage === "replay"
      || stage === "rebuild"
      || this.stallCategory !== null
      ? {
        activity: stage === "replay" && this.replaySubphase !== null
          ? replayActivity(this.replaySubphase)
          : this.activity,
        completed: this.completed,
        total: this.total,
        inFlight: this.inFlight,
        ...(stage === "replay" && this.replaySubphase !== null
          ? { subphase: this.replaySubphase }
          : {}),
        ...(this.stallCategory === null ? {} : { stallCategory: this.stallCategory }),
      }
      : undefined;
    const hasActive = this.activeGeneration !== null;
    const servingPriorDuringReplacement = hasActive && this.replacementBuildInProgress;
    const visibleSourceFormatCounts = servingPriorDuringReplacement
      ? this.activeSourceFormatCounts
      : this.sourceFormatCounts;
    const visibleQuarantinedSources = servingPriorDuringReplacement
      ? this.activeQuarantinedSources
      : this.quarantinedSources;
    const visibleUnreadableSources = servingPriorDuringReplacement
      ? this.activeUnreadableSources
      : this.unreadableSources.size;
    const visibleQuarantineFields = servingPriorDuringReplacement
      ? this.activeQuarantineValidatorFields
      : this.quarantineValidatorFields;
    const omissionIssue = visibleQuarantinedSources > 0
      ? "sources_quarantined"
      : visibleUnreadableSources > 0
        ? "sources_unreadable"
        : undefined;
    const issue = explicitIssue
      ?? (this.startupReconciling
        ? "index_reconciling"
        : this.cacheIssue ?? omissionIssue);
    const status: IndexControllerStatus = {
      stage,
      searchable: this.activeGeneration !== null && stage !== "disposed",
      generation: this.activeGeneration,
      ...(this.initialColdPreview === null
        ? {}
        : { initialColdPreview: this.initialColdPreview }),
      documents: this.documents,
      chunks: this.chunks,
      sourceFormatCounts: cloneSourceFormatCounts(visibleSourceFormatCounts),
      quarantinedSources: visibleQuarantinedSources,
      unreadableSources: visibleUnreadableSources,
      quarantineValidatorFields: [...visibleQuarantineFields].sort(),
      dirty,
      rebuilding: stage === "rebuild",
      mutationEpoch: this.mutationEpoch,
      ...(progress ? { progress } : {}),
      ...(issue ? { issue } : {}),
    };
    this.onStatus(status);
    this.observePublishedStartupStatus(status);
    this.updateExportSchedule(status);
  }

  private observePublishedStartupStatus(status: IndexControllerStatus): void {
    if (this.startupObservationFinished) return;
    if (isCleanStatus(status)) {
      this.startupObservationFinished = true;
      this.observeStartup({ kind: "fully_current" });
      return;
    }
    if (status.stage === "disposed") {
      this.startupObservationFinished = true;
      this.observeStartup({
        kind: "terminal",
        outcome: "cancelled",
        reason: "backend_unavailable",
      });
      return;
    }
    const hasOmissions = status.quarantinedSources > 0 || status.unreadableSources > 0;
    if (status.stage !== "failed" && status.stage !== "degraded"
      && !(status.stage === "ready" && hasOmissions)) return;
    this.startupObservationFinished = true;
    const reason = status.issue === "index_limit_exceeded"
      ? "index_capacity"
      : status.issue === "vault_read_failed"
        ? "vault_unavailable"
        : hasOmissions || status.issue === "sources_quarantined" || status.issue === "sources_unreadable"
          ? "sources_omitted"
          : "backend_unavailable";
    this.observeStartup({
      kind: "terminal",
      outcome: status.stage === "failed" ? "failed" : "degraded",
      reason,
    });
  }

  private observeStartup(observation: IndexControllerStartupObservation): void {
    try {
      this.onStartupObservation(observation);
    } catch {
      // Instrumentation cannot interrupt indexing, publication, or disposal.
    }
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
          source_policy_hash: exported.source_policy_hash,
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
    if (this.stoppingForCheckpoint) throw new ShutdownRequestedError();
  }
}

class ShutdownRequestedError extends Error {
  constructor() {
    super("in-plugin index controller is stopping for checkpoint");
    this.name = "ShutdownRequestedError";
  }
}

class CheckpointResumeFallbackError extends Error {
  constructor() {
    super("checkpoint prefix could not be proven current");
    this.name = "CheckpointResumeFallbackError";
  }
}

class VaultUnavailableError extends Error {
  constructor(cause: unknown) {
    super("active vault source could not be read", { cause });
    // Preserve the established diagnostics identifier while keeping systemic
    // failure unrelated to the source-local error that callers may quarantine.
    this.name = "VaultSourceReadError";
  }
}

class SourceReadWindowError extends VaultUnavailableError {
  constructor(readonly stallCategory: Extract<
    IndexControllerStallCategory,
    "source_read_timeout" | "source_read_capacity"
  >) {
    super(new Error(stallCategory));
  }
}

class UnreadableVaultSourceError extends Error {
  constructor(cause: unknown) {
    super("one active vault source could not be read", { cause });
    this.name = "UnreadableVaultSourceError";
  }
}

function isCacheIndexWorker(worker: IndexWorkerPort): worker is CacheIndexWorkerPort {
  const candidate = worker as Partial<CacheIndexWorkerPort>;
  return typeof candidate.restoreGeneration === "function"
    && typeof candidate.planReconciliation === "function"
    && typeof candidate.exportGeneration === "function";
}

function isCheckpointIndexWorker(worker: IndexWorkerPort): worker is CheckpointIndexWorkerPort {
  if (!isCacheIndexWorker(worker)) return false;
  const candidate = worker as Partial<CheckpointIndexWorkerPort>;
  return typeof candidate.exportInitialBuildCheckpoint === "function"
    && typeof candidate.restoreInitialBuildCheckpoint === "function"
    && typeof candidate.planInitialBuildCheckpointReconciliation === "function";
}

function isCheckpointStore(store: CacheStorePort | null): store is CacheStoreBundlePort {
  if (!store) return false;
  const candidate = store as Partial<CacheStoreBundlePort>;
  return typeof candidate.loadInitialBuildCheckpoint === "function"
    && typeof candidate.putInitialBuildCheckpoint === "function"
    && typeof candidate.discardInitialBuildCheckpoint === "function";
}

function sameCheckpointToken(
  left: InitialBuildCheckpointToken | null,
  right: InitialBuildCheckpointToken,
): boolean {
  return left?.generationId === right.generationId && left.sha256 === right.sha256;
}

function sameCheckpointCursor(
  left: InitialBuildCheckpointCursor,
  right: InitialBuildCheckpointCursor,
): boolean {
  return left.snapshot_source_count === right.snapshot_source_count
    && left.acknowledged_add_batches === right.acknowledged_add_batches
    && left.acknowledged_prefix_sources === right.acknowledged_prefix_sources
    && left.last_acknowledged_path === right.last_acknowledged_path;
}

async function raceWithDeadline(
  task: Promise<void>,
  deadlineMs: number,
  onDeadline?: () => void,
): Promise<void> {
  let timer: ReturnType<typeof setTimeout> | null = null;
  try {
    await Promise.race([
      task,
      new Promise<void>((resolve) => {
        timer = setTimeout(() => {
          onDeadline?.();
          resolve();
        }, deadlineMs);
      }),
    ]);
  } finally {
    if (timer !== null) clearTimeout(timer);
  }
}

function inspectionMetadata(
  inspection: PresentSourceInspection,
): ReconciliationSourceMetadata {
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
      format: inspection.format,
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
    && status.quarantinedSources === 0
    && status.unreadableSources === 0
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

function containsVaultUnavailableError(error: unknown): boolean {
  return error instanceof VaultUnavailableError
    || (error instanceof AggregateError
      && error.errors.some((nested) => containsVaultUnavailableError(nested)));
}

function findSourceReadWindowError(error: unknown): SourceReadWindowError | null {
  if (error instanceof SourceReadWindowError) return error;
  if (error instanceof AggregateError) {
    for (const nested of error.errors) {
      const found = findSourceReadWindowError(nested);
      if (found) return found;
    }
  }
  return null;
}

function containsWorkerTimeout(error: unknown): boolean {
  if (error instanceof AggregateError) {
    return error.errors.some((nested) => containsWorkerTimeout(nested));
  }
  return errorCode(error) === "timeout";
}

function isSystemicUnreadability(unreadable: number, attempted: number): boolean {
  return unreadable >= MIN_SYSTEMIC_UNREADABLE_SOURCES
    && unreadable > attempted * SYSTEMIC_UNREADABLE_READ_RATIO;
}

function replayActivity(subphase: IndexControllerReplaySubphase): IndexControllerActivity {
  switch (subphase) {
    case "planning": return "inventory";
    case "verifying": return "read";
    case "applying": return "apply";
  }
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
  const audit = new Set(plan.audit.map((entry) => entry.path));
  const refresh = new Set(plan.refresh);
  const remove = new Set(plan.remove);
  const uniqueCount = unchanged.size + audit.size + refresh.size + remove.size;
  const declaredCount = plan.unchanged.length + plan.audit.length
    + plan.refresh.length + plan.remove.length;
  if (current.size !== currentPaths.length
    || uniqueCount !== declaredCount
    || plan.unchanged.some((path) => audit.has(path) || refresh.has(path) || remove.has(path))
    || plan.audit.some((entry) => refresh.has(entry.path) || remove.has(entry.path))
    || plan.refresh.some((path) => remove.has(path))
    || plan.remove.some((path) => current.has(path))
    || plan.unchanged.some((path) => !current.has(path))
    || plan.audit.some((entry) => !current.has(entry.path))
    || plan.refresh.some((path) => !current.has(path))
    || plan.unchanged.length + plan.audit.length + plan.refresh.length !== current.size
    || [...current].some((path) => !unchanged.has(path) && !audit.has(path) && !refresh.has(path))
    || !Number.isSafeInteger(plan.stored_source_count)
    || !Number.isSafeInteger(plan.matched_source_count)
    || plan.stored_source_count < 0
    || plan.matched_source_count < plan.unchanged.length + plan.audit.length
    || plan.matched_source_count > current.size
    || plan.matched_source_count + plan.remove.length !== plan.stored_source_count) {
    throw new Error("reconciliation plan did not prove complete ledger coverage");
  }
}

function cloneSourceFormatCounts(counts: SourceFormatCounts): SourceFormatCounts {
  const clone = emptySourceFormatCounts();
  for (const format of SOURCE_FORMATS) {
    for (const coverage of EXTRACTION_COVERAGES) {
      clone[format][coverage] = counts[format][coverage];
    }
  }
  return clone;
}

async function sha256Hex(bytes: Uint8Array): Promise<string> {
  const digest = await crypto.subtle.digest("SHA-256", bytes as unknown as BufferSource);
  return [...new Uint8Array(digest)]
    .map((byte) => byte.toString(16).padStart(2, "0"))
    .join("");
}

function comparePaths(left: string, right: string): number {
  return left < right ? -1 : left > right ? 1 : 0;
}
