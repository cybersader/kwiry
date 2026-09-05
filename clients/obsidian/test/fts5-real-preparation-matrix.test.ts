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

import {
  openFts5Generation,
  openRestoredFts5Generation,
  type Fts5GenerationIndex,
  type SQLiteApi,
} from "../src/worker/fts5-index";
import type { SourceFormat } from "../src/worker/protocol";
import type {
  FinalizedLexicalV2Rank,
  FinalizedQuery,
  LexicalV2RankInput,
  PreparedQuery,
  QueryEvidenceObservation,
  SourcePreparation,
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

interface RawRustAdapter {
  prepare_query(request: string): string;
  finalize_query(request: string): string;
  finalize_lexical_v2_rank(request: string): string;
}

interface AdapterEnvelope<T> {
  status: "ok" | "error";
  result?: T;
  error?: { code: string; message: string };
}

interface MatrixCase {
  fixture: string;
  format: SourceFormat;
  path: string;
  query: string;
}

const VAULT_ID = "golden-vault";
const SOURCE_POLICY_HASH = "629adc7dd37b09cc9e452f5307199d3b5a6965fa0078f7a2b372aa397ae536a8";
const STANDARD_FIXTURE = "format-matrix-standard.json";
const STANDARD_PATH = "Format Matrix/standard-source.md";
const ENABLED_FORMATS: readonly SourceFormat[] = [
  "markdown",
  "text",
  "base",
  "canvas",
  "docx",
  "pdf",
  "excalidraw",
  "excel",
  "html",
];
const CASES: readonly MatrixCase[] = [
  {
    fixture: "format-matrix-markdown.json",
    format: "markdown",
    path: "Format Matrix/amberfield-cedarway.md",
    query: "amber cedar marker",
  },
  {
    fixture: "format-matrix-text.json",
    format: "text",
    path: "Format Matrix/birchstone-deltafield.txt",
    query: "birch delta inlet",
  },
  {
    fixture: "format-matrix-base.json",
    format: "base",
    path: "Format Matrix/cobaltview-elmbridge.base",
    query: "cobalt elm lantern",
  },
  {
    fixture: "format-matrix-canvas.json",
    format: "canvas",
    path: "Format Matrix/forestline-gardenway.canvas",
    query: "forest garden grove",
  },
  {
    fixture: "format-matrix-docx.json",
    format: "docx",
    path: "Format Matrix/harborstone-islandway.docx",
    query: "harbor island orchard",
  },
  {
    fixture: "format-matrix-pdf.json",
    format: "pdf",
    path: "Format Matrix/juniperfield-kestrelway.pdf",
    query: "juniper kestrel prairie",
  },
  {
    fixture: "format-matrix-excalidraw.json",
    format: "excalidraw",
    path: "Format Matrix/lakewood-maplefield.excalidraw",
    query: "lake maple quartz",
  },
  {
    fixture: "format-matrix-excel.json",
    format: "excel",
    path: "Format Matrix/meadowstone-northway.xlsx",
    query: "meadow north river",
  },
  {
    fixture: "format-matrix-html.json",
    format: "html",
    path: "Format Matrix/oakfield-pineway.html",
    query: "oak pine summit",
  },
];

const fixtureDirectory = fileURLToPath(
  new URL("./fixtures/source-preparations/", import.meta.url),
);
const require = createRequire(import.meta.url);
let rustAdapter: RawRustAdapter;
let adapterPackageDirectory: string | null = null;
let sqlite: SQLiteApi;

function adapterResult<T>(serialized: string): T {
  const envelope = JSON.parse(serialized) as AdapterEnvelope<T>;
  if (envelope.status !== "ok" || envelope.result === undefined) {
    throw new Error(envelope.error?.message ?? "portable Rust adapter failed");
  }
  return envelope.result;
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
  const result = spawnSync(command, args, {
    cwd,
    encoding: "utf8",
    env: { ...process.env, CARGO_BUILD_JOBS: "1" },
  });
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
    "build",
    "--manifest-path",
    manifest,
    "--target",
    "wasm32-unknown-unknown",
    "--release",
    "--lib",
  ], adapterRoot);
  adapterPackageDirectory = mkdtempSync(join(tmpdir(), "kwiry-format-matrix-"));
  run("wasm-bindgen", [
    join(adapterRoot, "target/wasm32-unknown-unknown/release/kwiry_obsidian_wasm.wasm"),
    "--target",
    "nodejs",
    "--out-dir",
    adapterPackageDirectory,
    "--out-name",
    "kwiry_obsidian_wasm",
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
  if (adapterPackageDirectory !== null) {
    rmSync(adapterPackageDirectory, { recursive: true, force: true });
  }
});

function readPreparation(name: string): SourcePreparation {
  return JSON.parse(readFileSync(join(fixtureDirectory, name), "utf8")) as SourcePreparation;
}

function matrixPreparations(): SourcePreparation[] {
  return [
    ...CASES.map(({ fixture }) => readPreparation(fixture)),
    readPreparation(STANDARD_FIXTURE),
  ];
}

