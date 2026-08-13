// SPDX-FileCopyrightText: 2026 cybersader
// SPDX-License-Identifier: GPL-3.0-only

import { createHash } from "node:crypto";
import { constants } from "node:fs";
import {
  copyFile, lstat, mkdir, readFile, readdir, rm, writeFile,
} from "node:fs/promises";
import { basename, dirname, isAbsolute, relative, resolve, sep } from "node:path";
import { fileURLToPath } from "node:url";

import { describeProductionPackage } from "./production-package.mjs";
import { validateWebdriverReleaseEvidence } from "./webdriver-release-gate-schema.mjs";

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const HANDOFF_KIND = "kwiry_obsidian_validated_candidate_handoff";
const HANDOFF_FILE = "release-handoff.json";
const CHECKSUM_FILE = "release-handoff.sha256";
const EVIDENCE_FILE = "webdriver.evidence.json";
const NOTES_FILE = "release-notes.md";
const ASSET_DIR = "release-assets";
const HASH = /^[a-f0-9]{64}$/u;
const SEMVER = /^[0-9]+\.[0-9]+\.[0-9]+(?:-[0-9A-Za-z.-]+)?(?:\+[0-9A-Za-z.-]+)?$/u;
const COMMIT = /^[a-f0-9]{40}$/u;
const ID = /^[1-9][0-9]*$/u;

export async function prepareReleaseCandidateHandoff({
  sourceRoot = ROOT,
  candidateRoot,
  evidencePath,
  releaseNotesPath,
  outputRoot,
  tag,
  tagCommit,
  ciRunId,
  ciRunAttempt,
  candidateRunId,
  candidateRunAttempt,
  runtimeManifestPath = resolve(sourceRoot, "scripts/webdriver-release-gate-manifest.json"),
} = {}) {
  validateInputs({ candidateRoot, evidencePath, releaseNotesPath, outputRoot, tag, tagCommit,
    ciRunId, ciRunAttempt, candidateRunId, candidateRunAttempt });
  const candidate = await describeProductionPackage({ sourceRoot, packageRoot: candidateRoot });
  if (candidate.version !== tag) throw new Error("handoff_tag_version_mismatch");
  const evidenceBytes = await readFile(evidencePath);
  const evidence = validateWebdriverReleaseEvidence(JSON.parse(evidenceBytes));
  if (evidence.candidate.version !== candidate.version
    || evidence.candidate.candidate_set_sha256 !== candidate.candidate_set_sha256
    || evidence.candidate.file_count !== candidate.file_count) {
    throw new Error("handoff_evidence_candidate_mismatch");
  }
  const runtimeManifestBytes = await readFile(runtimeManifestPath);
  if (evidence.runtime_manifest.sha256 !== sha256(runtimeManifestBytes)) {
    throw new Error("handoff_runtime_manifest_mismatch");
  }
  const notesBytes = await readFile(releaseNotesPath);
  if (notesBytes.byteLength > 1024 * 1024 || notesBytes.includes(0)) {
    throw new Error("handoff_release_notes_invalid");
  }

  assertSafeOutput(candidateRoot, outputRoot);
  await rm(outputRoot, { recursive: true, force: true });
  const assetRoot = resolve(outputRoot, ASSET_DIR);
  await mkdir(assetRoot, { recursive: true });
  for (const identity of candidate.files) {
    await copyRegularFile(resolve(candidateRoot, identity.name), resolve(assetRoot, identity.name));
  }
  await copyRegularFile(evidencePath, resolve(outputRoot, EVIDENCE_FILE));
  await copyRegularFile(releaseNotesPath, resolve(outputRoot, NOTES_FILE));

  const handoff = {
    schema_version: 1,
    kind: HANDOFF_KIND,
    tag,
    tag_commit: tagCommit,
    candidate: {
      version: candidate.version,
      candidate_set_sha256: candidate.candidate_set_sha256,
      file_count: candidate.file_count,
      files: candidate.files,
    },
    webdriver_evidence: {
      sha256: sha256(evidenceBytes),
      kind: evidence.kind,
      verdict: evidence.verdict,
      scope: evidence.scope,
    },
    runtime_manifest: { sha256: sha256(runtimeManifestBytes) },
    release_notes: { sha256: sha256(notesBytes), bytes: notesBytes.byteLength },
    authorization: {
      ci_run_id: String(ciRunId),
      ci_run_attempt: Number(ciRunAttempt),
      candidate_run_id: String(candidateRunId),
      candidate_run_attempt: Number(candidateRunAttempt),
    },
  };
  validateReleaseCandidateHandoff(handoff);
  await writeFile(resolve(outputRoot, HANDOFF_FILE), `${JSON.stringify(handoff, null, 2)}\n`, { flag: "wx" });
  await writeHandoffChecksums(outputRoot);
  await validateReleaseCandidateEnvelope(outputRoot);
  return { outputRoot, handoff };
}

