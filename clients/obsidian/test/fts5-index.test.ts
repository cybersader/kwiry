// SPDX-FileCopyrightText: 2026 cybersader
// SPDX-License-Identifier: GPL-3.0-only

import sqlite3InitModule from "@sqlite.org/sqlite-wasm";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { FORMAT_IDENTITIES } from "../src/source-formats";
import { encodeExactIdentifierToken } from "../src/worker/exact-identifier-token";
import {
  CacheImageInvalidError,
  CacheVersionMismatchError,
  Fts5GenerationIndex,
  IndexCapacityError,
  isInternalLexicalTrace,
  openFts5Generation,
  openRestoredFts5Generation,
  type SQLiteApi,
  type SQLiteDatabase,
} from "../src/worker/fts5-index";
import {
  CACHE_SCHEMA_VERSION,
  SOURCE_FORMATS,
  type PropertyBag,
  type SourceFormat,
} from "../src/worker/protocol";
import type {
  PreparedFrontmatter,
  PreparedPropertyValue,
  SourcePreparation,
} from "../src/worker/rust-adapter";

/**
 * Opens an exported image as a real database. Nothing about the export is
 * taken on trust: the bytes have to load and answer queries, or the test fails.
 */
function deserialize(api: SQLiteApi, image: Uint8Array): SQLiteDatabase {
  const runtime = api as unknown as {
    wasm: { allocFromTypedArray(bytes: Uint8Array): number };
    capi: Record<string, (...args: never[]) => unknown> & {
      SQLITE_DESERIALIZE_FREEONCLOSE: number;
      SQLITE_DESERIALIZE_RESIZEABLE: number;
    };
  };
  const db = new api.oo1.DB(":memory:", "c");
  const pointer = runtime.wasm.allocFromTypedArray(image);
  const rc = (runtime.capi.sqlite3_deserialize as unknown as (
    db: unknown,
    schema: string,
    data: number,
    size: number,
    buffer: number,
    flags: number,
  ) => number)(
    db.pointer,
    "main",
    pointer,
    image.byteLength,
    image.byteLength,
    runtime.capi.SQLITE_DESERIALIZE_FREEONCLOSE | runtime.capi.SQLITE_DESERIALIZE_RESIZEABLE,
  );
  if (rc !== 0) {
    db.close();
    throw new Error(`sqlite3_deserialize failed with ${rc}`);
  }
  return db;
}

let sqlite: SQLiteApi;
let index: Fts5GenerationIndex;
let consoleWarning: ReturnType<typeof vi.spyOn>;
let consoleFailure: ReturnType<typeof vi.spyOn>;

beforeEach(async () => {
  consoleWarning = vi.spyOn(console, "warn").mockImplementation(() => undefined);
  consoleFailure = vi.spyOn(console, "error").mockImplementation(() => undefined);
  const initializeSqlite = sqlite3InitModule as unknown as (options: {
    print: () => void;
    printErr: () => void;
  }) => Promise<SQLiteApi>;
  sqlite = await initializeSqlite({
    print: () => undefined,
    printErr: () => undefined,
  });
  index = openFts5Generation(sqlite);
});

afterEach(() => {
  index.close();
  consoleWarning.mockRestore();
  consoleFailure.mockRestore();
});

function prepareFrontmatter(frontmatter: PropertyBag): PreparedFrontmatter {
  return Object.fromEntries(
    Object.entries(frontmatter).map(([name, value]) => [name, preparePropertyValue(value)]),
  );
}

function preparePropertyValue(value: PropertyBag[string]): PreparedPropertyValue {
  if (value === null) return { type: "null" };
  if (typeof value === "boolean") return { type: "boolean", value };
  if (typeof value === "string") return { type: "string", value };
  if (typeof value === "number") {
    if (Number.isInteger(value)) return { type: "i64", value: String(value) };
    const bytes = new ArrayBuffer(8);
    const view = new DataView(bytes);
    view.setFloat64(0, value, false);
    return { type: "f64", value: view.getBigUint64(0, false).toString(16).padStart(16, "0") };
  }
  if (Array.isArray(value)) {
    return { type: "sequence", value: value.map(preparePropertyValue) };
  }
  return {
    type: "map",
    value: Object.fromEntries(
      Object.entries(value).map(([name, child]) => [name, preparePropertyValue(child)]),
    ),
  };
}

function source(
  sourceKey: string,
  chunkId: string,
  content: string,
  title = "Title",
): SourcePreparation {
  return sourceAt(sourceKey, `${sourceKey}.md`, chunkId, content, title);
}

/** A prepared source of a chosen format, for the per-format identity tests. */
function formatted(
  sourceKey: string,
  path: string,
  format: SourceFormat,
  chunkId: string,
  content: string,
): SourcePreparation {
  const prepared = sourceAt(sourceKey, path, chunkId, content);
  prepared.format = format;
  return prepared;
}

function normalizedFixtureExact(value: string): string | null {
  const normalized = value.trim().split(/\s+/u).join(" ").toLowerCase();
  return normalized.length === 0 || new TextEncoder().encode(normalized).byteLength > 4_096
    ? null
    : normalized;
}

function sourceAt(
  sourceKey: string,
  path: string,
  chunkId: string,
  content: string,
  title = "Title",
  frontmatter: PropertyBag = { title, tags: ["test"] },
): SourcePreparation {
  return {
    schema_version: 9,
    source_key: sourceKey,
    vault_id: "active",
    path,
    format: "markdown",
    extraction_profile: "portable",
    coverage: "indexed-complete",
    content_hash: `hash-${sourceKey}`,
    byte_length: content.length,
    mtime: 1,
    mtime_nanos: "1000001",
    retrieval: {
      filename: path,
      stem: sourceKey,
      aliases: [],
    },
    normalized_exact: {
      filename: normalizedFixtureExact(path),
      stem: normalizedFixtureExact(sourceKey),
      aliases: [],
      title: normalizedFixtureExact(title),
    },
    frontmatter: prepareFrontmatter(frontmatter),
    chunks: [{
      chunk: {
        chunk_id: chunkId,
        vault_id: "active",
        room: null,
        path: path,
        heading_path: ["Heading"],
        content,
        frontmatter,
        links_out: [],
        mtime: 1,
        content_hash: `hash-${sourceKey}`,
        chunking_version: 1,
      },
      heading_text: "Heading",
      normalized_heading: "heading",
      technical_identifiers: [],
    }],
    kind: "indexed",
  };
}

const matchPlan = (
  planId: "lexical_explicit_v3" | "lexical_exact_phrase_v3" | "lexical_all_terms_v3"
    | "lexical_partial_coverage_v3" | "lexical_prefix_v3",
  matchValue: string,
) => ({
  schema_version: 3 as const,
  profile_id: "lexical-v1" as const,
  disposition: planId === "lexical_explicit_v3" ? "explicit_bypass" as const : "ready" as const,
  max_total_candidates: 512 as const,
  stages: [{
    ordinal: 0,
    plan_id: planId,
    match_value: matchValue,
    max_candidates: 256,
  }],
});

const anyPlan = (term: string) => matchPlan("lexical_all_terms_v3", `"${term}"`);

