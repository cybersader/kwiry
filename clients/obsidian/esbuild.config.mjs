// SPDX-FileCopyrightText: 2026 cybersader
// SPDX-License-Identifier: GPL-3.0-only

import { createHash } from "node:crypto";
import {
  mkdirSync,
  mkdtempSync,
  readFileSync,
  rmSync,
  writeFileSync,
} from "node:fs";
import { createRequire } from "node:module";
import { dirname, resolve } from "node:path";
import { spawnSync } from "node:child_process";
import { fileURLToPath } from "node:url";

import builtins from "builtin-modules";
import esbuild from "esbuild";

import {
  assertWorkerAuthorityGraph,
  embedWorkerPrivacyBoundary,
} from "./scripts/privacy-policy.mjs";

const root = dirname(fileURLToPath(import.meta.url));
const require = createRequire(import.meta.url);
const cliMode = process.argv[2];
const production = cliMode === "production";
const cliInternalD5cPlayground = cliMode === "d5c";
const rustRoot = resolve(root, "rust/kwiry-obsidian-wasm");
const rustManifest = resolve(rustRoot, "Cargo.toml");
const rustTarget = resolve(rustRoot, "target");
const sqlitePackagePath = require.resolve("@sqlite.org/sqlite-wasm/package.json");
const sqliteWasm = require.resolve("@sqlite.org/sqlite-wasm/sqlite3.wasm");
const d5cFixtureCorpus = resolve(root, "../../fixtures/retrieval/d5c-balanced/corpus.json");

const expectedSqlite = Object.freeze({
  packageVersion: "3.53.0-build1",
  lockIntegrity: "sha512-PfWPWN2n+/37doa8oh2/oUXk4OOsRYZsxc1W1sDXIGb/Pu5Yrb+f2eyYpgQMGITVX7HVgxhs9P18Rc6I97ym/g==",
  wasmBytes: 864_752,
  wasmSha256: "02d7e48164395fa68f81c6ec33e9da5461be397dc57602ac0cd89b4bbba1d312",
});

const PRODUCTION_SOURCE_URL = "https://github.com/cybersader/kwiry (clients/obsidian).";

function buildBanner(sourceUrl) {
  const sourceLine = sourceUrl === PRODUCTION_SOURCE_URL
    ? `Source for this build: ${sourceUrl}`
    : `Corresponding source for this build: ${sourceUrl}`;
  return `/*
Kwiry Search — Obsidian client with explicit daemon and in-plugin lexical profiles.
Copyright (C) 2026 cybersader

This program is free software: you can redistribute it and/or modify
it under the terms of the GNU General Public License as published by
the Free Software Foundation, version 3.

This program is distributed in the hope that it will be useful, but
WITHOUT ANY WARRANTY; without even the implied warranty of
MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE. See the GNU
General Public License for more details: https://www.gnu.org/licenses/gpl-3.0.html

Portions adapted from Omnisearch (https://github.com/scambier/obsidian-omnisearch),
Copyright Simon Cambier and contributors, GPL-3.0.
${sourceLine}
*/`;
}

function run(command, args, cwd = root) {
  const result = spawnSync(command, args, { cwd, encoding: "utf8" });
  if (result.status !== 0) {
    process.stdout.write(result.stdout);
    process.stderr.write(result.stderr);
    throw new Error(`${command} failed`);
  }
  return result.stdout.trim();
}

function sha256(value) {
  return createHash("sha256").update(value).digest("hex");
}