export function validateReleaseCandidateHandoff(value) {
  const root = exact(value, [
    "schema_version", "kind", "tag", "tag_commit", "candidate", "webdriver_evidence",
    "runtime_manifest", "release_notes", "authorization",
  ]);
  equal(root.schema_version, 1);
  equal(root.kind, HANDOFF_KIND);
  match(root.tag, SEMVER);
  match(root.tag_commit, COMMIT);
  const candidate = exact(root.candidate, ["version", "candidate_set_sha256", "file_count", "files"]);
  equal(candidate.version, root.tag);
  match(candidate.version, SEMVER);
  match(candidate.candidate_set_sha256, HASH);
  if (!Number.isInteger(candidate.file_count) || candidate.file_count !== candidate.files.length) fail();
  let previous = null;
  for (const file of candidate.files) {
    const identity = exact(file, ["name", "bytes", "sha256"]);
    if (!safeFlatName(identity.name) || (previous !== null && identity.name.localeCompare(previous) <= 0)
      || !Number.isInteger(identity.bytes) || identity.bytes < 1) fail();
    match(identity.sha256, HASH);
    previous = identity.name;
  }
  const webdriver = exact(root.webdriver_evidence, ["sha256", "kind", "verdict", "scope"]);
  match(webdriver.sha256, HASH);
  equal(webdriver.kind, "kwiry_obsidian_webdriver_release_gate");
  equal(webdriver.verdict, "SELENIUM_RELEASE_GATE_PASSED");
  equal(webdriver.scope, "narrow_real_obsidian_selection_lifecycle");
  match(exact(root.runtime_manifest, ["sha256"]).sha256, HASH);
  const notes = exact(root.release_notes, ["sha256", "bytes"]);
  match(notes.sha256, HASH);
  if (!Number.isInteger(notes.bytes) || notes.bytes < 1 || notes.bytes > 1024 * 1024) fail();
  const auth = exact(root.authorization, [
    "ci_run_id", "ci_run_attempt", "candidate_run_id", "candidate_run_attempt",
  ]);
  match(auth.ci_run_id, ID);
  match(auth.candidate_run_id, ID);
  for (const attempt of [auth.ci_run_attempt, auth.candidate_run_attempt]) {
    if (!Number.isInteger(attempt) || attempt < 1) fail();
  }
  return root;
}

export async function validateReleaseCandidateEnvelope(outputRoot) {
  const rootEntries = await readdir(outputRoot, { withFileTypes: true });
  const expectedRoot = [ASSET_DIR, CHECKSUM_FILE, EVIDENCE_FILE, HANDOFF_FILE, NOTES_FILE].sort();
  if (JSON.stringify(rootEntries.map((entry) => entry.name).sort()) !== JSON.stringify(expectedRoot)
    || rootEntries.some((entry) => entry.name === ASSET_DIR ? !entry.isDirectory() : !entry.isFile())) {
    throw new Error("handoff_envelope_invalid");
  }
  const handoff = validateReleaseCandidateHandoff(JSON.parse(await readFile(resolve(outputRoot, HANDOFF_FILE))));
  const assets = await readdir(resolve(outputRoot, ASSET_DIR), { withFileTypes: true });
  if (assets.some((entry) => !entry.isFile())
    || JSON.stringify(assets.map((entry) => entry.name).sort())
      !== JSON.stringify(handoff.candidate.files.map(({ name }) => name).sort())) {
    throw new Error("handoff_asset_set_invalid");
  }
  for (const identity of handoff.candidate.files) {
    const bytes = await readFile(resolve(outputRoot, ASSET_DIR, identity.name));
    if (bytes.byteLength !== identity.bytes || sha256(bytes) !== identity.sha256) {
      throw new Error("handoff_asset_identity_invalid");
    }
  }
  const evidenceBytes = await readFile(resolve(outputRoot, EVIDENCE_FILE));
  const evidence = validateWebdriverReleaseEvidence(JSON.parse(evidenceBytes));
  if (sha256(evidenceBytes) !== handoff.webdriver_evidence.sha256
    || evidence.candidate.candidate_set_sha256 !== handoff.candidate.candidate_set_sha256) {
    throw new Error("handoff_evidence_identity_invalid");
  }
  const notesBytes = await readFile(resolve(outputRoot, NOTES_FILE));
  if (sha256(notesBytes) !== handoff.release_notes.sha256
    || notesBytes.byteLength !== handoff.release_notes.bytes) {
    throw new Error("handoff_notes_identity_invalid");
  }
  await validateHandoffChecksums(outputRoot);
  return handoff;
}

