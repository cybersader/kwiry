// SPDX-FileCopyrightText: 2026 cybersader
// SPDX-License-Identifier: GPL-3.0-only

import { createHash } from "node:crypto";
import { readFileSync } from "node:fs";
import { createRequire } from "node:module";
import { Worker } from "node:worker_threads";
import { beforeAll, describe, expect, it } from "vitest";

import { buildPlugin } from "../esbuild.config.mjs";
import {
  CACHE_SCHEMA_VERSION,
  WORKER_PROTOCOL_VERSION,
  isWorkerResponse,
} from "../src/worker/protocol";

const require = createRequire(import.meta.url);
const CACHE_IDENTITY = "0123456789abcdef".repeat(4);
let workerSource;
let guardWorkerSource;
let prototypeWorkerSource;
let artifactIdentities;
let prototypeArtifactIdentities;
let workerMetafile;
let prototypeWorkerMetafile;

beforeAll(async () => {
  ({ workerSource, identities: artifactIdentities, workerMetafile } = await buildPlugin({
    write: false,
    production: true,
  }));
  ({ workerSource: guardWorkerSource } = await buildPlugin({
    write: false,
    production: false,
  }));
  ({
    workerSource: prototypeWorkerSource,
    identities: prototypeArtifactIdentities,
    workerMetafile: prototypeWorkerMetafile,
  } = await buildPlugin({
    write: false,
    production: true,
    internalTypoPrototype: true,
  }));
}, 120_000);

function postMessageShim({ dropTransfer, failTransfer }) {
  // Rejects any post that carries a transfer list, which is what a real
  // DataCloneError looks like from inside the Worker.
  if (failTransfer) {
    return "(message, transfer) => {\n"
      + "      if (transfer !== undefined && transfer.length > 0) {\n"
      + "        throw new Error(\"DataCloneError: could not be transferred\");\n"
      + "      }\n"
      + "      parentPort.postMessage(message);\n"
      + "    }";
  }
  if (dropTransfer) return "(message) => parentPort.postMessage(message)";
  return "(message, transfer) => parentPort.postMessage(message, transfer)";
}

function nodeWorkerSource(source, { dropTransfer = false, failTransfer = false } = {}) {
  return `
    const { parentPort } = require("node:worker_threads");
    globalThis.self = globalThis;
    // The transfer list is forwarded, not dropped: the export path transfers
    // its image buffer, and a shim that quietly copied it would hide both the
    // transfer and the detachment it causes. The dropping variant exists so
    // the detachment probe below is provably able to fail, and the failing
    // variant so the post-failure path is a real path rather than a claim.
    globalThis.postMessage = ${postMessageShim({ dropTransfer, failTransfer })};
    globalThis.addEventListener = (type, listener) => {
      if (type === "message") parentPort.on("message", (data) => listener({ data }));
    };
    globalThis.close = () => process.exit(0);
    ${source}
  `;
}

function request(worker, message, timeoutMs = 30_000) {
  return new Promise((resolve, reject) => {
    const timeout = setTimeout(() => {
      cleanup();
      reject(new Error(`Worker request timed out: ${message.operation}`));
    }, timeoutMs);
    const onMessage = (response) => {
      if (response?.id !== message.id) return;
      cleanup();
      resolve(response);
    };
    const onError = (error) => {
      cleanup();
      reject(error);
    };
    const cleanup = () => {
      clearTimeout(timeout);
      worker.off("message", onMessage);
      worker.off("error", onError);
    };
    worker.on("message", onMessage);
    worker.on("error", onError);
    worker.postMessage({ version: WORKER_PROTOCOL_VERSION, ...message });
  });
}

function injectGuardProbe(source, needle, statement) {
  const injected = source.replace(needle, `${needle}\n${statement}`);
  expect(injected).not.toBe(source);
  return injected;
}

function source(path, text) {
  const bytes = Buffer.from(text, "utf8");
  return {
    descriptor: {
      vault_id: "active-vault",
      path,
      format: "markdown",
      byte_length: bytes.byteLength,
      mtime: 1,
      mtime_nanos: "1000001",
    },
    bytes,
  };
}

let sqliteRuntime;

/**
 * Opens a received export blob as a real database inside the TEST process,
 * using the same pinned package the Worker embeds. Nothing about the export is
 * taken on the Worker's word: the bytes have to load and answer queries.
 */
async function openExportedImage(bytes) {
  if (!sqliteRuntime) {
    const initialize = (await import("@sqlite.org/sqlite-wasm")).default;
    sqliteRuntime = await initialize({ print: () => undefined, printErr: () => undefined });
  }
  const db = new sqliteRuntime.oo1.DB(":memory:", "c");
  const pointer = sqliteRuntime.wasm.allocFromTypedArray(bytes);
  const rc = sqliteRuntime.capi.sqlite3_deserialize(
    db.pointer,
    "main",
    pointer,
    bytes.byteLength,
    bytes.byteLength,
    sqliteRuntime.capi.SQLITE_DESERIALIZE_FREEONCLOSE
      | sqliteRuntime.capi.SQLITE_DESERIALIZE_RESIZEABLE,
  );
  if (rc !== 0) {
    db.close();
    throw new Error(`sqlite3_deserialize failed with ${rc}`);
  }
  return db;
}

async function buildActiveGeneration(worker, { generation = "g1", path = "alpha.md", text } = {}) {
  await request(worker, { id: 1, operation: "initialize", vault_id: "active-vault" });
  await request(worker, { id: 2, operation: "begin_build", generation });
  await request(worker, {
    id: 3,
    operation: "add_source_batch",
    generation,
    sources: [source(path, text ?? "# Alpha\nstableterm portable cache")],
  });
  await request(worker, { id: 4, operation: "commit_build", generation });
}

async function buildTypoPrototypeGeneration(worker) {
  await request(worker, { id: 1, operation: "initialize", vault_id: "active-vault" });
  await request(worker, { id: 2, operation: "begin_build", generation: "typo-g1" });
  await request(worker, {
    id: 3,
    operation: "add_source_batch",
    generation: "typo-g1",
    sources: [
      source("alpha.md", "# Retrieval alpha\nretrieval retrieval stableterm"),
      source("beta.md", "# Retrieval beta\nretrieval stableterm"),
      source("gamma.md", "# Related forms\nretrieved retriever"),
      source("delta.md", "# Literal ordering\nstableterm"),
    ],
  });
  await request(worker, { id: 4, operation: "commit_build", generation: "typo-g1" });
}

function restoreFromExport(envelope, { id = 2, generation = envelope.generation, ...overrides } = {}) {
  return {
    id,
    operation: "restore_generation",
    generation,
    bytes: envelope.bytes.slice(),
    blob_byte_length: envelope.blob_byte_length,
    blob_sha256: envelope.blob_sha256,
    digest_verified: false,
    protocol_version: envelope.protocol_version,
    cache_schema_version: envelope.cache_schema_version,
    chunking_version: envelope.chunking_version,
    sqlite_version: envelope.sqlite_version,
    sqlite_wasm_sha256: envelope.sqlite_wasm_sha256,
    rust_wasm_sha256: envelope.rust_wasm_sha256,
    plugin_id: envelope.plugin_id,
    plugin_version: envelope.plugin_version,
    cache_identity: envelope.cache_identity,
    expected_cache_identity: envelope.cache_identity,
    ...overrides,
  };
}

async function exportedFixture(options = {}) {
  const worker = new Worker(nodeWorkerSource(workerSource), { eval: true });
  try {
    await buildActiveGeneration(worker, options);
    const response = await request(worker, {
      id: 5,
      operation: "export_generation",
      generation: "g1",
      cache_identity: CACHE_IDENTITY,
    });
    expect(response).toMatchObject({ ok: true });
    return response.result;
  } finally {
    await worker.terminate();
  }
}

function percentile(values, fraction) {
  const sorted = [...values].sort((left, right) => left - right);
  return sorted[Math.min(sorted.length - 1, Math.floor(sorted.length * fraction))];
}

async function mutateEnvelope(envelope, mutate) {
  const db = await openExportedImage(envelope.bytes);
  let bytes;
  try {
    mutate(db);
    bytes = sqliteRuntime.capi.sqlite3_js_db_export(db.pointer);
  } finally {
    db.close();
  }
  return {
    ...envelope,
    bytes,
    blob_byte_length: bytes.byteLength,
    blob_sha256: createHash("sha256").update(bytes).digest("hex"),
  };
}

describe("internal suggestion-only typo prototype", () => {
  const limitation = "Bounded prefix vocabulary only considers terms sharing the first four ASCII characters; it cannot catch early-character errors such as rettrieval.";

  it("is absent from normal product inputs and artifacts", async () => {
    const normalInputs = Object.keys(workerMetafile.inputs).join("\n").replaceAll("\\", "/");
    const prototypeInputs = Object.keys(prototypeWorkerMetafile.inputs).join("\n").replaceAll("\\", "/");
    for (const forbidden of [
      "internal_typo_prototype",
      "prepare_typo_suggestion",
      "finalize_typo_suggestion",
      "max_vocabulary_candidates",
      limitation,
    ]) {
      expect(workerSource).not.toContain(forbidden);
    }
    expect(normalInputs).not.toContain("typo-prototype.ts");
    expect(normalInputs).not.toContain("pkg/internal-typo-prototype");
    expect(prototypeInputs).toContain("typo-prototype.ts");
    expect(prototypeInputs).toContain("pkg/internal-typo-prototype");
    expect(prototypeWorkerSource).toContain("internal_typo_prototype");
    expect(prototypeWorkerSource).toContain(limitation);
    expect(prototypeWorkerSource.length - workerSource.length).toBeGreaterThan(1_000);
    expect(prototypeArtifactIdentities.rust.bytes - artifactIdentities.rust.bytes)
      .toBeGreaterThan(1_000);
    const worker = new Worker(nodeWorkerSource(workerSource), { eval: true });
    try {
      await buildActiveGeneration(worker);
      await expect(request(worker, {
        id: 5,
        operation: "internal_typo_prototype",
        query: "retrievel",
      })).resolves.toMatchObject({
        ok: false,
        operation: "status",
        error: { code: "invalid_request" },
      });
    } finally {
      await worker.terminate();
    }
  }, 120_000);

  it("suggests separately, preserves literal ordering, bypasses explicit syntax, and discloses coverage", async () => {
    const worker = new Worker(nodeWorkerSource(prototypeWorkerSource), { eval: true });
    try {
      await buildTypoPrototypeGeneration(worker);
      const before = await request(worker, {
        id: 5,
        operation: "search",
        query: "stableterm",
        limit: 100,
      });
      expect(before).toMatchObject({ ok: true });
      const beforeIds = before.result.hits.map((hit) => hit.chunk_id);

      const literal = await request(worker, {
        id: 6,
        operation: "internal_typo_prototype",
        query: "stableterm",
      });
      expect(literal).toMatchObject({
        ok: true,
        result: {
          disposition: "literal_results",
          suggested_query: null,
          candidates_examined: 0,
          limitation,
        },
      });
      expect(literal.result.literal_hits.map((hit) => hit.chunk_id)).toEqual(beforeIds);

      const suggested = await request(worker, {
        id: 7,
        operation: "internal_typo_prototype",
        query: "retrievel",
      });
      expect(suggested).toMatchObject({
        ok: true,
        result: {
          disposition: "suggestion",
          literal_hits: [],
          suggested_query: "retrieval",
          limitation,
          bounds: {
            prefix_chars: 4,
            max_vocabulary_candidates: 40,
            max_candidate_bytes: 96,
            max_edit_distance: 1,
            max_output_suggestions: 1,
            max_work_units: 3_840,
          },
        },
      });
      expect(suggested.result.candidates_examined).toBeLessThanOrEqual(40);
      expect(suggested.result.work_units).toBeLessThanOrEqual(3_840);

      await expect(request(worker, {
        id: 8,
        operation: "internal_typo_prototype",
        query: "title:retrievel",
      })).resolves.toMatchObject({
        ok: true,
        result: {
          disposition: "explicit_syntax_bypass",
          literal_hits: [],
          suggested_query: null,
          candidates_examined: 0,
        },
      });

      await expect(request(worker, {
        id: 9,
        operation: "internal_typo_prototype",
        query: "rettrieval",
      })).resolves.toMatchObject({
        ok: true,
        result: {
          disposition: "no_candidate",
          literal_hits: [],
          suggested_query: null,
          limitation,
        },
      });

      const after = await request(worker, {
        id: 10,
        operation: "search",
        query: "stableterm",
        limit: 100,
      });
      expect(after.result.hits.map((hit) => hit.chunk_id)).toEqual(beforeIds);
    } finally {
      await worker.terminate();
    }
  }, 120_000);

  it("measures bounded cost through the generated Worker bridge", async () => {
    const worker = new Worker(nodeWorkerSource(prototypeWorkerSource), { eval: true });
    try {
      await buildTypoPrototypeGeneration(worker);
      let id = 5;
      for (let index = 0; index < 5; index += 1) {
        await request(worker, { id: id++, operation: "internal_typo_prototype", query: "retrievel" });
      }
      const rssBefore = process.memoryUsage().rss;
      const utilizationBefore = worker.performance.eventLoopUtilization();
      const wallStarted = performance.now();
      const totals = [];
      const vocabularies = [];
      for (let index = 0; index < 30; index += 1) {
        const response = await request(worker, {
          id: id++,
          operation: "internal_typo_prototype",
          query: "retrievel",
        });
        expect(response).toMatchObject({
          ok: true,
          result: { disposition: "suggestion", suggested_query: "retrieval", literal_hits: [] },
        });
        totals.push(response.result.total_duration_ms);
        vocabularies.push(response.result.vocabulary_duration_ms);
      }
      const wallMs = performance.now() - wallStarted;
      const utilization = worker.performance.eventLoopUtilization(utilizationBefore);
      const rssDeltaBytes = Math.max(0, process.memoryUsage().rss - rssBefore);
      const report = {
        iterations: totals.length,
        total_p50_ms: percentile(totals, 0.50),
        total_p95_ms: percentile(totals, 0.95),
        total_max_ms: Math.max(...totals),
        vocabulary_p50_ms: percentile(vocabularies, 0.50),
        vocabulary_p95_ms: percentile(vocabularies, 0.95),
        vocabulary_max_ms: Math.max(...vocabularies),
        wall_ms: Math.round(wallMs * 1_000) / 1_000,
        worker_event_loop_active_ms: Math.round(utilization.active * 1_000) / 1_000,
        worker_event_loop_utilization: Math.round(utilization.utilization * 1_000_000) / 1_000_000,
        process_rss_delta_bytes: rssDeltaBytes,
        limitation,
      };
      console.info(`KWIRY_TYPO_PROTOTYPE_MEASUREMENT ${JSON.stringify(report)}`);
      expect(report.total_p95_ms).toBeLessThan(500);
      expect(report.vocabulary_p95_ms).toBeLessThan(100);
      expect(report.process_rss_delta_bytes).toBeLessThan(64 * 1024 * 1024);
      expect(Number.isFinite(report.worker_event_loop_active_ms)).toBe(true);
      expect(report.limitation).toContain("rettrieval");
      await expect(request(worker, { id: id++, operation: "status" })).resolves.toMatchObject({
        ok: true,
        result: { phase: "ready", searchable: true },
      });
    } finally {
      await worker.terminate();
    }
  }, 120_000);
});

