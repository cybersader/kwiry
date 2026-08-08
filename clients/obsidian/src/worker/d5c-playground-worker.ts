// SPDX-FileCopyrightText: 2026 cybersader
// SPDX-License-Identifier: GPL-3.0-only
/// <reference lib="webworker" />

import rustWasmBytes from "virtual:kwiry-rust-wasm-bytes";
import {
  abi_identity,
  initSync,
} from "virtual:kwiry-rust-wasm-bindings";
import {
  RUST_WASM_SHA256,
  RUST_WASM_SIZE,
} from "virtual:kwiry-artifact-identities";

import { evaluateInternalD5cCase } from "./d5c-evaluation";

const INTERNAL_PROTOCOL_VERSION = 1;
const WORKER_NAME = "internalD5cPlayground" as const;
const SCENARIO_ID = "balanced-playground-v1" as const;
const MAX_FIXTURE_BYTES = 1024 * 1024;
const MAX_EVALUATION_BYTES = 128 * 1024;
const MAX_SOURCES = 64;
const MAX_EVALUATIONS = 64;
const MAX_CHUNKS_PER_SOURCE = 1_024;
const MAX_STRING_CHARACTERS = 4_096;

type PlaygroundOperation =
  | "fixture_initialize"
  | "fixture_build"
  | "fixture_evaluate"
  | "fixture_dispose";
type PlaygroundState = "cold" | "ready" | "built" | "disposed" | "failed";

interface FixtureSource {
  source: {
    authorization_scope: string;
    source_key: string;
  };
  path: string;
  provider: Record<string, unknown> & { kind: "markdown" | "google_docs" | "canva"; provider_id: string };
  chunks: Array<{ chunk_id: string }>;
}

interface FixtureEvaluation {
  id: string;
  engine: "native_tantivy" | "portable_fts5" | "shared_contract";
  expected_disposition: "strict_balanced" | "neutralized_counterfactual" | "fatal";
  request: Record<string, unknown>;
  judgments: unknown[];
}

interface FixtureCorpus {
  schema_version: 1;
  scenario_id: typeof SCENARIO_ID;
  sources: FixtureSource[];
  evaluations: FixtureEvaluation[];
}

interface PlaygroundError {
  code: string;
  stage: "protocol" | "artifact" | "fixture" | "evaluation" | "lifecycle";
  message: string;
  retryable: false;
}

class PlaygroundFailure extends Error {
  constructor(
    public readonly code: string,
    public readonly stage: PlaygroundError["stage"],
    message: string,
  ) {
    super(message);
    this.name = "PlaygroundFailure";
  }
}

const scope = self as DedicatedWorkerGlobalScope;
let state: PlaygroundState = "cold";
let fixture: FixtureCorpus | null = null;
let evaluations = new Map<string, FixtureEvaluation>();
let lastRequestId = 0;

async function initialize(): Promise<unknown> {
  if (state !== "cold") {
    throw failure("invalid_state", "lifecycle", "Playground Worker is already initialized.");
  }
  try {
    installCapabilityGuards();
    await verifyRustArtifact();
    initSync({ module: rustWasmBytes.slice() });
    const identity = parseRustIdentity(abi_identity());
    state = "ready";
    return {
      abi_version: identity.abi_version,
      adapter: identity.adapter,
      adapter_version: identity.adapter_version,
      scenario_id: SCENARIO_ID,
    };
  } catch (error) {
    state = "failed";
    if (error instanceof PlaygroundFailure) throw error;
    throw failure("rust_init_failed", "artifact", "Playground Rust adapter initialization failed.");
  }
}

function buildFixture(value: unknown): unknown {
  requireState("ready");
  const parsed = parseFixtureCorpus(value);
  fixture = parsed;
  evaluations = new Map(parsed.evaluations.map((evaluation) => [evaluation.id, evaluation]));
  state = "built";
  return {
    scenario_id: parsed.scenario_id,
    source_count: parsed.sources.length,
    evaluation_count: parsed.evaluations.length,
  };
}

function evaluateFixture(evaluationId: string): unknown {
  requireState("built");
  const evaluation = evaluations.get(evaluationId);
  if (!evaluation) {
    throw failure("fixture_not_found", "fixture", "Requested fixture evaluation is unavailable.");
  }
  const comparison = evaluateInternalD5cCase(evaluation.request);
  return { evaluation_id: evaluation.id, comparison };
}

