// SPDX-FileCopyrightText: 2026 cybersader
// SPDX-License-Identifier: GPL-3.0-only

import {
  isWorkerResponse,
  type WorkerOperation,
} from "./protocol";
import type {
  RpcCommand,
  WorkerRpcProtocol,
} from "./rpc-client";

export const PRODUCTION_RPC_PROTOCOL: WorkerRpcProtocol = Object.freeze({
  isOperation(operation: string) {
    return isWorkerOperation(operation);
  },
  parseResponse(value: unknown) {
    return isWorkerResponse(value) ? value : null;
  },
  expectedGeneration(command: RpcCommand) {
    switch (command.operation) {
      case "begin_build":
      case "add_source_batch":
      case "commit_build":
      case "abort_build":
      case "restore_generation":
      case "plan_reconciliation":
      case "export_generation":
        return stringField(command, "generation");
      case "apply_source_changes":
        return stringField(command, "next_generation")
          ?? stringField(command, "generation");
      default:
        return null;
    }
  },
  transferList(command: RpcCommand) {
    if (command.operation !== "restore_generation") return [];
    const bytes = command.bytes;
    return bytes instanceof Uint8Array
      ? [bytes.buffer as ArrayBuffer]
      : [];
  },
});

function isWorkerOperation(value: string): value is WorkerOperation {
  switch (value) {
    case "initialize":
    case "begin_build":
    case "add_source_batch":
    case "apply_source_changes":
    case "commit_build":
    case "abort_build":
    case "export_generation":
    case "restore_generation":
    case "plan_reconciliation":
    case "search":
    case "status":
    case "dispose":
      return true;
    default:
      return false;
  }
}

function stringField(
  command: RpcCommand,
  field: string,
): string | null {
  const value = command[field];
  return typeof value === "string" ? value : null;
}
