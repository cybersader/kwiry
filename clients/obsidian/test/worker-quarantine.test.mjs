// SPDX-FileCopyrightText: 2026 cybersader
// SPDX-License-Identifier: GPL-3.0-only

import { Worker } from "node:worker_threads";
import { beforeAll, describe, expect, it } from "vitest";

import { buildPlugin } from "../esbuild.config.mjs";
import { WORKER_PROTOCOL_VERSION, isWorkerResponse } from "../src/worker/protocol";

const SOURCE_POLICY_HASH = "0".repeat(64);

let injectedWorkerSource;

beforeAll(async () => {
  const { workerSource } = await buildPlugin({ write: false, production: false });
  const preparationNeedle = "function prepareSourceUpsert(source) {";
  const preparationInjection = `${preparationNeedle}\n  if (source.descriptor.path.startsWith("abort-")) {\n    const code = source.descriptor.path === "abort-artifact.md" ? "artifact_mismatch"\n      : source.descriptor.path === "abort-envelope.md" ? "invalid_response"\n      : "rust_init_failed";\n    throw new RustAdapterError(code, "Portable Rust failed.");\n  }\n  const rejectOnce = source.descriptor.path === "reject-once.md";\n  if (rejectOnce) globalThis.__kwiryRejectOnceCalls = (globalThis.__kwiryRejectOnceCalls ?? 0) + 1;\n  if (source.descriptor.path.startsWith("reject-") && (!rejectOnce || globalThis.__kwiryRejectOnceCalls === 1)) {\n    const error = new RustAdapterError("invalid_response", "Portable Rust returned invalid source data.");\n    error.defectField = source.descriptor.path === "reject-path.md" ? "path" : "chunks_contents";\n    throw error;\n  }`;
  injectedWorkerSource = workerSource.replace(preparationNeedle, preparationInjection);
  expect(injectedWorkerSource).not.toBe(workerSource);

  const indexNeedle = "target.index.applySourceChanges(prepared.preparations, []);";
  const indexInjection = `if (sources.some((source) => source.descriptor.path === "capacity-failure.md")) {\n      throw new IndexCapacityError();\n    }\n    if (sources.some((source) => source.descriptor.path === "sqlite-failure.md")) {\n      throw new Error("injected SQLite failure");\n    }\n    ${indexNeedle}`;
  const withIndexFailure = injectedWorkerSource.replace(indexNeedle, indexInjection);
  expect(withIndexFailure).not.toBe(injectedWorkerSource);
  injectedWorkerSource = withIndexFailure;
}, 120_000);

