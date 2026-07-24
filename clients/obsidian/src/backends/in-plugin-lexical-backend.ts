// SPDX-FileCopyrightText: 2026 cybersader
// SPDX-License-Identifier: GPL-3.0-only

import type { SearchRequest } from "../api";
import {
  type BackendIdentity,
  type BackendStatus,
  type SearchBackend,
  type SearchExecution,
  KwiryBackendError,
} from "../backend";
import { parseFtsExcerpt } from "../excerpt";
import type { StatusResult } from "../worker/protocol";
import { WorkerRpcError } from "../worker/rpc-client";
import {
  InPluginWorkerSession,
  createBrowserWorkerSession,
} from "../worker/session";

export interface InPluginLexicalBackendOptions {
  instanceId: string;
  activeVaultId: string | null;
  workerSource: string;
  createSession?: (workerSource: string) => InPluginWorkerSession;
}

export class InPluginLexicalBackend implements SearchBackend {
  readonly identity: BackendIdentity;
  private readonly workerSource: string;
  private readonly createSession: (workerSource: string) => InPluginWorkerSession;
  private session: InPluginWorkerSession | null = null;
  private disposed = false;
  private issue: KwiryBackendError | null = null;

  constructor(options: InPluginLexicalBackendOptions) {
    this.identity = {
      profile: "in_plugin",
      instanceId: options.instanceId,
      label: "In-plugin",
      boundVaultId: options.activeVaultId,
    };
    this.workerSource = options.workerSource;
    this.createSession = options.createSession ?? createBrowserWorkerSession;
  }

  async initialize(): Promise<void> {
    this.requireActive();
    if (this.session) return;
    try {
      const session = this.createSession(this.workerSource);
      this.session = session;
      await session.initialize();
      this.issue = null;
    } catch (error) {
      this.session?.forceDispose();
      this.session = null;
      this.issue = workerBackendError(error);
      throw this.issue;
    }
  }

  async status(): Promise<BackendStatus> {
    if (this.disposed) return this.disposedStatus();
    if (this.issue) return this.unavailableStatus(this.issue);
    if (!this.session) {
      return this.unavailableStatus(new KwiryBackendError(
        "worker_failed",
        "in_plugin",
        "lifecycle",
        true,
        "In-plugin search Worker is unavailable.",
      ));
    }
    try {
      return mapStatus(this.identity, await this.session.status());
    } catch (error) {
      this.issue = workerBackendError(error);
      return this.unavailableStatus(this.issue);
    }
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
    if (!this.session) throw this.issue ?? workerBackendError(undefined);

    const status = await this.status();
    if (!status.searchable) {
      throw new KwiryBackendError(
        status.issue?.code ?? "index_building",
        "in_plugin",
        "index",
        status.issue?.recoverable ?? true,
        status.issue?.safeMessage ?? "In-plugin lexical index is still building.",
      );
    }
    try {
      const result = await this.session.search(request.q, request.limit ?? 20);
      this.requireActive();
      return {
        backend: this.identity,
        requestedMode: "lexical",
        effectiveMode: "lexical",
        generation: result.generation,
        response: {
          hits: result.hits.map((hit) => ({
            ...hit,
            excerpt: parseFtsExcerpt(hit.excerpt),
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
      throw workerBackendError(error);
    }
  }

  async dispose(): Promise<void> {
    if (this.disposed) return;
    this.disposed = true;
    const session = this.session;
    this.session = null;
    if (!session) return;
    try {
      await session.dispose();
    } catch {
      session.forceDispose();
    }
  }

  private requireActive(): void {
    if (this.disposed) {
      throw new KwiryBackendError(
        "disposed",
        "in_plugin",
        "lifecycle",
        false,
        "The in-plugin backend is disposed.",
      );
    }
  }

  private unavailableStatus(error: KwiryBackendError): BackendStatus {
    return {
      ...baseStatus(this.identity),
      phase: "unavailable",
      liveness: "terminated",
      issue: {
        code: error.code,
        safeMessage: error.safeMessage,
        recoverable: error.retryable,
      },
    };
  }

  private disposedStatus(): BackendStatus {
    return {
      ...baseStatus(this.identity),
      phase: "disposed",
      liveness: "terminated",
      issue: {
        code: "disposed",
        safeMessage: "The in-plugin backend is disposed.",
        recoverable: false,
      },
    };
  }
}

function mapStatus(identity: BackendIdentity, status: StatusResult): BackendStatus {
  const issue = status.searchable
    ? undefined
    : {
        code: "index_building",
        safeMessage: "In-plugin lexical index is still building.",
        recoverable: true,
      };
  return {
    identity,
    phase: status.phase === "failed"
      ? "unavailable"
      : status.phase,
    liveness: status.phase === "disposed" || status.phase === "failed"
      ? "terminated"
      : "alive",
    searchable: status.searchable,
    generation: status.active_generation,
    capabilities: {
      supportedModes: ["lexical"],
      sourceScope: "active_vault",
    },
    documents: status.documents,
    chunks: status.chunks,
    dirty: status.dirty,
    rebuilding: status.rebuilding,
    issue,
  };
}

function baseStatus(identity: BackendIdentity): BackendStatus {
  return {
    identity,
    phase: "building",
    liveness: "unknown",
    searchable: false,
    generation: null,
    capabilities: {
      supportedModes: ["lexical"],
      sourceScope: "active_vault",
    },
    documents: 0,
    chunks: 0,
    dirty: true,
    rebuilding: false,
  };
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
