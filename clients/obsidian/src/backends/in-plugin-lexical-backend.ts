// SPDX-FileCopyrightText: 2026 cybersader
// SPDX-License-Identifier: GPL-3.0-only

import type { ActiveVaultSource } from "../active-vault-source";
import type { SearchRequest } from "../api";
import {
  type BackendIdentity,
  type BackendRebuildResult,
  type BackendStatus,
  type SearchBackend,
  type SearchExecution,
  KwiryBackendError,
} from "../backend";
import { classifyFailure, type FailureClassification } from "../diagnostics/classify-failure";
import {
  createExcerptHydrator,
  extractHighlightTerms,
} from "../hydrate-excerpt";
import { WorkerRpcError } from "../worker/rpc-client";
import {
  InPluginWorkerSession,
  createBrowserWorkerSession,
} from "../worker/session";
import {
  InPluginIndexController,
  type IndexControllerCacheOptions,
  type IndexControllerStartupObservation,
  type IndexControllerStatus,
} from "./in-plugin-index-controller";

export type InPluginBackendStartupObservation = IndexControllerStartupObservation
  | { kind: "first_progress" };

export interface InPluginLexicalBackendOptions {
  instanceId: string;
  activeVaultId: string;
  source: ActiveVaultSource;
  workerSource: string;
  createSession?: (workerSource: string) => InPluginWorkerSession;
  nextGeneration?: () => string;
  yieldControl?: () => Promise<void>;
  cache?: IndexControllerCacheOptions;
  /// Records a safe classification of an indexing failure. Without this the
  /// controller's catch-all `index_build_failed` reaches a field report with
  /// no indication of which subsystem broke.
  onDiagnosticFailure?: (classification: FailureClassification) => void;
  onStartupObservation?: (observation: InPluginBackendStartupObservation) => void;
}

const CAPABILITIES = {
  supportedModes: ["lexical"] as const,
  sourceScope: "active_vault" as const,
  manualRebuild: true,
};
const CLEAN_UNLOAD_CHECKPOINT_DEADLINE_MS = 1_500;
const CLEAN_UNLOAD_STORE_DISPOSAL_GRACE_MS = 250;

export class InPluginLexicalBackend implements SearchBackend {
  readonly identity: BackendIdentity;
  private readonly source: ActiveVaultSource;
  private readonly workerSource: string;
  private readonly createSession: (workerSource: string) => InPluginWorkerSession;
  private readonly nextGeneration: () => string;
  private readonly yieldControl: () => Promise<void>;
  private readonly cache: IndexControllerCacheOptions | null;
  private readonly onDiagnosticFailure: (classification: FailureClassification) => void;
  private readonly onStartupObservation: (observation: InPluginBackendStartupObservation) => void;
  private readonly statusListeners = new Set<(status: BackendStatus) => void>();
  private session: InPluginWorkerSession | null = null;
  private controller: InPluginIndexController | null = null;
  private cachedStatus: BackendStatus;
  private epoch = 0;
  private disposed = false;
  private recovering = false;
  private automaticRecoveries = 0;
  private firstProgressObserved = false;

  constructor(options: InPluginLexicalBackendOptions) {
    this.identity = {
      profile: "in_plugin",
      instanceId: options.instanceId,
      label: "In-plugin",
      boundVaultId: options.activeVaultId,
    };
    this.source = options.source;
    this.workerSource = options.workerSource;
    this.createSession = options.createSession ?? createBrowserWorkerSession;
    let generation = 0;
    this.nextGeneration = options.nextGeneration
      ?? (() => `${options.instanceId}-generation-${++generation}`);
    this.yieldControl = options.yieldControl ?? yieldToBrowser;
    this.cache = options.cache ?? null;
    this.onDiagnosticFailure = options.onDiagnosticFailure ?? (() => undefined);
    this.onStartupObservation = options.onStartupObservation ?? (() => undefined);
    this.cachedStatus = baseStatus(this.identity);
  }

  async initialize(): Promise<void> {
    this.requireActive();
    if (this.controller) return;
    this.startController(false);
  }

  async status(): Promise<BackendStatus> {
    return this.cachedStatus;
  }

  subscribeStatus(listener: (status: BackendStatus) => void): () => void {
    if (this.disposed) {
      listener(this.cachedStatus);
      return () => undefined;
    }
    this.statusListeners.add(listener);
    listener(this.cachedStatus);
    let stopped = false;
    return () => {
      if (stopped) return;
      stopped = true;
      this.statusListeners.delete(listener);
    };
  }

