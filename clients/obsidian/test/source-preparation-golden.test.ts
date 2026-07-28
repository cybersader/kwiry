// SPDX-FileCopyrightText: 2026 cybersader
// SPDX-License-Identifier: GPL-3.0-only

import { readFileSync, readdirSync } from "node:fs";
import { fileURLToPath } from "node:url";

import { describe, expect, it } from "vitest";

import { sourcePreparationDefect } from "../src/worker/source-defect";

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
  it("contains the required adversarial producer matrix", () => {
    expect(fixtures.length).toBeGreaterThanOrEqual(7);
  });

  it.each(fixtures)("accepts real Rust output from $name", ({ preparation }) => {
    // Producer-owned output prevents the validator and its fixtures from
    // sharing the same unexamined assumptions about valid source shapes.
    expect(sourcePreparationDefect(preparation)).toBeNull();
  });
});
