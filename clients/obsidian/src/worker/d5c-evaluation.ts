// SPDX-FileCopyrightText: 2026 cybersader
// SPDX-License-Identifier: GPL-3.0-only

import { internal_d5c_evaluate } from "virtual:kwiry-rust-wasm-bindings";

const ABI_VERSION = 2;
const MAX_ADAPTER_REQUEST_BYTES = 64 * 1024 * 1024;

export function evaluateInternalD5cCase(request: unknown): unknown {
  return parseAdapterResponse(
    internal_d5c_evaluate(stringifyBounded(request)),
    "internal_d5c_evaluate",
  );
}

function parseAdapterResponse(source: string, operation: string): unknown {
  let value: unknown;
  try {
    value = JSON.parse(source);
  } catch {
    throw new Error("Internal D5C adapter returned invalid JSON.");
  }
  if (!isRecord(value)
    || value.abi_version !== ABI_VERSION
    || value.operation !== operation
    || (value.status !== "ok" && value.status !== "error")) {
    throw new Error("Internal D5C adapter response is invalid.");
  }
  if (value.status === "error") {
    if (!isRecord(value.error) || typeof value.error.code !== "string") {
      throw new Error("Internal D5C adapter response is invalid.");
    }
    throw new Error(value.error.code);
  }
  if (!("result" in value)) throw new Error("Internal D5C adapter response is invalid.");
  return value.result;
}

function stringifyBounded(value: unknown): string {
  let serialized: string;
  try {
    serialized = JSON.stringify(value);
  } catch {
    throw new Error("Internal D5C evaluation request is invalid.");
  }
  if (new TextEncoder().encode(serialized).byteLength > MAX_ADAPTER_REQUEST_BYTES) {
    throw new Error("Internal D5C evaluation request is over limit.");
  }
  return serialized;
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}