function prepareRustAdapter(variant) {
  const version = run("wasm-bindgen", ["--version"]);
  if (version !== "wasm-bindgen 0.2.126") {
    throw new Error("wasm-bindgen CLI must be exactly 0.2.126");
  }
  if (variant !== "production"
    && variant !== "internal-typo-prototype"
    && variant !== "internal-d5c-preview") {
    throw new Error("unknown Rust adapter variant");
  }
  const variantTarget = resolve(rustTarget, "esbuild", variant);
  const rustRawWasm = resolve(
    variantTarget,
    "wasm32-unknown-unknown/release/kwiry_obsidian_wasm.wasm",
  );
  const packageParent = resolve(rustRoot, `pkg/${variant}`);
  mkdirSync(packageParent, { recursive: true });
  const rustPackage = mkdtempSync(resolve(packageParent, "build-"));
  const buildArgs = [
    "build",
    "--manifest-path",
    rustManifest,
    "--target-dir",
    variantTarget,
    "--target",
    "wasm32-unknown-unknown",
    "--release",
    "--lib",
  ];
  if (variant === "internal-typo-prototype") {
    buildArgs.push("--features", "internal-typo-prototype");
  }
  if (variant === "internal-d5c-preview") {
    buildArgs.push("--features", "internal-d5c-preview");
  }
  try {
    run("cargo", buildArgs);
    run("wasm-bindgen", [
      rustRawWasm,
      "--target",
      "web",
      "--out-dir",
      rustPackage,
      "--out-name",
      "kwiry_obsidian_wasm",
    ]);
    const bindings = resolve(rustPackage, "kwiry_obsidian_wasm.js");
    const wasm = resolve(rustPackage, "kwiry_obsidian_wasm_bg.wasm");
    const bytes = readFileSync(wasm);
    return {
      bindings,
      wasm,
      bytes,
      identity: { bytes: bytes.byteLength, sha256: sha256(bytes) },
      cleanup: () => rmSync(rustPackage, { recursive: true, force: true }),
    };
  } catch (error) {
    rmSync(rustPackage, { recursive: true, force: true });
    throw error;
  }
}

function verifySqliteArtifact() {
  const packageJson = JSON.parse(readFileSync(sqlitePackagePath, "utf8"));
  const packageLock = JSON.parse(readFileSync(resolve(root, "package-lock.json"), "utf8"));
  const lockEntry = packageLock.packages["node_modules/@sqlite.org/sqlite-wasm"];
  const bytes = readFileSync(sqliteWasm);
  const identity = {
    packageVersion: packageJson.version,
    lockIntegrity: lockEntry?.integrity,
    wasmBytes: bytes.byteLength,
    wasmSha256: sha256(bytes),
  };
  for (const [key, expected] of Object.entries(expectedSqlite)) {
    if (identity[key] !== expected) throw new Error(`pinned SQLite artifact mismatch: ${key}`);
  }
  return { bytes: identity.wasmBytes, sha256: identity.wasmSha256 };
}

// The whole export identity envelope must be authored by the Worker that
// produces the image. `manifest.json` is a main-thread file, so its identity is
// read at build time and pinned into the Worker bundle instead of being
// stamped on by the host after the fact.
function readPluginIdentity() {
  const manifest = JSON.parse(readFileSync(resolve(root, "manifest.json"), "utf8"));
  return validatePluginIdentity(manifest);
}

function validatePluginIdentity(value) {
  const bounded = (candidate, maximum) =>
    typeof candidate === "string" && candidate.length > 0 && candidate.length <= maximum;
  if (!value || typeof value !== "object"
    || !bounded(value.id, 128)
    || !/^[a-z0-9][a-z0-9-]*$/u.test(value.id)
    || !bounded(value.version, 64)
    || !/^[0-9]+\.[0-9]+\.[0-9]+(?:-[0-9A-Za-z.-]+)?(?:\+[0-9A-Za-z.-]+)?$/u.test(value.version)) {
    throw new Error("plugin identity is invalid or unbounded");
  }
  return Object.freeze({ id: value.id, version: value.version });
}

function validateSourceUrl(value) {
  if (value === PRODUCTION_SOURCE_URL) return value;
  if (typeof value !== "string"
    || value.length < 1
    || value.length > 1_024
    || !/^https:\/\/github\.com\/[A-Za-z0-9_.-]+\/[A-Za-z0-9_.-]+\/tree\/[A-Za-z0-9._+-]+(?:\/[A-Za-z0-9._/-]+)?$/u.test(value)) {
    throw new Error("corresponding source URL is invalid or unbounded");
  }
  return value;
}

function rustVirtualPlugin(identities, adapter) {
  return {
    name: "kwiry-rust-wasm",
    setup(build) {
      build.onResolve({ filter: /^virtual:kwiry-rust-wasm-bindings$/ }, () => ({
        path: adapter.bindings,
      }));
      build.onResolve({ filter: /^virtual:kwiry-rust-wasm-bytes$/ }, () => ({
        path: adapter.wasm,
      }));
      build.onResolve({ filter: /^virtual:kwiry-artifact-identities$/ }, () => ({
        path: "kwiry-artifact-identities",
        namespace: "kwiry-artifact-identities",
      }));
      build.onLoad({ filter: /.*/, namespace: "kwiry-artifact-identities" }, () => ({
        contents: [
          `export const RUST_WASM_SIZE=${identities.rust.bytes};`,
          `export const RUST_WASM_SHA256=${JSON.stringify(identities.rust.sha256)};`,
          `export const SQLITE_WASM_SIZE=${identities.sqlite.bytes};`,
          `export const SQLITE_WASM_SHA256=${JSON.stringify(identities.sqlite.sha256)};`,
          `export const PLUGIN_ID=${JSON.stringify(identities.plugin.id)};`,
          `export const PLUGIN_VERSION=${JSON.stringify(identities.plugin.version)};`,
        ].join("\n"),
        loader: "js",
      }));
    },
  };
}

