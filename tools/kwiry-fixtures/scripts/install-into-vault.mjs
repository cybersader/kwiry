// SPDX-FileCopyrightText: 2026 cybersader
// SPDX-License-Identifier: GPL-3.0-only
//
// Installs the built fixtures plugin into an existing vault and enables it,
// without disturbing any other plugin already enabled there.
//
// Usage: node scripts/install-into-vault.mjs /path/to/vault

import { copyFile, mkdir, readFile, writeFile } from "node:fs/promises";
import { existsSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const root = dirname(dirname(fileURLToPath(import.meta.url)));
const target = process.argv[2];

if (!target) {
  console.error("usage: node scripts/install-into-vault.mjs /path/to/vault");
  process.exit(1);
}

const vault = resolve(target);
if (!existsSync(vault)) {
  console.error(`vault directory does not exist: ${vault}`);
  process.exit(1);
}
if (!existsSync(join(root, "main.js"))) {
  console.error("main.js is missing — run `npm run build` first");
  process.exit(1);
}

const pluginDir = join(vault, ".obsidian", "plugins", "kwiry-fixtures");
await mkdir(pluginDir, { recursive: true });
for (const name of ["main.js", "manifest.json"]) {
  await copyFile(join(root, name), join(pluginDir, name));
}

// Enable without clobbering other enabled plugins.
const communityPath = join(vault, ".obsidian", "community-plugins.json");
let enabled = [];
if (existsSync(communityPath)) {
  try {
    const parsed = JSON.parse(await readFile(communityPath, "utf8"));
    if (Array.isArray(parsed)) enabled = parsed;
  } catch {
    // Unreadable file: fall back to a fresh list rather than failing.
  }
}
if (!enabled.includes("kwiry-fixtures")) enabled.push("kwiry-fixtures");
await writeFile(communityPath, `${JSON.stringify(enabled, null, 2)}\n`, "utf8");

console.log(`installed kwiry-fixtures into ${vault}`);
console.log(`enabled plugins: ${enabled.join(", ")}`);
console.log("reload Obsidian (Ctrl+R) if the vault is already open");
