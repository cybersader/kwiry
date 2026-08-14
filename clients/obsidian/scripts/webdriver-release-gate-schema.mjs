// SPDX-FileCopyrightText: 2026 cybersader
// SPDX-License-Identifier: GPL-3.0-only

const HASH = /^[a-f0-9]{64}$/u;
const SEMVER = /^[0-9]+\.[0-9]+\.[0-9]+(?:-[0-9A-Za-z.-]+)?(?:\+[0-9A-Za-z.-]+)?$/u;
const VERSION = /^[0-9]+(?:\.[0-9]+){1,3}(?:-[0-9A-Za-z.-]+)?$/u;
const FORBIDDEN_KEY = /(?:^|_)(?:path|query|message|error|sql|token|url|port|pid)(?:_|$)/iu;
const FORBIDDEN_TEXT = /(?:\/home\/|\/Users\/|[A-Z]:\\Users\\|Bearer\s|sk_|ghp_|github_pat_|Co-Authored-By:|\b(?:GO|ACCEPTED|DELIVERED)\b)/iu;

export const WEBDRIVER_EVIDENCE_KIND = "kwiry_obsidian_webdriver_release_gate";
export const WEBDRIVER_EVIDENCE_VERDICT = "SELENIUM_RELEASE_GATE_PASSED";

export function validateWebdriverReleaseEvidence(value) {
  const root = object(value, [
    "schema_version", "kind", "verdict", "scope", "candidate", "runtime_manifest",
    "runtime", "isolation", "scenario", "cleanup", "privacy",
  ]);
  equal(root.schema_version, 1, "schema_version");
  equal(root.kind, WEBDRIVER_EVIDENCE_KIND, "kind");
  equal(root.verdict, WEBDRIVER_EVIDENCE_VERDICT, "verdict");
  equal(root.scope, "narrow_real_obsidian_selection_lifecycle", "scope");

  const candidate = object(root.candidate, ["version", "candidate_set_sha256", "file_count"]);
  text(candidate.version, SEMVER, "candidate.version");
  text(candidate.candidate_set_sha256, HASH, "candidate.candidate_set_sha256");
  integer(candidate.file_count, 1, 64, "candidate.file_count");

  const runtimeManifest = object(root.runtime_manifest, ["sha256"]);
  text(runtimeManifest.sha256, HASH, "runtime_manifest.sha256");

  const runtime = object(root.runtime, [
    "obsidian", "electron", "chromium", "driver", "selenium_webdriver",
    "obsidian_launcher", "node", "platform",
  ]);
  for (const key of [
    "obsidian", "electron", "chromium", "driver", "selenium_webdriver", "obsidian_launcher", "node",
  ]) text(runtime[key], VERSION, `runtime.${key}`);
  equal(runtime.platform, "linux-x64-xvfb", "runtime.platform");

  const isolation = object(root.isolation, [
    "private_state_root", "loopback_cdp", "loopback_webdriver", "selenium_manager_used",
    "system_browser_used", "system_driver_used",
  ]);
  requiredTrue(isolation.private_state_root, "isolation.private_state_root");
  requiredTrue(isolation.loopback_cdp, "isolation.loopback_cdp");
  requiredTrue(isolation.loopback_webdriver, "isolation.loopback_webdriver");
  requiredFalse(isolation.selenium_manager_used, "isolation.selenium_manager_used");
  requiredFalse(isolation.system_browser_used, "isolation.system_browser_used");
  requiredFalse(isolation.system_driver_used, "isolation.system_driver_used");

  const scenario = object(root.scenario, [
    "synthetic_xlsm", "excel_explicitly_enabled", "command_palette_used",
    "webdriver_input_used", "native_click_used", "modal_closed", "stale_notices",
    "open_failure_notices", "open_file_calls", "open_file_promise", "expected_file_active",
    "vba_payload_search_results",
  ]);
  for (const key of [
    "synthetic_xlsm", "excel_explicitly_enabled", "command_palette_used",
    "webdriver_input_used", "native_click_used", "modal_closed", "expected_file_active",
  ]) requiredTrue(scenario[key], `scenario.${key}`);
  for (const key of ["stale_notices", "open_failure_notices", "vba_payload_search_results"]) {
    equal(scenario[key], 0, `scenario.${key}`);
  }
  equal(scenario.open_file_calls, 1, "scenario.open_file_calls");
  equal(scenario.open_file_promise, "resolved", "scenario.open_file_promise");

  const cleanup = object(root.cleanup, [
    "webdriver_quit", "obsidian_reaped", "verified_download_server_closed",
    "ports_closed", "private_state_removed",
  ]);
  for (const key of Object.keys(cleanup)) requiredTrue(cleanup[key], `cleanup.${key}`);

  const privacy = object(root.privacy, [
    "aggregate_only", "paths_emitted", "queries_emitted", "note_content_emitted",
    "notice_text_emitted", "raw_logs_emitted", "screenshots_emitted", "stack_traces_emitted",
  ]);
  requiredTrue(privacy.aggregate_only, "privacy.aggregate_only");
  for (const key of Object.keys(privacy).filter((key) => key !== "aggregate_only")) {
    equal(privacy[key], 0, `privacy.${key}`);
  }
  rejectForbiddenData(root);
  return root;
}

