// SPDX-FileCopyrightText: 2026 cybersader
// SPDX-License-Identifier: GPL-3.0-only

import {
  MAX_PENDING_REQUESTS,
  WORKER_PROTOCOL_VERSION,
  WORKER_REQUEST_TIMEOUT_MS,
  type SourceInput,
  type WorkerError,
  type WorkerOperation,
  type WorkerRequest,
  type WorkerResult,
  fixedWorkerError,
  isWorkerResponse,
} from "./protocol";

export type WorkerCommand =
  | { operation: "initialize" }
  | { operation: "begin_build"; generation: string }
  | { operation: "add_source_batch"; generation: string; sources: SourceInput[] }
  | { operation: "commit_build"; generation: string }
  | { operation: "abort_build"; generation: string }
  | { operation: "search"; query: string; limit: number }
  | { operation: "status" }
  | { operation: "dispose" };

export interface WorkerLike {
  postMessage(message: WorkerRequest): void;
  terminate(): void;
  addEventListener(type: "message", listener: (event: MessageEvent<unknown>) => void): void;
  addEventListener(type: "error" | "messageerror", listener: (event: Event) => void): void;
  removeEventListener(type: "message", listener: (event: MessageEvent<unknown>) => void): void;
  removeEventListener(type: "error" | "messageerror", listener: (event: Event) => void): void;
}

interface PendingRequest {
  operation: WorkerOperation;
  resolve: (result: WorkerResult) => void;
  reject: (error: Error) => void;
  timeout: ReturnType<typeof setTimeout>;
}

export class WorkerRpcError extends Error {
  readonly code: WorkerError["code"];
  readonly stage: WorkerError["stage"];
  readonly retryable: boolean;

  constructor(error: WorkerError) {
    super(error.message);
    this.name = "WorkerRpcError";
    this.code = error.code;
    this.stage = error.stage;
    this.retryable = error.retryable;
  }
}

export class WorkerRpcClient {
  private readonly pending = new Map<number, PendingRequest>();
  private nextId = 1;
  private stopped = false;

  private readonly onMessage = (event: MessageEvent<unknown>): void => {
    if (!isWorkerResponse(event.data)) {
      this.failAll(new WorkerRpcError(fixedWorkerError(
        "invalid_request",
        "protocol",
        "Worker returned an invalid response.",
        false,
      )));
      return;
    }
    const pending = this.pending.get(event.data.id);
    if (!pending || pending.operation !== event.data.operation) {
      this.failAll(new WorkerRpcError(fixedWorkerError(
        "invalid_request",
        "protocol",
        "Worker returned an uncorrelated response.",
        false,
      )));
      return;
    }

    clearTimeout(pending.timeout);
    this.pending.delete(event.data.id);
    if (event.data.ok) pending.resolve(event.data.result);
    else pending.reject(new WorkerRpcError(event.data.error));
  };

  private readonly onWorkerFailure = (): void => {
    this.failAll(new WorkerRpcError(fixedWorkerError(
      "worker_crashed",
      "lifecycle",
      "In-plugin search Worker failed.",
      true,
    )));
  };

  constructor(
    private readonly worker: WorkerLike,
    private readonly timeoutMs = WORKER_REQUEST_TIMEOUT_MS,
  ) {
    worker.addEventListener("message", this.onMessage);
    worker.addEventListener("error", this.onWorkerFailure);
    worker.addEventListener("messageerror", this.onWorkerFailure);
  }

  get pendingCount(): number {
    return this.pending.size;
  }

  request(command: WorkerCommand): Promise<WorkerResult> {
    if (this.stopped) {
      return Promise.reject(new WorkerRpcError(fixedWorkerError(
        "disposed",
        "lifecycle",
        "In-plugin search Worker is disposed.",
        false,
      )));
    }
    if (this.pending.size >= MAX_PENDING_REQUESTS) {
      return Promise.reject(new WorkerRpcError(fixedWorkerError(
        "invalid_state",
        "protocol",
        "In-plugin search Worker queue is full.",
        true,
      )));
    }
    if (!Number.isSafeInteger(this.nextId) || this.nextId < 1) {
      return Promise.reject(new WorkerRpcError(fixedWorkerError(
        "invalid_state",
        "protocol",
        "In-plugin search Worker request IDs are exhausted.",
        false,
      )));
    }

    const id = this.nextId;
    this.nextId += 1;
    const request = {
      version: WORKER_PROTOCOL_VERSION,
      id,
      ...command,
    } as WorkerRequest;
    return new Promise<WorkerResult>((resolve, reject) => {
      const timeout = setTimeout(() => {
        this.pending.delete(id);
        reject(new WorkerRpcError(fixedWorkerError(
          "timeout",
          "lifecycle",
          "In-plugin search Worker timed out.",
          true,
        )));
      }, this.timeoutMs);
      this.pending.set(id, {
        operation: command.operation,
        resolve,
        reject,
        timeout,
      });
      try {
        this.worker.postMessage(request);
      } catch {
        clearTimeout(timeout);
        this.pending.delete(id);
        reject(new WorkerRpcError(fixedWorkerError(
          "worker_crashed",
          "lifecycle",
          "In-plugin search Worker failed.",
          true,
        )));
      }
    });
  }

  stop(error: Error = new WorkerRpcError(fixedWorkerError(
    "disposed",
    "lifecycle",
    "In-plugin search Worker is disposed.",
    false,
  ))): void {
    if (this.stopped) return;
    this.stopped = true;
    this.worker.removeEventListener("message", this.onMessage);
    this.worker.removeEventListener("error", this.onWorkerFailure);
    this.worker.removeEventListener("messageerror", this.onWorkerFailure);
    this.failAll(error);
  }

  private failAll(error: Error): void {
    for (const pending of this.pending.values()) {
      clearTimeout(pending.timeout);
      pending.reject(error);
    }
    this.pending.clear();
  }
}
