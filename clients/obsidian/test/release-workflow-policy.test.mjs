// SPDX-FileCopyrightText: 2026 cybersader
// SPDX-License-Identifier: GPL-3.0-only

import { readFile } from "node:fs/promises";
import { resolve } from "node:path";
import { describe, expect, it } from "vitest";

const repositoryRoot = resolve(import.meta.dirname, "../../..");
const workflowRoot = resolve(repositoryRoot, ".github/workflows");

async function workflow(name) {
  return readFile(resolve(workflowRoot, name), "utf8");
}

describe("Obsidian release workflow policy", () => {
  it("binds candidate validation to one explicit successful CI run and exact tag commit", async () => {
    const source = await workflow("release-plugin.yml");
    expect(source).toContain("ci_run_id:");
    expect(source).toContain("actions/runs/${CI_RUN_ID}");
    expect(source).toContain("test \"$GITHUB_REF\" = \"refs/heads/${default_branch}\"");
    expect(source).toContain("test \"$GITHUB_SHA\" = \"$trusted_sha\"");
    expect(source).toContain('git merge-base --is-ancestor "$tag_commit" "$GITHUB_SHA"');
    expect(source).toContain("test \"$(jq -r '.head_sha' <<<\"$run\")\" = \"$tag_commit\"");
    expect(source).not.toContain("workflow_runs[]");
    expect(source).not.toContain("grep -qvx 'success'");
  });

  it("runs real Xvfb Selenium only after preparing the exact candidate and validates evidence before upload", async () => {
    const source = await workflow("release-plugin.yml");
    const prepared = source.indexOf("name: Prepare exact candidate package");
    const windowManager = source.indexOf("name: Install pinned X11 window manager");
    const webdriver = source.indexOf("name: Run pinned real Obsidian WebDriver gate");
    const validated = source.indexOf("npm run validate:webdriver:evidence");
    const handoff = source.indexOf("name: Construct immutable tested-candidate handoff");
    const upload = source.indexOf("name: Upload exact validated candidate handoff");
    expect(prepared).toBeGreaterThan(-1);
    expect(windowManager).toBeGreaterThan(prepared);
    expect(webdriver).toBeGreaterThan(windowManager);
    expect(source).toContain("herbstluftwm=0.9.5-3");
    expect(source).toContain("herbstluftwm >/dev/null 2>&1 &");
    expect(source).toContain("trap cleanup_wm EXIT");
    expect(source).toContain("+extension GLX -noreset");
    expect(source).toContain('stage=$(mktemp -d "${GITHUB_WORKSPACE}.kwiry-webdriver-stage.XXXXXX")');
    expect(source).toContain('trap cleanup_stage EXIT');
    expect(source).toContain('mkdir "$stage/repository"');
    expect(source).toContain('cp -a "$GITHUB_WORKSPACE/." "$stage/repository"');
    expect(source).not.toContain('stage=$(mktemp -d "${GITHUB_WORKSPACE}/');
    expect(source).toContain('for gate_module in webdriver-release-gate.mjs webdriver-release-gate-schema.mjs');
    expect(source).toContain('git show "${GITHUB_SHA}:clients/obsidian/scripts/${gate_module}"');
    expect(source).toContain('KWIRY_WEBDRIVER_RUNTIME_ASSETS="$stage/runtime-assets"');
    expect(source).toContain('KWIRY_WEBDRIVER_PRIVATE_ROOT="$stage/private"');
    expect(source).toContain('cd "$stage/repository/clients/obsidian"');
    expect(source).toContain('cp "$PWD/.tmp/webdriver.evidence.json" "$GITHUB_WORKSPACE/clients/obsidian/.tmp/webdriver.evidence.json"');
    expect(source).toContain("xvfb-run --auto-servernum");
    expect(source).toContain("webdriver-release-gate-manifest.json");
    expect(validated).toBeGreaterThan(webdriver);
    expect(handoff).toBeGreaterThan(validated);
    expect(upload).toBeGreaterThan(handoff);
  });

  it("pins release-lane actions and prevents checkout credential persistence", async () => {
    const source = await workflow("release-plugin.yml");
    expect(source).toContain("actions/checkout@11d5960a326750d5838078e36cf38b85af677262");
    expect(source).toContain("actions/setup-node@49933ea5288caeca8642d1e84afbd3f7d6820020");
    expect(source).toContain("Swatinem/rust-cache@6323deb102c322ba6fcbdcafc7e3dddab59af2b6");
    expect(source).toContain("actions/upload-artifact@ea165f8d65b6e75b540449e92b4886f43607fa02");
    expect(source).toContain("persist-credentials: false");
    expect(source).not.toMatch(/actions\/(?:checkout|setup-node|upload-artifact)@v\d/u);
  });

  it("keeps ordinary CI free of proprietary real-host downloads", async () => {
    const source = await workflow("ci.yml");
    expect(source).not.toContain("webdriver:release");
    expect(source).not.toContain("webdriver-release-gate-manifest.json");
    expect(source).not.toContain("obsidian-releases/releases/download");
    expect(source).not.toContain("xvfb-run");
  });

  it("makes the protected publisher a data-only exact-artifact consumer", async () => {
    const source = await workflow("publish-plugin-release.yml");
    expect(source).toContain("environment: obsidian-release");
    expect(source).toContain("test \"$GITHUB_REF\" = \"refs/heads/${default_branch}\"");
    expect(source).toContain("test \"$GITHUB_SHA\" = \"$trusted_sha\"");
    expect(source).toContain("candidate_run_id:");
    expect(source).toContain("candidate_artifact_id:");
    expect(source).toContain("candidate_artifact_digest:");
    expect(source).toContain("actions/artifacts/${CANDIDATE_ARTIFACT_ID}");
    expect(source).toContain("actions/workflows/release-plugin.yml");
    expect(source).toContain("test \"$(jq -r '.workflow_id' <<<\"$run\")\" = \"$(jq -r '.id' <<<\"$workflow\")\"");
    expect(source).toContain("test \"$(jq -r '.head_branch' <<<\"$run\")\" = \"$default_branch\"");
    expect(source).toContain("test \"$(jq -r '.head_sha' <<<\"$run\")\" = \"$trusted_sha\"");
    expect(source).toContain("test \"$(jq -r '.workflow_run.id' <<<\"$artifact\")\" = \"$CANDIDATE_RUN_ID\"");
    expect(source).toContain("test \"$(jq -r '.digest' <<<\"$artifact\")\" = \"$CANDIDATE_ARTIFACT_DIGEST\"");
    expect(source).not.toContain("actions/checkout@");
    expect(source).not.toContain("actions/setup-node@");
    expect(source).not.toContain("dtolnay/rust-toolchain@");
    expect(source).not.toContain("Swatinem/rust-cache@");
    expect(source).not.toMatch(/\bnpm (?:ci|run|test)\b/u);
    expect(source).not.toMatch(/\bcargo\b/u);
    expect(source).not.toContain("release-candidate-handoff.mjs");
  });

  it("creates a draft, verifies uploaded asset bytes, then publishes", async () => {
    const source = await workflow("publish-plugin-release.yml");
    const draft = source.indexOf("--draft");
    const verify = source.indexOf("name: Verify uploaded release assets and publish");
    const publish = source.indexOf("--draft=false");
    expect(draft).toBeGreaterThan(-1);
    expect(verify).toBeGreaterThan(draft);
    expect(source.split('test "$current_commit" = "$AUTHORIZED_TAG_COMMIT"')).toHaveLength(3);
    expect(source).toContain("uploaded release asset identity invalid");
    expect(publish).toBeGreaterThan(verify);
  });
});
