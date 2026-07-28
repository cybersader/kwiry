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
