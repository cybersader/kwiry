// SPDX-FileCopyrightText: 2026 cybersader
// SPDX-License-Identifier: GPL-3.0-only

import { describe, expect, it } from "vitest";

import {
  CACHE_SCHEMA_VERSION,
  MAX_EXPORT_BLOB_BYTES,
  MAX_SOURCE_CHANGES,
  WORKER_PROTOCOL_VERSION,
  type SourceInput,
  isWorkerResponse,
  parseWorkerRequest,
} from "../src/worker/protocol";

const CACHE_IDENTITY = "0123456789abcdef".repeat(4);

function exportEnvelope(overrides: Record<string, unknown> = {}): Record<string, unknown> {
  return {
    generation: "g1",
    documents: 1,
    chunks: 2,
    bytes: new Uint8Array([1, 2, 3, 4]),
    blob_byte_length: 4,
    blob_sha256: "a".repeat(64),
    protocol_version: WORKER_PROTOCOL_VERSION,
    cache_schema_version: CACHE_SCHEMA_VERSION,
    chunking_version: 1,
    sqlite_version: "3.53.0",
    sqlite_wasm_sha256: "b".repeat(64),
    rust_wasm_sha256: "c".repeat(64),
    plugin_id: "kwiry-search",
    plugin_version: "0.1.0",
    cache_identity: CACHE_IDENTITY,
    ...overrides,
  };
}

