// SPDX-FileCopyrightText: 2026 cybersader
// SPDX-License-Identifier: GPL-3.0-only

import { mkdir, mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { resolve } from "node:path";
import { spawnSync } from "node:child_process";
import { afterEach, describe, expect, it } from "vitest";

import {
  assertPackagePrivacy,
  assertSourcePrivacy,
  assertWorkerAuthorityGraph,
  embedWorkerPrivacyBoundary,
  scanSourcePrivacy,
} from "../scripts/privacy-policy.mjs";

const repositoryRoot = resolve(import.meta.dirname, "..");
const policyScript = resolve(repositoryRoot, "scripts/privacy-policy.mjs");
const temporaryRoots = [];

function privateHome(user, ...segments) {
  return ["", "home", user, ...segments].join("/");
}

function workerAuthority() {
  return ["request", "Url"].join("");
}

function bearerCredential() {
  return ["Bearer", "A".repeat(32)].join(" ");
}

function aiAttribution() {
  return [["Co", "Authored", "By"].join("-"), ["G", "PT"].join("")].join(": ");
}

function mainWithEmbeddedWorker(workerSource = "export {};\n") {
  return `${embedWorkerPrivacyBoundary(workerSource)}\nmodule.exports = {};\n`;
}

async function createSourceFixture() {
  const root = await mkdtemp(resolve(tmpdir(), "kwiry-privacy-source-"));
  temporaryRoots.push(root);
  await Promise.all([
    mkdir(resolve(root, "src/worker"), { recursive: true }),
    mkdir(resolve(root, "test"), { recursive: true }),
  ]);
  await Promise.all([
    writeFile(resolve(root, "src/main.ts"), "export {};\n"),
    writeFile(resolve(root, "src/worker/worker.ts"), "export {};\n"),
    writeFile(resolve(root, "test/source.test.ts"), "export {};\n"),
    writeFile(resolve(root, "esbuild.config.mjs"), "export {};\n"),
    writeFile(resolve(root, "main.js"), mainWithEmbeddedWorker()),
  ]);
  return root;
}

async function createPackageFixture() {
  const root = await mkdtemp(resolve(tmpdir(), "kwiry-privacy-package-"));
  temporaryRoots.push(root);
  await writeFile(resolve(root, "main.js"), mainWithEmbeddedWorker());
  return root;
}

function runPolicy(...args) {
  return spawnSync(process.execPath, [policyScript, ...args], {
    encoding: "utf8",
  });
}

function runSourcePolicyInGithubBash(sourceRoot) {
  return spawnSync(
    "bash",
    [
      "--noprofile",
      "--norc",
      "-e",
      "-o",
      "pipefail",
      "-c",
      '"$NODE" "$POLICY" source "$SOURCE_ROOT"\nprintf "continued\\n"',
    ],
    {
      encoding: "utf8",
      env: {
        ...process.env,
        NODE: process.execPath,
        POLICY: policyScript,
        SOURCE_ROOT: sourceRoot,
      },
    },
  );
}

afterEach(async () => {
  await Promise.all(temporaryRoots.splice(0).map((path) =>
    rm(path, { recursive: true, force: true })
  ));
});

describe("privacy policy", () => {
  it("accepts a clean checkout through exact synthetic-fixture values before build", async () => {
    await expect(assertSourcePrivacy(repositoryRoot, {
      requireMainArtifact: false,
    })).resolves.toMatchObject({
      sourceRoot: repositoryRoot,
    });
  });

  it("requires the generated main artifact by default", async () => {
    const sourceRoot = await createSourceFixture();
    await rm(resolve(sourceRoot, "main.js"));

    await expect(assertSourcePrivacy(sourceRoot))
      .rejects.toThrow("privacy scan target is missing");
    await expect(assertSourcePrivacy(sourceRoot, {
      requireMainArtifact: false,
    })).resolves.toMatchObject({ sourceRoot });
  });

  it("makes a forbidden first-party source match fail the command", async () => {
    const sourceRoot = await createSourceFixture();
    await writeFile(
      resolve(sourceRoot, "src/leak.ts"),
      `export const leaked = ${JSON.stringify(privateHome("build-owner", "vault"))};\n`,
    );

    const result = runPolicy("source", sourceRoot);
    expect(result.status).toBe(1);
    expect(result.stderr).toContain("machine_path src/leak.ts:1");
    await expect(assertSourcePrivacy(sourceRoot)).rejects.toThrow("source privacy policy rejected");
  });

  it("fails closed before a later command under the GitHub Bash shell", async () => {
    const sourceRoot = await createSourceFixture();
    await writeFile(
      resolve(sourceRoot, "src/leak.ts"),
      `export const leaked = ${JSON.stringify(privateHome("build-owner", "vault"))};\n`,
    );

    const result = runSourcePolicyInGithubBash(sourceRoot);
    expect(result.status).toBe(1);
    expect(result.stdout).not.toContain("continued");
    expect(result.stderr).toContain("machine_path src/leak.ts:1");
  });

  it("makes a forbidden packaged-artifact match fail the command", async () => {
    const packageRoot = await createPackageFixture();
    await writeFile(
      resolve(packageRoot, "main.js"),
      `${mainWithEmbeddedWorker()}const token = ${JSON.stringify(bearerCredential())};\n`,
    );

    const result = runPolicy("package", packageRoot);
    expect(result.status).toBe(1);
    expect(result.stderr).toContain("literal_bearer main.js:6");
    await expect(assertPackagePrivacy(packageRoot)).rejects.toThrow("package privacy policy rejected");
  });

  it("makes Worker authority fail even when the general source scan is clean", async () => {
    const sourceRoot = await createSourceFixture();
    await writeFile(
      resolve(sourceRoot, "src/worker/authority.ts"),
      `export const forbidden = ${JSON.stringify(workerAuthority())};\n`,
    );

    const result = runPolicy("source", sourceRoot);
    expect(result.status).toBe(1);
    expect(result.stderr).toContain("worker_authority src/worker/authority.ts:1");
  });

  it("rejects Worker authority imported transitively from outside src/worker", async () => {
    const sourceRoot = await createSourceFixture();
    await writeFile(
      resolve(sourceRoot, "src/shared-authority.ts"),
      `export const forbidden = ${JSON.stringify(workerAuthority())};\n`,
    );

    await expect(assertWorkerAuthorityGraph(sourceRoot, {
      inputs: {
        "src/worker/worker.ts": {},
        "src/shared-authority.ts": {},
      },
    })).rejects.toThrow("worker_authority src/shared-authority.ts:1");
  });

  it("rejects forbidden authority in the Worker actually embedded in source main.js", async () => {
    const sourceRoot = await createSourceFixture();
    await writeFile(
      resolve(sourceRoot, "main.js"),
      mainWithEmbeddedWorker(`const forbidden = ${JSON.stringify(workerAuthority())};\n`),
    );

    await expect(assertSourcePrivacy(sourceRoot))
      .rejects.toThrow("worker_authority main.js:2");
  });

  it("rejects forbidden authority in a recursively packaged embedded Worker", async () => {
    const packageRoot = await createPackageFixture();
    await mkdir(resolve(packageRoot, "plugin"));
    await writeFile(
      resolve(packageRoot, "plugin/main.js"),
      mainWithEmbeddedWorker(`const forbidden = ${JSON.stringify(workerAuthority())};\n`),
    );

    await expect(assertPackagePrivacy(packageRoot))
      .rejects.toThrow("worker_authority plugin/main.js:2");
  });

  it("rejects every newly built package that omits embedded Worker markers", async () => {
    const packageRoot = await createPackageFixture();
    await writeFile(resolve(packageRoot, "main.js"), "module.exports = {};\n");

    await expect(assertPackagePrivacy(packageRoot))
      .rejects.toThrow("embedded Worker privacy markers are missing: main.js");
  });

  it("does not let an approved synthetic fixture mask another finding", async () => {
    const sourceRoot = await createSourceFixture();
    await rm(resolve(sourceRoot, "test/source.test.ts"));
    await writeFile(
      resolve(sourceRoot, "test/cache-root.test.ts"),
      [
        `const approved = ${JSON.stringify(privateHome("u", "vault"))};`,
        `const forbidden = ${JSON.stringify(privateHome("actual-owner", "private"))};`,
        "",
      ].join("\n"),
    );

    const findings = await scanSourcePrivacy(sourceRoot);
    expect(findings).toEqual([
      expect.objectContaining({
        rule: "machine_path",
        path: "test/cache-root.test.ts",
        line: 2,
        match: privateHome("actual-owner", "private"),
      }),
    ]);
  });

  it.each([
    ["credential", bearerCredential],
    ["AI attribution", aiAttribution],
  ])("rejects packaged %s content", async (_name, forbiddenContent) => {
    const packageRoot = await createPackageFixture();
    await writeFile(resolve(packageRoot, "notice.txt"), `${forbiddenContent()}\n`);
    await expect(assertPackagePrivacy(packageRoot)).rejects.toThrow("package privacy policy rejected");
  });

  it("routes CI and every release candidate surface through the validator", async () => {
    const workflowRoot = resolve(repositoryRoot, "../../.github/workflows");
    const expectations = [
      ["ci.yml", "npm run validate:privacy"],
      ["release-plugin.yml", "npm run validate:privacy"],
      ["publish-plugin-release.yml", "npm run validate:privacy"],
      ["validate-d5c-brat.yml", 'npm run validate:privacy -- package "$package"'],
    ];
    for (const [name, command] of expectations) {
      const workflow = await readFile(resolve(workflowRoot, name), "utf8");
      expect(workflow).toContain(command);
    }

    const ci = await readFile(resolve(workflowRoot, "ci.yml"), "utf8");
    expect(ci).not.toContain("src test esbuild.config.mjs main.js");
    expect(ci).not.toContain(
      "! rg -n '(requestUrl|readDaemonToken|Authorization|tokenProvider)' src/worker",
    );
  });
});
