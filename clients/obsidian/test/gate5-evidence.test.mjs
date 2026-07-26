// SPDX-FileCopyrightText: 2026 cybersader
// SPDX-License-Identifier: GPL-3.0-only

import { describe, expect, it } from "vitest";

import {
  GATE5_TARGETS,
  assertPrivacySafeEvidence,
  validateGate5AutomatedEvidence,
  validateGate5GeneratedPerformanceEvidence,
} from "../scripts/gate5-evidence-schema.mjs";

const HASH = "a".repeat(64);

function validEvidence() {
  return {
    schema_version: 1,
    kind: "kwiry_gate5_automated_evidence",
    verdict: "AUTOMATED_CHECKS_PASSED_OWNER_REVIEW_REQUIRED",
    automation_scope: "artifact_and_generated_functional_corpus",
    protocol_version: 2,
    artifact: {
      main: { bytes: 1, sha256: HASH },
      worker: { bytes: 1, sha256: HASH },
      rust_wasm: { bytes: 1, sha256: HASH },
      sqlite_wasm: { bytes: 1, sha256: HASH },
      deterministic: true,
      wasm_inputs: 2,
      loose_runtime_assets: 0,
    },
    corpus: {
      kind: "generated_functional",
      note_count: 14,
      markdown_bytes: 1_024,
      sha256: HASH,
      hash_algorithm: "sha256-path-nul-decimal-length-nul-bytes-nul-v1",
      expected_documents: 14,
    },
    checks: { total: 16, failed: 0 },
    targets: GATE5_TARGETS.map(([id, threshold, unit]) => ({
      id,
      threshold,
      unit,
      status: "not_measured",
      value: null,
      scope: "declared_reference_hardware",
    })),
    privacy: {
      aggregate_only: true,
      paths_emitted: 0,
      vault_names_emitted: 0,
      note_content_emitted: 0,
      raw_queries_emitted: 0,
      tokens_emitted: 0,
      stack_traces_emitted: 0,
      sql_emitted: 0,
      environment_paths_emitted: 0,
      private_corpus_hashes_emitted: 0,
      loose_evidence_artifacts: 0,
    },
  };
}

function validPerformanceEvidence() {
  const measurements = {
    worker_initialize_ms: 100,
    first_batch_ms: 50,
    build_duration_ms: 20_000,
    warm_search_p95_ms: 10,
    hydration_p95_ms: 15,
    update_visibility_p95_ms: 20,
    max_event_loop_delay_ms: 5,
    added_rss_mib: 350,
  };
  const measurementKeys = new Map([
    ["build_duration", "build_duration_ms"],
    ["warm_search_p95", "warm_search_p95_ms"],
    ["hydration_p95", "hydration_p95_ms"],
    ["update_visibility_p95", "update_visibility_p95_ms"],
    ["max_event_loop_delay", "max_event_loop_delay_ms"],
    ["added_steady_state_memory", "added_rss_mib"],
  ]);
  return {
    schema_version: 1,
    kind: "kwiry_gate5_generated_performance",
    verdict: "EVIDENCE_CAPTURE_COMPLETE_OWNER_DECISION_REQUIRED",
    host: "node_worker_threads",
    artifact: {
      worker: { bytes: 1, sha256: HASH },
      rust_wasm: { bytes: 1, sha256: HASH },
      sqlite_wasm: { bytes: 1, sha256: HASH },
    },
    corpus: {
      kind: "generated_performance",
      note_count: 10_000,
      markdown_bytes: 50 * 1024 * 1024,
      sha256: HASH,
      hash_algorithm: "sha256-path-nul-decimal-length-nul-bytes-nul-v1",
      expected_documents: 10_000,
      seed_u32: 0x4b574952,
    },
    index: { documents: 10_000, chunks: 50_000 },
    measurements,
    samples: { warm_search: 40, hydration: 20, update_visibility: 20 },
    targets: GATE5_TARGETS.map(([id, threshold, unit]) => {
      const measurementKey = measurementKeys.get(id);
      const value = measurementKey ? measurements[measurementKey] : null;
      return {
        id,
        threshold,
        unit,
        status: measurementKey ? (value <= threshold ? "met" : "missed") : "unavailable",
        value,
        scope: measurementKey
          ? "generated_node_worker_threads"
          : "installed_obsidian_reference_hardware",
      };
    }),
    privacy: {
      aggregate_only: true,
      paths_emitted: 0,
      vault_names_emitted: 0,
      note_content_emitted: 0,
      raw_queries_emitted: 0,
      tokens_emitted: 0,
      stack_traces_emitted: 0,
      sql_emitted: 0,
      environment_paths_emitted: 0,
      private_corpus_hashes_emitted: 0,
      loose_evidence_artifacts: 0,
    },
  };
}

