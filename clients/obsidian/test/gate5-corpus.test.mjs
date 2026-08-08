// SPDX-FileCopyrightText: 2026 cybersader
// SPDX-License-Identifier: GPL-3.0-only

import { mkdtemp, readFile, readdir, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { afterEach, describe, expect, it } from "vitest";

import {
  CORPUS_HASH_ALGORITHM,
  PERFORMANCE_CORPUS_BYTES,
  PERFORMANCE_NOTE_COUNT,
  PERFORMANCE_SEED,
  PERFORMANCE_VOCABULARY_SIZE,
  functionalOracles,
  generateFunctionalCorpus,
  generatePerformanceCorpus,
  performanceNoteBytes,
} from "../scripts/gate5-corpus.mjs";

const temporary = [];

afterEach(async () => {
  await Promise.all(temporary.splice(0).map((path) => rm(path, { recursive: true, force: true })));
});

async function tempDirectory() {
  const path = await mkdtemp(join(tmpdir(), "kwiry-gate5-corpus-"));
  temporary.push(path);
  return path;
}

describe("Gate 5 deterministic corpora", () => {
  it("generates a deterministic functional corpus with bounded synthetic oracles", async () => {
    const firstRoot = await tempDirectory();
    const secondRoot = await tempDirectory();
    const first = await generateFunctionalCorpus(firstRoot);
    const second = await generateFunctionalCorpus(secondRoot);

    expect(first).toEqual(second);
    expect(first).toMatchObject({
      kind: "generated_functional",
      note_count: 14,
      expected_documents: 14,
      hash_algorithm: CORPUS_HASH_ALGORITHM,
      search_oracle_count: 10,
      mutation_oracle_count: 3,
    });
    expect(first.sha256).toMatch(/^[a-f0-9]{64}$/u);
    const files = (await readdir(firstRoot, { recursive: true }))
      .filter((path) => path.endsWith(".md"));
    expect(files).toHaveLength(14);
    const oracles = functionalOracles();
    expect(oracles.search.map(({ id }) => id)).toEqual([
      "filename",
      "title",
      "alias",
      "tag",
      "heading",
      "path",
      "body",
      "identifier",
      "unicode",
      "ranking",
    ]);
    expect(oracles.mutations.map(({ id }) => id)).toEqual(["modify", "rename", "delete"]);
    expect(await readFile(join(firstRoot, "functional/unicode.md"), "utf8"))
      .toContain("café naïve 東京 ☃");
  });

  it("generates exact-size deterministic performance corpora from a seed", async () => {
    const firstRoot = await tempDirectory();
    const secondRoot = await tempDirectory();
    const options = { noteCount: 32, totalBytes: 64 * 1024, seed: 123456 };
    const first = await generatePerformanceCorpus(firstRoot, options);
    const second = await generatePerformanceCorpus(secondRoot, options);

    expect(first).toEqual(second);
    expect(first).toMatchObject({
      kind: "generated_performance",
      note_count: 32,
      markdown_bytes: 64 * 1024,
      expected_documents: 32,
      seed_u32: 123456,
      hash_algorithm: CORPUS_HASH_ALGORITHM,
    });
    expect(first.sha256).toMatch(/^[a-f0-9]{64}$/u);
    const note = await readFile(join(firstRoot, "performance/note-00007.md"), "utf8");
    expect(note).toContain("performancebeacon00007");
    expect(note).toContain("## Section three");
    const vocabularyIds = [...note.matchAll(/\b[wp](\d{4})\b/gu)]
      .map((match) => Number(match[1]));
    expect(vocabularyIds.length).toBeGreaterThan(0);
    expect(Math.max(...vocabularyIds)).toBeLessThan(PERFORMANCE_VOCABULARY_SIZE);
  });

  it("keeps the combined restore probe within one generated heading section", async () => {
    const note = performanceNoteBytes(
      0,
      Math.ceil(PERFORMANCE_CORPUS_BYTES / PERFORMANCE_NOTE_COUNT),
      PERFORMANCE_SEED,
    ).toString("utf8");
    const sectionStart = note.indexOf("# Performance Note 00000");
    const sectionEnd = note.indexOf("## Section one");
    expect(sectionStart).toBeGreaterThanOrEqual(0);
    expect(sectionEnd).toBeGreaterThan(sectionStart);
    expect(note.slice(sectionStart, sectionEnd))
      .toContain("performancebeacon00000 synthetic generated corpus");

    const performanceScript = await readFile(
      new URL("../scripts/gate5-performance.mjs", import.meta.url),
      "utf8",
    );
    expect(performanceScript)
      .toContain('["combined", "synthetic performancebeacon00000"]');
  });

  it("rejects a performance byte target too small for deterministic notes", async () => {
    const root = await tempDirectory();
    await expect(generatePerformanceCorpus(root, {
      noteCount: 10,
      totalBytes: 100,
      seed: 1,
    })).rejects.toThrow("byte target is too small");
  });
});
