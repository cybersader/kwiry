// SPDX-FileCopyrightText: 2026 cybersader
// SPDX-License-Identifier: GPL-3.0-only

import { createHash } from "node:crypto";
import {
  copyFile,
  mkdir,
  mkdtemp,
  readFile,
  readdir,
  rm,
} from "node:fs/promises";
import { tmpdir } from "node:os";
import { resolve } from "node:path";
import { afterAll, beforeAll, describe, expect, it } from "vitest";

import { buildPlugin } from "../esbuild.config.mjs";
import { packageD5cBrat } from "../scripts/package-d5c-brat.mjs";

const root = resolve(import.meta.dirname, "..");
const canonicalPaths = [
  "main.js",
  "manifest.json",
  "package.json",
  "versions.json",
  "styles.css",
];
let temporaryRoot;
let packaged;
let canonicalBefore;

beforeAll(async () => {
  temporaryRoot = await mkdtemp(resolve(tmpdir(), "kwiry-d5c-brat-package-"));
  canonicalBefore = await readCanonicalFiles();
  packaged = await packageD5cBrat({
    outputRoot: temporaryRoot,
    requireClean: false,
  });
}, 300_000);

afterAll(async () => {
  await rm(temporaryRoot, { recursive: true, force: true });
});

function normalizedInputs(metafile) {
  return Object.keys(metafile.inputs).map((input) => input.replaceAll("\\", "/"));
}

async function readCanonicalFiles() {
  return Object.fromEntries(await Promise.all(canonicalPaths.map(async (name) => {
    try {
      const bytes = await readFile(resolve(root, name));
      return [name, { bytes: bytes.byteLength, sha256: sha256(bytes) }];
    } catch (error) {
      if (error?.code === "ENOENT") return [name, null];
      throw error;
    }
  })));
}

function sha256(value) {
  return createHash("sha256").update(value).digest("hex");
}

