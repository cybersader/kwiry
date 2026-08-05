// SPDX-FileCopyrightText: 2026 cybersader
// SPDX-License-Identifier: GPL-3.0-only

import { KwiryApiError, KwiryClient, type SearchRequest, type Transport } from "../api";
import {
  type BackendCapabilities,
  type BackendIdentity,
  type BackendStatus,
  type SearchBackend,
  type SearchExecution,
  KwiryBackendError,
} from "../backend";
import { DaemonCredentialError } from "../credentials";
import { parseExcerpt } from "../excerpt";

export interface DaemonBackendOptions {
  instanceId: string;
  baseUrl: string;
  currentVaultId: string | null;
  tokenProvider: () => string;
  transport: Transport;
}

const LEXICAL_ONLY: BackendCapabilities = {
  supportedModes: ["lexical"],
  sourceScope: "registered_trees",
  manualRebuild: false,
};

export class DaemonBackend implements SearchBackend {
  readonly identity: BackendIdentity;
  private readonly baseUrl: string;
  private readonly tokenProvider: () => string;
  private readonly transport: Transport;
  private disposed = false;
  private lastStatus: BackendStatus | null = null;

  constructor(options: DaemonBackendOptions) {
    this.identity = {
      profile: "daemon",
      instanceId: options.instanceId,
      label: "Daemon",
      boundVaultId: options.currentVaultId,
    };
    this.baseUrl = options.baseUrl;
    this.tokenProvider = options.tokenProvider;
    this.transport = options.transport;
  }

  async initialize(): Promise<void> {
    this.requireActive();
  }

  async status(): Promise<BackendStatus> {
    if (this.disposed) return this.disposedStatus();
    try {
      const daemon = await this.makeClient().status();
      if (this.disposed) return this.disposedStatus();
      const capabilities: BackendCapabilities = daemon.model
        ? {
            supportedModes: ["lexical", "semantic", "hybrid"],
            sourceScope: "registered_trees",
            manualRebuild: false,
          }
        : LEXICAL_ONLY;
      const searchable = daemon.generation !== null && daemon.state !== "starting";
      const status: BackendStatus = {
        identity: this.identity,
        phase: daemon.rebuilding
          ? "building"
          : daemon.state,
        liveness: "alive",
        searchable,
        generation: daemon.generation,
        capabilities,
        documents: daemon.documents,
        chunks: daemon.chunks,
        dirty: daemon.dirty,
        rebuilding: daemon.rebuilding,
      };
      this.lastStatus = status;
      return status;
    } catch (error) {
      const issue = backendErrorFrom(error);
      const status: BackendStatus = {
        identity: this.identity,
        phase: "unavailable",
        liveness: issue.code === "daemon_unreachable" ? "unreachable" : "unknown",
        searchable: false,
        generation: null,
        capabilities: LEXICAL_ONLY,
        documents: 0,
        chunks: 0,
        dirty: true,
        rebuilding: false,
        issue: {
          code: issue.code,
          safeMessage: issue.safeMessage,
          recoverable: issue.retryable,
        },
      };
      this.lastStatus = status;
      return status;
    }
  }

  async search(request: SearchRequest): Promise<SearchExecution> {
    this.requireActive();
    const status = this.lastStatus ?? await this.status();
    if (!status.searchable) {
      const issue = status.issue;
      throw new KwiryBackendError(
        issue?.code ?? "index_not_ready",
        "daemon",
        issue?.code === "daemon_unreachable" ? "transport" : "index",
        issue?.recoverable ?? true,
        issue?.safeMessage ?? "The daemon does not have a ready index.",
      );
    }
    if (!status.capabilities.supportedModes.includes(request.mode)) {
      throw new KwiryBackendError(
        "mode_unavailable",
        "daemon",
        "query",
        false,
        "The selected search mode is unavailable for the daemon.",
      );
    }

    try {
      const response = await this.makeClient().search(request);
      this.requireActive();
      return {
        backend: this.identity,
        requestedMode: request.mode,
        effectiveMode: request.mode,
        generation: status.generation,
        candidateWindow: {
          // The frozen daemon body exposes only positive continuation evidence.
          // A null cursor says nothing about exhaustion or candidate totals.
          state: response.next_cursor === null ? "unknown" : "more_available",
          candidateCount: null,
          candidateLimit: null,
        },
        response: {
          hits: response.hits.map((hit) => ({
            ...hit,
            excerpt: parseExcerpt(hit.excerpt),
            origin: {
              profile: "daemon",
              backendInstanceId: this.identity.instanceId,
              vaultId: hit.vault_id,
            },
          })),
          next_cursor: response.next_cursor,
        },
      };
    } catch (error) {
      throw backendErrorFrom(error);
    }
  }

  async dispose(): Promise<void> {
    this.disposed = true;
    this.lastStatus = this.disposedStatus();
  }

  private makeClient(): KwiryClient {
    return new KwiryClient({
      baseUrl: this.baseUrl,
      tokenProvider: this.tokenProvider,
      transport: this.transport,
    });
  }

  private requireActive(): void {
    if (this.disposed) {
      throw new KwiryBackendError(
        "disposed",
        "daemon",
        "lifecycle",
        false,
        "The daemon backend is disposed.",
      );
    }
  }

  private disposedStatus(): BackendStatus {
    return {
      identity: this.identity,
      phase: "disposed",
      liveness: "terminated",
      searchable: false,
      generation: null,
      capabilities: LEXICAL_ONLY,
      documents: 0,
      chunks: 0,
      dirty: true,
      rebuilding: false,
      issue: {
        code: "disposed",
        safeMessage: "The daemon backend is disposed.",
        recoverable: false,
      },
    };
  }
}

function backendErrorFrom(error: unknown): KwiryBackendError {
  if (error instanceof KwiryBackendError) return error;
  if (error instanceof DaemonCredentialError) {
    return new KwiryBackendError(
      error.code,
      "daemon",
      "configuration",
      true,
      error.message,
    );
  }
  if (error instanceof KwiryApiError) {
    const authentication = error.status === 401 || error.status === 403;
    return new KwiryBackendError(
      authentication ? "authentication_failed" : error.code,
      "daemon",
      error.code === "daemon_unreachable" ? "transport" : "protocol",
      error.code === "daemon_unreachable" || authentication,
      authentication ? "Daemon authentication failed." : error.message,
    );
  }
  return new KwiryBackendError(
    "internal_error",
    "daemon",
    "protocol",
    false,
    "The daemon backend could not complete the request.",
  );
}
