// SPDX-FileCopyrightText: 2026 cybersader
// SPDX-License-Identifier: GPL-3.0-only

import sqlite3InitModule from "@sqlite.org/sqlite-wasm";
import { spawnSync } from "node:child_process";
import { mkdtempSync, readFileSync, rmSync } from "node:fs";
import { createRequire } from "node:module";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { fileURLToPath } from "node:url";
import { afterAll, beforeAll, describe, expect, it, vi } from "vitest";

import { MIN_STANDARD_SOURCES } from "../src/worker/protocol";
import {
  openFts5Generation,
  type Fts5GenerationIndex,
  type SQLiteApi,
} from "../src/worker/fts5-index";
import type {
  ExecutionPlan,
  FinalizedLexicalV2Rank,
  FinalizedQuery,
  LexicalV2RankInput,
  PreparedQuery,
  QueryEvidenceObservation,
  QueryEvidenceStageKind,
  SourcePreparation,
  StagePlan,
} from "../src/worker/rust-adapter";

const rustRankBridge = vi.hoisted(() => ({
  finalize: null as ((input: LexicalV2RankInput) => FinalizedLexicalV2Rank) | null,
}));

vi.mock("../src/worker/rust-adapter", () => ({
  LEXICAL_V2_RANK_SCHEMA_VERSION: 2,
  finalizeLexicalV2RankWithRust: (input: LexicalV2RankInput): FinalizedLexicalV2Rank => {
    if (input.schema_version !== 2) throw new Error("unexpected lexical-v2 rank schema");
    if (rustRankBridge.finalize === null) throw new Error("portable Rust rank bridge is not ready");
    return rustRankBridge.finalize(input);
  },
}));

interface CorpusDocument {
  scope: string;
  path: string;
  markdown: string;
}

interface GeneratedDocuments {
  scope: string;
  path_prefix: string;
  count: number;
  width: number;
  markdown: string;
}

interface ExpectedPath {
  path: string;
  tier: QueryEvidenceStageKind | "explicit";
}

interface CorpusCase {
  id: string;
  query: string;
  scope: string;
  limit: number;
  assistance: "explicit_syntax_bypass" | "eligible";
  execution: "explicit_bypass" | "ready" | "empty_no_evidence";
  stages: QueryEvidenceStageKind[];
  partial_condition?: StagePlan["condition"];
  anchors: string[];
  expected_paths?: ExpectedPath[];
  excluded_paths?: string[];
  prefix_expansions?: number;
  stable_ties?: boolean;
  combined_scope_changes_evidence?: boolean;
}

interface CorpusBounds {
  maximum_terms: number;
  over_limit_terms: number;
  maximum_query_bytes: number;
  over_limit_unicode: string;
  maximum_prefix_terms: number;
  maximum_prefix_expansions_per_term: number;
  maximum_prefix_expansion_scan: number;
  maximum_candidates_per_stage: number;
  maximum_total_candidates: number;
}

interface ConformanceCorpus {
  schema_version: number;
  profile_id: string;
  documents: CorpusDocument[];
  generated_documents: GeneratedDocuments[];
  cases: CorpusCase[];
  bounds: CorpusBounds;
}

const corpus = JSON.parse(readFileSync(
  new URL("../../../fixtures/retrieval/lexical-conformance/cases.json", import.meta.url),
  "utf8",
)) as ConformanceCorpus;
const encoder = new TextEncoder();
const require = createRequire(import.meta.url);
interface RawRustAdapter {
  prepare_source(request: string, bytes: Uint8Array): string;
  prepare_query(request: string): string;
  finalize_query(request: string): string;
  finalize_lexical_v2_rank(request: string): string;
}
let rustAdapter: RawRustAdapter;
let adapterPackageDirectory: string | null = null;
let sqlite: SQLiteApi;

interface AdapterEnvelope<T> {
  status: "ok" | "error";
  result?: T;
  error?: { code: string; message: string };
}

function adapterResult<T>(serialized: string): T {
  const envelope = JSON.parse(serialized) as AdapterEnvelope<T>;
  if (envelope.status !== "ok" || envelope.result === undefined) {
    throw new Error(envelope.error?.message ?? "portable Rust adapter failed");
  }
  return envelope.result;
}

