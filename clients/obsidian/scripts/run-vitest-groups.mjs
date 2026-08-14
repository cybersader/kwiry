// SPDX-FileCopyrightText: 2026 cybersader
// SPDX-License-Identifier: GPL-3.0-only

import { spawnSync } from "node:child_process";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const VITEST = resolve(ROOT, "node_modules/vitest/vitest.mjs");

const LEXICAL_WASM_FILE = "test/lexical-conformance.test.ts";
const WASM_BUILD_FILES = [
  "test/d5c-preview.test.mjs",
  "test/d5c-live-worker.test.mjs",
  "test/d5c-worker-isolation.test.mjs",
  "test/worker-quarantine.test.mjs",
  "test/worker-runtime.test.mjs",
  "test/d5c-brat-package.test.mjs",
];
const WASM_HEAVY_FILES = [LEXICAL_WASM_FILE, ...WASM_BUILD_FILES];

const forwardedArgs = process.argv.slice(2);
if (forwardedArgs.length > 0) {
  runGroup("focused tests (one worker)", ["run", "--maxWorkers=1", ...forwardedArgs]);
  process.exit(0);
}

const groups = [
  {
    name: "ordinary tests",
    args: [
      "run",
      ...WASM_HEAVY_FILES.flatMap((file) => ["--exclude", file]),
    ],
  },
  {
    name: "lexical WASM test (one worker)",
    args: ["run", LEXICAL_WASM_FILE, "--maxWorkers=1"],
  },
  {
    name: "WASM-build tests (two workers)",
    args: ["run", ...WASM_BUILD_FILES, "--maxWorkers=2"],
  },
];

for (const group of groups) runGroup(group.name, group.args);

function runGroup(name, args) {
  console.log(`\n=== ${name} ===`);
  const result = spawnSync(
    process.execPath,
    [VITEST, ...args],
    {
      cwd: ROOT,
      env: process.env,
      stdio: "inherit",
    },
  );
  if (result.error) throw result.error;
  if (result.status !== 0) process.exit(result.status ?? 1);
}
