// SPDX-FileCopyrightText: 2026 cybersader
// SPDX-License-Identifier: GPL-3.0-only

import { createHash } from "node:crypto";
import { readFile, writeFile } from "node:fs/promises";
import { dirname, resolve } from "node:path";
import { spawnSync } from "node:child_process";
import { fileURLToPath } from "node:url";

const root = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const manifestPath = resolve(root, "rust/kwiry-obsidian-wasm/Cargo.toml");
const noticeBundlePath = resolve(root, "licenses/Rust-DEPENDENCY-LICENSES.md");
const TARGET = "wasm32-unknown-unknown";
const CONFIGS = Object.freeze({
  d5c: Object.freeze({
    label: "D5C",
    inventoryPath: resolve(root, "d5c-rust-license-inventory.json"),
    features: Object.freeze(["internal-d5c-preview"]),
    assertGraph: false,
  }),
  docx: Object.freeze({
    label: "DOCX",
    inventoryPath: resolve(root, "docx-rust-license-inventory.json"),
    features: Object.freeze(["internal-docx-extractor"]),
    assertGraph: true,
  }),
});

export async function readD5cRustLicenseInventory() {
  return readRustLicenseInventory(CONFIGS.d5c);
}

export async function validateD5cRustLicenses() {
  return validateRustLicenses(CONFIGS.d5c);
}

export async function readDocxRustLicenseInventory() {
  return readRustLicenseInventory(CONFIGS.docx);
}

export async function validateDocxRustLicenses() {
  return validateRustLicenses(CONFIGS.docx);
}

