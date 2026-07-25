// SPDX-FileCopyrightText: 2026 cybersader
// SPDX-License-Identifier: GPL-3.0-only

import { describe, expect, it, vi } from "vitest";

import { WORKER_PROTOCOL_VERSION, type WorkerRequest } from "../src/worker/protocol";
import { WorkerRpcClient, type WorkerLike } from "../src/worker/rpc-client";

class MockWorker implements WorkerLike {
  readonly posted: WorkerRequest[] = [];
  readonly listeners = new Map<string, Set<(event: never) => void>>();
  terminate = vi.fn();

  postMessage(message: WorkerRequest): void {
    this.posted.push(message);
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
      dirty: true,
      rebuilding: false,
    },
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
      result: { generation: "g2", documents: 0, chunks: 0 },
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
      result: { generation: "g3", documents: 0, chunks: 0 },
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
