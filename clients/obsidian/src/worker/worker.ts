// SPDX-FileCopyrightText: 2026 cybersader
// SPDX-License-Identifier: GPL-3.0-only
/// <reference lib="webworker" />

import sqlite3InitModule from "@sqlite.org/sqlite-wasm";
import sqliteWasmBytes from "@sqlite.org/sqlite-wasm/sqlite3.wasm";
import rustWasmBytes from "virtual:kwiry-rust-wasm-bytes";
import {
  PLUGIN_ID,
  PLUGIN_VERSION,
  RUST_WASM_SHA256,
  RUST_WASM_SIZE,
  SQLITE_WASM_SHA256,
  SQLITE_WASM_SIZE,
} from "virtual:kwiry-artifact-identities";

import { BlockVfsUnavailableError } from "./block-vfs";
import {
  CacheImageInvalidError,
  CacheVersionMismatchError,
  IndexCapacityError,
  IndexIntegrityError,
  openFts5Generation,
  openRestoredFts5Generation,
  type Fts5GenerationIndex,
  type SQLiteApi,
} from "./fts5-index";
import { validateSQLiteImage } from "./image-header";
import {
  CACHE_SCHEMA_VERSION,
  MAX_EXPORT_BLOB_BYTES,
  WORKER_PROTOCOL_VERSION,
  type BuildResult,
  type DisposeResult,
  type ExportGenerationResult,
  type InitializeResult,
  type SearchResult,
  type StatusResult,
  type WorkerError,
  type WorkerOperation,
  type WorkerRequest,
  type WorkerResponse,
  fixedWorkerError,
  parseWorkerRequest,
} from "./protocol";
import {
  RustAdapterError,
  finalizeQueryWithRust,
  initializeRustAdapter,
  prepareQueryWithRust,
  prepareSourceWithRust,
} from "./rust-adapter";

interface SQLiteInitializerOptions {
  wasmBinary: Uint8Array;
  locateFile: () => string;
  print: () => void;
  printErr: () => void;
}

type SQLiteInitializer = (options: SQLiteInitializerOptions) => Promise<SQLiteApi>;
type WorkerState = "cold" | "initializing" | "ready" | "building" | "disposed" | "failed";

interface Generation {
  id: string;
  index: Fts5GenerationIndex;
}

interface GuardCounters {
  networkAttempts: number;
  persistenceAttempts: number;
  helperWorkerAttempts: number;
}

const scope = self as DedicatedWorkerGlobalScope;
let state: WorkerState = "cold";
let sqlite: SQLiteApi | null = null;
let active: Generation | null = null;
let staging: Generation | null = null;
const usedGenerations = new Set<string>();
let lastRequestId = 0;
let guards: GuardCounters | null = null;
// Declared by the Rust adapter at initialize. A generation with no chunks
// still has to be able to name the chunking contract its image was built
// under, so the adapter identity — not an observed chunk — is the source.
let declaredChunkingVersion: number | null = null;

