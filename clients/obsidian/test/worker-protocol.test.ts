// SPDX-FileCopyrightText: 2026 cybersader
// SPDX-License-Identifier: GPL-3.0-only

import { describe, expect, it } from "vitest";

import {
  MAX_SOURCE_CHANGES,
  WORKER_PROTOCOL_VERSION,
  type SourceInput,
  isWorkerResponse,
  parseWorkerRequest,
} from "../src/worker/protocol";

function source(path = "note.md", vaultId = "active"): SourceInput {
  const bytes = new Uint8Array([35, 32, 65]);
  return {
    descriptor: {
      vault_id: vaultId,
      path,
      format: "markdown",
      byte_length: bytes.byteLength,
      mtime: 1,
      mtime_nanos: "1000001",
    },
    bytes,
  };
}

describe("Worker protocol", () => {
  it("accepts exact bounded source batches", () => {
    const request = parseWorkerRequest({
      version: WORKER_PROTOCOL_VERSION,
      id: 1,
      operation: "add_source_batch",
      generation: "generation-1",
      sources: [source()],
    });
    expect(request).toMatchObject({ operation: "add_source_batch" });
  });

  it("accepts exact staging and active source changes", () => {
    expect(parseWorkerRequest({
      version: WORKER_PROTOCOL_VERSION,
      id: 1,
      operation: "apply_source_changes",
      generation: "generation-1",
      next_generation: null,
      upserts: [],
      removals: [{ vault_id: "active", path: "old.md" }],
    })).toMatchObject({ operation: "apply_source_changes", next_generation: null });

    expect(parseWorkerRequest({
      version: WORKER_PROTOCOL_VERSION,
      id: 2,
      operation: "apply_source_changes",
      generation: "generation-1",
      next_generation: "generation-2",
      upserts: [source("new.md")],
      removals: [],
    })).toMatchObject({ operation: "apply_source_changes", next_generation: "generation-2" });
  });

  it("distinguishes incompatible versions from malformed requests", () => {
    expect(parseWorkerRequest({ version: 1, id: 1, operation: "status" })).toMatchObject({
      code: "protocol_mismatch",
    });
    expect(parseWorkerRequest({
      version: WORKER_PROTOCOL_VERSION,
      id: 0,
      operation: "status",
    })).toMatchObject({ code: "invalid_request" });
  });

  it("rejects unknown fields, path-like generations, and mismatched byte lengths", () => {
    expect(parseWorkerRequest({
      version: WORKER_PROTOCOL_VERSION,
      id: 1,
      operation: "status",
      extra: true,
    })).toMatchObject({ code: "invalid_request" });
    expect(parseWorkerRequest({
      version: WORKER_PROTOCOL_VERSION,
      id: 1,
      operation: "begin_build",
      generation: "../generation",
    })).toMatchObject({ code: "invalid_request" });
    const mismatched = source();
    mismatched.descriptor.byte_length = 2;
    expect(parseWorkerRequest({
      version: WORKER_PROTOCOL_VERSION,
      id: 1,
      operation: "add_source_batch",
      generation: "g1",
      sources: [mismatched],
    })).toMatchObject({ code: "invalid_request" });
  });

  it("rejects empty, duplicate, overlapping, malformed, and oversized source changes", () => {
    const base = {
      version: WORKER_PROTOCOL_VERSION,
      id: 1,
      operation: "apply_source_changes",
      generation: "g1",
      next_generation: "g2",
    } as const;
    expect(parseWorkerRequest({ ...base, upserts: [], removals: [] })).toMatchObject({
      code: "invalid_request",
    });
    expect(parseWorkerRequest({
      ...base,
      upserts: [source(), source()],
      removals: [],
    })).toMatchObject({ code: "invalid_request" });
    expect(parseWorkerRequest({
      ...base,
      upserts: [source()],
      removals: [{ vault_id: "active", path: "note.md" }],
    })).toMatchObject({ code: "invalid_request" });
    expect(parseWorkerRequest({
      ...base,
      upserts: [],
      removals: [{ vault_id: "active", path: "../note.md" }],
    })).toMatchObject({ code: "invalid_request" });
    expect(parseWorkerRequest({
      ...base,
      next_generation: "g1",
      upserts: [source()],
      removals: [],
    })).toMatchObject({ code: "invalid_request" });
    expect(parseWorkerRequest({
      ...base,
      upserts: [],
      removals: Array.from({ length: MAX_SOURCE_CHANGES + 1 }, (_, index) => ({
        vault_id: "active",
        path: `${index}.md`,
      })),
    })).toMatchObject({ code: "invalid_request" });
  });

  it("validates operation-correlated exact responses", () => {
    expect(isWorkerResponse({
      version: WORKER_PROTOCOL_VERSION,
      id: 1,
      operation: "apply_source_changes",
      ok: true,
      result: { generation: "g2", documents: 1, chunks: 1 },
    })).toBe(true);
    expect(isWorkerResponse({
      version: WORKER_PROTOCOL_VERSION,
      id: 1,
      operation: "apply_source_changes",
      ok: true,
      result: { generation: "g2", documents: 1, chunks: 1, extra: true },
    })).toBe(false);
    expect(isWorkerResponse({
      version: WORKER_PROTOCOL_VERSION,
      id: 1,
      operation: "apply_source_changes",
      ok: false,
      error: {
        code: "index_limit_exceeded",
        stage: "index",
        message: "In-plugin index capacity was exceeded.",
        retryable: false,
      },
    })).toBe(true);
    expect(isWorkerResponse({
      version: WORKER_PROTOCOL_VERSION,
      id: 1,
      operation: "dispose",
      ok: true,
      result: { closed: true },
    })).toBe(true);
  });
});
