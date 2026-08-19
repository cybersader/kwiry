// SPDX-FileCopyrightText: 2026 cybersader
// SPDX-License-Identifier: GPL-3.0-only
/// <reference lib="webworker" />

import sqlite3InitModule from "@sqlite.org/sqlite-wasm";
import sqliteWasmBytes from "@sqlite.org/sqlite-wasm/sqlite3.wasm";
import rustWasmBytes from "virtual:kwiry-rust-wasm-bytes";
import { createInternalD5cPreviewHandler } from "virtual:kwiry-internal-d5c-preview";
import { createInternalPrototypeHandler } from "virtual:kwiry-internal-prototype";
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
  isD5cOwnerWorkerOperation,
  parseD5cOwnerWorkerRequest,
  type D5cOwnerWorkerRequest,
} from "virtual:kwiry-owner-worker-protocol";
import {
  CacheImageInvalidError,
  CacheVersionMismatchError,
  DEFAULT_DATABASE_BYTE_LIMIT,
  IndexCapacityError,
  IndexIntegrityError,
  MAX_INDEX_CHUNKS,
  openFts5Generation,
  openRestoredFts5Generation,
  type Fts5GenerationIndex,
  type SQLiteApi,
} from "./fts5-index";
import { validateSQLiteImage } from "./image-header";
import {
  CACHE_SCHEMA_VERSION,
  INITIAL_BUILD_CHECKPOINT_IMAGE_VERSION,
  INITIAL_BUILD_CHECKPOINT_RECORD_KIND,
  INITIAL_BUILD_CHECKPOINT_RECORD_VERSION,
  MAX_EXPORT_BLOB_BYTES,
  SOURCE_FORMATS,
  SOURCE_QUARANTINE_WARNING_CODE,
  WORKER_PROTOCOL_VERSION,
  type BuildResult,
  type DisposeResult,
  type ExportGenerationResult,
  type InitialBuildCheckpointExportResult,
  type InitialBuildCheckpointReconciliationPlanResult,
  type InitializeResult,
  type ReconciliationPlanResult,
  type RestoreGenerationResult,
  type RestoreInitialBuildCheckpointResult,
  type SearchResult,
  type SourceFormat,
  type SourcePreparationDefectField,
  type SourceRemoval,
  type SourceUpsert,
  type StatusResult,
  type WorkerError,
  type WorkerOperation,
  type WorkerRequest,
  type WorkerResponse,
  emptySourceFormatCounts,
  classifyWorkerCause,
  fixedWorkerError,
  isSourcePreparationDefectField,
  parseWorkerRequest,
} from "./protocol";
import {
  RustAdapterError,
  finalizeQueryWithRust,
  initializeRustAdapter,
  prepareOversizedSourceWithRust,
  prepareQueryWithRust,
  prepareSourceWithRust,
  type SourcePreparation,
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
  quarantinedSources: Map<string, SourcePreparationDefectField>;
}

interface PreparedSourceBatch {
  preparations: SourcePreparation[];
  quarantined: Map<string, SourcePreparationDefectField>;
}

interface GuardCounters {
  networkAttempts: number;
  persistenceAttempts: number;
  helperWorkerAttempts: number;
}

const INITIAL_BUILD_CHECKPOINT_MAGIC = Uint8Array.of(
  0x4b, 0x57, 0x49, 0x52, 0x59, 0x43, 0x50, 0x00,
);
const INITIAL_BUILD_CHECKPOINT_HEADER_BYTES = INITIAL_BUILD_CHECKPOINT_MAGIC.byteLength + 8;