async function initialize(): Promise<InitializeResult> {
  if (state !== "cold") {
    throw fixedWorkerError("invalid_state", "lifecycle", "Worker is already initialized.", false);
  }
  state = "initializing";
  try {
    guards = installGuards();
    await verifyArtifact(sqliteWasmBytes, SQLITE_WASM_SIZE, SQLITE_WASM_SHA256);
    await verifyArtifact(rustWasmBytes, RUST_WASM_SIZE, RUST_WASM_SHA256);
    const rustIdentity = initializeRustAdapter();
    declaredChunkingVersion = rustIdentity.chunking_version;

    const initializeSqlite = sqlite3InitModule as unknown as SQLiteInitializer;
    const originalWarn = console.warn;
    let unexpectedWarnings = 0;
    console.warn = (...values: unknown[]) => {
      const message = values.map((value) => String(value)).join(" ");
      if (message.startsWith("Ignoring inability to install 'opfs'")
        || message.startsWith("Ignoring inability to install the opfs-wl")) {
        return;
      }
      unexpectedWarnings += 1;
    };
    try {
      sqlite = await initializeSqlite({
        wasmBinary: sqliteWasmBytes.slice(),
        locateFile: () => "embedded://sqlite3.wasm",
        print: () => undefined,
        printErr: () => undefined,
      });
    } finally {
      console.warn = originalWarn;
    }
    if (unexpectedWarnings !== 0 || !sqlite) {
      throw fixedWorkerError(
        "sqlite_init_failed",
        "sqlite",
        "SQLite initialization failed.",
        false,
      );
    }

    const probe = new sqlite.oo1.DB(":memory:", "c");
    try {
      if (probe.filename !== ":memory:"
        || probe.selectValue("SELECT sqlite_version()") !== "3.53.0"
        || Number(probe.selectValue("SELECT sqlite_compileoption_used('ENABLE_FTS5')")) !== 1) {
        throw fixedWorkerError(
          "fts5_unavailable",
          "sqlite",
          "Required SQLite FTS5 runtime is unavailable.",
          false,
        );
      }
    } finally {
      probe.close();
    }
    installPersistenceGuards(guards);
    if (guards.networkAttempts !== 0
      || guards.persistenceAttempts !== 0
      || guards.helperWorkerAttempts !== 0) {
      throw fixedWorkerError(
        "sqlite_init_failed",
        "sqlite",
        "Embedded runtimes attempted a denied host capability.",
        false,
      );
    }

    state = "ready";
    return {
      rustAbiVersion: rustIdentity.abi_version,
      sourceSchemaVersion: rustIdentity.source_preparation_schema_version,
      querySchemaVersion: rustIdentity.lexical_query_plan_schema_version,
      matchPlanSchemaVersion: rustIdentity.fts5_match_plan_schema_version,
      sqliteVersion: "3.53.0",
      fts5Enabled: 1,
    };
  } catch (error) {
    state = "failed";
    sqlite = null;
    declaredChunkingVersion = null;
    if (isWorkerError(error)) throw error;
    if (error instanceof RustAdapterError) {
      throw fixedWorkerError(
        error.code === "artifact_mismatch" ? "artifact_mismatch" : "rust_init_failed",
        error.code === "artifact_mismatch" ? "artifact" : "rust",
        error.code === "artifact_mismatch"
          ? "Portable Rust artifact identity mismatch."
          : "Portable Rust initialization failed.",
        false,
      );
    }
    throw fixedWorkerError("sqlite_init_failed", "sqlite", "SQLite initialization failed.", false);
  }
}

function beginBuild(generation: string): BuildResult {
  requireInitialized();
  if (!sqlite || staging || usedGenerations.has(generation)) {
    throw fixedWorkerError(
      "invalid_state",
      "index",
      "Requested generation is unavailable.",
      true,
    );
  }
  staging = { id: generation, index: openFts5Generation(sqlite) };
  usedGenerations.add(generation);
  state = "building";
  return generationResult(staging);
}

function addSourceBatch(
  generation: string,
  sources: Extract<WorkerRequest, { operation: "add_source_batch" }>["sources"],
): BuildResult {
  const target = requireStaging(generation);
  try {
    const preparations = sources.map((source) =>
      prepareSourceWithRust(source.descriptor, source.bytes)
    );
    target.index.applySourceChanges(preparations, []);
    return generationResult(target);
  } catch (error) {
    abortStaging();
    if (error instanceof IndexCapacityError) throw indexCapacityError();
    if (error instanceof RustAdapterError) {
      throw fixedWorkerError(
        "source_rejected",
        "rust",
        "Portable Rust rejected a source batch.",
        false,
      );
    }
    throw fixedWorkerError(
      "source_rejected",
      "index",
      "In-plugin index rejected a source batch.",
      false,
    );
  }
}

function applySourceChanges(
  request: Extract<WorkerRequest, { operation: "apply_source_changes" }>,
): BuildResult {
  requireInitialized();
  if (request.next_generation === null) {
    const target = requireStaging(request.generation);
    try {
      const preparations = request.upserts.map((source) =>
        prepareSourceWithRust(source.descriptor, source.bytes)
      );
      target.index.applySourceChanges(preparations, request.removals);
      return generationResult(target);
    } catch (error) {
      abortStaging();
      throw sourceChangeError(error);
    }
  }

  if (staging
    || !active
    || active.id !== request.generation
    || usedGenerations.has(request.next_generation)) {
    throw fixedWorkerError(
      "invalid_state",
      "index",
      "Requested active generation is unavailable.",
      true,
    );
  }

  try {
    const preparations = request.upserts.map((source) =>
      prepareSourceWithRust(source.descriptor, source.bytes)
    );
    // In place on a published generation: there is no later commit gate, so
    // the reconciliation runs inside this transaction. A divergence rolls the
    // batch back and is reported instead of quietly living in the active index.
    active.index.applySourceChanges(preparations, request.removals, true);
    active.id = request.next_generation;
    usedGenerations.add(request.next_generation);
    return generationResult(active);
  } catch (error) {
    throw sourceChangeError(error);
  }
}

