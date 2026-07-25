// SPDX-FileCopyrightText: 2026 cybersader
// SPDX-License-Identifier: GPL-3.0-only

import { mkdtemp, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { resolve } from "node:path";

import { generatePerformanceCorpus } from "./gate5-corpus.mjs";

main().catch(() => {
  process.stderr.write("Gate 5 corpus smoke failed.\n");
  process.exitCode = 1;
});

async function main() {
  const roots = await Promise.all([
    mkdtemp(resolve(tmpdir(), "kwiry-gate5-smoke-a-")),
    mkdtemp(resolve(tmpdir(), "kwiry-gate5-smoke-b-")),
  ]);
  try {
    const options = { noteCount: 256, totalBytes: 2 * 1024 * 1024 };
    const first = await generatePerformanceCorpus(roots[0], options);
    const second = await generatePerformanceCorpus(roots[1], options);
    if (JSON.stringify(first) !== JSON.stringify(second)) {
      throw new Error("generated corpus is not deterministic");
    }
    process.stdout.write(`${JSON.stringify({
      schema_version: 1,
      status: "passed",
      note_count: first.note_count,
      markdown_bytes: first.markdown_bytes,
      sha256: first.sha256,
    })}\n`);
  } finally {
    await Promise.all(roots.map((root) => rm(root, { recursive: true, force: true })));
  }
}
