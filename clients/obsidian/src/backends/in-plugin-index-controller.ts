// SPDX-FileCopyrightText: 2026 cybersader
// SPDX-License-Identifier: GPL-3.0-only

import {
  ACTIVE_VAULT_ID,
  type ActiveVaultSource,
  type SourceInspection,
  type StableSourceRead,
  type VaultSourceEvent,
} from "../active-vault-source";
import type { BuildResult, SourceInput, SourceRemoval } from "../worker/protocol";

export type IndexCounts = BuildResult;
export type { SourceRemoval } from "../worker/protocol";

export interface IndexWorkerPort {
  initialize(): Promise<unknown>;
  beginBuild(generation: string): Promise<IndexCounts>;
  addSourceBatch(generation: string, sources: SourceInput[]): Promise<IndexCounts>;
  applySourceChanges(
    generation: string,
    nextGeneration: string | null,
    upserts: SourceInput[],
    removals: SourceRemoval[],
  ): Promise<IndexCounts>;
  commitBuild(generation: string): Promise<IndexCounts>;
  abortBuild(generation: string): Promise<IndexCounts>;
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
  issue?:
    | "vault_read_failed"
    | "index_build_failed"
    | "index_update_failed"
    | "index_limit_exceeded";
}

export interface IndexControllerLimits {
  maxConcurrentReads: number;
  maxBatchSources: number;
  maxBatchBytes: number;
  maxPendingPaths: number;
  maxStableReadAttempts: number;
}

export interface InPluginIndexControllerOptions {
  source: ActiveVaultSource;
  worker: IndexWorkerPort;
  nextGeneration: () => string;
  onStatus: (status: IndexControllerStatus) => void;
  onFailure?: (error: unknown) => void;
  yieldControl?: () => Promise<void>;
  limits?: Partial<IndexControllerLimits>;
}

const DEFAULT_LIMITS: IndexControllerLimits = {
  maxConcurrentReads: 4,
  maxBatchSources: 16,
  maxBatchBytes: 16 * 1024 * 1024,
  maxPendingPaths: 2_048,
  maxStableReadAttempts: 3,
};

export class InPluginIndexController {
  private readonly source: ActiveVaultSource;
  private readonly worker: IndexWorkerPort;
  private readonly nextGeneration: () => string;
  private readonly onStatus: (status: IndexControllerStatus) => void;
  private readonly onFailure: (error: unknown) => void;
  private readonly yieldControl: () => Promise<void>;
  private readonly limits: IndexControllerLimits;
  private readonly pendingUpserts = new Set<string>();
  private readonly pendingRemovals = new Set<string>();
  private readonly pendingRenames = new Map<string, string>();
  private unsubscribe: (() => void) | null = null;
  private running: Promise<void> | null = null;
  private started = false;
  private disposed = false;
  private blocked = false;
  private workerInitialized = false;
  private rebuildRequested = false;
  private rescanRequested = false;
  private activeGeneration: string | null = null;
  private documents = 0;
  private chunks = 0;
  private stage: IndexControllerStage = "starting";
  private completed = 0;
  private total: number | null = null;

  constructor(options: InPluginIndexControllerOptions) {
    this.source = options.source;
    this.worker = options.worker;
    this.nextGeneration = options.nextGeneration;
    this.onStatus = options.onStatus;
    this.onFailure = options.onFailure ?? (() => undefined);
    this.yieldControl = options.yieldControl ?? (() => Promise.resolve());
    this.limits = { ...DEFAULT_LIMITS, ...options.limits };
    validateLimits(this.limits);
  }

  start(): void {
    if (this.started) return;
    if (this.disposed) throw new Error("in-plugin index controller is disposed");
    this.started = true;
    this.unsubscribe = this.source.subscribe((event) => this.handleEvent(event));
    this.emit("starting");
    this.scheduleWork();
  }

  requestRebuild(): void {
    if (this.disposed) return;
    this.blocked = false;
    this.rebuildRequested = true;
    this.completed = 0;
    this.total = null;
    this.emit(this.activeGeneration === null ? "starting" : "rebuild");
    this.scheduleWork();
  }

