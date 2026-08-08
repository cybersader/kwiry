// SPDX-FileCopyrightText: 2026 cybersader
// SPDX-License-Identifier: GPL-3.0-only

import {
  INTERNAL_D5C_COMPARE_OPERATION,
  isD5cCompareResponse,
  type D5cCompareCommand,
  type D5cCompareResult,
} from "./d5c-compare-protocol";
import { D5C_OWNER_RPC_PROTOCOL } from "./d5c-owner-protocol";
import type {
  BuildResult,
  DisposeResult,
  InitializeResult,
  SourceRemoval,
  SourceUpsert,
} from "./protocol";
import {
  WorkerRpcClient,
  type RpcCommand,
  type WorkerLike,
  type WorkerRpcExtension,
} from "./rpc-client";

export const D5C_RPC_EXTENSION: WorkerRpcExtension = Object.freeze({
  operation: INTERNAL_D5C_COMPARE_OPERATION,
  parseResponse(value: unknown) {
    return isD5cCompareResponse(value) ? value : null;
  },
  expectedGeneration(command: RpcCommand) {
    return d5cCommand(command)?.generation ?? null;
  },
  matchesCorrelation(result: unknown, command: RpcCommand) {
    const request = d5cCommand(command);
    if (!request || typeof result !== "object" || result === null) return false;
    const comparison = result as D5cCompareResult;
    return request.revision === null
      ? comparison.publication === "active" && comparison.revision === null
      : comparison.publication === "initial_staging"
        && comparison.revision === request.revision;
  },
});

export class D5cWorkerSession {
  private readonly client: WorkerRpcClient;
  private cleaned = false;

  constructor(
    private readonly worker: WorkerLike,
    private readonly objectUrl: string,
    private readonly revokeObjectUrl: (url: string) => void,
    timeoutMs?: number,
  ) {
    this.client = new WorkerRpcClient(
      worker,
      D5C_OWNER_RPC_PROTOCOL,
      timeoutMs,
      D5C_RPC_EXTENSION,
    );
  }

  initialize(vaultId: string): Promise<InitializeResult> {
    return this.client.request({
      operation: "initialize",
      vault_id: vaultId,
    }) as Promise<InitializeResult>;
  }

  beginBuild(generation: string): Promise<BuildResult> {
    return this.client.request({
      operation: "begin_build",
      generation,
    }) as Promise<BuildResult>;
  }

  addSourceBatch(
    generation: string,
    sources: SourceUpsert[],
  ): Promise<BuildResult> {
    return this.client.request({
      operation: "add_source_batch",
      generation,
      sources,
    }) as Promise<BuildResult>;
  }

  applySourceChanges(
    generation: string,
    nextGeneration: string | null,
    upserts: SourceUpsert[],
    removals: SourceRemoval[],
  ): Promise<BuildResult> {
    return this.client.request({
      operation: "apply_source_changes",
      generation,
      next_generation: nextGeneration,
      upserts,
      removals,
    }) as Promise<BuildResult>;
  }

  commitBuild(generation: string): Promise<BuildResult> {
    return this.client.request({
      operation: "commit_build",
      generation,
    }) as Promise<BuildResult>;
  }

  abortBuild(generation: string): Promise<BuildResult> {
    return this.client.request({
      operation: "abort_build",
      generation,
    }) as Promise<BuildResult>;
  }

  compareD5c(
    generation: string,
    revision: number | null,
    query: string,
    limit: number,
    queryTimeEpochSeconds: string,
  ): Promise<D5cCompareResult> {
    return this.client.request({
      operation: INTERNAL_D5C_COMPARE_OPERATION,
      generation,
      revision,
      query,
      limit,
      query_time_epoch_seconds: queryTimeEpochSeconds,
    }) as Promise<D5cCompareResult>;
  }

  async dispose(): Promise<DisposeResult> {
    if (this.cleaned) return { closed: true };
    try {
      return await this.client.request({ operation: "dispose" }) as DisposeResult;
    } finally {
      this.forceDispose();
    }
  }

  forceDispose(): void {
    if (this.cleaned) return;
    this.cleaned = true;
    this.client.stop();
    this.worker.terminate();
    this.revokeObjectUrl(this.objectUrl);
  }
}

export function createBrowserD5cWorkerSession(
  workerSource: string,
  timeoutMs?: number,
): D5cWorkerSession {
  const blob = new Blob([workerSource], { type: "text/javascript" });
  const objectUrl = URL.createObjectURL(blob);
  try {
    const worker = new Worker(objectUrl, { name: "kwiry-d5c-owner-search" });
    return new D5cWorkerSession(
      worker,
      objectUrl,
      (url) => URL.revokeObjectURL(url),
      timeoutMs,
    );
  } catch (error) {
    URL.revokeObjectURL(objectUrl);
    throw error;
  }
}

function d5cCommand(command: RpcCommand): D5cCompareCommand | null {
  if (command.operation !== INTERNAL_D5C_COMPARE_OPERATION) return null;
  return command as unknown as D5cCompareCommand;
}
