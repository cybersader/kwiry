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
    validateLockedRustGraph: false,
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
  it("uses a distinct local owner-search identity", async () => {
    const manifest = JSON.parse(await readFile(resolve(packaged.pluginRoot, "manifest.json"), "utf8"));
    expect(manifest).toEqual({
      id: "kwiry-d5c-balanced-playground",
      name: "Kwiry D5C Balanced Playground",
      version: "0.0.3",
      minAppVersion: "1.7.2",
      description: "Local active-vault Text vs Balanced search experiment. Not a production Kwiry release.",
      author: "cybersader",
      authorUrl: "https://github.com/cybersader/kwiry",
      isDesktopOnly: true,
    });
    expect(packaged.attestation.plugin).toEqual({ id: manifest.id, version: manifest.version });
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
      "Emscripten-LICENSE.txt",
      "LICENSE",
      "Rust-DEPENDENCY-LICENSES.md",
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
    const emscripten = await readFile(
      resolve(packaged.supportRoot, "Emscripten-LICENSE.txt"),
      "utf8",
    );
    const rustDependencies = await readFile(
      resolve(packaged.supportRoot, "Rust-DEPENDENCY-LICENSES.md"),
      "utf8",
    );
    expect(notices).toContain("`Apache-2.0.txt`");
    expect(notices).toContain("`Emscripten-LICENSE.txt`");
    expect(notices).toContain("`Rust-DEPENDENCY-LICENSES.md`");
    expect(notices).toContain("University of Illinois/NCSA Open Source License");
    expect(notices).not.toContain("(licenses/Apache-2.0.txt)");
    expect(notices).not.toContain("(licenses/Emscripten-LICENSE.txt)");
    expect(notices).not.toContain("(licenses/Rust-DEPENDENCY-LICENSES.md)");
    expect(apache).toContain("Apache License");
    expect(apache).toContain("Version 2.0, January 2004");
    expect(emscripten).toContain("Emscripten is available under 2 licenses");
    expect(emscripten).toContain("Permission is hereby granted");
    expect(emscripten).toContain("University of Illinois/NCSA Open Source License");
    expect(rustDependencies).toContain("generic-array 0.14.7 — MIT");
    expect(rustDependencies).toContain("Copyright (c) 2015 Bartłomiej Kamiński");
    expect(rustDependencies).toContain(
      "pulldown-cmark 0.13.4 and pulldown-cmark-escape 0.11.0 — MIT",
    );
    expect(rustDependencies).toContain("Copyright 2015 Google Inc. All rights reserved.");
    expect(rustDependencies).toContain("zmij 1.0.23 — MIT");
    expect(rustDependencies).toContain("David Tolnay as the author");
    expect(rustDependencies).toContain(
      "aho-corasick 1.1.4 and memchr 2.8.3 — Unlicense",
    );
    expect(rustDependencies).toContain(
      "This is free and unencumbered software released into the public domain.",
    );
    expect(rustDependencies).toContain(
      "granit-parser 0.0.7 — inherited and modification notices",
    );
    expect(rustDependencies).toContain("Copyright (c) 2015 Chen Yuheng");
    expect(rustDependencies).toContain("Copyright (c) 2023 Ethiraric");
    expect(rustDependencies).toContain("Copyright (c) 2026 Bourumir Wyngs");
    expect(rustDependencies).toContain(
      "Redistributions of this Work must include all inherited and new license notices",
    );
    expect(rustDependencies).toContain("Copyright © WHATWG (Apple, Google, Mozilla, Microsoft).");
    expect(rustDependencies).toContain("UNICODE LICENSE V3");
    expect(rustDependencies).toContain("Copyright © 1991-2023 Unicode, Inc.");
  });

  it("validates the exact locked Rust release dependency inventory", async () => {
    const tracked = JSON.parse(
      await readFile(resolve(root, "d5c-rust-license-inventory.json"), "utf8"),
    );
    expect(packaged.rustLicenseInventory).toEqual(tracked);
    expect(tracked).toMatchObject({
      schema_version: 1,
      target: "wasm32-unknown-unknown",
      features: ["internal-d5c-preview"],
    });
    expect(tracked.dependencies).toHaveLength(59);
    expect(tracked.dependencies.filter(({ release_license: license }) => license === "MIT"))
      .toEqual([
        expect.objectContaining({ name: "generic-array", version: "0.14.7" }),
        expect.objectContaining({ name: "pulldown-cmark", version: "0.13.4" }),
        expect.objectContaining({ name: "pulldown-cmark-escape", version: "0.11.0" }),
        expect.objectContaining({ name: "zmij", version: "1.0.23" }),
      ]);
    expect(JSON.stringify(tracked)).not.toMatch(/\/home\/|\/Users\/|\/mnt\/[a-z]\//u);
  });

  it("ships owner-only styles without adding them to production CSS", async () => {
    const productionStyles = await readFile(resolve(root, "styles.css"), "utf8");
    const ownerStyles = await readFile(resolve(packaged.pluginRoot, "styles.css"), "utf8");
    expect(productionStyles).not.toContain(".kwiry-d5c-live");
    expect(ownerStyles).toContain(".kwiry-d5c-live");
    expect(ownerStyles).toContain(".kwiry-result-title");
  });

  it("builds the live Text-versus-Balanced owner host instead of the fixture UI", () => {
    expect(packaged.buildProfile).toMatchObject({
      production: true,
      write: false,
      activeVaultCache: false,
      internalD5cOwnerHost: true,
    });
    expect(packaged.mainText.slice(0, 1_500)).toContain("GNU General Public License");
    expect(packaged.mainText).toContain("module.exports");
    expect(packaged.mainText).not.toContain("sourceMappingURL=");
    expect(packaged.mainText).not.toMatch(/\bimport\s*\(|\bimportScripts\s*\(/u);
    expect(packaged.mainText).toContain(
      `https://github.com/cybersader/kwiry/tree/${packaged.attestation.source.commit}`,
    );
    expect(packaged.mainText).not.toContain(
      `https://github.com/cybersader/kwiry/tree/${packaged.config.source.tag}`,
    );
    for (const marker of [
      "open-text-vs-balanced",
      "Open Text vs Balanced search",
      "live-text-balanced-v1",
      "internal_d5c_compare",
    ]) {
      expect(packaged.mainText).toContain(marker);
    }
    for (const oldFixtureMarker of [
      "open-private-d5c-balanced-playground",
      "Private D5C Balanced playground",
      "balanced-playground-v1",
      "fixture_evaluate",
    ]) {
      expect(packaged.mainText).not.toContain(oldFixtureMarker);
    }
    expect(Buffer.byteLength(packaged.mainText, "utf8")).toBeGreaterThan(1_000_000);
  });

  it("compiles cache, daemon, network, credentials, settings, and fixture tools out", () => {
    expect(packaged.attestation.build).toMatchObject({
      host_profile: "local_active_vault_owner_preview",
      active_vault_cache: "disabled",
      network_access: "compiled_out",
      credential_access: "compiled_out",
      persistence: "disabled",
      same_environment_repeatable: true,
      embedded_workers: 1,
    });
    expect(packaged.attestation.build).not.toHaveProperty("deterministic");
    const mainInputs = normalizedInputs(packaged.mainMetafile);
    const workerInputs = normalizedInputs(packaged.workerMetafile);
    for (const forbidden of [
      "/src/main.ts",
      "/src/api.ts",
      "/src/backend-manager.ts",
      "/src/backends/daemon-backend.ts",
      "/src/credentials.ts",
      "/src/settings.ts",
      "/src/settings-tab.ts",
      "/src/cache/",
      "/src/worker/d5c-playground-worker.ts",
      "/src/worker/d5c-evaluation.ts",
    ]) {
      expect(mainInputs.some((input) => input.includes(forbidden))).toBe(false);
      expect(workerInputs.some((input) => input.includes(forbidden))).toBe(false);
    }
    expect(mainInputs.some((input) => input.endsWith("internal/d5c-playground/live-main.ts"))).toBe(true);
    expect(workerInputs.some((input) => input.endsWith("worker/d5c-preview.ts"))).toBe(true);
    expect(workerInputs.some((input) => input.endsWith("worker/d5c-compare-protocol.ts"))).toBe(true);
    expect(workerInputs.some((input) => input.endsWith("worker/d5c-owner-protocol.ts"))).toBe(true);
    for (const forbidden of [
      "export_generation",
      "restore_generation",
      "plan_reconciliation",
      "cache_schema_version",
    ]) {
      expect(packaged.workerSource).not.toContain(forbidden);
      if (forbidden !== "cache_schema_version") {
        expect(packaged.mainText).not.toContain(forbidden);
      }
    }
    expect(workerInputs.some((input) => input.endsWith("worker/block-vfs.ts"))).toBe(false);
    expect(workerInputs.some((input) => input.endsWith("worker/image-header.ts"))).toBe(false);
  });

  it("never permits a publication package to skip locked Rust license validation", async () => {
    await expect(packageD5cBrat({
      outputRoot: temporaryRoot,
      validateLockedRustGraph: false,
    })).rejects.toThrow("publication cannot skip locked Rust license validation");
  });

  it("fails closed if the owner host is combined with the active-vault cache", async () => {
    await expect(buildPlugin({
      write: false,
      production: true,
      internalD5cOwnerHost: true,
      activeVaultCache: true,
    })).rejects.toThrow("D5C owner host must compile without the active-vault cache");
  });

  it("preserves the normal production build identity and D5C exclusion", async () => {
    const production = await buildPlugin({ write: false, production: true });
    expect(production.buildProfile.activeVaultCache).toBe(true);
    expect(production.buildProfile.internalD5cOwnerHost).toBe(false);
    const productionManifest = JSON.parse(await readFile(resolve(root, "manifest.json"), "utf8"));
    expect(productionManifest.id).toBe("kwiry-search");
    expect(production.identities.plugin).toEqual({
      id: productionManifest.id,
      version: productionManifest.version,
    });
    const inputs = normalizedInputs(production.mainMetafile);
    const workerInputs = normalizedInputs(production.workerMetafile);
    expect(inputs.some((input) => input.endsWith("cache/build-cache-options.ts"))).toBe(true);
    expect(inputs.some((input) => input.endsWith("cache/local-cache-store.ts"))).toBe(true);
    expect(workerInputs.some((input) => input.endsWith("worker/d5c-preview.ts"))).toBe(false);
    expect(workerInputs.some((input) => input.endsWith("worker/d5c-owner-protocol.ts"))).toBe(false);
    for (const marker of [
      "kwiry-d5c-balanced-playground",
      "open-text-vs-balanced",
      "live-text-balanced-v1",
      "internal_d5c_compare",
    ]) {
      expect(production.mainText).not.toContain(marker);
      expect(production.workerSource).not.toContain(marker);
    }
  }, 180_000);

  it("attests hashes and the local-only runtime boundary without private paths", async () => {
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
    expect(packaged.attestation.known_limits).toEqual(expect.arrayContaining([
      "reads_markdown_from_the_active_vault",
      "initial_cold_search_is_partial_and_explicitly_labeled",
      "index_is_in_memory_and_rebuilt_after_restart",
      "general_gate5_capacity_regression_tracked_separately",
    ]));
    expect(attestationText).not.toMatch(/\/home\/|\/Users\/|\/mnt\/[a-z]\//u);
    expect(attestationText).not.toContain("authorization");

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