function commitBuild(generation: string): BuildResult {
  return publishStaging(requireStaging(generation), true);
}

/**
 * The single active/staging publication barrier. Restored cache images already
 * passed compaction before export, so they use the same two integrity gates and
 * atomic swap without rewriting the whole image again.
 */
function publishStaging(target: Generation, compact: boolean): BuildResult {
  try {
    target.index.assertIntegrity();
  } catch {
    abortStaging();
    throw fixedWorkerError(
      "integrity_failed",
      "index",
      "Staging generation failed its integrity check.",
      false,
    );
  }

  if (compact) {
    try {
      target.index.compact();
    } catch {
      abortStaging();
      throw fixedWorkerError(
        "integrity_failed",
        "index",
        "Staging generation failed its pre-publication compaction.",
        false,
      );
    }
  }

  try {
    target.index.assertIntegrity();
  } catch {
    abortStaging();
    throw fixedWorkerError(
      "integrity_failed",
      "index",
      compact
        ? "Compacted staging generation failed its integrity check."
        : "Restored staging generation failed its integrity check.",
      false,
    );
  }

  // Compute the immutable response and release the prior generation before the
  // commit point. After `active = target` there must be no operation left which
  // can throw and turn a successful publication into a failed protocol result.
  const result = generationResult(target);
  const previous = active;
  try {
    previous?.index.close();
  } catch {
    // The target is still STAGING, so a teardown failure cannot publish it. The
    // abort also releases a restored target's private VFS and JS block store.
    try {
      abortStaging();
    } catch {
      // `abortStaging` detaches the target before closing it. A close failure may
      // affect diagnostics, but it cannot leave the target published or staged.
    }
    throw fixedWorkerError(
      "worker_crashed",
      "lifecycle",
      "In-plugin search Worker failed.",
      true,
    );
  }

  active = target;
  staging = null;
  state = "ready";
  return result;
}

/**
 * Exports the ACTIVE generation, and only while it is clean.
 *
 * "No mutation in flight" needs no flag of its own: `handleMessage` is chained
 * onto a single serialized queue, so no other operation can be part-way through
 * when this one runs. What must be written is the rest: there is no staging
 * generation, and the caller named the generation that is actually active.
 *
 * The asymmetry with `commitBuild` is deliberate and load-bearing. `commitBuild`
 * aborts its staging generation when the gate fails; this operates on the live,
 * serving generation, so a failure here must leave `active` intact, open, and
 * searchable. Destroying a working index because an export failed would be a
 * strictly worse outcome than not having a cache.
 */
