// SPDX-FileCopyrightText: 2026 cybersader
// SPDX-License-Identifier: GPL-3.0-only

import { createHash } from "node:crypto";
import { existsSync, readFileSync, readdirSync } from "node:fs";
import { mkdtemp, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

import { buildPlugin } from "../esbuild.config.mjs";
import { generateFunctionalCorpus } from "./gate5-corpus.mjs";
import { GATE5_TARGETS, validateGate5AutomatedEvidence } from "./gate5-evidence-schema.mjs";

main().catch(() => {
  process.stderr.write("Gate 5 evidence generation failed.\n");
  process.exitCode = 1;
});

async function main() {
  const root = resolve(dirname(fileURLToPath(import.meta.url)), "..");
  const first = await buildPlugin({ write: false, production: true });
  const second = await buildPlugin({ write: false, production: true });
  const corpusRoot = await mkdtemp(resolve(tmpdir(), "kwiry-gate5-evidence-"));
  let corpus;
  try {
    corpus = await generateFunctionalCorpus(corpusRoot);
  } finally {
    await rm(corpusRoot, { recursive: true, force: true });
  }

  const manifest = JSON.parse(readFileSync(resolve(root, "manifest.json"), "utf8"));
  const packageJson = JSON.parse(readFileSync(resolve(root, "package.json"), "utf8"));
  const workerInputs = Object.keys(first.workerMetafile.inputs).map((input) =>
    input.replaceAll("\\", "/")
  );
  const wasmInputs = workerInputs.filter((input) => input.endsWith(".wasm"));
  const looseRuntimeAssets = readdirSync(root).filter((name) =>
    /(?:\.wasm|worker.*\.js|\.map|\.db|\.sqlite)$/iu.test(name)
  );
  const privatePattern = /(?:\/home\/(?!web_user(?:\/|"))[A-Za-z0-9._-]+\/|\/mnt\/[a-z]\/[Uu]sers\/|[A-Z]:\\Users\\)/u;
  const checks = [
    first.mainText === second.mainText,
    first.workerSource === second.workerSource,
    wasmInputs.length === 2,
    wasmInputs.some((input) => input.endsWith("sqlite3.wasm")),
    wasmInputs.some((input) => input.endsWith("kwiry_obsidian_wasm_bg.wasm")),
    looseRuntimeAssets.length === 0,
    first.mainText.slice(0, 2_000).includes("GNU General Public License"),
    manifest.version === packageJson.version,
    !first.mainText.includes("sourceMappingURL="),
    !/\bimport\s*\(|\bimportScripts\s*\(/u.test(first.workerSource),
    !privatePattern.test(first.mainText),
    existsSync(resolve(root, "THIRD_PARTY_NOTICES.md")),
    first.mainText.includes("Daemon"),
    first.mainText.includes("In-plugin"),
    corpus.note_count === 14,
    corpus.expected_documents === corpus.note_count,
  ];
  if (checks.some((passed) => !passed)) throw new Error("automated evidence check failed");

  const evidence = {
    schema_version: 1,
    kind: "kwiry_gate5_automated_evidence",
    verdict: "AUTOMATED_CHECKS_PASSED_OWNER_REVIEW_REQUIRED",
    automation_scope: "artifact_and_generated_functional_corpus",
    protocol_version: 2,
    artifact: {
      main: identity(first.mainText),
      worker: identity(first.workerSource),
      rust_wasm: first.identities.rust,
      sqlite_wasm: first.identities.sqlite,
      deterministic: true,
      wasm_inputs: 2,
      loose_runtime_assets: 0,
    },
    corpus: {
      kind: corpus.kind,
      note_count: corpus.note_count,
      markdown_bytes: corpus.markdown_bytes,
      sha256: corpus.sha256,
      hash_algorithm: corpus.hash_algorithm,
      expected_documents: corpus.expected_documents,
    },
    checks: { total: checks.length, failed: 0 },
    targets: GATE5_TARGETS.map(([id, threshold, unit]) => ({
      id,
      threshold,
      unit,
      status: "not_measured",
      value: null,
      scope: "declared_reference_hardware",
    })),
    privacy: {
      aggregate_only: true,
      paths_emitted: 0,
      vault_names_emitted: 0,
      note_content_emitted: 0,
      raw_queries_emitted: 0,
      tokens_emitted: 0,
      stack_traces_emitted: 0,
      sql_emitted: 0,
      environment_paths_emitted: 0,
      private_corpus_hashes_emitted: 0,
      loose_evidence_artifacts: 0,
    },
  };
  validateGate5AutomatedEvidence(evidence);
  process.stdout.write(`${JSON.stringify(evidence)}\n`);
}

function identity(value) {
  return {
    bytes: Buffer.byteLength(value),
    sha256: createHash("sha256").update(value).digest("hex"),
  };
}