export function sanitizedGateFailure(code) {
  const allowed = new Set([
    "candidate_invalid", "runtime_manifest_invalid", "download_identity_mismatch",
    "runtime_prepare_failed", "vault_prepare_failed", "installer_prepare_failed",
    "launcher_resolve_failed", "launcher_app_cache_failed", "launcher_installer_cache_failed",
    "launcher_vault_setup_failed", "launcher_config_setup_failed", "launcher_spawn_failed",
    "launcher_start_failed", "launch_process_exited", "cdp_ready_timeout",
    "launch_dependency_missing", "launch_display_unavailable", "launch_sandbox_unavailable",
    "launch_gpu_unavailable", "launch_instance_conflict", "launch_process_clean_exit",
    "launch_process_error_exit", "launch_process_signaled", "launch_failed", "webdriver_attach_failed",
    "result_not_rendered", "stale_notice_observed", "open_not_invoked", "open_promise_rejected",
    "cleanup_incomplete", "unexpected_failure",
  ]);
  return {
    schema_version: 1,
    kind: "kwiry_obsidian_webdriver_release_gate_failure",
    status: "failed",
    failure_stage: allowed.has(code) ? code : "unexpected_failure",
  };
}

function object(value, keys) {
  if (typeof value !== "object" || value === null || Array.isArray(value)) fail("object_invalid");
  const actual = Object.keys(value).sort();
  const expected = [...keys].sort();
  if (JSON.stringify(actual) !== JSON.stringify(expected)) fail("keys_invalid");
  return value;
}

function text(value, pattern, label) {
  if (typeof value !== "string" || !pattern.test(value)) fail(`${label}_invalid`);
}

function integer(value, min, max, label) {
  if (!Number.isInteger(value) || value < min || value > max) fail(`${label}_invalid`);
}

function equal(actual, expected, label) {
  if (actual !== expected) fail(`${label}_invalid`);
}

function requiredTrue(value, label) {
  equal(value, true, label);
}

function requiredFalse(value, label) {
  equal(value, false, label);
}

function rejectForbiddenData(value) {
  const visit = (current) => {
    if (typeof current === "string" && FORBIDDEN_TEXT.test(current)) fail("forbidden_text");
    if (typeof current !== "object" || current === null) return;
    for (const [key, child] of Object.entries(current)) {
      if (FORBIDDEN_KEY.test(key)) fail("forbidden_key");
      visit(child);
    }
  };
  visit(value);
}

function fail(code) {
  throw new Error(code);
}

if (process.argv[1] === new URL(import.meta.url).pathname) {
  const { readFile } = await import("node:fs/promises");
  const path = process.argv[2];
  if (!path || process.argv.length !== 3) throw new Error("usage_invalid");
  validateWebdriverReleaseEvidence(JSON.parse(await readFile(path, "utf8")));
}