function prepareSourceWithRust(
  descriptor: {
    vault_id: string;
    path: string;
    format: "markdown";
    byte_length: number;
    mtime: number;
    mtime_nanos: string;
  },
  bytes: Uint8Array,
): SourcePreparation {
  return adapterResult<{ preparation: SourcePreparation }>(rustAdapter.prepare_source(
    JSON.stringify({ abi_version: 3, operation: "prepare_source", descriptor }),
    bytes,
  )).preparation;
}

function prepareQueryWithRust(query: string): PreparedQuery {
  return adapterResult<PreparedQuery>(rustAdapter.prepare_query(JSON.stringify({
    abi_version: 3,
    operation: "prepare_query",
    query,
  })));
}

function finalizeQueryWithRust(
  query: string,
  evidence: QueryEvidenceObservation,
): FinalizedQuery {
  return adapterResult<FinalizedQuery>(rustAdapter.finalize_query(JSON.stringify({
    abi_version: 3,
    operation: "finalize_query",
    query,
    evidence_report: {
      schema_version: 12,
      identifier_probe_matched: evidence.identifier_probe_matched,
      term_support: evidence.term_support,
    },
    prefix_expansions: evidence.prefix_expansions,
  })));
}

function run(command: string, args: string[], cwd: string): void {
  const result = spawnSync(command, args, { cwd, encoding: "utf8" });
  if (result.status !== 0) {
    throw new Error(`${command} failed: ${result.stdout}${result.stderr}`);
  }
}

beforeAll(async () => {
  const adapterRoot = fileURLToPath(new URL(
    "../rust/kwiry-obsidian-wasm/",
    import.meta.url,
  ));
  const manifest = join(adapterRoot, "Cargo.toml");
  run("cargo", [
    "build", "--manifest-path", manifest, "--target", "wasm32-unknown-unknown", "--release", "--lib",
  ], adapterRoot);
  adapterPackageDirectory = mkdtempSync(join(tmpdir(), "kwiry-lexical-conformance-"));
  run("wasm-bindgen", [
    join(adapterRoot, "target/wasm32-unknown-unknown/release/kwiry_obsidian_wasm.wasm"),
    "--target", "nodejs",
    "--out-dir", adapterPackageDirectory,
    "--out-name", "kwiry_obsidian_wasm",
  ], adapterRoot);
  rustAdapter = require(join(adapterPackageDirectory, "kwiry_obsidian_wasm.js")) as RawRustAdapter;
  rustRankBridge.finalize = (input) => adapterResult<FinalizedLexicalV2Rank>(
    rustAdapter.finalize_lexical_v2_rank(JSON.stringify({
      abi_version: 3,
      operation: "finalize_lexical_v2_rank",
      input,
    })),
  );

  const initializeSqlite = sqlite3InitModule as unknown as (options: {
    print: () => void;
    printErr: () => void;
  }) => Promise<SQLiteApi>;
  sqlite = await initializeSqlite({ print: () => undefined, printErr: () => undefined });
}, 120_000);

afterAll(() => {
  rustRankBridge.finalize = null;
  if (adapterPackageDirectory !== null) rmSync(adapterPackageDirectory, { recursive: true, force: true });
});

function documentsFor(scopes: readonly string[]): CorpusDocument[] {
  const documents = corpus.documents.filter((document) => scopes.includes(document.scope));
  for (const generated of corpus.generated_documents) {
    if (!scopes.includes(generated.scope)) continue;
    for (let value = 0; value < generated.count; value += 1) {
      documents.push({
        scope: generated.scope,
        path: `${generated.path_prefix}${String(value).padStart(generated.width, "0")}.md`,
        markdown: generated.markdown,
      });
    }
  }
  return documents;
}

function prepareDocument(document: CorpusDocument): SourcePreparation {
  const bytes = encoder.encode(document.markdown);
  return prepareSourceWithRust({
    vault_id: "active-vault",
    path: document.path,
    format: "markdown",
    byte_length: bytes.byteLength,
    mtime: 1,
    mtime_nanos: "1000001",
  }, bytes);
}

function openCorpusIndex(scopes: readonly string[]): Fts5GenerationIndex {
  const index = openFts5Generation(sqlite, undefined, "active-vault");
  try {
    for (const document of documentsFor(scopes)) index.replaceSource(prepareDocument(document));
    return index;
  } catch (error) {
    index.close();
    throw error;
  }
}

function stageKind(stage: StagePlan): QueryEvidenceStageKind | "explicit" {
  switch (stage.plan_id) {
    case "lexical_exact_metadata_v3": return "exact_metadata";
    case "lexical_exact_phrase_v3": return "exact_phrase";
    case "lexical_all_terms_v3": return "all_terms";
    case "lexical_partial_coverage_v3": return "partial_coverage";
    case "lexical_prefix_metadata_v3": return "prefix_metadata";
    case "lexical_prefix_v3": return "prefix";
    case "lexical_explicit_v3": return "explicit";
  }
}

