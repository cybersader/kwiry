// SPDX-FileCopyrightText: 2026 cybersader
// SPDX-License-Identifier: GPL-3.0-only

export const GATE5_TARGETS = [
  ["async_start", 100, "ms"],
  ["first_progress", 500, "ms"],
  ["build_duration", 30_000, "ms"],
  ["warm_search_p95", 100, "ms"],
  // `warm_search_p95` stops at the Worker RPC boundary. Since the index became
  // contentless, excerpt text is produced on the host instead, so that boundary
  // is no longer the whole cost of a search: this target covers the host-side
  // read-and-hydrate step. The user-visible figure is the sum of the two, and
  // the owner owns that combined number.
  ["hydration_p95", 100, "ms"],
  ["update_visibility_p95", 300, "ms"],
  ["max_event_loop_delay", 100, "ms"],
  ["added_steady_state_memory", 300, "mib"],
];

const AUTOMATED_KEYS = [
  "schema_version",
  "kind",
  "verdict",
  "automation_scope",
  "protocol_version",
  "artifact",
  "corpus",
  "checks",
  "targets",
  "privacy",
];
const IDENTITY_KEYS = ["bytes", "sha256"];
const ARTIFACT_KEYS = [
  "main",
  "worker",
  "rust_wasm",
  "sqlite_wasm",
  "deterministic",
  "wasm_inputs",
  "loose_runtime_assets",
];
const CORPUS_KEYS = [
  "kind",
  "note_count",
  "markdown_bytes",
  "sha256",
  "hash_algorithm",
  "expected_documents",
];
const CHECK_KEYS = ["total", "failed"];
const TARGET_KEYS = ["id", "threshold", "unit", "status", "value", "scope"];
const PRIVACY_KEYS = [
  "aggregate_only",
  "paths_emitted",
  "vault_names_emitted",
  "note_content_emitted",
  "raw_queries_emitted",
  "tokens_emitted",
  "stack_traces_emitted",
  "sql_emitted",
  "environment_paths_emitted",
  "private_corpus_hashes_emitted",
  "loose_evidence_artifacts",
];
const GENERATED_PERFORMANCE_KEYS = [
  "schema_version",
  "kind",
  "verdict",
  "host",
  "artifact",
  "corpus",
  "index",
  "measurements",
  "samples",
  "targets",
  "privacy",
];
const PERFORMANCE_ARTIFACT_KEYS = ["worker", "rust_wasm", "sqlite_wasm"];
const PERFORMANCE_CORPUS_KEYS = [
  "kind",
  "note_count",
  "markdown_bytes",
  "sha256",
  "hash_algorithm",
  "expected_documents",
  "seed_u32",
];
const PERFORMANCE_INDEX_KEYS = ["documents", "chunks"];
const PERFORMANCE_MEASUREMENT_KEYS = [
  "worker_initialize_ms",
  "first_batch_ms",
  "build_duration_ms",
  "warm_search_p95_ms",
  // Named `hydration_*`, not `excerpt_*`: the privacy key filter rejects any
  // key mentioning excerpts, and this is a duration, never excerpt text.
  "hydration_p95_ms",
  "update_visibility_p95_ms",
  "max_event_loop_delay_ms",
  "added_rss_mib",
];
const PERFORMANCE_SAMPLE_KEYS = ["warm_search", "hydration", "update_visibility"];

const FORBIDDEN_KEY = /(?:^|_)(?:path|query|token|secret|authorization|content|excerpt|stack|sql|vault_name|note_name|error|message)(?:_|$)/iu;
const PRIVATE_PATH = /(?:\/home\/|\/Users\/|\/mnt\/[a-z]\/|[A-Z]:\\Users\\|\\\\|file:\/\/|(?:^|\s)~\/|(?:^|[\\/])\.\.(?:[\\/]|$)|\.obsidian[\\/])/u;
const TOKEN = /(?:bearer\s+[A-Za-z0-9._~+/-]{12,}|gh[pousr]_[A-Za-z0-9]{20,}|api[_-]?key\s*[:=]|secret\s*[:=])/iu;
const SQL = /\b(?:SELECT|INSERT|UPDATE|DELETE|CREATE|DROP|ALTER|PRAGMA|MATCH|BEGIN|COMMIT|ROLLBACK)\b/iu;
const STACK = /(?:\b(?:Aggregate)?Error:|\bat\s+[^\n]+:\d+:\d+|Caused by:)/u;
const OWNER_VERDICT = /(?:^|_)(?:GO|NO_GO|APPROVED|ACCEPTED|DELIVERED|READY_FOR_RELEASE)(?:_|$)/u;

