// SPDX-FileCopyrightText: 2026 cybersader
// SPDX-License-Identifier: GPL-3.0-only

import { createHash } from "node:crypto";
import {
  copyFile, mkdir, mkdtemp, readFile, rm, symlink, writeFile,
} from "node:fs/promises";
import { tmpdir } from "node:os";
import { resolve } from "node:path";
import { afterEach, describe, expect, it } from "vitest";

import { GATE5_TARGETS } from "../scripts/gate5-evidence-schema.mjs";
import { embedWorkerPrivacyBoundary } from "../scripts/privacy-policy.mjs";
import { prepareProductionPackage } from "../scripts/production-package.mjs";
import {
  prepareReleaseCandidateHandoff,
  validateReleaseCandidateEnvelope,
  validateReleaseCandidateHandoff,
} from "../scripts/release-candidate-handoff.mjs";
import { validateWebdriverReleaseEvidence } from "../scripts/webdriver-release-gate-schema.mjs";

const repositoryRoot = resolve(import.meta.dirname, "..");
const VERSION = "0.6.0-beta.15";
const HASH = "a".repeat(64);
const COMMIT = "b".repeat(40);
const MAIN_JS = [
  "/* GNU General Public License */",
  embedWorkerPrivacyBoundary("export {};\n"),
  "module.exports = {};",
  "",
].join("\n");
const temporaryRoots = [];

async function createFixture() {
  const root = await mkdtemp(resolve(tmpdir(), "kwiry-release-handoff-"));
  temporaryRoots.push(root);
  const sourceRoot = resolve(root, "source");
  const candidateRoot = resolve(root, "candidate");
  const outputRoot = resolve(root, "handoff");
  const runtimeManifestPath = resolve(root, "runtime-manifest.json");
  const evidencePath = resolve(root, "webdriver.evidence.json");
  const releaseNotesPath = resolve(root, "release-notes.md");
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
    writeJson(resolve(sourceRoot, "manifest.json"), { id: "kwiry-search", version: VERSION }),
    writeJson(resolve(sourceRoot, "package.json"), { name: "kwiry-search", version: VERSION }),
    writeJson(resolve(sourceRoot, "package-lock.json"), {
      name: "kwiry-search", version: VERSION, lockfileVersion: 3,
      packages: { "": { name: "kwiry-search", version: VERSION } },
    }),
    writeJson(resolve(sourceRoot, "gate5.evidence.json"), gate5Evidence()),
    copyFile(resolve(repositoryRoot, "LICENSE"), resolve(sourceRoot, "LICENSE")),
    copyFile(resolve(repositoryRoot, "THIRD_PARTY_NOTICES.md"), resolve(sourceRoot, "THIRD_PARTY_NOTICES.md")),
    ...["Apache-2.0.txt", "Emscripten-LICENSE.txt", "Rust-DEPENDENCY-LICENSES.md"].map((name) =>
      copyFile(resolve(repositoryRoot, "licenses", name), resolve(sourceRoot, "licenses", name))),
  ]);
  await prepareProductionPackage({ sourceRoot, outputRoot: candidateRoot });
  const runtimeManifestBytes = Buffer.from('{"schema_version":1}\n');
  await writeFile(runtimeManifestPath, runtimeManifestBytes);
  const candidate = await import("../scripts/production-package.mjs")
    .then(({ describeProductionPackage }) => describeProductionPackage({ sourceRoot, packageRoot: candidateRoot }));
  const evidence = webdriverEvidence(candidate, sha256(runtimeManifestBytes));
  validateWebdriverReleaseEvidence(evidence);
  await writeJson(evidencePath, evidence);
  await writeFile(releaseNotesPath, "Beta release notes.\n");
  return {
    root, sourceRoot, candidateRoot, outputRoot, runtimeManifestPath, evidencePath,
    releaseNotesPath, candidate,
  };
}

