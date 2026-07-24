// SPDX-License-Identifier: MIT OR Apache-2.0

"use strict";

const path = require("node:path");

const probeName = process.argv[2];
if (probeName !== "single-segment" && probeName !== "index-writer") {
  console.error(JSON.stringify({
    status: "error",
    stage: "node:arguments",
    message: "expected single-segment or index-writer",
  }));
  process.exit(2);
}

const bindings = require(path.join(
  __dirname,
  "..",
  "pkg",
  "kwiry_tantivy_wasm_probe.js",
));

const probe = probeName === "single-segment"
  ? bindings.probe_single_segment
  : bindings.probe_index_writer;

try {
  const result = probe();
  process.stdout.write(`${result}\n`);
} catch (error) {
  console.error(JSON.stringify({
    status: "error",
    stage: `node:${probeName}`,
    message: String(error),
  }));
  process.exit(1);
}
