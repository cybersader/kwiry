// SPDX-FileCopyrightText: 2026 cybersader
// SPDX-License-Identifier: GPL-3.0-only

import { spawnSync } from "node:child_process";
import { createHash } from "node:crypto";
import {
  lstat,
  mkdir,
  mkdtemp,
  readFile,
  readdir,
  rm,
  symlink,
  writeFile,
} from "node:fs/promises";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

main().catch(() => {
  process.stderr.write("Gate 5 disposable-vault smoke failed.\n");
  process.exitCode = 1;
});

async function main() {
  const clientDir = resolve(fileURLToPath(new URL("..", import.meta.url)));
  const creator = join(clientDir, "scripts", "create-test-vault.mjs");
  const root = await mkdtemp(resolve(tmpdir(), "kwiry-gate5-vault-smoke-"));
  try {
    const functional = join(root, "functional");
    await mkdir(functional);
    const created = runCreator(creator, functional, "functional");
    if (created.status !== 0 || created.stderr !== "") {
      throw new Error("functional disposable vault failed");
    }
    const output = JSON.parse(created.stdout);
    if (output.status !== "ready"
      || output.profile !== "functional"
      || output.corpus?.note_count !== 14
      || output.corpus?.markdown_bytes < 1
      || !isSha256(output.corpus?.sha256)
      || created.stdout.includes(functional)
      || created.stdout.includes(clientDir)) {
      throw new Error("functional disposable-vault output is invalid");
    }

    const pluginDir = join(functional, ".obsidian", "plugins", "kwiry-search");
    for (const name of ["main.js", "manifest.json", "styles.css"]) {
      const source = await readFile(join(clientDir, name));
      const installed = await readFile(join(pluginDir, name));
      if (sha256(source) !== sha256(installed)) {
        throw new Error("installed artifact identity mismatch");
      }
    }
    const data = JSON.parse(await readFile(join(pluginDir, "data.json"), "utf8"));
    if (data.backendProfile !== "in_plugin" || data.defaultMode !== "lexical") {
      throw new Error("functional disposable-vault configuration is invalid");
    }
    const manifest = JSON.parse(
      await readFile(join(functional, "kwiry-test-vault.json"), "utf8"),
    );
    if (manifest.kind !== "kwiry_gate5_test_vault"
      || manifest.profile !== "functional"
      || manifest.oracles?.search?.length !== 10
      || manifest.oracles?.mutations?.length !== 3) {
      throw new Error("functional disposable-vault manifest is invalid");
    }
    const paths = await readdir(functional, { recursive: true });
    if (paths.filter((path) => path.endsWith(".md")).length !== 14) {
      throw new Error("functional disposable-vault corpus is incomplete");
    }
    try {
      await lstat(join(functional, ".kwiry-gate5-creating"));
      throw new Error("incomplete marker remained after successful creation");
    } catch (error) {
      if (error?.code !== "ENOENT") throw error;
    }

    const nonempty = join(root, "nonempty");
    await mkdir(nonempty);
    await writeFile(join(nonempty, "occupied.txt"), "occupied\n", "utf8");
    assertSafeRefusal(
      runCreator(creator, nonempty, "functional"),
      "The disposable-vault target must be empty.\n",
      root,
      clientDir,
    );

    const real = join(root, "real");
    const linked = join(root, "linked");
    await mkdir(real);
    await symlink(real, linked, "dir");
    assertSafeRefusal(
      runCreator(creator, linked, "functional"),
      "The disposable-vault target must be a real directory.\n",
      root,
      clientDir,
    );

    process.stdout.write(`${JSON.stringify({
      schema_version: 1,
      status: "passed",
      profile: "functional",
      note_count: output.corpus.note_count,
      markdown_bytes: output.corpus.markdown_bytes,
      artifact_count: output.artifacts.files.length,
      refusal_checks: 2,
    })}\n`);
  } finally {
    await rm(root, { recursive: true, force: true });
  }
}

function runCreator(creator, target, profile) {
  return spawnSync(process.execPath, [creator, target, "--profile", profile], {
    encoding: "utf8",
  });
}

function assertSafeRefusal(result, expected, root, clientDir) {
  if (result.status === 0
    || result.stdout !== ""
    || result.stderr !== expected
    || result.stderr.includes(root)
    || result.stderr.includes(clientDir)
    || /\n\s*at\s/u.test(result.stderr)) {
    throw new Error("disposable-vault refusal was not sanitized");
  }
}

function sha256(bytes) {
  return createHash("sha256").update(bytes).digest("hex");
}

function isSha256(value) {
  return typeof value === "string" && /^[a-f0-9]{64}$/u.test(value);
}