async function writeHandoffChecksums(outputRoot) {
  const files = await envelopeFileList(outputRoot);
  const lines = [];
  for (const path of files) lines.push(`${sha256(await readFile(resolve(outputRoot, path)))}  ${path}`);
  await writeFile(resolve(outputRoot, CHECKSUM_FILE), `${lines.join("\n")}\n`, { flag: "wx" });
}

async function validateHandoffChecksums(outputRoot) {
  const files = await envelopeFileList(outputRoot);
  const expected = [];
  for (const path of files) expected.push(`${sha256(await readFile(resolve(outputRoot, path)))}  ${path}`);
  if (await readFile(resolve(outputRoot, CHECKSUM_FILE), "utf8") !== `${expected.join("\n")}\n`) {
    throw new Error("handoff_checksums_invalid");
  }
}

async function envelopeFileList(outputRoot) {
  const handoff = validateReleaseCandidateHandoff(JSON.parse(await readFile(resolve(outputRoot, HANDOFF_FILE))));
  return [
    ...handoff.candidate.files.map(({ name }) => `${ASSET_DIR}/${name}`),
    EVIDENCE_FILE,
    HANDOFF_FILE,
    NOTES_FILE,
  ].sort();
}

async function copyRegularFile(source, destination) {
  const metadata = await lstat(source);
  if (!metadata.isFile() || metadata.isSymbolicLink()) throw new Error("handoff_input_not_regular");
  await copyFile(source, destination, constants.COPYFILE_EXCL);
}

function validateInputs(values) {
  for (const key of ["candidateRoot", "evidencePath", "releaseNotesPath", "outputRoot"]) {
    if (!isAbsolute(values[key])) throw new Error("handoff_path_invalid");
  }
  match(values.tag, SEMVER);
  match(values.tagCommit, COMMIT);
  match(String(values.ciRunId), ID);
  match(String(values.candidateRunId), ID);
  for (const value of [Number(values.ciRunAttempt), Number(values.candidateRunAttempt)]) {
    if (!Number.isInteger(value) || value < 1) fail();
  }
}

function assertSafeOutput(candidateRoot, outputRoot) {
  const path = relative(resolve(candidateRoot), resolve(outputRoot));
  if (path === "" || (path !== ".." && !path.startsWith(`..${sep}`) && !isAbsolute(path))) {
    throw new Error("handoff_output_unsafe");
  }
}

function safeFlatName(name) {
  return typeof name === "string" && /^[A-Za-z0-9][A-Za-z0-9._-]*$/u.test(name)
    && name !== "." && name !== ".." && basename(name) === name;
}

function exact(value, keys) {
  if (typeof value !== "object" || value === null || Array.isArray(value)
    || JSON.stringify(Object.keys(value).sort()) !== JSON.stringify([...keys].sort())) fail();
  return value;
}

function equal(actual, expected) {
  if (actual !== expected) fail();
}

function match(value, pattern) {
  if (typeof value !== "string" || !pattern.test(value)) fail();
}

function fail() {
  throw new Error("handoff_schema_invalid");
}

function sha256(value) {
  return createHash("sha256").update(value).digest("hex");
}

if (process.argv[1] === fileURLToPath(import.meta.url)) {
  const [command, ...args] = process.argv.slice(2);
  if (command === "validate" && args.length === 1) {
    await validateReleaseCandidateEnvelope(resolve(args[0]));
  } else if (command === "prepare" && args.length === 10) {
    await prepareReleaseCandidateHandoff({
      candidateRoot: resolve(args[0]), evidencePath: resolve(args[1]),
      releaseNotesPath: resolve(args[2]), outputRoot: resolve(args[3]), tag: args[4],
      tagCommit: args[5], ciRunId: args[6], ciRunAttempt: args[7],
      candidateRunId: args[8], candidateRunAttempt: args[9],
    });
  } else {
    throw new Error("handoff_usage_invalid");
  }
}