function assertWorkerGraph(metafile) {
  const inputs = Object.keys(metafile.inputs).map((input) => input.replaceAll("\\", "/"));
  if (!inputs.some((input) => input.endsWith("@sqlite.org/sqlite-wasm/dist/index.mjs"))) {
    throw new Error("Worker build did not select the SQLite browser entry");
  }
  for (const forbidden of ["/dist/node.mjs", "sqlite3-worker1.mjs", "sqlite3-opfs-async-proxy.js"]) {
    if (inputs.some((input) => input.includes(forbidden))) {
      throw new Error(`Worker build selected forbidden input: ${forbidden}`);
    }
  }
  const wasmInputs = inputs.filter((input) => input.endsWith(".wasm"));
  if (wasmInputs.length !== 2
    || !wasmInputs.some((input) => input.endsWith("sqlite3.wasm"))
    || !wasmInputs.some((input) => input.endsWith("kwiry_obsidian_wasm_bg.wasm"))) {
    throw new Error(`Worker build expected exactly two WASM inputs, got ${wasmInputs.length}`);
  }
}

function assertD5cPlaygroundGraph(metafile) {
  const inputs = Object.keys(metafile.inputs).map((input) => input.replaceAll("\\", "/"));
  if (!inputs.some((input) => input.endsWith("src/worker/d5c-playground-worker.ts"))
    || !inputs.some((input) => input.endsWith("src/worker/d5c-evaluation.ts"))) {
    throw new Error("D5C playground Worker omitted its explicit entry or fixture evaluator");
  }
  if (inputs.some((input) => input.includes("@sqlite.org/sqlite-wasm")
    || input.endsWith("src/worker/worker.ts"))) {
    throw new Error("D5C playground Worker selected a production Worker input");
  }
  const wasmInputs = inputs.filter((input) => input.endsWith(".wasm"));
  if (wasmInputs.length !== 1
    || !wasmInputs[0].endsWith("kwiry_obsidian_wasm_bg.wasm")
    || !wasmInputs[0].includes("pkg/internal-d5c-preview")) {
    throw new Error("D5C playground Worker expected only the preview Rust WASM input");
  }
}

function assertD5cOwnerWorkerGraph(metafile) {
  const inputs = Object.keys(metafile.inputs).map((input) => input.replaceAll("\\", "/"));
  for (const required of [
    "src/worker/worker.ts",
    "src/worker/d5c-preview.ts",
    "src/worker/d5c-compare-protocol.ts",
    "src/worker/d5c-owner-protocol.ts",
  ]) {
    if (!inputs.some((input) => input.endsWith(required))) {
      throw new Error(`D5C owner Worker omitted required input: ${required}`);
    }
  }
  for (const forbidden of [
    "src/worker/d5c-playground-worker.ts",
    "src/worker/d5c-evaluation.ts",
    "src/worker/block-vfs.ts",
    "src/worker/image-header.ts",
  ]) {
    if (inputs.some((input) => input.endsWith(forbidden))) {
      throw new Error(`D5C owner Worker selected fixture input: ${forbidden}`);
    }
  }
  const rustWasm = inputs.find((input) => input.endsWith("kwiry_obsidian_wasm_bg.wasm"));
  if (!rustWasm?.includes("pkg/internal-d5c-preview")) {
    throw new Error("D5C owner Worker did not select preview Rust WASM");
  }
}

function assertD5cOwnerMainGraph(metafile) {
  const inputs = Object.keys(metafile.inputs).map((input) => input.replaceAll("\\", "/"));
  if (!inputs.some((input) => input.endsWith("src/internal/d5c-playground/live-main.ts"))) {
    throw new Error("D5C owner build omitted its dedicated entry point");
  }
  const forbidden = [
    "src/main.ts",
    "src/api.ts",
    "src/backend-manager.ts",
    "src/backends/daemon-backend.ts",
    "src/credentials.ts",
    "src/settings.ts",
    "src/settings-tab.ts",
    "src/internal/private-tools.ts",
    "src/internal/d5c-playground/index.ts",
    "src/internal/d5c-playground/modal.ts",
    "src/internal/d5c-playground/session.ts",
    "src/internal/d5c-playground/settings.ts",
    "src/worker/d5c-playground-worker.ts",
  ];
  for (const input of inputs) {
    if (forbidden.some((path) => input.endsWith(path)) || input.includes("/src/cache/")) {
      throw new Error(`D5C owner build selected forbidden input: ${input}`);
    }
  }
}

