// SPDX-FileCopyrightText: 2026 cybersader
// SPDX-License-Identifier: GPL-3.0-only

import { describe, expect, it } from "vitest";

import {
  CACHE_SCHEMA_VERSION,
  INITIAL_BUILD_CHECKPOINT_IMAGE_VERSION,
  INITIAL_BUILD_CHECKPOINT_RECORD_KIND,
  INITIAL_BUILD_CHECKPOINT_RECORD_VERSION,
  MAX_EXPORT_BLOB_BYTES,
  MAX_QUERY_CHARACTERS,
  MAX_RECONCILIATION_SOURCES,
  MAX_SOURCE_BYTES,
  MAX_SOURCE_CHANGES,
  SOURCE_FORMATS,
  WORKER_PROTOCOL_VERSION,
  emptyRestoreEvictionReport,
  emptySourceFormatCounts,
  emptySourceFormatTally,
  type SourceInput,
  isWorkerResponse,
  parseWorkerRequest,
} from "../src/worker/protocol";

const CACHE_IDENTITY = "0123456789abcdef".repeat(4);

function sourceFormatCounts(indexed = 0) {
  const counts = emptySourceFormatCounts();
  counts.markdown["indexed-complete"] = indexed;
  return counts;
}

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
    source_policy_hash: CACHE_IDENTITY,
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
    source_policy_hash: CACHE_IDENTITY,
    expected_cache_identity: CACHE_IDENTITY,
    expected_source_policy_hash: CACHE_IDENTITY,
    ...overrides,
  };
}

const CHECKPOINT_CURSOR = {
  snapshot_source_count: 2,
  acknowledged_add_batches: 1,
  acknowledged_prefix_sources: 1,
  last_acknowledged_path: "alpha.md",
} as const;

function checkpointRestoreRequest(overrides: Record<string, unknown> = {}): Record<string, unknown> {
  return {
    ...restoreRequest(),
    operation: "restore_initial_build_checkpoint",
    record_kind: INITIAL_BUILD_CHECKPOINT_RECORD_KIND,
    checkpoint_record_version: INITIAL_BUILD_CHECKPOINT_RECORD_VERSION,
    checkpoint_image_version: INITIAL_BUILD_CHECKPOINT_IMAGE_VERSION,
    cursor: CHECKPOINT_CURSOR,
    ...overrides,
  };
}

