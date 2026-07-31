// SPDX-FileCopyrightText: 2026 cybersader
// SPDX-License-Identifier: GPL-3.0-only

import { createHash } from "node:crypto";
import {
  copyFile,
  mkdir,
  readFile,
  readdir,
  rm,
  writeFile,
} from "node:fs/promises";
import { dirname, resolve } from "node:path";
import { spawnSync } from "node:child_process";
import { fileURLToPath } from "node:url";

import { buildPlugin } from "../esbuild.config.mjs";

const root = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const defaultConfigPath = resolve(root, "d5c-brat.config.json");
const defaultOutputRoot = resolve(root, "d5c-brat.tmp");
const RUNTIME_FILES = Object.freeze(["main.js", "manifest.json", "styles.css"]);
const SUPPORT_FILES = Object.freeze([
  "Apache-2.0.txt",
  "LICENSE",
  "THIRD_PARTY_NOTICES.md",
  "SHA256SUMS",
  "d5c-balanced-playground.attestation.json",
]);

export async function packageD5cBrat({
  configPath = defaultConfigPath,
  outputRoot = defaultOutputRoot,
  requireClean = true,
  sourceRef,
} = {}) {
  const config = validateConfig(JSON.parse(await readFile(configPath, "utf8")));
  if (sourceRef !== undefined && !isBoundedString(sourceRef, 256)) {
    throw new Error("D5C BRAT source ref is invalid or unbounded");
  }
  const sourceCommit = git(["rev-parse", "HEAD"]);
  const sourceClean = git(["status", "--porcelain", "--untracked-files=all"]) === "";
  if (requireClean && !sourceClean) {
    throw new Error("D5C BRAT publication package requires a clean source tree");
  }
  const sourceTagVerified = sourceRef === config.source.tag
    && git(["tag", "--points-at", "HEAD"]).split("\n").includes(config.source.tag);

  const sourceRevision = sourceTagVerified ? config.source.tag : sourceCommit;
  const sourceUrl = `${config.source.repository}/tree/${sourceRevision}`;
  const buildOptions = {
    write: false,
    production: true,
    internalD5cPlayground: true,
    pluginIdentity: {
      id: config.plugin.id,
      version: config.plugin.version,
    },
    sourceUrl,
    activeVaultCache: config.active_vault_cache,
  };
  const first = await buildPlugin(buildOptions);
  const second = await buildPlugin(buildOptions);
  if (first.mainText !== second.mainText
    || JSON.stringify(first.identities) !== JSON.stringify(second.identities)
    || JSON.stringify(first.internalD5cPlayground?.identities)
      !== JSON.stringify(second.internalD5cPlayground?.identities)) {
    throw new Error("D5C BRAT package build is not deterministic");
  }
  if (first.internalD5cPlayground === null) {
    throw new Error("D5C BRAT package is missing the playground Worker");
  }

  const packageRoot = resolve(outputRoot, config.plugin.version);
  const pluginRoot = resolve(packageRoot, "plugin");
  const supportRoot = resolve(packageRoot, "support");
  await rm(packageRoot, { recursive: true, force: true });
  await mkdir(pluginRoot, { recursive: true });
  await mkdir(supportRoot, { recursive: true });

  const manifestText = `${JSON.stringify(config.plugin, null, 2)}\n`;
  const stylesText = await readFile(resolve(root, "styles.css"), "utf8");
  const runtime = {
    "main.js": Buffer.from(first.mainText, "utf8"),
    "manifest.json": Buffer.from(manifestText, "utf8"),
    "styles.css": Buffer.from(stylesText, "utf8"),
  };
  for (const name of RUNTIME_FILES) {
    await writeFile(resolve(pluginRoot, name), runtime[name]);
  }
  await copyFile(resolve(root, "LICENSE"), resolve(supportRoot, "LICENSE"));
  await copyFile(
    resolve(root, "licenses/Apache-2.0.txt"),
    resolve(supportRoot, "Apache-2.0.txt"),
  );
  const noticeSource = await readFile(resolve(root, "THIRD_PARTY_NOTICES.md"), "utf8");
  const noticeText = noticeSource.replace(
    "[`licenses/Apache-2.0.txt`](licenses/Apache-2.0.txt)",
    "`Apache-2.0.txt`",
  );
  if (noticeText === noticeSource) {
    throw new Error("D5C BRAT notices did not contain the expected Apache license link");
  }
  await writeFile(resolve(supportRoot, "THIRD_PARTY_NOTICES.md"), noticeText);

  const runtimeIdentity = Object.fromEntries(
    RUNTIME_FILES.map((name) => [name, identity(runtime[name])]),
  );
  const attestation = {
    schema_version: 1,
    kind: "kwiry_d5c_balanced_playground_brat_candidate",
    verdict: "FIELD_TEST_CANDIDATE_OWNER_REVIEW_REQUIRED",
    publishable: sourceClean && sourceTagVerified,
    source: {
      repository: config.source.repository,
      tag: config.source.tag,
      commit: sourceCommit,
      tag_verified: sourceTagVerified,
    },
    distribution_repository: config.distribution_repository,
    plugin: {
      id: config.plugin.id,
      version: config.plugin.version,
    },
    build: {
      production: true,
      write: false,
      active_vault_cache: "disabled",
      deterministic: true,
      loose_runtime_assets: 0,
      embedded_workers: 2,
    },
    runtime: runtimeIdentity,
    embedded: {
      production_rust_wasm: first.identities.rust,
      sqlite_wasm: first.identities.sqlite,
      d5c_rust_wasm: first.internalD5cPlayground.identities.rust,
    },
    known_limits: [
      "fixture_only_no_live_connectors",
      "balanced_profile_not_owner_accepted",
      "general_gate5_capacity_regression_tracked_separately",
    ],
  };
  const attestationText = `${JSON.stringify(attestation, null, 2)}\n`;
  await writeFile(
    resolve(supportRoot, "d5c-balanced-playground.attestation.json"),
    attestationText,
  );
  await writeFile(
    resolve(supportRoot, "SHA256SUMS"),
    RUNTIME_FILES.map((name) => `${runtimeIdentity[name].sha256}  ${name}`).join("\n") + "\n",
  );

  const actualRuntime = (await readdir(pluginRoot)).sort();
  const actualSupport = (await readdir(supportRoot)).sort();
  if (JSON.stringify(actualRuntime) !== JSON.stringify([...RUNTIME_FILES].sort())
    || JSON.stringify(actualSupport) !== JSON.stringify([...SUPPORT_FILES].sort())) {
    throw new Error("D5C BRAT package file set is invalid");
  }

  return {
    config,
    packageRoot,
    pluginRoot,
    supportRoot,
    mainText: first.mainText,
    mainMetafile: first.mainMetafile,
    workerSource: first.workerSource,
    workerMetafile: first.workerMetafile,
    playground: first.internalD5cPlayground,
    attestation,
  };
}