describe("Fts5GenerationIndex", () => {
  it("indexes prepared chunks, probes metadata, and returns stored result identity", () => {
    index.replaceSource(source("alpha", "chunk-a", "portable quasar text", "Quasar Guide"));
    expect(index.documents).toBe(1);
    expect(index.chunks).toBe(1);
    expect(index.observeQuery([{
      schema_version: 3,
      plan_id: "identifier_metadata_v3",
      match_value: "{filename stem aliases title heading_text} : (\"quasar\")",
    }]).identifier_probe_matched).toBe(true);

    const hits = index.search(anyPlan("quasar"), 20);
    expect(hits).toHaveLength(1);
    expect(hits[0]).toMatchObject({
      chunk_id: "chunk-a",
      vault_id: "active",
      path: "alpha.md",
      heading_path: ["Heading"],
      frontmatter: { title: "Quasar Guide" },
    });
    // Canonical content is hydrated only after final hit selection; it is not
    // selected by the ranking SQL and cannot affect MATCH, BM25, or ordering.
    expect(hits[0]!.excerpt).toBe("portable quasar text");
    expect(hits[0]).toMatchObject({
      format: "markdown",
      coverage: "indexed-complete",
      locator: null,
    });
  });

  it("hydrates Base format, coverage, locator, excerpt, and status counts after ranking", () => {
    const base = sourceAt(
      "projects",
      "projects.base",
      "chunk-projects-active",
      "active projects",
      "Projects",
    );
    base.format = "base";
    base.chunks[0]!.source_locator = { kind: "base_view", view: "Active" };
    index.replaceSource(base);

    const hits = index.search(anyPlan("active"), 20);
    expect(hits).toEqual([expect.objectContaining({
      chunk_id: "chunk-projects-active",
      format: "base",
      coverage: "indexed-complete",
      locator: { kind: "base_view", view: "Active" },
      excerpt: "active projects",
    })]);
    expect(index.sourceFormatCounts.base["indexed-complete"]).toBe(1);
    expect(index.sourceFormatCounts.markdown["indexed-complete"]).toBe(0);
  });

  it("hydrates a PDF page locator without letting the page reach ranked text", () => {
    const pdf = sourceAt(
      "report",
      "papers/report.pdf",
      "chunk-report-page-7",
      "reactor coolant survey",
      "Report",
    );
    pdf.format = "pdf";
    // A PDF section has no heading path by construction — the page travels as a
    // locator instead, which is exactly why it must survive hydration.
    pdf.chunks[0]!.chunk.heading_path = [];
    pdf.chunks[0]!.heading_text = "";
    pdf.chunks[0]!.normalized_heading = null;
    pdf.chunks[0]!.source_locator = { kind: "pdf_page", page: 7 };
    index.replaceSource(pdf);

    expect(index.search(anyPlan("coolant"), 20)).toEqual([expect.objectContaining({
      chunk_id: "chunk-report-page-7",
      format: "pdf",
      coverage: "indexed-complete",
      locator: { kind: "pdf_page", page: 7 },
      heading_path: [],
      excerpt: "reactor coolant survey",
    })]);
    expect(index.sourceFormatCounts.pdf["indexed-complete"]).toBe(1);
    // The page is navigation metadata and nothing else: it is not a column of
    // `chunk_search`, so no query for it can match the source it came from.
    expect(index.search(anyPlan("page"), 20)).toEqual([]);
    expect(index.search(anyPlan("7"), 20)).toEqual([]);
  });

  it("persists Canvas JSON without projecting structural properties into search or D5C rows", () => {
    const db = new sqlite.oo1.DB(":memory:", "c");
    const scoped = new Fts5GenerationIndex(db);
    try {
      const canvas = sourceAt(
        "research-canvas",
        "research.canvas",
        "chunk-research-canvas",
        "authored canvas evidence",
        "Canvas",
        {},
      );
      canvas.format = "canvas";
      canvas.normalized_exact.title = null;
      canvas.frontmatter = {
        canvas: preparePropertyValue({
          nodes: [{
            id: "1111111111111111",
            type: "group",
            x: 17,
            label: "authored canvas evidence",
            background: "geometrysentinelqzx.png",
          }],
          edges: [{ id: "aaaaaaaaaaaaaaaa", fromNode: "1111111111111111" }],
        }),
      };
      canvas.chunks[0]!.chunk.frontmatter = {};

      scoped.replaceSource(canvas);

      const stored = db.selectObjects(`
        SELECT p.property_name, p.value_json, s.property_count, s.property_scalar_count
        FROM source_properties AS p
        JOIN sources AS s ON s.source_key = p.source_key
      `);
      expect(stored).toHaveLength(1);
      expect(stored[0]).toMatchObject({
        property_name: "canvas",
        property_count: 1,
        property_scalar_count: 0,
      });
      expect(String(stored[0]!.value_json)).toContain("1111111111111111");
      expect(String(stored[0]!.value_json)).toContain("aaaaaaaaaaaaaaaa");
      expect(db.selectValue("SELECT count(*) FROM source_property_scalars")).toBe(0);
      expect(db.selectValue(
        "SELECT count(*) FROM source_property_text_fts WHERE source_property_text_fts MATCH 'geometrysentinelqzx'",
      )).toBe(0);

      expect(scoped.search(anyPlan("authored"), 20)).toEqual([
        expect.objectContaining({
          path: "research.canvas",
          format: "canvas",
          coverage: "indexed-complete",
          locator: null,
        }),
      ]);
      expect(scoped.search(anyPlan("geometrysentinelqzx"), 20)).toEqual([]);
      scoped.assertIntegrity();
    } finally {
      scoped.close();
    }
  });

  it("intersects exact identifiers with ordinary FTS terms and identifier-only stages", () => {
    const alpha = source("alpha", "chunk-alpha", "cache guidance");
    alpha.chunks[0]!.technical_identifiers = ["rfc 9110"];
    const beta = source("beta", "chunk-beta", "cache guidance");
    beta.chunks[0]!.technical_identifiers = ["rfc 9110", "cve-2026-1234"];
    const gamma = source("gamma", "chunk-gamma", "unrelated guidance");
    gamma.chunks[0]!.technical_identifiers = ["rfc 9110", "cve-2026-1234"];
    index.applySourceChanges([gamma, beta, alpha], []);

    expect(index.observeQuery([{
      schema_version: 3,
      plan_id: "term_support_v3",
      probe_id: 0,
      term_index: 0,
      exact_identifier: "rfc 9110",
      prefix_pattern: null,
      max_prefix_expansions: 16,
      max_prefix_term_bytes: 96,
    }]).term_support[0]?.document_frequency).toBe(1);

    const combined = index.search({
      schema_version: 3,
      profile_id: "lexical-v1",
      disposition: "ready",
      max_total_candidates: 512,
      stages: [{
        ordinal: 0,
        plan_id: "lexical_all_terms_v3",
        match_value: "{content} : (\"cache\")",
        required_identifiers: ["rfc 9110", "cve-2026-1234"],
        max_candidates: 256,
      }],
    }, 20);
    expect(combined.map((hit) => hit.chunk_id)).toEqual(["chunk-beta"]);

    const identifierOnly = index.search({
      schema_version: 3,
      profile_id: "lexical-v1",
      disposition: "ready",
      max_total_candidates: 512,
      stages: [{
        ordinal: 0,
        plan_id: "lexical_partial_coverage_v3",
        required_identifiers: ["rfc 9110", "cve-2026-1234"],
        max_candidates: 256,
      }],
    }, 1);
    expect(identifierOnly.map((hit) => hit.chunk_id)).toEqual(["chunk-beta"]);
  });

  it("stores exactly one dense exact-identifier FTS row per chunk", () => {
    const db = new sqlite.oo1.DB(":memory:", "c");
    const scoped = new Fts5GenerationIndex(db);
    try {
      const empty = source("empty-identifiers", "chunk-empty-identifiers", "ordinary body");
      const one = source("one-identifier", "chunk-one-identifier", "ordinary body");
      one.chunks[0]!.technical_identifiers = ["rfc 9110"];
      const many = source("many-identifiers", "chunk-many-identifiers", "ordinary body");
      many.chunks[0]!.technical_identifiers = Array.from(
        { length: 256 },
        (_unused, value) => `cve-2026-${String(value).padStart(4, "0")}`,
      );

      scoped.applySourceChanges([empty, one, many], [], true);

      expect(db.selectValue("SELECT count(*) FROM chunks")).toBe(3);
      expect(db.selectValue("SELECT count(*) FROM chunk_exact_identifier_fts_docsize")).toBe(3);
      const postings = db.selectObjects(`
        SELECT doc, count(*) AS terms
        FROM chunk_exact_identifier_fts_vocab
        GROUP BY doc
        ORDER BY doc
      `);
      expect(postings).toEqual([
        { doc: 2, terms: 1 },
        { doc: 3, terms: 256 },
      ]);
      expect(() => scoped.assertIntegrity()).not.toThrow();
    } finally {
      scoped.close();
    }
  });

  it("exposes detail-none vocabulary instances in deterministic term/document order", () => {
    const db = new sqlite.oo1.DB(":memory:", "c");
    const scoped = new Fts5GenerationIndex(db);
    try {
      const alpha = source("vocab-alpha", "chunk-vocab-alpha", "ordinary body");
      alpha.chunks[0]!.technical_identifiers = ["zeta identifier", "alpha identifier"];
      const beta = source("vocab-beta", "chunk-vocab-beta", "ordinary body");
      beta.chunks[0]!.technical_identifiers = ["alpha identifier"];
      scoped.applySourceChanges([alpha, beta], []);

      expect(db.selectObjects(
        "SELECT term, doc, col, offset FROM chunk_exact_identifier_fts_vocab",
      )).toEqual([
        {
          term: encodeExactIdentifierToken("alpha identifier"),
          doc: 1,
          col: null,
          offset: null,
        },
        {
          term: encodeExactIdentifierToken("alpha identifier"),
          doc: 2,
          col: null,
          offset: null,
        },
        {
          term: encodeExactIdentifierToken("zeta identifier"),
          doc: 1,
          col: null,
          offset: null,
        },
      ]);
      expect(() => scoped.assertIntegrity()).not.toThrow();
    } finally {
      scoped.close();
    }
  });

  it("matches lossless Unicode and punctuation identifiers without query syntax interpretation", () => {
    const prepared = source("unicode-identifiers", "chunk-unicode-identifiers", "ordinary body");
    prepared.chunks[0]!.technical_identifiers = [
      "résumé api",
      "product/v2.4.1",
      "emoji🚀identifier",
      "field:value OR forged",
    ];
    index.replaceSource(prepared);

    for (const exactValue of prepared.chunks[0]!.technical_identifiers) {
      const hits = index.search({
        schema_version: 3,
        profile_id: "lexical-v1",
        disposition: "ready",
        max_total_candidates: 512,
        stages: [{
          ordinal: 0,
          plan_id: "lexical_exact_metadata_v3",
          exact_value: exactValue,
          max_candidates: 256,
        }],
      }, 20);
      expect(hits.map((hit) => hit.chunk_id)).toEqual(["chunk-unicode-identifiers"]);
    }
  });

  it("executes exact metadata before weaker FTS tiers and deduplicates", () => {
    index.replaceSource(source("exact", "chunk-exact", "ordinary body", "Quasar Guide"));
    index.replaceSource(source("phrase", "chunk-phrase", "quasar guide quasar guide quasar guide"));
    const hits = index.search({
      schema_version: 3,
      profile_id: "lexical-v1",
      disposition: "ready",
      max_total_candidates: 512,
      stages: [
        {
          ordinal: 0,
          plan_id: "lexical_exact_metadata_v3",
          exact_value: "quasar guide",
          max_candidates: 256,
        },
        {
          ordinal: 1,
          plan_id: "lexical_exact_phrase_v3",
          match_value: "{filename stem aliases title heading_text content} : \"quasar guide\"",
          max_candidates: 256,
        },
      ],
    }, 20);
    expect(hits.map((hit) => hit.chunk_id)).toEqual(["chunk-exact", "chunk-phrase"]);
  });

  it("matches frontmatter tags through the tags FTS column", () => {
    index.replaceSource(sourceAt(
      "tagged",
      "tagged.md",
      "chunk-tagged",
      "ordinary body",
      "Plain Title",
      { title: "Plain Title", tags: ["tagbeacon"] },
    ));
    index.replaceSource(sourceAt(
      "untagged",
      "untagged.md",
      "chunk-untagged",
      "ordinary body",
      "Other Title",
      { title: "Other Title" },
    ));
    const hits = index.search(matchPlan(
      "lexical_all_terms_v3",
      '{filename stem aliases title heading_text tags content} : ("tagbeacon")',
    ), 20);
    expect(hits.map((hit) => hit.chunk_id)).toEqual(["chunk-tagged"]);
  });

  it("uses Rust-normalized Unicode and collapsed whitespace for exact metadata", () => {
    index.replaceSource(sourceAt(
      "unicode-exact",
      "unicode-exact.md",
      "chunk-unicode-exact",
      "ordinary body",
      "RÉSUMÉ   Cache",
    ));

    const hits = index.search({
      schema_version: 3,
      profile_id: "lexical-v1",
      disposition: "ready",
      max_total_candidates: 512,
      stages: [{
        ordinal: 0,
        plan_id: "lexical_exact_metadata_v3",
        exact_value: "résumé cache",
        max_candidates: 256,
      }],
    }, 20);

    expect(hits.map((hit) => hit.chunk_id)).toEqual(["chunk-unicode-exact"]);
  });

  it("does not collide exact metadata values that differ after 256 scalars", () => {
    const prefix = `marker ${"a".repeat(249)}`;
    index.replaceSource(sourceAt(
      "bounded-exact",
      "bounded-exact.md",
      "chunk-bounded-exact",
      "ordinary body",
      `${prefix}x`,
    ));

    const hits = index.search({
      schema_version: 3,
      profile_id: "lexical-v1",
      disposition: "ready",
      max_total_candidates: 512,
      stages: [{
        ordinal: 0,
        plan_id: "lexical_exact_metadata_v3",
        exact_value: `${prefix}y`,
        max_candidates: 256,
      }],
    }, 20);

    expect(hits).toEqual([]);
  });

  it("maintains indexed exact alias and identifier projections transactionally", () => {
    const db = new sqlite.oo1.DB(":memory:", "c");
    const scoped = new Fts5GenerationIndex(db);
    try {
      const prepared = sourceAt(
        "exact-projection",
        "exact-projection.md",
        "chunk-exact-projection",
        "CVE-2026-1234 body",
        "Projection",
        { title: "Projection", aliases: ["Alias Signal"] },
      );
      prepared.retrieval.aliases = ["Alias Signal"];
      prepared.normalized_exact.aliases = ["alias signal"];
      prepared.chunks[0]!.technical_identifiers = ["cve-2026-1234"];
      scoped.replaceSource(prepared);

      expect(db.selectValue("SELECT count(*) FROM source_exact_aliases")).toBe(1);
      expect(db.selectValue("SELECT count(*) FROM chunk_exact_identifier_fts_docsize")).toBe(1);
      const exactPlan = (value: string) => ({
        schema_version: 3 as const,
        profile_id: "lexical-v1" as const,
        disposition: "ready" as const,
        max_total_candidates: 512 as const,
        stages: [{
          ordinal: 0,
          plan_id: "lexical_exact_metadata_v3" as const,
          exact_value: value,
          max_candidates: 256,
        }],
      });
      expect(scoped.search(exactPlan("alias signal"), 20)).toHaveLength(1);
      expect(scoped.search(exactPlan("cve-2026-1234"), 20)).toHaveLength(1);
      expect(() => scoped.assertIntegrity()).not.toThrow();

      db.exec("UPDATE source_exact_aliases SET value = 'forged alias'");
      expect(() => openRestoredFts5Generation(sqlite, scoped.exportImage(sqlite), 1))
        .toThrow(CacheImageInvalidError);
      db.exec("UPDATE source_exact_aliases SET value = 'alias signal'");
      db.exec("DELETE FROM chunk_exact_identifier_fts WHERE rowid = 1");
      db.exec("INSERT INTO chunk_exact_identifier_fts(rowid, token) VALUES(1, ?)", {
        bind: [encodeExactIdentifierToken("cve-2026-9999")],
      });
      expect(() => openRestoredFts5Generation(sqlite, scoped.exportImage(sqlite), 1))
        .toThrow(CacheImageInvalidError);
      db.exec("DELETE FROM chunk_exact_identifier_fts WHERE rowid = 1");
      db.exec("INSERT INTO chunk_exact_identifier_fts(rowid, token) VALUES(1, ?)", {
        bind: [encodeExactIdentifierToken("cve-2026-1234")],
      });
      expect(() => scoped.assertIntegrity()).not.toThrow();

      scoped.applySourceChanges([], [{ vault_id: "active", path: "exact-projection.md" }], true);
      expect(db.selectValue("SELECT count(*) FROM source_exact_aliases")).toBe(0);
      expect(db.selectValue("SELECT count(*) FROM chunk_exact_identifier_fts_docsize")).toBe(0);
    } finally {
      scoped.close();
    }
  });

  it("rejects missing, extra, and misassigned exact identifier postings", () => {
    const missingDb = new sqlite.oo1.DB(":memory:", "c");
    const missing = new Fts5GenerationIndex(missingDb);
    try {
      const prepared = source("missing-exact", "chunk-missing-exact", "ordinary body");
      prepared.chunks[0]!.technical_identifiers = ["rfc 9110"];
      missing.replaceSource(prepared);
      missingDb.exec("DELETE FROM chunk_exact_identifier_fts WHERE rowid = 1");
      expect(() => missing.assertIntegrity()).toThrow(/integrity check failed/);
    } finally {
      missing.close();
    }

    const extraDb = new sqlite.oo1.DB(":memory:", "c");
    const extra = new Fts5GenerationIndex(extraDb);
    try {
      const prepared = source("extra-exact", "chunk-extra-exact", "ordinary body");
      prepared.chunks[0]!.technical_identifiers = ["rfc 9110"];
      extra.replaceSource(prepared);
      extraDb.exec("INSERT INTO chunk_exact_identifier_fts(rowid, token) VALUES(1, ?)", {
        bind: [encodeExactIdentifierToken("forged extra")],
      });
      expect(() => extra.assertIntegrity()).toThrow(/integrity check failed/);
    } finally {
      extra.close();
    }

    const swappedDb = new sqlite.oo1.DB(":memory:", "c");
    const swapped = new Fts5GenerationIndex(swappedDb);
    try {
      const alpha = source("alpha-swap", "chunk-alpha-swap", "ordinary body");
      alpha.chunks[0]!.technical_identifiers = ["alpha identifier"];
      const beta = source("beta-swap", "chunk-beta-swap", "ordinary body");
      beta.chunks[0]!.technical_identifiers = ["beta identifier"];
      swapped.applySourceChanges([alpha, beta], []);
      swappedDb.exec("DELETE FROM chunk_exact_identifier_fts");
      swappedDb.exec("INSERT INTO chunk_exact_identifier_fts(rowid, token) VALUES(1, ?)", {
        bind: [encodeExactIdentifierToken("beta identifier")],
      });
      swappedDb.exec("INSERT INTO chunk_exact_identifier_fts(rowid, token) VALUES(2, ?)", {
        bind: [encodeExactIdentifierToken("alpha identifier")],
      });
      expect(() => swapped.assertIntegrity()).toThrow(/integrity check failed/);
      expect(() => openRestoredFts5Generation(sqlite, swapped.exportImage(sqlite), 1))
        .toThrow(CacheImageInvalidError);
    } finally {
      swapped.close();
    }
  });

  it("does not lose an exact metadata hit behind the per-stage candidate cutoff", () => {
    for (let value = 0; value < 300; value += 1) {
      const suffix = String(value).padStart(3, "0");
      index.replaceSource(sourceAt(
        `needle-signal-decoy-${suffix}`,
        `needle-signal-decoy-${suffix}.md`,
        `chunk-decoy-${suffix}`,
        "needle signal ".repeat(20),
        `Needle Signal decoy ${suffix}`,
        { title: `Needle Signal decoy ${suffix}`, aliases: [`Needle Signal decoy ${suffix}`] },
      ));
    }
    index.replaceSource(source("needle-exact", "chunk-needle-exact", "ordinary body", "Needle Signal"));

    const hits = index.search({
      schema_version: 3,
      profile_id: "lexical-v1",
      disposition: "ready",
      max_total_candidates: 512,
      stages: [{
        ordinal: 0,
        plan_id: "lexical_exact_metadata_v3",
        exact_value: "needle signal",
        max_candidates: 256,
      }],
    }, 20);
    expect(hits.map((hit) => hit.chunk_id)).toEqual(["chunk-needle-exact"]);
  });

  it("proves an exact result limit exhausted only when the searched stage itself exhausted", () => {
    for (let value = 0; value < 20; value += 1) {
      const suffix = String(value).padStart(2, "0");
      index.replaceSource(sourceAt(
        `window-exact-${suffix}`,
        `window-exact-${suffix}.md`,
        `chunk-window-exact-${suffix}`,
        "windowexact",
      ));
    }

    const result = index.searchWithCandidateWindow(anyPlan("windowexact"), 20);
    expect(result.hits).toHaveLength(20);
    expect(result.candidate_window).toEqual({
      state: "exhausted",
      candidate_count: 20,
      candidate_limit: 512,
    });
  });

  it("reports more_available from an observed unreturned candidate, not hits length", () => {
    for (let value = 0; value < 21; value += 1) {
      const suffix = String(value).padStart(2, "0");
      index.replaceSource(sourceAt(
        `window-more-${suffix}`,
        `window-more-${suffix}.md`,
        `chunk-window-more-${suffix}`,
        "windowmore",
      ));
    }

    const result = index.searchWithCandidateWindow(anyPlan("windowmore"), 20);
    expect(result.hits).toHaveLength(20);
    expect(result.candidate_window).toEqual({
      state: "more_available",
      candidate_count: 21,
      candidate_limit: 512,
    });
  });

  it("closes at the unchanged 256-per-stage and 512-total candidate ceilings", () => {
    for (let value = 0; value < 256; value += 1) {
      const suffix = String(value).padStart(3, "0");
      index.replaceSource(sourceAt(
        `window-cap-${suffix}`,
        `window-cap-${suffix}.md`,
        `chunk-window-cap-${suffix}`,
        "windowcap",
      ));
    }
    const stage = {
      plan_id: "lexical_all_terms_v3" as const,
      match_value: "\"windowcap\"",
      max_candidates: 256,
    };

    const result = index.searchWithCandidateWindow({
      schema_version: 3,
      profile_id: "lexical-v1",
      disposition: "ready",
      max_total_candidates: 512,
      stages: [
        { ...stage, ordinal: 0 },
        { ...stage, ordinal: 1 },
      ],
    }, 512);
    expect(result.hits).toHaveLength(256);
    expect(result.candidate_window).toEqual({
      state: "candidate_limit_reached",
      candidate_count: 512,
      candidate_limit: 512,
    });
  });

  it("keeps exact filename and title candidates in the same metadata score tier at cutoff", () => {
    index.replaceSource(sourceAt(
      "primary-filename",
      "needle.md",
      "000-primary",
      "ordinary body",
      "Unrelated title",
    ));
    for (let value = 0; value < 256; value += 1) {
      const suffix = String(value).padStart(3, "0");
      index.replaceSource(sourceAt(
        `title-decoy-${suffix}`,
        `title-decoy-${suffix}.md`,
        `chunk-title-${suffix}`,
        "ordinary body",
        "Needle.md",
      ));
    }

    const trace = index.beginInternalLexicalTrace(() => 0);
    const hits = index.search({
      schema_version: 3,
      profile_id: "lexical-v1",
      disposition: "ready",
      max_total_candidates: 512,
      stages: [{
        ordinal: 0,
        plan_id: "lexical_exact_metadata_v3",
        exact_value: "needle.md",
        max_candidates: 256,
      }],
    }, 100, trace);
    const summary = index.finishInternalLexicalTrace(trace);

    expect(hits).toHaveLength(100);
    expect(hits[0]?.chunk_id).toBe("000-primary");
    expect(summary.candidate_count).toBe(256);
    expect(summary.stages[0]?.candidate_count).toBe(256);
  });

  it("keeps equal evidence rank-neutral when only stored SourceFormat is mutated", () => {
    const db = new sqlite.oo1.DB(":memory:", "c");
    const scoped = new Fts5GenerationIndex(db);
    try {
      scoped.applySourceChanges([
        sourceAt("neutral-alpha", "neutral-alpha.md", "chunk-neutral-alpha", "formatneutral"),
        sourceAt("neutral-beta", "neutral-beta.md", "chunk-neutral-beta", "formatneutral"),
      ], []);
      const baseline = scoped.search(anyPlan("formatneutral"), 20);
      expect(baseline).toHaveLength(2);
      expect(baseline[0]!.score).toBe(baseline[1]!.score);

      db.exec("UPDATE sources SET source_format = 'canvas' WHERE source_key = 'neutral-beta'");
      const mutated = scoped.search(anyPlan("formatneutral"), 20);

      expect(mutated.map(({ format, ...hit }) => hit))
        .toEqual(baseline.map(({ format, ...hit }) => hit));
      expect(mutated.find((hit) => hit.path === "neutral-alpha.md")?.format).toBe("markdown");
      expect(mutated.find((hit) => hit.path === "neutral-beta.md")?.format).toBe("canvas");
      expect(mutated[0]!.score).toBe(mutated[1]!.score);
    } finally {
      scoped.close();
    }
  });

  it("orders equal-score ties by chunk ID then path independently of insertion order", () => {
    const chunkIds = ["chunk-z", "chunk-a", "chunk-m", "chunk-b", "chunk-y"];
    for (const [indexValue, chunkId] of chunkIds.entries()) {
      index.replaceSource(sourceAt(
        `stable-${indexValue}`,
        `stable-${indexValue}.md`,
        chunkId,
        "portabletie",
      ));
    }
    const first = index.search(anyPlan("portabletie"), 20);
    const repeated = index.search(anyPlan("portabletie"), 20);
    const expected = [...chunkIds].sort();
    expect(first.map((hit) => hit.chunk_id)).toEqual(expected);
    expect(repeated).toEqual(first);
  });

  it("returns no rows for a typed no-evidence execution plan", () => {
    index.replaceSource(source("alpha", "chunk-a", "popular common document"));
    expect(index.search({
      schema_version: 3,
      profile_id: "lexical-v1",
      disposition: "empty_no_evidence",
      max_total_candidates: 512,
      stages: [],
    }, 20)).toEqual([]);
  });

  it("observes support only in canonical fields and bounds sorted prefix terms", () => {
    const words = Array.from({ length: 20 }, (_, value) => `quasar${String(value).padStart(2, "0")}`);
    index.replaceSource(sourceAt(
      "support",
      "path-only-quasar.md",
      "chunk-support",
      words.join(" "),
      "Support",
      { tags: ["tag-only-nebula"] },
    ));
    const observed = index.observeQuery([{
      schema_version: 3,
      plan_id: "term_support_v3",
      probe_id: 0,
      term_index: 0,
      match_value: "{filename stem aliases title heading_text content} : (\"nebula\")",
      prefix_pattern: "quasar%",
      max_prefix_expansions: 16,
      max_prefix_term_bytes: 96,
    }]);
    expect(observed.term_support).toEqual([{
      probe_id: 0,
      term_index: 0,
      document_frequency: 0,
      prefix_expansions: 16,
    }]);
    expect(observed.prefix_expansions[0]?.terms).toEqual(["quasar", ...words.slice(0, 15)]);
  });

  it("records only bounded timing, count, and evidence-class traces", () => {
    index.replaceSource(source("trace", "chunk-trace", "traceprefix00"));
    const values = [0, 1, 2, 3, 4, 5, 6, 7];
    const trace = index.beginInternalLexicalTrace(() => values.shift() ?? 7);
    index.observeQuery([{
      schema_version: 3,
      plan_id: "term_support_v3",
      probe_id: 0,
      term_index: 0,
      match_value: "{content} : (\"missing\")",
      prefix_pattern: "tracepre%",
      max_prefix_expansions: 16,
      max_prefix_term_bytes: 96,
    }], trace);
    index.search(anyPlan("traceprefix00"), 20, trace);
    const summary = index.finishInternalLexicalTrace(trace);

    expect(isInternalLexicalTrace(summary)).toBe(true);
    expect(summary).toMatchObject({
      schema_version: 1,
      outcome: "complete",
      evidence_probe_count: 1,
      prefix_probe_count: 1,
      result_count: 1,
    });
    expect(JSON.stringify(summary)).not.toMatch(/traceprefix|chunk-trace|\.md|query|path|token|property/iu);
    expect(index.latestInternalLexicalTrace()).toEqual(summary);
  });

  it("rejects privacy-field insertion into internal lexical traces", () => {
    const trace = index.beginInternalLexicalTrace(() => 0);
    const summary = index.finishInternalLexicalTrace(trace);
    expect(isInternalLexicalTrace({ ...summary, query: "secret" })).toBe(false);
    expect(isInternalLexicalTrace({ ...summary, path: "secret.md" })).toBe(false);
    expect(isInternalLexicalTrace({ ...summary, property_name: "secret" })).toBe(false);
    expect(isInternalLexicalTrace({
      ...summary,
      stages: [{
        kind: "lexical_all_terms_v3",
        mandatory: true,
        status: "completed",
        duration_ms: 0,
        input_count: 1,
        output_count: 0,
        candidate_count: 0,
        token: "secret",
      }],
      stage_count: 1,
    })).toBe(false);
  });

  it("keeps optional execution deterministic regardless of observed duration", () => {
    index.replaceSource(source("duration", "chunk-duration", "durationprefix00"));
    const values = [0, 0, 10_000, 10_000, 20_000, 20_000];
    const trace = index.beginInternalLexicalTrace(() => values.shift() ?? 20_000);
    const evidence = index.observeQuery([{
      schema_version: 3,
      plan_id: "term_support_v3",
      probe_id: 0,
      term_index: 0,
      match_value: "{content} : (\"missing\")",
      prefix_pattern: "durationpre%",
      max_prefix_expansions: 16,
      max_prefix_term_bytes: 96,
    }], trace);
    const hits = index.search({
      schema_version: 3,
      profile_id: "lexical-v1",
      disposition: "ready",
      max_total_candidates: 512,
      stages: [{
        ordinal: 0,
        plan_id: "lexical_prefix_v3",
        match_value: "{content} : (\"durationprefix00\")",
        max_candidates: 256,
      }],
    }, 20, trace);
    const summary = index.finishInternalLexicalTrace(trace);

    expect(evidence.prefix_expansions[0]?.terms).toContain("durationprefix00");
    expect(hits.map((hit) => hit.chunk_id)).toEqual(["chunk-duration"]);
    expect(summary.outcome).toBe("complete");
    expect(summary.optional_duration_ms).toBeGreaterThan(0);
    expect(summary.stages.filter((stage) => !stage.mandatory))
      .toEqual(expect.arrayContaining([
        expect.objectContaining({ kind: "evidence_prefix", status: "completed" }),
        expect.objectContaining({ kind: "lexical_prefix_v3", status: "completed" }),
      ]));
  });

  it("coerces legacy title and tags exactly like the Rust projection", () => {
    index.replaceSource(sourceAt(
      "typed-legacy",
      "typed-legacy.md",
      "chunk-typed-legacy",
      "bodycontrol",
      "unused",
      { title: 7, tags: ["one", 2, true, null, { ignored: "map" }] },
    ));

    expect(index.search(anyPlan("7"), 20)[0]?.frontmatter).toEqual({ title: "7" });
    expect(index.search(anyPlan("2"), 20)).toHaveLength(1);
    expect(index.search(anyPlan("true"), 20)).toHaveLength(1);
    expect(index.search(anyPlan("ignored"), 20)).toEqual([]);
  });

  it("stores each property once per source and returns only compact display metadata", () => {
    const db = new sqlite.oo1.DB(":memory:", "c");
    const scoped = new Fts5GenerationIndex(db);
    try {
      const frontmatter: PropertyBag = {
        title: "Projection Guide",
        tags: ["one", "two"],
        nested: { z: 3, a: [true, "leaf"] },
      };
      const prepared = sourceAt(
        "alpha",
        "alpha.md",
        "chunk-a",
        "first projectionterm",
        "Projection Guide",
        frontmatter,
      );
      const second = structuredClone(prepared.chunks[0]!);
      second.chunk.chunk_id = "chunk-b";
      second.chunk.content = "second projectionterm";
      prepared.chunks.push(second);

      scoped.replaceSource(prepared);

      expect(db.selectObjects(
        "SELECT frontmatter_json FROM chunks ORDER BY rowid",
      )).toEqual([
        { frontmatter_json: '{"title":"Projection Guide"}' },
        { frontmatter_json: '{"title":"Projection Guide"}' },
      ]);
      expect(db.selectValue("SELECT count(*) FROM source_properties")).toBe(3);
      expect(db.selectValue("SELECT count(*) FROM source_property_scalars")).toBe(6);
      expect(db.selectValue("SELECT count(*) FROM source_property_text_fts")).toBe(3);
      expect(db.selectValue(
        "SELECT property_count FROM sources WHERE source_key = 'alpha'",
      )).toBe(3);
      expect(scoped.search(anyPlan("projectionterm"), 20)).toHaveLength(2);
      expect(scoped.search(anyPlan("projectionterm"), 20)[0]!.frontmatter)
        .toEqual({ title: "Projection Guide" });
    } finally {
      scoped.close();
    }
  });

  it("does not read or parse the open property bag for an ordinary lexical search", () => {
    const recorder = new RecordingDatabase(new sqlite.oo1.DB(":memory:", "c"));
    const scoped = new Fts5GenerationIndex(recorder);
    try {
      scoped.replaceSource(sourceAt(
        "lazy",
        "lazy.md",
        "chunk-lazy",
        "lazyterm",
        "Compact Title",
        { title: "Compact Title", payload: "x".repeat(2 * 1024 * 1024) },
      ));
      recorder.selectedStatements.length = 0;

      const hits = scoped.search(anyPlan("lazyterm"), 20);

      expect(hits).toHaveLength(1);
      expect(hits[0]!.frontmatter).toEqual({ title: "Compact Title" });
      expect(recorder.selectedStatements.join("\n")).not.toMatch(/source_properties|value_json/u);
    } finally {
      scoped.close();
    }
  });

  it("projects frontmatter-only sources without depending on a chunk", () => {
    const db = new sqlite.oo1.DB(":memory:", "c");
    const scoped = new Fts5GenerationIndex(db);
    try {
      const prepared = sourceAt(
        "properties-only",
        "properties-only.md",
        "unused-chunk",
        "unused",
        "Title",
        { priority: 7 },
      );
      prepared.chunks = [];

      scoped.replaceSource(prepared);

      expect(db.selectValue("SELECT count(*) FROM source_properties")).toBe(1);
      expect(db.selectObjects(`
        SELECT property_name, root_type, exact_value
        FROM source_properties
      `)).toEqual([{ property_name: "priority", root_type: "i64", exact_value: "7" }]);
    } finally {
      scoped.close();
    }
  });

  it("preserves unsafe integers and integral floats in the durable projection", () => {
    const db = new sqlite.oo1.DB(":memory:", "c");
    const scoped = new Fts5GenerationIndex(db);
    try {
      const prepared = sourceAt(
        "numeric",
        "numeric.md",
        "chunk-numeric",
        "numericterm",
        "Numeric",
        {},
      );
      prepared.frontmatter = {
        unsafe_i64: { type: "i64", value: "-9007199254740993" },
        max_u64: { type: "u64", value: "18446744073709551615" },
        integral_float: { type: "f64", value: "405f400000000000" },
      };

      scoped.replaceSource(prepared);

      expect(db.selectObjects(`
        SELECT property_name, scalar_type, exact_value, numeric_value
        FROM source_property_scalars
        ORDER BY property_name
      `)).toEqual([
        { property_name: "integral_float", scalar_type: "real", exact_value: "405f400000000000", numeric_value: 125 },
        { property_name: "max_u64", scalar_type: "u64", exact_value: "18446744073709551615", numeric_value: null },
        { property_name: "unsafe_i64", scalar_type: "i64", exact_value: "-9007199254740993", numeric_value: null },
      ]);
      expect(scoped.search(anyPlan("numericterm"), 20)[0]!.frontmatter).toEqual({});
    } finally {
      scoped.close();
    }
  });

  it("canonicalizes top-level JSON and flattens scalar leaves to JSON Pointers", () => {
    const db = new sqlite.oo1.DB(":memory:", "c");
    const scoped = new Fts5GenerationIndex(db);
    try {
      scoped.replaceSource(sourceAt(
        "alpha",
        "alpha.md",
        "chunk-a",
        "projectionterm",
        "Title",
        {
          complex: {
            z: [{ "a/b~": "needle" }, null],
            a: 12,
          },
        },
      ));

      expect(db.selectObjects(`
        SELECT property_name, value_json, root_type, exact_value
        FROM source_properties
      `)).toEqual([{
        property_name: "complex",
        value_json: '{"type":"map","value":{"a":{"type":"i64","value":"12"},"z":{"type":"sequence","value":[{"type":"map","value":{"a/b~":{"type":"string","value":"needle"}}},{"type":"null"}]}}}',
        root_type: "object",
        exact_value: null,
      }]);
      expect(db.selectObjects(`
        SELECT json_pointer, scalar_type, exact_value
        FROM source_property_scalars
        ORDER BY json_pointer
      `)).toEqual([
        { json_pointer: "/a", scalar_type: "i64", exact_value: "12" },
        { json_pointer: "/z/0/a~1b~0", scalar_type: "string", exact_value: "needle" },
        { json_pointer: "/z/1", scalar_type: "null", exact_value: "null" },
      ]);
    } finally {
      scoped.close();
    }
  });

  it("keeps a large array to one source-property text contribution", () => {
    const db = new sqlite.oo1.DB(":memory:", "c");
    const scoped = new Fts5GenerationIndex(db);
    try {
      scoped.replaceSource(sourceAt(
        "alpha",
        "alpha.md",
        "chunk-a",
        "projectionterm",
        "Title",
        { tags: Array.from({ length: 1_200 }, (_unused, index) => `tag-${index}`) },
      ));

      expect(db.selectValue("SELECT count(*) FROM source_properties")).toBe(1);
      expect(db.selectValue("SELECT count(*) FROM source_property_scalars")).toBe(1_200);
      expect(db.selectValue("SELECT count(*) FROM source_property_text_fts")).toBe(1);
      expect(db.selectValue(`
        SELECT count(DISTINCT source_key)
        FROM source_property_scalars
        WHERE property_name = 'tags'
      `)).toBe(1);
    } finally {
      scoped.close();
    }
  });

  it("indexes an open bag of one thousand top-level properties without truncation", () => {
    const db = new sqlite.oo1.DB(":memory:", "c");
    const scoped = new Fts5GenerationIndex(db);
    try {
      const properties = Object.fromEntries(
        Array.from({ length: 1_000 }, (_unused, index) => [`property-${index}`, index]),
      );
      scoped.replaceSource(sourceAt(
        "alpha",
        "alpha.md",
        "chunk-a",
        "projectionterm",
        "Title",
        properties,
      ));

      expect(db.selectValue("SELECT count(*) FROM source_properties")).toBe(1_000);
      expect(db.selectValue("SELECT count(*) FROM source_property_scalars")).toBe(1_000);
      expect(db.selectValue("SELECT count(*) FROM source_property_text_fts")).toBe(1_000);
      expect(db.selectValue(`
        SELECT exact_value FROM source_property_scalars
        WHERE property_name = 'property-999'
      `)).toBe("999");
    } finally {
      scoped.close();
    }
  });

  it("keeps exact and textual projections separated by scalar type", () => {
    const db = new sqlite.oo1.DB(":memory:", "c");
    const scoped = new Fts5GenerationIndex(db);
    try {
      scoped.replaceSource(sourceAt(
        "alpha",
        "alpha.md",
        "chunk-a",
        "projectionterm",
        "Title",
        {
          string_value: "12",
          integer_value: 12,
          real_value: 12.5,
          boolean_value: true,
          date_value: "2026-07-28",
          invalid_date: "2026-02-30",
        },
      ));

      expect(db.selectObjects(`
        SELECT property_name, scalar_type, exact_value, numeric_value, date_value
        FROM source_property_scalars
        ORDER BY property_name
      `)).toEqual([
        { property_name: "boolean_value", scalar_type: "boolean", exact_value: "true", numeric_value: null, date_value: null },
        { property_name: "date_value", scalar_type: "date", exact_value: "2026-07-28", numeric_value: null, date_value: "2026-07-28" },
        { property_name: "integer_value", scalar_type: "i64", exact_value: "12", numeric_value: 12, date_value: null },
        { property_name: "invalid_date", scalar_type: "string", exact_value: "2026-02-30", numeric_value: null, date_value: null },
        { property_name: "real_value", scalar_type: "real", exact_value: "4029000000000000", numeric_value: 12.5, date_value: null },
        { property_name: "string_value", scalar_type: "string", exact_value: "12", numeric_value: null, date_value: null },
      ]);

      const textMatches = (match: string): string[] => db.selectObjects(`
        SELECT p.property_name
        FROM source_property_text_fts AS f
        JOIN source_properties AS p ON p.rowid = f.rowid
        WHERE source_property_text_fts MATCH ?
        ORDER BY p.property_name
        LIMIT ?
      `, [match, 10]).map((row) => String(row.property_name));
      expect(textMatches('string_value : "12"')).toEqual(["string_value"]);
      expect(textMatches('integer_value : "12"')).toEqual(["integer_value"]);
      expect(textMatches('boolean_value : "true"')).toEqual(["boolean_value"]);
      expect(textMatches('date_value : "2026 07 28"')).toEqual(["date_value"]);
    } finally {
      scoped.close();
    }
  });

  it("keeps each property-text leg independently filtered and bounded", () => {
    const db = new sqlite.oo1.DB(":memory:", "c");
    const scoped = new Fts5GenerationIndex(db);
    try {
      scoped.applySourceChanges([
        sourceAt("alpha", "alpha.md", "chunk-a", "body-a", "Title", {
          summary: "common needle",
          other: "common needle",
        }),
        sourceAt("beta", "beta.md", "chunk-b", "body-b", "Title", {
          summary: "common needle",
        }),
        sourceAt("gamma", "gamma.md", "chunk-c", "body-c", "Title", {
          summary: "common needle",
        }),
      ], []);

      const textLeg = (propertyName: string, limit: number) => db.selectObjects(`
        SELECT p.source_key, p.property_name
        FROM source_property_text_fts AS f
        JOIN source_properties AS p ON p.rowid = f.rowid
        WHERE source_property_text_fts MATCH ? AND p.property_name = ?
        ORDER BY p.source_key
        LIMIT ?
      `, ['string_value : "needle"', propertyName, limit]);
      expect(textLeg("summary", 2)).toEqual([
        { source_key: "alpha", property_name: "summary" },
        { source_key: "beta", property_name: "summary" },
      ]);
      expect(textLeg("other", 2)).toEqual([
        { source_key: "alpha", property_name: "other" },
      ]);
    } finally {
      scoped.close();
    }
  });

  it("provides dedicated B-tree plans for presence, exact, numeric, and date rules", () => {
    const db = new sqlite.oo1.DB(":memory:", "c");
    const scoped = new Fts5GenerationIndex(db);
    try {
      scoped.replaceSource(sourceAt(
        "alpha",
        "alpha.md",
        "chunk-a",
        "projectionterm",
        "Title",
        { priority: 12, due: "2026-07-28" },
      ));

      const plan = (sql: string, bind: readonly unknown[]): string => db.selectObjects(
        `EXPLAIN QUERY PLAN ${sql}`,
        bind,
      ).map((row) => String(row.detail)).join("\n");
      expect(plan(
        "SELECT source_key FROM source_properties WHERE property_name = ?",
        ["priority"],
      )).toContain("source_properties_presence");
      expect(plan(
        "SELECT source_key FROM source_property_scalars "
          + "WHERE property_name = ? AND scalar_type = ? AND exact_value = ?",
        ["priority", "i64", "12"],
      )).toContain("source_property_scalars_exact");
      expect(plan(
        "SELECT source_key FROM source_property_scalars "
          + "WHERE property_name = ? AND numeric_value >= ? AND numeric_value IS NOT NULL",
        ["priority", 10],
      )).toContain("source_property_scalars_numeric");
      expect(plan(
        "SELECT source_key FROM source_property_scalars "
          + "WHERE property_name = ? AND date_value <= ? AND date_value IS NOT NULL",
        ["due", "2026-07-31"],
      )).toContain("source_property_scalars_date");
    } finally {
      scoped.close();
    }
  });

  it("keeps canonical FTS content external while phrase and column queries work", () => {
    const db = new sqlite.oo1.DB(":memory:", "c");
    const scoped = new Fts5GenerationIndex(db);
    try {
      scoped.replaceSource(source("alpha", "chunk-a", "portable quasar cache storage", "Quasar Guide"));

      expect(db.selectValue(
        "SELECT count(*) FROM pragma_table_info('chunks') WHERE name = 'content'",
      )).toBe(1);
      expect(db.selectValue("SELECT content FROM chunks WHERE rowid = 1"))
        .toBe("portable quasar cache storage");
      expect(db.selectValue("SELECT content FROM chunks_fts WHERE rowid = 1"))
        .toBe("portable quasar cache storage");
      expect(db.selectValue(
        "SELECT snippet(chunks_fts, 7, '[', ']', '…', 8) FROM chunks_fts WHERE chunks_fts MATCH ?",
        ['"quasar"'],
      )).toContain("[quasar]");

      expect(scoped.search(matchPlan("lexical_explicit_v3", "\"quasar cache\""), 20)).toHaveLength(1);
      expect(scoped.search(matchPlan("lexical_explicit_v3", "\"cache quasar\""), 20)).toEqual([]);
      expect(scoped.search(matchPlan("lexical_explicit_v3", "title : \"quasar\""), 20)).toHaveLength(1);
      expect(scoped.search(matchPlan("lexical_explicit_v3", "tags : \"quasar\""), 20)).toEqual([]);
    } finally {
      scoped.close();
    }
  });

  it("deletes contentless postings when a source is replaced or removed", () => {
    const db = new sqlite.oo1.DB(":memory:", "c");
    const scoped = new Fts5GenerationIndex(db);
    try {
      scoped.replaceSource(source("alpha", "chunk-a", "firstterm"));
      expect(db.selectValue("SELECT count(*) FROM chunks_fts")).toBe(1);
      expect(db.selectValue("SELECT count(*) FROM source_property_text_fts")).toBe(2);

      // Replacement frees rowid 1; the replacement must not reuse it while a
      // stale posting could still exist, and the old posting must be gone.
      scoped.replaceSource(source("alpha", "chunk-b", "secondterm"));
      expect(db.selectValue("SELECT count(*) FROM chunks_fts")).toBe(1);
      expect(scoped.search(anyPlan("firstterm"), 20)).toEqual([]);
      expect(db.selectValue("SELECT min(rowid) FROM chunks")).toBe(2);

      scoped.applySourceChanges([], [{ vault_id: "active", path: "alpha.md" }], true);
      expect(db.selectValue("SELECT count(*) FROM chunks_fts")).toBe(0);
      expect(db.selectValue("SELECT count(*) FROM chunks")).toBe(0);
      expect(db.selectValue("SELECT count(*) FROM source_properties")).toBe(0);
      expect(db.selectValue("SELECT count(*) FROM source_property_scalars")).toBe(0);
      expect(db.selectValue("SELECT count(*) FROM source_property_text_fts")).toBe(0);
      expect(scoped.search(anyPlan("secondterm"), 20)).toEqual([]);
      expect(() => scoped.assertIntegrity()).not.toThrow();
    } finally {
      scoped.close();
    }
  });

  it("atomically replaces a source and preserves it after a failed replacement", () => {
    index.replaceSource(source("alpha", "chunk-a", "firstterm"));
    index.replaceSource(source("alpha", "chunk-b", "secondterm"));
    expect(index.search(anyPlan("firstterm"), 20)).toEqual([]);
    expect(index.search(anyPlan("secondterm"), 20)).toHaveLength(1);

    const invalid = source("alpha", "duplicate", "thirdterm");
    invalid.chunks.push(structuredClone(invalid.chunks[0]!));
    expect(() => index.replaceSource(invalid)).toThrow();
    expect(index.search(anyPlan("secondterm"), 20)).toHaveLength(1);
    expect(index.documents).toBe(1);
    expect(index.chunks).toBe(1);
  });

  it("applies mixed removals and upserts in one batch", () => {
    index.applySourceChanges([
      source("alpha", "chunk-a", "alphaterm"),
      source("beta", "chunk-b", "betaterm"),
    ], []);

    index.applySourceChanges(
      [source("gamma", "chunk-g", "gammaterm")],
      [{ vault_id: "active", path: "beta.md" }],
      true,
    );

    expect(index.documents).toBe(2);
    expect(index.chunks).toBe(2);
    expect(index.search(anyPlan("alphaterm"), 20)).toHaveLength(1);
    expect(index.search(anyPlan("betaterm"), 20)).toEqual([]);
    expect(index.search(anyPlan("gammaterm"), 20)).toHaveLength(1);
  });

  it("renames by removing the old identity and inserting the new source", () => {
    index.replaceSource(source("alpha", "chunk-a", "oldterm"));
    index.applySourceChanges(
      [sourceAt("renamed-key", "renamed.md", "chunk-new", "newterm")],
      [{ vault_id: "active", path: "alpha.md" }],
      true,
    );

    expect(index.documents).toBe(1);
    expect(index.search(anyPlan("oldterm"), 20)).toEqual([]);
    expect(index.search(anyPlan("newterm"), 20)[0]).toMatchObject({ path: "renamed.md" });
  });

  it("rolls back every source when a later insert collides", () => {
    index.replaceSource(source("alpha", "chunk-a", "stableterm"));
    expect(() => index.applySourceChanges([
      source("beta", "chunk-b", "validterm"),
      source("gamma", "chunk-a", "collisionterm"),
    ], [], true)).toThrow();

    expect(index.documents).toBe(1);
    expect(index.chunks).toBe(1);
    expect(index.search(anyPlan("stableterm"), 20)).toHaveLength(1);
    expect(index.search(anyPlan("validterm"), 20)).toEqual([]);
    expect(index.search(anyPlan("collisionterm"), 20)).toEqual([]);
  });

  it("rejects a capacity overflow without changing rows or counters", () => {
    index.close();
    index = openFts5Generation(sqlite, {
      maxChunks: 1,
      maxDatabaseBytes: 1_048_576,
    });
    index.replaceSource(source("alpha", "chunk-a", "stableterm"));

    expect(() => index.applySourceChanges([
      source("beta", "chunk-b", "overflowterm"),
    ], [], true)).toThrow(IndexCapacityError);

    expect(index.documents).toBe(1);
    expect(index.chunks).toBe(1);
    expect(index.search(anyPlan("stableterm"), 20)).toHaveLength(1);
    expect(index.search(anyPlan("overflowterm"), 20)).toEqual([]);
  });

  it("counts a 10,000-null projection against durable SQLite pages and rolls it back", () => {
    const stable = source("stable", "chunk-stable", "stableterm");
    const overflow = sourceAt(
      "overflow",
      "overflow.md",
      "chunk-overflow",
      "overflowterm",
      "Overflow",
      { values: Array.from({ length: 10_000 }, () => null) },
    );

    const probeDb = new sqlite.oo1.DB(":memory:", "c");
    const probe = new Fts5GenerationIndex(probeDb);
    let stableBytes = 0;
    let overflowBytes = 0;
    try {
      probe.replaceSource(stable);
      stableBytes = probe.databaseBytes;
      probe.replaceSource(overflow);
      overflowBytes = probe.databaseBytes;
    } finally {
      probe.close();
    }
    expect(overflowBytes).toBeGreaterThan(stableBytes + 1_000_000);

    const limitedDb = new sqlite.oo1.DB(":memory:", "c");
    const limited = new Fts5GenerationIndex(limitedDb, {
      maxChunks: 100,
      maxDatabaseBytes: stableBytes + Math.floor((overflowBytes - stableBytes) / 2),
    });
    try {
      limited.replaceSource(stable);
      const bytesBefore = limited.databaseBytes;
      expect(() => limited.replaceSource(overflow)).toThrow(IndexCapacityError);

      expect(limited.databaseBytes).toBe(bytesBefore);
      expect(limited.databaseBytes).toBeLessThanOrEqual(limited.databaseByteLimit);
      expect(limited.documents).toBe(1);
      expect(limited.chunks).toBe(1);
      expect(limited.search(anyPlan("stableterm"), 20)).toHaveLength(1);
      expect(limited.search(anyPlan("overflowterm"), 20)).toEqual([]);
      expect(limitedDb.selectValue("SELECT count(*) FROM sources WHERE source_key = 'overflow'"))
        .toBe(0);
      expect(limitedDb.selectValue(
        "SELECT count(*) FROM source_properties WHERE source_key = 'overflow'",
      )).toBe(0);
      expect(limitedDb.selectValue(
        "SELECT count(*) FROM source_property_scalars WHERE source_key = 'overflow'",
      )).toBe(0);
      expect(limitedDb.selectValue(`
        SELECT count(*) FROM source_property_text_fts f
        JOIN source_properties p ON p.rowid = f.rowid
        WHERE p.source_key = 'overflow'
      `)).toBe(0);
    } finally {
      limited.close();
    }
  });

  it("distinguishes indexed empty documents from skipped sources", () => {
    const empty = source("empty", "unused", "");
    empty.chunks = [];
    index.replaceSource(empty);
    expect(index.documents).toBe(1);
    expect(index.chunks).toBe(0);

    const skipped = structuredClone(empty);
    skipped.kind = "skipped";
    skipped.coverage = "skipped-no-extractable-text";
    skipped.content_hash = null;
    index.replaceSource(skipped);
    expect(index.documents).toBe(0);
    expect(index.chunks).toBe(0);
    // The row itself must survive: a restore has to be able to tell a source
    // that was seen and skipped from one that was never seen at all.
    expect(index.sources).toBe(1);
  });

  // The freshness table is the differential state a later restore reconciles
  // against, so a skip has to be queryable — with the hash the Rust adapter
  // actually produced, not a hash TypeScript invented.
  it("records a skipped source with its adapter-produced hash and zero tallies", () => {
    const db = new sqlite.oo1.DB(":memory:", "c");
    const scoped = new Fts5GenerationIndex(db);
    try {
      const skipped = source("binary", "unused", "");
      skipped.chunks = [];
      skipped.kind = "skipped";
    skipped.coverage = "skipped-no-extractable-text";
      skipped.content_hash = "hash-binary";
      skipped.normalized_exact.title = null;
      skipped.byte_length = 4;
      skipped.mtime_nanos = "170000000000000000123456789";
      scoped.replaceSource(skipped);

      const rows = db.selectObjects("SELECT * FROM sources");
      expect(rows).toEqual([{
        source_key: "binary",
        vault_id: "active",
        path: "binary.md",
        source_format: "markdown",
        // The identity this build compiles for Markdown. A restore compares
        // this per row, so a row that cannot state it is not reusable.
        format_identity: FORMAT_IDENTITIES.markdown,
        extraction_coverage: "skipped-no-extractable-text",
        outcome: "skipped",
        content_hash: "hash-binary",
        byte_length: 4,
        // Stored as TEXT: a 27-digit nanosecond stamp does not fit a 64-bit
        // INTEGER, and a truncated value would corrupt the freshness compare.
        mtime_nanos: "170000000000000000123456789",
        retrieval_json: '{"filename":"binary.md","stem":"binary","aliases":[]}',
        exact_filename: "binary.md",
        exact_stem: "binary",
        exact_aliases_json: "[]",
        exact_title: null,
        aliases_text: "",
        // A skipped source retains freshness evidence only; it cannot claim
        // searchable metadata that Rust did not index.
        title_text: "",
        tags_text: "",
        chunk_count: 0,
        property_count: 0,
        property_scalar_count: 0,
      }]);
      expect(scoped.documents).toBe(0);
      expect(scoped.sources).toBe(1);
      expect(() => scoped.assertIntegrity()).not.toThrow();
    } finally {
      scoped.close();
    }
  });

  // The one skip outcome that genuinely has no hash: a source over the file
  // ceiling is refused before it is ever read.
  it("records an oversized skip with a null hash and stays counter-stable when repeated", () => {
    const db = new sqlite.oo1.DB(":memory:", "c");
    const scoped = new Fts5GenerationIndex(db);
    try {
      const oversized = source("huge", "unused", "");
      oversized.chunks = [];
      oversized.kind = "skipped";
      oversized.coverage = "unreadable";
      oversized.content_hash = null;
      oversized.byte_length = 10 * 1024 * 1024 + 1;

      scoped.replaceSource(oversized);
      expect(db.selectValue("SELECT content_hash FROM sources WHERE source_key = 'huge'"))
        .toBe(null);
      expect(db.selectValue("SELECT byte_length FROM sources WHERE source_key = 'huge'"))
        .toBe(10 * 1024 * 1024 + 1);

      // Re-preparing an already-recorded skip must not drive any counter
      // negative — that would turn a benign re-scan into a rejected batch.
      scoped.replaceSource(structuredClone(oversized));
      scoped.replaceSource(structuredClone(oversized));
      expect(scoped.documents).toBe(0);
      expect(scoped.chunks).toBe(0);
      expect(scoped.sources).toBe(1);
      expect(db.selectValue("SELECT count(*) FROM sources")).toBe(1);
      expect(() => scoped.assertIntegrity()).not.toThrow();
    } finally {
      scoped.close();
    }
  });

  it("round-trips indexed freshness facts verbatim and drops the row on removal", () => {
    const db = new sqlite.oo1.DB(":memory:", "c");
    const scoped = new Fts5GenerationIndex(db);
    try {
      const indexed = source("alpha", "chunk-a", "quasarterm");
      indexed.byte_length = 10;
      indexed.mtime_nanos = "99999999999999999999999999999999999999";
      scoped.replaceSource(indexed);

      expect(db.selectObjects("SELECT * FROM sources")).toEqual([{
        source_key: "alpha",
        vault_id: "active",
        path: "alpha.md",
        source_format: "markdown",
        format_identity: FORMAT_IDENTITIES.markdown,
        extraction_coverage: "indexed-complete",
        outcome: "indexed",
        content_hash: "hash-alpha",
        byte_length: 10,
        mtime_nanos: "99999999999999999999999999999999999999",
        retrieval_json: '{"filename":"alpha.md","stem":"alpha","aliases":[]}',
        exact_filename: "alpha.md",
        exact_stem: "alpha",
        exact_aliases_json: "[]",
        exact_title: "title",
        aliases_text: "",
        title_text: "Title",
        tags_text: "test",
        chunk_count: 1,
        property_count: 2,
        property_scalar_count: 2,
      }]);
      expect(scoped.sources).toBe(1);

      scoped.applySourceChanges([], [{ vault_id: "active", path: "alpha.md" }], true);
      expect(db.selectValue("SELECT count(*) FROM sources")).toBe(0);
      expect(scoped.sources).toBe(0);
      expect(scoped.documents).toBe(0);
    } finally {
      scoped.close();
    }
  });

  it("mixes indexed and skipped sources in one batch and counts only indexed as documents", () => {
    const skipped = source("skipped", "unused", "");
    skipped.chunks = [];
    skipped.kind = "skipped";
    skipped.coverage = "skipped-no-extractable-text";

    index.applySourceChanges([
      source("alpha", "chunk-a", "alphaterm"),
      skipped,
    ], [], true);

    expect(index.documents).toBe(1);
    expect(index.chunks).toBe(1);
    expect(index.sources).toBe(2);
    expect(index.sourceFormatCounts.markdown["indexed-complete"]).toBe(1);
    expect(index.sourceFormatCounts.markdown["skipped-no-extractable-text"]).toBe(1);
  });

  it("refuses a batch that would exceed the source ceiling without changing rows", () => {
    index.close();
    index = openFts5Generation(sqlite, {
      maxChunks: 100,
      maxDatabaseBytes: 1_048_576,
      maxSources: 1,
    });
    index.replaceSource(source("alpha", "chunk-a", "stableterm"));

    expect(() => index.replaceSource(source("beta", "chunk-b", "overflowterm")))
      .toThrow(IndexCapacityError);
    expect(index.sources).toBe(1);
    expect(index.documents).toBe(1);
    expect(index.search(anyPlan("overflowterm"), 20)).toEqual([]);
  });

  // The reconciliation clauses that make the stored per-source tallies
  // trustworthy have to be provably fail-able, so each is corrupted directly.
  it.each([
    [
      "a chunk outliving its source row",
      "DELETE FROM sources WHERE source_key = 'alpha'",
    ],
    [
      "a skipped source owning chunks",
      "UPDATE sources SET outcome = 'skipped', extraction_coverage = 'unreadable', "
        + "chunk_count = 0, property_count = 0, property_scalar_count = 0, content_hash = NULL "
        + "WHERE source_key = 'alpha'",
    ],
    [
      "a tampered per-source chunk tally",
      "UPDATE sources SET chunk_count = 5 WHERE source_key = 'alpha'",
    ],
    [
      "an invented source row",
      "INSERT INTO sources VALUES('ghost','active','ghost.md','markdown',"
        + `'${FORMAT_IDENTITIES.markdown}',`
        + "'unreadable',"
        + "'skipped',NULL,0,'1','{\"filename\":\"ghost.md\",\"stem\":\"ghost\","
        + "\"aliases\":[]}','ghost.md','ghost','[]',NULL,'','','',0,0,0)",
    ],
  ])("fails the integrity gate on %s", (_name, corruption) => {
    const db = new sqlite.oo1.DB(":memory:", "c");
    const scoped = new Fts5GenerationIndex(db);
    try {
      scoped.replaceSource(source("alpha", "chunk-a", "quasar"));
      expect(() => scoped.assertIntegrity()).not.toThrow();

      db.exec(corruption);

      // External-content integrity catches text/content divergence; explicit
      // reconciliation catches source tallies and ownership. The combined gate must fail.
      expect(() => scoped.assertIntegrity()).toThrow(/integrity check failed/);
    } finally {
      scoped.close();
    }
  });

  it("rejects a stored source row whose recorded facts are malformed", () => {
    const db = new sqlite.oo1.DB(":memory:", "c");
    const scoped = new Fts5GenerationIndex(db);
    try {
      scoped.replaceSource(source("alpha", "chunk-a", "quasar"));
      // Writing through the raw handle bypasses the column CHECKs only for
      // shapes the CHECKs do not cover; the read side must catch it anyway.
      db.exec("UPDATE sources SET mtime_nanos = '00000000000000000000000000000000000000000' "
        + "WHERE source_key = 'alpha'");

      expect(() => scoped.replaceSource(source("alpha", "chunk-b", "replacementterm")))
        .toThrow(/stored source metadata is invalid/);
    } finally {
      scoped.close();
    }
  });

  it("refuses a batch that would mix chunking versions in one generation", () => {
    index.replaceSource(source("alpha", "chunk-a", "quasar"));
    expect(index.chunkingVersion).toBe(1);

    const rechunked = source("beta", "chunk-b", "pulsarterm");
    rechunked.chunks[0]!.chunk.chunking_version = 2;
    expect(() => index.replaceSource(rechunked)).toThrow(/mixes chunking versions/);

    expect(index.chunkingVersion).toBe(1);
    expect(index.documents).toBe(1);
    expect(index.sources).toBe(1);
    expect(index.search(anyPlan("pulsarterm"), 20)).toEqual([]);
  });

  it("treats a missing removal as an idempotent no-op", () => {
    index.replaceSource(source("alpha", "chunk-a", "stableterm"));
    index.applySourceChanges([], [{ vault_id: "active", path: "missing.md" }], true);
    expect(index.documents).toBe(1);
    expect(index.chunks).toBe(1);
    expect(index.search(anyPlan("stableterm"), 20)).toHaveLength(1);
  });

  it("passes the FTS5 integrity check and closes idempotently", () => {
    index.replaceSource(source("alpha", "chunk-a", "quasar"));
    expect(() => index.assertIntegrity()).not.toThrow();
    index.close();
    expect(() => index.close()).not.toThrow();
  });

  // Negative control for the combined external-content FTS and metadata gate.
  it("fails the publish integrity gate when the index and metadata desync", () => {
    const orphaned = new sqlite.oo1.DB(":memory:", "c");
    const withOrphanFts = new Fts5GenerationIndex(orphaned);
    try {
      withOrphanFts.replaceSource(source("alpha", "chunk-a", "quasar"));
      orphaned.exec("DELETE FROM chunks WHERE rowid = 1");
      expect(() => withOrphanFts.assertIntegrity()).toThrow(/integrity check failed/);
    } finally {
      withOrphanFts.close();
    }

    const missing = new sqlite.oo1.DB(":memory:", "c");
    const withMissingFts = new Fts5GenerationIndex(missing);
    try {
      withMissingFts.replaceSource(source("alpha", "chunk-a", "quasar"));
      missing.exec("DELETE FROM chunks_fts WHERE rowid = 1");
      expect(() => withMissingFts.assertIntegrity()).toThrow(/integrity check failed/);
    } finally {
      withMissingFts.close();
    }
  });

  it("fails the publish integrity gate when property scalars and text postings diverge", () => {
    const orphaned = new sqlite.oo1.DB(":memory:", "c");
    const withOrphanPropertyFts = new Fts5GenerationIndex(orphaned);
    try {
      withOrphanPropertyFts.replaceSource(source("alpha", "chunk-a", "quasar"));
      orphaned.exec("DELETE FROM source_properties WHERE rowid = 1");
      expect(() => withOrphanPropertyFts.assertIntegrity()).toThrow(/integrity check failed/);
    } finally {
      withOrphanPropertyFts.close();
    }

    const missing = new sqlite.oo1.DB(":memory:", "c");
    const withMissingPropertyFts = new Fts5GenerationIndex(missing);
    try {
      withMissingPropertyFts.replaceSource(source("alpha", "chunk-a", "quasar"));
      missing.exec("DELETE FROM source_property_text_fts WHERE rowid = 1");
      expect(() => withMissingPropertyFts.assertIntegrity()).toThrow(/integrity check failed/);
    } finally {
      withMissingPropertyFts.close();
    }
  });

  it("fails the publish integrity gate when stored rows disagree with the counters", () => {
    const db = new sqlite.oo1.DB(":memory:", "c");
    const scoped = new Fts5GenerationIndex(db);
    try {
      scoped.replaceSource(source("alpha", "chunk-a", "quasar"));
      db.exec("DELETE FROM chunks WHERE rowid = 1");
      db.exec("DELETE FROM chunks_fts WHERE rowid = 1");
      expect(scoped.chunks).toBe(1);
      expect(() => scoped.assertIntegrity()).toThrow(/integrity check failed/);
    } finally {
      scoped.close();
    }
  });

  it("optimizes and vacuums a generation without changing its results", () => {
    const db = new sqlite.oo1.DB(":memory:", "c");
    const scoped = new Fts5GenerationIndex(db);
    try {
      scoped.replaceSource(source("alpha", "chunk-a", "quasar"));
      scoped.replaceSource(source("beta", "chunk-b", "quasar"));
      scoped.applySourceChanges([], [{ vault_id: "active", path: "beta.md" }]);
      const before = db.selectValue("SELECT count(*) FROM chunks_fts");

      expect(() => scoped.compact()).not.toThrow();

      expect(db.selectValue("SELECT count(*) FROM chunks_fts")).toBe(before);
      expect(scoped.search(anyPlan("quasar"), 20)).toHaveLength(1);
      expect(() => scoped.assertIntegrity()).not.toThrow();
    } finally {
      scoped.close();
    }
  });

  // "Results are unchanged" is satisfied by an empty compact(); the ruling
  // requires the two statements themselves, in order, outside a transaction.
  it("issues exactly the FTS optimize and then VACUUM when compacting", () => {
    const recorder = new RecordingDatabase(new sqlite.oo1.DB(":memory:", "c"));
    const scoped = new Fts5GenerationIndex(recorder);
    try {
      scoped.replaceSource(source("alpha", "chunk-a", "quasar"));
      recorder.statements.length = 0;

      scoped.compact();

      expect(recorder.statements).toEqual([
        "INSERT INTO chunks_fts(chunks_fts) VALUES('optimize')",
        "INSERT INTO chunk_exact_identifier_fts(chunk_exact_identifier_fts) VALUES('optimize')",
        "INSERT INTO source_property_text_fts(source_property_text_fts) VALUES('optimize')",
        "VACUUM",
      ]);
      expect(recorder.openTransactions).toBe(0);
    } finally {
      scoped.close();
    }
  });

  // The observable half: VACUUM must actually rebuild the published image
  // without the pages a churned build freed.
  it("reclaims the pages a churned generation freed before it is published", () => {
    const db = new sqlite.oo1.DB(":memory:", "c");
    const scoped = new Fts5GenerationIndex(db);
    try {
      const filler = "quasarterm pulsarterm nebulaterm lorem ipsum dolor sit amet ".repeat(24);
      for (let index = 0; index < 40; index += 1) {
        scoped.replaceSource(source(`note-${index}`, `chunk-${index}`, `${filler} ${index}`));
      }
      scoped.applySourceChanges(
        [],
        Array.from({ length: 38 }, (_unused, index) => ({
          vault_id: "active",
          path: `note-${index}.md`,
        })),
      );

      const pagesBefore = Number(db.selectValue("PRAGMA page_count"));
      expect(Number(db.selectValue("PRAGMA freelist_count"))).toBeGreaterThan(0);

      scoped.compact();

      expect(Number(db.selectValue("PRAGMA freelist_count"))).toBe(0);
      expect(Number(db.selectValue("PRAGMA page_count"))).toBeLessThan(pagesBefore);
      expect(scoped.chunks).toBe(2);
      expect(() => scoped.assertIntegrity()).not.toThrow();
    } finally {
      scoped.close();
    }
  });

  // Seeding the allocator from `chunks` alone would hand rowid 1 straight back
  // to the new chunk, shadowing the stray posting: the contentless table
  // accepts the duplicate silently and reconciliation then sees matching
  // counts and no orphan, so the corruption becomes permanently invisible.
  it("allocates chunk rowids above an FTS-only posting so it stays visible", () => {
    const db = new sqlite.oo1.DB(":memory:", "c");
    const scoped = new Fts5GenerationIndex(db);
    try {
      db.exec("INSERT INTO chunks_fts(rowid, content) VALUES(1, 'orphanterm')");

      scoped.replaceSource(source("alpha", "chunk-a", "quasar"));

      expect(db.selectValue("SELECT min(rowid) FROM chunks")).toBe(2);
      expect(db.selectValue("SELECT count(*) FROM chunks_fts_docsize")).toBe(2);
      expect(() => scoped.assertIntegrity()).toThrow(/integrity check failed/);
    } finally {
      scoped.close();
    }
  });

  it("allocates chunk rowids above an exact-FTS-only posting so it stays visible", () => {
    const db = new sqlite.oo1.DB(":memory:", "c");
    const scoped = new Fts5GenerationIndex(db);
    try {
      db.exec(
        "INSERT INTO chunk_exact_identifier_fts(rowid, token) VALUES(1, ?)",
        { bind: [encodeExactIdentifierToken("orphan identifier")] },
      );

      scoped.replaceSource(source("alpha", "chunk-a", "quasar"));

      expect(db.selectValue("SELECT min(rowid) FROM chunks")).toBe(2);
      expect(db.selectValue("SELECT count(*) FROM chunk_exact_identifier_fts_docsize")).toBe(2);
      expect(() => scoped.assertIntegrity()).toThrow(/integrity check failed/);
    } finally {
      scoped.close();
    }
  });

  it("allocates property rowids above a property-FTS-only posting", () => {
    const db = new sqlite.oo1.DB(":memory:", "c");
    const scoped = new Fts5GenerationIndex(db);
    try {
      db.exec(
        "INSERT INTO source_property_text_fts(rowid, string_value) VALUES(1, 'orphanterm')",
      );

      scoped.replaceSource(source("alpha", "chunk-a", "quasar"));

      expect(db.selectValue("SELECT min(rowid) FROM source_properties")).toBe(2);
      expect(db.selectValue("SELECT count(*) FROM source_property_text_fts")).toBe(3);
      expect(() => scoped.assertIntegrity()).toThrow(/integrity check failed/);
    } finally {
      scoped.close();
    }
  });

  it("exports a working image whose schema version is stamped into its header", () => {
    index.replaceSource(source("alpha", "chunk-a", "quasarterm"));

    const image = index.exportImage(sqlite);
    expect(image).toBeInstanceOf(Uint8Array);
    expect(image.byteLength).toBeGreaterThan(0);
    // The live generation is untouched by serialization.
    expect(index.search(anyPlan("quasarterm"), 20)).toHaveLength(1);

    const restored = deserialize(sqlite, image);
    try {
      expect(Number(restored.selectValue("PRAGMA user_version"))).toBe(CACHE_SCHEMA_VERSION);
      expect(restored.selectValue("SELECT count(*) FROM sources")).toBe(1);
      expect(restored.selectValue("SELECT outcome FROM sources")).toBe("indexed");
      expect(restored.selectValue(
        "SELECT path FROM chunks c JOIN chunks_fts f ON f.rowid = c.rowid "
        + "WHERE chunks_fts MATCH ?",
        ['"quasarterm"'],
      )).toBe("alpha.md");
    } finally {
      restored.close();
    }
  });

  it("requires the durable database budget to fit inside the transport ceiling", () => {
    index.close();
    expect(() => openFts5Generation(sqlite, {
      maxChunks: 100,
      maxDatabaseBytes: 1_048_576,
      maxExportBytes: 1_024,
    })).toThrow(/database byte limit must not exceed export byte limit/);
    index = openFts5Generation(sqlite);
  });

  // A generation that is already published has no later commit gate, so its
  // caller asks for the reconciliation inline and a divergence must roll the
  // whole batch back rather than land in a live index.
  it("reconciles inside the transaction on request and rolls a diverged batch back", () => {
    const db = new sqlite.oo1.DB(":memory:", "c");
    const scoped = new Fts5GenerationIndex(db);
    try {
      scoped.replaceSource(source("alpha", "chunk-a", "quasar"));
      db.exec("INSERT INTO chunks_fts(rowid, content) VALUES(9000, 'orphanterm')");

      // Unverified, the divergence is carried straight through.
      scoped.applySourceChanges([source("beta", "chunk-b", "pulsarterm")], []);
      expect(db.selectValue("SELECT count(*) FROM sources")).toBe(2);
      expect(scoped.search(anyPlan("pulsarterm"), 20)).toHaveLength(1);

      // Verified, the same batch is refused and nothing of it survives.
      expect(() => scoped.applySourceChanges(
        [source("gamma", "chunk-c", "nebulaterm")],
        [],
        true,
      )).toThrow(/canonical content/);
      expect(db.selectValue("SELECT count(*) FROM sources")).toBe(2);
      expect(db.selectValue("SELECT count(*) FROM chunks WHERE chunk_id = 'chunk-c'")).toBe(0);
      expect(scoped.search(anyPlan("nebulaterm"), 20)).toEqual([]);
    } finally {
      scoped.close();
    }
  });

  it("evicts only the format whose identity moved and keeps every other row", () => {
    index.applySourceChanges([
      source("alpha", "chunk-a", "alphaterm"),
      formatted("notes", "notes.txt", "text", "chunk-n", "notesterm"),
      formatted("board", "board.excalidraw", "excalidraw", "chunk-b", "boardterm"),
    ], []);
    index.assertIntegrity();

    // Exactly what a shipped `extractor_version_for(Text)` bump looks like from
    // the cache's point of view: the plain-text extractor moved, nothing else.
    const image = mutateExportedImage(index.exportImage(sqlite), (db) => {
      db.exec("UPDATE sources SET format_identity = ? WHERE source_format = 'text'", {
        bind: ["b".repeat(64)],
      });
    });

    const restored = openRestoredFts5Generation(sqlite, image, 1);
    try {
      expect(restored.evictions.stale_identity.text).toBe(1);
      expect(restored.evictions.stale_identity.markdown).toBe(0);
      expect(restored.evictions.stale_identity.excalidraw).toBe(0);
      expect(restored.evictions.disabled_format.text).toBe(0);
      // Every other format's rows are reusable and reused. That is the whole
      // saving: one extractor moved, one format's sources get re-read.
      expect(restored.sources).toBe(2);
      expect(restored.documents).toBe(2);
      expect(restored.chunks).toBe(2);
      expect(restored.sourceFormatCounts.markdown["indexed-complete"]).toBe(1);
      expect(restored.sourceFormatCounts.excalidraw["indexed-complete"]).toBe(1);
      expect(restored.sourceFormatCounts.text["indexed-complete"]).toBe(0);
      expect(restored.search(anyPlan("alphaterm"), 20)).toHaveLength(1);
      expect(restored.search(anyPlan("boardterm"), 20)).toHaveLength(1);
      // Eviction is an open-time transformation, not a read-time filter: the
      // very first query after restore must already be unable to see the row.
      expect(restored.search(anyPlan("notesterm"), 20)).toEqual([]);
      // Nothing is orphaned: the chunk, its FTS posting, its exact aliases,
      // its properties, and its per-source tallies all left with the row, or
      // the reconciliation check inside assertIntegrity would refuse the image.
      expect(() => restored.assertIntegrity()).not.toThrow();
    } finally {
      restored.close();
    }
  });

  it("removes a disabled format's rows before the restored image answers anything", () => {
    index.applySourceChanges([
      source("alpha", "chunk-a", "alphaterm"),
      formatted("paper", "paper.pdf", "pdf", "chunk-p", "paperterm"),
    ], []);
    const image = index.exportImage(sqlite);

    const restored = openRestoredFts5Generation(
      sqlite,
      image,
      1,
      undefined,
      undefined,
      undefined,
      ["markdown"],
    );
    try {
      // Configuration, not identity: the rows were built correctly and are
      // still valid, the user simply stopped wanting them. No filesystem probe
      // is involved, because a disabled format cannot be transiently wrong.
      expect(restored.evictions.disabled_format.pdf).toBe(1);
      expect(restored.evictions.stale_identity.pdf).toBe(0);
      expect(restored.sources).toBe(1);
      expect(restored.documents).toBe(1);
      expect(restored.search(anyPlan("paperterm"), 20)).toEqual([]);
      expect(restored.search(anyPlan("alphaterm"), 20)).toHaveLength(1);
      expect(() => restored.assertIntegrity()).not.toThrow();
    } finally {
      restored.close();
    }
  });

  it("reports nothing evicted when every stored format is still current", () => {
    index.applySourceChanges([source("alpha", "chunk-a", "alphaterm")], []);
    const restored = openRestoredFts5Generation(sqlite, index.exportImage(sqlite), 1);
    try {
      // A restore that reused everything must not be describable as a partial
      // reuse; the host keys its status line on exactly these zeros.
      for (const format of SOURCE_FORMATS) {
        expect(restored.evictions.stale_identity[format]).toBe(0);
        expect(restored.evictions.disabled_format[format]).toBe(0);
      }
      expect(restored.sources).toBe(1);
    } finally {
      restored.close();
    }
  });

  it("carries an Excalidraw source through insert, restore, and reconciliation", () => {
    // The schema's `source_format` CHECK omitted 'excalidraw' while the format
    // was enabled by default, so this row could not be inserted at all. The
    // fix rides the same CACHE_SCHEMA_VERSION bump the identity column needs.
    index.replaceSource(formatted("board", "board.excalidraw", "excalidraw", "chunk-b", "boardterm"));
    expect(() => index.assertIntegrity()).not.toThrow();

    const restored = openRestoredFts5Generation(sqlite, index.exportImage(sqlite), 1);
    try {
      expect(restored.sourceFormatCounts.excalidraw["indexed-complete"]).toBe(1);
      expect(restored.search(anyPlan("boardterm"), 20)).toHaveLength(1);
      expect(() => restored.assertIntegrity()).not.toThrow();
    } finally {
      restored.close();
    }
  });

  it("refuses the whole image when a surviving row carries a foreign identity", () => {
    index.replaceSource(source("alpha", "chunk-a", "alphaterm"));
    // Forge an identity that is not any compiled format's, so the projection's
    // own DELETE is bypassed only if the predicate is written wrongly. The
    // structural reconciliation check is the belt to the projection's braces.
    const image = mutateExportedImage(index.exportImage(sqlite), (db) => {
      db.exec("UPDATE sources SET format_identity = ?", { bind: ["c".repeat(64)] });
    });
    const restored = openRestoredFts5Generation(sqlite, image, 1);
    try {
      expect(restored.evictions.stale_identity.markdown).toBe(1);
      expect(restored.sources).toBe(0);
      expect(() => restored.assertIntegrity()).not.toThrow();
    } finally {
      restored.close();
    }
  });

  it("restores a plain-block image with hydrated counters and VFS-backed export", () => {
    index.applySourceChanges([
      source("alpha", "chunk-a", "oldterm"),
      source("keep", "chunk-k", "keepterm"),
    ], []);
    index.assertIntegrity();
    const image = index.exportImage(sqlite);

    const restored = openRestoredFts5Generation(sqlite, image, 1);
    try {
      expect(restored.documents).toBe(2);
      expect(restored.chunks).toBe(2);
      expect(restored.sources).toBe(2);
      expect(restored.sourceFormatCounts.markdown["indexed-complete"]).toBe(2);
      expect(restored.sourceFormatCounts.base["indexed-complete"]).toBe(0);
      expect(restored.search(anyPlan("oldterm"), 20)).toHaveLength(1);
      expect(() => restored.assertIntegrity()).not.toThrow();
      const reexported = restored.exportImage(sqlite);
      expect(reexported).toBeInstanceOf(Uint8Array);
      expect(reexported.byteLength).toBe(image.byteLength);
    } finally {
      restored.close();
    }
  });

  it("restores Rust-normalized Unicode whitespace exact fields without drift", () => {
    const prepared = sourceAt(
      "unicode-restore",
      "unicode-restore.md",
      "chunk-unicode-restore",
      "ordinary body",
      "RÉSUMÉ\u0085\u3000Cache",
    );
    prepared.normalized_exact.title = "r\u00e9sum\u00e9 cache";
    prepared.chunks[0]!.heading_text = "API\u202f\u2003Surface";
    prepared.chunks[0]!.normalized_heading = "api surface";
    index.replaceSource(prepared);

    const restored = openRestoredFts5Generation(sqlite, index.exportImage(sqlite), 1);
    try {
      const hits = restored.search({
        schema_version: 3,
        profile_id: "lexical-v1",
        disposition: "ready",
        max_total_candidates: 512,
        stages: [{
          ordinal: 0,
          plan_id: "lexical_exact_metadata_v3",
          exact_value: "résumé cache",
          max_candidates: 256,
        }],
      }, 20);
      expect(hits.map((hit) => hit.chunk_id)).toEqual(["chunk-unicode-restore"]);
    } finally {
      restored.close();
    }
  });

  it("restores only technical identifiers derivable from canonical chunk content", () => {
    const prepared = source(
      "identifier-restore",
      "chunk-identifier-restore",
      "CVE-2026-1234 RFC9110 product/v2.4.1 CVE-2026-1234 ordinary words",
    );
    prepared.chunks[0]!.technical_identifiers = [
      "cve-2026-1234",
      "rfc9110",
      "product/v2.4.1",
    ];
    index.replaceSource(prepared);

    const restored = openRestoredFts5Generation(sqlite, index.exportImage(sqlite), 1);
    try {
      const hits = restored.search({
        schema_version: 3,
        profile_id: "lexical-v1",
        disposition: "ready",
        max_total_candidates: 512,
        stages: [{
          ordinal: 0,
          plan_id: "lexical_exact_metadata_v3",
          exact_value: "cve-2026-1234",
          max_candidates: 256,
        }],
      }, 20);
      expect(hits.map((hit) => hit.chunk_id)).toEqual(["chunk-identifier-restore"]);
    } finally {
      restored.close();
    }
  });

  it("completes restore, mutate, VFS export, and second restore", () => {
    index.applySourceChanges([
      source("old", "chunk-old", "oldterm"),
      source("keep", "chunk-keep", "keepterm"),
    ], []);
    const first = openRestoredFts5Generation(sqlite, index.exportImage(sqlite), 1);
    let second: Fts5GenerationIndex | null = null;
    try {
      first.applySourceChanges(
        [source("new", "chunk-new", "newterm")],
        [{ vault_id: "active", path: "old.md" }],
        true,
      );
      expect(first.search(anyPlan("oldterm"), 20)).toEqual([]);
      expect(first.search(anyPlan("newterm"), 20)).toHaveLength(1);
      expect(first.search(anyPlan("keepterm"), 20)).toHaveLength(1);

      second = openRestoredFts5Generation(sqlite, first.exportImage(sqlite), 1);
      expect(second.search(anyPlan("oldterm"), 20)).toEqual([]);
      expect(second.search(anyPlan("newterm"), 20)).toHaveLength(1);
      expect(second.search(anyPlan("keepterm"), 20)).toHaveLength(1);
      expect(second.documents).toBe(first.documents);
      expect(second.chunks).toBe(first.chunks);
    } finally {
      second?.close();
      first.close();
    }
  });

  it("refuses internal schema version drift after the VFS opens", () => {
    index.replaceSource(source("alpha", "chunk-a", "quasar"));
    const image = mutateExportedImage(index.exportImage(sqlite), (db) => {
      db.exec(`PRAGMA user_version = ${CACHE_SCHEMA_VERSION + 1}`);
    });
    expect(() => openRestoredFts5Generation(sqlite, image, 1))
      .toThrow(CacheVersionMismatchError);
  });

  it("rejects the schema-v8 cache inside the disposable migration boundary", () => {
    index.replaceSource(source("alpha", "chunk-a", "quasar"));
    const oldImage = mutateExportedImage(index.exportImage(sqlite), (db) => {
      db.exec("PRAGMA user_version = 8");
    });
    expect(() => openRestoredFts5Generation(sqlite, oldImage, 1))
      .toThrow(CacheVersionMismatchError);
  });

  it("refuses an exact-schema mismatch before integrity is trusted", () => {
    index.replaceSource(source("alpha", "chunk-a", "quasar"));
    const image = mutateExportedImage(index.exportImage(sqlite), (db) => {
      db.exec("CREATE TABLE unexpected_cache_object(value TEXT)");
    });
    expect(() => openRestoredFts5Generation(sqlite, image, 1))
      .toThrow(CacheImageInvalidError);
  });

  it.each([
    ["source property", (db: SQLiteDatabase) => {
      db.exec("PRAGMA ignore_check_constraints = ON");
      db.exec("UPDATE source_properties SET rowid = -1 WHERE rowid = 1");
      db.exec("DELETE FROM source_property_text_fts WHERE rowid = 1");
      db.exec("INSERT INTO source_property_text_fts(rowid, string_value) VALUES(-1, 'negative')");
    }],
    ["property scalar", (db: SQLiteDatabase) => {
      db.exec("PRAGMA ignore_check_constraints = ON");
      db.exec("UPDATE source_property_scalars SET rowid = -1 WHERE rowid = 1");
    }],
    ["property FTS", (db: SQLiteDatabase) => {
      db.exec("DELETE FROM source_property_text_fts WHERE rowid = 1");
      db.exec("INSERT INTO source_property_text_fts(rowid, string_value) VALUES(-1, 'negative')");
    }],
  ])("rejects a digest-valid negative %s rowid before restore pagination", (_name, mutate) => {
    index.replaceSource(source("alpha", "chunk-a", "quasar"));
    const corrupt = mutateExportedImage(index.exportImage(sqlite), mutate);
    expect(() => openRestoredFts5Generation(sqlite, corrupt, 1))
      .toThrow(CacheImageInvalidError);
  });

  it("rebuilds forged property FTS text from canonical source properties", () => {
    index.replaceSource(source("alpha", "chunk-a", "quasar", "Canonical Title"));
    const forged = mutateExportedImage(index.exportImage(sqlite), (db) => {
      const rowid = Number(db.selectValue(
        "SELECT rowid FROM source_properties WHERE property_name = 'title'",
      ));
      db.exec("DELETE FROM source_property_text_fts WHERE rowid = ?", { bind: [rowid] });
      db.exec(
        "INSERT INTO source_property_text_fts(rowid, string_value) VALUES(?, 'forgedterm')",
        { bind: [rowid] },
      );
    });
    const precondition = deserialize(sqlite, forged);
    try {
      expect(precondition.selectValue(
        "SELECT count(*) FROM source_property_text_fts WHERE source_property_text_fts MATCH ?",
        ['string_value : "forgedterm"'],
      )).toBe(1);
      expect(precondition.selectValue(
        "SELECT count(*) FROM source_property_text_fts WHERE source_property_text_fts MATCH ?",
        ['string_value : "canonical"'],
      )).toBe(0);
    } finally {
      precondition.close();
    }

    const restored = openRestoredFts5Generation(sqlite, forged, 1);
    try {
      const repaired = deserialize(sqlite, restored.exportImage(sqlite));
      try {
        expect(repaired.selectValue(
          "SELECT count(*) FROM source_property_text_fts WHERE source_property_text_fts MATCH ?",
          ['string_value : "canonical"'],
        )).toBe(1);
        expect(repaired.selectValue(
          "SELECT count(*) FROM source_property_text_fts WHERE source_property_text_fts MATCH ?",
          ['string_value : "forgedterm"'],
        )).toBe(0);
      } finally {
        repaired.close();
      }
    } finally {
      restored.close();
    }
  });

  it("rejects same-rowid forged chunk postings against canonical chunk rows", () => {
    index.replaceSource(source("alpha", "chunk-a", "canonicalterm", "Canonical Title"));
    const forged = mutateExportedImage(index.exportImage(sqlite), (db) => {
      const rowid = Number(db.selectValue("SELECT rowid FROM chunks WHERE chunk_id = 'chunk-a'"));
      db.exec("DELETE FROM chunks_fts WHERE rowid = ?", { bind: [rowid] });
      db.exec(
        "INSERT INTO chunks_fts(rowid, content) VALUES(?, 'forgedterm')",
        { bind: [rowid] },
      );
    });

    expect(() => openRestoredFts5Generation(sqlite, forged, 1))
      .toThrow(CacheImageInvalidError);
  });

  it("rejects compact title metadata that disagrees with canonical source properties", () => {
    index.replaceSource(source("alpha", "chunk-a", "canonicalterm", "Canonical Title"));
    const forged = mutateExportedImage(index.exportImage(sqlite), (db) => {
      db.exec("UPDATE chunks SET frontmatter_json = '{\"title\":\"Forged Title\"}'");
    });

    expect(() => openRestoredFts5Generation(sqlite, forged, 1))
      .toThrow(CacheImageInvalidError);
  });

  it("refuses invalid source rows and per-source tally transfers", () => {
    index.applySourceChanges([
      source("alpha", "chunk-a", "quasar"),
      source("beta", "chunk-b", "pulsar"),
    ], []);
    const base = index.exportImage(sqlite);
    const invalidRow = mutateExportedImage(base, (db) => {
      db.exec("UPDATE sources SET content_hash = '' WHERE source_key = 'alpha'");
    });
    expect(() => openRestoredFts5Generation(sqlite, invalidRow, 1))
      .toThrow(CacheImageInvalidError);

    const transferredTallies = mutateExportedImage(base, (db) => {
      db.exec("UPDATE sources SET chunk_count = chunk_count + 1 WHERE source_key = 'alpha'");
      db.exec("UPDATE sources SET chunk_count = chunk_count - 1 WHERE source_key = 'beta'");
    });
    expect(() => openRestoredFts5Generation(sqlite, transferredTallies, 1))
      .toThrow(CacheImageInvalidError);
  });

  it("restores Rust-authored exact values without inventing a TypeScript normalization policy", () => {
    index.replaceSource(source("alpha", "chunk-a", "quasar"));
    const authored = mutateExportedImage(index.exportImage(sqlite), (db) => {
      db.exec("UPDATE sources SET exact_title = 'rust-authored-title'");
      db.exec("UPDATE chunks SET exact_heading = 'rust-authored-heading'");
    });

    const restored = openRestoredFts5Generation(sqlite, authored, 1);
    try {
      expect(restored.search({
        schema_version: 3,
        profile_id: "lexical-v1",
        disposition: "ready",
        max_total_candidates: 512,
        stages: [{
          ordinal: 0,
          plan_id: "lexical_exact_metadata_v3",
          exact_value: "rust-authored-title",
          max_candidates: 256,
        }],
      }, 10).map((hit) => hit.chunk_id)).toEqual(["chunk-a"]);
    } finally {
      restored.close();
    }
  });

  it("restores exact projections independent of SQLite UTF-8 versus JavaScript UTF-16 order", () => {
    const preparation = source("unicode", "chunk-unicode", "unicode body");
    const values = ["é", "😀", ""];
    preparation.retrieval.aliases = values;
    preparation.normalized_exact.aliases = values;
    preparation.frontmatter.aliases = {
      type: "sequence",
      value: values.map((value) => ({ type: "string", value })),
    };
    preparation.chunks[0]!.chunk.frontmatter.aliases = values;
    preparation.chunks[0]!.technical_identifiers = values;
    index.replaceSource(preparation);

    const restored = openRestoredFts5Generation(sqlite, index.exportImage(sqlite), 1);
    try {
      expect(restored.documents).toBe(1);
      expect(restored.chunks).toBe(1);
    } finally {
      restored.close();
    }
  });

  it.each([
    ["malformed heading JSON", (db: SQLiteDatabase) => {
      db.exec("UPDATE chunks SET heading_path_json = 'not-json'");
    }],
    ["property JSON disagreeing with its typed projection", (db: SQLiteDatabase) => {
      db.exec("UPDATE source_properties SET value_json = '42' WHERE property_name = 'title'");
    }],
    ["chunk/source identity disagreement", (db: SQLiteDatabase) => {
      db.exec("UPDATE chunks SET vault_id = 'another-vault'");
    }],
    ["malformed normalized source alias", (db: SQLiteDatabase) => {
      db.exec("UPDATE sources SET exact_aliases_json = '[\"\"]'");
    }],
    ["oversized normalized heading", (db: SQLiteDatabase) => {
      db.exec(`UPDATE chunks SET exact_heading = '${"x".repeat(4_097)}'`);
      db.exec("INSERT INTO chunks_fts(chunks_fts) VALUES('rebuild')");
    }],
    ["semantically forged technical identifier", (db: SQLiteDatabase) => {
      db.exec("UPDATE chunks SET identifiers_json = '[\"cve-2026-1234\"]'");
      db.exec("INSERT INTO chunks_fts(chunks_fts) VALUES('rebuild')");
    }],
  ])("refuses digest-valid %s even when SQLite and FTS integrity stay green", (_name, mutate) => {
    index.replaceSource(source("alpha", "chunk-a", "quasar"));
    const corrupt = mutateExportedImage(index.exportImage(sqlite), mutate);

    const precondition = deserialize(sqlite, corrupt);
    try {
      expect(precondition.selectValue("PRAGMA integrity_check")).toBe("ok");
      expect(() => precondition.exec(
        "INSERT INTO chunks_fts(chunks_fts, rank) VALUES('integrity-check', 1)",
      )).not.toThrow();
    } finally {
      precondition.close();
    }

    expect(() => openRestoredFts5Generation(sqlite, corrupt, 1))
      .toThrow(CacheImageInvalidError);
  });

  it("refuses interior B-tree corruption after header and schema validation pass", () => {
    index.replaceSource(source("alpha", "chunk-a", "quasar"));
    const good = index.exportImage(sqlite);
    const inspector = deserialize(sqlite, good);
    let rootPage: number;
    try {
      rootPage = Number(inspector.selectValue(
        "SELECT rootpage FROM sqlite_schema WHERE name = 'sources'",
      ));
    } finally {
      inspector.close();
    }
    const corrupt = good.slice();
    const encodedPageSize = new DataView(corrupt.buffer).getUint16(16);
    const pageSize = encodedPageSize === 1 ? 65_536 : encodedPageSize;
    corrupt[(rootPage - 1) * pageSize] = 0;

    const precondition = deserialize(sqlite, corrupt);
    try {
      expect(precondition.selectValue("SELECT count(*) FROM sqlite_schema"))
        .toBeGreaterThan(0);
      let integrity: unknown;
      try {
        integrity = precondition.selectValue("PRAGMA integrity_check");
      } catch {
        integrity = "threw";
      }
      expect(integrity).not.toBe("ok");
    } finally {
      precondition.close();
    }
    expect(() => openRestoredFts5Generation(sqlite, corrupt, 1))
      .toThrow(CacheImageInvalidError);
  });

  it("refuses FTS shadow corruption during full SQLite integrity validation", () => {
    index.replaceSource(source("alpha", "chunk-a", "quasar"));
    const corrupt = mutateExportedImage(index.exportImage(sqlite), (db) => {
      db.exec("DELETE FROM chunks_fts_data WHERE id > 10");
    });
    const precondition = deserialize(sqlite, corrupt);
    try {
      expect(precondition.selectValue("PRAGMA integrity_check")).not.toBe("ok");
    } finally {
      precondition.close();
    }
    expect(() => openRestoredFts5Generation(sqlite, corrupt, 1))
      .toThrow(CacheImageInvalidError);
  });

  it("refuses contentless FTS and metadata divergence", () => {
    index.replaceSource(source("alpha", "chunk-a", "quasar"));
    const image = mutateExportedImage(index.exportImage(sqlite), (db) => {
      db.exec("INSERT INTO chunks_fts(rowid, content) VALUES(900000, 'orphanterm')");
    });
    expect(() => openRestoredFts5Generation(sqlite, image, 1))
      .toThrow(CacheImageInvalidError);
  });
});