function gate5Evidence() {
  return {
    schema_version: 1,
    kind: "kwiry_gate5_automated_evidence",
    verdict: "AUTOMATED_CHECKS_PASSED_OWNER_REVIEW_REQUIRED",
    automation_scope: "artifact_and_generated_functional_corpus",
    protocol_version: 2,
    artifact: {
      main: { bytes: Buffer.byteLength(MAIN_JS), sha256: sha256(MAIN_JS) },
      worker: { bytes: 1, sha256: HASH }, rust_wasm: { bytes: 1, sha256: HASH },
      sqlite_wasm: { bytes: 1, sha256: HASH }, deterministic: true, wasm_inputs: 2,
      loose_runtime_assets: 0,
    },
    corpus: {
      kind: "generated_functional", note_count: 14, markdown_bytes: 1_024,
      sha256: HASH, hash_algorithm: "sha256-path-nul-decimal-length-nul-bytes-nul-v1",
      expected_documents: 14,
    },
    checks: { total: 1, failed: 0 },
    targets: GATE5_TARGETS.map(([id, threshold, unit]) => ({
      id, threshold, unit, status: "not_measured", value: null,
      scope: "declared_reference_hardware",
    })),
    privacy: {
      aggregate_only: true, paths_emitted: 0, vault_names_emitted: 0,
      note_content_emitted: 0, raw_queries_emitted: 0, tokens_emitted: 0,
      stack_traces_emitted: 0, sql_emitted: 0, environment_paths_emitted: 0,
      private_corpus_hashes_emitted: 0, loose_evidence_artifacts: 0,
    },
  };
}

function webdriverEvidence(candidate, runtimeManifestSha256) {
  return {
    schema_version: 1,
    kind: "kwiry_obsidian_webdriver_release_gate",
    verdict: "SELENIUM_RELEASE_GATE_PASSED",
    scope: "narrow_real_obsidian_selection_lifecycle",
    candidate: {
      version: candidate.version,
      candidate_set_sha256: candidate.candidate_set_sha256,
      file_count: candidate.file_count,
    },
    runtime_manifest: { sha256: runtimeManifestSha256 },
    runtime: {
      obsidian: "1.13.7", electron: "43.1.1", chromium: "150.0.7871.114",
      driver: "150.0.7871.114", selenium_webdriver: "4.39.0",
      obsidian_launcher: "3.1.1", node: "22.23.1", platform: "linux-x64-xvfb",
    },
    isolation: {
      private_state_root: true, loopback_cdp: true, loopback_webdriver: true,
      selenium_manager_used: false, system_browser_used: false, system_driver_used: false,
    },
    scenario: {
      synthetic_xlsm: true, excel_explicitly_enabled: true, command_palette_used: true,
      webdriver_input_used: true, native_click_used: true, modal_closed: true,
      stale_notices: 0, open_failure_notices: 0, open_file_calls: 1,
      open_file_promise: "resolved", expected_file_active: true,
      vba_payload_search_results: 0,
    },
    cleanup: {
      webdriver_quit: true, obsidian_reaped: true, verified_download_server_closed: true,
      ports_closed: true, private_state_removed: true,
    },
    privacy: {
      aggregate_only: true, paths_emitted: 0, queries_emitted: 0,
      note_content_emitted: 0, notice_text_emitted: 0, raw_logs_emitted: 0,
      screenshots_emitted: 0, stack_traces_emitted: 0,
    },
  };
}

function handoffOptions(fixture) {
  return {
    sourceRoot: fixture.sourceRoot,
    candidateRoot: fixture.candidateRoot,
    evidencePath: fixture.evidencePath,
    releaseNotesPath: fixture.releaseNotesPath,
    outputRoot: fixture.outputRoot,
    tag: VERSION,
    tagCommit: COMMIT,
    ciRunId: "12345",
    ciRunAttempt: 2,
    candidateRunId: "67890",
    candidateRunAttempt: 3,
    runtimeManifestPath: fixture.runtimeManifestPath,
  };
}

async function writeJson(path, value) {
  await writeFile(path, `${JSON.stringify(value, null, 2)}\n`);
}

