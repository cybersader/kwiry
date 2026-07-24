// SPDX-License-Identifier: GPL-3.0-only
export const PROTOCOL_VERSION = 1 as const;
export const MAX_MESSAGE_CHARACTERS = 4_096;
export const REQUEST_TIMEOUT_MS = 15_000;

export type ProbeOperation = "initialize" | "probe" | "dispose";
export type ProbeStage = "protocol" | "worker" | "artifact" | "sqlite" | "fts5" | "close";
export type ProbeErrorCode =
  | "protocol_mismatch"
  | "invalid_request"
  | "artifact_mismatch"
  | "sqlite_init_failed"
  | "fts5_unavailable"
  | "probe_failed"
  | "worker_crashed"
  | "timeout"
  | "disposed";

export interface ProbeRequest {
  version: typeof PROTOCOL_VERSION;
  id: number;
  operation: ProbeOperation;
}

export interface ProbeError {
  code: ProbeErrorCode;
  stage: ProbeStage;
  message: string;
}

export interface InitializeResult {
  sqliteVersion: "3.53.0";
  fts5Enabled: 1;
  wasmBytes: 864752;
  wasmSha256: string;
  database: ":memory:";
  networkAttempts: 0;
  persistenceAttempts: 0;
  helperWorkerAttempts: 0;
}

export interface SmokeResult {
  expectedTitle: "Synthetic Alpha";
  finiteScore: true;
  snippetMarked: true;
  rollbackAbsent: true;
  integrityPassed: true;
}

export interface DisposeResult {
  closed: true;
}

export type ProbeResult = InitializeResult | SmokeResult | DisposeResult;

export type ProbeResponse =
  | {
      version: typeof PROTOCOL_VERSION;
      id: number;
      operation: ProbeOperation;
      ok: true;
      result: ProbeResult;
    }
  | {
      version: typeof PROTOCOL_VERSION;
      id: number;
      operation: ProbeOperation;
      ok: false;
      error: ProbeError;
    };

export class ProbeRpcError extends Error {
  readonly code: ProbeErrorCode;
  readonly stage: ProbeStage;

  constructor(error: ProbeError) {
    super(error.message);
    this.name = "ProbeRpcError";
    this.code = error.code;
    this.stage = error.stage;
  }
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function hasBoundedShape(value: unknown): boolean {
  try {
    return JSON.stringify(value).length <= MAX_MESSAGE_CHARACTERS;
  } catch {
    return false;
  }
}

export function isProbeOperation(value: unknown): value is ProbeOperation {
  return value === "initialize" || value === "probe" || value === "dispose";
}

export function parseProbeRequest(value: unknown): ProbeRequest | ProbeError {
  if (!isRecord(value) || !hasBoundedShape(value)) {
    return { code: "invalid_request", stage: "protocol", message: "Invalid probe request." };
  }
  if (value.version !== PROTOCOL_VERSION) {
    return { code: "protocol_mismatch", stage: "protocol", message: "Unsupported probe protocol." };
  }
  if (!Number.isSafeInteger(value.id) || Number(value.id) < 1 || !isProbeOperation(value.operation)) {
    return { code: "invalid_request", stage: "protocol", message: "Invalid probe request." };
  }
  if (Object.keys(value).some((key) => !["version", "id", "operation"].includes(key))) {
    return { code: "invalid_request", stage: "protocol", message: "Invalid probe request." };
  }
  return value as unknown as ProbeRequest;
}

export function isProbeResponse(value: unknown): value is ProbeResponse {
  if (!isRecord(value) || !hasBoundedShape(value)) return false;
  if (value.version !== PROTOCOL_VERSION || !Number.isSafeInteger(value.id) || Number(value.id) < 1) return false;
  if (!isProbeOperation(value.operation) || typeof value.ok !== "boolean") return false;
  if (value.ok) return isRecord(value.result);
  if (!isRecord(value.error)) return false;
  return typeof value.error.code === "string"
    && typeof value.error.stage === "string"
    && typeof value.error.message === "string";
}

export interface WorkerLike {
  postMessage(message: ProbeRequest): void;
  terminate(): void;
  addEventListener(type: "message", listener: (event: MessageEvent<unknown>) => void): void;
  addEventListener(type: "error" | "messageerror", listener: (event: Event) => void): void;
  removeEventListener(type: "message", listener: (event: MessageEvent<unknown>) => void): void;
  removeEventListener(type: "error" | "messageerror", listener: (event: Event) => void): void;
}

interface PendingRequest {
  operation: ProbeOperation;
  resolve: (result: ProbeResult) => void;
  reject: (error: Error) => void;
  timeout: ReturnType<typeof setTimeout>;
}

export class ProbeRpcClient {
  private readonly pending = new Map<number, PendingRequest>();
  private nextId = 1;
  private stopped = false;

  private readonly onMessage = (event: MessageEvent<unknown>) => {
    if (!isProbeResponse(event.data)) {
      this.failAll(new ProbeRpcError({
        code: "invalid_request",
        stage: "protocol",
        message: "Invalid probe response.",
      }));
      return;
    }

    const pending = this.pending.get(event.data.id);
    if (!pending || pending.operation !== event.data.operation) {
      this.failAll(new ProbeRpcError({
        code: "invalid_request",
        stage: "protocol",
        message: "Uncorrelated probe response.",
      }));
      return;
    }

    clearTimeout(pending.timeout);
    this.pending.delete(event.data.id);
    if (event.data.ok) pending.resolve(event.data.result);
    else pending.reject(new ProbeRpcError(event.data.error));
  };

  private readonly onWorkerFailure = () => {
    this.failAll(new ProbeRpcError({
      code: "worker_crashed",
      stage: "worker",
      message: "Compatibility Worker failed.",
    }));
  };

  constructor(
    private readonly worker: WorkerLike,
    private readonly timeoutMs = REQUEST_TIMEOUT_MS,
  ) {
    worker.addEventListener("message", this.onMessage);
    worker.addEventListener("error", this.onWorkerFailure);
    worker.addEventListener("messageerror", this.onWorkerFailure);
  }

  get pendingCount(): number {
    return this.pending.size;
  }

  request(operation: ProbeOperation): Promise<ProbeResult> {
    if (this.stopped) {
      return Promise.reject(new ProbeRpcError({
        code: "disposed",
        stage: "protocol",
        message: "Compatibility probe is disposed.",
      }));
    }

    const id = this.nextId;
    this.nextId += 1;
    return new Promise<ProbeResult>((resolve, reject) => {
      const timeout = setTimeout(() => {
        this.pending.delete(id);
        reject(new ProbeRpcError({
          code: "timeout",
          stage: "worker",
          message: "Compatibility probe timed out.",
        }));
      }, this.timeoutMs);
      this.pending.set(id, { operation, resolve, reject, timeout });
      this.worker.postMessage({ version: PROTOCOL_VERSION, id, operation });
    });
  }

  stop(error: Error = new ProbeRpcError({
    code: "disposed",
    stage: "protocol",
    message: "Compatibility probe is disposed.",
  })): void {
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
