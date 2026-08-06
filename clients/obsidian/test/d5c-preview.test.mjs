// SPDX-FileCopyrightText: 2026 cybersader
// SPDX-License-Identifier: GPL-3.0-only

import { readFileSync } from "node:fs";
import { Worker } from "node:worker_threads";
import { beforeAll, describe, expect, it } from "vitest";

import { buildPlugin } from "../esbuild.config.mjs";

const PLAYGROUND_VERSION = 1;
const PLAYGROUND_NAME = "internalD5cPlayground";
const corpus = JSON.parse(readFileSync(
  new URL("../../../fixtures/retrieval/d5c-balanced/corpus.json", import.meta.url),
  "utf8",
));
let playgroundWorkerSource;

beforeAll(async () => {
  const build = await buildPlugin({
    write: false,
    production: true,
    internalD5cPlayground: true,
  });
  playgroundWorkerSource = build.internalD5cPlayground.workerSource;
}, 180_000);

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

function request(worker, message, timeoutMs = 60_000) {
  return new Promise((resolve, reject) => {
    const timeout = setTimeout(() => {
      cleanup();
      reject(new Error(`Playground request timed out: ${message.operation}`));
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
    worker.postMessage({
      version: PLAYGROUND_VERSION,
      worker: PLAYGROUND_NAME,
      ...message,
    });
  });
}

async function initializedWorker() {
  const worker = new Worker(nodeWorkerSource(playgroundWorkerSource), { eval: true });
  const initialized = await request(worker, { id: 1, operation: "fixture_initialize" });
  expect(initialized).toMatchObject({
    version: PLAYGROUND_VERSION,
    worker: PLAYGROUND_NAME,
    id: 1,
    operation: "fixture_initialize",
    ok: true,
    result: {
      abi_version: 3,
      adapter: "kwiry-obsidian-wasm",
      scenario_id: "balanced-playground-v1",
    },
  });
  return worker;
}

async function builtWorker() {
  const worker = await initializedWorker();
  const built = await request(worker, {
    id: 2,
    operation: "fixture_build",
    fixture: corpus,
  });
  expect(built).toEqual({
    version: PLAYGROUND_VERSION,
    worker: PLAYGROUND_NAME,
    id: 2,
    operation: "fixture_build",
    ok: true,
    result: {
      scenario_id: "balanced-playground-v1",
      source_count: 23,
      evaluation_count: 31,
    },
  });
  return worker;
}

async function evaluate(worker, id, evaluationId) {
  return request(worker, {
    id,
    operation: "fixture_evaluate",
    evaluation_id: evaluationId,
  });
}

describe("private D5C Balanced playground evaluation", () => {
  it("renders every Rust-owned comparison with complete discrepancy semantics", async () => {
    const worker = await builtWorker();
    try {
      let id = 3;
      const results = new Map();
      for (const evaluation of corpus.evaluations) {
        const response = await evaluate(worker, id++, evaluation.id);
        expect(response).toMatchObject({
          version: PLAYGROUND_VERSION,
          worker: PLAYGROUND_NAME,
          operation: "fixture_evaluate",
          ok: true,
          result: { evaluation_id: evaluation.id },
        });
        const comparison = response.result.comparison;
        expect(comparison.disposition.kind).toBe(evaluation.expected_disposition);
        expect(comparison.text_results.label).toBe("text");
        if (evaluation.expected_disposition === "fatal") {
          expect(comparison).not.toHaveProperty("balanced_results");
          expect(comparison).not.toHaveProperty("explanation");
        } else {
          expect(comparison.balanced_results.label).toBe(evaluation.expected_disposition);
        }
        results.set(evaluation.id, comparison);
      }

      expect(results.get("stronger-text-counterexample-native")
        .balanced_results.ordered_candidate_ordinals).toEqual([0, 1]);
      expect(results.get("same-tier-recency-native")
        .balanced_results.ordered_candidate_ordinals).toEqual([1, 0]);
      expect(results.get("archive-hierarchy-lookalikes-native")
        .balanced_results.ordered_candidate_ordinals).toEqual([3, 0, 2, 1]);
      expect(results.get("future-untrusted-clock-native").disposition.kind)
        .toBe("neutralized_counterfactual");
      expect(results.get("discrepancy-malformed").disposition.kind).toBe("fatal");
    } finally {
      await worker.terminate();
    }
  }, 180_000);

  it("keeps all private explanation levels structural and free of source-shaped input", async () => {
    const worker = await builtWorker();
    try {
      let id = 3;
      for (const evaluation of corpus.evaluations) {
        const response = await evaluate(worker, id++, evaluation.id);
        const explanation = response.result.comparison.explanation;
        if (explanation === undefined) continue;
        const serialized = JSON.stringify(explanation);
        for (const source of corpus.sources) {
          for (const forbidden of [
            source.path,
            source.source.authorization_scope,
            source.source.source_key,
            source.provider.provider_id,
          ]) {
            expect(serialized).not.toContain(forbidden);
          }
        }
        for (const forbidden of [
          "approved", "draft", "balanced-properties-a", "1999999900", "2000000000",
        ]) {
          expect(serialized).not.toContain(forbidden);
        }
      }
    } finally {
      await worker.terminate();
    }
  }, 180_000);

  it("has its own monotonic sequence and only the four fixture operations", async () => {
    const first = await initializedWorker();
    const second = await initializedWorker();
    try {
      await expect(request(first, { id: 1, operation: "fixture_dispose" }))
        .resolves.toMatchObject({ ok: false, error: { code: "invalid_request", stage: "protocol" } });
      await expect(request(first, { id: 2, operation: "search", query: "x", limit: 1 }))
        .resolves.toMatchObject({
          ok: false,
          operation: "fixture_initialize",
          error: { code: "invalid_request", stage: "protocol" },
        });
      await expect(request(second, { id: 2, operation: "fixture_build", fixture: corpus }))
        .resolves.toMatchObject({ ok: true, result: { evaluation_count: 31 } });
      await expect(request(second, { id: 3, operation: "export_generation" }))
        .resolves.toMatchObject({ ok: false, error: { code: "invalid_request" } });
      await expect(request(second, { id: 4, operation: "fixture_dispose" }))
        .resolves.toEqual({
          version: PLAYGROUND_VERSION,
          worker: PLAYGROUND_NAME,
          id: 4,
          operation: "fixture_dispose",
          ok: true,
          result: { closed: true },
        });
    } finally {
      await first.terminate();
      await second.terminate();
    }
  }, 180_000);

  it("refuses non-fixture, malformed, duplicate, and oversized fixture packs", async () => {
    const cases = [];
    const active = structuredClone(corpus);
    active.sources[0].source.authorization_scope = "obsidian-active-vault";
    cases.push(active);

    const duplicate = structuredClone(corpus);
    duplicate.evaluations[1].id = duplicate.evaluations[0].id;
    cases.push(duplicate);

    const malformed = structuredClone(corpus);
    malformed.extra = true;
    cases.push(malformed);

    const oversized = structuredClone(corpus);
    oversized.sources[0].provider.padding = "x".repeat(1024 * 1024);
    cases.push(oversized);

    for (const fixture of cases) {
      const worker = await initializedWorker();
      try {
        const response = await request(worker, {
          id: 2,
          operation: "fixture_build",
          fixture,
        });
        expect(response).toMatchObject({
          ok: false,
          error: { code: "invalid_fixture", stage: "fixture", retryable: false },
        });
        expect(response).not.toHaveProperty("result");
      } finally {
        await worker.terminate();
      }
    }
  }, 180_000);
});
