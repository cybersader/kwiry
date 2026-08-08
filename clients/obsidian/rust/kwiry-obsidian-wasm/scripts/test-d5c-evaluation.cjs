// SPDX-License-Identifier: GPL-3.0-only
"use strict";

const fs = require("node:fs");
const path = require("node:path");
const { spawnSync } = require("node:child_process");

const root = path.resolve(__dirname, "..");
const repository = path.resolve(root, "../../../..");
const manifest = path.join(root, "Cargo.toml");
const coreManifest = path.join(repository, "daemon", "crates", "kwiry-core", "Cargo.toml");
const corpusPath = path.join(repository, "fixtures", "retrieval", "d5c-balanced", "corpus.json");
const target = path.join(root, "target");
const packageDirectory = path.join(root, "pkg", "internal-d5c-evaluation");
const generatedFixtures = path.join(target, "d5c-evaluation-fixtures.json");

function run(command, args) {
  const result = spawnSync(command, args, { cwd: root, encoding: "utf8" });
  if (result.status !== 0) {
    process.stderr.write(result.stdout);
    process.stderr.write(result.stderr);
    process.exit(result.status ?? 1);
  }
  return result.stdout.trim();
}

const corpus = JSON.parse(fs.readFileSync(corpusPath, "utf8"));
if (corpus.schema_version !== 1 || corpus.scenario_id !== "balanced-playground-v1") {
  throw new Error("unexpected D5C Balanced corpus identity");
}
const fixtures = corpus.evaluations.map((evaluation) => ({
  name: evaluation.id,
  operation: "internal_d5c_evaluate",
  request: evaluation.request,
}));
fs.mkdirSync(target, { recursive: true });
fs.writeFileSync(generatedFixtures, `${JSON.stringify(fixtures)}\n`);

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
const nativeRaw = run(nativeBinary, [generatedFixtures, "--raw-adapter-output"]);
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
const wasmRawOutputs = [];
const wasmOutput = fixtures.map((fixture) => {
  const raw = adapter.internal_d5c_evaluate(JSON.stringify(fixture.request));
  const output = JSON.parse(raw);
  wasmRawOutputs.push(JSON.stringify({ name: fixture.name, output }));
  return { name: fixture.name, output };
});
const wasmRaw = `[${wasmRawOutputs.join(",")}]`;
if (nativeRaw !== wasmRaw) {
  throw new Error("native and Node-loaded Balanced evaluation bytes differ");
}

const byName = Object.fromEntries(wasmOutput.map((fixture) => [fixture.name, fixture.output]));
for (const evaluation of corpus.evaluations) {
  const output = byName[evaluation.id];
  if (output.status !== "ok" || output.operation !== "internal_d5c_evaluate") {
    throw new Error(`Balanced operation failed for ${evaluation.id}`);
  }
  const kind = output.result.disposition.kind;
  if (kind !== evaluation.expected_disposition) {
    throw new Error(`Balanced disposition mismatch for ${evaluation.id}: ${kind}`);
  }
  if (kind === "fatal") {
    if (Object.hasOwn(output.result, "balanced_results") || Object.hasOwn(output.result, "explanation")) {
      throw new Error(`fatal evaluation produced a counterfactual for ${evaluation.id}`);
    }
  } else if (output.result.text_results.label !== "text") {
    throw new Error(`text result was relabeled for ${evaluation.id}`);
  }
  if (output.result.explanation) {
    const explanation = JSON.stringify(output.result.explanation);
    for (const source of corpus.sources) {
      const forbidden = [
        source.path,
        source.source.authorization_scope,
        source.source.source_key,
        source.provider.provider_id,
      ];
      for (const value of forbidden) {
        if (explanation.includes(value)) {
          throw new Error(`explanation leaked source-shaped input for ${evaluation.id}`);
        }
      }
    }
    for (const forbidden of ["approved", "draft", "balanced-properties-a", "1999999900", "2000000000"]) {
      if (explanation.includes(forbidden)) {
        throw new Error(`explanation leaked private rule data for ${evaluation.id}`);
      }
    }
  }
}
if (JSON.stringify(byName["stronger-text-counterexample-native"].result.balanced_results.ordered_candidate_ordinals) !== "[0,1]"
  || JSON.stringify(byName["same-tier-recency-native"].result.balanced_results.ordered_candidate_ordinals) !== "[1,0]"
  || JSON.stringify(byName["old-authority-note-native"].result.balanced_results.ordered_candidate_ordinals) !== "[1,0]"
  || JSON.stringify(byName["archive-hierarchy-lookalikes-native"].result.balanced_results.ordered_candidate_ordinals) !== "[3,0,2,1]"
  || JSON.stringify(byName["multi-chunk-source-fanout-native"].result.balanced_results.ordered_candidate_ordinals) !== "[1,2,0]"
  || JSON.stringify(byName["property-type-collision-native"].result.balanced_results.ordered_candidate_ordinals) !== "[2,0,1]"
  || JSON.stringify(byName["duplicate-provider-id-qualified-scopes-native"].result.balanced_results.ordered_candidate_ordinals) !== "[1,0]") {
  throw new Error("Balanced corpus ordering assertion failed");
}

const report = JSON.parse(run("cargo", [
  "run", "--quiet", "--manifest-path", coreManifest,
  "--example", "d5c_balanced_playground", "--features", "internal-d5c-preview", "--", corpusPath,
]));
if (report.source_count !== corpus.sources.length
  || report.evaluation_count !== corpus.evaluations.length
  || report.provider_counts.markdown < 1
  || report.provider_counts.google_docs < 1
  || report.provider_counts.canva < 1) {
  throw new Error("native playground source report is incomplete");
}
for (const metrics of report.engine_metrics) {
  if (metrics.judged_cases !== 9
    || metrics.balanced_mean_reciprocal_rank < metrics.text_mean_reciprocal_rank
    || metrics.balanced_mean_ndcg_at_5 < metrics.text_mean_ndcg_at_5) {
    throw new Error(`judged metrics regressed for ${metrics.engine}`);
  }
}

// A normal adapter build must not carry any private operation/profile/scenario marker.
run("cargo", [
  "build", "--manifest-path", manifest, "--target", "wasm32-unknown-unknown", "--release", "--lib",
]);
const normalWasm = fs.readFileSync(
  path.join(target, "wasm32-unknown-unknown", "release", "kwiry_obsidian_wasm.wasm"),
);
for (const marker of ["internal_d5c_evaluate", "balanced-playground-v1", "d5c-preview-v1"]) {
  if (normalWasm.includes(Buffer.from(marker))) {
    throw new Error(`normal WASM artifact contains private marker: ${marker}`);
  }
}

console.log(JSON.stringify({
  status: "pass",
  sources: corpus.sources.length,
  evaluations: corpus.evaluations.length,
  judged_cases_per_engine: 9,
  native_bytes: Buffer.byteLength(nativeRaw),
  wasm_bytes: Buffer.byteLength(wasmRaw),
  byte_identical: true,
  adapter_wasm_bytes: fs.statSync(path.join(packageDirectory, "kwiry_obsidian_wasm_bg.wasm")).size,
}));
