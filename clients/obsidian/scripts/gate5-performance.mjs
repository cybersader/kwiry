// SPDX-FileCopyrightText: 2026 cybersader
// SPDX-License-Identifier: GPL-3.0-only

import { createHash } from "node:crypto";
import { readFile, rm, mkdtemp } from "node:fs/promises";
import { tmpdir } from "node:os";
import { dirname, resolve, join } from "node:path";
import { fileURLToPath } from "node:url";
import { Worker } from "node:worker_threads";
import { performance } from "node:perf_hooks";

import sqlite3InitModule from "@sqlite.org/sqlite-wasm";
import esbuild from "esbuild";

import { buildPlugin } from "../esbuild.config.mjs";
import {
  GATE5_TARGETS,
  validateGate5GeneratedPerformanceEvidence,
} from "./gate5-evidence-schema.mjs";
import {
  PERFORMANCE_CORPUS_BYTES,
  PERFORMANCE_NOTE_COUNT,
  generatePerformanceCorpus,
  performanceNotePath,
} from "./gate5-corpus.mjs";

const scriptRoot = dirname(fileURLToPath(import.meta.url));
/** Occurs in the beacon line of every generated note, so it fills a result page. */
const HYDRATION_QUERY = "synthetic";
const HYDRATION_SAMPLES = 20;
const WORKER_PROTOCOL_VERSION = 8;
const PERFORMANCE_VAULT_ID = "gate5-performance-vault";
const CACHE_IDENTITY = "c".repeat(64);
const EXPORT_BLOB_LIMIT = 384 * 1024 * 1024;

main().catch((error) => {
  const diagnostic = safeFailureDiagnostic(error);
  process.stderr.write(`Gate 5 generated performance capture failed (${diagnostic}).\n`);
  process.exitCode = 1;
});

