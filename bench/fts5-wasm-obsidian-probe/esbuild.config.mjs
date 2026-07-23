// SPDX-FileCopyrightText: 2026 cybersader
// SPDX-License-Identifier: GPL-3.0-only
import { createHash } from "node:crypto";
import { readFileSync } from "node:fs";
import { createRequire } from "node:module";
import { dirname, relative, resolve } from "node:path";
import { fileURLToPath } from "node:url";

import builtins from "builtin-modules";
import esbuild from "esbuild";

const root = dirname(fileURLToPath(import.meta.url));
const require = createRequire(import.meta.url);
const expected = Object.freeze({
  packageVersion: "3.53.0-build1",
  lockIntegrity: "sha512-PfWPWN2n+/37doa8oh2/oUXk4OOsRYZsxc1W1sDXIGb/Pu5Yrb+f2eyYpgQMGITVX7HVgxhs9P18Rc6I97ym/g==",
  wasmBytes: 864_752,
  wasmSha256: "02d7e48164395fa68f81c6ec33e9da5461be397dc57602ac0cd89b4bbba1d312",
});

const banner = `/*
Kwiry FTS5-WASM Compatibility Probe
Copyright (C) 2026 cybersader

This program is free software: you can redistribute it and/or modify
it under the terms of the GNU General Public License as published by
the Free Software Foundation, version 3.

This program is distributed in the hope that it will be useful, but
WITHOUT ANY WARRANTY; without even the implied warranty of
MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE. See the GNU
General Public License for more details: https://www.gnu.org/licenses/gpl-3.0.html

This synthetic probe does not read or search an Obsidian vault.
Source: https://github.com/cybersader/kwiry (bench/fts5-wasm-obsidian-probe).
*/`;

function sha256(value) {
  return createHash("sha256").update(value).digest("hex");
}

export function verifyPinnedArtifact() {
  const packagePath = require.resolve("@sqlite.org/sqlite-wasm/package.json");
  const wasmPath = require.resolve("@sqlite.org/sqlite-wasm/sqlite3.wasm");
  const packageJson = JSON.parse(readFileSync(packagePath, "utf8"));
  const packageLock = JSON.parse(readFileSync(resolve(root, "package-lock.json"), "utf8"));
  const lockEntry = packageLock.packages["node_modules/@sqlite.org/sqlite-wasm"];
  const wasm = readFileSync(wasmPath);
  const identity = {
    packageVersion: packageJson.version,
    lockIntegrity: lockEntry?.integrity,
    wasmBytes: wasm.byteLength,
    wasmSha256: sha256(wasm),
  };

  for (const key of Object.keys(expected)) {
    if (identity[key] !== expected[key]) {
      throw new Error(`pinned SQLite artifact mismatch: ${key}`);
    }
  }

  return { ...identity, wasmPath: relative(root, wasmPath) };
}

function assertWorkerGraph(metafile) {
  const inputs = Object.keys(metafile.inputs).map((input) => input.replaceAll("\\", "/"));
  if (!inputs.some((input) => input.endsWith("@sqlite.org/sqlite-wasm/dist/index.mjs"))) {
    throw new Error("worker build did not select the SQLite browser entry");
  }
  for (const forbidden of ["/dist/node.mjs", "sqlite3-worker1.mjs", "sqlite3-opfs-async-proxy.js"]) {
    if (inputs.some((input) => input.includes(forbidden))) {
      throw new Error(`worker build selected forbidden input: ${forbidden}`);
    }
  }
}

export async function buildProbe({ write = true, production = true } = {}) {
  const identity = verifyPinnedArtifact();
  const workerBuild = await esbuild.build({
    absWorkingDir: root,
    entryPoints: ["src/worker.ts"],
    bundle: true,
    platform: "browser",
    format: "iife",
    target: "es2022",
    write: false,
    minify: production,
    sourcemap: false,
    legalComments: "inline",
    loader: { ".wasm": "binary" },
    metafile: true,
    logLevel: "silent",
  });

  if (workerBuild.outputFiles.length !== 1) {
    throw new Error(`worker build emitted ${workerBuild.outputFiles.length} files`);
  }
  assertWorkerGraph(workerBuild.metafile);
  const workerSource = workerBuild.outputFiles[0].text;
  if (/\bimport\s*\(|\bimportScripts\s*\(/u.test(workerSource)) {
    throw new Error("worker bundle contains a runtime import");
  }

  const workerSourcePlugin = {
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

  const mainBuild = await esbuild.build({
    absWorkingDir: root,
    entryPoints: ["src/main.ts"],
    bundle: true,
    platform: "browser",
    format: "cjs",
    target: "es2022",
    external: ["obsidian", "electron", ...builtins],
    write,
    outfile: "main.js",
    minify: production,
    sourcemap: false,
    treeShaking: true,
    legalComments: "inline",
    banner: { js: banner },
    metafile: true,
    plugins: [workerSourcePlugin],
    logLevel: write ? "info" : "silent",
  });

  const mainText = write ? readFileSync(resolve(root, "main.js"), "utf8") : mainBuild.outputFiles[0].text;
  return {
    identity,
    workerSource,
    workerBytes: Buffer.byteLength(workerSource),
    workerSha256: sha256(workerSource),
    mainText,
    mainBytes: Buffer.byteLength(mainText),
    mainSha256: sha256(mainText),
    workerMetafile: workerBuild.metafile,
    mainMetafile: mainBuild.metafile,
  };
}

if (process.argv[1] === fileURLToPath(import.meta.url)) {
  await buildProbe({ write: true, production: process.argv[2] === "production" });
}
