// SPDX-FileCopyrightText: 2026 cybersader
// SPDX-License-Identifier: GPL-3.0-only

import { createHash } from "node:crypto";
import {
  copyFile,
  lstat,
  mkdir,
  readFile,
  readdir,
  rm,
  writeFile,
} from "node:fs/promises";
import { isAbsolute, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

import {
  functionalOracles,
  generateFunctionalCorpus,
  generatePerformanceCorpus,
} from "./gate5-corpus.mjs";

const clientDir = resolve(fileURLToPath(new URL("..", import.meta.url)));

class SafeTestVaultError extends Error {}

main().catch((error) => {
  const message = error instanceof SafeTestVaultError
    ? error.message
    : "The disposable Gate 5 vault could not be created. Delete the incomplete disposable target and retry.";
  process.stderr.write(`${message}\n`);
  process.exitCode = 1;
});

async function main() {
  const targetArgument = process.argv[2];
  const profile = parseProfile(process.argv.slice(3));
  if (!targetArgument || !isAbsolute(targetArgument)) {
    throw new SafeTestVaultError(
      "Usage: npm run test-vault -- /absolute/path/to/empty-disposable-vault [--profile functional|performance]",
    );
  }

  const targetDir = resolve(targetArgument);
  await requireEmptyRealDirectory(targetDir);
  const marker = join(targetDir, ".kwiry-gate5-creating");
  await writeFile(marker, "incomplete\n", { encoding: "utf8", flag: "wx" });

  try {
    const pluginDir = join(targetDir, ".obsidian", "plugins", "kwiry-search");
    await mkdir(pluginDir, { recursive: true });
    const artifacts = await installArtifacts(pluginDir);
    const configuration = await writeConfiguration(targetDir, pluginDir);
    const corpus = profile === "performance"
      ? await generatePerformanceCorpus(targetDir)
      : await generateFunctionalCorpus(targetDir);
    const manifest = {
      schema_version: 1,
      kind: "kwiry_gate5_test_vault",
      profile,
      generator: {
        id: "kwiry-gate5-corpus",
        version: 1,
      },
      corpus,
      artifacts,
      configuration,
      ...(profile === "functional" ? { oracles: functionalOracles() } : {}),
    };
    await writeFile(
      join(targetDir, "kwiry-test-vault.json"),
      `${JSON.stringify(manifest, null, 2)}\n`,
      "utf8",
    );
    await rm(marker);

    process.stdout.write(`${JSON.stringify({
      schema_version: 1,
      status: "ready",
      profile,
      corpus: {
        note_count: corpus.note_count,
        markdown_bytes: corpus.markdown_bytes,
        sha256: corpus.sha256,
      },
      artifacts: {
        plugin_version: artifacts.plugin_version,
        set_sha256: artifacts.set_sha256,
        files: artifacts.files,
      },
      next: "Open the disposable vault in Obsidian, enable Kwiry Search, and follow the generated oracle manifest.",
    }, null, 2)}\n`);
  } catch (error) {
    if (error instanceof SafeTestVaultError) throw error;
    throw new SafeTestVaultError("The disposable Gate 5 vault could not be created. Delete the incomplete disposable target and retry.");
  }
}

async function requireEmptyRealDirectory(path) {
  try {
    const stats = await lstat(path);
    if (stats.isSymbolicLink() || !stats.isDirectory()) {
      throw new SafeTestVaultError("The disposable-vault target must be a real directory.");
    }
  } catch (error) {
    if (error?.code !== "ENOENT") throw error;
    await mkdir(path, { recursive: true });
  }
  if ((await readdir(path)).length !== 0) {
    throw new SafeTestVaultError("The disposable-vault target must be empty.");
  }
}

async function installArtifacts(pluginDir) {
  const files = [];
  for (const name of ["main.js", "manifest.json", "styles.css"]) {
    const source = join(clientDir, name);
    const destination = join(pluginDir, name);
    const before = await fileIdentity(source, name);
    await copyFile(source, destination);
    const installed = await fileIdentity(destination, name);
    const after = await fileIdentity(source, name);
    if (before.bytes !== installed.bytes
      || before.sha256 !== installed.sha256
      || before.bytes !== after.bytes
      || before.sha256 !== after.sha256) {
      throw new SafeTestVaultError("Installed plugin artifact identity mismatch.");
    }
    files.push(installed);
  }
  files.sort((left, right) => left.name.localeCompare(right.name));
  const pluginManifest = JSON.parse(await readFile(join(pluginDir, "manifest.json"), "utf8"));
  return {
    plugin_id: "kwiry-search",
    plugin_version: pluginManifest.version,
    files,
    set_sha256: artifactSetHash(files),
  };
}

async function writeConfiguration(targetDir, pluginDir) {
  const communityPlugins = `${JSON.stringify(["kwiry-search"], null, 2)}\n`;
  const data = `${JSON.stringify({
    backendProfile: "in_plugin",
    daemonUrl: "http://127.0.0.1:32189",
    tokenFilePath: "",
    defaultMode: "lexical",
    resultLimit: 20,
    vaultId: "",
    daemonCurrentVaultId: "",
    showRibbonIcon: true,
  }, null, 2)}\n`;
  await writeFile(join(targetDir, ".obsidian", "community-plugins.json"), communityPlugins, "utf8");
  await writeFile(join(pluginDir, "data.json"), data, "utf8");
  return {
    community_plugins_sha256: sha256(Buffer.from(communityPlugins)),
    data_sha256: sha256(Buffer.from(data)),
  };
}

async function fileIdentity(path, name) {
  const bytes = await readFile(path);
  return { name, bytes: bytes.byteLength, sha256: sha256(bytes) };
}

function artifactSetHash(files) {
  const hash = createHash("sha256");
  for (const file of files) {
    hash.update(file.name);
    hash.update("\0");
    hash.update(String(file.bytes));
    hash.update("\0");
    hash.update(file.sha256);
    hash.update("\0");
  }
  return hash.digest("hex");
}

function sha256(bytes) {
  return createHash("sha256").update(bytes).digest("hex");
}

function parseProfile(args) {
  if (args.length === 0) return "functional";
  if (args.length === 2 && args[0] === "--profile"
    && (args[1] === "functional" || args[1] === "performance")) {
    return args[1];
  }
  throw new SafeTestVaultError("Profile must be functional or performance.");
}