async function main() {
  const corpusRoot = await mkdtemp(resolve(tmpdir(), "kwiry-gate5-performance-"));
  let worker;
  let restoreWorker;
  let loopDelay;
  try {
    const corpus = await generatePerformanceCorpus(corpusRoot);
    const build = await buildPlugin({ write: false, production: true });
    const { workerSource } = build;
    await collectMainGarbage();
    const baselineRss = process.memoryUsage().rss;
    loopDelay = monitorEventLoopDelay(10);
    const startup = performance.now();
    worker = new Worker(nodeWorkerSource(workerSource), { eval: true });
    let requestId = 0;
    const send = (message) => request(worker, { id: ++requestId, ...message });
    const initialized = await send({
      operation: "initialize",
      vault_id: PERFORMANCE_VAULT_ID,
      source_policy_hash: CACHE_IDENTITY,
    });
    requireOk(initialized);
    const initializeMs = performance.now() - startup;
    requireOk(await send({ operation: "begin_build", generation: "generation-0" }));

    const buildStart = performance.now();
    let firstBatchMs = null;
    let lastSuccessfulDocuments = 0;
    let lastSuccessfulChunks = 0;
    let lastSuccessfulDatabaseBytes = 0;
    let peakDatabaseBytes = 0;
    for (let offset = 0; offset < PERFORMANCE_NOTE_COUNT; offset += 16) {
      const sources = [];
      const end = Math.min(PERFORMANCE_NOTE_COUNT, offset + 16);
      for (let index = offset; index < end; index += 1) {
        const path = performanceNotePath(index);
        const bytes = await readFile(join(corpusRoot, path));
        sources.push(source(path, bytes));
      }
      const batch = await send({
        operation: "add_source_batch",
        generation: "generation-0",
        sources,
      });
      if (!batch?.ok && batch?.error?.code === "index_limit_exceeded") {
        throw new Error(
          `capacity-${lastSuccessfulDocuments}-${lastSuccessfulChunks}-${lastSuccessfulDatabaseBytes}`,
        );
      }
      requireOk(batch);
      lastSuccessfulDocuments = batch.result.documents;
      lastSuccessfulChunks = batch.result.chunks;
      lastSuccessfulDatabaseBytes = batch.result.database_bytes;
      peakDatabaseBytes = Math.max(peakDatabaseBytes, batch.result.database_bytes);
      if (firstBatchMs === null) firstBatchMs = performance.now() - buildStart;
    }
    const committed = await send({ operation: "commit_build", generation: "generation-0" });
    requireOk(committed);
    peakDatabaseBytes = Math.max(peakDatabaseBytes, committed.result.database_bytes);
    const buildDurationMs = performance.now() - buildStart;

    for (let index = 0; index < 5; index += 1) {
      requireOk(await send({
        operation: "search",
        query: `performancebeacon${String(index).padStart(5, "0")}`,
        limit: 20,
      }));
    }
    const searchSamples = [];
    for (let index = 0; index < 40; index += 1) {
      const started = performance.now();
      const result = await send({
        operation: "search",
        query: `performancebeacon${String(index * 17).padStart(5, "0")}`,
        limit: 20,
      });
      requireOk(result);
      searchSamples.push(performance.now() - started);
    }

    // Everything above stops at the Worker RPC boundary. Excerpt text is no
    // longer produced there, so the host-side read-and-hydrate step is timed
    // separately over a full result page; without it a generated evidence file
    // would certify a search target that omits most of a search's real cost.
    const hydration = await loadExcerptHydration();
    const hydrationSamples = [];
    for (let index = 0; index < HYDRATION_SAMPLES; index += 1) {
      const page = await send({
        operation: "search",
        query: HYDRATION_QUERY,
        limit: 20,
      });
      requireOk(page);
      if (page.result.hits.length === 0) throw new Error("hydration probe returned no hits");
      const started = performance.now();
      const hydrate = hydration.createExcerptHydrator(
        hydration.extractHighlightTerms(HYDRATION_QUERY),
      );
      const sources = new Map();
      for (const path of new Set(page.result.hits.map((hit) => hit.path))) {
        sources.set(path, { kind: "text", text: await readFile(join(corpusRoot, path), "utf8") });
      }
      for (const hit of page.result.hits) {
        hydrate(hit.path, sources.get(hit.path), hit.heading_path);
      }
      hydrationSamples.push(performance.now() - started);
    }

    let generation = "generation-0";
    const updateSamples = [];
    for (let index = 0; index < 20; index += 1) {
      const path = performanceNotePath(index);
      const original = await readFile(join(corpusRoot, path));
      const marker = `\nupdatebeacon${String(index).padStart(2, "0")}\n`;
      const bytes = Buffer.concat([original, Buffer.from(marker)]);
      const nextGeneration = `generation-${index + 1}`;
      const started = performance.now();
      const updated = await send({
        operation: "apply_source_changes",
        generation,
        next_generation: nextGeneration,
        upserts: [source(path, bytes, index + 2)],
        removals: [],
      });
      requireOk(updated);
      peakDatabaseBytes = Math.max(peakDatabaseBytes, updated.result.database_bytes);
      const visible = await send({
        operation: "search",
        query: `updatebeacon${String(index).padStart(2, "0")}`,
        limit: 20,
      });
      requireOk(visible);
      if (visible.result.hits.length < 1) throw new Error("updated source was not visible");
      updateSamples.push(performance.now() - started);
      generation = nextGeneration;
    }

    loopDelay.stop();
    await collectWorkerGarbage(worker);
    await collectMainGarbage();
    const addedRssMiB = Math.max(0, process.memoryUsage().rss - baselineRss) / (1024 * 1024);

    const exported = await send({
      operation: "export_generation",
      generation,
      cache_identity: CACHE_IDENTITY,
    });
    requireOk(exported);
    const envelope = exported.result;
    if (!(envelope.bytes instanceof Uint8Array)
      || envelope.bytes.byteLength !== envelope.blob_byte_length
      || envelope.blob_byte_length > EXPORT_BLOB_LIMIT) {
      throw new Error("worker-export-invalid");
    }
    const storage = await inspectExportedStorage(envelope.bytes, peakDatabaseBytes);

    restoreWorker = new Worker(nodeWorkerSource(workerSource), { eval: true });
    let restoreRequestId = 0;
    const sendRestored = (message) => request(
      restoreWorker,
      { id: ++restoreRequestId, ...message },
      300_000,
    );
    requireOk(await sendRestored({
      operation: "initialize",
      vault_id: PERFORMANCE_VAULT_ID,
      source_policy_hash: CACHE_IDENTITY,
    }));
    requireOk(await sendRestored(restoreRequest(envelope, generation)));
    for (const [probe, query] of [
      ["ordinary", "performancebeacon00000"],
      ["exact", "w0001"],
      ["combined", "synthetic performancebeacon00000"],
      ["update", "updatebeacon00"],
    ]) {
      const restoredSearch = await sendRestored({ operation: "search", query, limit: 20 });
      requireOk(restoredSearch);
      if (restoredSearch.result.hits.length === 0) {
        throw new Error(`worker-restore_search-${probe}_empty`);
      }
    }
    const restoredStatus = await sendRestored({ operation: "status" });
    requireOk(restoredStatus);

    const status = await send({ operation: "status" });
    requireOk(status);
    if (status.result.documents !== restoredStatus.result.documents
      || status.result.chunks !== restoredStatus.result.chunks
      || status.result.active_database_bytes !== storage.bytes.final_database_bytes) {
      throw new Error("worker-status-invalid");
    }
    const measurements = {
      worker_initialize_ms: round(initializeMs),
      first_batch_ms: round(firstBatchMs ?? 0),
      build_duration_ms: round(buildDurationMs),
      warm_search_p95_ms: round(percentile95(searchSamples)),
      hydration_p95_ms: round(percentile95(hydrationSamples)),
      update_visibility_p95_ms: round(percentile95(updateSamples)),
      max_event_loop_delay_ms: round(loopDelay.max()),
      added_rss_mib: round(addedRssMiB),
    };
    const evidence = {
      schema_version: 1,
      kind: "kwiry_gate5_generated_performance",
      verdict: "EVIDENCE_CAPTURE_COMPLETE_OWNER_DECISION_REQUIRED",
      host: "node_worker_threads",
      artifact: {
        worker: identity(workerSource),
        rust_wasm: build.identities.rust,
        sqlite_wasm: build.identities.sqlite,
      },
      corpus: {
        kind: corpus.kind,
        note_count: corpus.note_count,
        markdown_bytes: corpus.markdown_bytes,
        sha256: corpus.sha256,
        hash_algorithm: corpus.hash_algorithm,
        expected_documents: corpus.expected_documents,
        seed_u32: corpus.seed_u32,
      },
      index: {
        documents: restoredStatus.result.documents,
        chunks: restoredStatus.result.chunks,
        sources: storage.sources,
      },
      storage: {
        ...storage.bytes,
        max_page_count: Math.floor(
          status.result.database_byte_limit / storage.bytes.page_size,
        ),
        database_byte_limit: status.result.database_byte_limit,
        export_blob_limit: EXPORT_BLOB_LIMIT,
      },
      measurements,
      samples: {
        warm_search: searchSamples.length,
        hydration: hydrationSamples.length,
        update_visibility: updateSamples.length,
      },
      targets: generatedTargets(measurements),
      privacy: privacyCounters(),
    };
    validateGate5GeneratedPerformanceEvidence(evidence);
    process.stdout.write(`${JSON.stringify(evidence)}\n`);
    requireOk(await send({ operation: "dispose" }));
  } finally {
    loopDelay?.stop();
    await restoreWorker?.terminate();
    await worker?.terminate();
    await rm(corpusRoot, { recursive: true, force: true });
  }
}

