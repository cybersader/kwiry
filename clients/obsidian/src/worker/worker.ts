// SPDX-FileCopyrightText: 2026 cybersader
// SPDX-License-Identifier: GPL-3.0-only
/// <reference lib="webworker" />

import sqlite3InitModule from "@sqlite.org/sqlite-wasm";
import sqliteWasmBytes from "@sqlite.org/sqlite-wasm/sqlite3.wasm";
import rustWasmBytes from "virtual:kwiry-rust-wasm-bytes";
import {
  RUST_WASM_SHA256,
  RUST_WASM_SIZE,
  SQLITE_WASM_SHA256,
  SQLITE_WASM_SIZE,
} from "virtual:kwiry-artifact-identities";

import { openFts5Generation, type Fts5GenerationIndex, type SQLiteApi } from "./fts5-index";
import {
  WORKER_PROTOCOL_VERSION,
  type BuildResult,
  type DisposeResult,
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
let lastRequestId = 0;
let guards: GuardCounters | null = null;

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
  if (!sqlite || staging) {
    throw fixedWorkerError(
      "invalid_state",
      "index",
      "A staging generation already exists.",
      true,
    );
  }
  staging = { id: generation, index: openFts5Generation(sqlite) };
  state = "building";
  return generationResult(staging);
}

function addSourceBatch(
  generation: string,
  sources: Extract<WorkerRequest, { operation: "add_source_batch" }>["sources"],
): BuildResult {
  const target = requireStaging(generation);
  try {
    for (const source of sources) {
      const preparation = prepareSourceWithRust(source.descriptor, source.bytes);
      target.index.replaceSource(preparation);
    }
    return generationResult(target);
  } catch (error) {
    abortStaging();
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

function commitBuild(generation: string): BuildResult {
  const target = requireStaging(generation);
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

  const previous = active;
  active = target;
  staging = null;
  state = "ready";
  previous?.index.close();
  return generationResult(active);
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
  sqlite = null;
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
  staging?.index.close();
  staging = null;
  state = "ready";
}

function generationResult(generation: Generation): BuildResult {
  return {
    generation: generation.id,
    documents: generation.index.documents,
    chunks: generation.index.chunks,
  };
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
  scope.postMessage(response);
  if (!isWorkerError(parsed) && parsed.operation === "dispose" && response.ok) {
    setTimeout(() => scope.close(), 0);
  }
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
    || value === "commit_build"
    || value === "abort_build"
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
  const digest = await crypto.subtle.digest("SHA-256", bytes.slice().buffer as ArrayBuffer);
  const actual = Array.from(new Uint8Array(digest), (value) =>
    value.toString(16).padStart(2, "0")
  ).join("");
  if (actual !== expectedSha256) {
    throw fixedWorkerError("artifact_mismatch", "artifact", "Embedded WASM artifact mismatch.", false);
  }
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
  messageQueue = messageQueue.then(() => handleMessage(event));
});
