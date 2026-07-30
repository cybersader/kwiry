// SPDX-FileCopyrightText: 2026 cybersader
// SPDX-License-Identifier: GPL-3.0-only

import { describe, expect, it, vi } from "vitest";

import {
  CACHE_SCHEMA_VERSION,
  WORKER_PROTOCOL_VERSION,
  type WorkerRequest,
} from "../src/worker/protocol";
import { WorkerRpcClient, type WorkerLike } from "../src/worker/rpc-client";

class MockWorker implements WorkerLike {
  readonly posted: WorkerRequest[] = [];
  readonly transfers: (Transferable[] | undefined)[] = [];
  readonly listeners = new Map<string, Set<(event: never) => void>>();
  terminate = vi.fn();

  postMessage(message: WorkerRequest, transfer?: Transferable[]): void {
    this.posted.push(message);
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
      dirty: true,
      rebuilding: false,
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
    expected_cache_identity: identity,
  };
}

describe("WorkerRpcClient", () => {
  it("correlates responses and removes completed requests", async () => {
    const worker = new MockWorker();
    const client = new WorkerRpcClient(worker, 1_000);
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
    const client = new WorkerRpcClient(worker, 1_000);
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
      },
    });
    await expect(pending).resolves.toMatchObject({ generation: "g2" });
  });

  it("poisons the client on an uncorrelated response", async () => {
    const worker = new MockWorker();
    const client = new WorkerRpcClient(worker, 1_000);
    const pending = client.request({ operation: "status" });
    worker.emitMessage(statusResponse(2));
    await expect(pending).rejects.toMatchObject({ code: "invalid_request" });
    await expect(client.request({ operation: "status" })).rejects.toMatchObject({
      code: "disposed",
    });
  });

  it("poisons the client when initialization claims the legacy source schema", async () => {
    const worker = new MockWorker();
    const client = new WorkerRpcClient(worker, 1_000);
    const pending = client.request({ operation: "initialize", vault_id: "active-vault" });
    worker.emitMessage({
      version: WORKER_PROTOCOL_VERSION,
      id: 1,
      operation: "initialize",
      ok: true,
      result: {
        rustAbiVersion: 2,
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
    const client = new WorkerRpcClient(worker, 1_000);
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
      },
    });
    await expect(pending).rejects.toMatchObject({ code: "invalid_request" });
  });

  // Export inherits the existing generation correlation rather than inventing
  // a new rule: the caller names the generation it believes is active, and an
  // envelope describing a different one is a protocol failure, not a result.
  it("poisons the client on an export envelope for another generation", async () => {
    const worker = new MockWorker();
    const client = new WorkerRpcClient(worker, 1_000);
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
      code: "disposed",
    });
  });

  it("resolves an export envelope correlated to the requested generation", async () => {
    const worker = new MockWorker();
    const client = new WorkerRpcClient(worker, 1_000);
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
    const client = new WorkerRpcClient(worker, 1_000);
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
      worker.posted.push(honourTransfer
        ? structuredClone(message, { transfer: transfer as Transferable[] })
        : structuredClone(message));
    };
    const client = new WorkerRpcClient(worker, 1_000);
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
      },
    });
    await expect(pending).resolves.toMatchObject({ generation: "g1" });
  });

  it("poisons the client on a restore result for another generation", async () => {
    const worker = new MockWorker();
    const client = new WorkerRpcClient(worker, 1_000);
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
      },
    });
    await expect(pending).rejects.toMatchObject({ code: "invalid_request" });
  });

  it("rejects pending work on crashes and all later work", async () => {
    const worker = new MockWorker();
    const client = new WorkerRpcClient(worker, 1_000);
    const pending = client.request({ operation: "status" });
    worker.emitError();
    await expect(pending).rejects.toMatchObject({ code: "worker_crashed" });
    await expect(client.request({ operation: "status" })).rejects.toMatchObject({
      code: "disposed",
    });
  });

  it("poisons every pending request on timeout", async () => {
    vi.useFakeTimers();
    try {
      const worker = new MockWorker();
      const client = new WorkerRpcClient(worker, 10);
      const first = client.request({ operation: "status" });
      const second = client.request({ operation: "search", query: "query", limit: 20 });
      const firstRejected = expect(first).rejects.toMatchObject({ code: "timeout" });
      const secondRejected = expect(second).rejects.toMatchObject({ code: "timeout" });
      await vi.advanceTimersByTimeAsync(11);
      await Promise.all([firstRejected, secondRejected]);
      expect(client.pendingCount).toBe(0);
      await expect(client.request({ operation: "status" })).rejects.toMatchObject({
        code: "disposed",
      });
    } finally {
      vi.useRealTimers();
    }
  });
});
