// SPDX-FileCopyrightText: 2026 cybersader
// SPDX-License-Identifier: GPL-3.0-only

import { createHash } from "node:crypto";
import {
  copyFile,
  mkdir,
  mkdtemp,
  readFile,
  readdir,
  rm,
  writeFile,
} from "node:fs/promises";
import { tmpdir } from "node:os";
import { dirname, relative, resolve } from "node:path";
import { afterEach, describe, expect, it } from "vitest";

import { GATE5_TARGETS } from "../scripts/gate5-evidence-schema.mjs";
import { embedWorkerPrivacyBoundary } from "../scripts/privacy-policy.mjs";
import {
  prepareProductionPackage,
  validateProductionIdentity,
  validateProductionPackage,
} from "../scripts/production-package.mjs";

const repositoryRoot = resolve(import.meta.dirname, "..");
const HASH = "a".repeat(64);
const MAIN_JS = [
  "/* GNU General Public License */",
  embedWorkerPrivacyBoundary("export {};\n"),
  "module.exports = {};",
  "",
].join("\n");
const VERSION = "0.6.0-beta.2";
const temporaryRoots = [];

async function createSourceFixture() {
  const fixtureRoot = await mkdtemp(resolve(tmpdir(), "kwiry-production-package-fixture-"));
  const sourceRoot = resolve(fixtureRoot, "source");
  temporaryRoots.push(fixtureRoot);
  await Promise.all([
    mkdir(resolve(sourceRoot, "licenses"), { recursive: true }),
    mkdir(resolve(sourceRoot, "src/worker"), { recursive: true }),
    mkdir(resolve(sourceRoot, "test"), { recursive: true }),
  ]);
  await Promise.all([
    writeFile(resolve(sourceRoot, "main.js"), MAIN_JS),
    writeFile(resolve(sourceRoot, "styles.css"), ".kwiry-search { display: block; }\n"),
    writeFile(resolve(sourceRoot, "esbuild.config.mjs"), "export {};\n"),
    writeFile(resolve(sourceRoot, "src/main.ts"), "export {};\n"),
    writeFile(resolve(sourceRoot, "src/worker/worker.ts"), "export {};\n"),
    writeFile(resolve(sourceRoot, "test/source.test.ts"), "export {};\n"),
    writeJson(resolve(sourceRoot, "manifest.json"), {
      id: "kwiry-search",
      version: VERSION,
    }),
    writeJson(resolve(sourceRoot, "package.json"), {
      name: "kwiry-search",
      version: VERSION,
    }),
    writeJson(resolve(sourceRoot, "package-lock.json"), {
      name: "kwiry-search",
      version: VERSION,
      lockfileVersion: 3,
      packages: {
        "": { name: "kwiry-search", version: VERSION },
      },
    }),
    writeJson(resolve(sourceRoot, "gate5.evidence.json"), validEvidence()),
    copyFile(resolve(repositoryRoot, "LICENSE"), resolve(sourceRoot, "LICENSE")),
    copyFile(
      resolve(repositoryRoot, "THIRD_PARTY_NOTICES.md"),
      resolve(sourceRoot, "THIRD_PARTY_NOTICES.md"),
    ),
    ...[
      "Apache-2.0.txt",
      "Emscripten-LICENSE.txt",
      "Rust-DEPENDENCY-LICENSES.md",
    ].map((name) => copyFile(
      resolve(repositoryRoot, "licenses", name),
      resolve(sourceRoot, "licenses", name),
    )),
  ]);
  return sourceRoot;
}

