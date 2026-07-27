// SPDX-FileCopyrightText: 2026 cybersader
// SPDX-License-Identifier: GPL-3.0-only
//
// Installs the built plugin into an existing vault and enables it, without
// disturbing any other plugin already enabled there.
//
// Usage: node scripts/install-into-vault.mjs /path/to/vault
//
// This is the direct-install path used while iterating. BRAT installs the same
// artifacts from a GitHub release instead; both produce an identical plugin
// directory, so behaviour observed here carries over.

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
if (!existsSync(join(vault, ".obsidian"))) {
  // Refuse rather than create one: a missing .obsidian almost always means the
  // path is wrong, and silently scaffolding it hides the mistake.
  console.error(`not an Obsidian vault (no .obsidian directory): ${vault}`);
  process.exit(1);
}
if (!existsSync(join(root, "main.js"))) {
  console.error("main.js is missing — run `npm run build` first");
  process.exit(1);
}

const pluginDir = join(vault, ".obsidian", "plugins", "kwiry-search");
await mkdir(pluginDir, { recursive: true });
for (const name of ["main.js", "manifest.json", "styles.css"]) {
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
if (!enabled.includes("kwiry-search")) enabled.push("kwiry-search");
await writeFile(communityPath, `${JSON.stringify(enabled, null, 2)}\n`, "utf8");

console.log(`installed kwiry-search into ${vault}`);
console.log(`enabled plugins: ${enabled.join(", ")}`);
console.log("");
console.log("In Obsidian: reload (Ctrl+R), then Settings -> Kwiry Search ->");
console.log("set Backend to 'In-plugin - Lexical' so it indexes locally with no daemon.");
