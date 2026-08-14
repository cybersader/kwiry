// SPDX-FileCopyrightText: 2026 cybersader
// SPDX-License-Identifier: GPL-3.0-only

import { describe, expect, it, vi } from "vitest";

import { INTERNAL_D5C_COMPARE_OPERATION } from "../src/worker/d5c-compare-protocol";
import { D5C_OWNER_RPC_PROTOCOL } from "../src/worker/d5c-owner-protocol";
import { D5C_RPC_EXTENSION } from "../src/worker/d5c-session";
import {
  CACHE_SCHEMA_VERSION,
  INITIAL_BUILD_CHECKPOINT_IMAGE_VERSION,
  INITIAL_BUILD_CHECKPOINT_RECORD_KIND,
  INITIAL_BUILD_CHECKPOINT_RECORD_VERSION,
  WORKER_PROTOCOL_VERSION,
  emptyRestoreEvictionReport,
  emptySourceFormatCounts,
  type WorkerRequest,
} from "../src/worker/protocol";
import { PRODUCTION_RPC_PROTOCOL } from "../src/worker/production-rpc-protocol";
import { WorkerRpcClient, type WorkerLike } from "../src/worker/rpc-client";

function sourceFormatCounts(indexed = 0) {
  const counts = emptySourceFormatCounts();
  counts.markdown["indexed-complete"] = indexed;
  return counts;
}

class MockWorker implements WorkerLike {
  readonly posted: WorkerRequest[] = [];
  readonly transfers: (Transferable[] | undefined)[] = [];
  readonly listeners = new Map<string, Set<(event: never) => void>>();
  terminate = vi.fn();

  postMessage(message: unknown, transfer?: Transferable[]): void {
    this.posted.push(message as WorkerRequest);
    this.transfers.push(transfer);
  }

  addEventListener(type: "message" | "error" | "messageerror", listener: (event: never) => void): void {
    const listeners = this.listeners.get(type) ?? new Set();
    listeners.add(listener);
    this.listeners.set(type, listeners);
  }

  removeEventListener(type: "message" | "error" | "messageerror", listener: (event: never) => void): void {
    this.listeners.get(type)?.delete(listener);
  }

  emitMessage(data: unknown): void {
    for (const listener of this.listeners.get("message") ?? []) {
      listener({ data } as never);
    }
  }

  emitError(): void {
    for (const listener of this.listeners.get("error") ?? []) listener({} as never);
  }
}

function statusResponse(id: number): unknown {
  return {
    version: WORKER_PROTOCOL_VERSION,
    id,
    operation: "status",
    ok: true,
    result: {
      phase: "building",
      searchable: false,
      active_generation: null,
      staging_generation: null,
      documents: 0,
      chunks: 0,
      active_database_bytes: 0,
      staging_database_bytes: 0,
      database_byte_limit: 1,
      source_format_counts: emptySourceFormatCounts(),
      dirty: true,
      rebuilding: false,
    },
  };
}

function compareResponse(
  id: number,
  generation: string,
  revision: number | null,
  publication: "active" | "initial_staging",
): unknown {
  return {
    version: WORKER_PROTOCOL_VERSION,
    id,
    operation: INTERNAL_D5C_COMPARE_OPERATION,
    ok: true,
    result: {
      schema_version: 2,
      generation,
      publication,
      revision,
      candidate_pool_count: 1,
      display_candidates: [{
        ordinal: 0,
        hit: { path: "note.md", heading_path: [], frontmatter: { title: "Note" } },
      }],
      text_order: [0],
      balanced_order: [0],
      aggregate: { moved_candidate_count: 0, top_n_overlap: 1 },
    },
  };
}

function exportResult(generation: string, cacheIdentity: string): unknown {
  return {
    generation,
    documents: 1,
    chunks: 1,
    bytes: new Uint8Array([7, 7, 7]),
    blob_byte_length: 3,
    blob_sha256: "a".repeat(64),
    protocol_version: WORKER_PROTOCOL_VERSION,
    cache_schema_version: CACHE_SCHEMA_VERSION,
    chunking_version: 1,
    sqlite_version: "3.53.0",
    sqlite_wasm_sha256: "b".repeat(64),
    rust_wasm_sha256: "c".repeat(64),
    plugin_id: "kwiry-search",
    plugin_version: "0.1.0",
    cache_identity: cacheIdentity,
    source_policy_hash: "e".repeat(64),
  };
}

