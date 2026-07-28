// SPDX-FileCopyrightText: 2026 cybersader
// SPDX-License-Identifier: GPL-3.0-only

import { describe, expect, it } from "vitest";

import { sourcePreparationDefect } from "../src/worker/source-defect";
import { classifyFailure } from "../src/diagnostics/classify-failure";

const VALID = {
  schema_version: 1,
  source_key: "a".repeat(64),
  vault_id: "active-vault",
  path: "Notes/Example.md",
  format: "markdown",
  content_hash: "b".repeat(64),
  byte_length: 10,
  mtime: 1785253671659,
  mtime_nanos: "1785253671659000000",
  retrieval: { filename: "Example.md", stem: "Example", aliases: [] },
  chunks: [],
  kind: "skipped",
};

describe("sourcePreparationDefect", () => {
  it("accepts a valid preparation", () => {
    expect(sourcePreparationDefect(VALID)).toBeNull();
  });

  it("names the specific field that failed", () => {
    // The whole point: the old predicate collapsed every check into one
    // false, so eight releases narrowed a production failure to "rust
    // refused it" without ever saying which check refused.
    expect(sourcePreparationDefect({ ...VALID, mtime_nanos: "-1000000" })).toBe("mtime_nanos");
    expect(sourcePreparationDefect({ ...VALID, mtime: -1 })).toBe("mtime");
    expect(sourcePreparationDefect({ ...VALID, format: "pdf" })).toBe("format");
    expect(sourcePreparationDefect({ ...VALID, path: "" })).toBe("path");
    expect(sourcePreparationDefect({ ...VALID, byte_length: 1.5 })).toBe("byte_length");
    expect(sourcePreparationDefect({ ...VALID, kind: "indexed", content_hash: null }))
      .toBe("indexed_missing_hash");
  });

  it("never throws on a hostile value", () => {
    expect(() => sourcePreparationDefect(null)).not.toThrow();
    expect(() => sourcePreparationDefect({ get path(): string { throw new Error("x"); } }))
      .not.toThrow();
  });
});

describe("defect field reaches the classification", () => {
  it("extracts an identifier tail from the worker message", () => {
    const result = classifyFailure({
      code: "source_rejected",
      stage: "rust",
      message: "Portable Rust rejected a source batch: mtime_nanos",
    });
    expect(result.defectField).toBe("mtime_nanos");
  });

  it("ignores a tail that could be a path or a sentence", () => {
    const result = classifyFailure({
      code: "source_rejected",
      stage: "rust",
      message: "rejected: Clients/Acme Q3.md",
    });
    expect(result.defectField).toBeUndefined();
    expect(JSON.stringify(result)).not.toContain("Acme");
  });
});

describe("caps reflect the Rust contract, not an invented policy", () => {
  it("accepts a hub note with an unbounded number of wikilinks", () => {
    // Reproduced against the real Rust chunker: a note of 5,000 wikilinks
    // emits links_out=5000 per chunk. The old cap of 4,096 was chosen here,
    // not promised by Rust, so a single hub or MOC note rejected its source --
    // and a rejected source aborts the entire batch, so one note stopped a
    // whole production vault from indexing.
    const chunk = {
      chunk: {
        chunk_id: "c".repeat(64),
        vault_id: "active-vault",
        room: null,
        path: "Maps/Index.md",
        heading_path: [],
        content: "body",
        frontmatter: {},
        links_out: Array.from({ length: 5_000 }, (_, i) => `Note ${i}`),
        mtime: 1785253671659,
        content_hash: "d".repeat(64),
        chunking_version: 1,
      },
      heading_text: "",
      technical_identifiers: [],
    };
    expect(sourcePreparationDefect({ ...VALID, kind: "indexed", chunks: [chunk] })).toBeNull();
  });

  it("still rejects a structurally corrupt chunk", () => {
    // Raising the ceiling must not disable the check: its purpose is catching
    // a corrupt ABI response, which is a different thing from a large note.
    expect(sourcePreparationDefect({ ...VALID, kind: "indexed", chunks: [{ chunk: null }] }))
      .toBe("chunks_contents");
  });
});

describe("no count ceiling is enforced anywhere", () => {
  const chunkWith = (over: Record<string, unknown>) => ({
    chunk: {
      chunk_id: "c".repeat(64),
      vault_id: "active-vault",
      room: null,
      path: "Maps/Index.md",
      heading_path: [],
      content: "body",
      frontmatter: {},
      links_out: [],
      mtime: 1785253671659,
      content_hash: "d".repeat(64),
      chunking_version: 1,
      ...over,
    },
    heading_text: "",
    technical_identifiers: [],
  });

  it("accepts arrays far past every former cap", () => {
    // Pins the principle rather than a number: a count limit in this file is
    // a content policy, and no length is evidence of a corrupt ABI response.
    const huge = (n: number) => Array.from({ length: n }, (_, i) => `x${i}`);
    for (const over of [
      { links_out: huge(50_000) },
      { heading_path: huge(5_000) },
      { frontmatter: { tags: huge(10_000) } },
    ]) {
      expect(sourcePreparationDefect({ ...VALID, kind: "indexed", chunks: [chunkWith(over)] }))
        .toBeNull();
    }
  });

  it("still rejects a non-string element, which does indicate corruption", () => {
    expect(sourcePreparationDefect({
      ...VALID,
      kind: "indexed",
      chunks: [chunkWith({ links_out: ["ok", 42] })],
    })).toBe("chunks_contents");
  });
});