  async rebuild(): Promise<BackendRebuildResult> {
    this.requireActive();
    this.automaticRecoveries = 0;
    if (!this.controller) {
      this.startController(false);
      return "scheduled";
    }
    return this.controller.requestRebuild();
  }

  async search(request: SearchRequest): Promise<SearchExecution> {
    this.requireActive();
    if (request.mode !== "lexical") {
      throw new KwiryBackendError(
        "mode_unavailable",
        "in_plugin",
        "query",
        false,
        "In-plugin search supports lexical mode only.",
      );
    }

    const status = this.cachedStatus;
    if (!status.searchable) {
      throw new KwiryBackendError(
        "index_building",
        "in_plugin",
        "index",
        true,
        "In-plugin lexical index is still building.",
      );
    }

    const session = this.session;
    const epoch = this.epoch;
    if (!session) throw workerBackendError(undefined);
    try {
      const result = await session.search(request.q, request.limit ?? 20);
      this.requireActive();
      if (epoch !== this.epoch || session !== this.session) {
        throw disposedBackendError();
      }

      // Ranking completes before the Worker hydrates authoritative stored chunk
      // content. Presentation folds that bounded content locally; no vault file
      // is reread and format/locator/coverage remain non-ranking metadata.
      const hydrate = createExcerptHydrator(extractHighlightTerms(request.q));

      return {
        backend: this.identity,
        requestedMode: "lexical",
        effectiveMode: "lexical",
        generation: result.generation,
        response: {
          hits: result.hits.map((hit) => ({
            ...hit,
            excerpt: hydrate(
              hit.chunk_id,
              { kind: "text", text: hit.excerpt },
              [],
            ),
            origin: {
              profile: "in_plugin",
              backendInstanceId: this.identity.instanceId,
              vaultId: hit.vault_id,
            },
          })),
          next_cursor: null,
        },
      };
    } catch (error) {
      if (this.disposed || epoch !== this.epoch || session !== this.session) {
        throw disposedBackendError();
      }
      if (isUncertainWorkerFailure(error)) {
        this.handleUncertainWorkerFailure();
        throw recoveringBackendError();
      }
      throw workerBackendError(error);
    }
  }

  async dispose(): Promise<void> {
    if (this.disposed) return;
    this.disposed = true;
    this.epoch += 1;
    const controller = this.controller;
    this.controller = null;
    const session = this.session;
    this.session = null;
    this.recovering = false;
    this.publish(disposedStatus(this.identity));
    this.statusListeners.clear();

    try {
      await controller?.prepareForShutdown(CLEAN_UNLOAD_CHECKPOINT_DEADLINE_MS);
    } catch {
      // A final checkpoint is a bounded durability opportunity, never a reason to
      // retain the Worker or block plugin unload.
    }
    session?.forceDispose();
    controller?.dispose();
    if (controller) {
      await raceWithDeadline(
        controller.whenDisposed(),
        CLEAN_UNLOAD_STORE_DISPOSAL_GRACE_MS,
      );
    }
  }