function mutateExportedImage(
  image: Uint8Array,
  mutate: (db: SQLiteDatabase) => void,
): Uint8Array {
  const db = deserialize(sqlite, image);
  try {
    mutate(db);
    return sqlite.capi.sqlite3_js_db_export(db.pointer);
  } finally {
    db.close();
  }
}

/** Delegating wrapper that records the statements a generation issues. */
class RecordingDatabase implements SQLiteDatabase {
  readonly statements: string[] = [];
  readonly selectedStatements: string[] = [];
  openTransactions = 0;

  constructor(private readonly inner: SQLiteDatabase) {}

  get filename(): string {
    return this.inner.filename;
  }

  get pointer(): unknown | undefined {
    return this.inner.pointer;
  }

  exec(sql: string, options?: { bind?: readonly unknown[] }): unknown {
    this.statements.push(sql);
    // oo1 rejects an explicit `undefined` options argument.
    return options === undefined ? this.inner.exec(sql) : this.inner.exec(sql, options);
  }

  selectValue(sql: string, bind?: readonly unknown[]): unknown {
    this.selectedStatements.push(sql);
    return this.inner.selectValue(sql, bind);
  }

  selectObjects(sql: string, bind?: readonly unknown[]): Record<string, unknown>[] {
    this.selectedStatements.push(sql);
    return this.inner.selectObjects(sql, bind);
  }

  transaction<T>(qualifier: "IMMEDIATE", callback: () => T): T {
    this.openTransactions += 1;
    try {
      return this.inner.transaction(qualifier, callback);
    } finally {
      this.openTransactions -= 1;
    }
  }

  close(): void {
    this.inner.close();
  }
}
