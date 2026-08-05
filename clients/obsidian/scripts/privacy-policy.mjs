// SPDX-FileCopyrightText: 2026 cybersader
// SPDX-License-Identifier: GPL-3.0-only

import { createHash } from "node:crypto";
import { lstat, readFile, readdir } from "node:fs/promises";
import { dirname, relative, resolve, sep } from "node:path";
import { fileURLToPath } from "node:url";
import { TextDecoder } from "node:util";

const defaultSourceRoot = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const utf8 = new TextDecoder("utf-8", { fatal: true });

const SOURCE_TARGETS = Object.freeze([
  Object.freeze({ path: "src", kind: "directory" }),
  Object.freeze({ path: "test", kind: "directory" }),
  Object.freeze({ path: "esbuild.config.mjs", kind: "file" }),
]);
const GENERATED_SOURCE_TARGET = "main.js";

const CONTENT_RULES = Object.freeze([
  Object.freeze({
    id: "machine_path",
    pattern: /\/home\/(?!web_user(?:[\/"']|$))[A-Za-z0-9._-]+(?:\/[A-Za-z0-9._~!$&'()+,;=:@%-]+)*\/?/giu,
  }),
  Object.freeze({
    id: "machine_path",
    pattern: /\/Users\/[A-Za-z0-9._-]+(?:\/[A-Za-z0-9._~!$&'()+,;=:@%-]+)*\/?/gu,
  }),
  Object.freeze({
    id: "machine_path",
    pattern: /\/mnt\/[a-z]\/[A-Za-z0-9._-]+(?:\/[A-Za-z0-9._~!$&'()+,;=:@%-]+)*\/?/giu,
  }),
  Object.freeze({
    id: "machine_path",
    pattern: /[A-Z]:(?:\\{1,2})Users(?:\\{1,2})[A-Za-z0-9._-]+(?:(?:\\{1,2})[A-Za-z0-9._~!$&'()+,;=:@%-]+)*(?:\\{1,2})?/giu,
  }),
  Object.freeze({
    id: "credential_assignment",
    pattern: /(?:api[_-]?key|secret)\s*[:=]/giu,
  }),
  Object.freeze({
    id: "literal_bearer",
    pattern: /bearer\s+[A-Za-z0-9._~+/-]{12,}/giu,
  }),
  Object.freeze({
    id: "secret_prefix",
    pattern: /(?:sk|ghp|github_pat)_[A-Za-z0-9_-]{16,}/giu,
  }),
  Object.freeze({
    id: "ai_attribution",
    pattern: /(?:Co-Authored-By:|Generated(?:-by:|\s+by)|Written\s+by|Created\s+by).*?(?:Claude|Anthropic|GPT|OpenAI)/giu,
  }),
]);

const WORKER_AUTHORITY_RULE = Object.freeze({
  id: "worker_authority",
  pattern: /\b(?:requestUrl|readDaemonToken|Authorization|tokenProvider)\b/gu,
});
const WORKER_BUNDLE_START = "/*__KWIRY_WORKER_AUTHORITY_START_V1__*/";
const WORKER_BUNDLE_END = "/*__KWIRY_WORKER_AUTHORITY_END_V1__*/";
const WORKER_SOURCE_EXTENSION = /\.(?:[cm]?[jt]sx?)$/u;
// Frozen public D5C 0.0.1 and 0.0.2 plugin artifacts predate embedded-Worker
// markers. Exact hashes preserve their immutable validation without admitting
// any newly built unmarked artifact.
const APPROVED_LEGACY_UNMARKED_MAIN_SHA256 = new Set([
  "0999afcd6b772eff2990459b9bcb44436b9b418f6e7ca49c27641427bdd1becc",
  "aa34d8659a9157c4f7b171599ea2f78343aee21462ab51de900743a7be0c6d4c",
]);

const APPROVED_SYNTHETIC_FIXTURES = Object.freeze({
  "test/cache-root.test.ts": Object.freeze({
    machine_path: new Set([
      "/home/u/.cache/kwiry/obsidian-cache",
      "/home/u/.cache/kwiry",
      "/home/u/vault/notes",
      "/home/u/vaultsomething",
      "/home/u/vault/",
      "/home/u/vault",
      "/home/u/other",
      "/home/u/Vault",
      "/home/user",
      "/home/u",
      "/Users/u/Library/Caches/kwiry/obsidian-cache",
      "/Users/u/Vault",
      "/Users/u/vault",
      "/Users/u",
      "C:\\\\Users\\\\u\\\\AppData\\\\Local\\\\kwiry\\\\obsidian-cache",
      "C:\\\\Users\\\\u\\\\AppData\\\\Local",
      "C:\\\\Users\\\\u\\\\AppData\\\\Roaming",
      "C:\\\\Users\\\\u\\\\Vault",
      "C:\\\\Users\\\\u",
      "C:\\\\USERS\\\\U\\\\VAULT",
    ]),
  }),
  "test/cache-store.test.ts": Object.freeze({
    machine_path: new Set([
      "C:\\\\Users\\\\u\\\\Vault\\\\.obsidian\\\\kwiry-cache",
      "C:\\\\Users\\\\u\\\\VaultBackup\\\\cache",
      "C:\\\\Users\\\\u\\\\Vault\\\\cache",
      "C:\\\\Users\\\\u\\\\Vault",
      "C:\\\\Users\\\\u",
    ]),
  }),
  "test/classify-failure.test.ts": Object.freeze({
    credential_assignment: new Set([
      'secret =',
    ]),
  }),
  "test/diagnostics-log.test.ts": Object.freeze({
    credential_assignment: new Set([
      'secret =',
    ]),
  }),
  "test/d5c-brat-package.test.mjs": Object.freeze({
    ai_attribution: new Set([
      "Co-Authored-By:.*(?:Claude",
    ]),
  }),
  "test/gate5-evidence.test.mjs": Object.freeze({
    machine_path: new Set([
      "/home/example/private.md",
      "C:\\\\Users\\\\Example\\\\vault",
    ]),
  }),
  "test/worker-protocol.test.ts": Object.freeze({
    machine_path: new Set(["/home/user/vault"]),
  }),
});

export function embedWorkerPrivacyBoundary(workerSource) {
  if (typeof workerSource !== "string" || workerSource.length === 0) {
    throw new Error("embedded Worker source is unavailable");
  }
  if (workerSource.includes(WORKER_BUNDLE_START) || workerSource.includes(WORKER_BUNDLE_END)) {
    throw new Error("embedded Worker source contains a reserved privacy marker");
  }
  return `${WORKER_BUNDLE_START}\n${workerSource}\n${WORKER_BUNDLE_END}`;
}

export async function assertWorkerAuthorityGraph(sourceRoot, metafile) {
  sourceRoot = resolve(sourceRoot);
  if (!metafile || typeof metafile !== "object" || !metafile.inputs) {
    throw new Error("Worker authority validation requires an esbuild metafile");
  }
  const files = [];
  for (const input of Object.keys(metafile.inputs)) {
    if (!WORKER_SOURCE_EXTENSION.test(input)) continue;
    const path = resolve(sourceRoot, input);
    try {
      const metadata = await lstat(path);
      if (metadata.isSymbolicLink()) {
        throw new Error(`Worker authority validation refuses symbolic links: ${path}`);
      }
      if (metadata.isFile()) files.push(path);
    } catch (error) {
      if (error?.code !== "ENOENT") throw error;
    }
  }
  const findings = await scanFiles(sourceRoot, files, { rules: [WORKER_AUTHORITY_RULE] });
  if (findings.length !== 0) throw new Error(formatPrivacyFindings("Worker graph", findings));
  return { sourceRoot, files: files.map((path) => portableRelative(sourceRoot, path)) };
}

export async function scanSourcePrivacy(sourceRoot = defaultSourceRoot, {
  requireMainArtifact = true,
} = {}) {
  sourceRoot = resolve(sourceRoot);
  const files = [];
  for (const target of SOURCE_TARGETS) {
    const targetPath = resolve(sourceRoot, target.path);
    const metadata = await requirePath(targetPath, target.kind);
    if (metadata.isDirectory()) files.push(...await listFiles(targetPath));
    else files.push(targetPath);
  }

  const mainPath = resolve(sourceRoot, GENERATED_SOURCE_TARGET);
  let mainAvailable = false;
  try {
    const metadata = await lstat(mainPath);
    if (metadata.isSymbolicLink()) {
      throw new Error(`privacy scan target must not be a symbolic link: ${mainPath}`);
    }
    if (!metadata.isFile()) {
      throw new Error(`privacy scan target must be a file: ${mainPath}`);
    }
    mainAvailable = true;
    files.push(mainPath);
  } catch (error) {
    if (error?.code !== "ENOENT") throw error;
    if (requireMainArtifact) {
      throw new Error(`privacy scan target is missing: ${mainPath}`);
    }
  }

  const findings = await scanFiles(sourceRoot, files, { allowSyntheticFixtures: true });
  const workerRoot = resolve(sourceRoot, "src/worker");
  await requirePath(workerRoot, "directory");
  findings.push(...await scanFiles(sourceRoot, await listFiles(workerRoot), {
    rules: [WORKER_AUTHORITY_RULE],
  }));
  if (mainAvailable) findings.push(...await scanEmbeddedWorkerPrivacy(sourceRoot, mainPath));
  return sortFindings(findings);
}

export async function scanPackagePrivacy(packageRoot) {
  if (!packageRoot) throw new Error("privacy package path is required");
  packageRoot = resolve(packageRoot);
  await requirePath(packageRoot, "directory");
  const files = await listFiles(packageRoot);
  const findings = await scanFiles(packageRoot, files);
  const mainFiles = files.filter((path) => path.endsWith(`${sep}main.js`));
  if (mainFiles.length === 0) throw new Error("privacy package is missing main.js");
  for (const path of mainFiles) {
    findings.push(...await scanEmbeddedWorkerPrivacy(packageRoot, path, {
      allowApprovedLegacyArtifact: true,
    }));
  }
  return sortFindings(findings);
}

export async function assertSourcePrivacy(sourceRoot = defaultSourceRoot, options) {
  const findings = await scanSourcePrivacy(sourceRoot, options);
  if (findings.length !== 0) throw new Error(formatPrivacyFindings("source", findings));
  return {
    sourceRoot: resolve(sourceRoot),
    files: [
      ...SOURCE_TARGETS.map(({ path }) => path),
      ...(options?.requireMainArtifact === false ? [] : [GENERATED_SOURCE_TARGET]),
    ],
  };
}

export async function assertPackagePrivacy(packageRoot) {
  const findings = await scanPackagePrivacy(packageRoot);
  if (findings.length !== 0) throw new Error(formatPrivacyFindings("package", findings));
  return { packageRoot: resolve(packageRoot) };
}

export function formatPrivacyFindings(scope, findings) {
  return [
    `${scope} privacy policy rejected ${findings.length} finding(s):`,
    ...findings.map(({ rule, path, line, match }) =>
      `- ${rule} ${path}:${line}: ${JSON.stringify(match)}`
    ),
  ].join("\n");
}

async function scanEmbeddedWorkerPrivacy(root, path, {
  allowApprovedLegacyArtifact = false,
} = {}) {
  const relativePath = portableRelative(root, path);
  const bytes = await readFile(path);
  const text = utf8.decode(bytes);
  const findings = [];
  let cursor = 0;
  let bundles = 0;
  while (cursor < text.length) {
    const start = text.indexOf(WORKER_BUNDLE_START, cursor);
    const end = text.indexOf(WORKER_BUNDLE_END, cursor);
    if (start === -1 && end === -1) break;
    if (start === -1 || end < start) {
      throw new Error(`embedded Worker privacy markers are malformed: ${relativePath}`);
    }
    const contentStart = start + WORKER_BUNDLE_START.length;
    const contentEnd = text.indexOf(WORKER_BUNDLE_END, contentStart);
    if (contentEnd === -1) {
      throw new Error(`embedded Worker privacy markers are malformed: ${relativePath}`);
    }
    const workerSource = text.slice(contentStart, contentEnd);
    for (const match of workerSource.matchAll(WORKER_AUTHORITY_RULE.pattern)) {
      findings.push({
        rule: WORKER_AUTHORITY_RULE.id,
        path: relativePath,
        line: lineNumberAt(text, contentStart + match.index),
        match: match[0],
      });
    }
    bundles += 1;
    cursor = contentEnd + WORKER_BUNDLE_END.length;
  }
  if (bundles === 0) {
    const sha256 = createHash("sha256").update(bytes).digest("hex");
    if (!allowApprovedLegacyArtifact || !APPROVED_LEGACY_UNMARKED_MAIN_SHA256.has(sha256)) {
      throw new Error(`embedded Worker privacy markers are missing: ${relativePath}`);
    }
  }
  return findings;
}

async function scanFiles(root, files, {
  allowSyntheticFixtures = false,
  rules = CONTENT_RULES,
} = {}) {
  const findings = [];
  for (const path of [...new Set(files)].sort()) {
    const relativePath = portableRelative(root, path);
    const bytes = await readFile(path);
    let text;
    try {
      text = utf8.decode(bytes);
    } catch {
      throw new Error(`privacy scan requires UTF-8 text: ${relativePath}`);
    }
    for (const rule of rules) {
      for (const match of text.matchAll(rule.pattern)) {
        const value = match[0];
        if (allowSyntheticFixtures && isApprovedSyntheticFixture(relativePath, rule.id, value)) {
          continue;
        }
        findings.push({
          rule: rule.id,
          path: relativePath,
          line: lineNumberAt(text, match.index),
          match: value,
        });
      }
    }
  }
  return findings;
}

function isApprovedSyntheticFixture(path, rule, match) {
  return APPROVED_SYNTHETIC_FIXTURES[path]?.[rule]?.has(match) === true;
}

async function listFiles(root) {
  const files = [];
  const entries = await readdir(root, { withFileTypes: true });
  for (const entry of entries.sort((left, right) => left.name.localeCompare(right.name))) {
    const path = resolve(root, entry.name);
    if (entry.isSymbolicLink()) {
      throw new Error(`privacy scan refuses symbolic links: ${portableRelative(root, path)}`);
    }
    if (entry.isDirectory()) files.push(...await listFiles(path));
    else if (entry.isFile()) files.push(path);
  }
  return files;
}

async function requirePath(path, kind) {
  let metadata;
  try {
    metadata = await lstat(path);
  } catch (error) {
    if (error?.code === "ENOENT") throw new Error(`privacy scan target is missing: ${path}`);
    throw error;
  }
  if (metadata.isSymbolicLink()) throw new Error(`privacy scan target must not be a symbolic link: ${path}`);
  if (kind === "file" && !metadata.isFile()) throw new Error(`privacy scan target must be a file: ${path}`);
  if (kind === "directory" && !metadata.isDirectory()) {
    throw new Error(`privacy scan target must be a directory: ${path}`);
  }
  return metadata;
}

function portableRelative(root, path) {
  const value = relative(root, path);
  if (value === "" || value === ".." || value.startsWith(`..${sep}`)) {
    throw new Error(`privacy scan path escapes its root: ${path}`);
  }
  return value.split(sep).join("/");
}

function lineNumberAt(text, index = 0) {
  let line = 1;
  for (let cursor = 0; cursor < index; cursor += 1) {
    if (text.charCodeAt(cursor) === 10) line += 1;
  }
  return line;
}

function sortFindings(findings) {
  return findings.sort((left, right) =>
    left.path.localeCompare(right.path)
      || left.line - right.line
      || left.rule.localeCompare(right.rule)
      || left.match.localeCompare(right.match)
  );
}

if (process.argv[1] === fileURLToPath(import.meta.url)) {
  const [command = "source", root, ...unexpected] = process.argv.slice(2);
  if (unexpected.length !== 0) throw new Error("too many privacy policy arguments");
  if (command === "source") {
    await assertSourcePrivacy(root ? resolve(root) : defaultSourceRoot);
  } else if (command === "package") {
    if (!root) throw new Error("usage: privacy-policy.mjs package <package-root>");
    await assertPackagePrivacy(resolve(root));
  } else {
    throw new Error("unknown privacy policy command");
  }
}
