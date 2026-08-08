// SPDX-FileCopyrightText: 2026 cybersader
// SPDX-License-Identifier: GPL-3.0-only

import { Worker } from "node:worker_threads";
import { beforeAll, describe, expect, it } from "vitest";

import { buildPlugin } from "../esbuild.config.mjs";
import {
  INTERNAL_D5C_COMPARE_OPERATION,
  isD5cCompareResponse,
} from "../src/worker/d5c-compare-protocol";
import { WORKER_PROTOCOL_VERSION } from "../src/worker/protocol";

let workerSource;

beforeAll(async () => {
  ({ workerSource } = await buildPlugin({
    write: false,
    production: true,
    internalD5cOwnerHost: true,
    activeVaultCache: false,
    pluginIdentity: { id: "kwiry-d5c-balanced-playground", version: "0.0.3" },
  }));
}, 180_000);

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

function source(path, text, mtimeSeconds) {
  const bytes = Buffer.from(text, "utf8");
  return {
    descriptor: {
      vault_id: "active-vault",
      path,
      format: "markdown",
      byte_length: bytes.byteLength,
      mtime: mtimeSeconds * 1_000,
      mtime_nanos: `${mtimeSeconds}000000000`,
    },
    bytes,
  };
}

async function initializedWorker() {
  const worker = new Worker(nodeWorkerSource(workerSource), { eval: true });
  const initialized = await request(worker, {
    id: 1,
    operation: "initialize",
    vault_id: "active-vault",
  });
  expect(initialized).toMatchObject({ ok: true, operation: "initialize" });
  return worker;
}

function compare(
  id,
  generation,
  revision,
  query = "signal",
  limit = 10,
) {
  return {
    id,
    operation: INTERNAL_D5C_COMPARE_OPERATION,
    generation,
    revision,
    query,
    limit,
    query_time_epoch_seconds: "2000000000",
  };
}

const SOURCES = [
  source("old.md", "# Signal\nsignal reference material", 1_000_000_000),
  source("reference/recent.md", "# Signal\nsignal reference material", 1_999_999_900),
  source("archive/recent.md", "# Signal\nsignal reference material", 1_999_999_900),
];