function openMatrixIndex(): Fts5GenerationIndex {
  const index = openFts5Generation(sqlite, undefined, VAULT_ID, SOURCE_POLICY_HASH);
  try {
    index.applySourceChanges(matrixPreparations(), []);
    index.assertIntegrity();
    return index;
  } catch (error) {
    index.close();
    throw error;
  }
}

function execute(index: Fts5GenerationIndex, query: string) {
  const prepared = prepareQueryWithRust(query);
  const observation = index.observeQuery(prepared.probes);
  const finalized = finalizeQueryWithRust(query, observation);
  const result = index.searchWithCandidateWindow(finalized.execution_plan, 100);
  return { prepared, observation, finalized, result };
}

type HitIdentity = {
  chunk_id: string;
  path: string;
  heading_path: string[];
  format: SourceFormat;
  coverage: string;
  locator: unknown;
};

function assertMatrix(index: Fts5GenerationIndex): Array<{ query: string; hits: HitIdentity[] }> {
  return CASES.map((testCase) => {
    const terms = testCase.query.split(" ");
    expect(terms).toHaveLength(3);
    for (const width of [1, 2]) {
      const shorter = execute(index, terms.slice(0, width).join(" ")).result;
      expect(
        shorter.hits.some((hit) => hit.path === testCase.path),
        `${width}-term query must retain ${testCase.path}`,
      ).toBe(true);
    }

    const { observation, finalized, result } = execute(index, testCase.query);
    expect(observation.term_support).toHaveLength(3);
    expect(
      observation.term_support.every((support) => support.document_frequency > 0),
      `every typed token must be supported for ${testCase.format}`,
    ).toBe(true);
    const partial = finalized.plan.evidence_stages.find((stage) =>
      stage.kind === "partial_coverage");
    expect(partial, `${testCase.format} must plan sparse partial coverage`).toMatchObject({
      condition: "if_fewer_than_minimum_standard_sources",
      prefix_term_indexes: [0, 1],
      required_term_indexes: [2],
    });
    expect(result.lexical_match_quality).toBe("mixed");
    expect(result.hits[0]?.path).toBe(STANDARD_PATH);

    const targetHits = result.hits.filter((hit) => hit.path === testCase.path);
    expect(targetHits.length, `${testCase.format} target must surface below standard`).toBeGreaterThan(0);
    expect(result.hits.findIndex((hit) => hit.path === testCase.path)).toBeGreaterThan(0);
    expect(result.hits.every((hit) =>
      hit.path === STANDARD_PATH || hit.path === testCase.path)).toBe(true);
    expect(targetHits.every((hit) =>
      hit.format === testCase.format && hit.coverage === "indexed-complete")).toBe(true);
    assertMetadata(testCase.format, targetHits);

    return {
      query: testCase.query,
      hits: result.hits.map((hit) => ({
        chunk_id: hit.chunk_id,
        path: hit.path,
        heading_path: hit.heading_path,
        format: hit.format,
        coverage: hit.coverage,
        locator: hit.locator,
      })),
    };
  });
}

function assertMetadata(
  format: SourceFormat,
  hits: ReturnType<Fts5GenerationIndex["search"]>,
): void {
  if (format === "pdf") {
    expect(hits.every((hit) =>
      hit.heading_path.length === 0
      && JSON.stringify(hit.locator) === JSON.stringify({ kind: "pdf_page", page: 1 })))
      .toBe(true);
    return;
  }
  if (format === "excel") {
    expect(hits.some((hit) =>
      JSON.stringify(hit.locator)
        === JSON.stringify({ kind: "excel_cell", sheet: "Ledger", cell: "A1" })))
      .toBe(true);
    return;
  }
  if (format === "base") {
    expect(hits.some((hit) =>
      JSON.stringify(hit.locator)
        === JSON.stringify({ kind: "base_view", view: "Inventory" })))
      .toBe(true);
    return;
  }
  expect(hits.every((hit) => hit.locator === null)).toBe(true);
  if (["text", "canvas", "docx", "excalidraw", "html"].includes(format)) {
    expect(hits.every((hit) => hit.heading_path.length === 0)).toBe(true);
  }
}

describe("real preparation sparse prefix fallback format matrix", () => {
  it("admits two-name-signal targets below a different standard three-term source", () => {
    const index = openMatrixIndex();
    try {
      expect(index.documents).toBe(10);
      const first = assertMatrix(index);
      expect(assertMatrix(index)).toEqual(first);
    } finally {
      index.close();
    }
  });

  it("preserves results after restore with PDF and Excel explicitly enabled", () => {
    const index = openMatrixIndex();
    const before = assertMatrix(index);
    const image = index.exportImage(sqlite);
    index.close();

    const restored = openRestoredFts5Generation(
      sqlite,
      image,
      2,
      undefined,
      VAULT_ID,
      SOURCE_POLICY_HASH,
      ENABLED_FORMATS,
    );
    try {
      expect(restored.evictions.disabled_format.pdf).toBe(0);
      expect(restored.evictions.disabled_format.excel).toBe(0);
      expect(assertMatrix(restored)).toEqual(before);
    } finally {
      restored.close();
    }
  });
});