async function exportGeneration(
  generation: string,
  cacheIdentity: string,
): Promise<ExportGenerationResult> {
  requireInitialized();
  if (!sqlite || staging !== null || !active || active.id !== generation) {
    throw fixedWorkerError(
      "invalid_state",
      "index",
      "Requested generation is not the clean active generation.",
      true,
    );
  }
  if (declaredChunkingVersion === null) {
    throw fixedWorkerError(
      "invalid_state",
      "index",
      "Portable Rust chunking identity is unavailable.",
      true,
    );
  }
  const target = active;

  // The image that ships must be the image that passed the gate. Clean-built
  // in-memory generations are compacted and rechecked. Block-VFS generations
  // skip VACUUM because it would ratchet the non-shrinking WASM heap; they are
  // still rechecked immediately before their JS blocks are exported.
  try {
    target.index.assertIntegrity();
    if (target.index.requiresPreExportCompaction) target.index.compact();
    target.index.assertIntegrity();
  } catch {
    throw fixedWorkerError(
      "integrity_failed",
      "index",
      "Active generation failed its pre-export integrity check.",
      false,
    );
  }

  const observedChunkingVersion = target.index.chunkingVersion;
  if (observedChunkingVersion !== null && observedChunkingVersion !== declaredChunkingVersion) {
    throw fixedWorkerError(
      "integrity_failed",
      "index",
      "Active generation was chunked by a different chunker.",
      false,
    );
  }

  let image: Uint8Array;
  try {
    image = target.index.exportImage(sqlite);
  } catch (error) {
    if (error instanceof IndexCapacityError) throw indexCapacityError();
    throw fixedWorkerError(
      "internal_error",
      "index",
      "Active generation could not be exported.",
      false,
    );
  }

  // Measured before the response is posted: the buffer is transferred, so the
  // Worker's own view of it is detached by the time anything else could read it.
  const blobSha256 = await sha256Hex(image);
  return {
    generation: target.id,
    documents: target.index.documents,
    chunks: target.index.chunks,
    bytes: image,
    blob_byte_length: image.byteLength,
    blob_sha256: blobSha256,
    protocol_version: WORKER_PROTOCOL_VERSION,
    cache_schema_version: CACHE_SCHEMA_VERSION,
    chunking_version: declaredChunkingVersion,
    sqlite_version: "3.53.0",
    sqlite_wasm_sha256: SQLITE_WASM_SHA256,
    rust_wasm_sha256: RUST_WASM_SHA256,
    plugin_id: PLUGIN_ID,
    plugin_version: PLUGIN_VERSION,
    cache_identity: cacheIdentity,
  };
}

async function restoreGeneration(
  request: Extract<WorkerRequest, { operation: "restore_generation" }>,
): Promise<BuildResult> {
  requireInitialized();
  if (!sqlite || declaredChunkingVersion === null || staging || usedGenerations.has(request.generation)) {
    throw fixedWorkerError(
      "invalid_state",
      "index",
      "Requested generation is unavailable.",
      true,
    );
  }

  // Compatibility is checked against independently known running identities
  // before the candidate bytes are hashed, parsed, copied into blocks, or shown
  // to SQLite.
  if (request.cache_identity !== request.expected_cache_identity
    || request.plugin_id !== PLUGIN_ID) {
    throw fixedWorkerError(
      "cache_identity_mismatch",
      "index",
      "Cached generation belongs to a different identity.",
      false,
    );
  }
  if (request.protocol_version !== WORKER_PROTOCOL_VERSION
    || request.cache_schema_version !== CACHE_SCHEMA_VERSION
    || request.chunking_version !== declaredChunkingVersion
    || request.sqlite_version !== "3.53.0"
    || request.sqlite_wasm_sha256 !== SQLITE_WASM_SHA256
    || request.rust_wasm_sha256 !== RUST_WASM_SHA256
    || request.plugin_version !== PLUGIN_VERSION) {
    throw fixedWorkerError(
      "cache_version_mismatch",
      "index",
      "Cached generation is incompatible with this build.",
      false,
    );
  }
  if (request.bytes.byteLength > MAX_EXPORT_BLOB_BYTES
    || request.blob_byte_length > MAX_EXPORT_BLOB_BYTES) {
    throw fixedWorkerError(
      "cache_blob_too_large",
      "protocol",
      "Cached generation exceeds the restore limit.",
      false,
    );
  }
  if (request.bytes.byteLength === 0
    || request.blob_byte_length !== request.bytes.byteLength) {
    throw fixedWorkerError(
      "cache_image_invalid",
      "index",
      "Cached generation has an invalid length.",
      false,
    );
  }

  if (await sha256Hex(request.bytes) !== request.blob_sha256) {
    throw fixedWorkerError(
      "cache_digest_mismatch",
      "index",
      "Cached generation failed digest verification.",
      false,
    );
  }

  try {
    const header = validateSQLiteImage(request.bytes);
    if (header.wal) throw new CacheImageInvalidError("WAL images require unsupported VFS methods");
  } catch {
    throw fixedWorkerError(
      "cache_image_invalid",
      "index",
      "Cached generation is not a valid restorable SQLite image.",
      false,
    );
  }

  try {
    const index = openRestoredFts5Generation(
      sqlite,
      request.bytes,
      declaredChunkingVersion,
    );
    staging = { id: request.generation, index };
    usedGenerations.add(request.generation);
    state = "building";
    try {
      return publishStaging(staging, false);
    } catch (error) {
      if (isWorkerError(error) && error.code === "integrity_failed") {
        throw fixedWorkerError(
          "cache_image_invalid",
          "index",
          "Cached generation failed its publication integrity gate.",
          false,
        );
      }
      throw error;
    }
  } catch (error) {
    if (error instanceof CacheVersionMismatchError) {
      throw fixedWorkerError(
        "cache_version_mismatch",
        "index",
        "Cached generation schema is incompatible with this build.",
        false,
      );
    }
    if (error instanceof CacheImageInvalidError || error instanceof IndexCapacityError) {
      throw fixedWorkerError(
        "cache_image_invalid",
        "index",
        "Cached generation failed staged validation.",
        false,
      );
    }
    if (error instanceof BlockVfsUnavailableError) {
      throw fixedWorkerError(
        "internal_error",
        "sqlite",
        "Required SQLite restore capability is unavailable.",
        false,
      );
    }
    if (isWorkerError(error)) throw error;
    throw fixedWorkerError(
      "cache_image_invalid",
      "index",
      "Cached generation could not be opened safely.",
      false,
    );
  }
}

