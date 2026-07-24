// SPDX-FileCopyrightText: 2026 cybersader
// SPDX-License-Identifier: GPL-3.0-only

import { createHash } from "node:crypto";
import { existsSync, readFileSync, readdirSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

import { buildPlugin } from "../esbuild.config.mjs";

const root = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const first = await buildPlugin({ write: false, production: true });
const second = await buildPlugin({ write: false, production: true });

function sha256(value) {
  return createHash("sha256").update(value).digest("hex");
}

function check(name, passed) {
  if (!passed) throw new Error(`Gate 4 evidence check failed: ${name}`);
  return { name, passed: true };
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
const mainHash = sha256(first.mainText);
const workerHash = sha256(first.workerSource);

const checks = [
  check("deterministic main.js", first.mainText === second.mainText),
  check("deterministic Worker", first.workerSource === second.workerSource),
  check("exactly two WASM graph inputs", wasmInputs.length === 2),
  check("SQLite WASM graph input", wasmInputs.some((input) => input.endsWith("sqlite3.wasm"))),
  check(
    "portable Rust WASM graph input",
    wasmInputs.some((input) => input.endsWith("kwiry_obsidian_wasm_bg.wasm")),
  ),
  check("one-file runtime artifact", looseRuntimeAssets.length === 0),
  check("GPL banner", first.mainText.slice(0, 2_000).includes("GNU General Public License")),
  check("manifest/package version agreement", manifest.version === packageJson.version),
  check("no source map", !first.mainText.includes("sourceMappingURL=")),
  check("no runtime import", !/\bimport\s*\(|\bimportScripts\s*\(/u.test(first.workerSource)),
  check("no private path in artifact", !privatePattern.test(first.mainText)),
  check("third-party notices present", existsSync(resolve(root, "THIRD_PARTY_NOTICES.md"))),
  check("embedded daemon profile", first.mainText.includes("Daemon")),
  check("embedded in-plugin profile", first.mainText.includes("In-plugin")),
];

console.log(JSON.stringify({
  schema_version: 1,
  verdict: "READY_FOR_OWNER_REVIEW",
  checks: {
    total: checks.length,
    failed: 0,
  },
  artifact: {
    bytes: Buffer.byteLength(first.mainText),
    sha256: mainHash,
    deterministic: true,
    loose_runtime_assets: 0,
  },
  worker: {
    bytes: Buffer.byteLength(first.workerSource),
    sha256: workerHash,
    deterministic: true,
    wasm_inputs: 2,
    protocol_version: 1,
  },
  rust_wasm: {
    bytes: first.identities.rust.bytes,
    sha256: first.identities.rust.sha256,
  },
  sqlite_wasm: {
    bytes: first.identities.sqlite.bytes,
    sha256: first.identities.sqlite.sha256,
  },
}));
