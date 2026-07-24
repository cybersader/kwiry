// SPDX-License-Identifier: GPL-3.0-only
/// <reference lib="webworker" />
import sqlite3InitModule from "@sqlite.org/sqlite-wasm";
import sqliteWasmBytes from "@sqlite.org/sqlite-wasm/sqlite3.wasm";

import {
  PROTOCOL_VERSION,
  type DisposeResult,
  type InitializeResult,
  type ProbeError,
  type ProbeOperation,
  type ProbeRequest,
  type ProbeResponse,
  type SmokeResult,
  parseProbeRequest,
} from "./protocol";

const EXPECTED_WASM_BYTES = 864_752;
const EXPECTED_WASM_SHA256 = "02d7e48164395fa68f81c6ec33e9da5461be397dc57602ac0cd89b4bbba1d312";
const START_MARKER = "";
const END_MARKER = "";

type WorkerState = "cold" | "initializing" | "ready" | "running" | "disposing" | "disposed" | "failed";

interface SQLiteDatabase {
  readonly filename: string;
  readonly pointer: unknown | undefined;
  exec(sql: string, options?: { bind?: readonly unknown[] }): unknown;
  selectValue(sql: string, bind?: readonly unknown[]): unknown;
  selectObject(sql: string, bind?: readonly unknown[]): Record<string, unknown> | undefined;
  transaction<T>(qualifier: "IMMEDIATE", callback: () => T): T;
  close(): void;
}

interface SQLiteApi {
  oo1: {
    DB: new (filename: string, flags: string) => SQLiteDatabase;
  };
}

type SQLiteInitializer = (options: {
  wasmBinary: Uint8Array;
  locateFile: () => string;
  print: () => void;
  printErr: () => void;
}) => Promise<SQLiteApi>;

interface GuardCounters {
  networkAttempts: number;
  persistenceAttempts: number;
  helperWorkerAttempts: number;
}

const scope = self as DedicatedWorkerGlobalScope;
const seenIds = new Set<number>();
let state: WorkerState = "cold";
let db: SQLiteDatabase | undefined;
let guards: GuardCounters | undefined;

function fixedError(code: ProbeError["code"], stage: ProbeError["stage"], message: string): ProbeError {
  return { code, stage, message };
}

function denyProperty(target: object, property: PropertyKey, counter: keyof GuardCounters): void {
  try {
    Object.defineProperty(target, property, {
      configurable: true,
      get() {
        if (guards) guards[counter] += 1;
        throw new Error("denied compatibility-probe capability");
      },
      set() {
        if (guards) guards[counter] += 1;
        throw new Error("denied compatibility-probe capability");
      },
    });
  } catch {
    throw new Error("failed to install compatibility-probe guard");
  }
}

function disableProperty(target: object, property: PropertyKey): void {
  try {
    Object.defineProperty(target, property, {
      configurable: true,
      writable: false,
      value: undefined,
    });
  } catch {
    throw new Error("failed to disable compatibility-probe capability");
  }
}

function installGuards(): GuardCounters {
  guards = { networkAttempts: 0, persistenceAttempts: 0, helperWorkerAttempts: 0 };
  for (const property of ["fetch", "XMLHttpRequest", "WebSocket", "EventSource", "importScripts"]) {
    denyProperty(globalThis, property, "networkAttempts");
  }
  for (const property of ["indexedDB", "localStorage", "sessionStorage"]) {
    disableProperty(globalThis, property);
  }
  denyProperty(globalThis, "Worker", "helperWorkerAttempts");

  if (typeof navigator === "object" && navigator !== null) {
    disableProperty(navigator, "storage");
  }
  return guards;
}

async function sha256(bytes: Uint8Array): Promise<string> {
  const digest = await crypto.subtle.digest("SHA-256", bytes.slice().buffer as ArrayBuffer);
  return Array.from(new Uint8Array(digest), (value) => value.toString(16).padStart(2, "0")).join("");
}