function checkpointExportEnvelope(overrides: Record<string, unknown> = {}): Record<string, unknown> {
  return {
    generation: "g1",
    documents: 1,
    chunks: 2,
    database_bytes: 65_536,
    database_byte_limit: 1024 * 1024,
    quarantined_sources: 0,
    quarantine_fields: [],
    source_format_counts: sourceFormatCounts(1),
    record_kind: INITIAL_BUILD_CHECKPOINT_RECORD_KIND,
    checkpoint_record_version: INITIAL_BUILD_CHECKPOINT_RECORD_VERSION,
    checkpoint_image_version: INITIAL_BUILD_CHECKPOINT_IMAGE_VERSION,
    publication: "initial_staging",
    searchable: false,
    cursor: CHECKPOINT_CURSOR,
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
    source_policy_hash: CACHE_IDENTITY,
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
  it("publishes protocol 13, cache schema 11, and the closed eight-format set", () => {
    // Excel extends the transported format and locator unions. Cache schema 11
    // widens only the sources-table format check and migrates schema 10 in place.
    expect(WORKER_PROTOCOL_VERSION).toBe(13);
    expect(CACHE_SCHEMA_VERSION).toBe(11);
    expect(SOURCE_FORMATS).toEqual([
      "markdown",
      "text",
      "base",
      "canvas",
      "docx",
      "pdf",
      "excalidraw",
      "excel",
    ]);
  });

  it("requires initialize to declare a sorted, duplicate-free enabled format set", () => {
    const initialize = (overrides: Record<string, unknown> = {}) => parseWorkerRequest({
      version: WORKER_PROTOCOL_VERSION,
      id: 1,
      operation: "initialize",
      vault_id: "active-vault",
      source_policy_hash: "a".repeat(64),
      enabled_source_formats: ["base", "markdown"],
      ...overrides,
    });

    expect(initialize()).toMatchObject({ operation: "initialize" });
    // The empty set is legal: a user may disable every format, and that is a
    // configuration choice, not a malformed request.
    expect(initialize({ enabled_source_formats: [] }))
      .toMatchObject({ operation: "initialize" });
    expect(initialize({ enabled_source_formats: [...SOURCE_FORMATS].sort() }))
      .toMatchObject({ operation: "initialize" });

    // Sortedness is required rather than tolerated: the set decides which
    // restored rows are deleted before publication, so an ordering the sender
    // chose freely is an ordering nothing can check against.
    expect(initialize({ enabled_source_formats: ["markdown", "base"] }))
      .toMatchObject({ code: "invalid_request" });
    expect(initialize({ enabled_source_formats: ["base", "base"] }))
      .toMatchObject({ code: "invalid_request" });
    expect(initialize({ enabled_source_formats: ["base", "sqlite"] }))
      .toMatchObject({ code: "invalid_request" });
    expect(initialize({ enabled_source_formats: "markdown" }))
      .toMatchObject({ code: "invalid_request" });
    // Absent is a refusal, not a default: a Worker that guessed the enabled
    // set could publish rows of a format the user switched off.
    const { enabled_source_formats: _omitted, ...withoutFormats } = {
      version: WORKER_PROTOCOL_VERSION,
      id: 1,
      operation: "initialize" as const,
      vault_id: "active-vault",
      source_policy_hash: "a".repeat(64),
      enabled_source_formats: ["markdown"],
    };
    expect(parseWorkerRequest(withoutFormats)).toMatchObject({ code: "invalid_request" });
  });

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

  it("accepts all six source formats and rejects open-ended format strings", () => {
    for (const format of SOURCE_FORMATS) {
      const candidate = source(`note.${format === "markdown" ? "md" : format}`);
      candidate.descriptor.format = format;
      expect(parseWorkerRequest({
        version: WORKER_PROTOCOL_VERSION,
        id: 1,
        operation: "add_source_batch",
        generation: "g1",
        sources: [candidate],
      })).toMatchObject({ operation: "add_source_batch" });
    }
    const invalid = source();
    (invalid.descriptor as { format: string }).format = "html";
    expect(parseWorkerRequest({
      version: WORKER_PROTOCOL_VERSION,
      id: 1,
      operation: "add_source_batch",
      generation: "g1",
      sources: [invalid],
    })).toMatchObject({ code: "invalid_request" });
  });

  it("separates user query bounds from malformed Worker requests", () => {
    const request = (query: unknown, limit: unknown = 20) => parseWorkerRequest({
      version: WORKER_PROTOCOL_VERSION,
      id: 1,
      operation: "search",
      query,
      limit,
    });
    expect(request("   ")).toMatchObject({ code: "invalid_query", stage: "query" });
    expect(request("x".repeat(MAX_QUERY_CHARACTERS + 1)))
      .toMatchObject({ code: "invalid_query", stage: "query" });
    expect(request("query", 0)).toMatchObject({ code: "invalid_request", stage: "protocol" });
    expect(request({ private: "query" })).toMatchObject({
      code: "invalid_request",
      stage: "protocol",
    });
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

  it("accepts Rust-prepared oversized metadata without content bytes", () => {
    const oversized = {
      descriptor: {
        vault_id: "active",
        path: "large.md",
        format: "markdown",
        byte_length: MAX_SOURCE_BYTES,
        mtime: 1,
        mtime_nanos: "1000000000",
      },
      oversized: true,
    } as const;
    expect(parseWorkerRequest({
      version: WORKER_PROTOCOL_VERSION,
      id: 1,
      operation: "add_source_batch",
      generation: "g1",
      sources: [oversized],
    })).toMatchObject({ operation: "add_source_batch" });
    expect(parseWorkerRequest({
      version: WORKER_PROTOCOL_VERSION,
      id: 2,
      operation: "apply_source_changes",
      generation: "g1",
      next_generation: "g2",
      upserts: [oversized],
      removals: [],
    })).toMatchObject({ operation: "apply_source_changes" });
    expect(parseWorkerRequest({
      version: WORKER_PROTOCOL_VERSION,
      id: 3,
      operation: "add_source_batch",
      generation: "g1",
      sources: [{ ...oversized, descriptor: { ...oversized.descriptor, byte_length: 1 } }],
    })).toMatchObject({ code: "invalid_request" });
  });

  it("validates bounded exact reconciliation plans in both directions", () => {
    const request = {
      version: WORKER_PROTOCOL_VERSION,
      id: 1,
      operation: "plan_reconciliation",
      generation: "g1",
      vault_id: "active-vault",
      current_sources: [{
        path: "note.md",
        byte_length: 4,
        mtime_nanos: "1000000",
        indexable: true,
      }],
    } as const;
    expect(parseWorkerRequest(request)).toMatchObject({ operation: "plan_reconciliation" });
    expect(parseWorkerRequest({
      ...request,
      current_sources: Array(MAX_RECONCILIATION_SOURCES + 1).fill({
        path: "note.md",
        byte_length: 1,
        mtime_nanos: "1",
        indexable: true,
      }),
    })).toMatchObject({ code: "invalid_request" });
    expect(isWorkerResponse({
      version: WORKER_PROTOCOL_VERSION,
      id: 1,
      operation: "plan_reconciliation",
      ok: true,
      result: {
        generation: "g1",
        unchanged: ["note.md"],
        audit: [],
        refresh: ["changed.md"],
        remove: ["gone.md"],
        stored_source_count: 2,
        matched_source_count: 1,
      },
    })).toBe(true);
    expect(isWorkerResponse({
      version: WORKER_PROTOCOL_VERSION,
      id: 1,
      operation: "plan_reconciliation",
      ok: true,
      result: {
        generation: "g1",
        unchanged: ["note.md"],
        audit: [],
        refresh: ["note.md"],
        remove: [],
        stored_source_count: 1,
        matched_source_count: 1,
      },
    })).toBe(false);
  });

  it("accepts a disjoint stored/current inventory whose valid plan exceeds one source bound", () => {
    const inventorySize = Math.floor(MAX_RECONCILIATION_SOURCES / 2) + 1;
    const refresh = Array.from({ length: inventorySize }, (_, index) => `current-${index}.md`);
    const remove = Array.from({ length: inventorySize }, (_, index) => `stored-${index}.md`);
    expect(refresh.length + remove.length).toBeGreaterThan(MAX_RECONCILIATION_SOURCES);
    expect(isWorkerResponse({
      version: WORKER_PROTOCOL_VERSION,
      id: 1,
      operation: "plan_reconciliation",
      ok: true,
      result: {
        generation: "g1",
        unchanged: [],
        audit: [],
        refresh,
        remove,
        stored_source_count: inventorySize,
        matched_source_count: 0,
      },
    })).toBe(true);
  });

  it("rejects a reconciliation plan whose ledger coverage counts omit stored deletions", () => {
    expect(isWorkerResponse({
      version: WORKER_PROTOCOL_VERSION,
      id: 1,
      operation: "plan_reconciliation",
      ok: true,
      result: {
        generation: "g1",
        unchanged: ["current.md"],
        audit: [],
        refresh: [],
        remove: [],
        stored_source_count: 2,
        matched_source_count: 1,
      },
    })).toBe(false);
  });

  it("distinguishes incompatible versions from malformed requests", () => {
    expect(parseWorkerRequest({ version: 7, id: 1, operation: "status" })).toMatchObject({
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

  it("holds initial-build checkpoint requests and results to distinct exact shapes", () => {
    const exportRequest = {
      version: WORKER_PROTOCOL_VERSION,
      id: 1,
      operation: "export_initial_build_checkpoint",
      generation: "g1",
      cache_identity: CACHE_IDENTITY,
      cursor: CHECKPOINT_CURSOR,
    } as const;
    expect(parseWorkerRequest(exportRequest)).toMatchObject({
      operation: "export_initial_build_checkpoint",
    });
    expect(parseWorkerRequest({ ...exportRequest, cursor: { ...CHECKPOINT_CURSOR, extra: true } }))
      .toMatchObject({ code: "invalid_request" });
    expect(parseWorkerRequest({
      ...exportRequest,
      cursor: { ...CHECKPOINT_CURSOR, acknowledged_prefix_sources: 0 },
    })).toMatchObject({ code: "invalid_request" });
    expect(parseWorkerRequest({
      ...exportRequest,
      cursor: { ...CHECKPOINT_CURSOR, acknowledged_add_batches: 0 },
    })).toMatchObject({ code: "invalid_request" });

    expect(parseWorkerRequest(checkpointRestoreRequest())).toMatchObject({
      operation: "restore_initial_build_checkpoint",
    });
    for (const overrides of [
      { record_kind: "complete_generation" },
      { checkpoint_record_version: -1 },
      { checkpoint_image_version: 1.5 },
      { cursor: { ...CHECKPOINT_CURSOR, last_acknowledged_path: "../alpha.md" } },
      { digest_verified: true },
      { extra: true },
    ]) {
      const parsed = parseWorkerRequest(checkpointRestoreRequest(overrides));
      if (overrides.record_kind === "complete_generation") {
        expect(parsed).toMatchObject({ operation: "restore_initial_build_checkpoint" });
      } else {
        expect(parsed).toMatchObject({ code: "invalid_request" });
      }
    }

    const exportResponse = (result: unknown) => ({
      version: WORKER_PROTOCOL_VERSION,
      id: 1,
      operation: "export_initial_build_checkpoint" as const,
      ok: true as const,
      result,
    });
    expect(isWorkerResponse(exportResponse(checkpointExportEnvelope()))).toBe(true);
    expect(isWorkerResponse(exportResponse(checkpointExportEnvelope({
      record_kind: "complete_generation",
    })))).toBe(false);
    expect(isWorkerResponse(exportResponse(checkpointExportEnvelope({ searchable: true })))).toBe(false);
    expect(isWorkerResponse(exportResponse(checkpointExportEnvelope({ extra: true })))).toBe(false);

    expect(isWorkerResponse({
      version: WORKER_PROTOCOL_VERSION,
      id: 2,
      operation: "restore_initial_build_checkpoint",
      ok: true,
      result: {
        generation: "g1",
        documents: 1,
        chunks: 2,
        database_bytes: 65_536,
        database_byte_limit: 1024 * 1024,
        quarantined_sources: 0,
        quarantine_fields: [],
        source_format_counts: sourceFormatCounts(1),
        record_kind: INITIAL_BUILD_CHECKPOINT_RECORD_KIND,
        publication: "initial_staging",
        searchable: false,
        cursor: CHECKPOINT_CURSOR,
        evictions: emptyRestoreEvictionReport(),
      },
    })).toBe(true);
    expect(isWorkerResponse({
      version: WORKER_PROTOCOL_VERSION,
      id: 3,
      operation: "plan_initial_build_checkpoint_reconciliation",
      ok: true,
      result: {
        generation: "g1",
        publication: "initial_staging",
        searchable: false,
        unchanged: [],
        audit: [{ path: "alpha.md", content_hash: "a".repeat(64) }],
        refresh: [],
        remove: [],
        stored_source_count: 1,
        matched_source_count: 1,
      },
    })).toBe(true);
    expect(isWorkerResponse({
      version: WORKER_PROTOCOL_VERSION,
      id: 4,
      operation: "plan_reconciliation",
      ok: true,
      result: {
        generation: "g1",
        publication: "initial_staging",
        searchable: false,
        unchanged: [],
        audit: [],
        refresh: [],
        remove: [],
        stored_source_count: 0,
        matched_source_count: 0,
      },
    })).toBe(false);
  });

  it("validates restore responses and typed refusal envelopes", () => {
    const restored = (evictions: unknown) => ({
      version: WORKER_PROTOCOL_VERSION,
      id: 1,
      operation: "restore_generation" as const,
      ok: true as const,
      result: {
        generation: "g1",
        documents: 1,
        chunks: 2,
        database_bytes: 65_536,
        database_byte_limit: 1024 * 1024,
        quarantined_sources: 0,
        quarantine_fields: [],
        source_format_counts: sourceFormatCounts(1),
        evictions,
      },
    });
    expect(isWorkerResponse(restored(emptyRestoreEvictionReport()))).toBe(true);
    expect(isWorkerResponse(restored({
      stale_identity: { ...emptySourceFormatTally(), pdf: 3 },
      disabled_format: { ...emptySourceFormatTally(), docx: 1 },
    }))).toBe(true);
    // A restore that cannot state what it refused is not a restore this host
    // will accept: the status line would have to guess.
    expect(isWorkerResponse({
      version: WORKER_PROTOCOL_VERSION,
      id: 1,
      operation: "restore_generation",
      ok: true,
      result: {
        generation: "g1",
        documents: 1,
        chunks: 2,
        database_bytes: 65_536,
        database_byte_limit: 1024 * 1024,
        quarantined_sources: 0,
        quarantine_fields: [],
        source_format_counts: sourceFormatCounts(1),
      },
    })).toBe(false);
    // A tally is per compiled format and total: a partial map cannot say that
    // a format had nothing evicted, only that nobody looked.
    expect(isWorkerResponse(restored({
      stale_identity: { pdf: 1 },
      disabled_format: emptySourceFormatTally(),
    }))).toBe(false);
    expect(isWorkerResponse(restored({
      stale_identity: { ...emptySourceFormatTally(), pdf: -1 },
      disabled_format: emptySourceFormatTally(),
    }))).toBe(false);
    expect(isWorkerResponse(restored({ stale_identity: emptySourceFormatTally() }))).toBe(false);
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
      result: { generation: "g1", documents: 1, chunks: 1, database_bytes: 65_536, database_byte_limit: 1024 * 1024, quarantined_sources: 0, quarantine_fields: [], source_format_counts: sourceFormatCounts(1) },
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

  it("accepts only the protocol-10 structured query error vocabulary", () => {
    const response = (code: string) => ({
      version: WORKER_PROTOCOL_VERSION,
      id: 1,
      operation: "search",
      ok: false,
      error: {
        code,
        stage: "query",
        message: "Safe query failure.",
        retryable: false,
      },
    });
    for (const code of [
      "explicit_query_unsupported",
      "invalid_query",
      "invalid_query_plan",
      "query_execution_failed",
      "index_building",
    ]) {
      expect(isWorkerResponse(response(code)), code).toBe(true);
    }
    expect(isWorkerResponse(response("query_rejected"))).toBe(false);
    expect(isWorkerResponse(response("private_adapter_code"))).toBe(false);
  });

  it("validates operation-correlated exact responses", () => {
    expect(isWorkerResponse({
      version: WORKER_PROTOCOL_VERSION,
      id: 1,
      operation: "apply_source_changes",
      ok: true,
      result: { generation: "g2", documents: 1, chunks: 1, database_bytes: 65_536, database_byte_limit: 1024 * 1024, quarantined_sources: 0, quarantine_fields: [], source_format_counts: sourceFormatCounts(1) },
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

  it("requires measured database bytes in build and status responses", () => {
    const status = {
      phase: "ready",
      searchable: true,
      active_generation: "g1",
      staging_generation: null,
      documents: 1,
      chunks: 2,
      active_database_bytes: 131_072,
      staging_database_bytes: 0,
      database_byte_limit: 1024 * 1024,
      source_format_counts: sourceFormatCounts(1),
      dirty: false,
      rebuilding: false,
    };
    expect(isWorkerResponse({
      version: WORKER_PROTOCOL_VERSION,
      id: 1,
      operation: "status",
      ok: true,
      result: status,
    })).toBe(true);
    const { active_database_bytes: _bytes, ...missingBytes } = status;
    expect(isWorkerResponse({
      version: WORKER_PROTOCOL_VERSION,
      id: 1,
      operation: "status",
      ok: true,
      result: missingBytes,
    })).toBe(false);
    const invalidCounts = emptySourceFormatCounts();
    invalidCounts.pdf.unreadable = -1;
    expect(isWorkerResponse({
      version: WORKER_PROTOCOL_VERSION,
      id: 1,
      operation: "status",
      ok: true,
      result: { ...status, source_format_counts: invalidCounts },
    })).toBe(false);
  });

  it("carries only compact display frontmatter in ordinary search responses", () => {
    const response = (frontmatter: unknown) => ({
      version: WORKER_PROTOCOL_VERSION,
      id: 1,
      operation: "search" as const,
      ok: true as const,
      result: {
        generation: "g1",
        hits: [{
          chunk_id: "chunk-1",
          vault_id: "active-vault",
          path: "note.md",
          format: "markdown",
          coverage: "indexed-complete",
          locator: null,
          heading_path: [],
          score: 1,
          excerpt: "stored content",
          frontmatter,
        }],
        candidate_window: {
          state: "exhausted",
          candidate_count: 1,
          candidate_limit: 512,
        },
      },
    });
    expect(isWorkerResponse(response({ title: "Display title" }))).toBe(true);
    expect(isWorkerResponse(response({}))).toBe(true);
    expect(isWorkerResponse(response({ payload: "x".repeat(2 * 1024 * 1024) }))).toBe(false);
    expect(isWorkerResponse(response({ title: "x".repeat(1_025) }))).toBe(false);
  });

  it("holds the multi-format search hit shape and bounded stored excerpt exactly", () => {
    const hit = {
      chunk_id: "chunk-1",
      vault_id: "active-vault",
      path: "note.md",
      format: "markdown",
      coverage: "indexed-complete",
      locator: null,
      heading_path: ["Heading"],
      score: 1.5,
      excerpt: "portable quasar cache",
      frontmatter: { title: "Note" },
    };
    const candidateWindow = {
      state: "exhausted",
      candidate_count: 1,
      candidate_limit: 512,
    };
    const response = (result: Record<string, unknown>) => ({
      version: WORKER_PROTOCOL_VERSION,
      id: 1,
      operation: "search" as const,
      ok: true as const,
      result: { ...result, candidate_window: candidateWindow },
    });

    expect(isWorkerResponse(response({ generation: "g1", hits: [hit] }))).toBe(true);
    expect(isWorkerResponse(response({ generation: "g1", hits: [] }))).toBe(true);
    expect(isWorkerResponse({
      ...response({ generation: "g1", hits: [hit] }),
      result: {
        generation: "g1",
        hits: [hit],
        candidate_window: { ...candidateWindow, candidate_count: 0 },
      },
    })).toBe(false);

    const { excerpt: _excerpt, ...withoutExcerpt } = hit;
    expect(isWorkerResponse(response({ generation: "g1", hits: [withoutExcerpt] }))).toBe(false);
    expect(isWorkerResponse(response({
      generation: "g1",
      hits: [{ ...hit, match_terms: ["quasar"] }],
    }))).toBe(false);
    expect(isWorkerResponse(response({
      generation: "g1",
      hits: [{
        ...hit,
        path: "projects.base",
        format: "base",
        locator: { kind: "base_view", view: "Active" },
      }],
    }))).toBe(true);
    expect(isWorkerResponse(response({
      generation: "g1",
      hits: [{ ...hit, locator: { kind: "base_view", view: "Active" } }],
    }))).toBe(false);
    const pdfHit = {
      ...hit,
      path: "papers/report.pdf",
      format: "pdf",
      heading_path: [],
      locator: { kind: "pdf_page", page: 7 },
    };
    expect(isWorkerResponse(response({ generation: "g1", hits: [pdfHit] }))).toBe(true);
    expect(isWorkerResponse(response({
      generation: "g1",
      hits: [{ ...pdfHit, locator: null },
      ],
    }))).toBe(true);
    // Each locator kind pairs with exactly one format, in both directions.
    expect(isWorkerResponse(response({
      generation: "g1",
      hits: [{ ...hit, locator: { kind: "pdf_page", page: 7 } }],
    }))).toBe(false);
    expect(isWorkerResponse(response({
      generation: "g1",
      hits: [{ ...pdfHit, locator: { kind: "base_view", view: "Active" } }],
    }))).toBe(false);
    for (const page of [0, -1, 1.5, "7"]) {
      expect(isWorkerResponse(response({
        generation: "g1",
        hits: [{ ...pdfHit, locator: { kind: "pdf_page", page } }],
      }))).toBe(false);
    }
    expect(isWorkerResponse(response({
      generation: "g1",
      hits: [{ ...hit, excerpt: "x".repeat(16_385) }],
    }))).toBe(false);
    expect(isWorkerResponse(response({
      generation: "g1",
      hits: [{ ...hit, excerpt: null }],
    }))).toBe(false);
  });

  it("requires closed truthful candidate-window facts on every search result", () => {
    const response = (candidateWindow: unknown) => ({
      version: WORKER_PROTOCOL_VERSION,
      id: 1,
      operation: "search" as const,
      ok: true as const,
      result: {
        generation: "g1",
        hits: [],
        candidate_window: candidateWindow,
      },
    });
    for (const [state, candidateCount] of [
      ["exhausted", 0],
      ["more_available", 1],
      ["candidate_limit_reached", 512],
      ["unknown", 0],
    ] as const) {
      expect(isWorkerResponse(response({
        state,
        candidate_count: candidateCount,
        candidate_limit: 512,
      })), state).toBe(true);
    }
    expect(isWorkerResponse(response({
      state: "candidate_limit_reached",
      candidate_count: 0,
      candidate_limit: 512,
    }))).toBe(false);
    expect(isWorkerResponse(response({
      state: "more_available",
      candidate_count: 0,
      candidate_limit: 512,
    }))).toBe(false);
    expect(isWorkerResponse(response({
      state: "complete",
      candidate_count: 0,
      candidate_limit: 512,
    }))).toBe(false);
    expect(isWorkerResponse(response({
      state: "unknown",
      candidate_count: 513,
      candidate_limit: 512,
    }))).toBe(false);
    expect(isWorkerResponse(response({
      state: "unknown",
      candidate_count: 0,
      candidate_limit: 100,
    }))).toBe(false);
    expect(isWorkerResponse({
      ...response(undefined),
      result: { generation: "g1", hits: [] },
    })).toBe(false);
  });
});