describe("live D5C comparison Worker", () => {
  it("rejects cache operations at the owner Worker boundary", async () => {
    const worker = await initializedWorker();
    try {
      expect(await request(worker, {
        id: 2,
        operation: "export_generation",
        generation: "g1",
        cache_identity: "0".repeat(64),
      })).toMatchObject({
        ok: false,
        error: { code: "invalid_request", stage: "protocol" },
      });
    } finally {
      await worker.terminate();
    }
  });

  it("compares initial staging and active publication from one safe candidate pool", async () => {
    const worker = await initializedWorker();
    try {
      expect(await request(worker, { id: 2, operation: "begin_build", generation: "g1" }))
        .toMatchObject({ ok: true, result: { generation: "g1" } });
      expect(await request(worker, {
        id: 3,
        operation: "add_source_batch",
        generation: "g1",
        sources: SOURCES,
      })).toMatchObject({ ok: true, result: { generation: "g1", documents: 3 } });

      const partial = await request(worker, compare(4, "g1", 1));
      expect(isD5cCompareResponse(partial)).toBe(true);
      expect(partial).toMatchObject({
        ok: true,
        result: {
          schema_version: 2,
          generation: "g1",
          publication: "initial_staging",
          revision: 1,
          candidate_pool_count: 3,
        },
      });
      expect(partial.result.text_order).toHaveLength(3);
      expect(partial.result.balanced_order).toHaveLength(3);
      expect(new Set(partial.result.text_order)).toEqual(new Set(partial.result.balanced_order));
      const serialized = JSON.stringify(partial.result);
      for (const forbidden of ["score", "evidence", "points", "source_key", "property_rules"]) {
        expect(serialized).not.toContain(forbidden);
      }

      expect(await request(worker, { id: 5, operation: "commit_build", generation: "g1" }))
        .toMatchObject({ ok: true, result: { generation: "g1" } });
      const active = await request(worker, compare(6, "g1", null));
      expect(active).toMatchObject({
        ok: true,
        result: { generation: "g1", publication: "active", revision: null },
      });
      expect(active.result.text_order).toEqual(partial.result.text_order);
      expect(active.result.balanced_order).toEqual(partial.result.balanced_order);
    } finally {
      await worker.terminate();
    }
  }, 180_000);

  it("prefers active over replacement staging and revokes preview after replay mutation", async () => {
    const worker = await initializedWorker();
    try {
      await request(worker, { id: 2, operation: "begin_build", generation: "g1" });
      await request(worker, {
        id: 3,
        operation: "add_source_batch",
        generation: "g1",
        sources: SOURCES,
      });
      await request(worker, { id: 4, operation: "commit_build", generation: "g1" });
      await request(worker, { id: 5, operation: "begin_build", generation: "g2" });
      await request(worker, {
        id: 6,
        operation: "add_source_batch",
        generation: "g2",
        sources: [source("replacement.md", "# Signal\nsignal", 1_999_999_999)],
      });

      expect(await request(worker, compare(7, "g2", 1))).toMatchObject({
        ok: false,
        error: { code: "index_changed" },
      });
      expect(await request(worker, compare(8, "g1", null))).toMatchObject({
        ok: true,
        result: { publication: "active", generation: "g1" },
      });
    } finally {
      await worker.terminate();
    }

    const cold = await initializedWorker();
    try {
      await request(cold, { id: 2, operation: "begin_build", generation: "cold" });
      await request(cold, {
        id: 3,
        operation: "add_source_batch",
        generation: "cold",
        sources: SOURCES,
      });
      expect(await request(cold, compare(4, "cold", 1))).toMatchObject({ ok: true });
      await request(cold, {
        id: 5,
        operation: "apply_source_changes",
        generation: "cold",
        next_generation: null,
        upserts: [],
        removals: [],
      });
      expect(await request(cold, compare(6, "cold", 2))).toMatchObject({
        ok: false,
        error: { code: "index_changed" },
      });
    } finally {
      await cold.terminate();
    }
  }, 180_000);

  it("never lets metadata move a weaker text tier above a stronger match", async () => {
    const worker = await initializedWorker();
    try {
      await request(worker, { id: 2, operation: "begin_build", generation: "tiers" });
      await request(worker, {
        id: 3,
        operation: "add_source_batch",
        generation: "tiers",
        sources: [
          source(
            "archive/signal.md",
            "# Signal\nsignal signal signal exact phrase",
            1_000_000_000,
          ),
          source(
            "reference/recent-context.md",
            "# Context\nThis paragraph mentions signal once.",
            1_999_999_999,
          ),
        ],
      });
      await request(worker, { id: 4, operation: "commit_build", generation: "tiers" });

      const response = await request(worker, compare(5, "tiers", null));
      expect(response).toMatchObject({ ok: true });
      const paths = new Map(response.result.display_candidates.map((candidate) => [
        candidate.ordinal,
        candidate.hit.path,
      ]));
      const textPaths = response.result.text_order.map((ordinal) => paths.get(ordinal));
      const balancedPaths = response.result.balanced_order.map((ordinal) => paths.get(ordinal));
      expect(textPaths.indexOf("archive/signal.md"))
        .toBeLessThan(textPaths.indexOf("reference/recent-context.md"));
      expect(balancedPaths.indexOf("archive/signal.md"))
        .toBeLessThan(balancedPaths.indexOf("reference/recent-context.md"));
    } finally {
      await worker.terminate();
    }
  }, 180_000);

  it("keeps truncated Text and Balanced views as bounded orders over one pool", async () => {
    const worker = await initializedWorker();
    try {
      await request(worker, { id: 2, operation: "begin_build", generation: "bounded" });
      await request(worker, {
        id: 3,
        operation: "add_source_batch",
        generation: "bounded",
        sources: Array.from({ length: 8 }, (_, index) => source(
          index % 2 === 0
            ? `reference/note-${index}.md`
            : `archive/note-${index}.md`,
          `# Signal ${index}\nsignal shared material ${index}`,
          1_900_000_000 + index,
        )),
      });
      await request(worker, { id: 4, operation: "commit_build", generation: "bounded" });

      const first = await request(worker, compare(5, "bounded", null, "signal", 2));
      const second = await request(worker, compare(6, "bounded", null, "signal", 2));
      expect(first).toMatchObject({ ok: true });
      expect(first.result.candidate_pool_count).toBeGreaterThan(2);
      expect(first.result.text_order).toHaveLength(2);
      expect(first.result.balanced_order).toHaveLength(2);
      expect(first.result.aggregate.moved_candidate_count).toBe(
        first.result.text_order.filter(
          (ordinal, rank) => first.result.balanced_order[rank] !== ordinal,
        ).length,
      );
      expect(isD5cCompareResponse(first)).toBe(true);
      expect(new Set(first.result.text_order).size).toBe(2);
      expect(new Set(first.result.balanced_order).size).toBe(2);
      for (const ordinal of [
        ...first.result.text_order,
        ...first.result.balanced_order,
      ]) {
        expect(ordinal).toBeGreaterThanOrEqual(0);
        expect(ordinal).toBeLessThan(first.result.candidate_pool_count);
      }
      const displayed = new Set(
        first.result.display_candidates.map((candidate) => candidate.ordinal),
      );
      expect(displayed).toEqual(new Set([
        ...first.result.text_order,
        ...first.result.balanced_order,
      ]));
      expect(second.result).toEqual(first.result);
    } finally {
      await worker.terminate();
    }
  }, 180_000);
});
