// SPDX-FileCopyrightText: 2026 cybersader
// SPDX-License-Identifier: GPL-3.0-only

import { describe, expect, it } from "vitest";

import {
  WORKER_PROTOCOL_VERSION,
  isWorkerResponse,
  parseWorkerRequest,
} from "../src/worker/protocol";

describe("Worker protocol", () => {
  it("accepts exact bounded source batches", () => {
    const bytes = new Uint8Array([35, 32, 65]);
    const request = parseWorkerRequest({
      version: WORKER_PROTOCOL_VERSION,
      id: 1,
      operation: "add_source_batch",
      generation: "generation-1",
      sources: [{
        descriptor: {
          vault_id: "active",
          path: "note.md",
          format: "markdown",
          byte_length: bytes.byteLength,
          mtime: 1,
          mtime_nanos: "1000001",
        },
        bytes,
      }],
    });
    expect(request).toMatchObject({ operation: "add_source_batch" });
  });

  it("distinguishes incompatible versions from malformed requests", () => {
    expect(parseWorkerRequest({ version: 2, id: 1, operation: "status" })).toMatchObject({
      code: "protocol_mismatch",
    });
    expect(parseWorkerRequest({ version: 1, id: 0, operation: "status" })).toMatchObject({
      code: "invalid_request",
    });
  });

  it("rejects unknown fields, path-like generations, and mismatched byte lengths", () => {
    expect(parseWorkerRequest({
      version: 1,
      id: 1,
      operation: "status",
      extra: true,
    })).toMatchObject({ code: "invalid_request" });
    expect(parseWorkerRequest({
      version: 1,
      id: 1,
      operation: "begin_build",
      generation: "../generation",
    })).toMatchObject({ code: "invalid_request" });
    expect(parseWorkerRequest({
      version: 1,
      id: 1,
      operation: "add_source_batch",
      generation: "g1",
      sources: [{
        descriptor: {
          vault_id: "active",
          path: "note.md",
          format: "markdown",
          byte_length: 2,
          mtime: 0,
          mtime_nanos: "0",
        },
        bytes: new Uint8Array([1]),
      }],
    })).toMatchObject({ code: "invalid_request" });
  });

  it("validates operation-correlated exact responses", () => {
    expect(isWorkerResponse({
      version: 1,
      id: 1,
      operation: "dispose",
      ok: true,
      result: { closed: true },
    })).toBe(true);
    expect(isWorkerResponse({
      version: 1,
      id: 1,
      operation: "dispose",
      ok: true,
      result: { closed: true, extra: true },
    })).toBe(false);
  });
});