function dispose(): { closed: true } {
  fixture = null;
  evaluations.clear();
  state = "disposed";
  return { closed: true };
}

async function dispatch(value: unknown): Promise<void> {
  const identity = responseIdentity(value);
  let response: Record<string, unknown>;
  try {
    const request = parseRequest(value);
    if (request.id <= lastRequestId) {
      throw failure("invalid_request", "protocol", "Playground request ID is duplicate or stale.");
    }
    lastRequestId = request.id;
    let result: unknown;
    switch (request.operation) {
      case "fixture_initialize":
        result = await initialize();
        break;
      case "fixture_build":
        result = buildFixture(request.fixture);
        break;
      case "fixture_evaluate":
        result = evaluateFixture(request.evaluation_id);
        break;
      case "fixture_dispose":
        result = dispose();
        break;
    }
    response = {
      version: INTERNAL_PROTOCOL_VERSION,
      worker: WORKER_NAME,
      id: request.id,
      operation: request.operation,
      ok: true,
      result,
    };
  } catch (error) {
    response = {
      version: INTERNAL_PROTOCOL_VERSION,
      worker: WORKER_NAME,
      ...identity,
      ok: false,
      error: mapError(error),
    };
  }
  scope.postMessage(response);
  if (response.ok === true && response.operation === "fixture_dispose") {
    setTimeout(() => scope.close(), 0);
  }
}

function parseRequest(value: unknown):
  | { version: 1; worker: typeof WORKER_NAME; id: number; operation: "fixture_initialize" }
  | { version: 1; worker: typeof WORKER_NAME; id: number; operation: "fixture_build"; fixture: unknown }
  | { version: 1; worker: typeof WORKER_NAME; id: number; operation: "fixture_evaluate"; evaluation_id: string }
  | { version: 1; worker: typeof WORKER_NAME; id: number; operation: "fixture_dispose" } {
  if (!isRecord(value)
    || value.version !== INTERNAL_PROTOCOL_VERSION
    || value.worker !== WORKER_NAME
    || !Number.isSafeInteger(value.id)
    || Number(value.id) < 1
    || !isPlaygroundOperation(value.operation)) {
    throw failure("invalid_request", "protocol", "Invalid playground Worker request.");
  }
  if (value.operation === "fixture_initialize" || value.operation === "fixture_dispose") {
    if (!hasExactKeys(value, ["version", "worker", "id", "operation"])) {
      throw failure("invalid_request", "protocol", "Invalid playground Worker request.");
    }
    return value as never;
  }
  if (value.operation === "fixture_build") {
    if (!hasExactKeys(value, ["version", "worker", "id", "operation", "fixture"])) {
      throw failure("invalid_request", "protocol", "Invalid playground Worker request.");
    }
    return value as never;
  }
  if (!hasExactKeys(value, ["version", "worker", "id", "operation", "evaluation_id"])
    || !isBoundedString(value.evaluation_id, 256)) {
    throw failure("invalid_request", "protocol", "Invalid playground Worker request.");
  }
  return value as never;
}

function parseFixtureCorpus(value: unknown): FixtureCorpus {
  if (!isRecord(value)
    || !hasExactKeys(value, ["schema_version", "scenario_id", "sources", "evaluations"])
    || value.schema_version !== 1
    || value.scenario_id !== SCENARIO_ID
    || !Array.isArray(value.sources)
    || value.sources.length < 1
    || value.sources.length > MAX_SOURCES
    || !Array.isArray(value.evaluations)
    || value.evaluations.length < 1
    || value.evaluations.length > MAX_EVALUATIONS
    || serializedBytes(value) > MAX_FIXTURE_BYTES) {
    throw failure("invalid_fixture", "fixture", "Playground fixture is invalid or over limit.");
  }

  const sourceIds = new Set<string>();
  let totalChunks = 0;
  for (const source of value.sources) {
    if (!isFixtureSource(source)) {
      throw failure("invalid_fixture", "fixture", "Playground fixture source is invalid.");
    }
    const identity = sourceIdentity(source.source);
    if (sourceIds.has(identity)) {
      throw failure("invalid_fixture", "fixture", "Playground fixture source identity is duplicated.");
    }
    sourceIds.add(identity);
    totalChunks += source.chunks.length;
    if (!Number.isSafeInteger(totalChunks) || totalChunks > MAX_SOURCES * MAX_CHUNKS_PER_SOURCE) {
      throw failure("invalid_fixture", "fixture", "Playground fixture chunks exceed the limit.");
    }
  }

  const evaluationIds = new Set<string>();
  for (const evaluation of value.evaluations) {
    if (!isFixtureEvaluation(evaluation)
      || evaluationIds.has(evaluation.id)
      || serializedBytes(evaluation.request) > MAX_EVALUATION_BYTES
      || !usesOnlyFixtureSources(evaluation.request, sourceIds)) {
      throw failure("invalid_fixture", "fixture", "Playground fixture evaluation is invalid.");
    }
    evaluationIds.add(evaluation.id);
  }
  return value as unknown as FixtureCorpus;
}