function abortBuild(generation: string): BuildResult {
  const target = requireStaging(generation);
  const result = generationResult(target);
  abortStaging();
  return result;
}

function search(query: string, limit: number): SearchResult {
  requireInitialized();
  if (!active) {
    throw fixedWorkerError(
      "index_building",
      "query",
      "In-plugin lexical index is not ready.",
      true,
    );
  }
  try {
    const prepared = prepareQueryWithRust(query);
    const matched = prepared.metadata_probe
      ? active.index.metadataProbe(prepared.metadata_probe)
      : false;
    const finalized = finalizeQueryWithRust(query, matched);
    return {
      generation: active.id,
      hits: active.index.search(finalized.match_plan, limit),
    };
  } catch (error) {
    if (error instanceof RustAdapterError) {
      throw fixedWorkerError(
        "query_rejected",
        "query",
        "The query is unavailable in the in-plugin backend.",
        false,
      );
    }
    throw fixedWorkerError(
      "query_rejected",
      "query",
      "In-plugin lexical search failed.",
      false,
    );
  }
}

function status(): StatusResult {
  if (state === "disposed") {
    return {
      phase: "disposed",
      searchable: false,
      active_generation: null,
      staging_generation: null,
      documents: 0,
      chunks: 0,
      dirty: true,
      rebuilding: false,
    };
  }
  const failed = state === "failed";
  return {
    phase: failed ? "failed" : active && !staging ? "ready" : "building",
    searchable: !failed && active !== null,
    active_generation: active?.id ?? null,
    staging_generation: staging?.id ?? null,
    documents: active?.index.documents ?? 0,
    chunks: active?.index.chunks ?? 0,
    dirty: !active || staging !== null || failed,
    rebuilding: staging !== null,
  };
}

function dispose(): DisposeResult {
  if (state === "disposed") return { closed: true };
  staging?.index.close();
  active?.index.close();
  staging = null;
  active = null;
  usedGenerations.clear();
  sqlite = null;
  declaredChunkingVersion = null;
  state = "disposed";
  return { closed: true };
}

function requireInitialized(): void {
  if (state === "disposed") {
    throw fixedWorkerError("disposed", "lifecycle", "Worker is disposed.", false);
  }
  if (!sqlite || (state !== "ready" && state !== "building")) {
    throw fixedWorkerError("invalid_state", "lifecycle", "Worker is not ready.", true);
  }
}

function requireStaging(generation: string): Generation {
  requireInitialized();
  if (!staging || staging.id !== generation) {
    throw fixedWorkerError(
      "invalid_state",
      "index",
      "Requested staging generation is not active.",
      true,
    );
  }
  return staging;
}

function abortStaging(): void {
  const target = staging;
  // Detach first so even an injected or runtime close failure cannot leave a
  // rejected generation visible as retained STAGING state.
  staging = null;
  state = "ready";
  target?.index.close();
}

function generationResult(generation: Generation): BuildResult {
  return {
    generation: generation.id,
    documents: generation.index.documents,
    chunks: generation.index.chunks,
  };
}