function nodeWorkerSource(source) {
  return `
    const { parentPort } = require("node:worker_threads");
    globalThis.self = globalThis;
    globalThis.postMessage = (message, transfer) => parentPort.postMessage(message, transfer);
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

async function initializedWorker() {
  const worker = new Worker(nodeWorkerSource(injectedWorkerSource), { eval: true });
  await request(worker, {
    id: 1,
    operation: "initialize",
    vault_id: "active-vault",
    source_policy_hash: SOURCE_POLICY_HASH,
  });
  return worker;
}

describe("Worker source quarantine", () => {
  it("indexes surviving sources when one preparation is rejected", async () => {
    const worker = await initializedWorker();
    try {
      await request(worker, { id: 2, operation: "begin_build", generation: "mixed" });
      const response = await request(worker, {
        id: 3,
        operation: "add_source_batch",
        generation: "mixed",
        sources: [
          source("alpha.md", "# Alpha\nsearchable-alpha"),
          source("reject-once.md", "# Rejected\nmissing-rejected"),
          source("beta.md", "# Beta\nsearchable-beta"),
        ],
      });

      expect(isWorkerResponse(response)).toBe(true);
      expect(response).toMatchObject({
        ok: true,
        result: {
          generation: "mixed",
          documents: 2,
          chunks: 2,
          quarantined_sources: 1,
          quarantine_fields: ["chunks_contents"],
        },
      });
      expect(JSON.stringify(response)).not.toContain("reject-once.md");
      expect(JSON.stringify(response)).not.toContain("missing-rejected");

      await expect(request(worker, {
        id: 4,
        operation: "commit_build",
        generation: "mixed",
      })).resolves.toMatchObject({
        ok: true,
        result: { documents: 2, chunks: 2, quarantined_sources: 1 },
      });
      await expect(request(worker, {
        id: 5,
        operation: "search",
        query: "searchable-alpha",
        limit: 20,
      })).resolves.toMatchObject({ ok: true, result: { hits: [{ path: "alpha.md" }] } });
      await expect(request(worker, {
        id: 6,
        operation: "search",
        query: "searchable-beta",
        limit: 20,
      })).resolves.toMatchObject({ ok: true, result: { hits: [{ path: "beta.md" }] } });

      await expect(request(worker, {
        id: 7,
        operation: "plan_reconciliation",
        generation: "mixed",
        vault_id: "active-vault",
        current_sources: [{
          path: "reject-once.md",
          byte_length: Buffer.byteLength("# Rejected\nmissing-rejected"),
          mtime_nanos: "1000001",
          indexable: true,
        }],
      })).resolves.toMatchObject({
        ok: true,
        result: { refresh: ["reject-once.md"] },
      });
      await expect(request(worker, {
        id: 8,
        operation: "apply_source_changes",
        generation: "mixed",
        next_generation: "recovered",
        upserts: [source("reject-once.md", "# Recovered\nnow-searchable")],
        removals: [],
      })).resolves.toMatchObject({
        ok: true,
        result: {
          generation: "recovered",
          documents: 3,
          chunks: 3,
          quarantined_sources: 0,
          quarantine_fields: [],
        },
      });
      await expect(request(worker, {
        id: 9,
        operation: "search",
        query: "now-searchable",
        limit: 20,
      })).resolves.toMatchObject({
        ok: true,
        result: { generation: "recovered", hits: [{ path: "reject-once.md" }] },
      });
    } finally {
      await worker.terminate();
    }
  }, 120_000);

  it("publishes an empty searchable generation when every preparation is rejected", async () => {
    const worker = await initializedWorker();
    try {
      await request(worker, { id: 2, operation: "begin_build", generation: "all-rejected" });
      await expect(request(worker, {
        id: 3,
        operation: "add_source_batch",
        generation: "all-rejected",
        sources: [
          source("reject-path.md", "first rejected source"),
          source("reject-once.md", "second rejected source"),
          source("reject-chunks.md", "third rejected source"),
        ],
      })).resolves.toMatchObject({
        ok: true,
        result: {
          documents: 0,
          chunks: 0,
          quarantined_sources: 3,
          quarantine_fields: ["chunks_contents", "path"],
        },
      });
      await expect(request(worker, {
        id: 4,
        operation: "commit_build",
        generation: "all-rejected",
      })).resolves.toMatchObject({
        ok: true,
        result: { documents: 0, chunks: 0, quarantined_sources: 3 },
      });
      await expect(request(worker, { id: 5, operation: "status" })).resolves.toMatchObject({
        ok: true,
        result: { phase: "ready", searchable: true, active_generation: "all-rejected" },
      });
    } finally {
      await worker.terminate();
    }
  }, 120_000);

  it("reports zero quarantines when every preparation succeeds", async () => {
    const worker = await initializedWorker();
    try {
      await request(worker, { id: 2, operation: "begin_build", generation: "clean" });
      await expect(request(worker, {
        id: 3,
        operation: "add_source_batch",
        generation: "clean",
        sources: [
          source("alpha.md", "# Alpha\nclean-alpha"),
          source("beta.md", "# Beta\nclean-beta"),
        ],
      })).resolves.toMatchObject({
        ok: true,
        result: {
          documents: 2,
          chunks: 2,
          quarantined_sources: 0,
          quarantine_fields: [],
        },
      });
    } finally {
      await worker.terminate();
    }
  }, 120_000);

  it.each([
    ["rust initialization", "abort-rust-init.md"],
    ["artifact mismatch", "abort-artifact.md"],
    ["malformed response envelope", "abort-envelope.md"],
  ])("aborts staging for %s without a defectField", async (_name, path) => {
    const worker = await initializedWorker();
    try {
      await request(worker, { id: 2, operation: "begin_build", generation: "rust-failure" });
      const response = await request(worker, {
        id: 3,
        operation: "add_source_batch",
        generation: "rust-failure",
        sources: [source(path, "must not be quarantined")],
      });
      expect(response).toMatchObject({
        ok: false,
        error: { code: "source_rejected", stage: "rust", retryable: false },
      });
      expect(JSON.stringify(response)).not.toContain(path);
      expect(JSON.stringify(response)).not.toContain("must not be quarantined");
      await expect(request(worker, { id: 4, operation: "status" })).resolves.toMatchObject({
        ok: true,
        result: { active_generation: null, staging_generation: null, searchable: false },
      });
    } finally {
      await worker.terminate();
    }
  }, 120_000);

  it("aborts staging when index capacity is exceeded", async () => {
    const worker = await initializedWorker();
    try {
      await request(worker, { id: 2, operation: "begin_build", generation: "capacity-failure" });
      await expect(request(worker, {
        id: 3,
        operation: "add_source_batch",
        generation: "capacity-failure",
        sources: [source("capacity-failure.md", "healthy preparation")],
      })).resolves.toMatchObject({
        ok: false,
        error: { code: "index_limit_exceeded", stage: "index", retryable: false },
      });
      await expect(request(worker, { id: 4, operation: "status" })).resolves.toMatchObject({
        ok: true,
        result: { active_generation: null, staging_generation: null, searchable: false },
      });
    } finally {
      await worker.terminate();
    }
  }, 120_000);

  it("aborts staging instead of quarantining a batch-level index failure", async () => {
    const worker = await initializedWorker();
    try {
      await request(worker, { id: 2, operation: "begin_build", generation: "batch-failure" });
      const response = await request(worker, {
        id: 3,
        operation: "add_source_batch",
        generation: "batch-failure",
        sources: [source("sqlite-failure.md", "healthy preparation")],
      });
      expect(response).toMatchObject({
        ok: false,
        error: { code: "source_rejected", stage: "index", retryable: false },
      });
      expect(JSON.stringify(response)).not.toContain("sqlite-failure.md");
      expect(JSON.stringify(response)).not.toContain("healthy preparation");

      await expect(request(worker, { id: 4, operation: "status" })).resolves.toMatchObject({
        ok: true,
        result: {
          active_generation: null,
          staging_generation: null,
          searchable: false,
        },
      });
      await expect(request(worker, {
        id: 5,
        operation: "abort_build",
        generation: "batch-failure",
      })).resolves.toMatchObject({ ok: false, error: { code: "invalid_state" } });
    } finally {
      await worker.terminate();
    }
  }, 120_000);
});