  private startController(recovering: boolean): void {
    this.requireActive();
    this.stopCurrentSession();
    const epoch = ++this.epoch;
    this.recovering = recovering;
    if (recovering) this.publish(recoveringStatus(this.identity));
    else this.publish(baseStatus(this.identity));

    try {
      const session = this.createSession(this.workerSource);
      let latestStatus: IndexControllerStatus | null = null;
      const controller = new InPluginIndexController({
        source: this.source,
        worker: session,
        nextGeneration: this.nextGeneration,
        onStatus: (status) => {
          if (this.disposed || epoch !== this.epoch || controller !== this.controller) return;
          latestStatus = status;
          if (!this.firstProgressObserved && isMeaningfulControllerProgress(status.progress)) {
            this.firstProgressObserved = true;
            try {
              this.onStartupObservation({ kind: "first_progress" });
            } catch {
              // Startup instrumentation cannot interrupt backend publication.
            }
          }
          if (status.stage === "ready" && !status.dirty) {
            this.recovering = false;
            this.automaticRecoveries = 0;
          }
          this.publish(mapControllerStatus(this.identity, status, this.recovering));
        },
        onFailure: (error) => {
          // Classify before any epoch guard returns. The controller's issue
          // codes collapse everything unrecognised into index_build_failed, so
          // without this the raw cause is discarded here and a field report
          // can only say that indexing failed, not what failed.
          try {
            this.onDiagnosticFailure(classifyFailure(error));
          } catch {
            // Diagnostics must never change the failure path they observe.
          }
          if (this.disposed || epoch !== this.epoch || controller !== this.controller) return;
          if (isUncertainWorkerFailure(error)) {
            this.handleUncertainWorkerFailure();
          } else if (this.recovering && latestStatus) {
            this.recovering = false;
            this.publish(mapControllerStatus(this.identity, latestStatus, false));
          }
        },
        onStartupObservation: (observation) => {
          if (this.disposed || epoch !== this.epoch || controller !== this.controller) return;
          try {
            this.onStartupObservation(observation);
          } catch {
            // Startup instrumentation cannot interrupt backend publication.
          }
        },
        yieldControl: this.yieldControl,
        ...(this.cache ? { cache: this.cache } : {}),
      });
      this.session = session;
      this.controller = controller;
      controller.start();
    } catch {
      this.stopCurrentSession();
      this.recovering = false;
      this.publish(unavailableStatus(this.identity, "worker_failed", "In-plugin search Worker failed."));
      try {
        this.onStartupObservation({
          kind: "terminal",
          outcome: "failed",
          reason: "backend_unavailable",
        });
      } catch {
        // Startup instrumentation cannot interrupt backend failure handling.
      }
    }
  }

  private handleUncertainWorkerFailure(): void {
    if (this.disposed) return;
    if (this.automaticRecoveries >= 1) {
      this.stopCurrentSession();
      this.recovering = false;
      this.publish(unavailableStatus(
        this.identity,
        "worker_failed",
        "In-plugin search Worker failed.",
      ));
      return;
    }
    this.automaticRecoveries += 1;
    this.startController(true);
  }

  private stopCurrentSession(): void {
    const controller = this.controller;
    this.controller = null;
    controller?.dispose();
    const session = this.session;
    this.session = null;
    session?.forceDispose();
  }

  private publish(status: BackendStatus): void {
    if (this.disposed && status.phase !== "disposed") return;
    this.cachedStatus = status;
    for (const listener of this.statusListeners) {
      try {
        listener(status);
      } catch {
        // Status observers cannot interrupt indexing or cleanup.
      }
    }
  }

  private requireActive(): void {
    if (this.disposed) throw disposedBackendError();
  }
}

function isMeaningfulControllerProgress(
  progress: IndexControllerStatus["progress"],
): boolean {
  if (!progress) return false;
  if (progress.activity === "inventory") return progress.total !== null;
  if (progress.activity === "read") return progress.inFlight > 0;
  return true;
}

function mapControllerStatus(
  identity: BackendIdentity,
  status: IndexControllerStatus,
  recovering: boolean,
): BackendStatus {
  if (status.stage === "disposed") return disposedStatus(identity);
  const issue = recovering && !(status.stage === "ready" && !status.dirty)
    ? {
        code: "worker_recovering",
        safeMessage: "In-plugin search Worker is recovering.",
        recoverable: true,
      }
    : status.issue
      ? controllerIssue(status.issue, status)
      : status.searchable
        ? undefined
        : {
            code: "index_building",
            safeMessage: "In-plugin lexical index is still building.",
            recoverable: true,
          };
  const phase = status.stage === "ready" && !status.dirty
    && (!issue || isCurrentCachePersistencePending(issue.code))
    ? "ready"
    : status.stage === "ready" && !status.dirty && issue
      ? "degraded"
      : status.stage === "degraded"
      ? "degraded"
      : status.stage === "failed"
        ? "unavailable"
        : status.stage === "starting"
          ? "starting"
          : "building";
  const progress = status.progress && (
    status.stage === "snapshot"
    || status.stage === "replay"
    || status.stage === "rebuild"
    || status.stage === "degraded"
    || status.stage === "failed"
  )
    ? {
        stage: status.stage,
        activity: status.progress.activity,
        completed: status.progress.completed,
        total: status.progress.total,
        inFlight: status.progress.inFlight,
        ...(status.progress.subphase === undefined
          ? {}
          : { subphase: status.progress.subphase }),
        ...(status.progress.stallCategory === undefined
          ? {}
          : { stallCategory: status.progress.stallCategory }),
      }
    : undefined;
  return {
    identity,
    phase,
    liveness: "alive",
    searchable: status.searchable,
    generation: status.generation,
    capabilities: CAPABILITIES,
    documents: status.documents,
    chunks: status.chunks,
    sourceFormatCounts: status.sourceFormatCounts,
    quarantinedSources: status.quarantinedSources,
    unreadableSources: status.unreadableSources,
    quarantineValidatorFields: status.quarantineValidatorFields,
    dirty: status.dirty,
    rebuilding: status.rebuilding,
    ...(progress ? { progress } : {}),
    ...(issue ? { issue } : {}),
  };
}

