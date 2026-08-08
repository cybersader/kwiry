// SPDX-FileCopyrightText: 2026 cybersader
// SPDX-License-Identifier: GPL-3.0-only

import type {
  CacheLoad,
  InitialBuildCheckpointLoad,
} from "../cache/cache-store";
import type {
  BuildResult,
  DisposeResult,
  ExportGenerationResult,
  InitialBuildCheckpointExportResult,
  InitialBuildCheckpointReconciliationPlanResult,
  InitialBuildCheckpointCursor,
  InitializeResult,
  ReconciliationPlanResult,
  ReconciliationSourceMetadata,
  RestoreGenerationResult,
  RestoreInitialBuildCheckpointResult,
  SourceFormat,
  SearchResult,
  SourceUpsert,
  SourceRemoval,
  StatusResult,
} from "./protocol";
import { SOURCE_FORMATS } from "./protocol";
import {
  WorkerRpcClient,
  type WorkerLike,
} from "./rpc-client";
import { PRODUCTION_RPC_PROTOCOL } from "./production-rpc-protocol";

const DEFAULT_SOURCE_POLICY_HASH = "c414b56f31d22f8e1fbe69f5074bc8862337d1c8ee6065b6ad0da441b4f63860";

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

  initialize(
    vaultId: string,
    sourcePolicyHash = DEFAULT_SOURCE_POLICY_HASH,
    enabledSourceFormats: readonly SourceFormat[] = SOURCE_FORMATS,
  ): Promise<InitializeResult> {
    return this.client.request({
      operation: "initialize",
      vault_id: vaultId,
      source_policy_hash: sourcePolicyHash,
      // Sorted here rather than trusted: the Worker requires a sorted,
      // duplicate-free set, and the caller's own ordering is not evidence.
      enabled_source_formats: [...new Set(enabledSourceFormats)].sort(),
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
  exportInitialBuildCheckpoint(
    generation: string,
    cacheIdentity: string,
    cursor: InitialBuildCheckpointCursor,
  ): Promise<InitialBuildCheckpointExportResult> {
    return this.client.request({
      operation: "export_initial_build_checkpoint",
      generation,
      cache_identity: cacheIdentity,
      cursor,
    }) as Promise<InitialBuildCheckpointExportResult>;
  }

  restoreGeneration(
    hit: Extract<CacheLoad, { kind: "hit" }>,
    expectedCacheIdentity: string,
    expectedSourcePolicyHash = DEFAULT_SOURCE_POLICY_HASH,
  ): Promise<RestoreGenerationResult> {
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
    }) as Promise<RestoreGenerationResult>;
  }

  restoreInitialBuildCheckpoint(
    hit: Extract<InitialBuildCheckpointLoad, { kind: "hit" }>,
    expectedCacheIdentity: string,
    expectedSourcePolicyHash = DEFAULT_SOURCE_POLICY_HASH,
  ): Promise<RestoreInitialBuildCheckpointResult> {
    const { record } = hit;
    return this.client.request({
      operation: "restore_initial_build_checkpoint",
      record_kind: record.recordKind,
      checkpoint_record_version: record.recordVersion,
      checkpoint_image_version: record.imageVersion,
      generation: record.generationId,
      cursor: record.cursor,
      bytes: hit.bytes,
      blob_byte_length: record.byteLength,
      blob_sha256: record.sha256,
      digest_verified: hit.digestVerified,
      ...record.identity,
      expected_cache_identity: expectedCacheIdentity,
      expected_source_policy_hash: expectedSourcePolicyHash,
    }) as Promise<RestoreInitialBuildCheckpointResult>;
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

  planInitialBuildCheckpointReconciliation(
    generation: string,
    vaultId: string,
    currentSources: ReconciliationSourceMetadata[],
  ): Promise<InitialBuildCheckpointReconciliationPlanResult> {
    return this.client.request({
      operation: "plan_initial_build_checkpoint_reconciliation",
      generation,
      vault_id: vaultId,
      current_sources: currentSources,
    }) as Promise<InitialBuildCheckpointReconciliationPlanResult>;
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
