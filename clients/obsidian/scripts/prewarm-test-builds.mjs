// SPDX-FileCopyrightText: 2026 cybersader
// SPDX-License-Identifier: GPL-3.0-only
//
// Runs the exact three builds the worker-runtime suite performs in its setup,
// in a plain process where no test-runner deadline exists. The suite builds
// through a synchronous subprocess that blocks its vitest worker's event loop;
// when any one build runs cache-cold for longer than vitest's fixed 60 s
// worker-RPC deadline, the run fails with an unhandled "Timeout calling
// onTaskUpdate" even though every test passed. Warming only the production
// build is not enough — the prototype variant compiles a different feature
// graph and stays cold. After this script, the in-suite builds are incremental.
import { buildPlugin } from "../esbuild.config.mjs";

const variants = [
  { write: false, production: true },
  { write: false, production: false },
  { write: false, production: true, internalTypoPrototype: true },
];

for (const options of variants) {
  const started = Date.now();
  await buildPlugin(options);
  const seconds = ((Date.now() - started) / 1000).toFixed(1);
  process.stdout.write(`prewarmed ${JSON.stringify(options)} in ${seconds}s\n`);
}
