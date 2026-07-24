// SPDX-License-Identifier: GPL-3.0-only
"use strict";

const fs = require("node:fs");
const path = require("node:path");
const { spawnSync } = require("node:child_process");

const root = path.resolve(__dirname, "..");
const manifest = path.join(root, "Cargo.toml");
const fixturesPath = path.join(root, "fixtures", "cases.json");
const target = path.join(root, "target");
const packageDirectory = path.join(root, "pkg");
const nodePackageDirectory = path.join(packageDirectory, "node");
const webPackageDirectory = path.join(packageDirectory, "web");

function run(command, args) {
  const result = spawnSync(command, args, { cwd: root, encoding: "utf8" });
  if (result.status !== 0) {
    process.stderr.write(result.stdout);
    process.stderr.write(result.stderr);
    process.exit(result.status ?? 1);
  }
  return result.stdout.trim();
}

run("cargo", ["build", "--manifest-path", manifest, "--bin", "kwiry-obsidian-wasm-fixtures"]);
const nativeBinary = path.join(
  target,
  "debug",
  process.platform === "win32"
    ? "kwiry-obsidian-wasm-fixtures.exe"
    : "kwiry-obsidian-wasm-fixtures",
);
const nativeOutput = JSON.parse(run(nativeBinary, [fixturesPath]));

run("cargo", [
  "build",
  "--manifest-path",
  manifest,
  "--target",
  "wasm32-unknown-unknown",
  "--release",
  "--lib",
]);
const wasmBindgenVersion = run("wasm-bindgen", ["--version"]);
if (wasmBindgenVersion !== "wasm-bindgen 0.2.126") {
  throw new Error("unexpected wasm-bindgen CLI version");
}
const rawWasm = path.join(
  target,
  "wasm32-unknown-unknown",
  "release",
  "kwiry_obsidian_wasm.wasm",
);
fs.rmSync(packageDirectory, { recursive: true, force: true });
for (const [targetName, outputDirectory] of [
  ["web", webPackageDirectory],
  ["nodejs", nodePackageDirectory],
]) {
  run("wasm-bindgen", [
    rawWasm,
    "--target",
    targetName,
    "--out-dir",
    outputDirectory,
    "--out-name",
    "kwiry_obsidian_wasm",
  ]);
}

const adapter = require(path.join(nodePackageDirectory, "kwiry_obsidian_wasm.js"));
const cases = JSON.parse(fs.readFileSync(fixturesPath, "utf8"));
const wasmOutput = cases.map((fixture) => {
  let output;
  switch (fixture.operation) {
    case "identity":
      output = adapter.abi_identity();
      break;
    case "prepare_source": {
      const bytes = fixture.content.encoding === "utf8"
        ? Buffer.from(fixture.content.text, "utf8")
        : Uint8Array.from(fixture.content.values);
      output = adapter.prepare_source(JSON.stringify(fixture.request), bytes);
      break;
    }
    case "prepare_query":
      output = adapter.prepare_query(JSON.stringify(fixture.request));
      break;
    case "finalize_query":
      output = adapter.finalize_query(JSON.stringify(fixture.request));
      break;
    default:
      throw new Error("unknown fixture operation");
  }
  return { name: fixture.name, output: JSON.parse(output) };
});

function canonical(value) {
  if (Array.isArray(value)) return value.map(canonical);
  if (typeof value !== "object" || value === null) return value;
  return Object.fromEntries(
    Object.keys(value).sort().map((key) => [key, canonical(value[key])]),
  );
}

const nativeJson = JSON.stringify(canonical(nativeOutput));
const wasmJson = JSON.stringify(canonical(wasmOutput));
if (nativeJson !== wasmJson) {
  throw new Error("native and Node-loaded production adapter outputs differ");
}

const byName = Object.fromEntries(wasmOutput.map((fixture) => [fixture.name, fixture.output]));
if (byName["abi-identity"].abi_version !== 1
  || byName["ordinary-any-match"].result.match_plan.plan_id !== "lexical_any_v1"
  || byName["identifier-all-match"].result.match_plan.plan_id !== "lexical_all_v1"
  || byName["metadata-promoted-match"].result.plan.kind !== "identifier"
  || byName["allowlisted-explicit-match"].result.match_plan.plan_id !== "lexical_explicit_v1"
  || byName["inert-sql-looking-query"].error.code !== "explicit_query_unsupported"
  || JSON.stringify(byName["inert-sql-looking-query"]).includes("DROP TABLE")
  || byName["invalid-source-envelope"].error.code !== "invalid_request"
  || byName["invalid-query-envelope"].error.code !== "invalid_request"
  || byName["wrong-abi"].error.code !== "abi_mismatch") {
  throw new Error("production adapter fixture assertion failed");
}

const nodeWasmPath = path.join(nodePackageDirectory, "kwiry_obsidian_wasm_bg.wasm");
const webWasmPath = path.join(webPackageDirectory, "kwiry_obsidian_wasm_bg.wasm");
const wasmBytes = fs.statSync(webWasmPath).size;
if (wasmBytes !== fs.statSync(nodeWasmPath).size
  || !fs.readFileSync(webWasmPath).equals(fs.readFileSync(nodeWasmPath))
  || !fs.existsSync(path.join(webPackageDirectory, "kwiry_obsidian_wasm.js"))) {
  throw new Error("browser and Node binding artifacts disagree");
}
console.log(JSON.stringify({
  status: "pass",
  cases: cases.length,
  native_bytes: Buffer.byteLength(nativeJson),
  wasm_bytes: Buffer.byteLength(wasmJson),
  byte_identical: true,
  adapter_wasm_bytes: wasmBytes,
}));
