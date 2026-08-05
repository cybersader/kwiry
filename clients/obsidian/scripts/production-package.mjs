// SPDX-FileCopyrightText: 2026 cybersader
// SPDX-License-Identifier: GPL-3.0-only

import { createHash } from "node:crypto";
import {
  copyFile,
  mkdir,
  readFile,
  readdir,
  rm,
  stat,
  writeFile,
} from "node:fs/promises";
import { dirname, isAbsolute, relative, resolve, sep } from "node:path";
import { fileURLToPath } from "node:url";

import { validateGate5Evidence } from "./gate5-evidence-schema.mjs";
import { assertPackagePrivacy, assertSourcePrivacy } from "./privacy-policy.mjs";

const root = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const RUNTIME_FILES = Object.freeze(["main.js", "manifest.json", "styles.css"]);
const BASE_SUPPORT_FILES = Object.freeze([
  "LICENSE",
  "THIRD_PARTY_NOTICES.md",
  "gate5.evidence.json",
]);
const REQUIRED_NOTICE_SUPPORT_FILES = Object.freeze([
  "Apache-2.0.txt",
  "Emscripten-LICENSE.txt",
  "Rust-DEPENDENCY-LICENSES.md",
]);
const CHECKSUM_FILE = "SHA256SUMS";
const SOURCE_NOTICE_LINK = /\[`([^`]+)`\]\(licenses\/([A-Za-z0-9][A-Za-z0-9._-]*)\)/gu;
const FLAT_NOTICE_LINK = /\[`([^`]+)`\]\(([A-Za-z0-9][A-Za-z0-9._-]*)\)/gu;
const FORBIDDEN_ARTIFACT = /(?:\.wasm|worker.*\.js|\.map|\.db|\.sqlite)$/iu;

export async function validateProductionIdentity(sourceRoot = root) {
  const [manifest, packageJson, packageLock] = await Promise.all([
    readJson(resolve(sourceRoot, "manifest.json")),
    readJson(resolve(sourceRoot, "package.json")),
    readJson(resolve(sourceRoot, "package-lock.json")),
  ]);
  const lockRoot = packageLock.packages?.[""];
  const versions = [
    manifest.version,
    packageJson.version,
    packageLock.version,
    lockRoot?.version,
  ];
  if (!versions.every((version) => version === packageJson.version)) {
    throw new Error("manifest, package, and package-lock root versions must match");
  }
  if (packageLock.name !== packageJson.name || lockRoot?.name !== packageJson.name) {
    throw new Error("package and package-lock root names must match");
  }
  if (manifest.id !== packageJson.name) {
    throw new Error("manifest id and package name must match");
  }
  if (!isSemver(packageJson.version)) {
    throw new Error("production package version must be semantic");
  }
  return { manifest, packageJson, packageLock };
}

export async function validateProductionSource({
  sourceRoot = root,
  evidencePath = resolve(sourceRoot, "gate5.evidence.json"),
} = {}) {
  const identity = await validateProductionIdentity(sourceRoot);
  await assertSourcePrivacy(sourceRoot);
  const noticeSource = await readFile(resolve(sourceRoot, "THIRD_PARTY_NOTICES.md"), "utf8");
  const noticeFiles = noticeSupportFiles(noticeSource, "source");
  await requireNoticeFiles(sourceRoot, noticeFiles, true);

  for (const name of [...RUNTIME_FILES, "LICENSE"]) {
    if (!(await isFile(resolve(sourceRoot, name)))) {
      throw new Error(`production release input is missing: ${name}`);
    }
  }
  const mainBytes = await readFile(resolve(sourceRoot, "main.js"));
  if (!mainBytes.toString("utf8", 0, 2_000).includes("GNU General Public License")) {
    throw new Error("production main.js is missing its GPL banner");
  }
  const looseArtifacts = (await readdir(sourceRoot)).filter((name) => FORBIDDEN_ARTIFACT.test(name));
  if (looseArtifacts.length !== 0) {
    throw new Error("production source contains a loose runtime artifact");
  }
  const evidence = validateReleaseEvidence(
    validateGate5Evidence(await readJson(evidencePath)),
    mainBytes,
    "source main.js",
  );
  return { ...identity, evidence, noticeFiles, noticeSource };
}

export async function prepareProductionPackage({
  sourceRoot = root,
  outputRoot,
  evidencePath = resolve(sourceRoot, "gate5.evidence.json"),
} = {}) {
  if (!outputRoot) throw new Error("production package output path is required");
  sourceRoot = resolve(sourceRoot);
  outputRoot = resolve(outputRoot);
  evidencePath = resolve(evidencePath);
  assertSafeOutputRoot({ sourceRoot, outputRoot, evidencePath });
  const source = await validateProductionSource({ sourceRoot, evidencePath });
  await rm(outputRoot, { recursive: true, force: true });
  await mkdir(outputRoot, { recursive: true });

  for (const name of RUNTIME_FILES) {
    await copyFile(resolve(sourceRoot, name), resolve(outputRoot, name));
  }
  await copyFile(resolve(sourceRoot, "LICENSE"), resolve(outputRoot, "LICENSE"));
  for (const name of source.noticeFiles) {
    await copyFile(resolve(sourceRoot, "licenses", name), resolve(outputRoot, name));
  }
  await writeFile(
    resolve(outputRoot, "THIRD_PARTY_NOTICES.md"),
    flattenNoticeLinks(source.noticeSource),
  );
  await copyFile(evidencePath, resolve(outputRoot, "gate5.evidence.json"));
  await writeChecksums(outputRoot);
  await validateProductionPackage({ sourceRoot, packageRoot: outputRoot, evidencePath });
  return { packageRoot: outputRoot, files: productionPackageFiles(source.noticeFiles) };
}

export async function validateProductionPackage({
  sourceRoot = root,
  packageRoot,
  evidencePath = resolve(sourceRoot, "gate5.evidence.json"),
} = {}) {
  if (!packageRoot) throw new Error("production package path is required");
  await validateProductionIdentity(sourceRoot);
  const [sourceNoticeText, noticeText] = await Promise.all([
    readFile(resolve(sourceRoot, "THIRD_PARTY_NOTICES.md"), "utf8"),
    readFile(resolve(packageRoot, "THIRD_PARTY_NOTICES.md"), "utf8"),
  ]);
  const sourceNoticeFiles = noticeSupportFiles(sourceNoticeText, "source");
  const noticeFiles = noticeSupportFiles(noticeText, "flat");
  if (JSON.stringify(noticeFiles) !== JSON.stringify(sourceNoticeFiles)
    || noticeText !== flattenNoticeLinks(sourceNoticeText)) {
    throw new Error("packaged third-party notices do not match the source notices");
  }
  await requireNoticeFiles(sourceRoot, sourceNoticeFiles, true);
  await requireNoticeFiles(packageRoot, noticeFiles, false);

  const expectedFiles = productionPackageFiles(noticeFiles);
  const entries = await readdir(packageRoot, { withFileTypes: true });
  if (entries.some((entry) => !entry.isFile())) {
    throw new Error("production package must use one flat file layout");
  }
  const actualFiles = entries.map((entry) => entry.name).sort();
  if (JSON.stringify(actualFiles) !== JSON.stringify(expectedFiles)) {
    throw new Error("production package file set is invalid");
  }
  if (actualFiles.some((name) => FORBIDDEN_ARTIFACT.test(name))) {
    throw new Error("production package contains a forbidden loose runtime artifact");
  }

  for (const name of [...RUNTIME_FILES, "LICENSE", "gate5.evidence.json"]) {
    const sourcePath = name === "gate5.evidence.json" ? evidencePath : resolve(sourceRoot, name);
    const [sourceBytes, packageBytes] = await Promise.all([
      readFile(sourcePath),
      readFile(resolve(packageRoot, name)),
    ]);
    if (!sourceBytes.equals(packageBytes)) {
      throw new Error(`production package file does not match the validated source: ${name}`);
    }
  }
  for (const name of noticeFiles) {
    const [sourceBytes, packageBytes] = await Promise.all([
      readFile(resolve(sourceRoot, "licenses", name)),
      readFile(resolve(packageRoot, name)),
    ]);
    if (!sourceBytes.equals(packageBytes)) {
      throw new Error(`production support file does not match the validated source: ${name}`);
    }
  }
  const [sourceMainBytes, packageMainBytes] = await Promise.all([
    readFile(resolve(sourceRoot, "main.js")),
    readFile(resolve(packageRoot, "main.js")),
  ]);
  validateReleaseEvidence(
    validateGate5Evidence(await readJson(evidencePath)),
    sourceMainBytes,
    "source main.js",
  );
  validateReleaseEvidence(
    validateGate5Evidence(await readJson(resolve(packageRoot, "gate5.evidence.json"))),
    packageMainBytes,
    "packaged main.js",
  );
  if (!packageMainBytes.toString("utf8", 0, 2_000).includes("GNU General Public License")) {
    throw new Error("production package main.js is missing its GPL banner");
  }

  await assertPackagePrivacy(packageRoot);
  await validateChecksums(packageRoot, expectedFiles.filter((name) => name !== CHECKSUM_FILE));
  return { packageRoot, files: expectedFiles };
}

function assertSafeOutputRoot({ sourceRoot, outputRoot, evidencePath }) {
  if (containsPath(outputRoot, sourceRoot)) {
    throw new Error("production package output must not contain the production source");
  }
  if (containsPath(sourceRoot, outputRoot)
    && !containsPath(resolve(sourceRoot, ".tmp"), outputRoot)) {
    throw new Error("production package output inside the production source must stay under .tmp");
  }
  if (containsPath(outputRoot, evidencePath)) {
    throw new Error("production package output must not contain the Gate 5 evidence input");
  }
}

function containsPath(parent, child) {
  const path = relative(parent, child);
  return path === "" || (path !== ".." && !path.startsWith(`..${sep}`) && !isAbsolute(path));
}

function validateReleaseEvidence(evidence, mainBytes, label) {
  if (evidence.kind !== "kwiry_gate5_automated_evidence") {
    throw new Error("production release requires automated Gate 5 evidence");
  }
  if (evidence.artifact.main.bytes !== mainBytes.byteLength
    || evidence.artifact.main.sha256 !== sha256(mainBytes)) {
    throw new Error(`Gate 5 evidence main artifact does not match ${label}`);
  }
  return evidence;
}

function noticeSupportFiles(noticeText, layout) {
  const pattern = layout === "source" ? SOURCE_NOTICE_LINK : FLAT_NOTICE_LINK;
  const files = [];
  for (const match of noticeText.matchAll(pattern)) {
    const [, label, name] = match;
    const expectedLabel = layout === "source" ? `licenses/${name}` : name;
    if (label !== expectedLabel) {
      throw new Error("third-party notice support links must use consistent labels");
    }
    files.push(name);
  }
  const unique = [...new Set(files)].sort();
  if (unique.length !== files.length) {
    throw new Error("third-party notices contain duplicate support-license links");
  }
  for (const required of REQUIRED_NOTICE_SUPPORT_FILES) {
    if (!unique.includes(required)) {
      throw new Error(`third-party notices must reference support license: ${required}`);
    }
  }
  if (layout === "flat" && noticeText.includes("licenses/")) {
    throw new Error("packaged third-party notice links must be flat");
  }
  return unique;
}

async function requireNoticeFiles(baseRoot, noticeFiles, nested) {
  for (const name of noticeFiles) {
    const path = nested ? resolve(baseRoot, "licenses", name) : resolve(baseRoot, name);
    if (!(await isFile(path))) {
      throw new Error(`notice-referenced support file is missing: ${name}`);
    }
  }
}

function flattenNoticeLinks(noticeText) {
  const flattened = noticeText.replace(
    SOURCE_NOTICE_LINK,
    (_match, label, name) => {
      if (label !== `licenses/${name}`) {
        throw new Error("third-party notice support links must use consistent labels");
      }
      return `[\`${name}\`](${name})`;
    },
  );
  noticeSupportFiles(flattened, "flat");
  return flattened;
}

function productionPackageFiles(noticeFiles) {
  return [
    ...RUNTIME_FILES,
    ...BASE_SUPPORT_FILES,
    ...noticeFiles,
    CHECKSUM_FILE,
  ].sort();
}

async function writeChecksums(packageRoot) {
  const files = (await readdir(packageRoot)).sort();
  const lines = [];
  for (const name of files) {
    const bytes = await readFile(resolve(packageRoot, name));
    lines.push(`${sha256(bytes)}  ${name}`);
  }
  await writeFile(resolve(packageRoot, CHECKSUM_FILE), `${lines.join("\n")}\n`);
}

async function validateChecksums(packageRoot, files) {
  const expectedLines = [];
  for (const name of [...files].sort()) {
    const bytes = await readFile(resolve(packageRoot, name));
    expectedLines.push(`${sha256(bytes)}  ${name}`);
  }
  const expected = `${expectedLines.join("\n")}\n`;
  const actual = await readFile(resolve(packageRoot, CHECKSUM_FILE), "utf8");
  if (actual !== expected) throw new Error("production package checksums are invalid");
}

async function readJson(path) {
  return JSON.parse(await readFile(path, "utf8"));
}

async function isFile(path) {
  try {
    return (await stat(path)).isFile();
  } catch (error) {
    if (error?.code === "ENOENT") return false;
    throw error;
  }
}

function sha256(value) {
  return createHash("sha256").update(value).digest("hex");
}

function isSemver(value) {
  return typeof value === "string"
    && /^[0-9]+\.[0-9]+\.[0-9]+(?:-[0-9A-Za-z.-]+)?(?:\+[0-9A-Za-z.-]+)?$/u.test(value);
}

if (process.argv[1] === fileURLToPath(import.meta.url)) {
  const [command, first, second, ...unexpected] = process.argv.slice(2);
  if (unexpected.length !== 0) throw new Error("too many production package arguments");
  if (command === "validate-source") {
    await validateProductionSource({ evidencePath: first ? resolve(first) : undefined });
  } else if (command === "prepare") {
    if (!first) throw new Error("usage: production-package.mjs prepare <output> [evidence]");
    await prepareProductionPackage({
      outputRoot: resolve(first),
      evidencePath: second ? resolve(second) : undefined,
    });
  } else if (command === "validate") {
    if (!first) throw new Error("usage: production-package.mjs validate <package>");
    await validateProductionPackage({ packageRoot: resolve(first) });
  } else {
    throw new Error("unknown production package command");
  }
}