function isFixtureSource(value: unknown): value is FixtureSource {
  if (!isRecord(value)
    || !hasExactKeys(value, ["source", "path", "provider", "chunks"])
    || !isQualifiedFixtureSource(value.source)
    || !isFixturePath(value.path)
    || !isRecord(value.provider)
    || !isBoundedString(value.provider.provider_id, 256)
    || (value.provider.kind !== "markdown"
      && value.provider.kind !== "google_docs"
      && value.provider.kind !== "canva")
    || !Array.isArray(value.chunks)
    || value.chunks.length < 1
    || value.chunks.length > MAX_CHUNKS_PER_SOURCE) {
    return false;
  }
  const chunkIds = new Set<string>();
  return value.chunks.every((chunk) => {
    if (!isRecord(chunk)
      || !hasExactKeys(chunk, ["chunk_id"])
      || !isBoundedString(chunk.chunk_id, 256)
      || chunkIds.has(chunk.chunk_id)) return false;
    chunkIds.add(chunk.chunk_id);
    return true;
  });
}

function isFixtureEvaluation(value: unknown): value is FixtureEvaluation {
  return isRecord(value)
    && hasExactKeys(value, ["id", "engine", "expected_disposition", "request", "judgments"])
    && isBoundedString(value.id, 256)
    && (value.engine === "native_tantivy"
      || value.engine === "portable_fts5"
      || value.engine === "shared_contract")
    && (value.expected_disposition === "strict_balanced"
      || value.expected_disposition === "neutralized_counterfactual"
      || value.expected_disposition === "fatal")
    && isRecord(value.request)
    && Array.isArray(value.judgments)
    && value.judgments.length <= 512;
}

function usesOnlyFixtureSources(
  request: Record<string, unknown>,
  sourceIds: ReadonlySet<string>,
): boolean {
  if (!hasExactKeys(request, ["abi_version", "operation", "configuration", "case"])
    || request.abi_version !== 3
    || request.operation !== "internal_d5c_evaluate"
    || !isRecord(request.configuration)
    || request.configuration.scenario_id !== SCENARIO_ID
    || !isRecord(request.case)
    || request.case.scenario_id !== SCENARIO_ID
    || !Array.isArray(request.case.candidates)
    || !Array.isArray(request.case.source_facts)) {
    return false;
  }
  const references = [...request.case.candidates, ...request.case.source_facts];
  return references.every((entry) => isRecord(entry)
    && isQualifiedFixtureSource(entry.source)
    && sourceIds.has(sourceIdentity(entry.source)));
}

function isQualifiedFixtureSource(value: unknown): value is {
  authorization_scope: string;
  source_key: string;
} {
  return isRecord(value)
    && hasExactKeys(value, ["authorization_scope", "source_key"])
    && isBoundedString(value.authorization_scope, 256)
    && value.authorization_scope.startsWith("fixture:")
    && isBoundedString(value.source_key, 256);
}

function sourceIdentity(source: { authorization_scope: string; source_key: string }): string {
  return JSON.stringify([source.authorization_scope, source.source_key]);
}

function isFixturePath(value: unknown): value is string {
  return isBoundedString(value, 1_024)
    && !value.startsWith("/")
    && !value.includes("\\")
    && value.split("/").every((part) => part.length > 0 && part !== "." && part !== "..");
}

