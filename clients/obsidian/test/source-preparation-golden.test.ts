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
] as const;

const fixtureDirectory = fileURLToPath(
  new URL("./fixtures/source-preparations/", import.meta.url),
);
const fixtureNames = readdirSync(fixtureDirectory)
  .filter((name) => name.endsWith(".json"))
  .sort();
const fixtures = fixtureNames.map((name) => ({
  name,
  preparation: JSON.parse(readFileSync(`${fixtureDirectory}/${name}`, "utf8")) as unknown,
}));

describe("Rust SourcePreparation golden fixtures", () => {
  it("contains the complete adversarial producer matrix", () => {
    expect(fixtureNames).toEqual(expectedFixtureNames);
  });

  it.each(fixtures)("accepts real Rust output from $name", ({ preparation }) => {
    // Producer-owned output prevents the validator and its fixtures from
    // sharing the same unexamined assumptions about valid source shapes.
    expect(sourcePreparationDefect(preparation)).toBeNull();
  });
});
