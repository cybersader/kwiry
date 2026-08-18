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
if (byName["abi-identity"].abi_version !== 3
  || byName["abi-identity"].source_preparation_schema_version !== 9
  // The shipped plugin compiles the portable PDF tier and never the enhanced
  // one. This is the artifact the user installs, so it is the right place to
  // assert it: a build that had picked up `native-pdf-extractor` would report
  // `enhanced` here, and one that had lost the reader would report `none`.
  || byName["abi-identity"].extraction_policy.pdf !== "portable"
  || byName["abi-identity"].format_identity_schema_version !== 1
  || byName["abi-identity"].lexical_query_plan_schema_version !== 6
  || byName["abi-identity"].fts5_match_plan_schema_version !== 5
  // Link behaviour is declared by the backend registry, so the shipped
  // artifact must carry it: a client that decides for itself silently refuses
  // every format admitted later.
  || byName["abi-identity"].section_link_formats.markdown !== true
  || byName["abi-identity"].section_link_formats.pdf !== false
  || byName["abi-identity"].section_link_formats.excel !== false
  || byName["ordinary-any-match"].result.execution_plan.stages[0].plan_id
    !== "lexical_exact_metadata_v3"
  || byName["ordinary-any-match"].result.execution_plan.stages[0].match_value !== undefined
  || byName["ordinary-any-match"].result.execution_plan.stages[0].exact_value
    !== "dungeons and dragons"
  || byName["identifier-all-match"].result.plan.kind !== "identifier"
  || byName["metadata-promoted-match"].result.plan.kind !== "identifier"
  || byName["allowlisted-explicit-match"].result.execution_plan.stages[0].plan_id
    !== "lexical_explicit_v3"
  || byName["no-evidence-empty"].result.execution_plan.disposition !== "empty_no_evidence"
  || byName["bounded-prefix"].result.execution_plan.stages.at(-1).plan_id !== "lexical_prefix_v3"
  || byName["bounded-prefix"].result.execution_plan.stages.at(-2).plan_id
    !== "lexical_prefix_metadata_v3"
  || JSON.stringify(byName["prefix-before-partial"].result.execution_plan.stages
    .map((stage) => stage.plan_id)) !== JSON.stringify([
    "lexical_exact_metadata_v3",
    "lexical_exact_phrase_v3",
    "lexical_all_terms_v3",
    "lexical_prefix_metadata_v3",
    "lexical_prefix_v3",
    "lexical_partial_coverage_v3",
  ])
  // The metadata half is scoped to the fields a note is named by; the text
  // half carries the identical expansion set over everything.
  || byName["prefix-before-partial"].result.execution_plan.stages[3].match_value
    !== "{filename stem aliases title} : (\"orchard\" AND (\"adoption\"))"
  || byName["prefix-before-partial"].result.execution_plan.stages[4].match_value
    !== "{filename stem aliases title heading_text tags content} : (\"orchard\" AND (\"adoption\"))"
  || byName["numeric-field-explicit"].result.plan.kind !== "explicit"
  || byName["natural-question"].result.plan.kind !== "ordinary"
  || byName["natural-parenthetical"].result.plan.kind !== "ordinary"
  || byName["decomposed-accent-query"].result.plan.normalized_exact !== "resume cache"
  || byName["complete-long-exact"].result.plan.normalized_exact.length !== 4096
  || byName["accented-prefix"].result.execution_plan.stages[0].match_value !== "\"resu\"*"
  || byName["rfc-exact-identifier"].result.plan.terms[0] !== "rfc 9110"
  || byName["rfc-exact-identifier"].result.execution_plan.stages[0].required_identifiers[0]
    !== "rfc 9110"
  || byName["inert-sql-looking-query"].error.code !== "explicit_query_unsupported"
  || JSON.stringify(byName["inert-sql-looking-query"]).includes("DROP TABLE")
  || byName["invalid-source-envelope"].error.code !== "invalid_request"
  || byName["invalid-query-envelope"].error.code !== "invalid_request"
  || byName["wrong-abi"].error.code !== "abi_mismatch") {
  throw new Error("production adapter fixture assertion failed");
}

// The per-format identities the cache keys its rows on, asserted on the
// artifact the user installs. `source-formats.ts` mirrors these, and a drifted
// mirror would restore rows this adapter could not have produced.
//
// Every format, not a sample. This block previously pinned `pdf` and `markdown`
// only, so a drifted `base`, `canvas`, `docx`, `text`, `excalidraw`, or `excel`
// identity — including one produced by a half-applied extractor version bump — passed
// the check on the shipped artifact.
const PINNED_FORMAT_IDENTITIES = {
  markdown: "b678d0ea2d77d7a79ccc79f4f8a3a1d96aed9bb98757afb1381e5661a1fb96f7",
  text: "c89bb1c6cb87c1e6371d7d03956f1c6bf8bff605c847441c2c72d7599bbd464b",
  base: "d3eeb5a8e3246a07f0c1e41782a7f61628921f43f7afdd722f3a060104e7e079",
  canvas: "01eae3d6859de3287237e366b7fcd9f346dbab395453ef9422bcd67dc527858c",
  docx: "b4f9cff615a917e09d800c2784e17c836ef79cc767c49091818a7b1f8598a38e",
  pdf: "980924c70d64fc5de65ddc2141d043e9188f8856ec6196d30c0d5c11d363c3bc",
  excalidraw: "e1f6868bd320172f6b8d9afc3ac716e309499b065c62fa1b17ae4c2c09d98348",
  excel: "ddfee1499472f960540644e47069db3942a572e883d2328e2b5df856dbd04889",
};
const reportedIdentities = byName["abi-identity"].format_identities;
if (JSON.stringify(Object.keys(reportedIdentities).sort())
  !== JSON.stringify(Object.keys(PINNED_FORMAT_IDENTITIES).sort())) {
  throw new Error("the adapter reports a different format set than is pinned here");
}
for (const [format, expected] of Object.entries(PINNED_FORMAT_IDENTITIES)) {
  if (reportedIdentities[format] !== expected) {
    throw new Error(`production adapter ${format} identity is not the pinned one`);
  }
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