function indexCapacityError(): WorkerError {
  return fixedWorkerError(
    "index_limit_exceeded",
    "index",
    "In-plugin index capacity was exceeded.",
    false,
  );
}

function sourceChangeError(error: unknown): WorkerError {
  if (error instanceof IndexCapacityError) return indexCapacityError();
  if (error instanceof IndexIntegrityError) {
    return fixedWorkerError(
      "integrity_failed",
      "index",
      "In-plugin index failed its integrity check.",
      false,
    );
  }
  if (error instanceof RustAdapterError) {
    return fixedWorkerError(
      "source_rejected",
      "rust",
      "Portable Rust rejected a source batch.",
      false,
    );
  }
  return fixedWorkerError(
    "source_rejected",
    "index",
    "In-plugin index rejected a source batch.",
    false,
  );
}

async function dispatch(request: WorkerRequest): Promise<unknown> {
  if (request.id <= lastRequestId) {
    throw fixedWorkerError(
      "invalid_request",
      "protocol",
      "Worker request ID is duplicate or stale.",
      false,
    );
  }
  lastRequestId = request.id;
  switch (request.operation) {
    case "initialize":
      return initialize();
    case "begin_build":
      return beginBuild(request.generation);
    case "add_source_batch":
      return addSourceBatch(request.generation, request.sources);
    case "apply_source_changes":
      return applySourceChanges(request);
    case "commit_build":
      return commitBuild(request.generation);
    case "abort_build":
      return abortBuild(request.generation);
    case "export_generation":
      return exportGeneration(request.generation, request.cache_identity);
    case "restore_generation":
      return restoreGeneration(request);
    case "search":
      return search(request.query, request.limit);
    case "status":
      return status();
    case "dispose":
      return dispose();
  }
}

async function handleMessage(event: MessageEvent<unknown>): Promise<void> {
  const parsed = parseWorkerRequest(event.data);
  const identity = responseIdentity(event.data);
  let response: WorkerResponse;
  if (isWorkerError(parsed)) {
    response = {
      version: WORKER_PROTOCOL_VERSION,
      ...identity,
      ok: false,
      error: parsed,
    };
  } else {
    try {
      const result = await dispatch(parsed);
      response = {
        version: WORKER_PROTOCOL_VERSION,
        id: parsed.id,
        operation: parsed.operation,
        ok: true,
        result,
      } as WorkerResponse;
    } catch (error) {
      response = {
        version: WORKER_PROTOCOL_VERSION,
        id: parsed.id,
        operation: parsed.operation,
        ok: false,
        error: isWorkerError(error)
          ? error
          : fixedWorkerError(
              "internal_error",
              "lifecycle",
              "In-plugin search Worker failed.",
              false,
            ),
      };
    }
  }
  // Posting is itself fallible — the transfer list is the realistic source, and
  // a buffer that cannot be transferred raises DataCloneError. Left to
  // propagate, that throw escapes `handleMessage` entirely and rejects the
  // serialized queue, after which every later message is silently dropped and
  // the user sees every request die on the RPC timeout instead of a reported
  // fault. A failure to post the result is reported as a failure, without the
  // transfer list that could not be honoured.
  const transfer = transferListFor(response);
  try {
    if (transfer.length === 0) scope.postMessage(response);
    else scope.postMessage(response, transfer);
  } catch {
    scope.postMessage({
      version: WORKER_PROTOCOL_VERSION,
      id: response.id,
      operation: response.operation,
      ok: false,
      error: fixedWorkerError(
        "internal_error",
        "lifecycle",
        "In-plugin search Worker failed.",
        false,
      ),
    } satisfies WorkerResponse);
    return;
  }
  if (!isWorkerError(parsed) && parsed.operation === "dispose" && response.ok) {
    setTimeout(() => scope.close(), 0);
  }
}

/**
 * Only a successful export moves a buffer, and only the image buffer. A failed
 * response cannot carry bytes at all: the error envelope's key set is exact, so
 * a leaked blob would fail response validation on the host rather than be
 * quietly accepted.
 */
