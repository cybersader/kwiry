// SPDX-FileCopyrightText: 2026 cybersader
// SPDX-License-Identifier: GPL-3.0-only

import { describe, expect, it, vi } from "vitest";

import {
  WORKER_PROTOCOL_VERSION,
  type WorkerRequest,
  type WorkerResult,
} from "../src/worker/protocol";
import type { IndexWorkerPort } from "../src/backends/in-plugin-index-controller";
import type { WorkerLike } from "../src/worker/rpc-client";
import { InPluginWorkerSession } from "../src/worker/session";

class MockWorker implements WorkerLike {
  terminate = vi.fn();
  readonly posted: WorkerRequest[] = [];
  private readonly listeners = new Map<string, Set<(event: never) => void>>();

  postMessage(message: WorkerRequest): void {
    this.posted.push(message);
    queueMicrotask(() => {
      const result = resultFor(message);
      for (const listener of this.listeners.get("message") ?? []) {
        listener({
          data: {
            version: WORKER_PROTOCOL_VERSION,
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

function resultFor(message: WorkerRequest): WorkerResult {
  switch (message.operation) {
    case "initialize":
      return {
        rustAbiVersion: 1,
        sourceSchemaVersion: 1,
        querySchemaVersion: 2,
        matchPlanSchemaVersion: 1,
        sqliteVersion: "3.53.0",
        fts5Enabled: 1,
      };
    case "begin_build":
    case "add_source_batch":
    case "commit_build":
    case "abort_build":
      return { generation: message.generation, documents: 0, chunks: 0 };
    case "apply_source_changes":
      return {
        generation: message.next_generation ?? message.generation,
        documents: message.upserts.length,
        chunks: message.upserts.length,
      };
    case "search":
      return { generation: "g1", hits: [] };
    case "status":
      return {
        phase: "ready",
        searchable: true,
        active_generation: "g1",
        staging_generation: null,
        documents: 0,
        chunks: 0,
        dirty: false,
        rebuilding: false,
      };
    case "dispose":
      return { closed: true };
  }
}

describe("InPluginWorkerSession", () => {
  it("serializes active and staging source changes", async () => {
    const worker = new MockWorker();
    const session = new InPluginWorkerSession(worker, "blob:kwiry", vi.fn(), 1_000);
    const port: IndexWorkerPort = session;
    expect(port).toBe(session);

    await expect(session.applySourceChanges(
      "g1",
      "g2",
      [],
      [{ vault_id: "active", path: "old.md" }],
    )).resolves.toMatchObject({ generation: "g2" });
    await expect(session.applySourceChanges(
      "g3",
      null,
      [],
      [{ vault_id: "active", path: "old.md" }],
    )).resolves.toMatchObject({ generation: "g3" });

    expect(worker.posted).toEqual([
      {
        version: WORKER_PROTOCOL_VERSION,
        id: 1,
        operation: "apply_source_changes",
        generation: "g1",
        next_generation: "g2",
        upserts: [],
        removals: [{ vault_id: "active", path: "old.md" }],
      },
      {
        version: WORKER_PROTOCOL_VERSION,
        id: 2,
        operation: "apply_source_changes",
        generation: "g3",
        next_generation: null,
        upserts: [],
        removals: [{ vault_id: "active", path: "old.md" }],
      },
    ]);
  });

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