export function validateGate5AutomatedEvidence(value) {
  requireRecord(value, "evidence");
  exactKeys(value, AUTOMATED_KEYS, "evidence");
  requireEqual(value.schema_version, 1, "schema_version");
  requireEqual(value.kind, "kwiry_gate5_automated_evidence", "kind");
  if (value.verdict !== "AUTOMATED_CHECKS_PASSED_OWNER_REVIEW_REQUIRED"
    && value.verdict !== "AUTOMATED_CHECKS_FAILED") {
    throw new Error("invalid automated evidence verdict");
  }
  requireEqual(value.automation_scope, "artifact_and_generated_functional_corpus", "automation_scope");
  requireEqual(value.protocol_version, 2, "protocol_version");

  requireRecord(value.artifact, "artifact");
  exactKeys(value.artifact, ARTIFACT_KEYS, "artifact");
  for (const key of ["main", "worker", "rust_wasm", "sqlite_wasm"]) {
    validateIdentity(value.artifact[key], `artifact.${key}`);
  }
  requireBoolean(value.artifact.deterministic, "artifact.deterministic");
  requireEqual(value.artifact.wasm_inputs, 2, "artifact.wasm_inputs");
  requireEqual(value.artifact.loose_runtime_assets, 0, "artifact.loose_runtime_assets");

  requireRecord(value.corpus, "corpus");
  exactKeys(value.corpus, CORPUS_KEYS, "corpus");
  requireEqual(value.corpus.kind, "generated_functional", "corpus.kind");
  positiveInteger(value.corpus.note_count, "corpus.note_count");
  positiveInteger(value.corpus.markdown_bytes, "corpus.markdown_bytes");
  sha256(value.corpus.sha256, "corpus.sha256");
  requireEqual(
    value.corpus.hash_algorithm,
    "sha256-path-nul-decimal-length-nul-bytes-nul-v1",
    "corpus.hash_algorithm",
  );
  positiveInteger(value.corpus.expected_documents, "corpus.expected_documents");
  requireEqual(value.corpus.expected_documents, value.corpus.note_count, "corpus.expected_documents");

  requireRecord(value.checks, "checks");
  exactKeys(value.checks, CHECK_KEYS, "checks");
  positiveInteger(value.checks.total, "checks.total");
  nonNegativeInteger(value.checks.failed, "checks.failed");
  if (value.checks.failed > value.checks.total) throw new Error("invalid failed check count");
  if ((value.checks.failed === 0) !== value.verdict.includes("PASSED")) {
    throw new Error("verdict does not match check count");
  }

  validateTargets(value.targets);
  validatePrivacy(value.privacy);
  assertPrivacySafeEvidence(value, { privateCorpus: false });
  return value;
}