function internalPrototypePlugin(enabled) {
  return {
    name: "kwiry-internal-prototype",
    setup(build) {
      build.onResolve({ filter: /^virtual:kwiry-internal-prototype$/ }, () => enabled
        ? { path: resolve(root, "src/worker/typo-prototype.ts") }
        : { path: "kwiry-disabled-internal-prototype", namespace: "kwiry-disabled-internal-prototype" });
      build.onLoad({ filter: /.*/, namespace: "kwiry-disabled-internal-prototype" }, () => ({
        contents: "export const createInternalPrototypeHandler=()=>async()=>false;",
        loader: "js",
      }));
    },
  };
}

function internalD5cPreviewPlugin(enabled) {
  return {
    name: "kwiry-internal-d5c-preview",
    setup(build) {
      build.onResolve({ filter: /^virtual:kwiry-internal-d5c-preview$/ }, () => enabled
        ? { path: resolve(root, "src/worker/d5c-preview.ts") }
        : {
            path: "kwiry-disabled-internal-d5c-preview",
            namespace: "kwiry-disabled-internal-d5c-preview",
          });
      build.onLoad({ filter: /.*/, namespace: "kwiry-disabled-internal-d5c-preview" }, () => ({
        contents: "export const createInternalD5cPreviewHandler=()=>async()=>false;",
        loader: "js",
      }));
    },
  };
}

function ownerWorkerProtocolPlugin(enabled) {
  return {
    name: "kwiry-owner-worker-protocol",
    setup(build) {
      build.onResolve({ filter: /^virtual:kwiry-owner-worker-protocol$/ }, () => enabled
        ? { path: resolve(root, "src/worker/d5c-owner-protocol.ts") }
        : {
            path: "kwiry-disabled-owner-worker-protocol",
            namespace: "kwiry-disabled-owner-worker-protocol",
          });
      build.onLoad({
        filter: /.*/,
        namespace: "kwiry-disabled-owner-worker-protocol",
      }, () => ({
        contents: [
          "export const isD5cOwnerWorkerOperation=()=>false;",
          "export const parseD5cOwnerWorkerRequest=()=>null;",
        ].join("\n"),
        loader: "js",
      }));
    },
  };
}

function ownerWorkerCapabilityPlugin(enabled) {
  return {
    name: "kwiry-owner-worker-capabilities",
    setup(build) {
      if (!enabled) return;
      build.onResolve({ filter: /^\.\/block-vfs$/ }, () => ({
        path: "kwiry-disabled-block-vfs",
        namespace: "kwiry-disabled-worker-capability",
      }));
      build.onResolve({ filter: /^\.\/image-header$/ }, () => ({
        path: "kwiry-disabled-image-header",
        namespace: "kwiry-disabled-worker-capability",
      }));
      build.onLoad({
        filter: /^kwiry-disabled-block-vfs$/,
        namespace: "kwiry-disabled-worker-capability",
      }, () => ({
        contents: [
          "export class BlockVfsUnavailableError extends Error{}",
          "export function openPlainBlockVfs(){throw new BlockVfsUnavailableError();}",
        ].join("\n"),
        loader: "js",
      }));
      build.onLoad({
        filter: /^kwiry-disabled-image-header$/,
        namespace: "kwiry-disabled-worker-capability",
      }, () => ({
        contents: "export function validateSQLiteImage(){throw new Error('unavailable');}",
        loader: "js",
      }));
    },
  };
}

function workerSourcePlugin(workerSource) {
  const embeddedWorkerSource = embedWorkerPrivacyBoundary(workerSource);
  return {
    name: "kwiry-worker-source",
    setup(build) {
      build.onResolve({ filter: /^virtual:kwiry-worker-source$/ }, () => ({
        path: "kwiry-worker-source",
        namespace: "kwiry-worker-source",
      }));
      build.onLoad({ filter: /.*/, namespace: "kwiry-worker-source" }, () => ({
        contents: `export default ${JSON.stringify(embeddedWorkerSource)};`,
        loader: "js",
      }));
    },
  };
}

