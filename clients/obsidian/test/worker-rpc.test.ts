// SPDX-FileCopyrightText: 2026 cybersader
// SPDX-License-Identifier: GPL-3.0-only

import { describe, expect, it, vi } from "vitest";

import type { WorkerRequest } from "../src/worker/protocol";
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

describe("WorkerRpcClient", () => {
  it("correlates responses and removes completed requests", async () => {
    const worker = new MockWorker();
    const client = new WorkerRpcClient(worker, 1_000);
    const pending = client.request({ operation: "status" });
    expect(worker.posted[0]).toEqual({ version: 1, id: 1, operation: "status" });
    worker.emitMessage({
      version: 1,
      id: 1,
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
    });
    await expect(pending).resolves.toMatchObject({ phase: "building" });
    expect(client.pendingCount).toBe(0);
  });

  it("rejects all pending requests on an uncorrelated response", async () => {
    const worker = new MockWorker();
    const client = new WorkerRpcClient(worker, 1_000);
    const pending = client.request({ operation: "status" });
    worker.emitMessage({
      version: 1,
      id: 2,
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
    });
    await expect(pending).rejects.toMatchObject({ code: "invalid_request" });
  });

  it("rejects pending work on crashes and all later work after stop", async () => {
    const worker = new MockWorker();
    const client = new WorkerRpcClient(worker, 1_000);
    const pending = client.request({ operation: "status" });
    worker.emitError();
    await expect(pending).rejects.toMatchObject({ code: "worker_crashed" });
    client.stop();
    await expect(client.request({ operation: "status" })).rejects.toMatchObject({
      code: "disposed",
    });
  });

  it("times out without retaining the pending request", async () => {
    vi.useFakeTimers();
    try {
      const worker = new MockWorker();
      const client = new WorkerRpcClient(worker, 10);
      const pending = client.request({ operation: "status" });
      const rejected = expect(pending).rejects.toMatchObject({ code: "timeout" });
      await vi.advanceTimersByTimeAsync(11);
      await rejected;
      expect(client.pendingCount).toBe(0);
    } finally {
      vi.useRealTimers();
    }
  });
});
