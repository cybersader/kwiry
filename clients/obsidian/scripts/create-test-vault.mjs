// SPDX-FileCopyrightText: 2026 cybersader
// SPDX-License-Identifier: GPL-3.0-only

import { copyFile, mkdir, readFile, readdir, writeFile } from "node:fs/promises";
import { createHash } from "node:crypto";
import { isAbsolute, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const clientDir = resolve(fileURLToPath(new URL("..", import.meta.url)));
const targetArgument = process.argv[2];

if (!targetArgument || !isAbsolute(targetArgument)) {
  throw new Error(
    "Usage: npm run test-vault -- /absolute/path/to/empty-disposable-vault",
  );
}

const targetDir = resolve(targetArgument);
await mkdir(targetDir, { recursive: true });
if ((await readdir(targetDir)).length !== 0) {
  throw new Error("The disposable-vault target must be empty.");
}

const pluginDir = join(targetDir, ".obsidian", "plugins", "kwiry-search");
await mkdir(pluginDir, { recursive: true });

const artifactNames = ["main.js", "manifest.json", "styles.css"];
const hashes = {};
for (const name of artifactNames) {
  const source = join(clientDir, name);
  const destination = join(pluginDir, name);
  await copyFile(source, destination);
  hashes[name] = await sha256(destination);
}

await writeFile(
  join(targetDir, ".obsidian", "community-plugins.json"),
  `${JSON.stringify(["kwiry-search"], null, 2)}\n`,
  "utf8",
);
await writeFile(
  join(pluginDir, "data.json"),
  `${JSON.stringify({
    backendProfile: "in_plugin",
    daemonUrl: "http://127.0.0.1:32189",
    tokenFilePath: "",
    defaultMode: "hybrid",
    resultLimit: 20,
    vaultId: "",
    daemonCurrentVaultId: "",
    showRibbonIcon: true,
  }, null, 2)}\n`,
  "utf8",
);
await writeFile(
  join(targetDir, "Kwiry Gate 4 Test.md"),
  [
    "# Kwiry Gate 4 disposable test vault",
    "",
    "This vault contains no acceptance corpus. It exists only to verify the installed Gate 4 UI foundation.",
    "",
    "Expected in-plugin behavior:",
    "",
    "- the selected backend is **In-plugin · Lexical**;",
    "- only lexical mode is visible;",
    "- status and queries report that the in-memory index is still building;",
    "- no daemon or mode fallback occurs.",
    "",
  ].join("\n"),
  "utf8",
);

process.stdout.write(`${JSON.stringify({
  status: "ready",
  vault: targetDir,
  plugin: pluginDir,
  hashes,
  next: "Open this folder as an Obsidian vault and enable Kwiry Search if Obsidian asks.",
}, null, 2)}\n`);

async function sha256(path) {
  const bytes = await readFile(path);
  return createHash("sha256").update(bytes).digest("hex");
}
