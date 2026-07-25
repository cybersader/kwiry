// SPDX-FileCopyrightText: 2026 cybersader
// SPDX-License-Identifier: GPL-3.0-only

import type {
  BuildResult,
  DisposeResult,
  InitializeResult,
  SearchResult,
  SourceInput,
  SourceRemoval,
  StatusResult,
} from "./protocol";
import { WorkerRpcClient, type WorkerLike } from "./rpc-client";

export class InPluginWorkerSession {
  private readonly client: WorkerRpcClient;
  private cleaned = false;

  constructor(
    private readonly worker: WorkerLike,
    private readonly objectUrl: string,
    private readonly revokeObjectUrl: (url: string) => void,
    timeoutMs?: number,
  ) {
    this.client = new WorkerRpcClient(worker, timeoutMs);
  }

  initialize(): Promise<InitializeResult> {
    return this.client.request({ operation: "initialize" }) as Promise<InitializeResult>;
  }

  beginBuild(generation: string): Promise<BuildResult> {
    return this.client.request({ operation: "begin_build", generation }) as Promise<BuildResult>;
  }

  addSourceBatch(generation: string, sources: SourceInput[]): Promise<BuildResult> {
    return this.client.request({
      operation: "add_source_batch",
      generation,
      sources,
    }) as Promise<BuildResult>;
  }

  applySourceChanges(
    generation: string,
    nextGeneration: string | null,
    upserts: SourceInput[],
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
    return this.client.request({ operation: "commit_build", generation }) as Promise<BuildResult>;
  }

  abortBuild(generation: string): Promise<BuildResult> {
    return this.client.request({ operation: "abort_build", generation }) as Promise<BuildResult>;
  }

  search(query: string, limit: number): Promise<SearchResult> {
    return this.client.request({ operation: "search", query, limit }) as Promise<SearchResult>;
  }

  status(): Promise<StatusResult> {
    return this.client.request({ operation: "status" }) as Promise<StatusResult>;
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

export function createBrowserWorkerSession(
  workerSource: string,
  timeoutMs?: number,
): InPluginWorkerSession {
  const blob = new Blob([workerSource], { type: "text/javascript" });
  const objectUrl = URL.createObjectURL(blob);
  try {
    const worker = new Worker(objectUrl, { name: "kwiry-in-plugin-search" });
    return new InPluginWorkerSession(
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
