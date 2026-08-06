// SPDX-FileCopyrightText: 2026 cybersader
// SPDX-License-Identifier: GPL-3.0-only

import { readFileSync, readdirSync } from "node:fs";
import { fileURLToPath } from "node:url";

import { describe, expect, it } from "vitest";

import { sourcePreparationDefect } from "../src/worker/source-defect";

const expectedFixtureNames = [
  "01-thousands-of-wikilinks.json",
  "02-deep-heading-nesting.json",
  "03-very-large-single-section.json",
  "04-large-frontmatter-tags.json",
  "05-empty-note.json",
  "06-frontmatter-only.json",
  "07-unusual-valid-utf8.json",
  "08-thousand-open-properties.json",
  "09-deep-property-map.json",
  "10-large-property-array.json",
  "11-shared-key-integer.json",
  "12-shared-key-string.json",
  "13-shared-key-boolean.json",
  "14-property-key-and-scalar-edges.json",
  "15-base-project-dashboard.json",
  "16-canvas-research-board.json",
] as const;

const fixtureDirectory = fileURLToPath(
  new URL("./fixtures/source-preparations/", import.meta.url),
);
const fixtureNames = readdirSync(fixtureDirectory)
  .filter((name) => name.endsWith(".json"))
  .sort();
const fixtures = fixtureNames.map((name) => {
  const bytes = readFileSync(`${fixtureDirectory}/${name}`, "utf8");
  return {
    name,
    bytes,
    preparation: JSON.parse(bytes) as unknown,
  };
});

describe("Rust SourcePreparation golden fixtures", () => {
  it("contains the complete adversarial producer matrix", () => {
    expect(fixtureNames).toEqual(expectedFixtureNames);
  });

  it.each(fixtures)("accepts byte-canonical real Rust output from $name", ({ bytes, preparation }) => {
    // Producer-owned output prevents the validator and its fixtures from
    // sharing the same unexamined assumptions about valid source shapes. Re-encoding must be
    // byte-exact, so whitespace or ordering drift in the Rust ABI is visible to Vitest.
    expect(sourcePreparationDefect(preparation)).toBeNull();
    expect(`${JSON.stringify(preparation, null, 2)}\n`).toBe(bytes);
  });

  it("preserves Canvas node-then-edge order, Markdown headings, and typed authored JSON", () => {
    const canvas = fixtures.find(({ name }) => name === "16-canvas-research-board.json")
      ?.preparation as {
        schema_version?: unknown;
        format?: unknown;
        coverage?: unknown;
        normalized_exact?: { title?: unknown };
        frontmatter?: Record<string, unknown>;
        chunks?: Array<{
          chunk?: { heading_path?: unknown; content?: unknown; frontmatter?: unknown };
          source_locator?: unknown;
        }>;
      } | undefined;

    expect(canvas).toMatchObject({
      schema_version: 8,
      format: "canvas",
      coverage: "indexed-complete",
      normalized_exact: { title: null },
    });
    expect(canvas?.chunks?.map(({ chunk }) => ({
      heading_path: chunk?.heading_path,
      content: chunk?.content,
    }))).toEqual([
      { heading_path: [], content: "---\ntitle: Card-only title\ntags: [nested, card]\n---\nAuthored card preamble." },
      { heading_path: ["Alpha"], content: "# Alpha\nAlpha body." },
      { heading_path: ["Alpha", "Detail"], content: "## Detail\nDetail body." },
      { heading_path: [], content: "Research Cluster" },
      { heading_path: [], content: "https://example.com/canvas-source" },
      { heading_path: [], content: "References/target.md\n#Only Authored Subpath" },
      { heading_path: ["Closing"], content: "## Closing\nFinal card body." },
      { heading_path: [], content: "supports source" },
      { heading_path: [], content: "resolves into" },
    ]);
    expect(canvas?.chunks?.every((chunk) => chunk.source_locator === undefined)).toBe(true);
    expect(canvas?.chunks?.every((chunk) =>
      JSON.stringify(chunk.chunk?.frontmatter) === "{}")).toBe(true);
    expect(canvas?.frontmatter).toHaveProperty("canvas");
    expect(canvas?.frontmatter).not.toHaveProperty("title");
    expect(canvas?.frontmatter).not.toHaveProperty("tags");
    const typedCanvas = JSON.stringify(canvas?.frontmatter?.canvas);
    expect(typedCanvas).toContain('"id":{"type":"string","value":"1111111111111111"}');
    expect(typedCanvas).toContain('"id":{"type":"string","value":"aaaaaaaaaaaaaaaa"}');
    expect(typedCanvas).toContain('"max_items":{"type":"u64","value":"18446744073709551615"}');
  });

  it("preserves Base format, view order, and authored locator in the Rust golden", () => {
    const base = fixtures.find(({ name }) => name === "15-base-project-dashboard.json")
      ?.preparation as {
        format?: unknown;
        coverage?: unknown;
        chunks?: Array<{ chunk?: { heading_path?: unknown }; source_locator?: unknown }>;
      } | undefined;

    expect(base).toMatchObject({
      format: "base",
      coverage: "indexed-complete",
      chunks: [
        { chunk: { heading_path: [] } },
        { chunk: { heading_path: ["Active"] }, source_locator: { kind: "base_view", view: "Active" } },
        { chunk: { heading_path: ["Gallery"] }, source_locator: { kind: "base_view", view: "Gallery" } },
        { chunk: { heading_path: ["Active (2)"] }, source_locator: { kind: "base_view", view: "Active" } },
      ],
    });
  });
});
