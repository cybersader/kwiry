// SPDX-License-Identifier: GPL-3.0-only
"use strict";

const fs = require("node:fs");
const path = require("node:path");
const { spawnSync } = require("node:child_process");

const root = path.resolve(__dirname, "..");
const manifest = path.join(root, "Cargo.toml");
const fixturesPath = path.join(root, "fixtures", "d5c-cases.json");
const target = path.join(root, "target");
const packageDirectory = path.join(root, "pkg", "internal-d5c-parity");

function run(command, args) {
  const result = spawnSync(command, args, { cwd: root, encoding: "utf8" });
  if (result.status !== 0) {
    process.stderr.write(result.stdout);
    process.stderr.write(result.stderr);
    process.exit(result.status ?? 1);
  }
  return result.stdout.trim();
}

const featureArgs = ["--features", "internal-d5c-preview"];
run("cargo", [
  "build", "--manifest-path", manifest, "--bin", "kwiry-obsidian-wasm-fixtures",
  ...featureArgs,
]);
const nativeBinary = path.join(
  target,
  "debug",
  process.platform === "win32"
    ? "kwiry-obsidian-wasm-fixtures.exe"
    : "kwiry-obsidian-wasm-fixtures",
);
const nativeRaw = run(nativeBinary, [fixturesPath, "--raw-adapter-output"]);
const nativeOutput = JSON.parse(nativeRaw);

run("cargo", [
  "build", "--manifest-path", manifest, "--target", "wasm32-unknown-unknown",
  "--release", "--lib", ...featureArgs,
]);
if (run("wasm-bindgen", ["--version"]) !== "wasm-bindgen 0.2.126") {
  throw new Error("unexpected wasm-bindgen CLI version");
}
fs.rmSync(packageDirectory, { recursive: true, force: true });
run("wasm-bindgen", [
  path.join(target, "wasm32-unknown-unknown", "release", "kwiry_obsidian_wasm.wasm"),
  "--target", "nodejs", "--out-dir", packageDirectory, "--out-name", "kwiry_obsidian_wasm",
]);
const adapter = require(path.join(packageDirectory, "kwiry_obsidian_wasm.js"));
const cases = JSON.parse(fs.readFileSync(fixturesPath, "utf8"));
const wasmRawOutputs = [];
const wasmOutput = cases.map((fixture) => {
  const operation = fixture.operation;
  const raw = operation === "prepare_d5c_preview"
    ? adapter.prepare_d5c_preview(JSON.stringify(fixture.request))
    : operation === "finalize_d5c_preview"
      ? adapter.finalize_d5c_preview(JSON.stringify(fixture.request))
      : (() => { throw new Error(`unsupported D5C fixture operation: ${operation}`); })();
  const output = JSON.parse(raw);
  wasmRawOutputs.push(JSON.stringify({ name: fixture.name, output }));
  return { name: fixture.name, output };
});

const wasmRaw = `[${wasmRawOutputs.join(",")}]`;
if (nativeRaw !== wasmRaw) {
  throw new Error("native and Node-loaded D5C adapter outputs differ");
}
const byName = Object.fromEntries(wasmOutput.map((fixture) => [fixture.name, fixture.output]));
const order = (name) => byName[name].result.ordered_candidate_ordinals;
if (byName["prepare-full-profile"].result.signal_plan.max_candidates !== 512
  || byName["prepare-full-profile"].result.signal_plan.max_candidates_per_stage !== 256
  || JSON.stringify(order("tier-dominance")) !== JSON.stringify([0, 1])
  || JSON.stringify(order("same-tier-recency")) !== JSON.stringify([1, 0])
  || JSON.stringify(order("source-fanout")) !== JSON.stringify([1, 2, 0])
  || JSON.stringify(order("mixed-full-range-types")) !== JSON.stringify([2, 0, 1])
  || JSON.stringify(order("component-hierarchy")) !== JSON.stringify([2, 1, 3, 0])
  || JSON.stringify(order("stable-ties")) !== JSON.stringify([0, 1])
  || JSON.stringify(order("qualified-authorization")) !== JSON.stringify([1, 0])
  || byName["malformed-profile"].error.code !== "invalid_relevance_profile"
  || byName["incomplete-observations"].error.code !== "incomplete_rerank_input") {
  throw new Error("D5C conformance fixture assertion failed");
}
for (const [name, envelope] of Object.entries(byName)) {
  const evidence = envelope.result?.evidence;
  if (!evidence) continue;
  const serialized = JSON.stringify(evidence);
  for (const forbidden of ["priority", ".md", "source_key", "chunk", "path", "query"]) {
    if (serialized.includes(forbidden)) {
      throw new Error(`D5C evidence privacy failed for ${name}`);
    }
  }
}
console.log(JSON.stringify({
  status: "pass",
  cases: cases.length,
  native_bytes: Buffer.byteLength(nativeRaw),
  wasm_bytes: Buffer.byteLength(wasmRaw),
  byte_identical: true,
  adapter_wasm_bytes: fs.statSync(path.join(packageDirectory, "kwiry_obsidian_wasm_bg.wasm")).size,
}));
