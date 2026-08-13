// SPDX-FileCopyrightText: 2026 cybersader
// SPDX-License-Identifier: GPL-3.0-only

import { describe, expect, it } from "vitest";

import {
  sanitizedGateFailure,
  validateWebdriverReleaseEvidence,
} from "../scripts/webdriver-release-gate-schema.mjs";

const HASH = "a".repeat(64);

function validEvidence() {
  return {
    schema_version: 1,
    kind: "kwiry_obsidian_webdriver_release_gate",
    verdict: "SELENIUM_RELEASE_GATE_PASSED",
    scope: "narrow_real_obsidian_selection_lifecycle",
    candidate: {
      version: "0.6.0-beta.15",
      candidate_set_sha256: HASH,
      file_count: 11,
    },
    runtime_manifest: { sha256: HASH },
    runtime: {
      obsidian: "1.13.7",
      electron: "43.3.0",
      chromium: "150.0.7871.212",
      driver: "150.0.7871.212",
      selenium_webdriver: "4.39.0",
      obsidian_launcher: "3.1.1",
      node: "22.23.1",
      platform: "linux-x64-xvfb",
    },
    isolation: {
      private_state_root: true,
      loopback_cdp: true,
      loopback_webdriver: true,
      selenium_manager_used: false,
      system_browser_used: false,
      system_driver_used: false,
    },
    scenario: {
      synthetic_xlsm: true,
      excel_explicitly_enabled: true,
      command_palette_used: true,
      webdriver_input_used: true,
      native_click_used: true,
      modal_closed: true,
      stale_notices: 0,
      open_failure_notices: 0,
      open_file_calls: 1,
      open_file_promise: "resolved",
      expected_file_active: true,
      vba_payload_search_results: 0,
    },
    cleanup: {
      webdriver_quit: true,
      obsidian_reaped: true,
      verified_download_server_closed: true,
      ports_closed: true,
      private_state_removed: true,
    },
    privacy: {
      aggregate_only: true,
      paths_emitted: 0,
      queries_emitted: 0,
      note_content_emitted: 0,
      notice_text_emitted: 0,
      raw_logs_emitted: 0,
      screenshots_emitted: 0,
      stack_traces_emitted: 0,
    },
  };
}

describe("WebDriver release evidence schema", () => {
  it("accepts only the narrow passing evidence envelope", () => {
    expect(validateWebdriverReleaseEvidence(validEvidence())).toEqual(validEvidence());
  });

  it.each([
    ["unknown key", (value) => { value.extra = true; }],
    ["missing key", (value) => { delete value.cleanup.ports_closed; }],
    ["failed cleanup", (value) => { value.cleanup.private_state_removed = false; }],
    ["stale notice", (value) => { value.scenario.stale_notices = 1; }],
    ["duplicate open", (value) => { value.scenario.open_file_calls = 2; }],
    ["broad verdict", (value) => { value.verdict = "GO_ACCEPTED"; }],
    ["private path key", (value) => { value.runtime.binary_path = "fixture"; }],
    ["private path value", (value) => { value.runtime.node = ["/", "home", "private"].join("/"); }],
  ])("rejects %s mutations", (_name, mutate) => {
    const evidence = validEvidence();
    mutate(evidence);
    expect(() => validateWebdriverReleaseEvidence(evidence)).toThrow();
  });

  it("maps raw failures to a fixed closed stage vocabulary", () => {
    expect(sanitizedGateFailure("open_not_invoked")).toEqual({
      schema_version: 1,
      kind: "kwiry_obsidian_webdriver_release_gate_failure",
      status: "failed",
      failure_stage: "open_not_invoked",
    });
    expect(sanitizedGateFailure("private details").failure_stage).toBe("unexpected_failure");
  });
});
