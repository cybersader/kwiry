// SPDX-FileCopyrightText: 2026 cybersader
// SPDX-License-Identifier: GPL-3.0-only

import { createHash } from "node:crypto";
import { readFileSync } from "node:fs";
import { createRequire } from "node:module";
import { resolve } from "node:path";
import { runInNewContext } from "node:vm";
import { Worker } from "node:worker_threads";
import { build } from "esbuild";
import snappy from "snappyjs";
import { describe, expect, it } from "vitest";
import { compressedRustWasmPlugin } from "../scripts/community-bundle.mjs";
import { assertCommunityMainSize } from "../scripts/community-main-size.mjs";
import { WORKER_PROTOCOL_VERSION } from "../src/worker/protocol";

const require = createRequire(import.meta.url);
const root = resolve(import.meta.dirname, "..");
const sha256 = (bytes) => createHash("sha256").update(bytes).digest("hex");

describe("community plugin bundle", () => {
  it("rejects a main.js one byte over the browser host limit", () => {
    expect(() => assertCommunityMainSize(8_388_608)).not.toThrow();
    expect(() => assertCommunityMainSize(8_388_609)).toThrow(/8388609.*8388608/u);
  });

  it("inlines compressed WASM without changing its bytes or requiring webview decompression APIs", async () => {
    // Use the pinned SQLite WASM as a nontrivial fixture so this checks the
    // packer and bundled inflater without building the Rust adapter.
    const wasm = require.resolve("@sqlite.org/sqlite-wasm/sqlite3.wasm");
    const bytes = readFileSync(wasm);
    const bundled = await build({
      stdin: { contents: `import bytes from ${JSON.stringify(wasm)}; module.exports = bytes;`, resolveDir: process.cwd() },
      platform: "browser",
      format: "cjs",
      target: "es2022",
      bundle: true,
      minify: true,
      write: false,
      plugins: [compressedRustWasmPlugin({ wasm, bytes })],
    });
    expect(bundled.outputFiles).toHaveLength(1);
    const [output] = bundled.outputFiles;
    expect(output.contents.byteLength).toBeLessThan(bytes.byteLength);
    const module = { exports: {} };
    runInNewContext(output.text, { module, atob });
    const artifact = module.exports;
    expect(artifact.packedSize).toBe(artifact.packed.byteLength);
    expect(artifact.packedSha256).toBe(sha256(artifact.packed));
    expect(Buffer.from(artifact.decode()).equals(bytes)).toBe(true);
  });

  it("reports a damaged packed Rust payload as a non-retryable artifact mismatch", async () => {
    const bytes = Buffer.from("0061736d01000000", "hex"); // Empty WASM module.
    const packed = snappy.compress(bytes);
    const wasm = resolve(root, "test/fixtures/packed-rust-probe.wasm");
    const source = await buildProbeWorker(wasm, bytes);
    const damaged = Buffer.from(packed);
    damaged[0] ^= 1; // Alter the decoded length without changing the packed length.
    const original = packed.toString("base64");
    expect(source.includes(`"${original}"`)).toBe(true);
    const corrupted = source.replace(`"${original}"`, JSON.stringify(damaged.toString("base64")));
    const worker = new Worker(`
      const { parentPort } = require("node:worker_threads");
      globalThis.self = globalThis;
      globalThis.postMessage = (...args) => parentPort.postMessage(...args);
      globalThis.addEventListener = (type, listener) => parentPort.on(type, data => listener({ data }));
      ${corrupted}
    `, { eval: true });
    try {
      // Reject immediately if the Worker crashes at module load; do not use its watchdog.
      const reply = new Promise((resolve, reject) => {
        worker.once("error", reject);
        worker.on("message", (message) => { if (message.id === 1) resolve(message); });
        worker.postMessage({
          version: WORKER_PROTOCOL_VERSION,
          id: 1,
          operation: "initialize",
          vault_id: "synthetic-vault",
          source_policy_hash: "0".repeat(64),
          enabled_source_formats: ["markdown"],
        });
      });
      await expect(reply).resolves.toEqual({
        version: WORKER_PROTOCOL_VERSION,
        id: 1,
        operation: "initialize",
        ok: false,
        error: {
          code: "artifact_mismatch",
          stage: "artifact",
          message: "Embedded WASM artifact mismatch.",
          retryable: false,
        },
      });
    } finally {
      await worker.terminate();
    }
  });
});

// Bundle the real primary Worker with a synthetic Rust module. The regression
// must reject it before entering any Rust binding, without needing Cargo.
async function buildProbeWorker(wasm, bytes) {
  const sqlite = readFileSync(require.resolve("@sqlite.org/sqlite-wasm/sqlite3.wasm"));
  const identities = {
    RUST_WASM_SIZE: bytes.length,
    RUST_WASM_SHA256: sha256(bytes),
    SQLITE_WASM_SIZE: sqlite.length,
    SQLITE_WASM_SHA256: sha256(sqlite),
    PLUGIN_ID: "kwiry-search",
    PLUGIN_VERSION: "0.0.0",
  };
  const rejectBindingCall = [
    "abi_identity", "finalize_lexical_v2_rank", "finalize_query", "initSync",
    "prepare_oversized_source", "prepare_query", "prepare_source",
  ].map((name) => `export function ${name}() { throw new Error("Unexpected Rust binding call"); }`).join("\n");
  const modules = {
    "virtual:kwiry-artifact-identities": Object.entries(identities)
      .map(([key, value]) => `export const ${key}=${JSON.stringify(value)};`).join("\n"),
    "virtual:kwiry-rust-wasm-bindings": rejectBindingCall,
    "virtual:kwiry-internal-prototype": "export const createInternalPrototypeHandler=()=>async()=>false;",
    "virtual:kwiry-internal-d5c-preview": "export const createInternalD5cPreviewHandler=()=>async()=>false;",
    "virtual:kwiry-owner-worker-protocol": "export const isD5cOwnerWorkerOperation=()=>false; export const parseD5cOwnerWorkerRequest=()=>null;",
  };
  const built = await build({
    absWorkingDir: root,
    entryPoints: ["src/worker/worker.ts"],
    platform: "browser",
    format: "iife",
    target: "es2022",
    define: { __KWIRY_D5C_OWNER_WORKER__: "false" },
    loader: { ".wasm": "binary" },
    bundle: true,
    minify: true,
    write: false,
    plugins: [{
      name: "packed-artifact-fixture",
      setup(builder) {
        builder.onResolve({ filter: /^virtual:kwiry-rust-wasm-(?:artifact|bytes)$/ }, () => ({ path: wasm }));
        builder.onResolve({ filter: /^virtual:/ }, ({ path }) => ({ path, namespace: "probe-fixture" }));
        builder.onLoad({ filter: /.*/, namespace: "probe-fixture" }, ({ path }) => ({ contents: modules[path], loader: "js" }));
      },
    }, compressedRustWasmPlugin({ wasm, bytes })],
  });
  return built.outputFiles[0].text;
}
