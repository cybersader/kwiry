// SPDX-FileCopyrightText: 2026 cybersader
// SPDX-License-Identifier: GPL-3.0-only

import { describe, expect, it, vi } from "vitest";

import type { WorkerRequest } from "../src/worker/protocol";
import type { WorkerLike } from "../src/worker/rpc-client";
import { InPluginWorkerSession } from "../src/worker/session";

class MockWorker implements WorkerLike {
  terminate = vi.fn();
  private readonly listeners = new Map<string, Set<(event: never) => void>>();

  postMessage(message: WorkerRequest): void {
    queueMicrotask(() => {
      const result = message.operation === "dispose"
        ? { closed: true }
        : {
            rustAbiVersion: 1,
            sourceSchemaVersion: 1,
            querySchemaVersion: 2,
            matchPlanSchemaVersion: 1,
            sqliteVersion: "3.53.0",
            fts5Enabled: 1,
          };
      for (const listener of this.listeners.get("message") ?? []) {
        listener({
          data: {
            version: 1,
            id: message.id,
            operation: message.operation,
            ok: true,
            result,
          },
        } as never);
      }
    });
  }

  addEventListener(type: "message" | "error" | "messageerror", listener: (event: never) => void): void {
    const listeners = this.listeners.get(type) ?? new Set();
    listeners.add(listener);
    this.listeners.set(type, listeners);
  }

  removeEventListener(type: "message" | "error" | "messageerror", listener: (event: never) => void): void {
    this.listeners.get(type)?.delete(listener);
  }
}

describe("InPluginWorkerSession", () => {
  it("terminates its Worker and revokes the Blob URL exactly once", async () => {
    const worker = new MockWorker();
    const revoke = vi.fn();
    const session = new InPluginWorkerSession(worker, "blob:kwiry", revoke, 1_000);

    await expect(session.initialize()).resolves.toMatchObject({ rustAbiVersion: 1 });
    await expect(session.dispose()).resolves.toEqual({ closed: true });
    session.forceDispose();

    expect(worker.terminate).toHaveBeenCalledTimes(1);
    expect(revoke).toHaveBeenCalledTimes(1);
    expect(revoke).toHaveBeenCalledWith("blob:kwiry");
  });

  it("force disposal rejects pending work", async () => {
    const worker = new MockWorker();
    worker.postMessage = vi.fn();
    const session = new InPluginWorkerSession(worker, "blob:kwiry", vi.fn(), 1_000);
    const pending = session.initialize();
    const rejected = expect(pending).rejects.toMatchObject({ code: "disposed" });
    session.forceDispose();
    await rejected;
  });
});