function nodeWorkerSource(sourceText) {
  return `
    const { parentPort } = require("node:worker_threads");
    globalThis.self = globalThis;
    globalThis.postMessage = (message) => parentPort.postMessage(message);
    globalThis.addEventListener = (type, listener) => {
      if (type !== "message") return;
      parentPort.on("message", (data) => {
        if (data?.__kwiryHarnessOperation === "collect_garbage") {
          globalThis.gc?.();
          parentPort.postMessage({ __kwiryHarnessOperation: "garbage_collected" });
          return;
        }
        listener({ data });
      });
    };
    globalThis.close = () => process.exit(0);
    ${sourceText}
  `;
}

function restoreRequest(envelope, generation) {
  return {
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
    source_policy_hash: envelope.source_policy_hash,
    expected_source_policy_hash: envelope.source_policy_hash,
  };
}

async function inspectExportedStorage(image, peakDatabaseBytes) {
  const sqlite = await sqlite3InitModule({ print: () => undefined, printErr: () => undefined });
  const db = deserialize(sqlite, image);
  try {
    const pageSize = Number(db.selectValue("PRAGMA page_size"));
    const pageCount = Number(db.selectValue("PRAGMA page_count"));
    const freelistCount = Number(db.selectValue("PRAGMA freelist_count"));
    const categories = {
      main_chunks_bytes: 0,
      main_fts_bytes: 0,
      exact_identifier_fts_bytes: 0,
      properties_bytes: 0,
      sources_bytes: 0,
      other_indexes_bytes: 0,
    };
    for (const row of db.selectObjects(
      "SELECT name, sum(pgsize) AS bytes FROM dbstat GROUP BY name ORDER BY name",
    )) {
      if (typeof row.name !== "string" || !Number.isSafeInteger(row.bytes) || row.bytes < 0) {
        throw new Error("worker-storage-invalid");
      }
      categories[storageCategory(row.name)] += row.bytes;
    }
    return {
      sources: Number(db.selectValue("SELECT count(*) FROM sources")),
      bytes: {
        page_size: pageSize,
        page_count: pageCount,
        freelist_count: freelistCount,
        peak_database_bytes: peakDatabaseBytes,
        final_database_bytes: pageSize * pageCount,
        export_blob_bytes: image.byteLength,
        ...categories,
      },
    };
  } finally {
    db.close();
  }
}

