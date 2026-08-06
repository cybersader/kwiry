// SPDX-License-Identifier: MIT OR Apache-2.0

"use strict";

const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");
const { spawnSync } = require("node:child_process");

const root = path.resolve(__dirname, "..");
const manifest = path.join(root, "Cargo.toml");
const fixture = path.join(root, "fixtures", "cases.json");
const pkg = path.join(root, "pkg");
const wasm = path.join(
  root,
  "target",
  "wasm32-unknown-unknown",
  "release",
  "kwiry_portable_core_wasm_probe.wasm",
);

function run(command, args) {
  const result = spawnSync(command, args, {
    cwd: root,
    encoding: "utf8",
    stdio: ["ignore", "pipe", "pipe"],
  });
  if (result.status !== 0) {
    process.stderr.write(result.stdout);
    process.stderr.write(result.stderr);
    throw new Error(`${command} exited with status ${result.status}`);
  }
  return result.stdout.trimEnd();
}

run("cargo", ["test", "--manifest-path", manifest, "--locked"]);
run("cargo", [
  "build",
  "--manifest-path",
  manifest,
  "--target",
  "wasm32-unknown-unknown",
  "--release",
  "--locked",
]);
fs.rmSync(pkg, { recursive: true, force: true });
run("wasm-bindgen", [
  "--target",
  "nodejs",
  "--out-dir",
  pkg,
  "--out-name",
  "kwiry_portable_core_wasm_probe",
  wasm,
]);

const nativeOutput = run("cargo", [
  "run",
  "--quiet",
  "--manifest-path",
  manifest,
  "--locked",
  "--bin",
  "kwiry-portable-core-native",
  "--",
  fixture,
]);
const fixtureInput = fs.readFileSync(fixture, "utf8");
const bindings = require(path.join(pkg, "kwiry_portable_core_wasm_probe.js"));
const wasmOutput = bindings.run_cases(fixtureInput);

assert.equal(
  Buffer.compare(Buffer.from(nativeOutput), Buffer.from(wasmOutput)),
  0,
  "native and Node-loaded WASM JSON must be byte-for-byte identical",
);

const cases = new Map(JSON.parse(wasmOutput).map((entry) => [entry.name, entry]));
const get = (name) => {
  const value = cases.get(name);
  assert.ok(value, `missing fixture output: ${name}`);
  return value;
};

assert.equal(get("markdown-frontmatter-links-identifiers").preparation.schema_version, 9);
assert.equal(get("markdown-frontmatter-links-identifiers").preparation.retrieval.aliases.length, 2);
assert.ok(
  get("markdown-frontmatter-links-identifiers").preparation.chunks.some((chunk) =>
    chunk.technical_identifiers.includes("rfc 9110")
  ),
);
assert.deepEqual(get("crlf-frontmatter").preparation.frontmatter.title, {
  type: "string",
  value: "Windows note",
});
assert.ok(get("malformed-frontmatter").preparation.warning);
assert.equal(get("plain-text").preparation.format, "text");
assert.equal(get("nul-source").preparation.kind, "skipped");
assert.ok(get("nul-source").preparation.content_hash);
assert.equal(get("invalid-utf8").preparation.kind, "skipped");
assert.equal(get("empty-source").preparation.chunks.length, 0);
assert.ok(get("heading-overlap").preparation.chunks.length > 1);
assert.notEqual(
  get("rename-before").preparation.chunks[0].chunk.chunk_id,
  get("rename-after").preparation.chunks[0].chunk.chunk_id,
);
assert.equal(get("invalid-relative-path").status, "error");
assert.equal(get("oversized-source").preparation.kind, "skipped");
assert.equal(get("underreported-source-length").status, "error");
assert.equal(get("overreported-source-length").status, "error");
assert.equal(get("ordinary-query").plan.schema_version, 4);
assert.equal(get("metadata-probe-unmatched").plan.kind, "ordinary");
assert.equal(get("metadata-probe-unmatched").plan.match_operator, "any");
assert.equal(get("metadata-probe-matched").plan.kind, "identifier");
assert.equal(get("metadata-probe-matched").plan.match_operator, "all");
assert.deepEqual(get("identifier-query").plan.terms, ["iia", "2", "line"]);
assert.equal(get("identifier-query").plan.kind, "identifier");
assert.equal(get("identifier-query").plan.match_operator, "all");
assert.equal(get("explicit-query").plan.kind, "explicit");
assert.equal(get("explicit-query").plan.match_operator, "explicit");
assert.deepEqual(get("explicit-query").plan.terms, []);
assert.match(get("sql-looking-query").plan.query, /DROP TABLE/);
assert.equal(get("empty-query").status, "error");
assert.equal(get("portable-api-request").request.mode, "lexical");
assert.equal(get("portable-api-request").request.filters.vault_id, "fixture");
const zeroCoverageCounts = {
  "indexed-complete": 0,
  "indexed-partial": 0,
  "skipped-no-extractable-text": 0,
  unreadable: 0,
  quarantined: 0,
};
assert.deepEqual(get("portable-daemon-status").daemon_status.source_format_counts, {
  markdown: { ...zeroCoverageCounts, "indexed-complete": 2 },
  text: { ...zeroCoverageCounts, "indexed-complete": 1 },
  base: zeroCoverageCounts,
  canvas: zeroCoverageCounts,
  docx: zeroCoverageCounts,
  pdf: zeroCoverageCounts,
  excalidraw: zeroCoverageCounts,
});
assert.equal(get("portable-daemon-status").daemon_status.state, "ready");
assert.equal(get("portable-daemon-status").daemon_status.generation, "generation-0001");
assert.equal(
  get("portable-daemon-status").daemon_status.chunking_version,
  get("markdown-frontmatter-links-identifiers").preparation.chunks[0].chunk.chunking_version,
);
assert.equal(
  typeof get("markdown-frontmatter-links-identifiers").preparation.mtime_nanos,
  "string",
);

process.stdout.write(JSON.stringify({
  status: "pass",
  cases: cases.size,
  native_bytes: Buffer.byteLength(nativeOutput),
  wasm_bytes: Buffer.byteLength(wasmOutput),
  byte_identical: true,
}) + "\n");