function transferListFor(response: WorkerResponse): Transferable[] {
  if (!response.ok || response.operation !== "export_generation") return [];
  const result = response.result as Partial<ExportGenerationResult>;
  return result.bytes instanceof Uint8Array ? [result.bytes.buffer as ArrayBuffer] : [];
}

function responseIdentity(value: unknown): { id: number; operation: WorkerOperation } {
  if (typeof value === "object" && value !== null) {
    const candidate = value as { id?: unknown; operation?: unknown };
    const id = typeof candidate.id === "number"
      && Number.isSafeInteger(candidate.id)
      && candidate.id >= 1
      ? candidate.id
      : 1;
    const operation = isOperation(candidate.operation) ? candidate.operation : "status";
    return { id, operation };
  }
  return { id: 1, operation: "status" };
}

function isOperation(value: unknown): value is WorkerOperation {
  return value === "initialize"
    || value === "begin_build"
    || value === "add_source_batch"
    || value === "apply_source_changes"
    || value === "commit_build"
    || value === "abort_build"
    || value === "export_generation"
    || value === "restore_generation"
    || value === "search"
    || value === "status"
    || value === "dispose";
}

function isWorkerError(value: unknown): value is WorkerError {
  if (typeof value !== "object" || value === null) return false;
  const candidate = value as Partial<WorkerError>;
  return typeof candidate.code === "string"
    && typeof candidate.stage === "string"
    && typeof candidate.message === "string"
    && typeof candidate.retryable === "boolean";
}

async function verifyArtifact(
  bytes: Uint8Array,
  expectedSize: number,
  expectedSha256: string,
): Promise<void> {
  if (bytes.byteLength !== expectedSize) {
    throw fixedWorkerError("artifact_mismatch", "artifact", "Embedded WASM artifact mismatch.", false);
  }
  const actual = await sha256Hex(bytes.slice());
  if (actual !== expectedSha256) {
    throw fixedWorkerError("artifact_mismatch", "artifact", "Embedded WASM artifact mismatch.", false);
  }
}

async function sha256Hex(data: Uint8Array): Promise<string> {
  const digest = await crypto.subtle.digest("SHA-256", data as unknown as BufferSource);
  return Array.from(new Uint8Array(digest), (value) =>
    value.toString(16).padStart(2, "0")
  ).join("");
}

function installGuards(): GuardCounters {
  const counters: GuardCounters = {
    networkAttempts: 0,
    persistenceAttempts: 0,
    helperWorkerAttempts: 0,
  };
  for (const property of ["fetch", "XMLHttpRequest", "WebSocket", "EventSource", "importScripts"]) {
    denyProperty(globalThis, property, counters, "networkAttempts");
  }
  denyProperty(globalThis, "Worker", counters, "helperWorkerAttempts");
  for (const property of ["indexedDB", "localStorage", "sessionStorage"]) {
    disableProperty(globalThis, property);
  }
  if (typeof navigator === "object" && navigator !== null) {
    disableProperty(navigator, "storage");
  }
  return counters;
}

function installPersistenceGuards(counters: GuardCounters): void {
  for (const property of ["indexedDB", "localStorage", "sessionStorage"]) {
    denyProperty(globalThis, property, counters, "persistenceAttempts");
  }
  if (typeof navigator === "object" && navigator !== null) {
    denyProperty(navigator, "storage", counters, "persistenceAttempts");
  }
}

function denyProperty(
  target: object,
  property: PropertyKey,
  counters: GuardCounters,
  counter: keyof GuardCounters,
): void {
  Object.defineProperty(target, property, {
    configurable: true,
    get() {
      counters[counter] += 1;
      throw new Error("denied Worker capability");
    },
    set() {
      counters[counter] += 1;
      throw new Error("denied Worker capability");
    },
  });
}

function disableProperty(target: object, property: PropertyKey): void {
  Object.defineProperty(target, property, {
    configurable: true,
    writable: false,
    value: undefined,
  });
}

let messageQueue = Promise.resolve();
scope.addEventListener("message", (event: MessageEvent<unknown>) => {
  // The chain is re-resolved after every link. A rejection left in it would
  // poison it permanently: every subsequent message would be dropped without a
  // response, and the user would see every request die on the RPC timeout
  // rather than receive a reported fault.
  messageQueue = messageQueue
    .then(() => handleMessage(event))
    .catch(() => undefined);
});