function restoreCommand(generation = "g1") {
  const identity = "d".repeat(64);
  return {
    operation: "restore_generation" as const,
    generation,
    bytes: new Uint8Array([1, 2, 3]),
    blob_byte_length: 3,
    blob_sha256: "a".repeat(64),
    digest_verified: false as const,
    protocol_version: WORKER_PROTOCOL_VERSION,
    cache_schema_version: CACHE_SCHEMA_VERSION,
    chunking_version: 1,
    sqlite_version: "3.53.0",
    sqlite_wasm_sha256: "b".repeat(64),
    rust_wasm_sha256: "c".repeat(64),
    plugin_id: "kwiry-search",
    plugin_version: "0.1.0",
    cache_identity: identity,
    source_policy_hash: identity,
    expected_cache_identity: identity,
    expected_source_policy_hash: identity,
  };
}

const CHECKPOINT_CURSOR = {
  snapshot_source_count: 1,
  acknowledged_add_batches: 1,
  acknowledged_prefix_sources: 1,
  last_acknowledged_path: "alpha.md",
} as const;

function checkpointRestoreCommand(generation = "g1") {
  return {
    ...restoreCommand(generation),
    operation: "restore_initial_build_checkpoint" as const,
    record_kind: INITIAL_BUILD_CHECKPOINT_RECORD_KIND,
    checkpoint_record_version: INITIAL_BUILD_CHECKPOINT_RECORD_VERSION,
    checkpoint_image_version: INITIAL_BUILD_CHECKPOINT_IMAGE_VERSION,
    cursor: CHECKPOINT_CURSOR,
  };
}

function checkpointRestoreResult(generation = "g1") {
  return {
    generation,
    documents: 1,
    chunks: 1,
    database_bytes: 1,
    database_byte_limit: 2,
    quarantined_sources: 0,
    quarantine_fields: [],
    source_format_counts: sourceFormatCounts(1),
    record_kind: INITIAL_BUILD_CHECKPOINT_RECORD_KIND,
    publication: "initial_staging" as const,
    searchable: false as const,
    cursor: CHECKPOINT_CURSOR,
  };
}