function storageCategory(name) {
  if (name === "chunks") return "main_chunks_bytes";
  if (name.startsWith("chunks_fts_")) return "main_fts_bytes";
  if (name.startsWith("chunk_exact_identifier_fts_")) {
    return "exact_identifier_fts_bytes";
  }
  if (name.startsWith("source_properties")
    || name.startsWith("source_property_scalars")
    || name.startsWith("source_property_text_fts")
    || name.startsWith("sqlite_autoindex_source_properties_")
    || name.startsWith("sqlite_autoindex_source_property_scalars_")) {
    return "properties_bytes";
  }
  if (name === "sources"
    || name.startsWith("sources_")
    || name.startsWith("source_exact_aliases")
    || name.startsWith("sqlite_autoindex_sources_")) {
    return "sources_bytes";
  }
  return "other_indexes_bytes";
}

function deserialize(sqlite, image) {
  const db = new sqlite.oo1.DB(":memory:", "c");
  const pointer = sqlite.wasm.allocFromTypedArray(image);
  const rc = sqlite.capi.sqlite3_deserialize(
    db.pointer,
    "main",
    pointer,
    image.byteLength,
    image.byteLength,
    sqlite.capi.SQLITE_DESERIALIZE_FREEONCLOSE
      | sqlite.capi.SQLITE_DESERIALIZE_RESIZEABLE,
  );
  if (rc !== 0) {
    db.close();
    throw new Error("worker-storage-invalid");
  }
  return db;
}

async function collectMainGarbage() {
  if (typeof globalThis.gc !== "function") {
    throw new Error("performance harness requires exposed garbage collection");
  }
  globalThis.gc();
  await new Promise((resolvePromise) => setTimeout(resolvePromise, 50));
  globalThis.gc();
}

function collectWorkerGarbage(worker, timeoutMs = 30_000) {
  return new Promise((resolvePromise, rejectPromise) => {
    const timeout = setTimeout(() => {
      cleanup();
      rejectPromise(new Error("Worker garbage collection timed out"));
    }, timeoutMs);
    const onMessage = (response) => {
      if (response?.__kwiryHarnessOperation !== "garbage_collected") return;
      cleanup();
      resolvePromise();
    };
    const onError = () => {
      cleanup();
      rejectPromise(new Error("Worker failed during garbage collection"));
    };
    const cleanup = () => {
      clearTimeout(timeout);
      worker.off("message", onMessage);
      worker.off("error", onError);
    };
    worker.on("message", onMessage);
    worker.on("error", onError);
    worker.postMessage({ __kwiryHarnessOperation: "collect_garbage" });
  });
}

