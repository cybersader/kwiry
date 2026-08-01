// SPDX-FileCopyrightText: 2026 cybersader
// SPDX-License-Identifier: GPL-3.0-only

import {
  MAX_PENDING_REQUESTS,
  WORKER_PROTOCOL_VERSION,
  WORKER_REQUEST_TIMEOUT_MS,
  fixedWorkerError,
  type WorkerError,
} from "./protocol";

export interface RpcCommand {
  operation: string;
  [key: string]: unknown;
}

export interface RpcResponse {
  version: number;
  id: number;
  operation: string;
  ok: boolean;
  result?: unknown;
  error?: WorkerError;
}

export interface WorkerRpcProtocol {
  isOperation(operation: string): boolean;
  parseResponse(value: unknown): RpcResponse | null;
  expectedGeneration(command: RpcCommand): string | null;
  transferList(command: RpcCommand): Transferable[];
}

export interface WorkerRpcExtension {
  operation: string;
  parseResponse(value: unknown): RpcResponse | null;
  expectedGeneration(command: RpcCommand): string | null;
  matchesCorrelation(result: unknown, command: RpcCommand): boolean;
}

export interface WorkerLike {
  postMessage(message: unknown, transfer?: Transferable[]): void;
  terminate(): void;
  addEventListener(type: "message", listener: (event: MessageEvent<unknown>) => void): void;
  addEventListener(type: "error" | "messageerror", listener: (event: Event) => void): void;
  removeEventListener(type: "message", listener: (event: MessageEvent<unknown>) => void): void;
  removeEventListener(type: "error" | "messageerror", listener: (event: Event) => void): void;
}

interface PendingRequest {
  command: RpcCommand;
  expectedGeneration: string | null;
  resolve: (result: unknown) => void;
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
    const responseId = positiveResponseId(event.data);
    const pending = responseId === null ? undefined : this.pending.get(responseId);
    const extension = pending?.command.operation === this.extension?.operation
      ? this.extension
      : null;
    const response: RpcResponse | null = extension
      ? extension.parseResponse(event.data)
      : this.protocol.parseResponse(event.data);
    if (response === null) {
      this.poison(new WorkerRpcError(fixedWorkerError(
        "invalid_request",
        "protocol",
        "Worker returned an invalid response.",
        false,
      )));
      return;
    }
    if (!pending || pending.command.operation !== response.operation) {
      this.poison(new WorkerRpcError(fixedWorkerError(
        "invalid_request",
        "protocol",
        "Worker returned an uncorrelated response.",
        false,
      )));
      return;
    }
    if (response.ok
      && pending.expectedGeneration !== null
      && resultGeneration(response.result) !== pending.expectedGeneration) {
      this.poison(new WorkerRpcError(fixedWorkerError(
        "invalid_request",
        "protocol",
        "Worker returned an uncorrelated generation.",
        false,
      )));
      return;
    }
    if (response.ok
      && extension
      && !extension.matchesCorrelation(response.result, pending.command)) {
      this.poison(new WorkerRpcError(fixedWorkerError(
        "invalid_request",
        "protocol",
        "Worker returned an uncorrelated extension response.",
        false,
      )));
      return;
    }

    clearTimeout(pending.timeout);
    this.pending.delete(response.id);
    if (response.ok) pending.resolve(response.result);
    else pending.reject(new WorkerRpcError(response.error!));
  };

  private readonly onWorkerFailure = (): void => {
    this.poison(new WorkerRpcError(fixedWorkerError(
      "worker_crashed",
      "lifecycle",
      "In-plugin search Worker failed.",
      true,
    )));
  };

  constructor(
    private readonly worker: WorkerLike,
    private readonly protocol: WorkerRpcProtocol,
    private readonly timeoutMs = WORKER_REQUEST_TIMEOUT_MS,
    private readonly extension: WorkerRpcExtension | null = null,
  ) {
    worker.addEventListener("message", this.onMessage);
    worker.addEventListener("error", this.onWorkerFailure);
    worker.addEventListener("messageerror", this.onWorkerFailure);
  }

  get pendingCount(): number {
    return this.pending.size;
  }

  request(command: RpcCommand): Promise<unknown> {
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
    const isExtension = command.operation === this.extension?.operation;
    if (!isExtension && !this.protocol.isOperation(command.operation)) {
      return Promise.reject(new WorkerRpcError(fixedWorkerError(
        "invalid_request",
        "protocol",
        "In-plugin search Worker operation is unavailable.",
        false,
      )));
    }

    const id = this.nextId;
    this.nextId += 1;
    const request = {
      version: WORKER_PROTOCOL_VERSION,
      id,
      ...command,
    };
    return new Promise<unknown>((resolve, reject) => {
      const timeout = setTimeout(() => {
        this.poison(new WorkerRpcError(fixedWorkerError(
          "timeout",
          "lifecycle",
          "In-plugin search Worker timed out.",
          true,
        )));
      }, this.timeoutMs);
      this.pending.set(id, {
        command,
        expectedGeneration: isExtension
          ? this.extension!.expectedGeneration(command)
          : this.protocol.expectedGeneration(command),
        resolve,
        reject,
        timeout,
      });
      try {
        const transfer = this.protocol.transferList(command);
        if (transfer.length === 0) this.worker.postMessage(request);
        else this.worker.postMessage(request, transfer);
      } catch {
        this.poison(new WorkerRpcError(fixedWorkerError(
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
    this.poison(error);
  }

  private poison(error: Error): void {
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

function positiveResponseId(value: unknown): number | null {
  if (typeof value !== "object" || value === null || !("id" in value)) return null;
  const id = (value as { id?: unknown }).id;
  return Number.isSafeInteger(id) && Number(id) > 0 ? Number(id) : null;
}

function resultGeneration(result: unknown): string | null {
  if (typeof result !== "object" || result === null || !("generation" in result)) return null;
  return typeof result.generation === "string" ? result.generation : null;
}
