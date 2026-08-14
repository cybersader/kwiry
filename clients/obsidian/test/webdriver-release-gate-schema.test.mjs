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
      expected_result_selected: true,
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
    expect(sanitizedGateFailure("runtime_prepare_failed").failure_stage).toBe("runtime_prepare_failed");
    expect(sanitizedGateFailure("runtime_temp_prepare_failed").failure_stage).toBe("runtime_temp_prepare_failed");
    expect(sanitizedGateFailure("vault_prepare_failed").failure_stage).toBe("vault_prepare_failed");
    expect(sanitizedGateFailure("installer_prepare_failed").failure_stage).toBe("installer_prepare_failed");
    expect(sanitizedGateFailure("launcher_resolve_failed").failure_stage).toBe("launcher_resolve_failed");
    expect(sanitizedGateFailure("launcher_app_cache_failed").failure_stage).toBe("launcher_app_cache_failed");
    expect(sanitizedGateFailure("launcher_installer_cache_failed").failure_stage).toBe("launcher_installer_cache_failed");
    expect(sanitizedGateFailure("launcher_vault_setup_failed").failure_stage).toBe("launcher_vault_setup_failed");
    expect(sanitizedGateFailure("launcher_config_setup_failed").failure_stage).toBe("launcher_config_setup_failed");
    expect(sanitizedGateFailure("launcher_spawn_failed").failure_stage).toBe("launcher_spawn_failed");
    expect(sanitizedGateFailure("launcher_start_failed").failure_stage).toBe("launcher_start_failed");
    expect(sanitizedGateFailure("launch_process_exited").failure_stage).toBe("launch_process_exited");
    expect(sanitizedGateFailure("cdp_ready_timeout").failure_stage).toBe("cdp_ready_timeout");
    expect(sanitizedGateFailure("launch_dependency_missing").failure_stage).toBe("launch_dependency_missing");
    expect(sanitizedGateFailure("launch_shared_memory_unavailable").failure_stage).toBe("launch_shared_memory_unavailable");
    expect(sanitizedGateFailure("launch_runtime_file_missing").failure_stage).toBe("launch_runtime_file_missing");
    expect(sanitizedGateFailure("launch_session_bus_unavailable").failure_stage).toBe("launch_session_bus_unavailable");
    expect(sanitizedGateFailure("launch_subprocess_failed").failure_stage).toBe("launch_subprocess_failed");
    expect(sanitizedGateFailure("launch_display_unavailable").failure_stage).toBe("launch_display_unavailable");
    expect(sanitizedGateFailure("launch_sandbox_unavailable").failure_stage).toBe("launch_sandbox_unavailable");
    expect(sanitizedGateFailure("launch_gpu_unavailable").failure_stage).toBe("launch_gpu_unavailable");
    expect(sanitizedGateFailure("launch_instance_conflict").failure_stage).toBe("launch_instance_conflict");
    expect(sanitizedGateFailure("launch_singleton_socket_path_failed").failure_stage).toBe("launch_singleton_socket_path_failed");
    expect(sanitizedGateFailure("launch_crash_reporter_unavailable").failure_stage).toBe("launch_crash_reporter_unavailable");
    expect(sanitizedGateFailure("launch_runtime_resources_unavailable").failure_stage).toBe("launch_runtime_resources_unavailable");
    expect(sanitizedGateFailure("launch_platform_runtime_failed").failure_stage).toBe("launch_platform_runtime_failed");
    expect(sanitizedGateFailure("launch_v8_bootstrap_failed").failure_stage).toBe("launch_v8_bootstrap_failed");
    expect(sanitizedGateFailure("launch_electron_bootstrap_failed").failure_stage).toBe("launch_electron_bootstrap_failed");
    expect(sanitizedGateFailure("launch_browser_bootstrap_failed").failure_stage).toBe("launch_browser_bootstrap_failed");
    expect(sanitizedGateFailure("launch_node_bootstrap_failed").failure_stage).toBe("launch_node_bootstrap_failed");
    expect(sanitizedGateFailure("launch_process_model_failed").failure_stage).toBe("launch_process_model_failed");
    expect(sanitizedGateFailure("launch_process_cpu_metrics_failed").failure_stage).toBe("launch_process_cpu_metrics_failed");
    expect(sanitizedGateFailure("launch_process_memory_metrics_failed").failure_stage).toBe("launch_process_memory_metrics_failed");
    expect(sanitizedGateFailure("launch_process_metrics_failed").failure_stage).toBe("launch_process_metrics_failed");
    expect(sanitizedGateFailure("launch_process_spawn_runtime_failed").failure_stage).toBe("launch_process_spawn_runtime_failed");
    expect(sanitizedGateFailure("launch_process_handle_runtime_failed").failure_stage).toBe("launch_process_handle_runtime_failed");
    expect(sanitizedGateFailure("launch_process_enumeration_failed").failure_stage).toBe("launch_process_enumeration_failed");
    expect(sanitizedGateFailure("launch_process_scheduling_failed").failure_stage).toBe("launch_process_scheduling_failed");
    expect(sanitizedGateFailure("launch_process_identity_failed").failure_stage).toBe("launch_process_identity_failed");
    expect(sanitizedGateFailure("launch_process_platform_failed").failure_stage).toBe("launch_process_platform_failed");
    expect(sanitizedGateFailure("launch_thread_runtime_failed").failure_stage).toBe("launch_thread_runtime_failed");
    expect(sanitizedGateFailure("launch_sequence_runtime_failed").failure_stage).toBe("launch_sequence_runtime_failed");
    expect(sanitizedGateFailure("launch_task_runtime_failed").failure_stage).toBe("launch_task_runtime_failed");
    expect(sanitizedGateFailure("launch_run_loop_failed").failure_stage).toBe("launch_run_loop_failed");
    expect(sanitizedGateFailure("launch_blocking_runtime_failed").failure_stage).toBe("launch_blocking_runtime_failed");
    expect(sanitizedGateFailure("launch_event_loop_failed").failure_stage).toBe("launch_event_loop_failed");
    expect(sanitizedGateFailure("launch_feature_initialization_failed").failure_stage).toBe("launch_feature_initialization_failed");
    expect(sanitizedGateFailure("launch_filesystem_unavailable").failure_stage).toBe("launch_filesystem_unavailable");
    expect(sanitizedGateFailure("launch_memory_unavailable").failure_stage).toBe("launch_memory_unavailable");
    expect(sanitizedGateFailure("launch_ipc_unavailable").failure_stage).toBe("launch_ipc_unavailable");
    expect(sanitizedGateFailure("launch_argument_invalid").failure_stage).toBe("launch_argument_invalid");
    expect(sanitizedGateFailure("launch_network_runtime_failed").failure_stage).toBe("launch_network_runtime_failed");
    expect(sanitizedGateFailure("launch_proxy_runtime_failed").failure_stage).toBe("launch_proxy_runtime_failed");
    expect(sanitizedGateFailure("launch_network_monitor_failed").failure_stage).toBe("launch_network_monitor_failed");
    expect(sanitizedGateFailure("launch_socket_runtime_failed").failure_stage).toBe("launch_socket_runtime_failed");
    expect(sanitizedGateFailure("launch_devtools_server_failed").failure_stage).toBe("launch_devtools_server_failed");
    expect(sanitizedGateFailure("launch_socket_address_in_use").failure_stage).toBe("launch_socket_address_in_use");
    expect(sanitizedGateFailure("launch_socket_family_unavailable").failure_stage).toBe("launch_socket_family_unavailable");
    expect(sanitizedGateFailure("launch_socket_creation_failed").failure_stage).toBe("launch_socket_creation_failed");
    expect(sanitizedGateFailure("launch_dns_runtime_failed").failure_stage).toBe("launch_dns_runtime_failed");
    expect(sanitizedGateFailure("launch_security_runtime_failed").failure_stage).toBe("launch_security_runtime_failed");
    expect(sanitizedGateFailure("launch_ui_runtime_failed").failure_stage).toBe("launch_ui_runtime_failed");
    expect(sanitizedGateFailure("launch_permission_denied").failure_stage).toBe("launch_permission_denied");
    expect(sanitizedGateFailure("launch_runtime_assertion_failed").failure_stage).toBe("launch_runtime_assertion_failed");
    expect(sanitizedGateFailure("launch_runtime_fatal").failure_stage).toBe("launch_runtime_fatal");
    expect(sanitizedGateFailure("launch_process_clean_exit").failure_stage).toBe("launch_process_clean_exit");
    expect(sanitizedGateFailure("launch_process_error_exit").failure_stage).toBe("launch_process_error_exit");
    expect(sanitizedGateFailure("launch_process_signaled").failure_stage).toBe("launch_process_signaled");
    expect(sanitizedGateFailure("launch_process_aborted").failure_stage).toBe("launch_process_aborted");
    expect(sanitizedGateFailure("launch_process_bus_error").failure_stage).toBe("launch_process_bus_error");
    expect(sanitizedGateFailure("launch_process_arithmetic_fault").failure_stage).toBe("launch_process_arithmetic_fault");
    expect(sanitizedGateFailure("launch_process_illegal_instruction").failure_stage).toBe("launch_process_illegal_instruction");
    expect(sanitizedGateFailure("launch_process_killed").failure_stage).toBe("launch_process_killed");
    expect(sanitizedGateFailure("launch_process_segmentation_fault").failure_stage).toBe("launch_process_segmentation_fault");
    expect(sanitizedGateFailure("launch_process_terminated").failure_stage).toBe("launch_process_terminated");
    expect(sanitizedGateFailure("launch_process_trapped").failure_stage).toBe("launch_process_trapped");
    expect(sanitizedGateFailure("webdriver_attach_failed").failure_stage).toBe("webdriver_attach_failed");
    expect(sanitizedGateFailure("scenario_execution_failed").failure_stage).toBe("scenario_execution_failed");
    expect(sanitizedGateFailure("scenario_plugin_ready_failed").failure_stage).toBe("scenario_plugin_ready_failed");
    expect(sanitizedGateFailure("scenario_command_registration_failed").failure_stage).toBe("scenario_command_registration_failed");
    expect(sanitizedGateFailure("scenario_leaf_ready_failed").failure_stage).toBe("scenario_leaf_ready_failed");
    expect(sanitizedGateFailure("scenario_instrumentation_failed").failure_stage).toBe("scenario_instrumentation_failed");
    expect(sanitizedGateFailure("scenario_state_setup_failed").failure_stage).toBe("scenario_state_setup_failed");
    expect(sanitizedGateFailure("scenario_open_hook_failed").failure_stage).toBe("scenario_open_hook_failed");
    expect(sanitizedGateFailure("scenario_notice_observer_failed").failure_stage).toBe("scenario_notice_observer_failed");
    expect(sanitizedGateFailure("scenario_search_command_failed").failure_stage).toBe("scenario_search_command_failed");
    expect(sanitizedGateFailure("scenario_window_focus_failed").failure_stage).toBe("scenario_window_focus_failed");
    expect(sanitizedGateFailure("scenario_palette_shortcut_failed").failure_stage).toBe("scenario_palette_shortcut_failed");
    expect(sanitizedGateFailure("scenario_palette_input_failed").failure_stage).toBe("scenario_palette_input_failed");
    expect(sanitizedGateFailure("scenario_command_text_failed").failure_stage).toBe("scenario_command_text_failed");
    expect(sanitizedGateFailure("scenario_command_lookup_failed").failure_stage).toBe("scenario_command_lookup_failed");
    expect(sanitizedGateFailure("scenario_command_click_failed").failure_stage).toBe("scenario_command_click_failed");
    expect(sanitizedGateFailure("scenario_result_lookup_failed").failure_stage).toBe("scenario_result_lookup_failed");
    expect(sanitizedGateFailure("scenario_result_activation_failed").failure_stage).toBe("scenario_result_activation_failed");
    expect(sanitizedGateFailure("scenario_open_settlement_failed").failure_stage).toBe("scenario_open_settlement_failed");
    expect(sanitizedGateFailure("scenario_modal_close_failed").failure_stage).toBe("scenario_modal_close_failed");
    expect(sanitizedGateFailure("scenario_isolation_search_failed").failure_stage).toBe("scenario_isolation_search_failed");
    expect(sanitizedGateFailure("scenario_observation_failed").failure_stage).toBe("scenario_observation_failed");
    expect(sanitizedGateFailure("expected_file_inactive").failure_stage).toBe("expected_file_inactive");
    expect(sanitizedGateFailure("private details").failure_stage).toBe("unexpected_failure");
  });
});