function parseRustIdentity(source: string): {
  abi_version: 3;
  adapter: "kwiry-obsidian-wasm";
  adapter_version: string;
} {
  let value: unknown;
  try {
    value = JSON.parse(source);
  } catch {
    throw failure("artifact_mismatch", "artifact", "Playground Rust identity is invalid.");
  }
  if (!isRecord(value)
    || value.abi_version !== 3
    || value.adapter !== "kwiry-obsidian-wasm"
    || !isBoundedString(value.adapter_version, 64)
    || !Array.isArray(value.operations)
    || JSON.stringify(value.operations) !== JSON.stringify([
      "prepare_source",
      "prepare_oversized_source",
      "prepare_query",
      "finalize_query",
    ])) {
    throw failure("artifact_mismatch", "artifact", "Playground Rust identity is invalid.");
  }
  return value as never;
}

async function verifyRustArtifact(): Promise<void> {
  if (rustWasmBytes.byteLength !== RUST_WASM_SIZE) {
    throw failure("artifact_mismatch", "artifact", "Playground Rust artifact mismatch.");
  }
  const digest = await crypto.subtle.digest("SHA-256", rustWasmBytes.slice());
  const actual = Array.from(new Uint8Array(digest), (byte) => byte.toString(16).padStart(2, "0")).join("");
  if (actual !== RUST_WASM_SHA256) {
    throw failure("artifact_mismatch", "artifact", "Playground Rust artifact mismatch.");
  }
}

function requireState(required: PlaygroundState): void {
  if (state === "disposed") {
    throw failure("disposed", "lifecycle", "Playground Worker is disposed.");
  }
  if (state !== required) {
    throw failure("invalid_state", "lifecycle", "Playground Worker is not ready for this operation.");
  }
}

function responseIdentity(value: unknown): { id: number; operation: PlaygroundOperation } {
  if (isRecord(value)) {
    const id = Number.isSafeInteger(value.id) && Number(value.id) > 0 ? Number(value.id) : 1;
    const operation = isPlaygroundOperation(value.operation) ? value.operation : "fixture_initialize";
    return { id, operation };
  }
  return { id: 1, operation: "fixture_initialize" };
}

function mapError(error: unknown): PlaygroundError {
  if (error instanceof PlaygroundFailure) {
    return { code: error.code, stage: error.stage, message: error.message, retryable: false };
  }
  const code = isRecord(error) && typeof error.code === "string" ? error.code : "evaluation_failed";
  return {
    code,
    stage: "evaluation",
    message: "Playground evaluation failed.",
    retryable: false,
  };
}

function failure(code: string, stage: PlaygroundError["stage"], message: string): PlaygroundFailure {
  return new PlaygroundFailure(code, stage, message);
}

function installCapabilityGuards(): void {
  for (const property of [
    "fetch", "XMLHttpRequest", "WebSocket", "EventSource", "importScripts", "Worker",
    "indexedDB", "localStorage", "sessionStorage",
  ]) {
    Object.defineProperty(globalThis, property, {
      configurable: true,
      get() { throw new Error("denied playground capability"); },
      set() { throw new Error("denied playground capability"); },
    });
  }
  if (typeof navigator === "object" && navigator !== null) {
    Object.defineProperty(navigator, "storage", {
      configurable: true,
      get() { throw new Error("denied playground capability"); },
      set() { throw new Error("denied playground capability"); },
    });
  }
}

function serializedBytes(value: unknown): number {
  try {
    return new TextEncoder().encode(JSON.stringify(value)).byteLength;
  } catch {
    return Number.POSITIVE_INFINITY;
  }
}

function isPlaygroundOperation(value: unknown): value is PlaygroundOperation {
  return value === "fixture_initialize"
    || value === "fixture_build"
    || value === "fixture_evaluate"
    || value === "fixture_dispose";
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function hasExactKeys(value: Record<string, unknown>, keys: readonly string[]): boolean {
  const actual = Object.keys(value);
  return actual.length === keys.length && actual.every((key) => keys.includes(key));
}

function isBoundedString(value: unknown, maximum: number): value is string {
  return typeof value === "string"
    && value.length > 0
    && value.length <= Math.min(maximum, MAX_STRING_CHARACTERS);
}

let messageQueue = Promise.resolve();
scope.addEventListener("message", (event: MessageEvent<unknown>) => {
  messageQueue = messageQueue
    .then(() => dispatch(event.data))
    .catch(() => undefined);
});