function request(worker, message, timeoutMs = 120_000) {
  return new Promise((resolvePromise, rejectPromise) => {
    const timeout = setTimeout(() => {
      cleanup();
      rejectPromise(new Error(`Worker request timed out: ${message.operation}`));
    }, timeoutMs);
    const onMessage = (response) => {
      if (response?.id !== message.id) return;
      cleanup();
      resolvePromise(response);
    };
    const onError = () => {
      cleanup();
      rejectPromise(new Error("Worker failed"));
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

function source(path, bytes, mtime = 1) {
  return {
    descriptor: {
      vault_id: PERFORMANCE_VAULT_ID,
      path,
      format: "markdown",
      byte_length: bytes.byteLength,
      mtime,
      mtime_nanos: `${mtime}000000000`,
    },
    bytes,
  };
}

function monitorEventLoopDelay(intervalMs) {
  let previous = performance.now();
  let maximum = 0;
  const timer = setInterval(() => {
    const now = performance.now();
    maximum = Math.max(maximum, now - previous - intervalMs);
    previous = now;
  }, intervalMs);
  return {
    stop: () => clearInterval(timer),
    max: () => maximum,
  };
}

function percentile95(values) {
  if (values.length === 0) throw new Error("performance sample set is empty");
  const sorted = [...values].sort((left, right) => left - right);
  return sorted[Math.ceil(sorted.length * 0.95) - 1];
}

function requireOk(response) {
  if (response?.ok) return;
  const stage = typeof response?.error?.stage === "string" ? response.error.stage : "unknown";
  const code = typeof response?.error?.code === "string" ? response.error.code : "unknown";
  throw new Error(`worker-${stage}-${code}`);
}

function safeFailureDiagnostic(error) {
  if (!(error instanceof Error)) return "unspecified";
  if (/^worker-[a-z_]+-[a-z_]+$/u.test(error.message)
    || /^capacity-[0-9]+-[0-9]+-[0-9]+$/u.test(error.message)) {
    return error.message;
  }
  return error.message.length <= 200
    && /^[A-Za-z0-9 _().:'-]+$/u.test(error.message)
    && !/\b(?:SELECT|INSERT|UPDATE|DELETE|CREATE|DROP|ALTER|PRAGMA|MATCH)\b/iu.test(error.message)
    ? error.message
    : "unspecified";
}

/**
 * Loads the shipped excerpt hydration module so the measurement runs the same
 * code the plugin runs, not a re-implementation of it.
 */
async function loadExcerptHydration() {
  const bundled = await esbuild.build({
    entryPoints: [resolve(scriptRoot, "../src/hydrate-excerpt.ts")],
    bundle: true,
    write: false,
    format: "esm",
    platform: "neutral",
    target: "es2022",
  });
  const output = bundled.outputFiles?.[0];
  if (!output) throw new Error("excerpt hydration bundle is empty");
  const encoded = Buffer.from(output.text, "utf8").toString("base64");
  return import(`data:text/javascript;base64,${encoded}`);
}

function generatedTargets(measurements) {
  const measurementKeys = new Map([
    ["build_duration", "build_duration_ms"],
    ["warm_search_p95", "warm_search_p95_ms"],
    ["hydration_p95", "hydration_p95_ms"],
    ["update_visibility_p95", "update_visibility_p95_ms"],
    ["max_event_loop_delay", "max_event_loop_delay_ms"],
    ["added_steady_state_memory", "added_rss_mib"],
  ]);
  return GATE5_TARGETS.map(([id, threshold, unit]) => {
    const measurementKey = measurementKeys.get(id);
    if (!measurementKey) {
      return {
        id,
        threshold,
        unit,
        status: "unavailable",
        value: null,
        scope: "installed_obsidian_reference_hardware",
      };
    }
    const value = measurements[measurementKey];
    return {
      id,
      threshold,
      unit,
      status: value <= threshold ? "met" : "missed",
      value,
      scope: "generated_node_worker_threads",
    };
  });
}

function identity(value) {
  return {
    bytes: Buffer.byteLength(value),
    sha256: createHash("sha256").update(value).digest("hex"),
  };
}

function privacyCounters() {
  return {
    aggregate_only: true,
    paths_emitted: 0,
    vault_names_emitted: 0,
    note_content_emitted: 0,
    raw_queries_emitted: 0,
    tokens_emitted: 0,
    stack_traces_emitted: 0,
    sql_emitted: 0,
    environment_paths_emitted: 0,
    private_corpus_hashes_emitted: 0,
    loose_evidence_artifacts: 0,
  };
}

function round(value) {
  return Math.round(value * 100) / 100;
}
