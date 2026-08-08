// SPDX-FileCopyrightText: 2026 cybersader
// SPDX-License-Identifier: GPL-3.0-only

import { createHash } from "node:crypto";
import { mkdir, writeFile } from "node:fs/promises";
import { dirname, join } from "node:path";

export const PERFORMANCE_NOTE_COUNT = 10_000;
export const PERFORMANCE_CORPUS_BYTES = 50 * 1024 * 1024;
export const PERFORMANCE_SEED = 0x4b574952;
export const PERFORMANCE_VOCABULARY_SIZE = 4_096;
export const CORPUS_HASH_ALGORITHM = "sha256-path-nul-decimal-length-nul-bytes-nul-v1";

const FUNCTIONAL_NOTES = [
  ["functional/filenamebeacon.md", "# Filename witness\nThe filename owns this synthetic witness.\n"],
  ["functional/title.md", "---\ntitle: titlebeacon\n---\n# Title witness\nSynthetic title field.\n"],
  ["functional/alias.md", "---\naliases: [aliasbeacon]\n---\n# Alias witness\nSynthetic alias field.\n"],
  ["functional/tag.md", "---\ntags: [tagbeacon]\n---\n# Tag witness\nSynthetic tag field.\n"],
  ["functional/heading.md", "# headingbeacon\nSynthetic heading field.\n"],
  ["functional/pathbeacon/folder-note.md", "# Path witness\nSynthetic path field.\n"],
  ["functional/body.md", "# Body witness\nbodybeacon appears only in synthetic body content.\n"],
  ["functional/identifier.md", "# Identifier witness\nSynthetic case identifier KWIR-2048.\n"],
  ["functional/ranking-title.md", "---\ntitle: luminous quasar\n---\n# Ranking title\nSynthetic ranking witness.\n"],
  ["functional/ranking-body.md", "# Ranking body\nThe synthetic phrase luminous quasar appears in body text.\n"],
  ["functional/mutations/modify.md", "# Modify witness\nmutation-before\n"],
  ["functional/mutations/rename.md", "# Rename witness\nrename-before\n"],
  ["functional/mutations/delete.md", "# Delete witness\ndelete-before\n"],
  ["functional/unicode.md", "# Unicode witness\nSynthetic café naïve 東京 ☃ content.\n\n## Second section\nunicodebeacon\n"],
];

export async function generateFunctionalCorpus(root) {
  const hash = createHash("sha256");
  let markdownBytes = 0;
  for (const [path, text] of FUNCTIONAL_NOTES) {
    const bytes = Buffer.from(text, "utf8");
    await writeCorpusFile(root, path, bytes);
    updateCorpusHash(hash, path, bytes);
    markdownBytes += bytes.byteLength;
  }
  return {
    kind: "generated_functional",
    note_count: FUNCTIONAL_NOTES.length,
    markdown_bytes: markdownBytes,
    sha256: hash.digest("hex"),
    hash_algorithm: CORPUS_HASH_ALGORITHM,
    expected_documents: FUNCTIONAL_NOTES.length,
    search_oracle_count: 10,
    mutation_oracle_count: 3,
  };
}

export async function generatePerformanceCorpus(root, options = {}) {
  const noteCount = options.noteCount ?? PERFORMANCE_NOTE_COUNT;
  const totalBytes = options.totalBytes ?? PERFORMANCE_CORPUS_BYTES;
  const seed = options.seed ?? PERFORMANCE_SEED;
  requirePositiveSafeInteger(noteCount, "noteCount");
  requirePositiveSafeInteger(totalBytes, "totalBytes");
  requireUint32(seed, "seed");
  if (totalBytes < noteCount * 512) {
    throw new Error("performance corpus byte target is too small");
  }

  const hash = createHash("sha256");
  const baseBytes = Math.floor(totalBytes / noteCount);
  const extraBytes = totalBytes % noteCount;
  for (let index = 0; index < noteCount; index += 1) {
    const targetBytes = baseBytes + (index < extraBytes ? 1 : 0);
    const path = performanceNotePath(index);
    const bytes = performanceNoteBytes(index, targetBytes, seed);
    await writeCorpusFile(root, path, bytes);
    updateCorpusHash(hash, path, bytes);
  }

  return {
    kind: "generated_performance",
    note_count: noteCount,
    markdown_bytes: totalBytes,
    sha256: hash.digest("hex"),
    hash_algorithm: CORPUS_HASH_ALGORITHM,
    expected_documents: noteCount,
    seed_u32: seed,
  };
}