function singleStagePlan(stage: StagePlan): ExecutionPlan {
  return {
    schema_version: 11,
    profile_id: stage.plan_id === "lexical_explicit_v3" ? "lexical-v1" : "lexical-v2",
    disposition: stage.plan_id === "lexical_explicit_v3" ? "explicit_bypass" : "ready",
    max_total_candidates: 512,
    min_standard_sources: MIN_STANDARD_SOURCES,
    stages: [{ ...stage, ordinal: 0 }],
  };
}

function execute(index: Fts5GenerationIndex, query: string) {
  const prepared = prepareQueryWithRust(query);
  const observation = index.observeQuery(prepared.probes);
  const finalized = finalizeQueryWithRust(query, observation);
  return { prepared, observation, finalized };
}

describe("shared lexical-v1 conformance corpus", () => {
  it("matches the Rust-owned evidence contract and portable FTS5 execution", () => {
    expect(corpus.schema_version).toBe(1);
    expect(corpus.profile_id).toBe("lexical-v1");
    const index = openCorpusIndex(["allowed"]);
    try {
      for (const testCase of corpus.cases.filter((value) => value.scope === "allowed")) {
        const { observation, finalized } = execute(index, testCase.query);
        expect(finalized.plan.assistance, `${testCase.id} assistance`).toBe(testCase.assistance);
        expect(finalized.plan.execution, `${testCase.id} execution`).toBe(testCase.execution);
        // Most corpus entries list the bounded stage envelope because backend
        // token support can omit a redundant always-on partial stage. A declared
        // condition is an exact expectation for the fully supported fallback.
        const expectedStages = testCase.partial_condition !== undefined
          ? testCase.stages
          : observation.term_support.every((support) => support.document_frequency > 0)
            ? testCase.stages.filter((stage) => stage !== "partial_coverage")
            : testCase.stages;
        expect(
          finalized.plan.evidence_stages.map((stage) => stage.kind),
          `${testCase.id} stages`,
        ).toEqual(expectedStages);
        if (testCase.partial_condition !== undefined) {
          expect(
            finalized.plan.evidence_stages.find((stage) =>
              stage.kind === "partial_coverage")?.condition,
            `${testCase.id} partial condition`,
          ).toBe(testCase.partial_condition);
          expect(
            finalized.execution_plan.stages
              .filter((stage) => stage.plan_id === "lexical_partial_coverage_v3")
              .every((stage) => stage.condition === testCase.partial_condition),
            `${testCase.id} partial lane conditions`,
          ).toBe(true);
        }
        expect(
          finalized.plan.term_intents
            .filter((intent) => intent.role === "required_identifier_anchor")
            .map((intent) => intent.text),
          `${testCase.id} anchors`,
        ).toEqual(testCase.anchors);
        if (testCase.prefix_expansions !== undefined) {
          expect(
            observation.prefix_expansions.reduce((sum, value) => sum + value.terms.length, 0),
            `${testCase.id} prefix expansions`,
          ).toBe(testCase.prefix_expansions);
        }

        const hits = index.search(finalized.execution_plan, testCase.limit);
        const hitPaths = hits.map((hit) => hit.path);
        if (testCase.id === "sparse-three-term-prefix-aware-partial") {
          expect(index.searchWithCandidateWindow(finalized.execution_plan, testCase.limit)
            .lexical_match_quality).toBe("mixed");
        }
        for (const excluded of testCase.excluded_paths ?? []) {
          expect(hitPaths, `${testCase.id} excluded ${excluded}`).not.toContain(excluded);
        }
        for (const expected of testCase.expected_paths ?? []) {
          expect(hitPaths, `${testCase.id} expected ${expected.path}`).toContain(expected.path);
          const firstTier = finalized.execution_plan.stages
            .map((stage) => ({
              kind: stageKind(stage),
              hits: index.search(singleStagePlan(stage), 100),
            }))
            .find((stage) => stage.hits.some((hit) => hit.path === expected.path))?.kind;
          expect(firstTier, `${testCase.id} tier for ${expected.path}`).toBe(expected.tier);
        }
        if (testCase.id === "tier-dominance") {
          const expectedOrder = (testCase.expected_paths ?? []).map((value) => value.path);
          expect(hitPaths.filter((path) => expectedOrder.includes(path))).toEqual(expectedOrder);
        }
        if (testCase.stable_ties) {
          const repeated = index.search(finalized.execution_plan, testCase.limit);
          expect(repeated).toEqual(hits);
          const identity = hits.map((hit) => [hit.chunk_id, hit.path] as const);
          expect(identity).toEqual([...identity].sort((left, right) =>
            left[0].localeCompare(right[0]) || left[1].localeCompare(right[1])));
        }
      }
    } finally {
      index.close();
    }
  }, 120_000);

  it("keeps evidence observations scoped to the active generation", () => {
    const allowed = openCorpusIndex(["allowed"]);
    const combined = openCorpusIndex(["allowed", "forbidden"]);
    try {
      const testCase = corpus.cases.find((value) => value.combined_scope_changes_evidence);
      expect(testCase).toBeDefined();
      const allowedFirst = execute(allowed, testCase!.query);
      const allowedRepeated = execute(allowed, testCase!.query);
      expect(allowedRepeated).toEqual(allowedFirst);
      expect(allowedFirst.finalized.plan.evidence_stages.map((stage) => stage.kind))
        .toContain("partial_coverage");
      expect(allowed.search(allowedFirst.finalized.execution_plan, testCase!.limit)
        .map((hit) => hit.path)).toContain("scope-allowed.md");

      const combinedResult = execute(combined, testCase!.query);
      const combinedPartial = combinedResult.finalized.plan.evidence_stages.find((stage) =>
        stage.kind === "partial_coverage");
      expect(combinedPartial?.condition)
        .toBe("if_fewer_than_minimum_standard_sources");
      expect(combined.search(combinedResult.finalized.execution_plan, testCase!.limit)
        .map((hit) => hit.path)).toContain("scope-forbidden.md");

      expect(execute(allowed, "secretpre").finalized.plan.execution).toBe("empty_no_evidence");
      expect(execute(combined, "secretpre").finalized.plan.evidence_stages.at(-1)?.kind)
        .toBe("prefix");
    } finally {
      allowed.close();
      combined.close();
    }
  }, 120_000);

  it("declares analyzed identifier anchors before and after filename metadata promotion", () => {
    const prepared = prepareQueryWithRust("IIA 2 optionalmissing");
    expect(prepared.plan.term_intents).toEqual([
      expect.objectContaining({
        text: "iia",
        role: "required_identifier_anchor",
        projection: "analyzed_text",
      }),
      expect.objectContaining({
        text: "2",
        role: "required_identifier_anchor",
        projection: "analyzed_text",
      }),
      expect.objectContaining({
        text: "optionalmissing",
        role: "optional_context",
        projection: "analyzed_text",
      }),
    ]);

    const index = openFts5Generation(sqlite, undefined, "active-vault");
    try {
      index.replaceSource(prepareDocument({
        scope: "identifier-filename",
        path: "iia 2 line xlsx.md",
        markdown: "# Workbook\nquarterly figures",
      }));
      const result = execute(index, "iia 2 line xlsx");
      expect(result.observation.identifier_probe_matched).toBe(true);
      expect(result.finalized.plan.term_intents).toEqual([
        expect.objectContaining({
          text: "iia",
          role: "required_identifier_anchor",
          projection: "analyzed_text",
        }),
        expect.objectContaining({
          text: "2",
          role: "required_identifier_anchor",
          projection: "analyzed_text",
        }),
        expect.objectContaining({
          text: "line",
          role: "optional_context",
          projection: "analyzed_text",
        }),
        expect.objectContaining({
          text: "xlsx",
          role: "optional_context",
          projection: "analyzed_text",
        }),
      ]);
      expect(result.finalized.execution_plan.stages.flatMap(
        (stage) => stage.required_identifiers ?? [],
      )).toEqual([]);
      expect(index.search(result.finalized.execution_plan, 20).map((hit) => hit.path))
        .toContain("iia 2 line xlsx.md");
    } finally {
      index.close();
    }
  });

  it("keeps portable prefix vocabulary discovery inside the Rust-declared body scope", () => {
    const index = openFts5Generation(sqlite, undefined, "active-vault");
    try {
      index.replaceSource(prepareDocument({
        scope: "body-scope",
        path: "standard.md",
        markdown: "# Standard\nember cobalt marker",
      }));
      index.replaceSource(prepareDocument({
        scope: "body-scope",
        path: "target.md",
        markdown: "# Target\nemberzz",
      }));
      for (let value = 0; value < 16; value += 1) {
        index.replaceSource(prepareDocument({
          scope: "body-scope",
          path: `decoy-${String(value).padStart(2, "0")}.md`,
          markdown: `---\ntitle: Embertitle${String(value).padStart(2, "0")}\n---\n# Decoy\nneutral body`,
        }));
      }

      const { observation, finalized } = execute(index, "in:body ember cobalt marker");
      expect(observation.prefix_expansions[0]?.terms).toEqual(["ember", "emberzz"]);
      const result = index.searchWithCandidateWindow(finalized.execution_plan, 100);
      expect(result.lexical_match_quality).toBe("mixed");
      expect(result.hits[0]?.path).toBe("standard.md");
      expect(result.hits.map((hit) => hit.path)).toContain("target.md");
      expect(result.hits.every((hit) => !hit.path.startsWith("decoy-"))).toBe(true);
    } finally {
      index.close();
    }
  });

  it("gates emphasized portable partial lanes on all distinct standard sources", () => {
    const search = (standardSources: number) => {
      const index = openFts5Generation(sqlite, undefined, "active-vault");
      try {
        for (let value = 0; value < standardSources; value += 1) {
          index.replaceSource(prepareDocument({
            scope: "emphasis-gate",
            path: `standard-${String(value).padStart(2, "0")}.md`,
            markdown: "---\ntitle: Cedar\n---\n# Marker\namber",
          }));
        }
        index.replaceSource(prepareDocument({
          scope: "emphasis-gate",
          path: "partial-target.md",
          markdown: "---\ntitle: Amberfield\n---\n# Target\nneutral body",
        }));
        const { finalized } = execute(index, ">title amber cedar marker");
        const lanes = finalized.execution_plan.stages;
        const firstPartial = lanes.findIndex((stage) =>
          stage.plan_id === "lexical_partial_coverage_v3");
        expect(firstPartial).toBeGreaterThan(0);
        expect(lanes.slice(0, firstPartial).every((stage) =>
          stage.plan_id !== "lexical_partial_coverage_v3")).toBe(true);
        expect(lanes.slice(firstPartial).every((stage) =>
          stage.plan_id === "lexical_partial_coverage_v3")).toBe(true);
        return index.searchWithCandidateWindow(finalized.execution_plan, 100);
      } finally {
        index.close();
      }
    };

    const sparse = search(19);
    expect(sparse.lexical_match_quality).toBe("mixed");
    expect(sparse.hits).toHaveLength(20);
    expect(sparse.hits.at(-1)?.path).toBe("partial-target.md");
    expect(sparse.hits.slice(0, 19).every((hit) => hit.path.startsWith("standard-")))
      .toBe(true);

    const threshold = search(20);
    expect(threshold.lexical_match_quality).toBe("standard_only");
    expect(threshold.hits).toHaveLength(20);
    expect(threshold.hits.every((hit) => hit.path.startsWith("standard-"))).toBe(true);
  });

  it("enforces the shared byte, term, prefix, stage, and candidate bounds", () => {
    expect(corpus.bounds).toEqual({
      maximum_terms: 128,
      over_limit_terms: 129,
      maximum_query_bytes: 4096,
      over_limit_unicode: "é",
      maximum_prefix_terms: 8,
      maximum_prefix_expansions_per_term: 16,
      maximum_prefix_expansion_scan: 256,
      maximum_candidates_per_stage: 256,
      maximum_total_candidates: 512,
    });
    const maximum = Array.from(
      { length: corpus.bounds.maximum_terms },
      (_, index) => `boundterm${index}`,
    ).join(" ");
    expect(prepareQueryWithRust(maximum).plan.support_probes).toHaveLength(128);
    const duplicates = Array.from(
      { length: corpus.bounds.maximum_terms },
      () => "boundterm",
    ).join(" ");
    expect(prepareQueryWithRust(duplicates).plan.support_probes).toHaveLength(1);
    const overTerms = Array.from(
      { length: corpus.bounds.over_limit_terms },
      () => "boundterm",
    ).join(" ");
    expect(() => prepareQueryWithRust(overTerms)).toThrow(/terms/u);
    const overBytes = corpus.bounds.over_limit_unicode
      .repeat(Math.floor(corpus.bounds.maximum_query_bytes / 2) + 1);
    expect(() => prepareQueryWithRust(overBytes)).toThrow(/UTF-8 bytes/u);
  });
});