describe("D5C BRAT package", () => {
  it("uses a unique conspicuous fixture-only identity", async () => {
    const manifest = JSON.parse(await readFile(resolve(packaged.pluginRoot, "manifest.json"), "utf8"));
    expect(manifest).toEqual({
      id: "kwiry-d5c-balanced-playground",
      name: "Kwiry D5C Balanced Playground",
      version: "0.0.1",
      minAppVersion: "1.7.2",
      description: "Fixture-only private Balanced ranking playground. Not a production Kwiry release.",
      author: "cybersader",
      authorUrl: "https://github.com/cybersader/kwiry",
      isDesktopOnly: true,
    });
    expect(packaged.attestation.plugin).toEqual({
      id: manifest.id,
      version: manifest.version,
    });
    expect(packaged.config.plugin.id).toBe(manifest.id);
    expect(packaged.config.plugin.version).toBe(manifest.version);
  });

  it("emits the exact self-contained three-file runtime package", async () => {
    expect((await readdir(packaged.pluginRoot)).sort()).toEqual([
      "main.js",
      "manifest.json",
      "styles.css",
    ]);
    expect((await readdir(packaged.supportRoot)).sort()).toEqual([
      "Apache-2.0.txt",
      "LICENSE",
      "SHA256SUMS",
      "THIRD_PARTY_NOTICES.md",
      "d5c-balanced-playground.attestation.json",
    ]);
    const allNames = [
      ...await readdir(packaged.pluginRoot),
      ...await readdir(packaged.supportRoot),
    ];
    expect(allNames.some((name) => /\.(?:wasm|map|db|sqlite3?|jsonl)$/u.test(name))).toBe(false);
    expect(allNames.some((name) => /worker.*\.js$/u.test(name))).toBe(false);
    const notices = await readFile(resolve(packaged.supportRoot, "THIRD_PARTY_NOTICES.md"), "utf8");
    const apache = await readFile(resolve(packaged.supportRoot, "Apache-2.0.txt"), "utf8");
    expect(notices).toContain("`Apache-2.0.txt`");
    expect(notices).not.toContain("(licenses/Apache-2.0.txt)");
    expect(apache).toContain("Apache License");
    expect(apache).toContain("Version 2.0, January 2004");
  });

  it("is a minified source-mapped-free private playground build", () => {
    expect(packaged.mainText.slice(0, 1_500)).toContain("GNU General Public License");
    expect(packaged.mainText).toContain("module.exports");
    expect(packaged.mainText).not.toContain("sourceMappingURL=");
    expect(packaged.mainText).not.toMatch(/\bimport\s*\(|\bimportScripts\s*\(/u);
    expect(packaged.mainText).toContain(
      `https://github.com/cybersader/kwiry/tree/${packaged.attestation.source.commit}`,
    );
    expect(packaged.mainText).not.toContain(
      "https://github.com/cybersader/kwiry/tree/d5c-balanced-playground-0.0.1-source",
    );
    for (const marker of [
      "open-private-d5c-balanced-playground",
      "Private D5C Balanced playground",
      "balanced-playground-v1",
      "internalD5cPlayground",
      "fixture_evaluate",
    ]) {
      expect(packaged.mainText).toContain(marker);
    }
    expect(Buffer.byteLength(packaged.mainText, "utf8")).toBeGreaterThan(1_000_000);
  });

  it("compiles normal host cache access out of the test build", () => {
    expect(packaged.attestation.build.active_vault_cache).toBe("disabled");
    const inputs = normalizedInputs(packaged.mainMetafile);
    expect(inputs.some((input) => input.endsWith("cache/local-cache-store.ts"))).toBe(false);
    expect(inputs.some((input) => input.endsWith("cache/cache-root.ts"))).toBe(false);
    expect(inputs.some((input) => input.endsWith("cache/build-cache-options.ts"))).toBe(false);
    expect(inputs.some((input) => input.includes("kwiry-disabled-cache-profile"))).toBe(true);
  });

  it("preserves the normal production build identity and D5C exclusion", async () => {
    const production = await buildPlugin({ write: false, production: true });
    expect(production.buildProfile.activeVaultCache).toBe(true);
    const productionManifest = JSON.parse(await readFile(resolve(root, "manifest.json"), "utf8"));
    expect(productionManifest.id).toBe("kwiry-search");
    expect(production.identities.plugin).toEqual({
      id: productionManifest.id,
      version: productionManifest.version,
    });
    const inputs = normalizedInputs(production.mainMetafile);
    expect(inputs.some((input) => input.endsWith("cache/build-cache-options.ts"))).toBe(true);
    expect(inputs.some((input) => input.endsWith("cache/local-cache-store.ts"))).toBe(true);
    for (const marker of [
      "kwiry-d5c-balanced-playground",
      "open-private-d5c-balanced-playground",
      "balanced-playground-v1",
      "internalD5cPlayground",
    ]) {
      expect(production.mainText).not.toContain(marker);
    }
  }, 180_000);

  it("attests independently recomputable runtime hashes without private paths", async () => {
    const attestationText = await readFile(
      resolve(packaged.supportRoot, "d5c-balanced-playground.attestation.json"),
      "utf8",
    );
    expect(JSON.parse(attestationText)).toEqual(packaged.attestation);
    for (const name of ["main.js", "manifest.json", "styles.css"]) {
      const bytes = await readFile(resolve(packaged.pluginRoot, name));
      expect(packaged.attestation.runtime[name]).toEqual({
        bytes: bytes.byteLength,
        sha256: sha256(bytes),
      });
    }
    expect(packaged.attestation.publishable).toBe(false);
    expect(packaged.attestation.source.tag_verified).toBe(false);
    expect(attestationText).not.toMatch(/\/home\/|\/Users\/|\/mnt\/[a-z]\//u);
    expect(attestationText).not.toContain("query");
    expect(attestationText).not.toContain("authorization");
    expect(attestationText).not.toContain("fixture_contents");

    // Emscripten intentionally embeds its fixed virtual home. Reject every
    // other host-shaped home plus literal credential and attribution patterns.
    expect(packaged.mainText).toContain("/home/web_user");
    expect(packaged.mainText).not.toMatch(
      /\/home\/(?!web_user(?:[\/"']|$))[A-Za-z0-9._-]+/u,
    );
    expect(packaged.mainText).not.toMatch(/\/Users\/|\/mnt\/[a-z]\/|[A-Z]:\\\\Users\\\\/u);
    expect(packaged.mainText).not.toMatch(/Bearer\s+[A-Za-z0-9._~-]{24,}/u);
    expect(packaged.mainText).not.toMatch(/(?:sk|ghp|github_pat)_[A-Za-z0-9_-]{16,}/u);
    expect(packaged.mainText).not.toMatch(
      /Co-Authored-By:.*(?:Claude|Anthropic|GPT|OpenAI)/iu,
    );
  });

  it("installs into a disposable vault under the unique plugin ID", async () => {
    const pluginDirectory = resolve(
      temporaryRoot,
      "vault/.obsidian/plugins/kwiry-d5c-balanced-playground",
    );
    await mkdir(pluginDirectory, { recursive: true });
    for (const name of ["main.js", "manifest.json", "styles.css"]) {
      await copyFile(resolve(packaged.pluginRoot, name), resolve(pluginDirectory, name));
    }
    expect((await readdir(pluginDirectory)).sort()).toEqual([
      "main.js",
      "manifest.json",
      "styles.css",
    ]);
    for (const name of ["main.js", "manifest.json", "styles.css"]) {
      const installed = await readFile(resolve(pluginDirectory, name));
      expect(sha256(installed)).toBe(packaged.attestation.runtime[name].sha256);
    }
  });

  it("does not create or alter canonical production package files", async () => {
    expect(await readCanonicalFiles()).toEqual(canonicalBefore);
  });
});
