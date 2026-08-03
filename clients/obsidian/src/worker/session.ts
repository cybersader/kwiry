// SPDX-FileCopyrightText: 2026 cybersader
// SPDX-License-Identifier: GPL-3.0-only

import type { CacheLoad } from "../cache/cache-store";
import type {
  BuildResult,
  DisposeResult,
  ExportGenerationResult,
  InitializeResult,
  ReconciliationPlanResult,
  ReconciliationSourceMetadata,
  SearchResult,
  SourceUpsert,
  SourceRemoval,
  StatusResult,
} from "./protocol";
import {
  WorkerRpcClient,
  type WorkerLike,
} from "./rpc-client";
import { PRODUCTION_RPC_PROTOCOL } from "./production-rpc-protocol";

const DEFAULT_SOURCE_POLICY_HASH = "9ac3d481372532c3c6259eedd2c1fdb51a3de4dd6807bf1ef8f95d4fc47fe20b";

export class InPluginWorkerSession {
  protected readonly client: WorkerRpcClient;
  private cleaned = false;

  constructor(
    private readonly worker: WorkerLike,
    private readonly objectUrl: string,
    private readonly revokeObjectUrl: (url: string) => void,
    timeoutMs?: number,
  ) {
    this.client = new WorkerRpcClient(
      worker,
      PRODUCTION_RPC_PROTOCOL,
      timeoutMs,
    );
  }

  initialize(vaultId: string, sourcePolicyHash = DEFAULT_SOURCE_POLICY_HASH): Promise<InitializeResult> {
    return this.client.request({
      operation: "initialize",
      vault_id: vaultId,
      source_policy_hash: sourcePolicyHash,
    }) as Promise<InitializeResult>;
  }

  beginBuild(generation: string): Promise<BuildResult> {
    return this.client.request({ operation: "begin_build", generation }) as Promise<BuildResult>;
  }

  addSourceBatch(generation: string, sources: SourceUpsert[]): Promise<BuildResult> {
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
    return this.client.request({ operation: "commit_build", generation }) as Promise<BuildResult>;
  }

  abortBuild(generation: string): Promise<BuildResult> {
    return this.client.request({ operation: "abort_build", generation }) as Promise<BuildResult>;
  }

  /**
   * Exports the clean active generation. Deliberately absent from
   * `IndexWorkerPort`: exporting a cache is not part of the index controller's
   * contract, and adding it there would pull indexing into cache concerns.
   */
  exportGeneration(generation: string, cacheIdentity: string): Promise<ExportGenerationResult> {
    return this.client.request({
      operation: "export_generation",
      generation,
      cache_identity: cacheIdentity,
    }) as Promise<ExportGenerationResult>;
  }

  /**
   * Transfers a B6.2 hit to the persistence-blind Worker. The literal false is
   * forwarded rather than erased so the Worker can require and discharge the
   * digest-verification obligation itself.
   */
  restoreGeneration(
    hit: Extract<CacheLoad, { kind: "hit" }>,
    expectedCacheIdentity: string,
    expectedSourcePolicyHash = DEFAULT_SOURCE_POLICY_HASH,
  ): Promise<BuildResult> {
    const { record } = hit;
    return this.client.request({
      operation: "restore_generation",
      generation: record.generationId,
      bytes: hit.bytes,
      blob_byte_length: record.byteLength,
      blob_sha256: record.sha256,
      digest_verified: hit.digestVerified,
      ...record.identity,
      expected_cache_identity: expectedCacheIdentity,
      expected_source_policy_hash: expectedSourcePolicyHash,
    }) as Promise<BuildResult>;
  }

  planReconciliation(
    generation: string,
    vaultId: string,
    currentSources: ReconciliationSourceMetadata[],
  ): Promise<ReconciliationPlanResult> {
    return this.client.request({
      operation: "plan_reconciliation",
      generation,
      vault_id: vaultId,
      current_sources: currentSources,
    }) as Promise<ReconciliationPlanResult>;
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