async function initialize(): Promise<InitializeResult> {
  if (state !== "cold") {
    throw fixedError("invalid_request", "protocol", "Probe initialization is not available.");
  }
  state = "initializing";

  if (sqliteWasmBytes.byteLength !== EXPECTED_WASM_BYTES
    || await sha256(sqliteWasmBytes) !== EXPECTED_WASM_SHA256) {
    state = "failed";
    throw fixedError("artifact_mismatch", "artifact", "Embedded SQLite artifact mismatch.");
  }

  const counters = installGuards();
  try {
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
    let sqlite3: SQLiteApi;
    try {
      sqlite3 = await initializeSqlite({
        wasmBinary: sqliteWasmBytes.slice(),
        locateFile: () => "embedded://sqlite3.wasm",
        print: () => undefined,
        printErr: () => undefined,
      });
    } finally {
      console.warn = originalWarn;
    }
    if (unexpectedWarnings !== 0) {
      throw fixedError("sqlite_init_failed", "sqlite", "SQLite emitted an unexpected warning.");
    }
    db = new sqlite3.oo1.DB(":memory:", "c");
    const sqliteVersion = db.selectValue("SELECT sqlite_version()");
    const fts5Enabled = Number(db.selectValue("SELECT sqlite_compileoption_used('ENABLE_FTS5')"));

    if (sqliteVersion !== "3.53.0") {
      throw fixedError("sqlite_init_failed", "sqlite", "Unexpected SQLite runtime version.");
    }
    if (fts5Enabled !== 1) {
      throw fixedError("fts5_unavailable", "fts5", "FTS5 is unavailable.");
    }
    if (db.filename !== ":memory:") {
      throw fixedError("sqlite_init_failed", "sqlite", "Probe database is not in memory.");
    }
    if (counters.networkAttempts !== 0
      || counters.persistenceAttempts !== 0
      || counters.helperWorkerAttempts !== 0) {
      throw fixedError("sqlite_init_failed", "sqlite", "SQLite attempted a denied host capability.");
    }

    state = "ready";
    return {
      sqliteVersion,
      fts5Enabled: 1,
      wasmBytes: EXPECTED_WASM_BYTES,
      wasmSha256: EXPECTED_WASM_SHA256,
      database: ":memory:",
      networkAttempts: 0,
      persistenceAttempts: 0,
      helperWorkerAttempts: 0,
    };
  } catch (error) {
    state = "failed";
    if (isProbeError(error)) throw error;
    throw fixedError("sqlite_init_failed", "sqlite", "SQLite initialization failed.");
  }
}

function runSmoke(): SmokeResult {
  if (state !== "ready" || !db) {
    throw fixedError("invalid_request", "protocol", "Probe is not ready.");
  }
  state = "running";

  try {
    db.exec(`
      CREATE VIRTUAL TABLE probe_fts USING fts5(
        title,
        content,
        tokenize='unicode61 remove_diacritics 2'
      )
    `);
    db.exec("INSERT INTO probe_fts(title, content) VALUES(?, ?)", {
      bind: ["Synthetic Alpha", "fixed compatibility token quasar"],
    });
    db.exec("INSERT INTO probe_fts(title, content) VALUES(?, ?)", {
      bind: ["Synthetic Beta", "unrelated synthetic row"],
    });

    const row = db.selectObject(`
      SELECT
        title,
        -bm25(probe_fts, 6.0, 1.0) AS score,
        snippet(probe_fts, 1, '${START_MARKER}', '${END_MARKER}', '…', 12) AS excerpt
      FROM probe_fts
      WHERE probe_fts MATCH ?
      ORDER BY score DESC, title ASC
      LIMIT 1
    `, ["quasar"]);

    let rollbackObserved = false;
    try {
      db.transaction("IMMEDIATE", () => {
        db?.exec("INSERT INTO probe_fts(title, content) VALUES(?, ?)", {
          bind: ["Rollback Only", "rollbackterm"],
        });
        throw new Error("expected synthetic rollback");
      });
    } catch {
      rollbackObserved = true;
    }

    const rollbackCount = Number(db.selectValue(
      "SELECT count(*) FROM probe_fts WHERE probe_fts MATCH ?",
      ["rollbackterm"],
    ));
    db.exec("INSERT INTO probe_fts(probe_fts) VALUES('integrity-check')");

    if (row?.title !== "Synthetic Alpha"
      || !Number.isFinite(Number(row.score))
      || typeof row.excerpt !== "string"
      || !row.excerpt.includes(`${START_MARKER}quasar${END_MARKER}`)
      || !rollbackObserved
      || rollbackCount !== 0) {
      throw new Error("synthetic FTS5 assertion failed");
    }
    if (!guards || guards.networkAttempts !== 0
      || guards.persistenceAttempts !== 0
      || guards.helperWorkerAttempts !== 0) {
      throw new Error("denied host capability was attempted");
    }

    state = "ready";
    return {
      expectedTitle: "Synthetic Alpha",
      finiteScore: true,
      snippetMarked: true,
      rollbackAbsent: true,
      integrityPassed: true,
    };
  } catch {
    state = "failed";
    throw fixedError("probe_failed", "fts5", "Synthetic FTS5 probe failed.");
  }
}