describe("WorkerRpcClient", () => {
  it("correlates responses and removes completed requests", async () => {
    const worker = new MockWorker();
    const client = new WorkerRpcClient(worker, PRODUCTION_RPC_PROTOCOL, 1_000);
    const pending = client.request({ operation: "status" });
    expect(worker.posted[0]).toEqual({
      version: WORKER_PROTOCOL_VERSION,
      id: 1,
      operation: "status",
    });
    worker.emitMessage(statusResponse(1));
    await expect(pending).resolves.toMatchObject({ phase: "building" });
    expect(client.pendingCount).toBe(0);
  });

  it("serializes source changes and correlates their published generation", async () => {
    const worker = new MockWorker();
    const client = new WorkerRpcClient(worker, PRODUCTION_RPC_PROTOCOL, 1_000);
    const pending = client.request({
      operation: "apply_source_changes",
      generation: "g1",
      next_generation: "g2",
      upserts: [],
      removals: [{ vault_id: "active", path: "old.md" }],
    });
    expect(worker.posted[0]).toEqual({
      version: WORKER_PROTOCOL_VERSION,
      id: 1,
      operation: "apply_source_changes",
      generation: "g1",
      next_generation: "g2",
      upserts: [],
      removals: [{ vault_id: "active", path: "old.md" }],
    });
    worker.emitMessage({
      version: WORKER_PROTOCOL_VERSION,
      id: 1,
      operation: "apply_source_changes",
      ok: true,
      result: {
        generation: "g2",
        documents: 0,
        chunks: 0,
        database_bytes: 0,
        database_byte_limit: 1,
        quarantined_sources: 0,
        quarantine_fields: [],
        source_format_counts: emptySourceFormatCounts(),
      },
    });
    await expect(pending).resolves.toMatchObject({ generation: "g2" });
  });

  it("poisons the client on an uncorrelated response", async () => {
    const worker = new MockWorker();
    const client = new WorkerRpcClient(worker, PRODUCTION_RPC_PROTOCOL, 1_000);
    const pending = client.request({ operation: "status" });
    worker.emitMessage(statusResponse(2));
    await expect(pending).rejects.toMatchObject({ code: "invalid_request" });
    await expect(client.request({ operation: "status" })).rejects.toMatchObject({
      code: "invalid_request",
      stage: "protocol",
    });
  });

  it("poisons the client when initialization claims the legacy source schema", async () => {
    const worker = new MockWorker();
    const client = new WorkerRpcClient(worker, PRODUCTION_RPC_PROTOCOL, 1_000);
    const pending = client.request({ operation: "initialize", vault_id: "active-vault" });
    worker.emitMessage({
      version: WORKER_PROTOCOL_VERSION,
      id: 1,
      operation: "initialize",
      ok: true,
      result: {
        rustAbiVersion: 3,
        sourceSchemaVersion: 2,
        querySchemaVersion: 3,
        matchPlanSchemaVersion: 2,
        sqliteVersion: "3.53.0",
        fts5Enabled: 1,
      },
    });
    await expect(pending).rejects.toMatchObject({ code: "invalid_request" });
  });

  it("poisons the client on an uncorrelated build generation", async () => {
    const worker = new MockWorker();
    const client = new WorkerRpcClient(worker, PRODUCTION_RPC_PROTOCOL, 1_000);
    const pending = client.request({
      operation: "apply_source_changes",
      generation: "g1",
      next_generation: "g2",
      upserts: [],
      removals: [{ vault_id: "active", path: "old.md" }],
    });
    worker.emitMessage({
      version: WORKER_PROTOCOL_VERSION,
      id: 1,
      operation: "apply_source_changes",
      ok: true,
      result: {
        generation: "g3",
        documents: 0,
        chunks: 0,
        database_bytes: 0,
        database_byte_limit: 1,
        quarantined_sources: 0,
        quarantine_fields: [],
        source_format_counts: emptySourceFormatCounts(),
      },
    });
    await expect(pending).rejects.toMatchObject({ code: "invalid_request" });
  });

  // Export inherits the existing generation correlation rather than inventing
  // a new rule: the caller names the generation it believes is active, and an
  // envelope describing a different one is a protocol failure, not a result.
  it("poisons the client on an export envelope for another generation", async () => {
    const worker = new MockWorker();
    const client = new WorkerRpcClient(worker, PRODUCTION_RPC_PROTOCOL, 1_000);
    const identity = "d".repeat(64);
    const pending = client.request({
      operation: "export_generation",
      generation: "g1",
      cache_identity: identity,
    });
    expect(worker.posted[0]).toEqual({
      version: WORKER_PROTOCOL_VERSION,
      id: 1,
      operation: "export_generation",
      generation: "g1",
      cache_identity: identity,
    });

    worker.emitMessage({
      version: WORKER_PROTOCOL_VERSION,
      id: 1,
      operation: "export_generation",
      ok: true,
      result: exportResult("g2", identity),
    });
    await expect(pending).rejects.toMatchObject({ code: "invalid_request" });
    await expect(client.request({ operation: "status" })).rejects.toMatchObject({
      code: "invalid_request",
      stage: "protocol",
    });
  });

  it("resolves an export envelope correlated to the requested generation", async () => {
    const worker = new MockWorker();
    const client = new WorkerRpcClient(worker, PRODUCTION_RPC_PROTOCOL, 1_000);
    const identity = "e".repeat(64);
    const pending = client.request({
      operation: "export_generation",
      generation: "g1",
      cache_identity: identity,
    });
    worker.emitMessage({
      version: WORKER_PROTOCOL_VERSION,
      id: 1,
      operation: "export_generation",
      ok: true,
      result: exportResult("g1", identity),
    });
    await expect(pending).resolves.toMatchObject({
      generation: "g1",
      cache_identity: identity,
    });
  });

  it("transfers only the restore image buffer and correlates its generation", async () => {
    const worker = new MockWorker();
    const client = new WorkerRpcClient(worker, PRODUCTION_RPC_PROTOCOL, 1_000);
    const command = restoreCommand();
    const buffer = command.bytes.buffer;
    const pending = client.request(command);
    expect(worker.transfers[0]).toEqual([buffer]);
    worker.emitMessage({
      version: WORKER_PROTOCOL_VERSION,
      id: 1,
      operation: "restore_generation",
      ok: true,
      result: {
        generation: "g1",
        documents: 1,
        chunks: 1,
        database_bytes: 1,
        database_byte_limit: 2,
        quarantined_sources: 0,
        quarantine_fields: [],
        source_format_counts: sourceFormatCounts(1),
        evictions: emptyRestoreEvictionReport(),
      },
    });
    await expect(pending).resolves.toMatchObject({ generation: "g1" });
  });

  it.each([
    ["honours", true, 0],
    ["drops", false, 3],
  ])("%s the inbound restore transfer list", async (_name, honourTransfer, remainingBytes) => {
    const worker = new MockWorker();
    worker.postMessage = (message, transfer) => {
      worker.posted.push((honourTransfer
        ? structuredClone(message, { transfer: transfer as Transferable[] })
        : structuredClone(message)) as WorkerRequest);
    };
    const client = new WorkerRpcClient(worker, PRODUCTION_RPC_PROTOCOL, 1_000);
    const command = restoreCommand();
    const pending = client.request(command);
    expect(command.bytes.byteLength).toBe(remainingBytes);
    worker.emitMessage({
      version: WORKER_PROTOCOL_VERSION,
      id: 1,
      operation: "restore_generation",
      ok: true,
      result: {
        generation: "g1",
        documents: 1,
        chunks: 1,
        database_bytes: 1,
        database_byte_limit: 2,
        quarantined_sources: 0,
        quarantine_fields: [],
        source_format_counts: sourceFormatCounts(1),
        evictions: emptyRestoreEvictionReport(),
      },
    });
    await expect(pending).resolves.toMatchObject({ generation: "g1" });
  });

  it("poisons the client on a restore result for another generation", async () => {
    const worker = new MockWorker();
    const client = new WorkerRpcClient(worker, PRODUCTION_RPC_PROTOCOL, 1_000);
    const pending = client.request(restoreCommand("g1"));
    worker.emitMessage({
      version: WORKER_PROTOCOL_VERSION,
      id: 1,
      operation: "restore_generation",
      ok: true,
      result: {
        generation: "g2",
        documents: 1,
        chunks: 1,
        database_bytes: 1,
        database_byte_limit: 2,
        quarantined_sources: 0,
        quarantine_fields: [],
        source_format_counts: sourceFormatCounts(1),
        evictions: emptyRestoreEvictionReport(),
      },
    });
    await expect(pending).rejects.toMatchObject({ code: "invalid_request" });
  });

  it("transfers only checkpoint restore bytes and correlates staging generations", async () => {
    const worker = new MockWorker();
    const client = new WorkerRpcClient(worker, PRODUCTION_RPC_PROTOCOL, 1_000);
    const command = checkpointRestoreCommand();
    const pending = client.request(command);
    expect(worker.transfers[0]).toEqual([command.bytes.buffer]);
    worker.emitMessage({
      version: WORKER_PROTOCOL_VERSION,
      id: 1,
      operation: "restore_initial_build_checkpoint",
      ok: true,
      result: { ...checkpointRestoreResult(), evictions: emptyRestoreEvictionReport() },
    });
    await expect(pending).resolves.toMatchObject({
      generation: "g1",
      publication: "initial_staging",
      searchable: false,
    });
  });

  it("poisons the client on an uncorrelated checkpoint generation", async () => {
    const worker = new MockWorker();
    const client = new WorkerRpcClient(worker, PRODUCTION_RPC_PROTOCOL, 1_000);
    const pending = client.request(checkpointRestoreCommand("g1"));
    worker.emitMessage({
      version: WORKER_PROTOCOL_VERSION,
      id: 1,
      operation: "restore_initial_build_checkpoint",
      ok: true,
      result: { ...checkpointRestoreResult("g2"), evictions: emptyRestoreEvictionReport() },
    });
    await expect(pending).rejects.toMatchObject({ code: "invalid_request" });
  });

  it("correlates checkpoint export and staging reconciliation operations", async () => {
    const worker = new MockWorker();
    const client = new WorkerRpcClient(worker, PRODUCTION_RPC_PROTOCOL, 1_000);
    const identity = "d".repeat(64);
    const exportPending = client.request({
      operation: "export_initial_build_checkpoint",
      generation: "g1",
      cache_identity: identity,
      cursor: CHECKPOINT_CURSOR,
    });
    worker.emitMessage({
      version: WORKER_PROTOCOL_VERSION,
      id: 1,
      operation: "export_initial_build_checkpoint",
      ok: true,
      result: {
        ...checkpointRestoreResult(),
        checkpoint_record_version: INITIAL_BUILD_CHECKPOINT_RECORD_VERSION,
        checkpoint_image_version: INITIAL_BUILD_CHECKPOINT_IMAGE_VERSION,
        bytes: new Uint8Array([1, 2, 3]),
        blob_byte_length: 3,
        blob_sha256: "a".repeat(64),
        protocol_version: WORKER_PROTOCOL_VERSION,
        cache_schema_version: CACHE_SCHEMA_VERSION,
        chunking_version: 1,
        sqlite_version: "3.53.0",
        sqlite_wasm_sha256: "b".repeat(64),
        rust_wasm_sha256: "c".repeat(64),
        plugin_id: "kwiry-search",
        plugin_version: "0.1.0",
        cache_identity: identity,
        source_policy_hash: "e".repeat(64),
      },
    });
    await expect(exportPending).resolves.toMatchObject({ generation: "g1" });

    const planPending = client.request({
      operation: "plan_initial_build_checkpoint_reconciliation",
      generation: "g1",
      vault_id: "active-vault",
      current_sources: [],
    });
    worker.emitMessage({
      version: WORKER_PROTOCOL_VERSION,
      id: 2,
      operation: "plan_initial_build_checkpoint_reconciliation",
      ok: true,
      result: {
        generation: "g1",
        publication: "initial_staging",
        searchable: false,
        unchanged: [],
        audit: [],
        refresh: [],
        remove: [],
        stored_source_count: 0,
        matched_source_count: 0,
      },
    });
    await expect(planPending).resolves.toMatchObject({ generation: "g1" });
  });

  it("refuses private operations when no extension is installed", async () => {
    const worker = new MockWorker();
    const client = new WorkerRpcClient(worker, PRODUCTION_RPC_PROTOCOL, 1_000);

    await expect(client.request({
      operation: INTERNAL_D5C_COMPARE_OPERATION,
      generation: "g1",
      revision: null,
      query: "needle",
      limit: 20,
      query_time_epoch_seconds: "2000000000",
    })).rejects.toMatchObject({ code: "invalid_request" });
    expect(worker.posted).toEqual([]);
  });

  it("routes public and private operations through one monotonic request sequence", async () => {
    const worker = new MockWorker();
    const client = new WorkerRpcClient(
      worker,
      D5C_OWNER_RPC_PROTOCOL,
      1_000,
      D5C_RPC_EXTENSION,
    );
    const status = client.request({ operation: "status" });
    const comparison = client.request({
      operation: INTERNAL_D5C_COMPARE_OPERATION,
      generation: "g1",
      revision: 2,
      query: "needle",
      limit: 20,
      query_time_epoch_seconds: "2000000000",
    });

    expect(worker.posted).toEqual([
      { version: WORKER_PROTOCOL_VERSION, id: 1, operation: "status" },
      {
        version: WORKER_PROTOCOL_VERSION,
        id: 2,
        operation: INTERNAL_D5C_COMPARE_OPERATION,
        generation: "g1",
        revision: 2,
        query: "needle",
        limit: 20,
        query_time_epoch_seconds: "2000000000",
      },
    ]);
    worker.emitMessage(statusResponse(1));
    worker.emitMessage(compareResponse(2, "g1", 2, "initial_staging"));

    await expect(status).resolves.toMatchObject({ phase: "building" });
    await expect(comparison).resolves.toMatchObject({
      generation: "g1",
      publication: "initial_staging",
      revision: 2,
      text_order: [0],
      balanced_order: [0],
    });
    expect(client.pendingCount).toBe(0);
  });

  it("poisons the shared client on an uncorrelated private revision", async () => {
    const worker = new MockWorker();
    const client = new WorkerRpcClient(
      worker,
      D5C_OWNER_RPC_PROTOCOL,
      1_000,
      D5C_RPC_EXTENSION,
    );
    const comparison = client.request({
      operation: INTERNAL_D5C_COMPARE_OPERATION,
      generation: "g1",
      revision: 2,
      query: "needle",
      limit: 20,
      query_time_epoch_seconds: "2000000000",
    });
    worker.emitMessage(compareResponse(1, "g1", 3, "initial_staging"));

    await expect(comparison).rejects.toMatchObject({ code: "invalid_request" });
    await expect(client.request({ operation: "status" })).rejects.toMatchObject({
      code: "invalid_request",
      stage: "protocol",
    });
  });

  it("poisons the shared client on a malformed private response", async () => {
    const worker = new MockWorker();
    const client = new WorkerRpcClient(
      worker,
      D5C_OWNER_RPC_PROTOCOL,
      1_000,
      D5C_RPC_EXTENSION,
    );
    const comparison = client.request({
      operation: INTERNAL_D5C_COMPARE_OPERATION,
      generation: "g1",
      revision: null,
      query: "needle",
      limit: 20,
      query_time_epoch_seconds: "2000000000",
    });
    const malformed = compareResponse(1, "g1", null, "active") as {
      result: { evidence?: unknown };
    };
    malformed.result.evidence = { points: 99 };
    worker.emitMessage(malformed);

    await expect(comparison).rejects.toMatchObject({ code: "invalid_request" });
  });

  it("uses a per-request timeout override without changing the ordinary deadline", async () => {
    vi.useFakeTimers();
    try {
      const worker = new MockWorker();
      const client = new WorkerRpcClient(worker, PRODUCTION_RPC_PROTOCOL, 10);
      let overrideSettled = false;
      const overridden = client.request({ operation: "status" }, 50);
      void overridden.finally(() => {
        overrideSettled = true;
      });

      await vi.advanceTimersByTimeAsync(11);
      expect(overrideSettled).toBe(false);
      worker.emitMessage(statusResponse(1));
      await expect(overridden).resolves.toMatchObject({ phase: "building" });

      const ordinary = client.request({ operation: "status" });
      const ordinaryRejected = expect(ordinary).rejects.toMatchObject({ code: "timeout" });
      await vi.advanceTimersByTimeAsync(11);
      await ordinaryRejected;
    } finally {
      vi.useRealTimers();
    }
  });

  it("rejects later work as disposed after an explicit stop", async () => {
    const worker = new MockWorker();
    const client = new WorkerRpcClient(worker, PRODUCTION_RPC_PROTOCOL, 1_000);

    client.stop();

    await expect(client.request({ operation: "status" })).rejects.toMatchObject({
      code: "disposed",
    });
  });

  it("rejects pending work on crashes and preserves that cause for later work", async () => {
    const worker = new MockWorker();
    const client = new WorkerRpcClient(worker, PRODUCTION_RPC_PROTOCOL, 1_000);
    const pending = client.request({ operation: "status" });
    worker.emitError();
    await expect(pending).rejects.toMatchObject({ code: "worker_crashed" });
    await expect(client.request({ operation: "status" })).rejects.toMatchObject({
      code: "worker_crashed",
    });
  });

  it("poisons every pending request on timeout", async () => {
    vi.useFakeTimers();
    try {
      const worker = new MockWorker();
      const client = new WorkerRpcClient(worker, PRODUCTION_RPC_PROTOCOL, 10);
      const first = client.request({ operation: "status" });
      const second = client.request({ operation: "search", query: "query", limit: 20 });
      const firstRejected = expect(first).rejects.toMatchObject({ code: "timeout" });
      const secondRejected = expect(second).rejects.toMatchObject({ code: "timeout" });
      await vi.advanceTimersByTimeAsync(11);
      await Promise.all([firstRejected, secondRejected]);
      expect(client.pendingCount).toBe(0);
      await expect(client.request({ operation: "status" })).rejects.toMatchObject({
        code: "timeout",
      });
    } finally {
      vi.useRealTimers();
    }
  });
});
