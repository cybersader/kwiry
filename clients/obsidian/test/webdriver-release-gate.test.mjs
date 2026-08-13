// SPDX-FileCopyrightText: 2026 cybersader
// SPDX-License-Identifier: GPL-3.0-only

import { createHash } from "node:crypto";
import { execFile } from "node:child_process";
import { mkdir, mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { resolve } from "node:path";
import { promisify } from "node:util";
import { afterEach, describe, expect, it } from "vitest";

import {
  WebdriverGateError,
  assertObserved,
  buildEvidence,
  buildSyntheticXlsm,
  downloadPinnedArtifact,
  runWebdriverReleaseGate,
  validateRuntimeManifest,
} from "../scripts/webdriver-release-gate.mjs";
import { parseStoredZip } from "../scripts/stored-zip.mjs";

const repositoryRoot = resolve(import.meta.dirname, "..");
const manifestPath = resolve(repositoryRoot, "scripts/webdriver-release-gate-manifest.json");
const temporaryRoots = [];
const execFileAsync = promisify(execFile);
const HASH = "a".repeat(64);

async function createCandidateFixture() {
  const root = await mkdtemp(resolve(tmpdir(), "kwiry-webdriver-candidate-"));
  temporaryRoots.push(root);
  await mkdir(root, { recursive: true });
  for (const name of [
    "main.js", "manifest.json", "styles.css", "LICENSE", "THIRD_PARTY_NOTICES.md",
    "Apache-2.0.txt", "Emscripten-LICENSE.txt", "Rust-DEPENDENCY-LICENSES.md",
    "gate5.evidence.json", "kwiry-search.zip", "SHA256SUMS",
  ]) await writeFile(resolve(root, name), name === "manifest.json"
    ? `${JSON.stringify({ id: "kwiry-search", version: "0.6.0-beta.15" })}\n`
    : `${name}\n`);
  return root;
}

function manifestFixture() {
  return {
    schema_version: 1,
    platform: "linux-x64-xvfb",
    dependencies: {
      node: process.versions.node,
      obsidian_launcher: "3.1.1",
      selenium_webdriver: "4.39.0",
    },
    runtime: {
      obsidian_app: "1.13.7",
      obsidian_installer: "1.13.7",
      electron: "43.3.0",
      chromium: "150.0.7871.212",
      chromedriver: "150.0.7871.212",
    },
    artifacts: {
      obsidian_installer: { url: "https://example.test/v1.13.7/installer", bytes: 1, sha256: HASH },
      obsidian_app: { url: "https://example.test/v1.13.7/app", bytes: 1, sha256: HASH },
      chromedriver: { url: "https://example.test/v43.3.0/driver", bytes: 1, sha256: HASH },
    },
    derived: {
      obsidian_app_asar: { bytes: 1, sha256: HASH },
      chromedriver_binary: { bytes: 1, sha256: HASH },
    },
  };
}

function observed() {
  return {
    modalClosed: true,
    staleNotices: 0,
    openFailureNotices: 0,
    openFileCalls: 1,
    openFilePromise: "resolved",
    expectedFileActive: true,
    vbaPayloadSearchResults: 0,
    electron: "43.3.0",
    chromium: "150.0.7871.212",
    driver: "150.0.7871.212",
  };
}

afterEach(async () => {
  await Promise.all(temporaryRoots.splice(0).map((path) => rm(path, { recursive: true, force: true })));
});

describe("WebDriver release gate", () => {
  it("pins a closed reviewed runtime manifest to the package dependencies", async () => {
    const manifest = JSON.parse(await readFile(manifestPath, "utf8"));
    const packageJson = JSON.parse(await readFile(resolve(repositoryRoot, "package.json"), "utf8"));
    expect(validateRuntimeManifest(manifest, packageJson)).toEqual(manifest);
    expect(manifest.artifacts.obsidian_installer.url).not.toContain("latest");
    expect(manifest.artifacts.chromedriver.sha256).toMatch(/^[a-f0-9]{64}$/u);
  });

  it("runs the CLI entry point from a checkout path containing spaces", async () => {
    const root = await mkdtemp(resolve(tmpdir(), "kwiry webdriver cli "));
    temporaryRoots.push(root);
    const scriptDir = resolve(root, "scripts");
    await mkdir(scriptDir, { recursive: true });
    const script = resolve(scriptDir, "webdriver-release-gate.mjs");
    const modules = [
      "webdriver-release-gate.mjs", "production-package.mjs", "stored-zip.mjs",
      "webdriver-release-gate-schema.mjs", "gate5-evidence-schema.mjs", "privacy-policy.mjs",
    ];
    for (const name of modules) {
      await writeFile(resolve(scriptDir, name), await readFile(resolve(repositoryRoot, "scripts", name)));
    }
    await writeFile(resolve(root, "package.json"), await readFile(resolve(repositoryRoot, "package.json")));
    await expect(execFileAsync(process.execPath, [
      script,
      "--candidate", resolve(root, "candidate"),
      "--tag", "0.6.0-beta.16",
      "--manifest", resolve(root, "runtime.json"),
      "--evidence", resolve(root, "evidence.json"),
    ], { cwd: root })).rejects.toMatchObject({
      stderr: expect.stringContaining('"failure_stage":"candidate_invalid"'),
    });
  });

  it.each([
    ["moving app", (value) => { value.runtime.obsidian_app = "latest"; }],
    ["driver mismatch", (value) => { value.runtime.chromedriver = "150.0.7871.187"; }],
    ["dependency drift", (value) => { value.dependencies.selenium_webdriver = "4.40.0"; }],
    ["redirecting URL", (value) => { value.artifacts.obsidian_app.url = "https://example.test/latest/app"; }],
    ["unknown key", (value) => { value.runtime.extra = true; }],
  ])("rejects %s in the runtime manifest", (_name, mutate) => {
    const manifest = manifestFixture();
    mutate(manifest);
    expect(() => validateRuntimeManifest(manifest, {
      devDependencies: { "obsidian-launcher": "3.1.1", "selenium-webdriver": "4.39.0" },
    })).toThrow("runtime_manifest_invalid");
  });

  it("generates deterministic XLSM content while isolating VBA text", () => {
    const first = buildSyntheticXlsm();
    const second = buildSyntheticXlsm();
    expect(second).toEqual(first);
    const entries = new Map(parseStoredZip(first).map((entry) => [entry.name, entry.bytes]));
    expect(entries.get("xl/sharedStrings.xml").toString()).toContain("macro boundary");
    expect(entries.get("xl/vbaProject.bin").toString()).toContain("must-not-index");
    for (const [name, bytes] of entries) {
      if (name !== "xl/vbaProject.bin") expect(bytes.toString()).not.toContain("must-not-index");
    }
  });

  it("rejects an unreviewed redirect chain before consuming bytes", async () => {
    const artifact = { url: "https://example.test/v1/file", bytes: 1, sha256: sha256("x") };
    await expect(downloadPinnedArtifact(artifact, resolve(tmpdir(), "unused"), {
      fetchImpl: async () => new Response(null, { status: 302, headers: { location: "https://evil.test/file" } }),
    })).rejects.toThrow("download_identity_mismatch");
  });

  it("binds passing aggregate evidence to candidate and runtime hashes", () => {
    const evidence = buildEvidence({
      candidate: { version: "0.6.0-beta.15", candidate_set_sha256: HASH, file_count: 11 },
      manifest: manifestFixture(),
      manifestSha256: HASH,
      observed: observed(),
      cleanup: {
        webdriver_quit: true, obsidian_reaped: true, verified_download_server_closed: true,
        ports_closed: true, private_state_removed: true,
      },
    });
    expect(evidence.verdict).toBe("SELENIUM_RELEASE_GATE_PASSED");
    expect(JSON.stringify(evidence)).not.toContain("macro boundary");
  });

  it("rejects a non-production candidate before any runtime seam is called", async () => {
    const candidate = await createCandidateFixture();
    const manifestFile = resolve(candidate, "runtime.json");
    await writeFile(manifestFile, `${JSON.stringify(manifestFixture())}\n`);
    let runtimeCalls = 0;
    await expect(runWebdriverReleaseGate({
      candidate,
      manifest: manifestFile,
      evidence: resolve(candidate, "webdriver.evidence.json"),
      tag: "0.6.0-beta.15",
    }, {
      createPrivateRoot: async () => { runtimeCalls += 1; return candidate; },
    })).rejects.toThrow();
    expect(runtimeCalls).toBe(0);
  });

  it.each([
    ["stale notice", { ...observed(), staleNotices: 1 }, "stale_notice_observed"],
    ["missing open", { ...observed(), openFileCalls: 0 }, "open_not_invoked"],
    ["rejected open", { ...observed(), openFilePromise: "rejected" }, "open_promise_rejected"],
    ["runtime drift", { ...observed(), electron: "43.2.0" }, "launch_failed"],
  ])("does not accept %s from the real scenario", (_name, changed, code) => {
    expect(() => assertObserved(changed, manifestFixture())).toThrow(code);
  });
});

function sha256(value) {
  return createHash("sha256").update(value).digest("hex");
}
