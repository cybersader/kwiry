// SPDX-FileCopyrightText: 2026 cybersader
// SPDX-License-Identifier: GPL-3.0-only

import { createHash } from "node:crypto";
import { readFile } from "node:fs/promises";
import { dirname, resolve } from "node:path";
import { spawnSync } from "node:child_process";
import { fileURLToPath } from "node:url";

const root = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const manifestPath = resolve(root, "rust/kwiry-obsidian-wasm/Cargo.toml");
const inventoryPath = resolve(root, "d5c-rust-license-inventory.json");
const noticeBundlePath = resolve(root, "licenses/Rust-DEPENDENCY-LICENSES.md");
const TARGET = "wasm32-unknown-unknown";
const FEATURES = Object.freeze(["internal-d5c-preview"]);

export async function readD5cRustLicenseInventory() {
  const [inventoryText, noticeBundle] = await Promise.all([
    readFile(inventoryPath, "utf8"),
    readFile(noticeBundlePath),
  ]);
  const inventory = JSON.parse(inventoryText);
  validateInventoryShape(inventory);
  if (createHash("sha256").update(noticeBundle).digest("hex")
    !== inventory.notice_bundle_sha256) {
    throw new Error("D5C Rust dependency notice bundle does not match its inventory");
  }
  return Object.freeze({
    schema_version: inventory.schema_version,
    target: inventory.target,
    features: Object.freeze([...inventory.features]),
    notice_bundle_sha256: inventory.notice_bundle_sha256,
    dependencies: Object.freeze(
      inventory.dependencies.map((dependency) => Object.freeze({ ...dependency })),
    ),
  });
}

export async function validateD5cRustLicenses() {
  const inventory = await readD5cRustLicenseInventory();
  const result = spawnSync("cargo", [
    "metadata",
    "--locked",
    "--manifest-path",
    manifestPath,
    "--format-version",
    "1",
    "--features",
    FEATURES.join(","),
    "--filter-platform",
    TARGET,
  ], {
    cwd: root,
    encoding: "utf8",
    maxBuffer: 64 * 1024 * 1024,
  });
  if (result.status !== 0) {
    throw new Error("Cargo metadata failed while validating D5C Rust licenses");
  }

  const metadata = JSON.parse(result.stdout);
  const rootPackage = metadata.packages.find(
    (candidate) => resolve(candidate.manifest_path) === manifestPath,
  );
  if (rootPackage === undefined) {
    throw new Error("D5C Rust adapter is absent from Cargo metadata");
  }

  const packagesById = new Map(metadata.packages.map((candidate) => [candidate.id, candidate]));
  const nodesById = new Map(metadata.resolve.nodes.map((node) => [node.id, node]));
  const pending = [rootPackage.id];
  const packageIds = new Set();
  while (pending.length > 0) {
    const packageId = pending.pop();
    if (packageIds.has(packageId)) continue;
    packageIds.add(packageId);
    const node = nodesById.get(packageId);
    if (node === undefined) {
      throw new Error("D5C Rust dependency is absent from the resolved Cargo graph");
    }
    pending.push(...node.dependencies);
  }

  const actual = [...packageIds].map((packageId) => {
    const candidate = packagesById.get(packageId);
    if (candidate === undefined) {
      throw new Error("D5C Rust dependency metadata is incomplete");
    }
    if (typeof candidate.license !== "string" || candidate.license.length === 0) {
      throw new Error(`D5C Rust dependency has no declared license: ${candidate.name}`);
    }
    return {
      name: candidate.name,
      version: candidate.version,
      declared_license: candidate.license,
      release_license: selectReleaseLicense(candidate),
    };
  }).sort(compareDependencies);

  if (JSON.stringify(actual) !== JSON.stringify(inventory.dependencies)) {
    throw new Error("D5C Rust dependency license inventory does not match Cargo.lock");
  }
  return inventory;
}

function validateInventoryShape(inventory) {
  if (!isRecord(inventory)
    || !hasExactKeys(inventory, [
      "schema_version",
      "target",
      "features",
      "notice_bundle_sha256",
      "dependencies",
    ])
    || inventory.schema_version !== 1
    || inventory.target !== TARGET
    || JSON.stringify(inventory.features) !== JSON.stringify(FEATURES)
    || !/^[0-9a-f]{64}$/u.test(inventory.notice_bundle_sha256)
    || !Array.isArray(inventory.dependencies)
    || inventory.dependencies.length === 0) {
    throw new Error("D5C Rust dependency license inventory is invalid");
  }

  let previous = null;
  for (const dependency of inventory.dependencies) {
    if (!isRecord(dependency)
      || !hasExactKeys(dependency, [
        "name",
        "version",
        "declared_license",
        "release_license",
      ])
      || !isBoundedString(dependency.name, 128)
      || !isBoundedString(dependency.version, 64)
      || !isBoundedString(dependency.declared_license, 256)
      || !isBoundedString(dependency.release_license, 128)
      || selectReleaseLicense({
        name: dependency.name,
        version: dependency.version,
        license: dependency.declared_license,
      }) !== dependency.release_license) {
      throw new Error("D5C Rust dependency license entry is invalid");
    }
    if (previous !== null && compareDependencies(previous, dependency) >= 0) {
      throw new Error("D5C Rust dependency license inventory is not uniquely sorted");
    }
    previous = dependency;
  }
}

function selectReleaseLicense(candidate) {
  if (candidate.name === "kwiry-obsidian-wasm") {
    requirePackage(candidate, "0.1.0", "GPL-3.0-only");
    return "GPL-3.0-only";
  }
  if (candidate.name === "encoding_rs") {
    requirePackage(candidate, "0.8.35", "(Apache-2.0 OR MIT) AND BSD-3-Clause");
    return "Apache-2.0 AND BSD-3-Clause";
  }
  if (candidate.name === "unicode-ident") {
    requirePackage(candidate, "1.0.24", "(MIT OR Apache-2.0) AND Unicode-3.0");
    return "Apache-2.0 AND Unicode-3.0";
  }
  const unlicensedVersions = {
    "aho-corasick": "1.1.4",
    memchr: "2.8.3",
  };
  if (candidate.name in unlicensedVersions) {
    requirePackage(candidate, unlicensedVersions[candidate.name], "Unlicense OR MIT");
    return "Unlicense";
  }
  const mitVersions = {
    "generic-array": "0.14.7",
    "pulldown-cmark": "0.13.4",
    "pulldown-cmark-escape": "0.11.0",
    zmij: "1.0.23",
  };
  if (candidate.license === "MIT") {
    if (!(candidate.name in mitVersions)) {
      throw new Error(`D5C Rust dependency requires a component MIT notice: ${candidate.name}`);
    }
    requirePackage(candidate, mitVersions[candidate.name], "MIT");
    return "MIT";
  }
  if (candidate.license.includes("Apache-2.0")) return "Apache-2.0";
  throw new Error(`D5C Rust dependency has no approved release license: ${candidate.name}`);
}

function requirePackage(candidate, version, license) {
  if (candidate.version !== version || candidate.license !== license) {
    throw new Error(`${candidate.name} version or license declaration changed`);
  }
}

function compareDependencies(left, right) {
  if (left.name !== right.name) return left.name < right.name ? -1 : 1;
  if (left.version === right.version) return 0;
  return left.version < right.version ? -1 : 1;
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

if (process.argv[1] === fileURLToPath(import.meta.url)) {
  const inventory = await validateD5cRustLicenses();
  process.stdout.write(`${JSON.stringify({
    target: inventory.target,
    features: inventory.features,
    dependencies: inventory.dependencies.length,
  })}\n`);
}