const scope = self as DedicatedWorkerGlobalScope;
let state: WorkerState = "cold";
let sqlite: SQLiteApi | null = null;
let active: Generation | null = null;
let staging: Generation | null = null;
let stagingIsInitialCold = false;
let stagingRestoredCheckpoint = false;
let stagingPreviewEligible = false;
let stagingRevision = 0;
const usedGenerations = new Set<string>();
let lastRequestId = 0;
let guards: GuardCounters | null = null;
// Declared by the Rust adapter at initialize. A generation with no chunks
// still has to be able to name the chunking contract its image was built
// under, so the adapter identity — not an observed chunk — is the source.
let declaredChunkingVersion: number | null = null;
let initializedVaultId: string | null = null;
let initializedSourcePolicyHash: string | null = null;
// Configuration, not identity: never digested, never persisted, and never a
// reason to refuse a cache. It exists so a restored image can be projected
// down to the formats the user currently wants before it answers a query.
let initializedEnabledFormats: readonly SourceFormat[] | null = null;
const handleInternalPrototypeMessage = createInternalPrototypeHandler({
  scope,
  getActive: () => active,
  requireInitialized,
  search,
  getLastRequestId: () => lastRequestId,
  setLastRequestId: (id) => { lastRequestId = id; },
  mapError: (error) => isWorkerError(error)
    ? error
    : fixedWorkerError("internal_error", "query", "Internal prototype failed.", false),
});
const handleInternalD5cPreviewMessage = __KWIRY_D5C_OWNER_WORKER__
  ? createInternalD5cPreviewHandler({
      scope,
      resolveTarget: (generation, revision) => {
        if (active !== null) {
          return active.id === generation && revision === null
            ? { ...active, publication: "active" as const, revision: null }
            : null;
        }
        return staging !== null
          && stagingIsInitialCold
          && stagingPreviewEligible
          && staging.id === generation
          && revision === stagingRevision
          ? { ...staging, publication: "initial_staging" as const, revision: stagingRevision }
          : null;
      },
      getInitializedVaultId: requireInitializedVaultId,
      requireInitialized,
      getLastRequestId: () => lastRequestId,
      setLastRequestId: (id) => { lastRequestId = id; },
      mapError: (error) => isWorkerError(error)
        ? error
        : fixedWorkerError("internal_error", "query", "Internal preview failed.", false),
    })
  : async () => false;

async function initialize(
  vaultId: string,
  sourcePolicyHash = "c414b56f31d22f8e1fbe69f5074bc8862337d1c8ee6065b6ad0da441b4f63860",
  enabledFormats: readonly SourceFormat[] = SOURCE_FORMATS,
): Promise<InitializeResult> {
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
    initializedVaultId = vaultId;
    initializedSourcePolicyHash = sourcePolicyHash;
    initializedEnabledFormats = [...enabledFormats];

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
    initializedVaultId = null;
    initializedSourcePolicyHash = null;
    initializedEnabledFormats = null;
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
  staging = {
    id: generation,
    index: openFts5Generation(
      sqlite,
      undefined,
      requireInitializedVaultId(),
      requireInitializedSourcePolicyHash(),
    ),
    quarantinedSources: new Map(),
  };
  stagingIsInitialCold = active === null;
  stagingRestoredCheckpoint = false;
  stagingPreviewEligible = false;
  stagingRevision = 0;
  usedGenerations.add(generation);
  state = "building";
  return generationResult(staging);
}

async function addSourceBatch(
  generation: string,
  sources: Extract<WorkerRequest, { operation: "add_source_batch" }>["sources"],
): Promise<BuildResult> {
  const target = requireStaging(generation);
  try {
    const prepared = await prepareSourceBatch(sources);
    target.index.applySourceChanges(prepared.preparations, []);
    updateQuarantinedSources(target, sources, [], prepared.quarantined);
    if (stagingIsInitialCold) {
      stagingRevision += 1;
      stagingPreviewEligible = !stagingRestoredCheckpoint
        && target.index.documents > 0
        && target.index.chunks > 0;
    }
    return generationResult(target);
  } catch (error) {
    abortStaging();
    if (error instanceof IndexCapacityError) throw indexCapacityError();
    throw sourceChangeError(error);
  }
}

