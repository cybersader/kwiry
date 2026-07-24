// SPDX-License-Identifier: GPL-3.0-only
import { describe, expect, test, vi } from "vitest";

import {
  PROTOCOL_VERSION,
  ProbeRpcClient,
  ProbeRpcError,
  isProbeResponse,
  parseProbeRequest,
  type ProbeRequest,
  type ProbeResponse,
  type WorkerLike,
} from "../src/protocol";
import { ProbeSession } from "../src/session";

class FakeWorker implements WorkerLike {
  readonly requests: ProbeRequest[] = [];
  terminateCount = 0;
  private readonly listeners = new Map<string, Set<(event: never) => void>>();

  postMessage(message: ProbeRequest): void {
    this.requests.push(message);
  }

  terminate(): void {
    this.terminateCount += 1;
  }

  addEventListener(type: "message" | "error" | "messageerror", listener: ((event: MessageEvent<unknown>) => void) | ((event: Event) => void)): void {
    const listeners = this.listeners.get(type) ?? new Set();
    listeners.add(listener as (event: never) => void);
    this.listeners.set(type, listeners);
  }

  removeEventListener(type: "message" | "error" | "messageerror", listener: ((event: MessageEvent<unknown>) => void) | ((event: Event) => void)): void {
    this.listeners.get(type)?.delete(listener as (event: never) => void);
  }

  emitMessage(response: unknown): void {
    for (const listener of this.listeners.get("message") ?? []) {
      listener({ data: response } as never);
    }
  }

  emitFailure(type: "error" | "messageerror" = "error"): void {
    for (const listener of this.listeners.get(type) ?? []) {
      listener(new Event(type) as never);
    }
  }
}

class AutoWorker extends FakeWorker {
  override postMessage(message: ProbeRequest): void {
    super.postMessage(message);
    const result = message.operation === "initialize"
      ? {
          sqliteVersion: "3.53.0",
          fts5Enabled: 1,
          wasmBytes: 864752,
          wasmSha256: "02d7e48164395fa68f81c6ec33e9da5461be397dc57602ac0cd89b4bbba1d312",
          database: ":memory:",
          networkAttempts: 0,
          persistenceAttempts: 0,
          helperWorkerAttempts: 0,
        }
      : message.operation === "probe"
        ? {
            expectedTitle: "Synthetic Alpha",
            finiteScore: true,
            snippetMarked: true,
            rollbackAbsent: true,
            integrityPassed: true,
          }
        : { closed: true };
    queueMicrotask(() => this.emitMessage({
      version: PROTOCOL_VERSION,
      id: message.id,
      operation: message.operation,
      ok: true,
      result,
    }));
  }
}

describe("protocol guards", () => {
  test("accepts only the versioned allowlisted request shape", () => {
    expect(parseProbeRequest({ version: 1, id: 1, operation: "initialize" })).toEqual({
      version: 1,
      id: 1,
      operation: "initialize",
    });
    expect(parseProbeRequest({ version: 2, id: 1, operation: "initialize" })).toMatchObject({ code: "protocol_mismatch" });
    expect(parseProbeRequest({ version: 1, id: 0, operation: "probe" })).toMatchObject({ code: "invalid_request" });
    expect(parseProbeRequest({ version: 1, id: 1, operation: "exec", sql: "SELECT 1" })).toMatchObject({ code: "invalid_request" });
    expect(parseProbeRequest({ version: 1, id: 1, operation: "probe", payload: "x".repeat(5_000) })).toMatchObject({ code: "invalid_request" });
  });

  test("validates response envelopes", () => {
    expect(isProbeResponse({ version: 1, id: 1, operation: "dispose", ok: true, result: { closed: true } })).toBe(true);
    expect(isProbeResponse({ version: 1, id: 1, operation: "dispose", ok: false, error: { code: "disposed", stage: "close", message: "Disposed." } })).toBe(true);
    expect(isProbeResponse({ version: 1, id: 1, operation: "dispose", ok: true })).toBe(false);
  });
});

describe("RPC lifecycle", () => {
  test("correlates out-of-order responses", async () => {
    const worker = new FakeWorker();
    const client = new ProbeRpcClient(worker, 1_000);
    const initialize = client.request("initialize");
    const probe = client.request("probe");

    const [initializeRequest, probeRequest] = worker.requests;
    worker.emitMessage({
      version: 1,
      id: probeRequest.id,
      operation: "probe",
      ok: true,
      result: {
        expectedTitle: "Synthetic Alpha",
        finiteScore: true,
        snippetMarked: true,
        rollbackAbsent: true,
        integrityPassed: true,
      },
    } satisfies ProbeResponse);
    worker.emitMessage({
      version: 1,
      id: initializeRequest.id,
      operation: "initialize",
      ok: true,
      result: {
        sqliteVersion: "3.53.0",
        fts5Enabled: 1,
        wasmBytes: 864752,
        wasmSha256: "02d7e48164395fa68f81c6ec33e9da5461be397dc57602ac0cd89b4bbba1d312",
        database: ":memory:",
        networkAttempts: 0,
        persistenceAttempts: 0,
        helperWorkerAttempts: 0,
      },
    } satisfies ProbeResponse);

    await expect(probe).resolves.toMatchObject({ expectedTitle: "Synthetic Alpha" });
    await expect(initialize).resolves.toMatchObject({ sqliteVersion: "3.53.0" });
    expect(client.pendingCount).toBe(0);
    client.stop();
  });

  test("rejects all pending work on malformed messages and Worker failures", async () => {
    const malformedWorker = new FakeWorker();
    const malformedClient = new ProbeRpcClient(malformedWorker, 1_000);
    const malformed = malformedClient.request("initialize");
    malformedWorker.emitMessage({ nope: true });
    await expect(malformed).rejects.toMatchObject({ code: "invalid_request" });

    const failedWorker = new FakeWorker();
    const failedClient = new ProbeRpcClient(failedWorker, 1_000);
    const failed = failedClient.request("probe");
    failedWorker.emitFailure();
    await expect(failed).rejects.toMatchObject({ code: "worker_crashed" });
  });

  test("times out and rejects use after stop", async () => {
    vi.useFakeTimers();
    const worker = new FakeWorker();
    const client = new ProbeRpcClient(worker, 25);
    const request = client.request("initialize");
    const timedOut = expect(request).rejects.toMatchObject({ code: "timeout" });
    await vi.advanceTimersByTimeAsync(25);
    await timedOut;
    client.stop();
    await expect(client.request("probe")).rejects.toBeInstanceOf(ProbeRpcError);
    vi.useRealTimers();
  });

  test("session cleanup terminates and revokes exactly once", async () => {
    const worker = new AutoWorker();
    const revoked: string[] = [];
    const session = new ProbeSession(worker, "blob:synthetic", (url) => revoked.push(url), 1_000);

    await expect(session.run()).resolves.toMatchObject({
      sqliteVersion: "3.53.0",
      syntheticQueryPassed: true,
      closed: true,
    });
    session.forceDispose();

    expect(worker.requests.map((request) => request.operation)).toEqual(["initialize", "probe", "dispose"]);
    expect(worker.terminateCount).toBe(1);
    expect(revoked).toEqual(["blob:synthetic"]);
  });
});