function validEvidence(main = MAIN_JS) {
  return {
    schema_version: 1,
    kind: "kwiry_gate5_automated_evidence",
    verdict: "AUTOMATED_CHECKS_PASSED_OWNER_REVIEW_REQUIRED",
    automation_scope: "artifact_and_generated_functional_corpus",
    protocol_version: 2,
    artifact: {
      main: { bytes: Buffer.byteLength(main), sha256: sha256(main) },
      worker: { bytes: 1, sha256: HASH },
      rust_wasm: { bytes: 1, sha256: HASH },
      sqlite_wasm: { bytes: 1, sha256: HASH },
      deterministic: true,
      wasm_inputs: 2,
      loose_runtime_assets: 0,
    },
    corpus: {
      kind: "generated_functional",
      note_count: 14,
      markdown_bytes: 1_024,
      sha256: HASH,
      hash_algorithm: "sha256-path-nul-decimal-length-nul-bytes-nul-v1",
      expected_documents: 14,
    },
    checks: { total: 1, failed: 0 },
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
}

async function writeJson(path, value) {
  await writeFile(path, `${JSON.stringify(value, null, 2)}\n`);
}

function validPerformanceEvidence() {
  const measurements = {
    worker_initialize_ms: 100,
    first_batch_ms: 50,
    build_duration_ms: 20_000,
    warm_search_p95_ms: 10,
    hydration_p95_ms: 15,
    update_visibility_p95_ms: 20,
    max_event_loop_delay_ms: 5,
    added_rss_mib: 250,
  };
  const measurementKeys = new Map([
    ["build_duration", "build_duration_ms"],
    ["warm_search_p95", "warm_search_p95_ms"],
    ["hydration_p95", "hydration_p95_ms"],
    ["update_visibility_p95", "update_visibility_p95_ms"],
    ["max_event_loop_delay", "max_event_loop_delay_ms"],
    ["added_steady_state_memory", "added_rss_mib"],
  ]);
  return {
    schema_version: 2,
    kind: "kwiry_gate5_generated_performance",
    verdict: "EVIDENCE_CAPTURE_COMPLETE_OWNER_DECISION_REQUIRED",
    host: "node_worker_threads",
    provenance: {
      runtime: { node_version: "v22.21.1", platform: "linux", architecture: "x64" },
      measurement_runs: 1,
      baseline_runs: 0,
      regression_assessed: false,
    },
    artifact: {
      worker: { bytes: 1, sha256: HASH },
      rust_wasm: { bytes: 1, sha256: HASH },
      sqlite_wasm: { bytes: 1, sha256: HASH },
    },
    corpus: {
      kind: "generated_performance",
      note_count: 10_000,
      markdown_bytes: 50 * 1024 * 1024,
      sha256: HASH,
      hash_algorithm: "sha256-path-nul-decimal-length-nul-bytes-nul-v1",
      expected_documents: 10_000,
      seed_u32: 0x4b574952,
    },
    index: { documents: 10_000, chunks: 50_000, sources: 10_000 },
    storage: {
      page_size: 4_096,
      page_count: 100,
      freelist_count: 0,
      max_page_count: 81_920,
      peak_database_bytes: 500_000,
      final_database_bytes: 409_600,
      database_byte_limit: 320 * 1024 * 1024,
      export_blob_bytes: 409_600,
      export_blob_limit: 384 * 1024 * 1024,
      main_chunks_bytes: 80_000,
      main_fts_bytes: 100_000,
      exact_identifier_fts_bytes: 60_000,
      properties_bytes: 60_000,
      sources_bytes: 60_000,
      other_indexes_bytes: 49_600,
    },
    measurements,
    samples: { warm_search: 40, hydration: 20, update_visibility: 20 },
    targets: GATE5_TARGETS.map(([id, threshold, unit]) => {
      const measurementKey = measurementKeys.get(id);
      const value = measurementKey ? measurements[measurementKey] : null;
      return {
        id,
        threshold,
        unit,
        status: measurementKey ? (value <= threshold ? "met" : "missed") : "unavailable",
        value,
        scope: measurementKey
          ? "generated_node_worker_threads"
          : "installed_obsidian_reference_hardware",
      };
    }),
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
}

async function snapshotFiles(root, current = root, snapshot = {}) {
  const entries = await readdir(current, { withFileTypes: true });
  for (const entry of entries.sort((left, right) => left.name.localeCompare(right.name))) {
    const path = resolve(current, entry.name);
    if (entry.isDirectory()) await snapshotFiles(root, path, snapshot);
    else snapshot[relative(root, path)] = sha256(await readFile(path));
  }
  return snapshot;
}

function sha256(value) {
  return createHash("sha256").update(value).digest("hex");
}

afterEach(async () => {
  await Promise.all(temporaryRoots.splice(0).map((path) =>
    rm(path, { recursive: true, force: true })
  ));
});

describe("production release package", () => {
  it("prepares and validates the exact flat package with every notice support file", async () => {
    const sourceRoot = await createSourceFixture();
    const packageRoot = resolve(sourceRoot, ".tmp/release");
    const packaged = await prepareProductionPackage({ sourceRoot, outputRoot: packageRoot });

    expect(packaged.files).toEqual([
      "Apache-2.0.txt",
      "Emscripten-LICENSE.txt",
      "LICENSE",
      "Rust-DEPENDENCY-LICENSES.md",
      "SHA256SUMS",
      "THIRD_PARTY_NOTICES.md",
      "gate5.evidence.json",
      "kwiry-search.zip",
      "main.js",
      "manifest.json",
      "styles.css",
    ]);
    expect((await readdir(packageRoot)).sort()).toEqual(packaged.files);

    const notices = await readFile(resolve(packageRoot, "THIRD_PARTY_NOTICES.md"), "utf8");
    for (const name of [
      "Apache-2.0.txt",
      "Emscripten-LICENSE.txt",
      "Rust-DEPENDENCY-LICENSES.md",
    ]) {
      expect(notices).toContain(`[\`${name}\`](${name})`);
      expect(notices).not.toContain(`licenses/${name}`);
    }

    const checksums = (await readFile(resolve(packageRoot, "SHA256SUMS"), "utf8"))
      .trim().split("\n");
    expect(checksums).toHaveLength(packaged.files.length - 1);
    for (const name of packaged.files.filter((name) => name !== "SHA256SUMS")) {
      expect(checksums.some((line) => line.endsWith(`  ${name}`))).toBe(true);
    }
    await expect(validateProductionPackage({ sourceRoot, packageRoot })).resolves.toEqual(packaged);
  });

  // The archive is advertised as the install-by-hand path. Shipping only the
  // three runtime files inside it handed someone a GPL-3.0-only binary with no
  // licence and no third-party notices, because the notices sat beside the zip
  // as separate release assets that a hand installer never takes.
  it("conveys the license and every notice inside the runtime archive", async () => {
    const sourceRoot = await createSourceFixture();
    const packageRoot = resolve(sourceRoot, ".tmp/release");
    await prepareProductionPackage({ sourceRoot, outputRoot: packageRoot });

    const bytes = await readFile(resolve(packageRoot, "kwiry-search.zip"));
    const entries = new Map();
    let offset = 0;
    while (offset + 30 <= bytes.length && bytes.readUInt32LE(offset) === 0x04034b50) {
      const size = bytes.readUInt32LE(offset + 18);
      const nameLength = bytes.readUInt16LE(offset + 26);
      const start = offset + 30 + nameLength + bytes.readUInt16LE(offset + 28);
      expect(bytes.readUInt16LE(offset + 8), "archive entries must be stored").toBe(0);
      entries.set(
        bytes.subarray(offset + 30, offset + 30 + nameLength).toString("utf8"),
        bytes.subarray(start, start + size),
      );
      offset = start + size;
    }

    expect([...entries.keys()].sort()).toEqual([
      "Apache-2.0.txt",
      "Emscripten-LICENSE.txt",
      "LICENSE",
      "Rust-DEPENDENCY-LICENSES.md",
      "THIRD_PARTY_NOTICES.md",
      "main.js",
      "manifest.json",
      "styles.css",
    ]);
    // Every entry must be the package's own file, so nothing reaches a user
    // through the archive that the package validation did not already cover.
    for (const [name, entryBytes] of entries) {
      expect(await readFile(resolve(packageRoot, name)), name).toEqual(entryBytes);
    }
    // The archive must not carry the release's own description or checksums.
    expect(entries.has("gate5.evidence.json")).toBe(false);
    expect(entries.has("SHA256SUMS")).toBe(false);
  });

  it.each([
    ["source root", (sourceRoot) => sourceRoot],
    ["source ancestor", (sourceRoot) => dirname(sourceRoot)],
  ])("rejects a destructive %s output before preserving every source file", async (_name, output) => {
    const sourceRoot = await createSourceFixture();
    const sourceSnapshot = await snapshotFiles(sourceRoot);

    await expect(prepareProductionPackage({ sourceRoot, outputRoot: output(sourceRoot) }))
      .rejects.toThrow("must not contain the production source");
    expect(await snapshotFiles(sourceRoot)).toEqual(sourceSnapshot);
  });

  it("rejects a destructive source descendant outside the dedicated staging root", async () => {
    const sourceRoot = await createSourceFixture();
    const sourceSnapshot = await snapshotFiles(sourceRoot);

    await expect(prepareProductionPackage({
      sourceRoot,
      outputRoot: resolve(sourceRoot, "src"),
    })).rejects.toThrow("inside the production source must stay under .tmp");
    expect(await snapshotFiles(sourceRoot)).toEqual(sourceSnapshot);
  });

  it("rejects an output that would delete its evidence input", async () => {
    const sourceRoot = await createSourceFixture();
    const evidenceRoot = resolve(sourceRoot, ".tmp/evidence");
    const evidencePath = resolve(evidenceRoot, "gate5.evidence.json");
    await mkdir(evidenceRoot, { recursive: true });
    await copyFile(resolve(sourceRoot, "gate5.evidence.json"), evidencePath);

    await expect(prepareProductionPackage({
      sourceRoot,
      outputRoot: evidenceRoot,
      evidencePath,
    })).rejects.toThrow("must not contain the Gate 5 evidence input");
    expect(await readFile(evidencePath, "utf8")).toContain("kwiry_gate5_automated_evidence");
  });

  it("rejects schema-valid performance evidence that is not release evidence", async () => {
    const sourceRoot = await createSourceFixture();
    await writeJson(resolve(sourceRoot, "gate5.evidence.json"), validPerformanceEvidence());

    await expect(prepareProductionPackage({
      sourceRoot,
      outputRoot: resolve(sourceRoot, ".tmp/release"),
    })).rejects.toThrow("production release requires automated Gate 5 evidence");
  });

  it("rejects schema-valid evidence from a different main artifact", async () => {
    const sourceRoot = await createSourceFixture();
    const evidence = validEvidence();
    evidence.artifact.main = { bytes: 1, sha256: HASH };
    await writeJson(resolve(sourceRoot, "gate5.evidence.json"), evidence);

    await expect(prepareProductionPackage({
      sourceRoot,
      outputRoot: resolve(sourceRoot, ".tmp/release"),
    })).rejects.toThrow("Gate 5 evidence main artifact does not match source main.js");
  });

  it("rejects main.js changed after its Gate 5 evidence was produced", async () => {
    const sourceRoot = await createSourceFixture();
    await writeFile(resolve(sourceRoot, "main.js"), `${MAIN_JS}// changed after evidence\n`);

    await expect(prepareProductionPackage({
      sourceRoot,
      outputRoot: resolve(sourceRoot, ".tmp/release"),
    })).rejects.toThrow("Gate 5 evidence main artifact does not match source main.js");
  });

  it("routes CI, candidate validation, and publication through the same packager", async () => {
    for (const [name, command] of [
      ["ci.yml", "npm run package:release -- .tmp/gate5-field-package gate5.evidence.json"],
      ["release-plugin.yml", "npm run package:release -- .tmp/release-candidate gate5.evidence.json"],
      ["publish-plugin-release.yml", "npm run package:release -- .tmp/release-assets gate5.evidence.json"],
    ]) {
      const workflow = await readFile(
        resolve(repositoryRoot, "../../.github/workflows", name),
        "utf8",
      );
      expect(workflow).toContain(command);
      if (name !== "ci.yml") {
        expect(workflow).not.toContain(
          "cp main.js manifest.json styles.css LICENSE THIRD_PARTY_NOTICES.md",
        );
      }
    }
  });

  it("fails when a notice-referenced support file is missing", async () => {
    const sourceRoot = await createSourceFixture();
    const packageRoot = resolve(sourceRoot, ".tmp/release");
    await prepareProductionPackage({ sourceRoot, outputRoot: packageRoot });
    await rm(resolve(packageRoot, "Emscripten-LICENSE.txt"));

    await expect(validateProductionPackage({ sourceRoot, packageRoot }))
      .rejects.toThrow("notice-referenced support file is missing: Emscripten-LICENSE.txt");
  });

  it("fails when package-lock root identity drifts from the manifest and package", async () => {
    const sourceRoot = await createSourceFixture();
    const lock = JSON.parse(await readFile(resolve(sourceRoot, "package-lock.json"), "utf8"));
    lock.packages[""].version = "0.1.0";
    await writeJson(resolve(sourceRoot, "package-lock.json"), lock);

    await expect(validateProductionIdentity(sourceRoot))
      .rejects.toThrow("manifest, package, and package-lock root versions must match");
  });
});