function validateConfig(value) {
  const sourceTagMatch = typeof value?.source?.tag === "string"
    ? /^d5c-balanced-playground-([0-9]+\.[0-9]+\.[0-9]+)(?:-r[1-9][0-9]*)?-source$/u.exec(value.source.tag)
    : null;
  if (!isRecord(value)
    || !hasExactKeys(value, [
      "schema_version",
      "plugin",
      "source",
      "distribution_repository",
      "active_vault_cache",
    ])
    || value.schema_version !== 1
    || value.active_vault_cache !== false
    || !isRecord(value.plugin)
    || !hasExactKeys(value.plugin, [
      "id",
      "name",
      "version",
      "minAppVersion",
      "description",
      "author",
      "authorUrl",
      "isDesktopOnly",
    ])
    || value.plugin.id !== "kwiry-d5c-balanced-playground"
    || value.plugin.name !== "Kwiry D5C Balanced Playground"
    || !isSemver(value.plugin.version)
    || !isSemver(value.plugin.minAppVersion)
    || !isBoundedString(value.plugin.description, 256)
    || !isBoundedString(value.plugin.author, 128)
    || value.plugin.authorUrl !== "https://github.com/cybersader/kwiry"
    || value.plugin.isDesktopOnly !== true
    || !isRecord(value.source)
    || !hasExactKeys(value.source, ["repository", "tag"])
    || value.source.repository !== "https://github.com/cybersader/kwiry"
    || sourceTagMatch?.[1] !== value.plugin.version
    || value.distribution_repository !== "cybersader/kwiry-d5c-balanced-playground") {
    throw new Error("D5C BRAT package configuration is invalid");
  }
  return Object.freeze({
    ...value,
    plugin: Object.freeze({ ...value.plugin }),
    source: Object.freeze({ ...value.source }),
  });
}

function git(args) {
  const result = spawnSync("git", args, { cwd: resolve(root, "../.."), encoding: "utf8" });
  if (result.status !== 0) throw new Error("git command failed while packaging D5C BRAT candidate");
  return result.stdout.trim();
}

function identity(value) {
  return {
    bytes: value.byteLength,
    sha256: createHash("sha256").update(value).digest("hex"),
  };
}

function isRecord(value) {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function hasExactKeys(value, keys) {
  const actual = Object.keys(value);
  return actual.length === keys.length && actual.every((key) => keys.includes(key));
}

function isBoundedString(value, maximum) {
  return typeof value === "string" && value.length > 0 && value.length <= maximum;
}

function isSemver(value) {
  return typeof value === "string"
    && /^[0-9]+\.[0-9]+\.[0-9]+(?:-[0-9A-Za-z.-]+)?(?:\+[0-9A-Za-z.-]+)?$/u.test(value);
}

if (process.argv[1] === fileURLToPath(import.meta.url)) {
  const allowDirty = process.argv.slice(2).includes("--allow-dirty");
  const unexpected = process.argv.slice(2).filter((argument) => argument !== "--allow-dirty");
  if (unexpected.length !== 0) throw new Error("unknown D5C BRAT packager argument");
  const sourceRef = process.env.GITHUB_REF_TYPE === "tag"
    ? process.env.GITHUB_REF_NAME
    : undefined;
  const packaged = await packageD5cBrat({
    requireClean: !allowDirty,
    sourceRef,
  });
  process.stdout.write(`${JSON.stringify({
    plugin: packaged.config.plugin.id,
    version: packaged.config.plugin.version,
    publishable: packaged.attestation.publishable,
    runtime: packaged.attestation.runtime,
  })}\n`);
}