function dispose(): DisposeResult {
  if (state === "disposed") return { closed: true };
  state = "disposing";
  try {
    db?.close();
    const closed = db?.pointer === undefined;
    db = undefined;
    if (!closed) throw new Error("database remained open");
    state = "disposed";
    return { closed: true };
  } catch {
    state = "failed";
    throw fixedError("probe_failed", "close", "SQLite close failed.");
  }
}

function isProbeError(value: unknown): value is ProbeError {
  if (typeof value !== "object" || value === null) return false;
  const candidate = value as Partial<ProbeError>;
  return typeof candidate.code === "string"
    && typeof candidate.stage === "string"
    && typeof candidate.message === "string";
}

async function dispatch(request: ProbeRequest): Promise<InitializeResult | SmokeResult | DisposeResult> {
  if (seenIds.has(request.id)) {
    throw fixedError("invalid_request", "protocol", "Duplicate probe request ID.");
  }
  seenIds.add(request.id);
  if (seenIds.size > 32) {
    throw fixedError("invalid_request", "protocol", "Probe request limit exceeded.");
  }

  if (request.operation === "initialize") return initialize();
  if (request.operation === "probe") return runSmoke();
  return dispose();
}

function responseIdentity(value: unknown): { id: number; operation: ProbeOperation } {
  if (typeof value === "object" && value !== null) {
    const candidate = value as { id?: unknown; operation?: unknown };
    const id = Number.isSafeInteger(candidate.id) && Number(candidate.id) > 0 ? Number(candidate.id) : 1;
    const operation = candidate.operation === "probe" || candidate.operation === "dispose"
      ? candidate.operation
      : "initialize";
    return { id, operation };
  }
  return { id: 1, operation: "initialize" };
}

async function handleMessage(event: MessageEvent<unknown>): Promise<void> {
  const parsed = parseProbeRequest(event.data);
  const identity = responseIdentity(event.data);
  let response: ProbeResponse;

  if ("code" in parsed) {
    response = { version: PROTOCOL_VERSION, ...identity, ok: false, error: parsed };
  } else {
    try {
      const result = await dispatch(parsed);
      response = {
        version: PROTOCOL_VERSION,
        id: parsed.id,
        operation: parsed.operation,
        ok: true,
        result,
      };
    } catch (error) {
      response = {
        version: PROTOCOL_VERSION,
        id: parsed.id,
        operation: parsed.operation,
        ok: false,
        error: isProbeError(error)
          ? error
          : fixedError("probe_failed", "worker", "Compatibility probe failed."),
      };
    }
  }

  scope.postMessage(response);
  if (!("code" in parsed) && parsed.operation === "dispose" && response.ok) {
    setTimeout(() => scope.close(), 0);
  }
}

let messageQueue = Promise.resolve();
scope.addEventListener("message", (event: MessageEvent<unknown>) => {
  messageQueue = messageQueue.then(() => handleMessage(event));
});