function cacheProfilePlugin(enabled) {
  return {
    name: "kwiry-cache-profile",
    setup(build) {
      build.onResolve({ filter: /^\.\/cache\/build-cache-options$/ }, () => enabled
        ? undefined
        : { path: "kwiry-disabled-cache-profile", namespace: "kwiry-disabled-cache-profile" });
      build.onLoad({ filter: /.*/, namespace: "kwiry-disabled-cache-profile" }, () => ({
        contents: "export function createInPluginCacheOptions(){return undefined;}",
        loader: "js",
      }));
    },
  };
}

function privateToolsPlugin(enabled, workerSource) {
  return {
    name: "kwiry-private-tools",
    setup(build) {
      build.onResolve({ filter: /^\.\/internal\/private-tools$/ }, () => enabled
        ? { path: "kwiry-private-tools", namespace: "kwiry-private-tools" }
        : undefined);
      build.onResolve({ filter: /^\//, namespace: "kwiry-private-tools" }, (args) => ({
        path: args.path,
        namespace: "file",
      }));
      build.onLoad({ filter: /.*/, namespace: "kwiry-private-tools" }, () => {
        if (typeof workerSource !== "string") {
          throw new Error("private playground Worker source is unavailable");
        }
        const corpus = JSON.parse(readFileSync(d5cFixtureCorpus, "utf8"));
        const embeddedWorkerSource = embedWorkerPrivacyBoundary(workerSource);
        return {
          contents: [
            `import { createD5cPlaygroundTools } from ${JSON.stringify(resolve(root, "src/internal/d5c-playground/index.ts"))};`,
            `const workerSource=${JSON.stringify(embeddedWorkerSource)};`,
            `const fixtureCorpus=${JSON.stringify(corpus)};`,
            "export function createPrivateTools(plugin,stored){",
            "return createD5cPlaygroundTools(plugin,stored,{workerSource,fixtureCorpus});",
            "}",
          ].join("\n"),
          loader: "js",
        };
      });
    },
  };
}