describe("exported cache generation", () => {
  it("exports a working database with an independently verifiable identity", async () => {
    const worker = new Worker(nodeWorkerSource(workerSource), { eval: true });
    try {
      await buildActiveGeneration(worker);

      const response = await request(worker, {
        id: 5,
        operation: "export_generation",
        generation: "g1",
        cache_identity: CACHE_IDENTITY,
      });
      expect(response).toMatchObject({ ok: true });
      // The real Worker's real output, checked by the validator that gates it
      // in production rather than by a hand-written approximation.
      expect(isWorkerResponse(response)).toBe(true);

      const envelope = response.result;
      const bytes = envelope.bytes;
      expect(bytes).toBeInstanceOf(Uint8Array);
      expect(bytes.byteLength).toBe(envelope.blob_byte_length);
      // Independently measured, never echoed.
      expect(createHash("sha256").update(bytes).digest("hex")).toBe(envelope.blob_sha256);

      expect(envelope).toMatchObject({
        generation: "g1",
        documents: 1,
        chunks: 1,
        protocol_version: WORKER_PROTOCOL_VERSION,
        cache_schema_version: CACHE_SCHEMA_VERSION,
        chunking_version: 2,
        sqlite_version: "3.53.0",
        plugin_id: "kwiry-search",
        cache_identity: CACHE_IDENTITY,
      });

      // The embedded-artifact digests are checked against the build's own
      // identities AND against the artifact files hashed directly here, so the
      // assertion does not rest on the build script being correct.
      expect(envelope.sqlite_wasm_sha256).toBe(artifactIdentities.sqlite.sha256);
      expect(envelope.rust_wasm_sha256).toBe(artifactIdentities.rust.sha256);
      expect(envelope.plugin_id).toBe(artifactIdentities.plugin.id);
      expect(envelope.plugin_version).toBe(artifactIdentities.plugin.version);
      expect(envelope.sqlite_wasm_sha256).toBe(createHash("sha256")
        .update(readFileSync(require.resolve("@sqlite.org/sqlite-wasm/sqlite3.wasm")))
        .digest("hex"));
      expect(envelope.rust_wasm_sha256).toBe(createHash("sha256")
        .update(readFileSync(new URL(
          "../rust/kwiry-obsidian-wasm/pkg/production/kwiry_obsidian_wasm_bg.wasm",
          import.meta.url,
        )))
        .digest("hex"));

      const restored = await openExportedImage(bytes);
      try {
        expect(() => restored.exec(
          "INSERT INTO chunks_fts(chunks_fts, rank) VALUES('integrity-check', 1)",
        )).not.toThrow();
        expect(Number(restored.selectValue("PRAGMA user_version"))).toBe(CACHE_SCHEMA_VERSION);
        const counts = restored.selectObjects(`
          SELECT
            (SELECT count(*) FROM chunks) AS chunks,
            (SELECT count(*) FROM chunks_fts) AS fts,
            (SELECT count(*) FROM chunks_fts f LEFT JOIN chunks c ON c.rowid = f.rowid
               WHERE c.rowid IS NULL) AS orphan_fts,
            (SELECT count(*) FROM chunks c LEFT JOIN chunks_fts f ON f.rowid = c.rowid
               WHERE f.rowid IS NULL) AS missing_fts,
            (SELECT COALESCE(SUM(chunk_count), 0) FROM sources) AS source_chunks
        `)[0];
        expect(counts).toEqual({
          chunks: 1,
          fts: 1,
          orphan_fts: 0,
          missing_fts: 0,
          source_chunks: 1,
        });
        // The restorable half: the image is a searchable index, not merely a
        // hash-consistent buffer.
        expect(restored.selectValue(
          "SELECT c.path FROM chunks c JOIN chunks_fts f ON f.rowid = c.rowid "
          + "WHERE chunks_fts MATCH ?",
          ['"stableterm"'],
        )).toBe("alpha.md");
        expect(restored.selectObjects("SELECT outcome, content_hash, mtime_nanos FROM sources"))
          .toEqual([{
            outcome: "indexed",
            content_hash: expect.stringMatching(/^[0-9a-f]{64}$/u),
            mtime_nanos: "1000001",
          }]);
      } finally {
        restored.close();
      }

      // Exporting neither disturbed the serving generation nor damaged the
      // Worker's own state: it still searches, and it still exports.
      await expect(request(worker, {
        id: 6,
        operation: "search",
        query: "stableterm",
        limit: 20,
      })).resolves.toMatchObject({
        ok: true,
        result: { generation: "g1", hits: [{ path: "alpha.md" }] },
      });
      const second = await request(worker, {
        id: 7,
        operation: "export_generation",
        generation: "g1",
        cache_identity: CACHE_IDENTITY,
      });
      expect(isWorkerResponse(second)).toBe(true);
      // Deliberately NOT asserted byte-identical: VACUUM rewrites the file
      // header on every run, so an identical image is not something SQLite
      // promises. What must hold is that the second export is the same size,
      // describes itself truthfully, and is still a working index.
      expect(second.result.blob_byte_length).toBe(envelope.blob_byte_length);
      expect(createHash("sha256").update(second.result.bytes).digest("hex"))
        .toBe(second.result.blob_sha256);
      const reexported = await openExportedImage(second.result.bytes);
      try {
        expect(reexported.selectValue(
          "SELECT c.path FROM chunks c JOIN chunks_fts f ON f.rowid = c.rowid "
          + "WHERE chunks_fts MATCH ?",
          ['"stableterm"'],
        )).toBe("alpha.md");
      } finally {
        reexported.close();
      }

      await request(worker, { id: 8, operation: "dispose" });
    } finally {
      await worker.terminate();
    }
  }, 120_000);

  // The image is transferred, never copied. The probe below fires when the
  // Worker's own view of the buffer survives the post, and the second case
  // proves the probe can actually fail.
  //
  // The probe reports by POSTING a sentinel rather than by throwing. An escaping
  // throw would be observed only as the Worker dying, and the Worker no longer
  // dies on one — the message chain is re-resolved after every link precisely so
  // a single failure cannot deafen it. Tying this probe to that crash would have
  // made it an assertion about the bug rather than about the transfer.
  it.each([
    ["transferred", false, true],
    ["copied", true, false],
  ])("detects that the export buffer was %s", async (_name, dropTransfer, expectDetached) => {
    const needle = "if (!isWorkerError(parsed) && parsed.operation === \"dispose\"";
    const injected = guardWorkerSource.replace(
      needle,
      `if (transfer.length > 0 && response.result.bytes.byteLength !== 0) {\n`
      + `    scope.postMessage({ probe: "export image buffer survived its transfer" });\n`
      + `  }\n  ${needle}`,
    );
    expect(injected).not.toBe(guardWorkerSource);
    const worker = new Worker(nodeWorkerSource(injected, { dropTransfer }), { eval: true });
    const survived = new Promise((resolve) => {
      worker.on("message", (message) => {
        if (message?.probe !== undefined) resolve("failed");
      });
    });
    try {
      await buildActiveGeneration(worker);
      await expect(request(worker, {
        id: 5,
        operation: "export_generation",
        generation: "g1",
        cache_identity: CACHE_IDENTITY,
      })).resolves.toMatchObject({ ok: true });

      const outcome = await Promise.race([
        survived,
        request(worker, { id: 6, operation: "status" }).then(() => "alive", () => "failed"),
      ]);
      expect(outcome).toBe(expectDetached ? "alive" : "failed");
    } finally {
      await worker.terminate();
    }
  }, 120_000);

  // Posting is itself fallible, and the transfer list is the realistic source.
  // Left to propagate, the throw escapes the handler and the caller sees only
  // the RPC timeout; the export must instead come back as a reported failure
  // carrying no bytes, on a Worker that is still serving.
  it("reports a post that cannot be transferred instead of dropping the response", async () => {
    const worker = new Worker(
      nodeWorkerSource(guardWorkerSource, { failTransfer: true }),
      { eval: true },
    );
    try {
      await buildActiveGeneration(worker);

      const response = await request(worker, {
        id: 5,
        operation: "export_generation",
        generation: "g1",
        cache_identity: CACHE_IDENTITY,
      }, 15_000);
      expect(response).toMatchObject({
        ok: false,
        operation: "export_generation",
        error: { code: "internal_error" },
      });
      expect(response.result).toBeUndefined();
      expect(Object.keys(response)).toEqual(["version", "id", "operation", "ok", "error"]);

      // The active generation is untouched and still answering.
      await expect(request(worker, { id: 6, operation: "status" }, 15_000)).resolves.toMatchObject({
        ok: true,
        result: { active_generation: "g1", staging_generation: null, searchable: true },
      });
    } finally {
      await worker.terminate();
    }
  }, 120_000);

  // The resilience the probe above had to be decoupled from, asserted directly:
  // one throw escaping the message handler must not stop the Worker answering.
  // Left unhandled, the serialized chain stays rejected and every later request
  // is dropped, which the user experiences as every request timing out rather
  // than as a reported fault.
  it("keeps answering after a message handler throws", async () => {
    const needle = "if (!isWorkerError(parsed) && parsed.operation === \"dispose\"";
    const injected = guardWorkerSource.replace(
      needle,
      `if (parsed.operation === "status" && parsed.id === 5) {\n`
      + `    throw new Error("injected post-response failure");\n`
      + `  }\n  ${needle}`,
    );
    expect(injected).not.toBe(guardWorkerSource);
    const worker = new Worker(nodeWorkerSource(injected), { eval: true });
    try {
      await request(worker, { id: 1, operation: "initialize", vault_id: "active-vault" });
      await expect(request(worker, { id: 5, operation: "status" })).resolves.toMatchObject({
        ok: true,
      });
      // The throw happened after id 5 was answered. Every later message must
      // still be handled.
      await expect(request(worker, { id: 6, operation: "status" })).resolves.toMatchObject({
        ok: true,
      });
      await expect(request(worker, { id: 7, operation: "status" })).resolves.toMatchObject({
        ok: true,
      });
    } finally {
      await worker.terminate();
    }
  }, 120_000);

  it.each([
    ["before any generation is published", async (worker) => {
      await request(worker, { id: 1, operation: "initialize", vault_id: "active-vault" });
      return { id: 2, generation: "g1" };
    }],
    ["while a staging generation is in flight", async (worker) => {
      await buildActiveGeneration(worker);
      await request(worker, { id: 5, operation: "begin_build", generation: "g2" });
      return { id: 6, generation: "g1" };
    }],
    ["for a generation that is no longer active", async (worker) => {
      await buildActiveGeneration(worker);
      await request(worker, {
        id: 5,
        operation: "apply_source_changes",
        generation: "g1",
        next_generation: "g2",
        upserts: [source("added.md", "addedterm")],
        removals: [],
      });
      return { id: 6, generation: "g1" };
    }],
  ])("refuses an export %s and returns no bytes", async (_name, arrange) => {
    const worker = new Worker(nodeWorkerSource(workerSource), { eval: true });
    try {
      const { id, generation } = await arrange(worker);
      const response = await request(worker, {
        id,
        operation: "export_generation",
        generation,
        cache_identity: CACHE_IDENTITY,
      });
      expect(response).toMatchObject({ ok: false, error: { code: "invalid_state" } });
      expect(response.result).toBeUndefined();
      expect(Object.keys(response)).toEqual(["version", "id", "operation", "ok", "error"]);
      expect(isWorkerResponse(response)).toBe(true);
    } finally {
      await worker.terminate();
    }
  }, 120_000);

  // The asymmetry with commit_build: a failed export operates on the LIVE
  // generation, so it must leave it intact, open and searchable. Aborting it
  // the way a failed commit aborts staging would destroy a working index.
  it.each([
    ["pre-export compaction", "target.index.compact = () => { throw new Error(\"compaction failed\"); };"],
    ["post-compaction re-verification", "target.index.assertIntegrity = () => { throw new Error(\"divergence\"); };"],
  ])("reports a failed %s without disturbing the active generation", async (_name, sabotage) => {
    const needle = "const target = active;";
    const injected = guardWorkerSource.replace(needle, `${needle}\n  ${sabotage}`);
    expect(injected).not.toBe(guardWorkerSource);
    const worker = new Worker(nodeWorkerSource(injected), { eval: true });
    try {
      await buildActiveGeneration(worker);

      const response = await request(worker, {
        id: 5,
        operation: "export_generation",
        generation: "g1",
        cache_identity: CACHE_IDENTITY,
      });
      expect(response).toMatchObject({ ok: false, error: { code: "integrity_failed" } });
      expect(response.result).toBeUndefined();

      await expect(request(worker, { id: 6, operation: "status" })).resolves.toMatchObject({
        ok: true,
        result: {
          active_generation: "g1",
          staging_generation: null,
          searchable: true,
          documents: 1,
          chunks: 1,
        },
      });
      await expect(request(worker, {
        id: 7,
        operation: "search",
        query: "stableterm",
        limit: 20,
      })).resolves.toMatchObject({
        ok: true,
        result: { generation: "g1", hits: [{ path: "alpha.md" }] },
      });
    } finally {
      await worker.terminate();
    }
  }, 120_000);

  it("records every prepared source, including a skipped one, in the exported image", async () => {
    const worker = new Worker(nodeWorkerSource(workerSource), { eval: true });
    try {
      await request(worker, { id: 1, operation: "initialize", vault_id: "active-vault" });
      await request(worker, { id: 2, operation: "begin_build", generation: "g1" });
      const binary = source("binary.md", "readable");
      // A NUL byte is one of the content skips the Rust chunker performs, and
      // it is the case that must still leave a queryable freshness row.
      binary.bytes = Buffer.from([0x61, 0x00, 0x62]);
      binary.descriptor.byte_length = 3;
      await expect(request(worker, {
        id: 3,
        operation: "add_source_batch",
        generation: "g1",
        sources: [source("alpha.md", "stableterm"), binary],
      })).resolves.toMatchObject({ ok: true, result: { documents: 1, chunks: 1 } });
      await request(worker, { id: 4, operation: "commit_build", generation: "g1" });

      const response = await request(worker, {
        id: 5,
        operation: "export_generation",
        generation: "g1",
        cache_identity: CACHE_IDENTITY,
      });
      expect(response).toMatchObject({ ok: true, result: { documents: 1, chunks: 1 } });

      const restored = await openExportedImage(response.result.bytes);
      try {
        const rows = restored.selectObjects(
          "SELECT path, outcome, content_hash, byte_length, chunk_count FROM sources ORDER BY path",
        );
        expect(rows).toEqual([
          {
            path: "alpha.md",
            outcome: "indexed",
            content_hash: expect.stringMatching(/^[0-9a-f]{64}$/u),
            byte_length: 10,
            chunk_count: 1,
          },
          // Seen and skipped, with the hash the Rust adapter computed — not an
          // absent row a restore would have to re-read forever.
          {
            path: "binary.md",
            outcome: "skipped",
            content_hash: expect.stringMatching(/^[0-9a-f]{64}$/u),
            byte_length: 3,
            chunk_count: 0,
          },
        ]);
      } finally {
        restored.close();
      }

      await request(worker, { id: 6, operation: "dispose" });
    } finally {
      await worker.terminate();
    }
  }, 120_000);

  it("records an oversized source without bytes and plans metadata-only reconciliation", async () => {
    const worker = new Worker(nodeWorkerSource(workerSource), { eval: true });
    try {
      await request(worker, { id: 1, operation: "initialize", vault_id: "active-vault" });
      await request(worker, { id: 2, operation: "begin_build", generation: "g1" });
      await expect(request(worker, {
        id: 3,
        operation: "add_source_batch",
        generation: "g1",
        sources: [
          source("alpha.md", "stableterm"),
          {
            oversized: true,
            descriptor: {
              vault_id: "active-vault",
              path: "large.md",
              format: "markdown",
              byte_length: 10 * 1024 * 1024 + 1,
              mtime: 2,
              mtime_nanos: "2000000",
            },
          },
        ],
      })).resolves.toMatchObject({ ok: true, result: { documents: 1, chunks: 1 } });
      await request(worker, { id: 4, operation: "commit_build", generation: "g1" });

      await expect(request(worker, {
        id: 5,
        operation: "plan_reconciliation",
        generation: "g1",
        vault_id: "active-vault",
        current_sources: [
          { path: "alpha.md", byte_length: 10, mtime_nanos: "1000001", indexable: true },
          {
            path: "large.md",
            byte_length: 10 * 1024 * 1024 + 1,
            mtime_nanos: "2000000",
            indexable: false,
          },
        ],
      })).resolves.toMatchObject({
        ok: true,
        result: {
          generation: "g1",
          unchanged: ["large.md"],
          audit: [{ path: "alpha.md", content_hash: expect.stringMatching(/^[0-9a-f]{64}$/u) }],
          refresh: [],
          remove: [],
        },
      });

      await expect(request(worker, {
        id: 6,
        operation: "plan_reconciliation",
        generation: "g1",
        vault_id: "active-vault",
        current_sources: [
          { path: "alpha.md", byte_length: 10, mtime_nanos: "3000000", indexable: true },
        ],
      })).resolves.toMatchObject({
        ok: true,
        result: {
          generation: "g1",
          unchanged: [],
          audit: [],
          refresh: ["alpha.md"],
          remove: ["large.md"],
        },
      });
      await request(worker, { id: 7, operation: "dispose" });
    } finally {
      await worker.terminate();
    }
  }, 120_000);
});