describe("Gate 5 evidence schema", () => {
  it("accepts only the exact aggregate automated evidence shape", () => {
    expect(validateGate5AutomatedEvidence(validEvidence())).toMatchObject({
      verdict: "AUTOMATED_CHECKS_PASSED_OWNER_REVIEW_REQUIRED",
      checks: { failed: 0 },
    });
  });

  it("rejects unknown fields and incomplete target sets", () => {
    const unknown = validEvidence();
    unknown.extra = true;
    expect(() => validateGate5AutomatedEvidence(unknown)).toThrow("unexpected or missing keys");

    const missingTarget = validEvidence();
    missingTarget.targets.pop();
    expect(() => validateGate5AutomatedEvidence(missingTarget)).toThrow("every Gate 5 target");
  });

  it("rejects owner-decision wording and inconsistent verdicts", () => {
    const delivered = validEvidence();
    delivered.verdict = "DELIVERED";
    expect(() => validateGate5AutomatedEvidence(delivered)).toThrow("invalid automated evidence verdict");

    const failed = validEvidence();
    failed.checks.failed = 1;
    expect(() => validateGate5AutomatedEvidence(failed)).toThrow("verdict does not match");
  });

  it.each([
    ["private path", { label: "/home/example/private.md" }],
    ["Windows path", { label: "C:\\Users\\Example\\vault" }],
    ["credential", { label: `Bearer ${"A".repeat(43)}` }],
    ["SQL", { label: "SELECT content FROM chunks" }],
    ["stack", { label: "Error: failed\n    at file.js:1:2" }],
    ["forbidden key", { raw_query: "synthetic" }],
  ])("rejects %s leakage", (_name, value) => {
    expect(() => assertPrivacySafeEvidence(value)).toThrow();
  });

  it("allows generated hashes but forbids private corpus hashes", () => {
    expect(() => assertPrivacySafeEvidence({ corpus: { sha256: HASH } })).not.toThrow();
    expect(() => assertPrivacySafeEvidence(
      { corpus: { sha256: HASH } },
      { privateCorpus: true },
    )).toThrow("private corpus hashes");
  });

  it("rejects invalid numbers rather than serializing NaN or impossible counts", () => {
    const nan = validEvidence();
    nan.targets[0].status = "met";
    nan.targets[0].value = Number.NaN;
    expect(() => validateGate5AutomatedEvidence(nan)).toThrow("invalid target value");

    const count = validEvidence();
    count.checks.failed = 17;
    expect(() => validateGate5AutomatedEvidence(count)).toThrow("invalid failed check count");
  });

  it("accepts exact generated performance evidence while preserving target misses", () => {
    const evidence = validPerformanceEvidence();
    expect(validateGate5GeneratedPerformanceEvidence(evidence)).toMatchObject({
      verdict: "EVIDENCE_CAPTURE_COMPLETE_OWNER_DECISION_REQUIRED",
      targets: expect.arrayContaining([
        expect.objectContaining({ id: "added_steady_state_memory", status: "missed" }),
      ]),
    });
  });

  it("rejects invented installed-host measurements and inconsistent performance targets", () => {
    const invented = validPerformanceEvidence();
    invented.targets[0] = {
      ...invented.targets[0],
      status: "met",
      value: 50,
      scope: "generated_node_worker_threads",
    };
    expect(() => validateGate5GeneratedPerformanceEvidence(invented))
      .toThrow("targets.0.status");

    const inconsistent = validPerformanceEvidence();
    const memory = inconsistent.targets.find((target) =>
      target.id === "added_steady_state_memory"
    );
    memory.status = "met";
    expect(() => validateGate5GeneratedPerformanceEvidence(inconsistent))
      .toThrow("targets.7.status");
  });
});
