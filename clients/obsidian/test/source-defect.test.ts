// SPDX-FileCopyrightText: 2026 cybersader
// SPDX-License-Identifier: GPL-3.0-only

import { describe, expect, it } from "vitest";

import { sourcePreparationDefect } from "../src/worker/source-defect";
import { classifyFailure } from "../src/diagnostics/classify-failure";

const VALID = {
  schema_version: 8,
  source_key: "a".repeat(64),
  vault_id: "active-vault",
  path: "Notes/Example.md",
  format: "markdown",
  coverage: "skipped-no-extractable-text",
  content_hash: "b".repeat(64),
  byte_length: 10,
  mtime: 1785253671659,
  mtime_nanos: "1785253671659000000",
  retrieval: { filename: "Example.md", stem: "Example", aliases: [] },
  normalized_exact: {
    filename: "example.md",
    stem: "example",
    aliases: [],
    title: null,
  },
  frontmatter: {},
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
    expect(sourcePreparationDefect({ ...VALID, format: "html" })).toBe("format");
    expect(sourcePreparationDefect({ ...VALID, path: "" })).toBe("path");
    expect(sourcePreparationDefect({ ...VALID, byte_length: 1.5 })).toBe("byte_length");
    expect(sourcePreparationDefect({ ...VALID, kind: "indexed", content_hash: null }))
      .toBe("indexed_missing_hash");
  });

  it("validates normalized exact values by Rust's UTF-8 byte bound", () => {
    const bounded = "🚀".repeat(1_024);
    expect(sourcePreparationDefect({
      ...VALID,
      normalized_exact: { ...VALID.normalized_exact, title: bounded },
    })).toBeNull();
    expect(sourcePreparationDefect({
      ...VALID,
      normalized_exact: { ...VALID.normalized_exact, title: `${bounded}🚀` },
    })).toBe("normalized_exact");
    expect(sourcePreparationDefect({
      ...VALID,
      normalized_exact: { ...VALID.normalized_exact, aliases: [""] },
    })).toBe("normalized_exact");
  });

  it("never throws on a hostile value", () => {
    expect(() => sourcePreparationDefect(null)).not.toThrow();
    expect(() => sourcePreparationDefect({ get path(): string { throw new Error("x"); } }))
      .not.toThrow();
  });
});

describe("chunk/source correlation", () => {
  const indexed = () => ({
    ...VALID,
    room: "reference",
    kind: "indexed",
    coverage: "indexed-complete",
    frontmatter: {},
    chunks: [{
      chunk: {
        chunk_id: "c".repeat(64),
        vault_id: VALID.vault_id,
        room: "reference",
        path: VALID.path,
        heading_path: [],
        content: "body",
        frontmatter: {},
        links_out: [],
        mtime: VALID.mtime,
        content_hash: VALID.content_hash,
        chunking_version: 1,
      },
      heading_text: "",
      normalized_heading: null,
      technical_identifiers: [],
    }],
  });

  it.each([
    ["vault_id", "different-vault"],
    ["room", "different-room"],
    ["path", "Different.md"],
    ["mtime", VALID.mtime + 1],
    ["content_hash", "e".repeat(64)],
  ])("names a mismatched chunk %s at the source quarantine boundary", (field, value) => {
    const preparation = indexed();
    (preparation.chunks[0]!.chunk as Record<string, unknown>)[field] = value;
    expect(sourcePreparationDefect(preparation)).toBe("chunks_source_correlation");
  });

  it("accepts the normalized absent-room correlation", () => {
    const preparation = indexed();
    delete (preparation as { room?: string }).room;
    (preparation.chunks[0]!.chunk as Record<string, unknown>).room = null;
    expect(sourcePreparationDefect(preparation)).toBeNull();
  });

  it("accepts Base view locators and rejects locators on other formats", () => {
    const preparation = indexed();
    preparation.format = "base";
    (preparation.chunks[0] as Record<string, unknown>).source_locator = {
      kind: "base_view",
      view: "Active",
    };
    expect(sourcePreparationDefect(preparation)).toBeNull();

    preparation.format = "markdown";
    expect(sourcePreparationDefect(preparation)).toBe("chunks_source_locator");
    preparation.format = "canvas";
    expect(sourcePreparationDefect(preparation)).toBe("chunks_source_locator");
  });
});