export async function buildPlugin({
  write = true,
  production: optimized = true,
  internalTypoPrototype = false,
  internalD5cPlayground = false,
  internalD5cOwnerHost = false,
  pluginIdentity,
  sourceUrl = PRODUCTION_SOURCE_URL,
  activeVaultCache = true,
} = {}) {
  const internalVariants = [
    internalTypoPrototype,
    internalD5cPlayground,
    internalD5cOwnerHost,
  ].filter(Boolean).length;
  if (internalVariants > 1) {
    throw new Error("internal Worker variants must be built separately");
  }
  if (internalD5cOwnerHost && activeVaultCache) {
    throw new Error("D5C owner host must compile without the active-vault cache");
  }
  if (typeof activeVaultCache !== "boolean") {
    throw new Error("activeVaultCache must be boolean");
  }
  const resolvedPluginIdentity = pluginIdentity === undefined
    ? readPluginIdentity()
    : validatePluginIdentity(pluginIdentity);
  const resolvedSourceUrl = validateSourceUrl(sourceUrl);
  const sqliteIdentity = verifySqliteArtifact();
  const rustAdapter = prepareRustAdapter(
    internalTypoPrototype
      ? "internal-typo-prototype"
      : internalD5cOwnerHost
        ? "internal-d5c-preview"
        : "production",
  );
  const identities = {
    rust: rustAdapter.identity,
    sqlite: sqliteIdentity,
    plugin: resolvedPluginIdentity,
  };
  let workerBuild;
  let workerSource;
  try {
    workerBuild = await esbuild.build({
      absWorkingDir: root,
      entryPoints: ["src/worker/worker.ts"],
      bundle: true,
      platform: "browser",
      format: "iife",
      target: "es2022",
      write: false,
      minify: optimized,
      sourcemap: false,
      legalComments: "inline",
      define: {
        __KWIRY_D5C_OWNER_WORKER__: JSON.stringify(Boolean(internalD5cOwnerHost)),
      },
      loader: { ".wasm": "binary" },
      metafile: true,
      logLevel: "silent",
      plugins: [
        rustVirtualPlugin(identities, rustAdapter),
        internalPrototypePlugin(Boolean(internalTypoPrototype)),
        internalD5cPreviewPlugin(Boolean(internalD5cOwnerHost)),
        ownerWorkerProtocolPlugin(Boolean(internalD5cOwnerHost)),
        ownerWorkerCapabilityPlugin(Boolean(internalD5cOwnerHost)),
      ],
    });
    if (workerBuild.outputFiles.length !== 1) {
      throw new Error(`Worker build emitted ${workerBuild.outputFiles.length} files`);
    }
    assertWorkerGraph(workerBuild.metafile);
    await assertWorkerAuthorityGraph(root, workerBuild.metafile);
    if (internalD5cOwnerHost) assertD5cOwnerWorkerGraph(workerBuild.metafile);
    workerSource = workerBuild.outputFiles[0].text;
    if (/\bimport\s*\(|\bimportScripts\s*\(/u.test(workerSource)) {
      throw new Error("Worker bundle contains a runtime import");
    }
  } finally {
    rustAdapter.cleanup();
  }

  let playgroundBuild = null;
  let playgroundIdentities = null;
  let playgroundSource = null;
  if (internalD5cPlayground) {
    const playgroundAdapter = prepareRustAdapter("internal-d5c-preview");
    playgroundIdentities = {
      rust: playgroundAdapter.identity,
      sqlite: identities.sqlite,
      plugin: identities.plugin,
    };
    try {
      playgroundBuild = await esbuild.build({
        absWorkingDir: root,
        entryPoints: ["src/worker/d5c-playground-worker.ts"],
        bundle: true,
        platform: "browser",
        format: "iife",
        target: "es2022",
        write: false,
        minify: optimized,
        sourcemap: false,
        legalComments: "inline",
        loader: { ".wasm": "binary" },
        metafile: true,
        logLevel: "silent",
        plugins: [rustVirtualPlugin(playgroundIdentities, playgroundAdapter)],
      });
      if (playgroundBuild.outputFiles.length !== 1) {
        throw new Error(`D5C playground Worker build emitted ${playgroundBuild.outputFiles.length} files`);
      }
      assertD5cPlaygroundGraph(playgroundBuild.metafile);
      await assertWorkerAuthorityGraph(root, playgroundBuild.metafile);
      playgroundSource = playgroundBuild.outputFiles[0].text;
      if (/\bimport\s*\(|\bimportScripts\s*\(/u.test(playgroundSource)) {
        throw new Error("D5C playground Worker bundle contains a runtime import");
      }
    } finally {
      playgroundAdapter.cleanup();
    }
    if (write) {
      writeFileSync(
        resolve(rustRoot, "pkg/internal-d5c-preview/internal-d5c-playground-worker.js"),
        playgroundSource,
      );
    }
  }

  const mainBuild = await esbuild.build({
    absWorkingDir: root,
    entryPoints: [internalD5cOwnerHost
      ? "src/internal/d5c-playground/live-main.ts"
      : "src/main.ts"],
    bundle: true,
    platform: "browser",
    external: ["obsidian", "electron", ...builtins],
    format: "cjs",
    target: "es2022",
    write,
    logLevel: write ? "info" : "silent",
    minify: optimized,
    sourcemap: optimized ? false : "inline",
    treeShaking: true,
    outfile: "main.js",
    legalComments: "inline",
    banner: { js: buildBanner(resolvedSourceUrl) },
    metafile: true,
    plugins: [
      cacheProfilePlugin(activeVaultCache),
      workerSourcePlugin(workerSource),
      privateToolsPlugin(Boolean(internalD5cPlayground), playgroundSource),
    ],
  });
  if (internalD5cOwnerHost) assertD5cOwnerMainGraph(mainBuild.metafile);
  const mainText = write
    ? readFileSync(resolve(root, "main.js"), "utf8")
    : mainBuild.outputFiles[0].text;
  return {
    identities,
    rustArtifactBytes: rustAdapter.bytes,
    workerSource,
    workerMetafile: workerBuild.metafile,
    mainText,
    mainMetafile: mainBuild.metafile,
    buildProfile: Object.freeze({
      production: optimized,
      write,
      activeVaultCache,
      internalD5cOwnerHost: Boolean(internalD5cOwnerHost),
      sourceUrl: resolvedSourceUrl,
      plugin: resolvedPluginIdentity,
    }),
    internalD5cPlayground: playgroundBuild === null ? null : {
      identities: playgroundIdentities,
      workerSource: playgroundSource,
      workerMetafile: playgroundBuild.metafile,
    },
  };
}

if (process.argv[1] === fileURLToPath(import.meta.url)) {
  await buildPlugin({
    write: true,
    production,
    internalD5cPlayground: cliInternalD5cPlayground,
  });
}