describe("restored cache generation", () => {
  it("restores a validated image through the block VFS and searches it", async () => {
    const envelope = await exportedFixture();
    const worker = new Worker(nodeWorkerSource(workerSource), { eval: true });
    try {
      await request(worker, { id: 1, operation: "initialize", vault_id: "active-vault" });
      await expect(request(worker, restoreFromExport(envelope))).resolves.toMatchObject({
        ok: true,
        result: { generation: "g1", documents: 1, chunks: 1, quarantined_sources: 0, quarantine_fields: [] },
      });
      await expect(request(worker, { id: 3, operation: "status" })).resolves.toMatchObject({
        ok: true,
        result: {
          active_generation: "g1",
          staging_generation: null,
          searchable: true,
          documents: 1,
          chunks: 1,
        },
      });
      await expect(request(worker, {
        id: 4,
        operation: "search",
        query: "stableterm",
        limit: 20,
      })).resolves.toMatchObject({
        ok: true,
        result: { generation: "g1", hits: [{ path: "alpha.md" }] },
      });

      const reexported = await request(worker, {
        id: 5,
        operation: "export_generation",
        generation: "g1",
        cache_identity: CACHE_IDENTITY,
      });
      expect(reexported).toMatchObject({ ok: true, result: { generation: "g1" } });
      expect(createHash("sha256").update(reexported.result.bytes).digest("hex"))
        .toBe(reexported.result.blob_sha256);
      const reopened = await openExportedImage(reexported.result.bytes);
      try {
        expect(reopened.selectValue(
          "SELECT c.path FROM chunks c JOIN chunks_fts f ON f.rowid = c.rowid "
          + "WHERE chunks_fts MATCH ?",
          ['"stableterm"'],
        )).toBe("alpha.md");
      } finally {
        reopened.close();
      }
    } finally {
      await worker.terminate();
    }
  }, 120_000);

  it("keeps the SQLite WASM floor independent of restored image size and VFS export", async () => {
    const small = await exportedFixture();
    const largeText = Array.from({ length: 250_000 }, (_, index) => `floor${index.toString(36)}`).join(" ");
    const large = await exportedFixture({
      path: "large.md",
      text: largeText,
    });
    expect(large.blob_byte_length).toBeGreaterThan(small.blob_byte_length * 4);

    let injected = guardWorkerSource;
    for (const [needle, replacement] of [
      ["state = \"ready\";\n      return {", "state = \"ready\";\n      scope.postMessage({ probe: 'initialized-wasm', wasmBytes: sqlite.wasm.memory.buffer.byteLength });\n      return {"],
      ["return publishStaging(staging, false);", "const published = publishStaging(staging, false);\n      scope.postMessage({ probe: 'restored-wasm', wasmBytes: sqlite.wasm.memory.buffer.byteLength });\n      return published;"],
      ["image = target.index.exportImage(sqlite);", "image = target.index.exportImage(sqlite);\n    scope.postMessage({ probe: 'exported-wasm', wasmBytes: sqlite.wasm.memory.buffer.byteLength });"],
    ]) {
      const next = injected.replace(needle, replacement);
      expect(next).not.toBe(injected);
      injected = next;
    }

    const measure = async (envelope) => {
      const worker = new Worker(nodeWorkerSource(injected), { eval: true });
      const probes = new Map();
      worker.on("message", (message) => {
        if (message?.probe && Number.isSafeInteger(message.wasmBytes)) {
          probes.set(message.probe, message.wasmBytes);
        }
      });
      try {
        await request(worker, { id: 1, operation: "initialize", vault_id: "active-vault" });
        await request(worker, restoreFromExport(envelope));
        await request(worker, {
          id: 3,
          operation: "export_generation",
          generation: envelope.generation,
          cache_identity: CACHE_IDENTITY,
        });
        return {
          initialized: probes.get("initialized-wasm"),
          restored: probes.get("restored-wasm"),
          exported: probes.get("exported-wasm"),
        };
      } finally {
        await worker.terminate();
      }
    };

    const smallFloor = await measure(small);
    const largeFloor = await measure(large);
    for (const floor of [smallFloor, largeFloor]) {
      expect(floor.initialized).toEqual(expect.any(Number));
      expect(floor.restored).toEqual(expect.any(Number));
      expect(floor.exported).toEqual(expect.any(Number));
      expect(floor.exported - floor.restored).toBeLessThanOrEqual(2 * 1024 * 1024);
    }
    const smallRestoreDelta = smallFloor.restored - smallFloor.initialized;
    const largeRestoreDelta = largeFloor.restored - largeFloor.initialized;
    expect(Math.abs(largeRestoreDelta - smallRestoreDelta)).toBeLessThanOrEqual(4 * 1024 * 1024);
    expect(largeRestoreDelta).toBeLessThan(large.blob_byte_length);
  }, 120_000);

  it.each([
    ["protocol_version", 1, "cache_version_mismatch"],
    ["cache_schema_version", CACHE_SCHEMA_VERSION + 1, "cache_version_mismatch"],
    ["chunking_version", 999, "cache_version_mismatch"],
    ["sqlite_version", "3.52.0", "cache_version_mismatch"],
    ["sqlite_wasm_sha256", "a".repeat(64), "cache_version_mismatch"],
    ["rust_wasm_sha256", "a".repeat(64), "cache_version_mismatch"],
    ["plugin_version", "999.0.0", "cache_version_mismatch"],
    ["plugin_id", "another-plugin", "cache_identity_mismatch"],
    ["expected_cache_identity", "f".repeat(64), "cache_identity_mismatch"],
  ])("refuses one-field identity drift in %s before opening", async (field, value, code) => {
    const envelope = await exportedFixture();
    const worker = new Worker(nodeWorkerSource(workerSource), { eval: true });
    try {
      await request(worker, { id: 1, operation: "initialize", vault_id: "active-vault" });
      await expect(request(worker, restoreFromExport(envelope, { [field]: value })))
        .resolves.toMatchObject({ ok: false, error: { code, retryable: false } });
      await expect(request(worker, { id: 3, operation: "status" })).resolves.toMatchObject({
        ok: true,
        result: { active_generation: null, staging_generation: null, searchable: false },
      });
    } finally {
      await worker.terminate();
    }
  }, 120_000);

  it("rejects same-length tampering at the digest gate and preserves the active generation", async () => {
    const envelope = await exportedFixture();
    const worker = new Worker(nodeWorkerSource(workerSource), { eval: true });
    try {
      await buildActiveGeneration(worker, {
        generation: "live",
        path: "live.md",
        text: "liveterm",
      });
      const bytes = envelope.bytes.slice();
      bytes[Math.floor(bytes.byteLength / 2)] ^= 0xff;
      const response = await request(worker, restoreFromExport(envelope, {
        id: 5,
        generation: "cached",
        bytes,
      }));
      expect(response).toMatchObject({ ok: false, error: { code: "cache_digest_mismatch" } });
      await expect(request(worker, {
        id: 6,
        operation: "search",
        query: "liveterm",
        limit: 20,
      })).resolves.toMatchObject({
        ok: true,
        result: { generation: "live", hits: [{ path: "live.md" }] },
      });
      await expect(request(worker, { id: 7, operation: "status" })).resolves.toMatchObject({
        ok: true,
        result: { active_generation: "live", staging_generation: null },
      });
    } finally {
      await worker.terminate();
    }
  }, 120_000);

  it.each([
    ["magic", (bytes) => { bytes[0] ^= 0xff; }],
    ["illegal page size", (bytes) => { bytes[16] = 0; bytes[17] = 3; }],
    ["WAL header", (bytes) => { bytes[18] = 2; }],
  ])("rejects a digest-valid %s corruption at the image gate", async (_name, corrupt) => {
    const envelope = await exportedFixture();
    const bytes = envelope.bytes.slice();
    corrupt(bytes);
    const worker = new Worker(nodeWorkerSource(workerSource), { eval: true });
    try {
      await request(worker, { id: 1, operation: "initialize", vault_id: "active-vault" });
      const response = await request(worker, restoreFromExport(envelope, {
        bytes,
        blob_sha256: createHash("sha256").update(bytes).digest("hex"),
      }));
      expect(response).toMatchObject({ ok: false, error: { code: "cache_image_invalid" } });
      await expect(request(worker, { id: 3, operation: "status" })).resolves.toMatchObject({
        ok: true,
        result: { active_generation: null, staging_generation: null },
      });
    } finally {
      await worker.terminate();
    }
  }, 120_000);

  it("proves identity, digest, header, VFS, and publication stages are ordered and fail-able", async () => {
    const envelope = await exportedFixture();
    let injected = guardWorkerSource;
    for (const [needle, replacement] of [
      ["if (await sha256Hex(request.bytes) !== request.blob_sha256)", "scope.postMessage({ probe: 'hash' });\n  if (await sha256Hex(request.bytes) !== request.blob_sha256)"],
      ["const header = validateSQLiteImage(request.bytes);", "scope.postMessage({ probe: 'header' });\n    const header = validateSQLiteImage(request.bytes);"],
      ["const index = openRestoredFts5Generation(", "scope.postMessage({ probe: 'vfs' });\n    const index = openRestoredFts5Generation("],
      ["return publishStaging(staging, false);", "scope.postMessage({ probe: 'publish' });\n      return publishStaging(staging, false);"],
    ]) {
      const next = injected.replace(needle, replacement);
      expect(next).not.toBe(injected);
      injected = next;
    }
    const worker = new Worker(nodeWorkerSource(injected), { eval: true });
    const probes = [];
    worker.on("message", (message) => {
      if (message?.probe) probes.push(message.probe);
    });
    try {
      await request(worker, { id: 1, operation: "initialize", vault_id: "active-vault" });

      probes.length = 0;
      await request(worker, restoreFromExport(envelope, {
        id: 2,
        generation: "identity-fail",
        expected_cache_identity: "f".repeat(64),
      }));
      expect(probes).toEqual([]);

      probes.length = 0;
      await request(worker, restoreFromExport(envelope, {
        id: 3,
        generation: "digest-fail",
        blob_sha256: "f".repeat(64),
      }));
      expect(probes).toEqual(["hash"]);

      probes.length = 0;
      const badHeader = envelope.bytes.slice();
      badHeader[0] ^= 0xff;
      await request(worker, restoreFromExport(envelope, {
        id: 4,
        generation: "header-fail",
        bytes: badHeader,
        blob_sha256: createHash("sha256").update(badHeader).digest("hex"),
      }));
      expect(probes).toEqual(["hash", "header"]);

      probes.length = 0;
      await request(worker, restoreFromExport(envelope, { id: 5, generation: "restored" }));
      expect(probes).toEqual(["hash", "header", "vfs", "publish"]);
    } finally {
      await worker.terminate();
    }
  }, 120_000);

  it("proves the explicit rank-1 FTS5 integrity gate can independently refuse", async () => {
    const envelope = await exportedFixture();
    const needle = `try {\n        db.exec("INSERT INTO chunks_fts(chunks_fts, rank) VALUES('integrity-check', 1)");`;
    const injected = guardWorkerSource.replace(
      needle,
      `${needle}\n        throw new Error("injected FTS integrity failure");`,
    );
    expect(injected).not.toBe(guardWorkerSource);
    const worker = new Worker(nodeWorkerSource(injected), { eval: true });
    try {
      await request(worker, { id: 1, operation: "initialize", vault_id: "active-vault" });
      await expect(request(worker, restoreFromExport(envelope))).resolves.toMatchObject({
        ok: false,
        error: { code: "cache_image_invalid", stage: "index", retryable: false },
      });
      await expect(request(worker, { id: 3, operation: "status" })).resolves.toMatchObject({
        ok: true,
        result: { active_generation: null, staging_generation: null },
      });
    } finally {
      await worker.terminate();
    }
  }, 120_000);

  it("reports a missing VFS installation capability as runtime failure, not cache corruption", async () => {
    const envelope = await exportedFixture();
    const needle = "const index = openRestoredFts5Generation(";
    const injected = guardWorkerSource.replace(
      needle,
      `sqlite.vfs.installVfs = () => { throw new Error("injected capability failure"); };\n    ${needle}`,
    );
    expect(injected).not.toBe(guardWorkerSource);
    const worker = new Worker(nodeWorkerSource(injected), { eval: true });
    try {
      await request(worker, { id: 1, operation: "initialize", vault_id: "active-vault" });
      await expect(request(worker, restoreFromExport(envelope))).resolves.toMatchObject({
        ok: false,
        error: { code: "internal_error", stage: "sqlite", retryable: false },
      });
      await expect(request(worker, { id: 3, operation: "status" })).resolves.toMatchObject({
        ok: true,
        result: { active_generation: null, staging_generation: null },
      });
    } finally {
      await worker.terminate();
    }
  }, 120_000);

  it.each([
    ["internal user_version drift", (db) => db.exec(`PRAGMA user_version = ${CACHE_SCHEMA_VERSION + 1}`), "cache_version_mismatch"],
    ["extra schema object", (db) => db.exec("CREATE TABLE unexpected_cache_object(value TEXT)"), "cache_image_invalid"],
    ["invalid source inventory", (db) => db.exec("UPDATE sources SET content_hash = ''"), "cache_image_invalid"],
    ["per-source tally mismatch", (db) => db.exec("UPDATE sources SET chunk_count = chunk_count + 1"), "cache_image_invalid"],
    ["malformed heading JSON", (db) => db.exec("UPDATE chunks SET heading_path_json = 'not-json'"), "cache_image_invalid"],
    ["negative property FTS rowid", (db) => {
      db.exec(
        "INSERT INTO source_property_text_fts(rowid, string_value) VALUES(-1, 'negative')",
      );
    }, "cache_image_invalid"],
    ["invalid frontmatter shape", (db) => db.exec(`
      INSERT INTO source_properties(
        rowid, source_key, property_name, value_json, root_type, exact_value
      )
      SELECT 900001, source_key, 'broken', '{"type":"broken"}', 'string', 'broken'
      FROM sources LIMIT 1
    `), "cache_image_invalid"],
    ["chunk/source identity disagreement", (db) => db.exec("UPDATE chunks SET path = 'other.md'"), "cache_image_invalid"],
    ["forged compact title", (db) => db.exec(
      "UPDATE chunks SET frontmatter_json = '{\"title\":\"Forged Title\"}'",
    ), "cache_image_invalid"],
    ["orphan FTS posting", (db) => db.exec("INSERT INTO chunks_fts(rowid, content) VALUES(900000, 'orphanterm')"), "cache_image_invalid"],
  ])("rejects digest-valid staged %s and leaves the active generation serving", async (_name, mutate, code) => {
    const corrupt = await mutateEnvelope(await exportedFixture(), mutate);
    const precondition = await openExportedImage(corrupt.bytes);
    try {
      expect(precondition.selectValue("PRAGMA integrity_check")).toBe("ok");
      const ftsIntegrity = () => precondition.exec(
        "INSERT INTO chunks_fts(chunks_fts, rank) VALUES('integrity-check', 1)",
      );
      if (_name === "orphan FTS posting") expect(ftsIntegrity).toThrow();
      else expect(ftsIntegrity).not.toThrow();
    } finally {
      precondition.close();
    }
    const worker = new Worker(nodeWorkerSource(workerSource), { eval: true });
    try {
      await buildActiveGeneration(worker, {
        generation: "live",
        path: "live.md",
        text: "liveterm",
      });
      const response = await request(worker, restoreFromExport(corrupt, {
        id: 5,
        generation: "cached",
      }));
      expect(response).toMatchObject({ ok: false, error: { code, retryable: false } });
      await expect(request(worker, {
        id: 6,
        operation: "search",
        query: "liveterm",
        limit: 20,
      })).resolves.toMatchObject({
        ok: true,
        result: { generation: "live", hits: [{ path: "live.md" }] },
      });
      await expect(request(worker, { id: 7, operation: "status" })).resolves.toMatchObject({
        ok: true,
        result: { active_generation: "live", staging_generation: null },
      });
    } finally {
      await worker.terminate();
    }
  }, 120_000);

  it("rejects a digest-valid image coherently rewritten to another vault", async () => {
    const forged = await mutateEnvelope(await exportedFixture(), (db) => {
      db.exec("UPDATE generation_identity SET vault_id = 'other-vault'");
      db.exec("UPDATE sources SET vault_id = 'other-vault'");
      db.exec("UPDATE chunks SET vault_id = 'other-vault'");
    });
    const precondition = await openExportedImage(forged.bytes);
    try {
      expect(precondition.selectValue("PRAGMA integrity_check")).toBe("ok");
      expect(precondition.selectValue(
        "SELECT count(*) FROM sources WHERE vault_id = 'other-vault'",
      )).toBe(1);
      expect(precondition.selectValue(
        "SELECT count(*) FROM chunks WHERE vault_id = 'other-vault'",
      )).toBe(1);
    } finally {
      precondition.close();
    }

    const worker = new Worker(nodeWorkerSource(workerSource), { eval: true });
    try {
      await request(worker, { id: 1, operation: "initialize", vault_id: "active-vault" });
      await expect(request(worker, restoreFromExport(forged))).resolves.toMatchObject({
        ok: false,
        error: { code: "cache_image_invalid", retryable: false },
      });
      await expect(request(worker, { id: 3, operation: "status" })).resolves.toMatchObject({
        ok: true,
        result: { active_generation: null, staging_generation: null, searchable: false },
      });
    } finally {
      await worker.terminate();
    }
  }, 120_000);

  it("rejects same-rowid forged chunk postings before staged publication", async () => {
    const forged = await mutateEnvelope(await exportedFixture({ text: "canonicalterm" }), (db) => {
      const rowid = Number(db.selectValue("SELECT rowid FROM chunks LIMIT 1"));
      db.exec("DELETE FROM chunks_fts WHERE rowid = ?", { bind: [rowid] });
      db.exec("INSERT INTO chunks_fts(rowid, content) VALUES(?, 'forgedterm')", {
        bind: [rowid],
      });
    });
    const worker = new Worker(nodeWorkerSource(workerSource), { eval: true });
    try {
      await request(worker, { id: 1, operation: "initialize", vault_id: "active-vault" });
      await expect(request(worker, restoreFromExport(forged))).resolves.toMatchObject({
        ok: false,
        error: { code: "cache_image_invalid", retryable: false },
      });
      await expect(request(worker, { id: 3, operation: "status" })).resolves.toMatchObject({
        ok: true,
        result: { active_generation: null, staging_generation: null, searchable: false },
      });
    } finally {
      await worker.terminate();
    }
  }, 120_000);

  it("repairs same-rowid forged property postings before staged publication", async () => {
    const forged = await mutateEnvelope(await exportedFixture({
      text: "---\ntitle: Canonical Title\n---\nstableterm",
    }), (db) => {
      const rowid = Number(db.selectValue(
        "SELECT rowid FROM source_properties WHERE property_name = 'title'",
      ));
      db.exec("DELETE FROM source_property_text_fts WHERE rowid = ?", { bind: [rowid] });
      db.exec(
        "INSERT INTO source_property_text_fts(rowid, string_value) VALUES(?, 'forgedterm')",
        { bind: [rowid] },
      );
    });
    const before = await openExportedImage(forged.bytes);
    try {
      expect(before.selectValue(
        "SELECT count(*) FROM source_property_text_fts WHERE source_property_text_fts MATCH ?",
        ['string_value : "forgedterm"'],
      )).toBe(1);
      expect(before.selectValue(
        "SELECT count(*) FROM source_property_text_fts WHERE source_property_text_fts MATCH ?",
        ['string_value : "canonical"'],
      )).toBe(0);
    } finally {
      before.close();
    }

    const worker = new Worker(nodeWorkerSource(workerSource), { eval: true });
    try {
      await request(worker, { id: 1, operation: "initialize", vault_id: "active-vault" });
      await expect(request(worker, restoreFromExport(forged))).resolves.toMatchObject({
        ok: true,
        result: { generation: "g1" },
      });
      const repairedEnvelope = (await request(worker, {
        id: 3,
        operation: "export_generation",
        generation: "g1",
        cache_identity: CACHE_IDENTITY,
      })).result;
      const repaired = await openExportedImage(repairedEnvelope.bytes);
      try {
        expect(repaired.selectValue(
          "SELECT count(*) FROM source_property_text_fts WHERE source_property_text_fts MATCH ?",
          ['string_value : "canonical"'],
        )).toBe(1);
        expect(repaired.selectValue(
          "SELECT count(*) FROM source_property_text_fts WHERE source_property_text_fts MATCH ?",
          ['string_value : "forgedterm"'],
        )).toBe(0);
      } finally {
        repaired.close();
      }
    } finally {
      await worker.terminate();
    }
  }, 120_000);

  it("does not publish or retain a restored generation when prior cleanup fails", async () => {
    const envelope = await exportedFixture();
    let injected = guardWorkerSource;
    for (const [needle, replacement] of [
      [
        "previous?.index.close();",
        `if (previous) previous.index.close = () => { throw new Error("cleanup failed"); };\n    previous?.index.close();`,
      ],
      [
        "mainFile = null;",
        `mainFile = null;\n    self.postMessage({ probe: "vfs-released" });`,
      ],
    ]) {
      const next = injected.replace(needle, replacement);
      expect(next).not.toBe(injected);
      injected = next;
    }

    const worker = new Worker(nodeWorkerSource(injected), { eval: true });
    const probes = [];
    worker.on("message", (message) => {
      if (message?.probe) probes.push(message.probe);
    });
    try {
      await buildActiveGeneration(worker, {
        generation: "live",
        path: "live.md",
        text: "liveterm",
      });
      await expect(request(worker, restoreFromExport(envelope, {
        id: 5,
        generation: "cached",
      }))).resolves.toMatchObject({
        ok: false,
        error: { code: "worker_crashed", stage: "lifecycle", retryable: true },
      });
      expect(probes).toContain("vfs-released");
      await expect(request(worker, { id: 6, operation: "status" })).resolves.toMatchObject({
        ok: true,
        result: {
          active_generation: "live",
          staging_generation: null,
          searchable: true,
        },
      });
      await expect(request(worker, {
        id: 7,
        operation: "search",
        query: "stableterm",
        limit: 20,
      })).resolves.toMatchObject({
        ok: true,
        result: { generation: "live", hits: [] },
      });
      await expect(request(worker, {
        id: 8,
        operation: "search",
        query: "liveterm",
        limit: 20,
      })).resolves.toMatchObject({
        ok: true,
        result: { generation: "live", hits: [{ path: "live.md" }] },
      });
    } finally {
      await worker.terminate();
    }
  }, 120_000);

  it("reuses private VFS registrations across repeated staged replacements", async () => {
    const envelope = await exportedFixture();
    const worker = new Worker(nodeWorkerSource(workerSource), { eval: true });
    try {
      await request(worker, { id: 1, operation: "initialize", vault_id: "active-vault" });
      for (let index = 0; index < 6; index += 1) {
        const generation = `restored-${index}`;
        await expect(request(worker, restoreFromExport(envelope, {
          id: index + 2,
          generation,
        }))).resolves.toMatchObject({
          ok: true,
          result: { generation, documents: 1, chunks: 1 },
        });
      }
      await expect(request(worker, {
        id: 8,
        operation: "search",
        query: "stableterm",
        limit: 20,
      })).resolves.toMatchObject({
        ok: true,
        result: { generation: "restored-5", hits: [{ path: "alpha.md" }] },
      });
    } finally {
      await worker.terminate();
    }
  }, 120_000);

  it("cleans a rejected staged VFS before a later good restore", async () => {
    const envelope = await exportedFixture();
    const corrupt = await mutateEnvelope(envelope, (db) => {
      db.exec("CREATE TABLE unexpected_cache_object(value TEXT)");
    });
    const worker = new Worker(nodeWorkerSource(workerSource), { eval: true });
    try {
      await request(worker, { id: 1, operation: "initialize", vault_id: "active-vault" });
      await expect(request(worker, restoreFromExport(corrupt, {
        id: 2,
        generation: "bad",
      }))).resolves.toMatchObject({ ok: false, error: { code: "cache_image_invalid" } });
      await expect(request(worker, restoreFromExport(envelope, {
        id: 3,
        generation: "good",
      }))).resolves.toMatchObject({ ok: true, result: { generation: "good" } });
      await expect(request(worker, {
        id: 4,
        operation: "search",
        query: "stableterm",
        limit: 20,
      })).resolves.toMatchObject({ ok: true, result: { hits: [{ path: "alpha.md" }] } });
    } finally {
      await worker.terminate();
    }
  }, 120_000);

  it("completes restore, mutate, block export, and restore again", async () => {
    const producer = new Worker(nodeWorkerSource(workerSource), { eval: true });
    let firstEnvelope;
    try {
      await request(producer, { id: 1, operation: "initialize", vault_id: "active-vault" });
      await request(producer, { id: 2, operation: "begin_build", generation: "g1" });
      await request(producer, {
        id: 3,
        operation: "add_source_batch",
        generation: "g1",
        sources: [source("old.md", "oldterm"), source("keep.md", "keepterm")],
      });
      await request(producer, { id: 4, operation: "commit_build", generation: "g1" });
      firstEnvelope = (await request(producer, {
        id: 5,
        operation: "export_generation",
        generation: "g1",
        cache_identity: CACHE_IDENTITY,
      })).result;
    } finally {
      await producer.terminate();
    }

    const mutator = new Worker(nodeWorkerSource(workerSource), { eval: true });
    let secondEnvelope;
    try {
      await request(mutator, { id: 1, operation: "initialize", vault_id: "active-vault" });
      await request(mutator, restoreFromExport(firstEnvelope));
      await expect(request(mutator, {
        id: 3,
        operation: "apply_source_changes",
        generation: "g1",
        next_generation: "g2",
        upserts: [source("new.md", "newterm")],
        removals: [{ vault_id: "active-vault", path: "old.md" }],
      })).resolves.toMatchObject({ ok: true, result: { generation: "g2", documents: 2 } });
      for (const [id, query, count] of [[4, "oldterm", 0], [5, "newterm", 1], [6, "keepterm", 1]]) {
        const response = await request(mutator, { id, operation: "search", query, limit: 20 });
        expect(response.result.hits).toHaveLength(count);
      }
      secondEnvelope = (await request(mutator, {
        id: 7,
        operation: "export_generation",
        generation: "g2",
        cache_identity: CACHE_IDENTITY,
      })).result;
    } finally {
      await mutator.terminate();
    }

    const consumer = new Worker(nodeWorkerSource(workerSource), { eval: true });
    try {
      await request(consumer, { id: 1, operation: "initialize", vault_id: "active-vault" });
      await expect(request(consumer, restoreFromExport(secondEnvelope, { generation: "g2" })))
        .resolves.toMatchObject({ ok: true, result: { generation: "g2", documents: 2 } });
      for (const [id, query, count] of [[3, "oldterm", 0], [4, "newterm", 1], [5, "keepterm", 1]]) {
        const response = await request(consumer, { id, operation: "search", query, limit: 20 });
        expect(response.result.hits).toHaveLength(count);
      }
    } finally {
      await consumer.terminate();
    }
  }, 120_000);
});

