// SPDX-FileCopyrightText: 2026 cybersader
// SPDX-License-Identifier: GPL-3.0-only

import { createHash } from "node:crypto";
import { readFileSync, rmSync } from "node:fs";
import { createRequire } from "node:module";
import { dirname, resolve } from "node:path";
import { spawnSync } from "node:child_process";
import { fileURLToPath } from "node:url";

import builtins from "builtin-modules";
import esbuild from "esbuild";

const root = dirname(fileURLToPath(import.meta.url));
const require = createRequire(import.meta.url);
const production = process.argv[2] === "production";
const rustRoot = resolve(root, "rust/kwiry-obsidian-wasm");
const rustManifest = resolve(rustRoot, "Cargo.toml");
const rustTarget = resolve(rustRoot, "target");
const rustRawWasm = resolve(
  rustTarget,
  "wasm32-unknown-unknown/release/kwiry_obsidian_wasm.wasm",
);
const sqlitePackagePath = require.resolve("@sqlite.org/sqlite-wasm/package.json");
const sqliteWasm = require.resolve("@sqlite.org/sqlite-wasm/sqlite3.wasm");

const expectedSqlite = Object.freeze({
  packageVersion: "3.53.0-build1",
  lockIntegrity: "sha512-PfWPWN2n+/37doa8oh2/oUXk4OOsRYZsxc1W1sDXIGb/Pu5Yrb+f2eyYpgQMGITVX7HVgxhs9P18Rc6I97ym/g==",
  wasmBytes: 864_752,
  wasmSha256: "02d7e48164395fa68f81c6ec33e9da5461be397dc57602ac0cd89b4bbba1d312",
});

const banner = `/*
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
Source for this build: https://github.com/cybersader/kwiry (clients/obsidian).
*/`;

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

function prepareRustAdapter(internalTypoPrototype) {
  const version = run("wasm-bindgen", ["--version"]);
  if (version !== "wasm-bindgen 0.2.126") {
    throw new Error("wasm-bindgen CLI must be exactly 0.2.126");
  }
  const variant = internalTypoPrototype ? "internal-typo-prototype" : "production";
  const rustPackage = resolve(rustRoot, `pkg/${variant}`);
  const buildArgs = [
    "build",
    "--manifest-path",
    rustManifest,
    "--target",
    "wasm32-unknown-unknown",
    "--release",
    "--lib",
  ];
  if (internalTypoPrototype) buildArgs.push("--features", "internal-typo-prototype");
  run("cargo", buildArgs);
  rmSync(rustPackage, { recursive: true, force: true });
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
    identity: { bytes: bytes.byteLength, sha256: sha256(bytes) },
  };
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
  const bounded = (value, maximum) =>
    typeof value === "string" && value.length > 0 && value.length <= maximum;
  if (!bounded(manifest.id, 128) || !bounded(manifest.version, 64)) {
    throw new Error("plugin manifest identity is missing or unbounded");
  }
  return { id: manifest.id, version: manifest.version };
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

function workerSourcePlugin(workerSource) {
  return {
    name: "kwiry-worker-source",
    setup(build) {
      build.onResolve({ filter: /^virtual:kwiry-worker-source$/ }, () => ({
        path: "kwiry-worker-source",
        namespace: "kwiry-worker-source",
      }));
      build.onLoad({ filter: /.*/, namespace: "kwiry-worker-source" }, () => ({
        contents: `export default ${JSON.stringify(workerSource)};`,
        loader: "js",
      }));
    },
  };
}

export async function buildPlugin({
  write = true,
  production: optimized = true,
  internalTypoPrototype = false,
} = {}) {
  const rustAdapter = prepareRustAdapter(Boolean(internalTypoPrototype));
  const identities = {
    rust: rustAdapter.identity,
    sqlite: verifySqliteArtifact(),
    plugin: readPluginIdentity(),
  };
  const workerBuild = await esbuild.build({
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
    loader: { ".wasm": "binary" },
    metafile: true,
    logLevel: "silent",
    plugins: [
      rustVirtualPlugin(identities, rustAdapter),
      internalPrototypePlugin(Boolean(internalTypoPrototype)),
    ],
  });
  if (workerBuild.outputFiles.length !== 1) {
    throw new Error(`Worker build emitted ${workerBuild.outputFiles.length} files`);
  }
  assertWorkerGraph(workerBuild.metafile);
  const workerSource = workerBuild.outputFiles[0].text;
  if (/\bimport\s*\(|\bimportScripts\s*\(/u.test(workerSource)) {
    throw new Error("Worker bundle contains a runtime import");
  }

  const mainBuild = await esbuild.build({
    absWorkingDir: root,
    entryPoints: ["src/main.ts"],
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
    banner: { js: banner },
    metafile: true,
    plugins: [workerSourcePlugin(workerSource)],
  });
  const mainText = write
    ? readFileSync(resolve(root, "main.js"), "utf8")
    : mainBuild.outputFiles[0].text;
  return {
    identities,
    workerSource,
    workerMetafile: workerBuild.metafile,
    mainText,
    mainMetafile: mainBuild.metafile,
  };
}

if (process.argv[1] === fileURLToPath(import.meta.url)) {
  await buildPlugin({ write: true, production });
}
