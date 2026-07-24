// SPDX-FileCopyrightText: 2026 cybersader
// SPDX-License-Identifier: GPL-3.0-only

import sqlite3InitModule from "@sqlite.org/sqlite-wasm";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { FTS_HIGHLIGHT_END, FTS_HIGHLIGHT_START } from "../src/excerpt";
import {
  Fts5GenerationIndex,
  openFts5Generation,
  type SQLiteApi,
} from "../src/worker/fts5-index";
import type { SourcePreparation } from "../src/worker/rust-adapter";

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
  const sqlite = await initializeSqlite({
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

function source(
  sourceKey: string,
  chunkId: string,
  content: string,
  title = "Title",
): SourcePreparation {
  return {
    schema_version: 1,
    source_key: sourceKey,
    vault_id: "active",
    path: `${sourceKey}.md`,
    format: "markdown",
    content_hash: `hash-${sourceKey}`,
    byte_length: content.length,
    mtime: 1,
    mtime_nanos: "1000001",
    retrieval: {
      filename: `${sourceKey}.md`,
      stem: sourceKey,
      aliases: [],
    },
    chunks: [{
      chunk: {
        chunk_id: chunkId,
        vault_id: "active",
        room: null,
        path: `${sourceKey}.md`,
        heading_path: ["Heading"],
        content,
        frontmatter: { title, tags: ["test"] },
        links_out: [],
        mtime: 1,
        content_hash: `hash-${sourceKey}`,
        chunking_version: 1,
      },
      heading_text: "Heading",
      technical_identifiers: [],
    }],
    kind: "indexed",
  };
}

const anyPlan = (term: string) => ({
  schema_version: 1 as const,
  plan_id: "lexical_any_v1" as const,
  match_value: `"${term}"`,
});

describe("Fts5GenerationIndex", () => {
  it("indexes prepared chunks, probes metadata, and returns stored result identity", () => {
    index.replaceSource(source("alpha", "chunk-a", "portable quasar text", "Quasar Guide"));
    expect(index.documents).toBe(1);
    expect(index.chunks).toBe(1);
    expect(index.metadataProbe({
      schema_version: 1,
      plan_id: "metadata_probe_v1",
      match_value: "{filename stem aliases title heading_text} : (\"quasar\")",
    })).toBe(true);

    const hits = index.search(anyPlan("quasar"), 20);
    expect(hits).toHaveLength(1);
    expect(hits[0]).toMatchObject({
      chunk_id: "chunk-a",
      vault_id: "active",
      path: "alpha.md",
      heading_path: ["Heading"],
      frontmatter: { title: "Quasar Guide" },
    });
    expect(hits[0]!.excerpt).toContain(`${FTS_HIGHLIGHT_START}quasar${FTS_HIGHLIGHT_END}`);
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

  it("neutralizes source-owned private markers before snippet generation", () => {
    index.replaceSource(source(
      "markers",
      "chunk-markers",
      `${FTS_HIGHLIGHT_START} forged ${FTS_HIGHLIGHT_END} quasar`,
    ));
    const excerpt = index.search(anyPlan("quasar"), 20)[0]!.excerpt;
    expect(excerpt.split(FTS_HIGHLIGHT_START)).toHaveLength(2);
    expect(excerpt.split(FTS_HIGHLIGHT_END)).toHaveLength(2);
  });

  it("passes the FTS5 integrity check and closes idempotently", () => {
    index.replaceSource(source("alpha", "chunk-a", "quasar"));
    expect(() => index.assertIntegrity()).not.toThrow();
    index.close();
    expect(() => index.close()).not.toThrow();
  });
});