function controllerIssue(
  issue: NonNullable<IndexControllerStatus["issue"]>,
  status: IndexControllerStatus,
) {
  switch (issue) {
    case "vault_read_failed":
      return {
        code: issue,
        safeMessage: "The active vault could not be read completely.",
        recoverable: true,
      };
    case "index_build_failed":
      return {
        code: issue,
        safeMessage: "The in-plugin lexical index could not be built.",
        recoverable: true,
      };
    case "index_update_failed":
      return {
        code: issue,
        safeMessage: "The in-plugin lexical index could not be updated.",
        recoverable: true,
      };
    case "index_limit_exceeded":
      return {
        code: issue,
        safeMessage: "The in-plugin lexical index reached its capacity limit.",
        recoverable: true,
      };
    case "sources_quarantined":
    case "sources_unreadable":
      return {
        code: issue,
        safeMessage: omissionMessage(status.quarantinedSources, status.unreadableSources),
        recoverable: true,
      };
    case "index_reconciling":
      return {
        code: issue,
        safeMessage: "Cached index searchable; reconciling vault changes…",
        recoverable: true,
      };
    case "cache_absent":
      return {
        code: issue,
        safeMessage: status.searchable
          ? status.dirty
            ? "Index is searchable but stale; cached durability is pending."
            : "Index is current; cached durability is pending."
          : "No cached index; building a fresh index…",
        recoverable: true,
      };
    case "cache_unavailable":
      return {
        code: issue,
        safeMessage: status.searchable
          ? status.dirty
            ? "Index is searchable but stale; cache durability is unavailable."
            : "Index is current, but cache durability is unavailable."
          : "Cache unavailable; building a fresh index…",
        recoverable: true,
      };
    case "cache_corrupt":
      return {
        code: issue,
        safeMessage: status.searchable
          ? status.dirty
            ? "Fresh index is searchable but stale; cache replacement is pending."
            : "Fresh index is current; replacing the discarded cache…"
          : "Cached index rejected and discarded; building fresh…",
        recoverable: true,
      };
    case "cache_incompatible":
      return {
        code: issue,
        safeMessage: status.searchable && !status.dirty
          ? "Fresh index is current; replacing the incompatible cache…"
          : "Cached index is incompatible; building fresh…",
        recoverable: true,
      };
    case "cache_restore_unavailable":
      return {
        code: issue,
        safeMessage: status.searchable && !status.dirty
          ? "Fresh index is current; replacing the unrestorable cache…"
          : "Cached index could not be restored; building fresh…",
        recoverable: true,
      };
    case "cache_discard_failed":
      return {
        code: issue,
        safeMessage: status.searchable && !status.dirty
          ? "Fresh index is current; rejected cache could not be discarded."
          : "Cached index was rejected but could not be discarded; building fresh…",
        recoverable: true,
      };
    case "cache_save_failed":
      return {
        code: issue,
        safeMessage: "Search ready; cache save failed",
        recoverable: true,
      };
    case "checkpoint_corrupt":
      return {
        code: issue,
        safeMessage: "Initial-build checkpoint was corrupt; building fresh…",
        recoverable: true,
      };
    case "checkpoint_incompatible":
      return {
        code: issue,
        safeMessage: "Initial-build checkpoint is incompatible; building fresh…",
        recoverable: true,
      };
    case "checkpoint_unavailable":
      return {
        code: issue,
        safeMessage: "Initial-build checkpoint could not be resumed; building fresh…",
        recoverable: true,
      };
    case "checkpoint_discard_failed":
      return {
        code: issue,
        safeMessage: status.searchable
          ? "Search ready; stale initial-build checkpoint cleanup failed."
          : "Initial-build checkpoint was rejected but could not be discarded; building fresh…",
        recoverable: true,
      };
    case "checkpoint_save_failed":
      return {
        code: issue,
        safeMessage: "Initial build continues, but checkpoint save failed.",
        recoverable: true,
      };
  }
}

