// SPDX-FileCopyrightText: 2026 cybersader
// SPDX-License-Identifier: GPL-3.0-only

import { createHash } from "node:crypto";
import { readFile, rm, mkdtemp } from "node:fs/promises";
import { tmpdir } from "node:os";
import { dirname, resolve, join } from "node:path";
import { fileURLToPath } from "node:url";
import { Worker } from "node:worker_threads";
import { performance } from "node:perf_hooks";

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
const WORKER_PROTOCOL_VERSION = 6;
const PERFORMANCE_VAULT_ID = "gate5-performance-vault";

main().catch((error) => {
  const diagnostic = safeFailureDiagnostic(error);
  process.stderr.write(`Gate 5 generated performance capture failed (${diagnostic}).\n`);
  process.exitCode = 1;
});

async function main() {
  const corpusRoot = await mkdtemp(resolve(tmpdir(), "kwiry-gate5-performance-"));
  let worker;
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
    });
    requireOk(initialized);
    const initializeMs = performance.now() - startup;
    requireOk(await send({ operation: "begin_build", generation: "generation-0" }));

    const buildStart = performance.now();
    let firstBatchMs = null;
    let lastSuccessfulDocuments = 0;
    let lastSuccessfulChunks = 0;
    let lastSuccessfulDatabaseBytes = 0;
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
      if (firstBatchMs === null) firstBatchMs = performance.now() - buildStart;
    }
    const committed = await send({ operation: "commit_build", generation: "generation-0" });
    requireOk(committed);
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
      requireOk(await send({
        operation: "apply_source_changes",
        generation,
        next_generation: nextGeneration,
        upserts: [source(path, bytes, index + 2)],
        removals: [],
      }));
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
    const status = await send({ operation: "status" });
    requireOk(status);
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
        documents: status.result.documents,
        chunks: status.result.chunks,
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
      rejectPromise(new Error("Worker request timed out"));
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
  if (!(error instanceof Error)
    || (!/^worker-[a-z_]+-[a-z_]+$/u.test(error.message)
      && !/^capacity-[0-9]+-[0-9]+-[0-9]+$/u.test(error.message))) {
    return "unspecified";
  }
  return error.message;
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