async function readRustLicenseInventory(config) {
  const [inventoryText, noticeBundle] = await Promise.all([
    readFile(config.inventoryPath, "utf8"),
    readFile(noticeBundlePath),
  ]);
  const inventory = JSON.parse(inventoryText);
  validateInventoryShape(inventory, config);
  if (createHash("sha256").update(noticeBundle).digest("hex")
    !== inventory.notice_bundle_sha256) {
    throw new Error(`${config.label} Rust dependency notice bundle does not match its inventory`);
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

async function validateRustLicenses(config) {
  const inventory = await readRustLicenseInventory(config);
  const metadata = loadCargoMetadata(config);
  const actual = collectReleaseDependencies(metadata, config);
  if (JSON.stringify(actual) !== JSON.stringify(inventory.dependencies)) {
    throw new Error(`${config.label} Rust dependency license inventory does not match Cargo.lock`);
  }
  if (config.assertGraph) {
    const packagesById = new Map(metadata.packages.map((candidate) => [candidate.id, candidate]));
    const nodesById = new Map(metadata.resolve.nodes.map((node) => [node.id, node]));
    assertDocxDependencyFeatures(metadata, packagesById, nodesById);
  }
  return inventory;
}

// Regeneration is an explicit subcommand, never an environment variable. The
// previous `KWIRY_WRITE_RUST_LICENSE_INVENTORY=1` branch wrote nothing despite
// its name and returned before the graph comparison, so any CI environment that
// happened to set it would have made the inventory check pass unconditionally.
// It also cannot go through readRustLicenseInventory: that gate rejects a stale
// notice-bundle digest, which is precisely what regeneration is fixing.
export async function regenerateRustLicenseInventory(configName) {
  const config = CONFIGS[configName];
  if (config === undefined) throw new Error(`unknown Rust license inventory: ${configName}`);
  const [noticeBundle, metadata] = [
    await readFile(noticeBundlePath),
    loadCargoMetadata(config),
  ];
  const regenerated = {
    schema_version: 1,
    target: TARGET,
    features: [...config.features],
    notice_bundle_sha256: createHash("sha256").update(noticeBundle).digest("hex"),
    dependencies: collectReleaseDependencies(metadata, config),
  };
  validateInventoryShape(regenerated, config);
  await writeFile(config.inventoryPath, `${JSON.stringify(regenerated, null, 2)}\n`, "utf8");
  return regenerated;
}

// The notice bundle's header claims the production, D5C and DOCX builds resolve
// to one identical package set. That was prose nothing derived, so a MIT crate
// reaching only one of the three would have carried no notice in the others.
export function collectRustReleaseGraph(features) {
  const config = { label: "release", features };
  return collectReleaseDependencies(loadCargoMetadata(config), config);
}

function loadCargoMetadata(config) {
  const result = spawnSync("cargo", [
    "metadata",
    "--locked",
    "--manifest-path",
    manifestPath,
    "--format-version",
    "1",
    ...(config.features.length > 0 ? ["--features", config.features.join(",")] : []),
    "--filter-platform",
    TARGET,
  ], {
    cwd: root,
    encoding: "utf8",
    maxBuffer: 64 * 1024 * 1024,
  });
  if (result.status !== 0) {
    throw new Error(`Cargo metadata failed while validating ${config.label} Rust licenses`);
  }
  return JSON.parse(result.stdout);
}

function collectReleaseDependencies(metadata, config) {
  const rootPackage = metadata.packages.find(
    (candidate) => resolve(candidate.manifest_path) === manifestPath,
  );
  if (rootPackage === undefined) {
    throw new Error(`${config.label} Rust adapter is absent from Cargo metadata`);
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
      throw new Error(`${config.label} Rust dependency is absent from the resolved Cargo graph`);
    }
    pending.push(...node.dependencies);
  }

  return [...packageIds].map((packageId) => {
    const candidate = packagesById.get(packageId);
    if (candidate === undefined) {
      throw new Error(`${config.label} Rust dependency metadata is incomplete`);
    }
    if (typeof candidate.license !== "string" || candidate.license.length === 0) {
      throw new Error(`${config.label} Rust dependency has no declared license: ${candidate.name}`);
    }
    return {
      name: candidate.name,
      version: candidate.version,
      declared_license: candidate.license,
      release_license: selectReleaseLicense(candidate),
    };
  }).sort(compareDependencies);
}

function assertDocxDependencyFeatures(metadata, packagesById, nodesById) {
  const featuresByName = new Map();
  for (const node of metadata.resolve.nodes) {
    const pkg = packagesById.get(node.id);
    if (pkg !== undefined) featuresByName.set(pkg.name, [...node.features].sort());
  }
  const rawzip = featuresByName.get("rawzip");
  const flate2 = featuresByName.get("flate2");
  const quickXml = featuresByName.get("quick-xml");
  if (JSON.stringify(rawzip) !== "[]") {
    throw new Error("DOCX rawzip must resolve with an empty feature list");
  }
  // The property that matters is that no C zlib backend reaches the WASM build,
  // not that a particular feature name is absent. Banning "default" outright
  // stood in for that and stopped being equivalent once lopdf arrived: it
  // depends on flate2 without default-features = false, so "default" resolves,
  // and flate2 1.1.x defines default = ["rust_backend"] — the very backend this
  // gate requires. Assert the backend directly instead, and assert that no
  // C-zlib feature or sys crate is in the graph at all.
  for (const required of ["rust_backend", "miniz_oxide"]) {
    if (!flate2.includes(required)) {
      throw new Error(`DOCX flate2 must resolve through ${required}`);
    }
  }
  for (const forbidden of ["any_c_zlib", "any_zlib", "cloudflare_zlib", "zlib", "zlib-ng", "zlib-rs"]) {
    if (flate2.includes(forbidden)) {
      throw new Error(`DOCX flate2 unexpectedly enables the C backend feature ${forbidden}`);
    }
  }
  for (const sys of ["libz-sys", "libz-ng-sys", "libz-rs-sys", "cloudflare-zlib-sys"]) {
    if (featuresByName.has(sys)) {
      throw new Error(`DOCX Rust dependency graph unexpectedly contains ${sys}`);
    }
  }
  if (JSON.stringify(quickXml) !== JSON.stringify(["encoding", "encoding_rs"])) {
    throw new Error("DOCX quick-xml must resolve only through encoding");
  }
  for (const forbidden of ["default", "std", "zlib", "zlib-ng", "zlib-rs"]) {
    if (rawzip.includes(forbidden) || quickXml.includes(forbidden)) {
      throw new Error(`DOCX Rust dependency graph unexpectedly enables ${forbidden}`);
    }
  }
  void nodesById;
}

function validateInventoryShape(inventory, config) {
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
    || JSON.stringify(inventory.features) !== JSON.stringify(config.features)
    || !/^[0-9a-f]{64}$/u.test(inventory.notice_bundle_sha256)
    || !Array.isArray(inventory.dependencies)
    || inventory.dependencies.length === 0) {
    throw new Error(`${config.label} Rust dependency license inventory is invalid`);
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
      throw new Error(`${config.label} Rust dependency license entry is invalid`);
    }
    if (previous !== null && compareDependencies(previous, dependency) >= 0) {
      throw new Error(`${config.label} Rust dependency license inventory is not uniquely sorted`);
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
  if (candidate.name === "granit-parser") {
    requirePackage(candidate, "0.0.7", "MIT OR Apache-2.0");
    return "Apache-2.0";
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
    ecb: "0.2.0",
    lopdf: "0.44.0",
    nom: "8.0.0",
    "pulldown-cmark": "0.13.4",
    "pulldown-cmark-escape": "0.11.0",
    "quick-xml": "0.41.0",
    rawzip: "0.5.1",
    "simd-adler32": "0.3.10",
    zmij: "1.0.23",
  };
  if (candidate.license === "MIT") {
    if (!(candidate.name in mitVersions)) {
      throw new Error(`Rust dependency requires a component MIT notice: ${candidate.name}`);
    }
    requirePackage(candidate, mitVersions[candidate.name], "MIT");
    return "MIT";
  }
  if (candidate.license.includes("Apache-2.0")) return "Apache-2.0";
  throw new Error(`Rust dependency has no approved release license: ${candidate.name}`);
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
  const argv = process.argv.slice(2).filter((token) => token !== "--");
  const regenerate = argv.includes("regenerate");
  const configName = argv.includes("docx") ? "docx" : "d5c";
  const inventory = regenerate
    ? await regenerateRustLicenseInventory(configName)
    : await (configName === "docx" ? validateDocxRustLicenses() : validateD5cRustLicenses());
  process.stdout.write(`${JSON.stringify({
    target: inventory.target,
    features: inventory.features,
    dependencies: inventory.dependencies.length,
    ...(regenerate ? { regenerated: configName } : {}),
  })}\n`);
}