function isCurrentCachePersistencePending(code: string): boolean {
  return code === "cache_absent"
    || code === "cache_corrupt"
    || code === "cache_incompatible"
    || code === "cache_restore_unavailable";
}

function omissionMessage(quarantined: number, unreadable: number): string {
  const total = quarantined + unreadable;
  const note = total === 1 ? "note may be" : "notes may be";
  const details = [
    ...(quarantined === 0 ? [] : [`${quarantined} quarantined`]),
    ...(unreadable === 0 ? [] : [`${unreadable} unreadable`]),
  ].join(", ");
  return `${total} ${note} missing from search (${details}).`;
}

function baseStatus(identity: BackendIdentity): BackendStatus {
  return {
    identity,
    phase: "starting",
    liveness: "unknown",
    searchable: false,
    generation: null,
    capabilities: CAPABILITIES,
    documents: 0,
    chunks: 0,
    dirty: true,
    rebuilding: false,
    issue: {
      code: "index_building",
      safeMessage: "In-plugin lexical index is still building.",
      recoverable: true,
    },
  };
}

function recoveringStatus(identity: BackendIdentity): BackendStatus {
  return {
    ...baseStatus(identity),
    phase: "building",
    liveness: "alive",
    issue: {
      code: "worker_recovering",
      safeMessage: "In-plugin search Worker is recovering.",
      recoverable: true,
    },
  };
}

function unavailableStatus(
  identity: BackendIdentity,
  code: string,
  safeMessage: string,
): BackendStatus {
  return {
    ...baseStatus(identity),
    phase: "unavailable",
    liveness: "terminated",
    issue: { code, safeMessage, recoverable: true },
  };
}

function disposedStatus(identity: BackendIdentity): BackendStatus {
  return {
    ...baseStatus(identity),
    phase: "disposed",
    liveness: "terminated",
    issue: {
      code: "disposed",
      safeMessage: "The in-plugin backend is disposed.",
      recoverable: false,
    },
  };
}

function isUncertainWorkerFailure(error: unknown): boolean {
  if (error instanceof AggregateError) {
    return error.errors.some((nested) => isUncertainWorkerFailure(nested));
  }
  return error instanceof WorkerRpcError && (
    error.code === "timeout"
    || error.code === "worker_crashed"
    || error.code === "protocol_mismatch"
    || (error.code === "invalid_request" && error.stage === "protocol")
  );
}

function workerBackendError(error: unknown): KwiryBackendError {
  if (error instanceof KwiryBackendError) return error;
  if (error instanceof WorkerRpcError) {
    const code = error.code === "query_rejected"
      ? "invalid_query"
      : error.code === "index_building"
        ? "index_building"
        : "worker_failed";
    return new KwiryBackendError(
      code,
      "in_plugin",
      error.stage === "query" ? "query" : "lifecycle",
      error.retryable,
      error.code === "query_rejected"
        ? "The query is unavailable in the in-plugin backend."
        : error.code === "index_building"
          ? "In-plugin lexical index is still building."
          : "In-plugin search Worker failed.",
    );
  }
  return new KwiryBackendError(
    "worker_failed",
    "in_plugin",
    "lifecycle",
    true,
    "In-plugin search Worker failed.",
  );
}

function recoveringBackendError(): KwiryBackendError {
  return new KwiryBackendError(
    "worker_recovering",
    "in_plugin",
    "lifecycle",
    true,
    "In-plugin search Worker is recovering.",
  );
}

function disposedBackendError(): KwiryBackendError {
  return new KwiryBackendError(
    "disposed",
    "in_plugin",
    "lifecycle",
    false,
    "The in-plugin backend is disposed.",
  );
}

async function raceWithDeadline(task: Promise<void>, deadlineMs: number): Promise<void> {
  let timer: ReturnType<typeof setTimeout> | null = null;
  try {
    await Promise.race([
      task,
      new Promise<void>((resolve) => {
        timer = setTimeout(resolve, deadlineMs);
      }),
    ]);
  } finally {
    if (timer !== null) clearTimeout(timer);
  }
}

function yieldToBrowser(): Promise<void> {
  return new Promise((resolve) => window.setTimeout(resolve, 0));
}