  async whenIdle(): Promise<void> {
    while (this.running) await this.running;
  }

  dispose(): void {
    if (this.disposed) return;
    this.disposed = true;
    this.blocked = true;
    this.unsubscribe?.();
    this.unsubscribe = null;
    this.pendingUpserts.clear();
    this.pendingRemovals.clear();
    this.pendingRenames.clear();
    this.rebuildRequested = false;
    this.rescanRequested = false;
    this.emit("disposed");
  }

  private handleEvent(event: VaultSourceEvent): void {
    if (this.disposed) return;
    switch (event.kind) {
      case "upsert":
        this.queueUpsert(event.path);
        break;
      case "remove":
        this.queueRemoval(event.path);
        break;
      case "rename":
        this.queueRename(event.oldPath, event.path);
        break;
      case "rescan":
        this.rescanRequested = true;
        this.rebuildRequested = true;
        this.pendingUpserts.clear();
        this.pendingRemovals.clear();
        this.pendingRenames.clear();
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
    this.rebuildRequested = true;
  }

  private enforcePendingBound(): void {
    const paths = new Set([...this.pendingUpserts, ...this.pendingRemovals]);
    for (const [oldPath, path] of this.pendingRenames) {
      paths.add(oldPath);
      paths.add(path);
    }
    if (paths.size <= this.limits.maxPendingPaths) return;
    this.pendingUpserts.clear();
    this.pendingRemovals.clear();
    this.pendingRenames.clear();
    this.rescanRequested = true;
    this.rebuildRequested = true;
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

    while (this.hasWork()) {
      this.requireActive();
      if (this.activeGeneration === null || this.rebuildRequested || this.rescanRequested) {
        const rebuilding = this.activeGeneration !== null;
        this.rebuildRequested = false;
        this.rescanRequested = false;
        await this.buildGeneration(rebuilding);
      } else if (this.pendingUpserts.size > 0 || this.pendingRemovals.size > 0 || this.pendingRenames.size > 0) {
        await this.flushActiveChanges();
      }
    }
  }

  private hasWork(): boolean {
    return !this.workerInitialized
      || this.activeGeneration === null
      || this.rebuildRequested
      || this.rescanRequested
      || this.pendingUpserts.size > 0
      || this.pendingRemovals.size > 0
      || this.pendingRenames.size > 0;
  }

  private async buildGeneration(rebuilding: boolean): Promise<void> {
    const generation = this.nextGeneration();
    let began = false;
    try {
      this.emit(rebuilding ? "rebuild" : "snapshot");
      let counts = await this.worker.beginBuild(generation);
      began = true;
      this.requireActive();

      const paths = this.listMarkdownPaths();
      this.completed = 0;
      this.total = paths.length;
      this.emit(rebuilding ? "rebuild" : "snapshot");
      counts = await this.addSnapshotSources(generation, paths, counts, rebuilding);

      if (this.rescanRequested) {
        await this.worker.abortBuild(generation);
        return;
      }

      this.emit(rebuilding ? "rebuild" : "replay");
      while (this.pendingUpserts.size > 0 || this.pendingRemovals.size > 0 || this.pendingRenames.size > 0) {
        counts = await this.applyPendingChanges(generation, null, counts);
        if (this.rescanRequested) {
          await this.worker.abortBuild(generation);
          return;
        }
      }

      counts = await this.worker.commitBuild(generation);
      this.requireActive();
      this.activeGeneration = counts.generation;
      this.documents = counts.documents;
      this.chunks = counts.chunks;
      this.completed = this.total ?? 0;
      this.emit("ready");
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

  private async addSnapshotSources(
    generation: string,
    paths: readonly string[],
    initialCounts: IndexCounts,
    rebuilding: boolean,
  ): Promise<IndexCounts> {
    let counts = initialCounts;
    let batch: SourceInput[] = [];
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
    while (cursor < paths.length) {
      let reservedBytes = 0;
      const window: Array<{ path: string; read: Promise<StableSourceRead> }> = [];
      while (cursor < paths.length && window.length < this.limits.maxConcurrentReads) {
        const path = paths[cursor]!;
        const inspection = this.inspectMarkdown(path);
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
          window.push({ path, read: this.readStable(path) });
        } else {
          window.push({ path, read: Promise.resolve(inspection) });
        }
        cursor += 1;
      }

      const settled = await Promise.allSettled(window.map((entry) => entry.read));
      this.requireActive();
      for (let index = 0; index < window.length; index += 1) {
        const result = settled[index]!;
        if (result.status === "rejected") throw result.reason;
        const read = result.value;
        this.completed += 1;
        if (read.kind === "source") {
          const sourceBytes = read.source.bytes.byteLength;
          if (sourceBytes > this.limits.maxBatchBytes) {
            throw new Error("source exceeds the configured batch byte limit");
          }
          if (batch.length >= this.limits.maxBatchSources
            || batchBytes + sourceBytes > this.limits.maxBatchBytes) {
            await flush();
          }
          batch.push(read.source);
          batchBytes += sourceBytes;
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
    const nextGeneration = this.nextGeneration();
    const counts = await this.applyPendingChanges(generation, nextGeneration, {
      generation,
      documents: this.documents,
      chunks: this.chunks,
    });
    this.requireActive();
    this.activeGeneration = counts.generation;
    this.documents = counts.documents;
    this.chunks = counts.chunks;
    this.emit("ready");
  }

  private async applyPendingChanges(
    generation: string,
    nextGeneration: string | null,
    previousCounts: IndexCounts,
  ): Promise<IndexCounts> {
    const changes = this.takePendingChanges();
    if (changes.paths.length === 0) return previousCounts;

    try {
      const upserts: SourceInput[] = [];
      const removals = new Map<string, SourceRemoval>();
      for (const path of changes.removals) {
        removals.set(path, { vault_id: ACTIVE_VAULT_ID, path });
      }
      for (const path of changes.upserts) {
        const read = await this.readStable(path);
        this.requireActive();
        if (read.kind === "source") {
          if (read.source.bytes.byteLength > this.limits.maxBatchBytes) {
            throw new Error("source exceeds the configured batch byte limit");
          }
          upserts.push(read.source);
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

  private async readStable(path: string): Promise<StableSourceRead> {
    try {
      for (let attempt = 0; attempt < this.limits.maxStableReadAttempts; attempt += 1) {
        const inspection = this.inspectMarkdown(path);
        if (inspection.kind === "missing") return inspection;
        if (inspection.kind === "oversized") return inspection;
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

  private handleFailure(error: unknown): void {
    if (this.disposed) return;
    this.blocked = true;
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

  private emit(
    stage: IndexControllerStage,
    issue?: IndexControllerStatus["issue"],
  ): void {
    this.stage = stage;
    const dirty = this.pendingUpserts.size > 0
      || this.pendingRemovals.size > 0
      || this.pendingRenames.size > 0
      || this.rebuildRequested
      || this.rescanRequested
      || stage === "snapshot"
      || stage === "replay"
      || stage === "rebuild"
      || stage === "degraded"
      || stage === "failed";
    const progress = stage === "snapshot" || stage === "replay" || stage === "rebuild"
      ? { completed: this.completed, total: this.total }
      : undefined;
    this.onStatus({
      stage,
      searchable: this.activeGeneration !== null && stage !== "disposed",
      generation: this.activeGeneration,
      documents: this.documents,
      chunks: this.chunks,
      dirty,
      rebuilding: stage === "rebuild",
      ...(progress ? { progress } : {}),
      ...(issue ? { issue } : {}),
    });
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

function containsIndexLimitError(error: unknown): boolean {
  if (error instanceof AggregateError) {
    return error.errors.some((nested) => containsIndexLimitError(nested));
  }
  return typeof error === "object"
    && error !== null
    && "code" in error
    && error.code === "index_limit_exceeded";
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

function comparePaths(left: string, right: string): number {
  return left < right ? -1 : left > right ? 1 : 0;
}