export function validateGate5GeneratedPerformanceEvidence(value) {
  requireRecord(value, "evidence");
  exactKeys(value, GENERATED_PERFORMANCE_KEYS, "evidence");
  requireEqual(value.schema_version, 1, "schema_version");
  requireEqual(value.kind, "kwiry_gate5_generated_performance", "kind");
  requireEqual(
    value.verdict,
    "EVIDENCE_CAPTURE_COMPLETE_OWNER_DECISION_REQUIRED",
    "verdict",
  );
  requireEqual(value.host, "node_worker_threads", "host");

  requireRecord(value.artifact, "artifact");
  exactKeys(value.artifact, PERFORMANCE_ARTIFACT_KEYS, "artifact");
  for (const key of PERFORMANCE_ARTIFACT_KEYS) {
    validateIdentity(value.artifact[key], `artifact.${key}`);
  }

  requireRecord(value.corpus, "corpus");
  exactKeys(value.corpus, PERFORMANCE_CORPUS_KEYS, "corpus");
  requireEqual(value.corpus.kind, "generated_performance", "corpus.kind");
  positiveInteger(value.corpus.note_count, "corpus.note_count");
  positiveInteger(value.corpus.markdown_bytes, "corpus.markdown_bytes");
  sha256(value.corpus.sha256, "corpus.sha256");
  requireEqual(
    value.corpus.hash_algorithm,
    "sha256-path-nul-decimal-length-nul-bytes-nul-v1",
    "corpus.hash_algorithm",
  );
  positiveInteger(value.corpus.expected_documents, "corpus.expected_documents");
  requireEqual(value.corpus.expected_documents, value.corpus.note_count, "corpus.expected_documents");
  uint32(value.corpus.seed_u32, "corpus.seed_u32");

  requireRecord(value.index, "index");
  exactKeys(value.index, PERFORMANCE_INDEX_KEYS, "index");
  positiveInteger(value.index.documents, "index.documents");
  positiveInteger(value.index.chunks, "index.chunks");
  requireEqual(value.index.documents, value.corpus.expected_documents, "index.documents");

  requireRecord(value.measurements, "measurements");
  exactKeys(value.measurements, PERFORMANCE_MEASUREMENT_KEYS, "measurements");
  for (const key of PERFORMANCE_MEASUREMENT_KEYS) {
    nonNegativeFinite(value.measurements[key], `measurements.${key}`);
  }

  requireRecord(value.samples, "samples");
  exactKeys(value.samples, PERFORMANCE_SAMPLE_KEYS, "samples");
  positiveInteger(value.samples.warm_search, "samples.warm_search");
  positiveInteger(value.samples.update_visibility, "samples.update_visibility");

  validateGeneratedPerformanceTargets(value.targets, value.measurements);
  validatePrivacy(value.privacy);
  assertPrivacySafeEvidence(value, { privateCorpus: false });
  return value;
}

export function assertPrivacySafeEvidence(value, options = {}) {
  walk(value, [], (key, item, path) => {
    if (path.startsWith("privacy.")) return;
    if (FORBIDDEN_KEY.test(key)) throw new Error(`forbidden evidence key: ${path}`);
    if (options.privateCorpus && path.startsWith("corpus.")
      && /(?:sha|hash|digest|fingerprint|checksum)/iu.test(key)) {
      throw new Error("private corpus hashes are forbidden");
    }
    if (typeof item !== "string") return;
    if (PRIVATE_PATH.test(item)) throw new Error("private or environment path detected");
    if (TOKEN.test(item)) throw new Error("credential-shaped value detected");
    if (SQL.test(item)) throw new Error("SQL text detected");
    if (STACK.test(item)) throw new Error("stack trace detected");
    if (OWNER_VERDICT.test(item)) throw new Error("owner decision wording detected");
  });
  return value;
}

function validateTargets(targets) {
  if (!Array.isArray(targets) || targets.length !== GATE5_TARGETS.length) {
    throw new Error("every Gate 5 target is required");
  }
  const seen = new Set();
  for (let index = 0; index < GATE5_TARGETS.length; index += 1) {
    const target = targets[index];
    const expected = GATE5_TARGETS[index];
    requireRecord(target, `targets.${index}`);
    exactKeys(target, TARGET_KEYS, `targets.${index}`);
    requireEqual(target.id, expected[0], `targets.${index}.id`);
    requireEqual(target.threshold, expected[1], `targets.${index}.threshold`);
    requireEqual(target.unit, expected[2], `targets.${index}.unit`);
    if (seen.has(target.id)) throw new Error("duplicate Gate 5 target");
    seen.add(target.id);
    if (!new Set(["not_measured", "met", "missed", "unavailable"]).has(target.status)) {
      throw new Error("invalid target status");
    }
    if (target.value !== null && (!Number.isFinite(target.value) || target.value < 0)) {
      throw new Error("invalid target value");
    }
    requireEqual(target.scope, "declared_reference_hardware", `targets.${index}.scope`);
    if (target.status === "not_measured" || target.status === "unavailable") {
      requireEqual(target.value, null, `targets.${index}.value`);
    }
  }
}