async function applySourceChanges(
  request: Extract<WorkerRequest, { operation: "apply_source_changes" }>,
): Promise<BuildResult> {
  requireInitialized();
  if (request.next_generation === null) {
    const target = requireStaging(request.generation);
    try {
      const prepared = await prepareSourceBatch(request.upserts);
      target.index.applySourceChanges(prepared.preparations, request.removals);
      updateQuarantinedSources(
        target,
        request.upserts,
        request.removals,
        prepared.quarantined,
      );
      stagingRevision += 1;
      stagingPreviewEligible = false;
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
    const prepared = await prepareSourceBatch(request.upserts);
    // In place on a published generation: there is no later commit gate, so
    // the reconciliation runs inside this transaction. A divergence rolls the
    // batch back and is reported instead of quietly living in the active index.
    active.index.applySourceChanges(prepared.preparations, request.removals, true);
    updateQuarantinedSources(
      active,
      request.upserts,
      request.removals,
      prepared.quarantined,
    );
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
    // The staging index was assembled only through allocator-owned transactions.
    // Do the cheap structural gate before compaction and the full canonical exact
    // projection comparison after compaction, immediately before publication.
    target.index.assertIntegrity(false);
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
    } catch (error) {
      abortStaging();
      if (error instanceof IndexCapacityError) throw indexCapacityError();
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
  stagingIsInitialCold = false;
  stagingRestoredCheckpoint = false;
  stagingPreviewEligible = false;
  stagingRevision = 0;
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
    target.index.assertIntegrity(false);
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
    source_policy_hash: requireInitializedSourcePolicyHash(),
  };
}

async function exportInitialBuildCheckpoint(
  request: Extract<WorkerRequest, { operation: "export_initial_build_checkpoint" }>,
): Promise<InitialBuildCheckpointExportResult> {
  requireInitialized();
  if (!sqlite
    || active !== null
    || !staging
    || staging.id !== request.generation
    || !stagingIsInitialCold) {
    throw fixedWorkerError(
      "invalid_state",
      "index",
      "Requested generation is not an initial-cold staging generation.",
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
  const target = staging;

  try {
    target.index.assertIntegrity(false);
    target.index.assertIntegrity();
  } catch {
    throw fixedWorkerError(
      "integrity_failed",
      "index",
      "Initial-build checkpoint failed its integrity check.",
      false,
    );
  }

  const observedChunkingVersion = target.index.chunkingVersion;
  if (observedChunkingVersion !== null && observedChunkingVersion !== declaredChunkingVersion) {
    throw fixedWorkerError(
      "integrity_failed",
      "index",
      "Initial-build checkpoint was chunked by a different chunker.",
      false,
    );
  }

  let payload: Uint8Array;
  try {
    payload = target.index.exportImage(sqlite);
  } catch (error) {
    if (error instanceof IndexCapacityError) throw indexCapacityError();
    throw fixedWorkerError(
      "internal_error",
      "index",
      "Initial-build checkpoint could not be exported.",
      false,
    );
  }

  let bytes: Uint8Array;
  try {
    bytes = wrapInitialBuildCheckpoint(payload);
  } catch (error) {
    if (error instanceof IndexCapacityError) {
      throw fixedWorkerError(
        "checkpoint_blob_too_large",
        "index",
        "Initial-build checkpoint exceeds the export limit.",
        false,
      );
    }
    throw error;
  }
  const blobSha256 = await sha256Hex(bytes);
  return {
    ...generationResult(target),
    record_kind: INITIAL_BUILD_CHECKPOINT_RECORD_KIND,
    checkpoint_record_version: INITIAL_BUILD_CHECKPOINT_RECORD_VERSION,
    checkpoint_image_version: INITIAL_BUILD_CHECKPOINT_IMAGE_VERSION,
    publication: "initial_staging",
    searchable: false,
    cursor: request.cursor,
    bytes,
    blob_byte_length: bytes.byteLength,
    blob_sha256: blobSha256,
    protocol_version: WORKER_PROTOCOL_VERSION,
    cache_schema_version: CACHE_SCHEMA_VERSION,
    chunking_version: declaredChunkingVersion,
    sqlite_version: "3.53.0",
    sqlite_wasm_sha256: SQLITE_WASM_SHA256,
    rust_wasm_sha256: RUST_WASM_SHA256,
    plugin_id: PLUGIN_ID,
    plugin_version: PLUGIN_VERSION,
    cache_identity: request.cache_identity,
    source_policy_hash: requireInitializedSourcePolicyHash(),
  };
}

async function restoreGeneration(
  request: Extract<WorkerRequest, { operation: "restore_generation" }>,
): Promise<RestoreGenerationResult> {
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
    || request.source_policy_hash !== request.expected_source_policy_hash
    || request.source_policy_hash !== requireInitializedSourcePolicyHash()
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
      undefined,
      requireInitializedVaultId(),
      requireInitializedSourcePolicyHash(),
      requireInitializedEnabledFormats(),
    );
    staging = {
      id: request.generation,
      index,
      quarantinedSources: new Map(),
    };
    usedGenerations.add(request.generation);
    state = "building";
    try {
      const published = publishStaging(staging, false);
      // The eviction report travels with the publication so the host can say
      // what was refused at the moment it starts serving the rest.
      return { ...published, evictions: index.evictions };
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

async function restoreInitialBuildCheckpoint(
  request: Extract<WorkerRequest, { operation: "restore_initial_build_checkpoint" }>,
): Promise<RestoreInitialBuildCheckpointResult> {
  requireInitialized();
  if (!sqlite
    || declaredChunkingVersion === null
    || active !== null
    || staging !== null
    || usedGenerations.has(request.generation)) {
    throw fixedWorkerError(
      "invalid_state",
      "index",
      "Requested initial-build checkpoint generation is unavailable.",
      true,
    );
  }

  if (request.record_kind !== INITIAL_BUILD_CHECKPOINT_RECORD_KIND) {
    throw fixedWorkerError(
      "checkpoint_kind_mismatch",
      "index",
      "Initial-build checkpoint has the wrong record kind.",
      false,
    );
  }
  if (request.cache_identity !== request.expected_cache_identity
    || request.source_policy_hash !== request.expected_source_policy_hash
    || request.source_policy_hash !== requireInitializedSourcePolicyHash()
    || request.plugin_id !== PLUGIN_ID) {
    throw fixedWorkerError(
      "checkpoint_identity_mismatch",
      "index",
      "Initial-build checkpoint belongs to a different identity.",
      false,
    );
  }
  if (request.checkpoint_record_version !== INITIAL_BUILD_CHECKPOINT_RECORD_VERSION
    || request.checkpoint_image_version !== INITIAL_BUILD_CHECKPOINT_IMAGE_VERSION
    || request.protocol_version !== WORKER_PROTOCOL_VERSION
    || request.cache_schema_version !== CACHE_SCHEMA_VERSION
    || request.chunking_version !== declaredChunkingVersion
    || request.sqlite_version !== "3.53.0"
    || request.sqlite_wasm_sha256 !== SQLITE_WASM_SHA256
    || request.rust_wasm_sha256 !== RUST_WASM_SHA256
    || request.plugin_version !== PLUGIN_VERSION) {
    throw fixedWorkerError(
      "checkpoint_version_mismatch",
      "index",
      "Initial-build checkpoint is incompatible with this build.",
      false,
    );
  }
  if (request.bytes.byteLength > MAX_EXPORT_BLOB_BYTES
    || request.blob_byte_length > MAX_EXPORT_BLOB_BYTES) {
    throw fixedWorkerError(
      "checkpoint_blob_too_large",
      "protocol",
      "Initial-build checkpoint exceeds the restore limit.",
      false,
    );
  }
  if (request.bytes.byteLength === 0
    || request.blob_byte_length !== request.bytes.byteLength) {
    throw fixedWorkerError(
      "checkpoint_image_invalid",
      "index",
      "Initial-build checkpoint has an invalid length.",
      false,
    );
  }
  if (await sha256Hex(request.bytes) !== request.blob_sha256) {
    throw fixedWorkerError(
      "checkpoint_digest_mismatch",
      "index",
      "Initial-build checkpoint failed digest verification.",
      false,
    );
  }

  let payload: Uint8Array;
  try {
    payload = unwrapInitialBuildCheckpoint(request.bytes);
    const header = validateSQLiteImage(payload);
    if (header.wal) throw new CacheImageInvalidError("WAL images require unsupported VFS methods");
  } catch {
    throw fixedWorkerError(
      "checkpoint_image_invalid",
      "index",
      "Initial-build checkpoint is not a valid staged SQLite image.",
      false,
    );
  }

  let candidate: Fts5GenerationIndex | null = null;
  try {
    candidate = openRestoredFts5Generation(
      sqlite,
      payload,
      declaredChunkingVersion,
      undefined,
      requireInitializedVaultId(),
      requireInitializedSourcePolicyHash(),
      requireInitializedEnabledFormats(),
    );
    const restored: Generation = {
      id: request.generation,
      index: candidate,
      quarantinedSources: new Map(),
    };
    const result: RestoreInitialBuildCheckpointResult = {
      ...generationResult(restored),
      record_kind: INITIAL_BUILD_CHECKPOINT_RECORD_KIND,
      publication: "initial_staging",
      searchable: false,
      cursor: request.cursor,
      evictions: candidate.evictions,
    };
    staging = restored;
    candidate = null;
    stagingIsInitialCold = true;
    stagingRestoredCheckpoint = true;
    stagingPreviewEligible = false;
    stagingRevision = 0;
    usedGenerations.add(request.generation);
    state = "building";
    return result;
  } catch (error) {
    candidate?.close();
    if (error instanceof CacheVersionMismatchError) {
      throw fixedWorkerError(
        "checkpoint_version_mismatch",
        "index",
        "Initial-build checkpoint schema is incompatible with this build.",
        false,
      );
    }
    if (error instanceof CacheImageInvalidError || error instanceof IndexCapacityError) {
      throw fixedWorkerError(
        "checkpoint_image_invalid",
        "index",
        "Initial-build checkpoint failed staged validation.",
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
      "checkpoint_image_invalid",
      "index",
      "Initial-build checkpoint could not be opened safely.",
      false,
    );
  }
}

function planReconciliation(
  request: Extract<WorkerRequest, { operation: "plan_reconciliation" }>,
): ReconciliationPlanResult {
  requireInitialized();
  if (staging || !active || active.id !== request.generation) {
    throw fixedWorkerError(
      "invalid_state",
      "index",
      "Requested generation is not the clean active generation.",
      true,
    );
  }
  try {
    return {
      generation: active.id,
      ...active.index.planReconciliation(request.vault_id, request.current_sources),
    };
  } catch (error) {
    if (error instanceof IndexCapacityError) throw indexCapacityError();
    if (error instanceof IndexIntegrityError) {
      throw fixedWorkerError(
        "integrity_failed",
        "index",
        "Active generation source inventory is invalid.",
        false,
      );
    }
    throw fixedWorkerError(
      "internal_error",
      "index",
      "Active generation could not be reconciled.",
      false,
    );
  }
}

function planInitialBuildCheckpointReconciliation(
  request: Extract<WorkerRequest, {
    operation: "plan_initial_build_checkpoint_reconciliation";
  }>,
): InitialBuildCheckpointReconciliationPlanResult {
  requireInitialized();
  if (active !== null
    || !staging
    || staging.id !== request.generation
    || !stagingIsInitialCold
    || !stagingRestoredCheckpoint) {
    throw fixedWorkerError(
      "invalid_state",
      "index",
      "Requested generation is not a restored initial-build checkpoint.",
      true,
    );
  }
  try {
    return {
      generation: staging.id,
      publication: "initial_staging",
      searchable: false,
      ...staging.index.planReconciliation(request.vault_id, request.current_sources),
    };
  } catch (error) {
    if (error instanceof IndexCapacityError) throw indexCapacityError();
    if (error instanceof IndexIntegrityError) {
      throw fixedWorkerError(
        "integrity_failed",
        "index",
        "Initial-build checkpoint source inventory is invalid.",
        false,
      );
    }
    throw fixedWorkerError(
      "internal_error",
      "index",
      "Initial-build checkpoint could not be reconciled.",
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
  const trace = active.index.beginInternalLexicalTrace();
  let traceFinished = false;
  try {
    const prepared = prepareQueryWithRust(query);
    const evidence = active.index.observeQuery(prepared.probes, trace);
    const finalized = finalizeQueryWithRust(query, evidence);
    const collected = active.index.searchWithCandidateWindow(
      finalized.execution_plan,
      limit,
      trace,
    );
    active.index.finishInternalLexicalTrace(trace);
    traceFinished = true;
    return {
      generation: active.id,
      hits: collected.hits,
      candidate_window: collected.candidate_window,
    };
  } catch (error) {
    if (!traceFinished) {
      try {
        active.index.finishInternalLexicalTrace(trace);
      } catch {
        // A trace is diagnostic state only and must never replace the query failure.
      }
    }
    if (error instanceof RustAdapterError) throw rustQueryError(error);
    // The thrown value itself is discarded on purpose: an exception message can
    // quote SQL, the query, or vault text. Its classification is safe to keep,
    // and without it a failure report cannot say anything about its own cause.
    throw fixedWorkerError(
      "query_execution_failed",
      "query",
      "In-plugin lexical search could not complete.",
      true,
      classifyWorkerCause(error),
    );
  }
}

function rustQueryError(error: RustAdapterError): WorkerError {
  switch (error.code) {
    case "explicit_query_unsupported":
      return fixedWorkerError(
        "explicit_query_unsupported",
        "query",
        "This explicit query is unavailable in the in-plugin backend.",
        false,
      );
    case "invalid_query":
      return fixedWorkerError(
        "invalid_query",
        "query",
        "The query is invalid or exceeds the supported limits.",
        false,
      );
    case "invalid_query_plan":
    case "invalid_request":
    case "abi_mismatch":
    case "invalid_response":
    default:
      return fixedWorkerError(
        "invalid_query_plan",
        "rust",
        "Portable Rust returned invalid query data.",
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
      active_database_bytes: 0,
      staging_database_bytes: 0,
      database_byte_limit: DEFAULT_DATABASE_BYTE_LIMIT,
      source_format_counts: emptySourceFormatCounts(),
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
    active_database_bytes: active?.index.databaseBytes ?? 0,
    staging_database_bytes: staging?.index.databaseBytes ?? 0,
    database_byte_limit: active?.index.databaseByteLimit
      ?? staging?.index.databaseByteLimit
      ?? DEFAULT_DATABASE_BYTE_LIMIT,
    source_format_counts: active?.index.sourceFormatCounts ?? emptySourceFormatCounts(),
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
  stagingIsInitialCold = false;
  stagingRestoredCheckpoint = false;
  stagingPreviewEligible = false;
  stagingRevision = 0;
  usedGenerations.clear();
  sqlite = null;
  declaredChunkingVersion = null;
  initializedVaultId = null;
  state = "disposed";
  return { closed: true };
}

function requireInitializedVaultId(): string {
  requireInitialized();
  if (initializedVaultId === null) {
    throw fixedWorkerError("invalid_state", "lifecycle", "Worker vault is unavailable.", false);
  }
  return initializedVaultId;
}

function requireInitializedSourcePolicyHash(): string {
  requireInitialized();
  if (initializedSourcePolicyHash === null) {
    throw fixedWorkerError("invalid_state", "lifecycle", "Worker source policy is unavailable.", false);
  }
  return initializedSourcePolicyHash;
}

function requireInitializedEnabledFormats(): readonly SourceFormat[] {
  requireInitialized();
  if (initializedEnabledFormats === null) {
    throw fixedWorkerError(
      "invalid_state",
      "lifecycle",
      "Worker source format selection is unavailable.",
      false,
    );
  }
  return initializedEnabledFormats;
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
  stagingIsInitialCold = false;
  stagingRestoredCheckpoint = false;
  stagingPreviewEligible = false;
  stagingRevision = 0;
  state = "ready";
  target?.index.close();
}

function generationResult(generation: Generation): BuildResult {
  return {
    generation: generation.id,
    documents: generation.index.documents,
    chunks: generation.index.chunks,
    database_bytes: generation.index.databaseBytes,
    database_byte_limit: generation.index.databaseByteLimit,
    quarantined_sources: generation.quarantinedSources.size,
    quarantine_fields: [...new Set(generation.quarantinedSources.values())].sort(),
    source_format_counts: generation.index.sourceFormatCounts,
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

async function prepareSourceBatch(sources: readonly SourceUpsert[]): Promise<PreparedSourceBatch> {
  const preparations: SourcePreparation[] = [];
  const quarantined = new Map<string, SourcePreparationDefectField>();
  let preparedChunks = 0;
  for (const source of sources) {
    try {
      const preparation = prepareSourceUpsert(source);
      preparedChunks += preparation.kind === "indexed" ? preparation.chunks.length : 0;
      if (!Number.isSafeInteger(preparedChunks) || preparedChunks > MAX_INDEX_CHUNKS) {
        throw new IndexCapacityError();
      }
      preparations.push(preparation);
    } catch (error) {
      const defectField = quarantinablePreparationDefect(error);
      if (defectField === null) throw error;
      const identity = sourceIdentity(source.descriptor.vault_id, source.descriptor.path);
      preparations.push(await quarantinedPreparation(source));
      quarantined.set(identity, defectField);
    }
  }
  return { preparations, quarantined };
}

function prepareSourceUpsert(source: SourceUpsert): SourcePreparation {
  return "bytes" in source
    ? prepareSourceWithRust(source.descriptor, source.bytes)
    : prepareOversizedSourceWithRust(source.descriptor);
}

function quarantinablePreparationDefect(error: unknown): SourcePreparationDefectField | null {
  if (!(error instanceof RustAdapterError)) return null;
  const field = (error as { defectField?: unknown }).defectField;
  // A validated defectField proves the TypeScript ABI validator rejected this
  // one preparation. RustAdapterError without one includes malformed JSON,
  // invalid envelopes, ABI drift, and adapter failures, so it aborts. IndexCapacityError,
  // IndexIntegrityError, and untyped SQLite errors arise after this function and
  // also abort; Worker crashes and RPC timeouts are lifecycle failures outside
  // this path and can never be converted into skipped sources.
  return isSourcePreparationDefectField(field) ? field : null;
}

async function quarantinedPreparation(source: SourceUpsert): Promise<SourcePreparation> {
  const descriptor = source.descriptor;
  const filename = descriptor.path.split("/").at(-1) ?? descriptor.path;
  const separator = filename.lastIndexOf(".");
  return {
    schema_version: 9,
    source_key: await sourceKey(descriptor.vault_id, descriptor.path),
    vault_id: descriptor.vault_id,
    ...(descriptor.room === undefined ? {} : { room: descriptor.room }),
    path: descriptor.path,
    format: descriptor.format,
    // The plugin only ever runs the portable extractor set, so a preparation
    // it synthesizes for a quarantined source names that set rather than
    // leaving the field to be inferred.
    extraction_profile: "portable",
    coverage: "quarantined",
    content_hash: null,
    byte_length: descriptor.byte_length,
    mtime: descriptor.mtime,
    mtime_nanos: descriptor.mtime_nanos,
    retrieval: {
      filename,
      stem: separator > 0 ? filename.slice(0, separator) : filename,
      aliases: [],
    },
    normalized_exact: {
      filename: null,
      stem: null,
      aliases: [],
      title: null,
    },
    frontmatter: {},
    chunks: [],
    kind: "skipped",
    warning: SOURCE_QUARANTINE_WARNING_CODE,
  };
}

function updateQuarantinedSources(
  generation: Generation,
  upserts: readonly SourceUpsert[],
  removals: readonly SourceRemoval[],
  quarantined: ReadonlyMap<string, SourcePreparationDefectField>,
): void {
  for (const source of upserts) {
    generation.quarantinedSources.delete(
      sourceIdentity(source.descriptor.vault_id, source.descriptor.path),
    );
  }
  for (const removal of removals) {
    generation.quarantinedSources.delete(sourceIdentity(removal.vault_id, removal.path));
  }
  for (const [identity, field] of quarantined) {
    generation.quarantinedSources.set(identity, field);
  }
}

// A later successful retry arrives with Rust's path-derived key. Reproducing
// that contract here lets it replace the quarantine row instead of tripping the
// index's source-key/identity conflict check.
async function sourceKey(vaultId: string, path: string): Promise<string> {
  const encoder = new TextEncoder();
  const parts = [encoder.encode("kwiry-source-v1\0")];
  for (const component of [encoder.encode(vaultId), encoder.encode(path)]) {
    const length = new Uint8Array(8);
    new DataView(length.buffer).setBigUint64(0, BigInt(component.byteLength), true);
    parts.push(length, component);
  }
  const bytes = new Uint8Array(parts.reduce((total, part) => total + part.byteLength, 0));
  let offset = 0;
  for (const part of parts) {
    bytes.set(part, offset);
    offset += part.byteLength;
  }
  return sha256Hex(bytes);
}

function sourceIdentity(vaultId: string, path: string): string {
  return JSON.stringify([vaultId, path]);
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
    if (error.code === "index_limit_exceeded") return indexCapacityError();
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

async function dispatchProduction(request: WorkerRequest): Promise<unknown> {
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
      return initialize(
        request.vault_id,
        request.source_policy_hash,
        request.enabled_source_formats,
      );
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
    case "plan_reconciliation":
      return planReconciliation(request);
    case "export_initial_build_checkpoint":
      return exportInitialBuildCheckpoint(request);
    case "restore_initial_build_checkpoint":
      return restoreInitialBuildCheckpoint(request);
    case "plan_initial_build_checkpoint_reconciliation":
      return planInitialBuildCheckpointReconciliation(request);
    case "search":
      return search(request.query, request.limit);
    case "status":
      return status();
    case "dispose":
      return dispose();
  }
}

async function dispatchOwner(
  request: D5cOwnerWorkerRequest,
): Promise<unknown> {
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
      return initialize(request.vault_id);
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
    case "search":
      return search(request.query, request.limit);
    case "status":
      return status();
    case "dispose":
      return dispose();
  }
}

async function handleMessage(event: MessageEvent<unknown>): Promise<void> {
  if (await handleInternalD5cPreviewMessage(event.data)) return;
  if (await handleInternalPrototypeMessage(event.data)) return;
  const parsed = __KWIRY_D5C_OWNER_WORKER__
    ? parseD5cOwnerWorkerRequest(event.data)
    : parseWorkerRequest(event.data);
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
      const result = __KWIRY_D5C_OWNER_WORKER__
        ? await dispatchOwner(parsed as D5cOwnerWorkerRequest)
        : await dispatchProduction(parsed as WorkerRequest);
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
  return __KWIRY_D5C_OWNER_WORKER__
    ? []
    : productionTransferListFor(response);
}

function productionTransferListFor(
  response: WorkerResponse,
): Transferable[] {
  if (!response.ok
    || (response.operation !== "export_generation"
      && response.operation !== "export_initial_build_checkpoint")) {
    return [];
  }
  const result = response.result as Partial<ExportGenerationResult | InitialBuildCheckpointExportResult>;
  return result.bytes instanceof Uint8Array
    ? [result.bytes.buffer as ArrayBuffer]
    : [];
}

function responseIdentity(value: unknown): { id: number; operation: WorkerOperation } {
  if (typeof value === "object" && value !== null) {
    const candidate = value as { id?: unknown; operation?: unknown };
    const id = typeof candidate.id === "number"
      && Number.isSafeInteger(candidate.id)
      && candidate.id >= 1
      ? candidate.id
      : 1;
    const operation = (__KWIRY_D5C_OWNER_WORKER__
      ? isD5cOwnerWorkerOperation(candidate.operation)
        ? candidate.operation
        : "status"
      : isOperation(candidate.operation)
        ? candidate.operation
        : "status") as WorkerOperation;
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
    || value === "plan_reconciliation"
    || value === "export_initial_build_checkpoint"
    || value === "restore_initial_build_checkpoint"
    || value === "plan_initial_build_checkpoint_reconciliation"
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

function wrapInitialBuildCheckpoint(payload: Uint8Array): Uint8Array {
  const byteLength = INITIAL_BUILD_CHECKPOINT_HEADER_BYTES + payload.byteLength;
  if (byteLength > MAX_EXPORT_BLOB_BYTES) throw new IndexCapacityError();
  const wrapped = new Uint8Array(byteLength);
  wrapped.set(INITIAL_BUILD_CHECKPOINT_MAGIC, 0);
  const view = new DataView(wrapped.buffer);
  view.setUint32(INITIAL_BUILD_CHECKPOINT_MAGIC.byteLength, INITIAL_BUILD_CHECKPOINT_IMAGE_VERSION, true);
  view.setUint32(INITIAL_BUILD_CHECKPOINT_MAGIC.byteLength + 4, payload.byteLength, true);
  wrapped.set(payload, INITIAL_BUILD_CHECKPOINT_HEADER_BYTES);
  return wrapped;
}

function unwrapInitialBuildCheckpoint(bytes: Uint8Array): Uint8Array {
  if (bytes.byteLength <= INITIAL_BUILD_CHECKPOINT_HEADER_BYTES) {
    throw new CacheImageInvalidError("checkpoint image is truncated");
  }
  for (let index = 0; index < INITIAL_BUILD_CHECKPOINT_MAGIC.byteLength; index += 1) {
    if (bytes[index] !== INITIAL_BUILD_CHECKPOINT_MAGIC[index]) {
      throw new CacheImageInvalidError("checkpoint image has the wrong kind");
    }
  }
  const view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
  if (view.getUint32(INITIAL_BUILD_CHECKPOINT_MAGIC.byteLength, true)
    !== INITIAL_BUILD_CHECKPOINT_IMAGE_VERSION) {
    throw new CacheImageInvalidError("checkpoint image version is incompatible");
  }
  const payloadByteLength = view.getUint32(INITIAL_BUILD_CHECKPOINT_MAGIC.byteLength + 4, true);
  if (payloadByteLength === 0
    || payloadByteLength !== bytes.byteLength - INITIAL_BUILD_CHECKPOINT_HEADER_BYTES) {
    throw new CacheImageInvalidError("checkpoint payload length is invalid");
  }
  return bytes.subarray(INITIAL_BUILD_CHECKPOINT_HEADER_BYTES);
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