function restoreRequest(overrides: Record<string, unknown> = {}): Record<string, unknown> {
  return {
    version: WORKER_PROTOCOL_VERSION,
    id: 1,
    operation: "restore_generation",
    generation: "g1",
    bytes: new Uint8Array([1, 2, 3, 4]),
    blob_byte_length: 4,
    blob_sha256: "a".repeat(64),
    digest_verified: false,
    protocol_version: WORKER_PROTOCOL_VERSION,
    cache_schema_version: CACHE_SCHEMA_VERSION,
    chunking_version: 1,
    sqlite_version: "3.53.0",
    sqlite_wasm_sha256: "b".repeat(64),
    rust_wasm_sha256: "c".repeat(64),
    plugin_id: "kwiry-search",
    plugin_version: "0.1.0",
    cache_identity: CACHE_IDENTITY,
    expected_cache_identity: CACHE_IDENTITY,
    ...overrides,
  };
}

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

  it("accepts an exact export request and rejects every loosened form", () => {
    const base = {
      version: WORKER_PROTOCOL_VERSION,
      id: 1,
      operation: "export_generation",
      generation: "g1",
      cache_identity: CACHE_IDENTITY,
    } as const;
    expect(parseWorkerRequest(base)).toMatchObject({ operation: "export_generation" });

    const { cache_identity: _identity, ...withoutIdentity } = base;
    expect(parseWorkerRequest(withoutIdentity)).toMatchObject({ code: "invalid_request" });
    expect(parseWorkerRequest({ ...base, extra: true })).toMatchObject({
      code: "invalid_request",
    });
    expect(parseWorkerRequest({ ...base, generation: "../g1" })).toMatchObject({
      code: "invalid_request",
    });
    // A path is not 64 lowercase hex characters, so passing one here is
    // structurally impossible rather than merely discouraged.
    for (const identity of [
      "/home/user/vault",
      CACHE_IDENTITY.toUpperCase(),
      CACHE_IDENTITY.slice(0, 63),
      `${CACHE_IDENTITY}0`,
      "g".repeat(64),
      64,
    ]) {
      expect(parseWorkerRequest({ ...base, cache_identity: identity })).toMatchObject({
        code: "invalid_request",
      });
    }
  });

  it("accepts only an exact unverified restore envelope", () => {
    expect(parseWorkerRequest(restoreRequest())).toMatchObject({ operation: "restore_generation" });
    for (const overrides of [
      { digest_verified: true },
      { digest_verified: undefined },
      { expected_cache_identity: "/vault/path" },
      { blob_sha256: "A".repeat(64) },
      { bytes: [1, 2, 3, 4] },
      { extra: true },
    ]) {
      expect(parseWorkerRequest(restoreRequest(overrides))).toMatchObject({ code: "invalid_request" });
    }
  });

  it("returns typed restore refusals for invalid lengths and oversized blobs", () => {
    expect(parseWorkerRequest(restoreRequest({
      bytes: new Uint8Array(0),
      blob_byte_length: 0,
    }))).toMatchObject({ code: "cache_image_invalid", stage: "protocol" });
    expect(parseWorkerRequest(restoreRequest({ blob_byte_length: 3 }))).toMatchObject({
      code: "cache_image_invalid",
    });

    const oversized = { byteLength: MAX_EXPORT_BLOB_BYTES + 1 };
    Object.setPrototypeOf(oversized, Uint8Array.prototype);
    expect(parseWorkerRequest(restoreRequest({
      bytes: oversized,
      blob_byte_length: MAX_EXPORT_BLOB_BYTES + 1,
    }))).toMatchObject({ code: "cache_blob_too_large", stage: "protocol" });
  });

  it("validates restore responses and typed refusal envelopes", () => {
    expect(isWorkerResponse({
      version: WORKER_PROTOCOL_VERSION,
      id: 1,
      operation: "restore_generation",
      ok: true,
      result: { generation: "g1", documents: 1, chunks: 2 },
    })).toBe(true);
    for (const code of [
      "cache_identity_mismatch",
      "cache_version_mismatch",
      "cache_digest_mismatch",
      "cache_image_invalid",
      "cache_blob_too_large",
    ]) {
      expect(isWorkerResponse({
        version: WORKER_PROTOCOL_VERSION,
        id: 1,
        operation: "restore_generation",
        ok: false,
        error: { code, stage: "index", message: "Restore refused.", retryable: false },
      })).toBe(true);
    }
  });

  it("holds the export envelope shape exactly and cross-checks its declared length", () => {
    const response = (result: unknown) => ({
      version: WORKER_PROTOCOL_VERSION,
      id: 1,
      operation: "export_generation" as const,
      ok: true as const,
      result,
    });

    expect(isWorkerResponse(response(exportEnvelope()))).toBe(true);

    expect(isWorkerResponse(response(exportEnvelope({ extra: true })))).toBe(false);
    const { plugin_id: _pluginId, ...withoutPluginId } = exportEnvelope();
    expect(isWorkerResponse(response(withoutPluginId))).toBe(false);
    // The declared length must equal the buffer that actually arrived.
    expect(isWorkerResponse(response(exportEnvelope({ blob_byte_length: 3 })))).toBe(false);
    expect(isWorkerResponse(response(exportEnvelope({ bytes: [1, 2, 3, 4] })))).toBe(false);
    expect(isWorkerResponse(response(exportEnvelope({
      bytes: new Uint8Array(0),
      blob_byte_length: 0,
    })))).toBe(false);
    for (const digest of ["a".repeat(63), "A".repeat(64), "z".repeat(64), null]) {
      expect(isWorkerResponse(response(exportEnvelope({ blob_sha256: digest })))).toBe(false);
      expect(isWorkerResponse(response(exportEnvelope({ rust_wasm_sha256: digest })))).toBe(false);
      expect(isWorkerResponse(response(exportEnvelope({ cache_identity: digest })))).toBe(false);
    }
    expect(isWorkerResponse(response(exportEnvelope({
      cache_schema_version: CACHE_SCHEMA_VERSION + 1,
    })))).toBe(false);
    expect(isWorkerResponse(response(exportEnvelope({ protocol_version: 1 })))).toBe(false);
    expect(isWorkerResponse(response(exportEnvelope({ sqlite_version: "3.52.0" })))).toBe(false);
    expect(isWorkerResponse(response(exportEnvelope({ chunking_version: 1.5 })))).toBe(false);
    expect(isWorkerResponse(response(exportEnvelope({ plugin_version: "" })))).toBe(false);

    // Cross-operation confusion in both directions.
    expect(isWorkerResponse({
      version: WORKER_PROTOCOL_VERSION,
      id: 1,
      operation: "export_generation",
      ok: true,
      result: { generation: "g1", documents: 1, chunks: 1 },
    })).toBe(false);
    expect(isWorkerResponse({
      version: WORKER_PROTOCOL_VERSION,
      id: 1,
      operation: "commit_build",
      ok: true,
      result: exportEnvelope(),
    })).toBe(false);
  });

  // Refusing an oversized image must be structural, not a truncation, so the
  // validator has to reject a blob above the cap even if one somehow arrived.
  it("rejects an export blob above the transported ceiling", () => {
    expect(MAX_EXPORT_BLOB_BYTES).toBe(384 * 1024 * 1024);
    const oversized = {
      byteLength: MAX_EXPORT_BLOB_BYTES + 1,
    };
    Object.setPrototypeOf(oversized, Uint8Array.prototype);
    expect(isWorkerResponse({
      version: WORKER_PROTOCOL_VERSION,
      id: 1,
      operation: "export_generation",
      ok: true,
      result: exportEnvelope({
        bytes: oversized,
        blob_byte_length: MAX_EXPORT_BLOB_BYTES + 1,
      }),
    })).toBe(false);
  });

  // The error envelope's key set is exact, so a failed export provably cannot
  // smuggle bytes back to the host.
  it("refuses an error response that carries export bytes", () => {
    const error = {
      code: "integrity_failed",
      stage: "index",
      message: "Active generation failed its pre-export integrity check.",
      retryable: false,
    };
    expect(isWorkerResponse({
      version: WORKER_PROTOCOL_VERSION,
      id: 1,
      operation: "export_generation",
      ok: false,
      error,
    })).toBe(true);
    expect(isWorkerResponse({
      version: WORKER_PROTOCOL_VERSION,
      id: 1,
      operation: "export_generation",
      ok: false,
      error,
      result: exportEnvelope(),
    })).toBe(false);
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

  // The hit shape is frozen. Slimming the index changed how the excerpt text
  // is produced, not the transported fields, so the exact key set must still
  // be enforced in both directions.
  it("holds the search hit shape exactly, including the excerpt field", () => {
    const hit = {
      chunk_id: "chunk-1",
      vault_id: "active-vault",
      path: "note.md",
      heading_path: ["Heading"],
      score: 1.5,
      excerpt: "",
      frontmatter: { title: "Note" },
    };
    const response = (result: unknown) => ({
      version: WORKER_PROTOCOL_VERSION,
      id: 1,
      operation: "search" as const,
      ok: true as const,
      result,
    });

    expect(isWorkerResponse(response({ generation: "g1", hits: [hit] }))).toBe(true);
    expect(isWorkerResponse(response({ generation: "g1", hits: [] }))).toBe(true);

    const { excerpt: _excerpt, ...withoutExcerpt } = hit;
    expect(isWorkerResponse(response({ generation: "g1", hits: [withoutExcerpt] }))).toBe(false);
    expect(isWorkerResponse(response({
      generation: "g1",
      hits: [{ ...hit, match_terms: ["quasar"] }],
    }))).toBe(false);
    // Contentless index: the Worker has no text to snippet, so the empty
    // string is enforced rather than merely bounded. A Worker that regressed
    // to emitting snippet text would be rejected here.
    expect(isWorkerResponse(response({
      generation: "g1",
      hits: [{ ...hit, excerpt: "portable <b>quasar</b> cache" }],
    }))).toBe(false);
    expect(isWorkerResponse(response({
      generation: "g1",
      hits: [{ ...hit, excerpt: "x".repeat(262_145) }],
    }))).toBe(false);
    expect(isWorkerResponse(response({
      generation: "g1",
      hits: [{ ...hit, excerpt: null }],
    }))).toBe(false);
  });
});