function validateGeneratedPerformanceTargets(targets, measurements) {
  if (!Array.isArray(targets) || targets.length !== GATE5_TARGETS.length) {
    throw new Error("every Gate 5 target is required");
  }
  const measurementKeys = new Map([
    ["build_duration", "build_duration_ms"],
    ["warm_search_p95", "warm_search_p95_ms"],
    ["hydration_p95", "hydration_p95_ms"],
    ["update_visibility_p95", "update_visibility_p95_ms"],
    ["max_event_loop_delay", "max_event_loop_delay_ms"],
    ["added_steady_state_memory", "added_rss_mib"],
  ]);
  for (let index = 0; index < GATE5_TARGETS.length; index += 1) {
    const target = targets[index];
    const expected = GATE5_TARGETS[index];
    requireRecord(target, `targets.${index}`);
    exactKeys(target, TARGET_KEYS, `targets.${index}`);
    requireEqual(target.id, expected[0], `targets.${index}.id`);
    requireEqual(target.threshold, expected[1], `targets.${index}.threshold`);
    requireEqual(target.unit, expected[2], `targets.${index}.unit`);
    const measurementKey = measurementKeys.get(target.id);
    if (!measurementKey) {
      requireEqual(target.status, "unavailable", `targets.${index}.status`);
      requireEqual(target.value, null, `targets.${index}.value`);
      requireEqual(
        target.scope,
        "installed_obsidian_reference_hardware",
        `targets.${index}.scope`,
      );
      continue;
    }
    const value = measurements[measurementKey];
    requireEqual(target.value, value, `targets.${index}.value`);
    requireEqual(
      target.status,
      value <= target.threshold ? "met" : "missed",
      `targets.${index}.status`,
    );
    requireEqual(target.scope, "generated_node_worker_threads", `targets.${index}.scope`);
  }
}

function validatePrivacy(value) {
  requireRecord(value, "privacy");
  exactKeys(value, PRIVACY_KEYS, "privacy");
  requireEqual(value.aggregate_only, true, "privacy.aggregate_only");
  for (const key of PRIVACY_KEYS.slice(1)) requireEqual(value[key], 0, `privacy.${key}`);
}

function validateIdentity(value, label) {
  requireRecord(value, label);
  exactKeys(value, IDENTITY_KEYS, label);
  positiveInteger(value.bytes, `${label}.bytes`);
  sha256(value.sha256, `${label}.sha256`);
}

function exactKeys(value, expected, label) {
  const actual = Object.keys(value).sort();
  const wanted = [...expected].sort();
  if (JSON.stringify(actual) !== JSON.stringify(wanted)) {
    throw new Error(`${label} has unexpected or missing keys`);
  }
}

function walk(value, path, visit) {
  if (Array.isArray(value)) {
    value.forEach((item, index) => walk(item, [...path, String(index)], visit));
    return;
  }
  if (!value || typeof value !== "object") return;
  for (const [key, item] of Object.entries(value)) {
    const next = [...path, key];
    visit(key, item, next.join("."));
    walk(item, next, visit);
  }
}

function requireRecord(value, label) {
  if (!value || typeof value !== "object" || Array.isArray(value)) {
    throw new Error(`${label} must be an object`);
  }
}

function requireEqual(actual, expected, label) {
  if (actual !== expected) throw new Error(`${label} is invalid`);
}

function requireBoolean(value, label) {
  if (typeof value !== "boolean") throw new Error(`${label} must be boolean`);
}

function positiveInteger(value, label) {
  if (!Number.isSafeInteger(value) || value < 1) throw new Error(`${label} must be positive`);
}

function nonNegativeInteger(value, label) {
  if (!Number.isSafeInteger(value) || value < 0) throw new Error(`${label} must be nonnegative`);
}

function nonNegativeFinite(value, label) {
  if (!Number.isFinite(value) || value < 0) throw new Error(`${label} must be nonnegative`);
}

function uint32(value, label) {
  if (!Number.isInteger(value) || value < 0 || value > 0xffff_ffff) {
    throw new Error(`${label} must be an unsigned 32-bit integer`);
  }
}

function sha256(value, label) {
  if (typeof value !== "string" || !/^[a-f0-9]{64}$/u.test(value)) {
    throw new Error(`${label} must be SHA-256`);
  }
}
