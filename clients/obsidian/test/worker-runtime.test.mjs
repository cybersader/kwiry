// SPDX-FileCopyrightText: 2026 cybersader
// SPDX-License-Identifier: GPL-3.0-only

import { readFileSync } from "node:fs";
import { createRequire } from "node:module";
import { Worker } from "node:worker_threads";
import { beforeAll, describe, expect, it } from "vitest";

import { buildPlugin } from "../esbuild.config.mjs";

const require = createRequire(import.meta.url);
let workerSource;
let guardWorkerSource;

beforeAll(async () => {
  ({ workerSource } = await buildPlugin({ write: false, production: true }));
  ({ workerSource: guardWorkerSource } = await buildPlugin({ write: false, production: false }));
}, 120_000);

function nodeWorkerSource(source) {
  return `
    const { parentPort } = require("node:worker_threads");
    globalThis.self = globalThis;
    globalThis.postMessage = (message) => parentPort.postMessage(message);
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
    worker.postMessage({ version: 1, ...message });
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

describe("exact generated production Worker", () => {
  it("initializes both WASM runtimes and publishes only complete generations", async () => {
    const worker = new Worker(nodeWorkerSource(workerSource), { eval: true });
    try {
      await expect(request(worker, { id: 1, operation: "initialize" })).resolves.toMatchObject({
        ok: true,
        result: {
          rustAbiVersion: 1,
          sourceSchemaVersion: 1,
          querySchemaVersion: 2,
          matchPlanSchemaVersion: 1,
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
          hits: [{ chunk_id: expect.any(String), path: "alpha.md" }],
        },
      });

      await expect(request(worker, {
        id: 7,
        operation: "search",
        query: "iia 2 line",
        limit: 20,
      })).resolves.toMatchObject({ ok: true, result: { hits: [{ path: "alpha.md" }] } });
      await expect(request(worker, {
        id: 8,
        operation: "search",
        query: "title:\"IIA 2 line guide\" OR content:cache*",
        limit: 20,
      })).resolves.toMatchObject({ ok: true, result: { hits: [{ path: "alpha.md" }] } });

      await request(worker, { id: 9, operation: "begin_build", generation: "generation-2" });
      await request(worker, {
        id: 10,
        operation: "add_source_batch",
        generation: "generation-2",
        sources: [source("beta.md", "# Beta\nstagingterm")],
      });
      await expect(request(worker, {
        id: 11,
        operation: "search",
        query: "quasar",
        limit: 20,
      })).resolves.toMatchObject({
        ok: true,
        result: { generation: "generation-1", hits: [{ path: "alpha.md" }] },
      });
      await request(worker, { id: 12, operation: "abort_build", generation: "generation-2" });

      await expect(request(worker, { id: 13, operation: "dispose" })).resolves.toMatchObject({
        ok: true,
        result: { closed: true },
      });
    } finally {
      await worker.terminate();
    }
  }, 120_000);

  it("aborts a rejected staging generation and preserves the active generation", async () => {
    const worker = new Worker(nodeWorkerSource(workerSource), { eval: true });
    try {
      await request(worker, { id: 1, operation: "initialize" });
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

  it("rejects a corrupted embedded SQLite artifact before runtime initialization", async () => {
    const sqlite = readFileSync(require.resolve("@sqlite.org/sqlite-wasm/sqlite3.wasm"));
    const encoded = sqlite.toString("base64");
    const replacement = `${encoded[0] === "A" ? "B" : "A"}${encoded.slice(1)}`;
    const corrupted = workerSource.replace(encoded, replacement);
    expect(corrupted).not.toBe(workerSource);

    const worker = new Worker(nodeWorkerSource(corrupted), { eval: true });
    try {
      await expect(request(worker, { id: 1, operation: "initialize" })).resolves.toMatchObject({
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
      await expect(request(worker, { id: 1, operation: "initialize" })).resolves.toMatchObject({
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
        await expect(request(worker, { id: 1, operation: "initialize" })).resolves.toMatchObject({
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
      await request(worker, { id: 1, operation: "initialize" });
      await expect(request(worker, { id: 1, operation: "status" })).resolves.toMatchObject({
        ok: false,
        error: { code: "invalid_request" },
      });
    } finally {
      await worker.terminate();
    }
  }, 120_000);
});