function sha256(value) {
  return createHash("sha256").update(value).digest("hex");
}

afterEach(async () => {
  await Promise.all(temporaryRoots.splice(0).map((path) => rm(path, { recursive: true, force: true })));
});

describe("release candidate handoff", () => {
  it("copies and validates the exact tested candidate envelope", async () => {
    const fixture = await createFixture();
    const prepared = await prepareReleaseCandidateHandoff(handoffOptions(fixture));
    expect(validateReleaseCandidateHandoff(prepared.handoff)).toEqual(prepared.handoff);
    await expect(validateReleaseCandidateEnvelope(fixture.outputRoot)).resolves.toEqual(prepared.handoff);
    expect(prepared.handoff.authorization).toEqual({
      ci_run_id: "12345", ci_run_attempt: 2,
      candidate_run_id: "67890", candidate_run_attempt: 3,
    });
  });

  it.each([
    ["tag", async (fixture) => ({ ...handoffOptions(fixture), tag: "0.6.0-beta.14" }), "handoff_tag_version_mismatch"],
    ["runtime manifest", async (fixture) => {
      await writeFile(fixture.runtimeManifestPath, "changed\n");
      return handoffOptions(fixture);
    }, "handoff_runtime_manifest_mismatch"],
    ["candidate evidence", async (fixture) => {
      const evidence = JSON.parse(await readFile(fixture.evidencePath));
      evidence.candidate.candidate_set_sha256 = HASH;
      await writeJson(fixture.evidencePath, evidence);
      return handoffOptions(fixture);
    }, "handoff_evidence_candidate_mismatch"],
  ])("rejects a mismatched %s before writing a handoff", async (_name, mutate, message) => {
    const fixture = await createFixture();
    await expect(prepareReleaseCandidateHandoff(await mutate(fixture))).rejects.toThrow(message);
  });

  it.each([
    ["asset", async (fixture) => writeFile(resolve(fixture.outputRoot, "release-assets/styles.css"), "changed\n"), "handoff_asset_identity_invalid"],
    ["evidence", async (fixture) => writeFile(resolve(fixture.outputRoot, "webdriver.evidence.json"), "{}\n"), undefined],
    ["notes", async (fixture) => writeFile(resolve(fixture.outputRoot, "release-notes.md"), "changed\n"), "handoff_notes_identity_invalid"],
    ["checksum", async (fixture) => writeFile(resolve(fixture.outputRoot, "release-handoff.sha256"), "changed\n"), "handoff_checksums_invalid"],
  ])("rejects a mutated %s in the prepared envelope", async (_name, mutate, message) => {
    const fixture = await createFixture();
    await prepareReleaseCandidateHandoff(handoffOptions(fixture));
    await mutate(fixture);
    const result = expect(validateReleaseCandidateEnvelope(fixture.outputRoot)).rejects;
    if (message) await result.toThrow(message);
    else await result.toThrow();
  });

  it("rejects extra and symbolic-link envelope entries", async () => {
    const fixture = await createFixture();
    await prepareReleaseCandidateHandoff(handoffOptions(fixture));
    await writeFile(resolve(fixture.outputRoot, "extra"), "extra\n");
    await expect(validateReleaseCandidateEnvelope(fixture.outputRoot)).rejects.toThrow("handoff_envelope_invalid");
    await rm(resolve(fixture.outputRoot, "extra"));
    await rm(resolve(fixture.outputRoot, "release-notes.md"));
    await symlink(resolve(fixture.root, "outside"), resolve(fixture.outputRoot, "release-notes.md"));
    await expect(validateReleaseCandidateEnvelope(fixture.outputRoot)).rejects.toThrow("handoff_envelope_invalid");
  });

  it("rejects an output nested inside the validated candidate", async () => {
    const fixture = await createFixture();
    await expect(prepareReleaseCandidateHandoff({
      ...handoffOptions(fixture),
      outputRoot: resolve(fixture.candidateRoot, "handoff"),
    })).rejects.toThrow("handoff_output_unsafe");
  });
});