describe("exact generated production Worker", () => {
  it("initializes both WASM runtimes and publishes only complete generations", async () => {
    const worker = new Worker(nodeWorkerSource(workerSource), { eval: true });
    try {
      await expect(request(worker, { id: 1, operation: "initialize", vault_id: "active-vault" })).resolves.toMatchObject({
        ok: true,
        result: {
          rustAbiVersion: 2,
          sourceSchemaVersion: 4,
          querySchemaVersion: 4,
          matchPlanSchemaVersion: 3,
          sqliteVersion: "3.53.0",
          fts5Enabled: 1,
        },
      });
      await expect(request(worker, {
        id: 2,
        operation: "begin_build",
        generation: "generation-1",
      })).resolves.toMatchObject({ ok: true, result: { chunks: 0 } });
      await expect(request(worker, {
        id: 3,
        operation: "add_source_batch",
        generation: "generation-1",
        sources: [source(
          "alpha.md",
          "---\ntitle: IIA 2 line guide\n---\n# Alpha\nportable quasar cache storage",
        )],
      })).resolves.toMatchObject({ ok: true, result: { documents: 1, chunks: 1 } });

      await expect(request(worker, {
        id: 4,
        operation: "search",
        query: "quasar",
        limit: 20,
      })).resolves.toMatchObject({ ok: false, error: { code: "index_building" } });

      await expect(request(worker, {
        id: 5,
        operation: "commit_build",
        generation: "generation-1",
      })).resolves.toMatchObject({ ok: true, result: { generation: "generation-1" } });
      await expect(request(worker, {
        id: 6,
        operation: "search",
        query: "quasar",
        limit: 20,
      })).resolves.toMatchObject({
        ok: true,
        result: {
          generation: "generation-1",
          // The index is contentless: the Worker returns identity and ranking
          // and never index-derived excerpt text. The host hydrates excerpts
          // from the vault file.
          hits: [{ chunk_id: expect.any(String), path: "alpha.md", excerpt: "" }],
        },
      });

      await expect(request(worker, {
        id: 7,
        operation: "search",
        query: "line guide",
        limit: 20,
      })).resolves.toMatchObject({ ok: true, result: { hits: [{ path: "alpha.md" }] } });
      await expect(request(worker, {
        id: 8,
        operation: "search",
        query: "title:\"IIA 2 line guide\" OR content:cache*",
        limit: 20,
      })).resolves.toMatchObject({ ok: true, result: { hits: [{ path: "alpha.md" }] } });
      await expect(request(worker, {
        id: 9,
        operation: "search",
        query: `${"🚀".repeat(128)}a`,
        limit: 20,
      })).resolves.toMatchObject({ ok: true, result: { hits: [] } });

      await request(worker, { id: 10, operation: "begin_build", generation: "generation-2" });
      await request(worker, {
        id: 11,
        operation: "add_source_batch",
        generation: "generation-2",
        sources: [source("beta.md", "# Beta\nstagingterm")],
      });
      await expect(request(worker, {
        id: 12,
        operation: "search",
        query: "quasar",
        limit: 20,
      })).resolves.toMatchObject({
        ok: true,
        result: { generation: "generation-1", hits: [{ path: "alpha.md" }] },
      });
      await request(worker, { id: 13, operation: "abort_build", generation: "generation-2" });

      await expect(request(worker, { id: 14, operation: "dispose" })).resolves.toMatchObject({
        ok: true,
        result: { closed: true },
      });
    } finally {
      await worker.terminate();
    }
  }, 120_000);

  it("executes the Rust-owned evidence ladder across exact, partial, prefix, anchor, and empty branches", async () => {
    const worker = new Worker(nodeWorkerSource(workerSource), { eval: true });
    try {
      await request(worker, { id: 1, operation: "initialize", vault_id: "active-vault" });
      await request(worker, { id: 2, operation: "begin_build", generation: "ladder" });
      await request(worker, {
        id: 3,
        operation: "add_source_batch",
        generation: "ladder",
        sources: [
          source("exact.md", "---\ntitle: Quasar Guide\n---\n# Exact\nordinary body"),
          source("phrase.md", "# Phrase\nquasar guide quasar guide quasar guide"),
          source("prefix.md", "# Prefix\nprefixevidence"),
          source("all.md", "# All\nalpha beta"),
          source("rfc.md", "# RFC\nRFC 9110 archival"),
        ],
      });
      await request(worker, { id: 4, operation: "commit_build", generation: "ladder" });

      const exact = await request(worker, {
        id: 5, operation: "search", query: "quasar guide", limit: 20,
      });
      expect(exact).toMatchObject({ ok: true, result: { hits: [
        { path: "exact.md" },
        { path: "phrase.md" },
      ] } });

      await expect(request(worker, {
        id: 6, operation: "search", query: "quasar missingcontext", limit: 20,
      })).resolves.toMatchObject({ ok: true, result: { hits: expect.arrayContaining([
        expect.objectContaining({ path: "exact.md" }),
      ]) } });
      await expect(request(worker, {
        id: 7, operation: "search", query: "alpha beta", limit: 20,
      })).resolves.toMatchObject({ ok: true, result: { hits: [{ path: "all.md" }] } });
      await expect(request(worker, {
        id: 8, operation: "search", query: "prefixevid", limit: 20,
      })).resolves.toMatchObject({ ok: true, result: { hits: [{ path: "prefix.md" }] } });
      await expect(request(worker, {
        id: 9, operation: "search", query: "RFC 9110 missingcontext", limit: 20,
      })).resolves.toMatchObject({ ok: true, result: { hits: [{ path: "rfc.md" }] } });
      await expect(request(worker, {
        id: 10, operation: "search", query: "RFC 9999 quasar", limit: 20,
      })).resolves.toMatchObject({ ok: true, result: { hits: [] } });
      await expect(request(worker, {
        id: 11, operation: "search", query: "zzzznoevidence", limit: 20,
      })).resolves.toMatchObject({ ok: true, result: { hits: [] } });
      await expect(request(worker, {
        id: 12, operation: "search", query: "title:\"Quasar Guide\"", limit: 20,
      })).resolves.toMatchObject({ ok: true, result: { hits: [{ path: "exact.md" }] } });

      const first = await request(worker, {
        id: 13, operation: "search", query: "quasar guide", limit: 20,
      });
      const second = await request(worker, {
        id: 14, operation: "search", query: "quasar guide", limit: 20,
      });
      expect(second.result.hits).toEqual(first.result.hits);
    } finally {
      await worker.terminate();
    }
  }, 120_000);

  it("refuses a finalized execution plan corrupted across the exact validator boundary", async () => {
    const needle = "function isFinalizedQuery(value) {";
    const injected = guardWorkerSource.replace(
      needle,
      `${needle}\n  if (value?.plan?.query === "validatorprobe") value.execution_plan.stages[0].ordinal = 1;`,
    );
    expect(injected).not.toBe(guardWorkerSource);
    const worker = new Worker(nodeWorkerSource(injected), { eval: true });
    try {
      await request(worker, { id: 1, operation: "initialize", vault_id: "active-vault" });
      await request(worker, { id: 2, operation: "begin_build", generation: "validator" });
      await request(worker, {
        id: 3,
        operation: "add_source_batch",
        generation: "validator",
        sources: [source("validator.md", "# Validator\nvalidatorprobe")],
      });
      await request(worker, { id: 4, operation: "commit_build", generation: "validator" });
      await expect(request(worker, {
        id: 5, operation: "search", query: "validatorprobe", limit: 20,
      })).resolves.toMatchObject({ ok: false, error: { code: "query_rejected" } });
    } finally {
      await worker.terminate();
    }
  }, 120_000);

  it("refuses a finalized plan that relaxes a required identifier anchor", async () => {
    const needle = "function isFinalizedQuery(value) {";
    const injected = guardWorkerSource.replace(
      needle,
      `${needle}\n  if (value?.plan?.query === "RFC 9110 missingcontext") { const partial = value.plan.evidence_stages.find((stage) => stage.kind === "partial_coverage"); if (partial) partial.required_term_indexes = [2]; }`,
    );
    expect(injected).not.toBe(guardWorkerSource);
    const worker = new Worker(nodeWorkerSource(injected), { eval: true });
    try {
      await request(worker, { id: 1, operation: "initialize", vault_id: "active-vault" });
      await request(worker, { id: 2, operation: "begin_build", generation: "anchor-validator" });
      await request(worker, {
        id: 3,
        operation: "add_source_batch",
        generation: "anchor-validator",
        sources: [source("rfc.md", "# RFC\nRFC 9110 authority")],
      });
      await request(worker, { id: 4, operation: "commit_build", generation: "anchor-validator" });
      await expect(request(worker, {
        id: 5, operation: "search", query: "RFC 9110 missingcontext", limit: 20,
      })).resolves.toMatchObject({ ok: false, error: { code: "query_rejected" } });
    } finally {
      await worker.terminate();
    }
  }, 120_000);

  it("reports durable page-cap overflow and preserves the published generation", async () => {
    const injected = guardWorkerSource.replace(
      "var DEFAULT_DATABASE_BYTE_LIMIT = 320 * 1024 * 1024;",
      "var DEFAULT_DATABASE_BYTE_LIMIT = 1 * 1024 * 1024;",
    );
    expect(injected).not.toBe(guardWorkerSource);
    const worker = new Worker(nodeWorkerSource(injected), { eval: true });
    const nullBag = `---\nvalues:\n${"  - null\n".repeat(10_000)}---\n`;
    try {
      await buildActiveGeneration(worker, {
        generation: "live",
        path: "stable.md",
        text: "stableterm",
      });
      const before = await request(worker, { id: 5, operation: "status" });
      expect(before).toMatchObject({
        ok: true,
        result: {
          active_generation: "live",
          staging_generation: null,
          active_database_bytes: expect.any(Number),
          staging_database_bytes: 0,
          database_byte_limit: 1 * 1024 * 1024,
        },
      });

      await request(worker, { id: 6, operation: "begin_build", generation: "overflow-stage" });
      await expect(request(worker, {
        id: 7,
        operation: "add_source_batch",
        generation: "overflow-stage",
        sources: [source("overflow.md", nullBag)],
      }, 120_000)).resolves.toMatchObject({
        ok: false,
        error: { code: "index_limit_exceeded" },
      });
      await expect(request(worker, { id: 8, operation: "status" })).resolves.toMatchObject({
        ok: true,
        result: {
          active_generation: "live",
          staging_generation: null,
          active_database_bytes: before.result.active_database_bytes,
          staging_database_bytes: 0,
        },
      });

      await expect(request(worker, {
        id: 9,
        operation: "apply_source_changes",
        generation: "live",
        next_generation: "after-overflow",
        upserts: [source("overflow.md", nullBag)],
        removals: [],
      }, 120_000)).resolves.toMatchObject({
        ok: false,
        error: { code: "index_limit_exceeded" },
      });
      await expect(request(worker, {
        id: 10,
        operation: "search",
        query: "stableterm",
        limit: 20,
      })).resolves.toMatchObject({
        ok: true,
        result: { generation: "live", hits: [{ path: "stable.md" }] },
      });
      await expect(request(worker, { id: 11, operation: "status" })).resolves.toMatchObject({
        ok: true,
        result: {
          active_generation: "live",
          active_database_bytes: before.result.active_database_bytes,
          staging_database_bytes: 0,
        },
      });
    } finally {
      await worker.terminate();
    }
  }, 120_000);

  it("quarantines a chunk/source mismatch without aborting the good source batch", async () => {
    const needle = "function requireSourcePreparation(value) {";
    const injected = guardWorkerSource.replace(
      needle,
      `${needle}\n  if (value?.path === "bad.md" && value.chunks?.[0]?.chunk) value.chunks[0].chunk.path = "other.md";`,
    );
    expect(injected).not.toBe(guardWorkerSource);
    const worker = new Worker(nodeWorkerSource(injected), { eval: true });
    try {
      await request(worker, { id: 1, operation: "initialize", vault_id: "active-vault" });
      await request(worker, { id: 2, operation: "begin_build", generation: "g1" });
      await expect(request(worker, {
        id: 3,
        operation: "add_source_batch",
        generation: "g1",
        sources: [source("good.md", "goodterm"), source("bad.md", "badterm")],
      })).resolves.toMatchObject({
        ok: true,
        result: {
          generation: "g1",
          documents: 1,
          chunks: 1,
          quarantined_sources: 1,
          quarantine_fields: ["chunks_source_correlation"],
        },
      });
      await request(worker, { id: 4, operation: "commit_build", generation: "g1" });
      await expect(request(worker, {
        id: 5,
        operation: "search",
        query: "goodterm",
        limit: 20,
      })).resolves.toMatchObject({ ok: true, result: { hits: [{ path: "good.md" }] } });
      await expect(request(worker, {
        id: 6,
        operation: "search",
        query: "badterm",
        limit: 20,
      })).resolves.toMatchObject({ ok: true, result: { hits: [] } });
      const exported = await request(worker, {
        id: 7,
        operation: "export_generation",
        generation: "g1",
        cache_identity: CACHE_IDENTITY,
      });
      const db = await openExportedImage(exported.result.bytes);
      try {
        expect(db.selectValue(`
          SELECT count(*) FROM chunks c JOIN sources s ON s.source_key = c.source_key
          WHERE s.path = 'bad.md'
        `)).toBe(0);
        expect(db.selectValue(`
          SELECT count(*) FROM source_properties p JOIN sources s ON s.source_key = p.source_key
          WHERE s.path = 'bad.md'
        `)).toBe(0);
      } finally {
        db.close();
      }
    } finally {
      await worker.terminate();
    }
  }, 120_000);

  it("aborts a rejected staging generation and preserves the active generation", async () => {
    const worker = new Worker(nodeWorkerSource(workerSource), { eval: true });
    try {
      await request(worker, { id: 1, operation: "initialize", vault_id: "active-vault" });
      await request(worker, { id: 2, operation: "begin_build", generation: "generation-1" });
      await request(worker, {
        id: 3,
        operation: "add_source_batch",
        generation: "generation-1",
        sources: [source("alpha.md", "stableterm")],
      });
      await request(worker, { id: 4, operation: "commit_build", generation: "generation-1" });
      await request(worker, { id: 5, operation: "begin_build", generation: "generation-bad" });
      await expect(request(worker, {
        id: 6,
        operation: "add_source_batch",
        generation: "generation-bad",
        sources: [source("../invalid.md", "badterm")],
      })).resolves.toMatchObject({ ok: false, error: { code: "source_rejected" } });
      await expect(request(worker, { id: 7, operation: "status" })).resolves.toMatchObject({
        ok: true,
        result: {
          active_generation: "generation-1",
          staging_generation: null,
          searchable: true,
        },
      });
      await expect(request(worker, {
        id: 8,
        operation: "search",
        query: "stableterm",
        limit: 20,
      })).resolves.toMatchObject({ ok: true, result: { hits: [{ path: "alpha.md" }] } });
      const rejected = await request(worker, {
        id: 9,
        operation: "search",
        query: "title:notes OR '); DROP TABLE chunks; --",
        limit: 20,
      });
      expect(rejected).toMatchObject({ ok: false, error: { code: "query_rejected" } });
      expect(JSON.stringify(rejected)).not.toContain("DROP TABLE");
      await request(worker, { id: 10, operation: "dispose" });
    } finally {
      await worker.terminate();
    }
  }, 120_000);

  it("atomically renames active content and advances the generation once", async () => {
    const worker = new Worker(nodeWorkerSource(workerSource), { eval: true });
    try {
      await request(worker, { id: 1, operation: "initialize", vault_id: "active-vault" });
      await request(worker, { id: 2, operation: "begin_build", generation: "g1" });
      await request(worker, {
        id: 3,
        operation: "add_source_batch",
        generation: "g1",
        sources: [source("alpha.md", "stableterm")],
      });
      await request(worker, { id: 4, operation: "commit_build", generation: "g1" });

      await expect(request(worker, {
        id: 5,
        operation: "apply_source_changes",
        generation: "g1",
        next_generation: "g2",
        upserts: [source("renamed.md", "newterm")],
        removals: [{ vault_id: "active-vault", path: "alpha.md" }],
      })).resolves.toMatchObject({
        ok: true,
        result: { generation: "g2", documents: 1, chunks: 1, quarantined_sources: 0, quarantine_fields: [] },
      });
      await expect(request(worker, {
        id: 6,
        operation: "search",
        query: "stableterm",
        limit: 20,
      })).resolves.toMatchObject({ ok: true, result: { generation: "g2", hits: [] } });
      await expect(request(worker, {
        id: 7,
        operation: "search",
        query: "newterm",
        limit: 20,
      })).resolves.toMatchObject({
        ok: true,
        result: { generation: "g2", hits: [{ path: "renamed.md" }] },
      });
      await expect(request(worker, {
        id: 8,
        operation: "apply_source_changes",
        generation: "g1",
        next_generation: "g3",
        upserts: [],
        removals: [{ vault_id: "active-vault", path: "renamed.md" }],
      })).resolves.toMatchObject({ ok: false, error: { code: "invalid_state" } });
      await expect(request(worker, {
        id: 9,
        operation: "apply_source_changes",
        generation: "g2",
        next_generation: "g1",
        upserts: [],
        removals: [{ vault_id: "active-vault", path: "renamed.md" }],
      })).resolves.toMatchObject({ ok: false, error: { code: "invalid_state" } });
      await expect(request(worker, { id: 10, operation: "status" })).resolves.toMatchObject({
        ok: true,
        result: { active_generation: "g2", documents: 1, chunks: 1 },
      });
      await request(worker, { id: 11, operation: "dispose" });
    } finally {
      await worker.terminate();
    }
  }, 120_000);

  it("replays staging changes without exposing them before commit", async () => {
    const worker = new Worker(nodeWorkerSource(workerSource), { eval: true });
    try {
      await request(worker, { id: 1, operation: "initialize", vault_id: "active-vault" });
      await request(worker, { id: 2, operation: "begin_build", generation: "g1" });
      await request(worker, {
        id: 3,
        operation: "add_source_batch",
        generation: "g1",
        sources: [source("active.md", "activeterm")],
      });
      await request(worker, { id: 4, operation: "commit_build", generation: "g1" });
      await request(worker, { id: 5, operation: "begin_build", generation: "g2" });
      await request(worker, {
        id: 6,
        operation: "add_source_batch",
        generation: "g2",
        sources: [source("snapshot.md", "snapshotterm")],
      });
      await expect(request(worker, {
        id: 7,
        operation: "apply_source_changes",
        generation: "g2",
        next_generation: null,
        upserts: [source("replayed.md", "replayterm")],
        removals: [{ vault_id: "active-vault", path: "snapshot.md" }],
      })).resolves.toMatchObject({ ok: true, result: { generation: "g2" } });
      await expect(request(worker, {
        id: 8,
        operation: "apply_source_changes",
        generation: "g1",
        next_generation: "g3",
        upserts: [],
        removals: [{ vault_id: "active-vault", path: "active.md" }],
      })).resolves.toMatchObject({ ok: false, error: { code: "invalid_state" } });
      await expect(request(worker, {
        id: 9,
        operation: "search",
        query: "replayterm",
        limit: 20,
      })).resolves.toMatchObject({ ok: true, result: { generation: "g1", hits: [] } });
      await expect(request(worker, {
        id: 10,
        operation: "search",
        query: "activeterm",
        limit: 20,
      })).resolves.toMatchObject({
        ok: true,
        result: { generation: "g1", hits: [{ path: "active.md" }] },
      });
      await request(worker, { id: 11, operation: "commit_build", generation: "g2" });
      await expect(request(worker, {
        id: 12,
        operation: "search",
        query: "replayterm",
        limit: 20,
      })).resolves.toMatchObject({
        ok: true,
        result: { generation: "g2", hits: [{ path: "replayed.md" }] },
      });
      await request(worker, { id: 13, operation: "dispose" });
    } finally {
      await worker.terminate();
    }
  }, 120_000);

  it("preserves active content when any source in a mutation is rejected", async () => {
    const worker = new Worker(nodeWorkerSource(workerSource), { eval: true });
    try {
      await request(worker, { id: 1, operation: "initialize", vault_id: "active-vault" });
      await request(worker, { id: 2, operation: "begin_build", generation: "g1" });
      await request(worker, {
        id: 3,
        operation: "add_source_batch",
        generation: "g1",
        sources: [source("stable.md", "stableterm")],
      });
      await request(worker, { id: 4, operation: "commit_build", generation: "g1" });
      await expect(request(worker, {
        id: 5,
        operation: "apply_source_changes",
        generation: "g1",
        next_generation: "g2",
        upserts: [
          source("valid.md", "validterm"),
          source("../invalid.md", "invalidterm"),
        ],
        removals: [{ vault_id: "active-vault", path: "stable.md" }],
      })).resolves.toMatchObject({ ok: false, error: { code: "source_rejected" } });
      await expect(request(worker, { id: 6, operation: "status" })).resolves.toMatchObject({
        ok: true,
        result: { active_generation: "g1", documents: 1, chunks: 1 },
      });
      await expect(request(worker, {
        id: 7,
        operation: "search",
        query: "stableterm",
        limit: 20,
      })).resolves.toMatchObject({
        ok: true,
        result: { generation: "g1", hits: [{ path: "stable.md" }] },
      });
      await expect(request(worker, {
        id: 8,
        operation: "search",
        query: "validterm",
        limit: 20,
      })).resolves.toMatchObject({ ok: true, result: { generation: "g1", hits: [] } });
      await request(worker, { id: 9, operation: "dispose" });
    } finally {
      await worker.terminate();
    }
  }, 120_000);

  it("drops a failed staging replay and preserves the active generation", async () => {
    const worker = new Worker(nodeWorkerSource(workerSource), { eval: true });
    try {
      await request(worker, { id: 1, operation: "initialize", vault_id: "active-vault" });
      await request(worker, { id: 2, operation: "begin_build", generation: "g1" });
      await request(worker, {
        id: 3,
        operation: "add_source_batch",
        generation: "g1",
        sources: [source("active.md", "activeterm")],
      });
      await request(worker, { id: 4, operation: "commit_build", generation: "g1" });
      await request(worker, { id: 5, operation: "begin_build", generation: "bad-stage" });
      await expect(request(worker, {
        id: 6,
        operation: "apply_source_changes",
        generation: "bad-stage",
        next_generation: null,
        upserts: [source("../invalid.md", "invalidterm")],
        removals: [],
      })).resolves.toMatchObject({ ok: false, error: { code: "source_rejected" } });
      await expect(request(worker, { id: 7, operation: "status" })).resolves.toMatchObject({
        ok: true,
        result: {
          active_generation: "g1",
          staging_generation: null,
          searchable: true,
        },
      });
      await expect(request(worker, {
        id: 8,
        operation: "abort_build",
        generation: "bad-stage",
      })).resolves.toMatchObject({ ok: false, error: { code: "invalid_state" } });
      await expect(request(worker, {
        id: 9,
        operation: "begin_build",
        generation: "bad-stage",
      })).resolves.toMatchObject({ ok: false, error: { code: "invalid_state" } });
      await expect(request(worker, {
        id: 10,
        operation: "search",
        query: "activeterm",
        limit: 20,
      })).resolves.toMatchObject({
        ok: true,
        result: { generation: "g1", hits: [{ path: "active.md" }] },
      });
      await request(worker, { id: 11, operation: "dispose" });
    } finally {
      await worker.terminate();
    }
  }, 120_000);

  it("drops a staging generation that fails commit integrity and preserves active search", async () => {
    const needle = "target.index.assertIntegrity();";
    const injected = guardWorkerSource.replace(
      needle,
      `globalThis.__kwiryIntegrityCalls = (globalThis.__kwiryIntegrityCalls ?? 0) + 1;\n      if (globalThis.__kwiryIntegrityCalls === 2) target.index.assertIntegrity = () => { throw new Error("integrity failed"); };\n      ${needle}`,
    );
    expect(injected).not.toBe(guardWorkerSource);
    const worker = new Worker(nodeWorkerSource(injected), { eval: true });
    try {
      await request(worker, { id: 1, operation: "initialize", vault_id: "active-vault" });
      await request(worker, { id: 2, operation: "begin_build", generation: "g1" });
      await request(worker, {
        id: 3,
        operation: "add_source_batch",
        generation: "g1",
        sources: [source("stable.md", "stableterm")],
      });
      await request(worker, { id: 4, operation: "commit_build", generation: "g1" });
      await request(worker, { id: 5, operation: "begin_build", generation: "g2" });
      await request(worker, {
        id: 6,
        operation: "add_source_batch",
        generation: "g2",
        sources: [source("replacement.md", "replacementterm")],
      });

      await expect(request(worker, {
        id: 7,
        operation: "commit_build",
        generation: "g2",
      })).resolves.toMatchObject({
        ok: false,
        error: { code: "integrity_failed" },
      });
      await expect(request(worker, { id: 8, operation: "status" })).resolves.toMatchObject({
        ok: true,
        result: {
          active_generation: "g1",
          staging_generation: null,
          searchable: true,
        },
      });
      await expect(request(worker, {
        id: 9,
        operation: "search",
        query: "stableterm",
        limit: 20,
      })).resolves.toMatchObject({
        ok: true,
        result: { generation: "g1", hits: [{ path: "stable.md" }] },
      });
    } finally {
      await worker.terminate();
    }
  }, 120_000);

  // contentless_delete=1 replaces the removed external-content triggers: a
  // removal must drop the postings, and the rowid freed by that removal must
  // not resurrect them when the same path is indexed again.
  it("removes contentless postings on delete and never resurrects them", async () => {
    const worker = new Worker(nodeWorkerSource(workerSource), { eval: true });
    try {
      await request(worker, { id: 1, operation: "initialize", vault_id: "active-vault" });
      await request(worker, { id: 2, operation: "begin_build", generation: "g1" });
      await request(worker, {
        id: 3,
        operation: "add_source_batch",
        generation: "g1",
        sources: [source("alpha.md", "removableterm"), source("keep.md", "keepterm")],
      });
      await request(worker, { id: 4, operation: "commit_build", generation: "g1" });

      await expect(request(worker, {
        id: 5,
        operation: "apply_source_changes",
        generation: "g1",
        next_generation: "g2",
        upserts: [],
        removals: [{ vault_id: "active-vault", path: "alpha.md" }],
      })).resolves.toMatchObject({
        ok: true,
        result: { generation: "g2", documents: 1, chunks: 1, quarantined_sources: 0, quarantine_fields: [] },
      });
      await expect(request(worker, {
        id: 6,
        operation: "search",
        query: "removableterm",
        limit: 20,
      })).resolves.toMatchObject({ ok: true, result: { hits: [] } });

      await expect(request(worker, {
        id: 7,
        operation: "apply_source_changes",
        generation: "g2",
        next_generation: "g3",
        upserts: [source("alpha.md", "replacementterm")],
        removals: [],
      })).resolves.toMatchObject({
        ok: true,
        result: { generation: "g3", documents: 2, chunks: 2, quarantined_sources: 0, quarantine_fields: [] },
      });
      await expect(request(worker, {
        id: 8,
        operation: "search",
        query: "removableterm",
        limit: 20,
      })).resolves.toMatchObject({ ok: true, result: { hits: [] } });
      await expect(request(worker, {
        id: 9,
        operation: "search",
        query: "replacementterm",
        limit: 20,
      })).resolves.toMatchObject({ ok: true, result: { hits: [{ path: "alpha.md" }] } });
      await expect(request(worker, {
        id: 10,
        operation: "search",
        query: "keepterm",
        limit: 20,
      })).resolves.toMatchObject({ ok: true, result: { hits: [{ path: "keep.md" }] } });

      await request(worker, { id: 11, operation: "dispose" });
    } finally {
      await worker.terminate();
    }
  }, 120_000);

  it("compacts a staging generation before publication and never publishes an uncompacted one", async () => {
    const needle = "target.index.compact();";
    const injected = guardWorkerSource.replace(
      needle,
      `globalThis.__kwiryCompactCalls = (globalThis.__kwiryCompactCalls ?? 0) + 1;\n    if (globalThis.__kwiryCompactCalls === 2) target.index.compact = () => { throw new Error("compaction failed"); };\n    ${needle}`,
    );
    expect(injected).not.toBe(guardWorkerSource);
    const worker = new Worker(nodeWorkerSource(injected), { eval: true });
    try {
      await request(worker, { id: 1, operation: "initialize", vault_id: "active-vault" });
      await request(worker, { id: 2, operation: "begin_build", generation: "g1" });
      await request(worker, {
        id: 3,
        operation: "add_source_batch",
        generation: "g1",
        sources: [source("stable.md", "stableterm")],
      });
      await expect(request(worker, {
        id: 4,
        operation: "commit_build",
        generation: "g1",
      })).resolves.toMatchObject({ ok: true, result: { generation: "g1" } });

      await request(worker, { id: 5, operation: "begin_build", generation: "g2" });
      await request(worker, {
        id: 6,
        operation: "add_source_batch",
        generation: "g2",
        sources: [source("replacement.md", "replacementterm")],
      });
      await expect(request(worker, {
        id: 7,
        operation: "commit_build",
        generation: "g2",
      })).resolves.toMatchObject({ ok: false, error: { code: "integrity_failed" } });

      await expect(request(worker, { id: 8, operation: "status" })).resolves.toMatchObject({
        ok: true,
        result: { active_generation: "g1", staging_generation: null, searchable: true },
      });
      await expect(request(worker, {
        id: 9,
        operation: "search",
        query: "replacementterm",
        limit: 20,
      })).resolves.toMatchObject({ ok: true, result: { generation: "g1", hits: [] } });
      await expect(request(worker, {
        id: 10,
        operation: "search",
        query: "stableterm",
        limit: 20,
      })).resolves.toMatchObject({
        ok: true,
        result: { generation: "g1", hits: [{ path: "stable.md" }] },
      });
    } finally {
      await worker.terminate();
    }
  }, 120_000);

  // `compact()` VACUUMs, which rewrites the whole database after the commit
  // gate already ran. The artifact that ships must be the artifact that passed,
  // so the gate is re-run on the compacted image.
  it("re-verifies the compacted image and refuses to publish a corrupted one", async () => {
    const needle = "target.index.compact();";
    const injected = guardWorkerSource.replace(
      needle,
      `${needle}\n    target.index.assertIntegrity = () => { throw new Error("post-compaction divergence"); };`,
    );
    expect(injected).not.toBe(guardWorkerSource);
    const worker = new Worker(nodeWorkerSource(injected), { eval: true });
    try {
      await request(worker, { id: 1, operation: "initialize", vault_id: "active-vault" });
      await request(worker, { id: 2, operation: "begin_build", generation: "g1" });
      await request(worker, {
        id: 3,
        operation: "add_source_batch",
        generation: "g1",
        sources: [source("stable.md", "stableterm")],
      });

      await expect(request(worker, {
        id: 4,
        operation: "commit_build",
        generation: "g1",
      })).resolves.toMatchObject({ ok: false, error: { code: "integrity_failed" } });
      await expect(request(worker, { id: 5, operation: "status" })).resolves.toMatchObject({
        ok: true,
        result: { active_generation: null, staging_generation: null, searchable: false },
      });
      await expect(request(worker, {
        id: 6,
        operation: "search",
        query: "stableterm",
        limit: 20,
      })).resolves.toMatchObject({ ok: false, error: { code: "index_building" } });
    } finally {
      await worker.terminate();
    }
  }, 120_000);

  // A published generation is mutated in place and never re-enters the commit
  // gate, so its reconciliation runs inside that transaction. The injected
  // posting is exactly the divergence the removed external-content triggers
  // used to make structurally impossible.
  it("refuses an in-place active update whose postings diverge from its chunk rows", async () => {
    const needle = "active.index.applySourceChanges(prepared.preparations, request.removals, true);";
    const injected = guardWorkerSource.replace(
      needle,
      `active.index.db.exec("INSERT INTO chunks_fts(rowid, content) VALUES(900000, 'sabotageterm')");\n    ${needle}`,
    );
    expect(injected).not.toBe(guardWorkerSource);
    const worker = new Worker(nodeWorkerSource(injected), { eval: true });
    try {
      await request(worker, { id: 1, operation: "initialize", vault_id: "active-vault" });
      await request(worker, { id: 2, operation: "begin_build", generation: "g1" });
      await request(worker, {
        id: 3,
        operation: "add_source_batch",
        generation: "g1",
        sources: [source("stable.md", "stableterm")],
      });
      await request(worker, { id: 4, operation: "commit_build", generation: "g1" });

      await expect(request(worker, {
        id: 5,
        operation: "apply_source_changes",
        generation: "g1",
        next_generation: "g2",
        upserts: [source("added.md", "addedterm")],
        removals: [],
      })).resolves.toMatchObject({ ok: false, error: { code: "integrity_failed" } });

      await expect(request(worker, { id: 6, operation: "status" })).resolves.toMatchObject({
        ok: true,
        result: { active_generation: "g1", documents: 1, chunks: 1, searchable: true },
      });
      await expect(request(worker, {
        id: 7,
        operation: "search",
        query: "addedterm",
        limit: 20,
      })).resolves.toMatchObject({ ok: true, result: { generation: "g1", hits: [] } });
      await expect(request(worker, {
        id: 8,
        operation: "search",
        query: "stableterm",
        limit: 20,
      })).resolves.toMatchObject({
        ok: true,
        result: { generation: "g1", hits: [{ path: "stable.md" }] },
      });
    } finally {
      await worker.terminate();
    }
  }, 120_000);

  it("aborts publication before the swap when prior-generation cleanup fails", async () => {
    const needle = "previous?.index.close();";
    const injected = guardWorkerSource.replace(
      needle,
      `if (previous) previous.index.close = () => { throw new Error("cleanup failed"); };\n    ${needle}`,
    );
    expect(injected).not.toBe(guardWorkerSource);
    const worker = new Worker(nodeWorkerSource(injected), { eval: true });
    try {
      await request(worker, { id: 1, operation: "initialize", vault_id: "active-vault" });
      await request(worker, { id: 2, operation: "begin_build", generation: "g1" });
      await request(worker, {
        id: 3,
        operation: "add_source_batch",
        generation: "g1",
        sources: [source("old.md", "oldterm")],
      });
      await request(worker, { id: 4, operation: "commit_build", generation: "g1" });
      await request(worker, { id: 5, operation: "begin_build", generation: "g2" });
      await request(worker, {
        id: 6,
        operation: "add_source_batch",
        generation: "g2",
        sources: [source("new.md", "newterm")],
      });

      await expect(request(worker, {
        id: 7,
        operation: "commit_build",
        generation: "g2",
      })).resolves.toMatchObject({
        ok: false,
        error: { code: "worker_crashed", stage: "lifecycle", retryable: true },
      });
      await expect(request(worker, { id: 8, operation: "status" })).resolves.toMatchObject({
        ok: true,
        result: {
          active_generation: "g1",
          staging_generation: null,
          searchable: true,
        },
      });
      await expect(request(worker, {
        id: 9,
        operation: "search",
        query: "oldterm",
        limit: 20,
      })).resolves.toMatchObject({
        ok: true,
        result: { generation: "g1", hits: [{ path: "old.md" }] },
      });
      await expect(request(worker, {
        id: 10,
        operation: "search",
        query: "newterm",
        limit: 20,
      })).resolves.toMatchObject({ ok: true, result: { generation: "g1", hits: [] } });
    } finally {
      await worker.terminate();
    }
  }, 120_000);

  it("rejects a corrupted embedded SQLite artifact before runtime initialization", async () => {
    const sqlite = readFileSync(require.resolve("@sqlite.org/sqlite-wasm/sqlite3.wasm"));
    const encoded = sqlite.toString("base64");
    const replacement = `${encoded[0] === "A" ? "B" : "A"}${encoded.slice(1)}`;
    const corrupted = workerSource.replace(encoded, replacement);
    expect(corrupted).not.toBe(workerSource);

    const worker = new Worker(nodeWorkerSource(corrupted), { eval: true });
    try {
      await expect(request(worker, { id: 1, operation: "initialize", vault_id: "active-vault" })).resolves.toMatchObject({
        ok: false,
        error: { code: "artifact_mismatch", stage: "artifact" },
      });
    } finally {
      await worker.terminate();
    }
  }, 120_000);

  it.each([
    ["network", "guards = installGuards();", "try { globalThis.fetch; } catch {}"],
    ["helper Worker", "guards = installGuards();", "try { globalThis.Worker; } catch {}"],
    [
      "persistence",
      "installPersistenceGuards(guards);",
      "try { globalThis.indexedDB; } catch {}",
    ],
  ])("counts and rejects a denied %s capability probe", async (_name, needle, statement) => {
    const injected = injectGuardProbe(guardWorkerSource, needle, statement);
    const worker = new Worker(nodeWorkerSource(injected), { eval: true });
    try {
      await expect(request(worker, { id: 1, operation: "initialize", vault_id: "active-vault" })).resolves.toMatchObject({
        ok: false,
        error: { code: "sqlite_init_failed" },
      });
    } finally {
      await worker.terminate();
    }
  }, 120_000);

  it("completes ten fresh initialize/dispose cycles", async () => {
    for (let cycle = 0; cycle < 10; cycle += 1) {
      const worker = new Worker(nodeWorkerSource(workerSource), { eval: true });
      try {
        await expect(request(worker, { id: 1, operation: "initialize", vault_id: "active-vault" })).resolves.toMatchObject({
          ok: true,
        });
        await expect(request(worker, { id: 2, operation: "dispose" })).resolves.toMatchObject({
          ok: true,
          result: { closed: true },
        });
      } finally {
        await worker.terminate();
      }
    }
  }, 120_000);

  it("rejects duplicate request IDs", async () => {
    const worker = new Worker(nodeWorkerSource(workerSource), { eval: true });
    try {
      await request(worker, { id: 1, operation: "initialize", vault_id: "active-vault" });
      await expect(request(worker, { id: 1, operation: "status" })).resolves.toMatchObject({
        ok: false,
        error: { code: "invalid_request" },
      });
    } finally {
      await worker.terminate();
    }
  }, 120_000);
});