describe("open property bag validation", () => {
  const prepareValue = (value: unknown): Record<string, unknown> => {
    if (value === null) return { type: "null" };
    if (typeof value === "boolean") return { type: "boolean", value };
    if (typeof value === "string") return { type: "string", value };
    if (typeof value === "number") {
      if (!Number.isFinite(value)) return { type: "f64", value: "invalid" };
      if (Number.isInteger(value)) return { type: "i64", value: String(value) };
      const bytes = new ArrayBuffer(8);
      const view = new DataView(bytes);
      view.setFloat64(0, value, false);
      return {
        type: "f64",
        value: view.getBigUint64(0, false).toString(16).padStart(16, "0"),
      };
    }
    if (Array.isArray(value)) return { type: "sequence", value: value.map(prepareValue) };
    if (typeof value !== "object") return { type: "broken" };
    return {
      type: "map",
      value: Object.fromEntries(
        Object.entries(value as Record<string, unknown>)
          .map(([name, child]) => [name, prepareValue(child)]),
      ),
    };
  };
  const prepareBag = (frontmatter: Record<string, unknown>) => Object.fromEntries(
    Object.entries(frontmatter).map(([name, value]) => [name, prepareValue(value)]),
  );
  const preparationWith = (frontmatter: Record<string, unknown>) => ({
    ...VALID,
    path: "Properties/Open.md",
    content_hash: VALID.content_hash,
    kind: "indexed",
    coverage: "indexed-complete",
    frontmatter: prepareBag(frontmatter),
    chunks: [{
      chunk: {
        chunk_id: "c".repeat(64),
        vault_id: "active-vault",
        room: null,
        path: "Properties/Open.md",
        heading_path: [],
        content: "body",
        frontmatter,
        links_out: [],
        mtime: 1785253671659,
        content_hash: VALID.content_hash,
        chunking_version: 1,
      },
      heading_text: "",
      normalized_heading: null,
      technical_identifiers: [],
    }],
  });

  it("accepts one thousand properties without a cardinality policy", () => {
    const properties = Object.fromEntries(
      Array.from({ length: 1_000 }, (_, index) => [`property_${index}`, `value_${index}`]),
    );
    expect(sourcePreparationDefect(preparationWith(properties))).toBeNull();
  });

  it("accepts deeply nested maps and names the corruption-only depth boundary", () => {
    let nested: unknown = "leaf";
    for (let depth = 0; depth < 64; depth += 1) nested = { nested };
    expect(sourcePreparationDefect(preparationWith({ nested }))).toBeNull();

    let tooDeep: unknown = "leaf";
    for (let depth = 0; depth < 128; depth += 1) tooDeep = { nested: tooDeep };
    expect(sourcePreparationDefect(preparationWith({ nested: tooDeep })))
      .toBe("frontmatter_property_nesting");
  });

  it("accepts a twelve-hundred-element array as one property value", () => {
    const items = Array.from({ length: 1_200 }, (_, index) => index);
    expect(sourcePreparationDefect(preparationWith({ items }))).toBeNull();
  });

  it("accepts mixed types for the same key across notes", () => {
    for (const signal of [null, true, 7, 3.5, "7", [7, "7"], { nested: false }]) {
      expect(sourcePreparationDefect(preparationWith({ signal }))).toBeNull();
    }
  });

  it("accepts a property value measured in megabytes without truncation", () => {
    const payload = "x".repeat(2 * 1024 * 1024);
    const preparation = preparationWith({ payload });
    expect(sourcePreparationDefect(preparation)).toBeNull();
    expect((preparation.chunks[0]!.chunk.frontmatter.payload as string).length)
      .toBe(payload.length);
  });

  it("rejects impossible JSON values and cycles with fixed privacy-safe fields", () => {
    expect(sourcePreparationDefect(preparationWith({ broken: undefined })))
      .toBe("frontmatter_property_value");
    expect(sourcePreparationDefect(preparationWith({ broken: Number.POSITIVE_INFINITY })))
      .toBe("frontmatter_property_value");

    const cycle: Record<string, unknown> = { type: "map", value: {} };
    (cycle.value as Record<string, unknown>).self = cycle;
    const cyclicPreparation = { ...preparationWith({}), frontmatter: { cycle } };
    expect(() => sourcePreparationDefect(cyclicPreparation)).not.toThrow();
    expect(sourcePreparationDefect(cyclicPreparation)).toBe("frontmatter_property_cycle");
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
    expect(classifyFailure({
      code: "source_rejected",
      stage: "rust",
      message: "Portable Rust rejected a source batch: frontmatter_property_value",
    }).defectField).toBe("frontmatter_property_value");
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
        path: VALID.path,
        heading_path: [],
        content: "body",
        frontmatter: {},
        links_out: Array.from({ length: 5_000 }, (_, i) => `Note ${i}`),
        mtime: 1785253671659,
        content_hash: VALID.content_hash,
        chunking_version: 1,
      },
      heading_text: "",
      normalized_heading: null,
      technical_identifiers: [],
    };
    expect(sourcePreparationDefect({
      ...VALID,
      kind: "indexed",
      coverage: "indexed-complete",
      chunks: [chunk],
    })).toBeNull();
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
      path: VALID.path,
      heading_path: [],
      content: "body",
      frontmatter: {},
      links_out: [],
      mtime: 1785253671659,
      content_hash: VALID.content_hash,
      chunking_version: 1,
      ...over,
    },
    heading_text: "",
    normalized_heading: null,
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
      expect(sourcePreparationDefect({
        ...VALID,
        kind: "indexed",
        coverage: "indexed-complete",
        chunks: [chunkWith(over)],
      })).toBeNull();
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
