// SPDX-FileCopyrightText: 2026 cybersader
// SPDX-License-Identifier: GPL-3.0-only

import {
  MAX_QUERY_CHARACTERS,
  MAX_SEARCH_HITS,
  WORKER_PROTOCOL_VERSION,
  fixedWorkerError,
  hasExactKeys,
  isBoundedString,
  isBuildResult,
  isGeneration,
  isInitializeResult,
  isRecord,
  isRequestId,
  isSearchResult,
  isSourceBatch,
  isSourceChanges,
  isStatusResult,
  type WorkerError,
  type WorkerRequest,
} from "./protocol";
import type {
  RpcCommand,
  WorkerRpcProtocol,
} from "./rpc-client";

export type D5cOwnerWorkerOperation =
  | "initialize"
  | "begin_build"
  | "add_source_batch"
  | "apply_source_changes"
  | "commit_build"
  | "abort_build"
  | "search"
  | "status"
  | "dispose";

export type D5cOwnerWorkerRequest = Exclude<
  WorkerRequest,
  { operation: "export_generation" | "restore_generation" | "plan_reconciliation" }
>;

export type D5cOwnerWorkerResponse =
  | {
      version: typeof WORKER_PROTOCOL_VERSION;
      id: number;
      operation: D5cOwnerWorkerOperation;
      ok: true;
      result: unknown;
    }
  | {
      version: typeof WORKER_PROTOCOL_VERSION;
      id: number;
      operation: D5cOwnerWorkerOperation;
      ok: false;
      error: WorkerError;
    };

export const D5C_OWNER_RPC_PROTOCOL: WorkerRpcProtocol = Object.freeze({
  isOperation(operation: string) {
    return isD5cOwnerWorkerOperation(operation);
  },
  parseResponse(value: unknown) {
    return isD5cOwnerWorkerResponse(value) ? value : null;
  },
  expectedGeneration(command: RpcCommand) {
    switch (command.operation) {
      case "begin_build":
      case "add_source_batch":
      case "commit_build":
      case "abort_build":
        return stringField(command, "generation");
      case "apply_source_changes":
        return stringField(command, "next_generation")
          ?? stringField(command, "generation");
      default:
        return null;
    }
  },
  transferList() {
    return [];
  },
});

export function parseD5cOwnerWorkerRequest(
  value: unknown,
): D5cOwnerWorkerRequest | WorkerError {
  if (!isRecord(value)
    || !isRequestId(value.id)
    || !isD5cOwnerWorkerOperation(value.operation)) {
    return fixedWorkerError(
      "invalid_request",
      "protocol",
      "Invalid Worker request.",
      false,
    );
  }
  if (value.version !== WORKER_PROTOCOL_VERSION) {
    return fixedWorkerError(
      "protocol_mismatch",
      "protocol",
      "Unsupported Worker protocol.",
      false,
    );
  }

  const base = ["version", "id", "operation"];
  switch (value.operation) {
    case "initialize":
      return hasExactKeys(value, [...base, "vault_id"])
        && isBoundedString(value.vault_id, 1_024)
        && value.vault_id.trim().length > 0
        ? value as unknown as D5cOwnerWorkerRequest
        : invalidRequest();
    case "status":
    case "dispose":
      return hasExactKeys(value, base)
        ? value as unknown as D5cOwnerWorkerRequest
        : invalidRequest();
    case "begin_build":
    case "commit_build":
    case "abort_build":
      return hasExactKeys(value, [...base, "generation"])
        && isGeneration(value.generation)
        ? value as unknown as D5cOwnerWorkerRequest
        : invalidRequest();
    case "add_source_batch":
      return hasExactKeys(value, [...base, "generation", "sources"])
        && isGeneration(value.generation)
        && isSourceBatch(value.sources)
        ? value as unknown as D5cOwnerWorkerRequest
        : invalidRequest();
    case "apply_source_changes":
      return hasExactKeys(value, [
        ...base,
        "generation",
        "next_generation",
        "upserts",
        "removals",
      ])
        && isGeneration(value.generation)
        && (value.next_generation === null || isGeneration(value.next_generation))
        && value.next_generation !== value.generation
        && isSourceChanges(value.upserts, value.removals)
        ? value as unknown as D5cOwnerWorkerRequest
        : invalidRequest();
    case "search":
      return hasExactKeys(value, [...base, "query", "limit"])
        && typeof value.query === "string"
        && value.query.trim().length > 0
        && value.query.length <= MAX_QUERY_CHARACTERS
        && Number.isSafeInteger(value.limit)
        && Number(value.limit) >= 1
        && Number(value.limit) <= MAX_SEARCH_HITS
        ? value as unknown as D5cOwnerWorkerRequest
        : invalidRequest();
  }
}

export function isD5cOwnerWorkerResponse(
  value: unknown,
): value is D5cOwnerWorkerResponse {
  if (!isRecord(value)
    || value.version !== WORKER_PROTOCOL_VERSION
    || !isRequestId(value.id)
    || !isD5cOwnerWorkerOperation(value.operation)
    || typeof value.ok !== "boolean") {
    return false;
  }
  if (value.ok) {
    return hasExactKeys(value, ["version", "id", "operation", "ok", "result"])
      && isD5cOwnerResult(value.operation, value.result);
  }
  return hasExactKeys(value, ["version", "id", "operation", "ok", "error"])
    && isD5cOwnerWorkerError(value.error);
}

export function isD5cOwnerWorkerOperation(
  value: unknown,
): value is D5cOwnerWorkerOperation {
  return value === "initialize"
    || value === "begin_build"
    || value === "add_source_batch"
    || value === "apply_source_changes"
    || value === "commit_build"
    || value === "abort_build"
    || value === "search"
    || value === "status"
    || value === "dispose";
}

function isD5cOwnerResult(
  operation: D5cOwnerWorkerOperation,
  value: unknown,
): boolean {
  switch (operation) {
    case "initialize":
      return isInitializeResult(value);
    case "begin_build":
    case "add_source_batch":
    case "apply_source_changes":
    case "commit_build":
    case "abort_build":
      return isBuildResult(value);
    case "search":
      return isSearchResult(value);
    case "status":
      return isStatusResult(value);
    case "dispose":
      return isRecord(value)
        && hasExactKeys(value, ["closed"])
        && value.closed === true;
  }
}

function isD5cOwnerWorkerError(value: unknown): value is WorkerError {
  if (!isRecord(value)
    || !hasExactKeys(value, ["code", "stage", "message", "retryable"])
    || typeof value.code !== "string"
    || typeof value.stage !== "string"
    || typeof value.message !== "string"
    || value.message.length > 1_024
    || typeof value.retryable !== "boolean") {
    return false;
  }
  return [
    "protocol_mismatch",
    "invalid_request",
    "invalid_state",
    "artifact_mismatch",
    "rust_init_failed",
    "sqlite_init_failed",
    "fts5_unavailable",
    "source_rejected",
    "query_rejected",
    "index_building",
    "index_limit_exceeded",
    "integrity_failed",
    "worker_crashed",
    "disposed",
    "internal_error",
  ].includes(value.code)
    && [
      "protocol",
      "artifact",
      "rust",
      "sqlite",
      "index",
      "query",
      "lifecycle",
    ].includes(value.stage);
}

function stringField(
  command: RpcCommand,
  field: string,
): string | null {
  const value = command[field];
  return typeof value === "string" ? value : null;
}

function invalidRequest(): WorkerError {
  return fixedWorkerError(
    "invalid_request",
    "protocol",
    "Invalid Worker request.",
    false,
  );
}