export function functionalOracles() {
  return {
    search: [
      { id: "filename", query: "filenamebeacon", paths: ["functional/filenamebeacon.md"] },
      { id: "title", query: "titlebeacon", paths: ["functional/title.md"] },
      { id: "alias", query: "aliasbeacon", paths: ["functional/alias.md"] },
      { id: "tag", query: "tagbeacon", paths: ["functional/tag.md"] },
      { id: "heading", query: "headingbeacon", paths: ["functional/heading.md"] },
      { id: "path", query: "pathbeacon", paths: ["functional/pathbeacon/folder-note.md"] },
      { id: "body", query: "bodybeacon", paths: ["functional/body.md"] },
      { id: "identifier", query: "KWIR-2048", paths: ["functional/identifier.md"] },
      { id: "unicode", query: "unicodebeacon", paths: ["functional/unicode.md"] },
      {
        id: "ranking",
        query: "luminous quasar",
        paths: ["functional/ranking-title.md", "functional/ranking-body.md"],
      },
    ],
    mutations: [
      {
        id: "modify",
        operation: "modify",
        path: "functional/mutations/modify.md",
        before: "mutation-before",
        after: "mutation-after",
      },
      {
        id: "rename",
        operation: "rename",
        path: "functional/mutations/rename.md",
        next_path: "functional/mutations/renamed.md",
        before: "rename-before",
      },
      {
        id: "delete",
        operation: "delete",
        path: "functional/mutations/delete.md",
        before: "delete-before",
      },
    ],
  };
}

export function performanceNotePath(index) {
  return `performance/note-${String(index).padStart(5, "0")}.md`;
}

export function performanceNoteBytes(index, targetBytes, seed = PERFORMANCE_SEED) {
  const id = String(index).padStart(5, "0");
  const group = String(index % 64).padStart(2, "0");
  const prefix = [
    "---",
    `title: Performance Note ${id}`,
    `tags: [kwiry-generated, group-${group}]`,
    "---",
    `# Performance Note ${id}`,
    `performancebeacon${id} synthetic generated corpus`,
    "",
    "## Section one",
    deterministicWords(index, seed, 24),
    "",
    "## Section two",
    deterministicWords(index + 1, seed, 24),
    "",
    "## Section three",
    deterministicWords(index + 2, seed, 24),
    "",
  ].join("\n");
  const prefixBytes = Buffer.byteLength(prefix, "utf8");
  if (prefixBytes > targetBytes) throw new Error("performance note target is too small");
  const filler = deterministicFiller(index, seed, targetBytes - prefixBytes);
  const bytes = Buffer.from(prefix + filler, "utf8");
  if (bytes.byteLength !== targetBytes) throw new Error("performance note byte count mismatch");
  return bytes;
}

function deterministicWords(index, seed, count) {
  let state = (seed ^ index) >>> 0;
  const words = [];
  for (let offset = 0; offset < count; offset += 1) {
    state = xorshift32(state || 1);
    words.push(`w${String(state % PERFORMANCE_VOCABULARY_SIZE).padStart(4, "0")}`);
  }
  return words.join(" ");
}

function deterministicFiller(index, seed, byteLength) {
  if (byteLength === 0) return "";
  let state = (seed + index) >>> 0;
  let text = "";
  while (text.length < byteLength) {
    state = xorshift32(state || 1);
    text += ` p${String(state % PERFORMANCE_VOCABULARY_SIZE).padStart(4, "0")}`;
  }
  return text.slice(0, byteLength);
}

function xorshift32(value) {
  let state = value >>> 0;
  state ^= state << 13;
  state ^= state >>> 17;
  state ^= state << 5;
  return state >>> 0;
}

async function writeCorpusFile(root, path, bytes) {
  const destination = join(root, path);
  await mkdir(dirname(destination), { recursive: true });
  await writeFile(destination, bytes);
}

function updateCorpusHash(hash, path, bytes) {
  hash.update(path, "utf8");
  hash.update("\0");
  hash.update(String(bytes.byteLength), "utf8");
  hash.update("\0");
  hash.update(bytes);
  hash.update("\0");
}

function requirePositiveSafeInteger(value, name) {
  if (!Number.isSafeInteger(value) || value < 1) throw new Error(`${name} must be positive`);
}

function requireUint32(value, name) {
  if (!Number.isInteger(value) || value < 0 || value > 0xffff_ffff) {
    throw new Error(`${name} must be an unsigned 32-bit integer`);
  }
}
