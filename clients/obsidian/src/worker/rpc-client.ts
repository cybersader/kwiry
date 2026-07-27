// SPDX-FileCopyrightText: 2026 cybersader
// SPDX-License-Identifier: GPL-3.0-only

import {
  MAX_PENDING_REQUESTS,
  WORKER_PROTOCOL_VERSION,
  WORKER_REQUEST_TIMEOUT_MS,
  type ReconciliationSourceMetadata,
  type RestoreGenerationInput,
  type SourceUpsert,
  type SourceRemoval,
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
  | { operation: "add_source_batch"; generation: string; sources: SourceUpsert[] }
  | {
      operation: "apply_source_changes";
      generation: string;
      next_generation: string | null;
      upserts: SourceUpsert[];
      removals: SourceRemoval[];
    }
  | { operation: "commit_build"; generation: string }
  | { operation: "abort_build"; generation: string }
  | { operation: "export_generation"; generation: string; cache_identity: string }
  | ({ operation: "restore_generation" } & RestoreGenerationInput)
  | {
      operation: "plan_reconciliation";
      generation: string;
      vault_id: string;
      current_sources: ReconciliationSourceMetadata[];
    }
  | { operation: "search"; query: string; limit: number }
  | { operation: "status" }
  | { operation: "dispose" };

export interface WorkerLike {
  postMessage(message: WorkerRequest, transfer?: Transferable[]): void;
  terminate(): void;
  addEventListener(type: "message", listener: (event: MessageEvent<unknown>) => void): void;
  addEventListener(type: "error" | "messageerror", listener: (event: Event) => void): void;
  removeEventListener(type: "message", listener: (event: MessageEvent<unknown>) => void): void;
  removeEventListener(type: "error" | "messageerror", listener: (event: Event) => void): void;
}

interface PendingRequest {
  operation: WorkerOperation;
  expectedGeneration: string | null;
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
      this.poison(new WorkerRpcError(fixedWorkerError(
        "invalid_request",
        "protocol",
        "Worker returned an invalid response.",
        false,
      )));
      return;
    }
    const pending = this.pending.get(event.data.id);
    if (!pending || pending.operation !== event.data.operation) {
      this.poison(new WorkerRpcError(fixedWorkerError(
        "invalid_request",
        "protocol",
        "Worker returned an uncorrelated response.",
        false,
      )));
      return;
    }
    if (event.data.ok
      && pending.expectedGeneration !== null
      && resultGeneration(event.data.result) !== pending.expectedGeneration) {
      this.poison(new WorkerRpcError(fixedWorkerError(
        "invalid_request",
        "protocol",
        "Worker returned an uncorrelated generation.",
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
    this.poison(new WorkerRpcError(fixedWorkerError(
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
        this.poison(new WorkerRpcError(fixedWorkerError(
          "timeout",
          "lifecycle",
          "In-plugin search Worker timed out.",
          true,
        )));
      }, this.timeoutMs);
      this.pending.set(id, {
        operation: command.operation,
        expectedGeneration: expectedGeneration(command),
        resolve,
        reject,
        timeout,
      });
      try {
        const transfer = requestTransferList(command);
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

function requestTransferList(command: WorkerCommand): Transferable[] {
  if (command.operation !== "restore_generation") return [];
  return [command.bytes.buffer as ArrayBuffer];
}

function expectedGeneration(command: WorkerCommand): string | null {
  switch (command.operation) {
    case "begin_build":
    case "add_source_batch":
    case "commit_build":
    case "abort_build":
    case "restore_generation":
    case "plan_reconciliation":
    // The caller names the generation it believes is active, so an export that
    // came back describing a different one is uncorrelated and poisons the
    // client, exactly as a build result would.
    case "export_generation":
      return command.generation;
    case "apply_source_changes":
      return command.next_generation ?? command.generation;
    case "initialize":
    case "search":
    case "status":
    case "dispose":
      return null;
  }
}

function resultGeneration(result: WorkerResult): string | null {
  if (typeof result !== "object" || result === null || !("generation" in result)) return null;
  return typeof result.generation === "string" ? result.generation : null;
}
