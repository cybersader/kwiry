// SPDX-FileCopyrightText: 2026 cybersader
// SPDX-License-Identifier: GPL-3.0-only
//
// Field-level validation of a Rust source preparation, kept in its own module
// free of the WASM import so it is directly testable. The adapter cannot be
// imported in a unit test because it pulls in the Rust binary.

const SOURCE_SCHEMA_VERSION = 1;

/// Names the first field of a source preparation that fails validation, or
/// null when the preparation is valid.
///
/// Eight releases were spent narrowing a production failure to
/// `source_rejected` at the rust stage, at which point the report still could
/// not say WHICH field the Rust layer refused. This function is why: the
/// predicate below is one long boolean chain that collapses forty checks into
/// a single false. Field names are fixed identifiers chosen in this codebase,
/// never vault content, so reporting one is safe.
export function sourcePreparationDefect(value: unknown): string | null {
  if (!isRecord(value)) return "not_a_record";
  const checks: readonly (readonly [string, () => boolean])[] = [
    ["schema_version", () => value.schema_version === SOURCE_SCHEMA_VERSION],
    ["source_key", () => isBoundedString(value.source_key, 128)],
    ["vault_id", () => isBoundedString(value.vault_id, 1_024)],
    ["room", () => value.room === undefined || isBoundedString(value.room, 1_024)],
    ["path", () => isBoundedString(value.path, 4_096)],
    ["format", () => value.format === "markdown" || value.format === "text"],
    ["content_hash", () => value.content_hash === null
      || isBoundedString(value.content_hash, 128)],
    ["byte_length", () => isNonNegativeSafeInteger(value.byte_length)],
    ["mtime", () => isNonNegativeSafeInteger(value.mtime)],
    ["mtime_nanos", () => typeof value.mtime_nanos === "string"
      && /^[0-9]{1,39}$/u.test(value.mtime_nanos)],
    ["retrieval", () => isRetrieval(value.retrieval)],
    ["chunks_shape", () => Array.isArray(value.chunks) && value.chunks.length <= 100_000],
    ["chunks_contents", () => Array.isArray(value.chunks) && value.chunks.every(isPreparedChunk)],
    ["kind", () => value.kind === "indexed" || value.kind === "skipped"],
    ["warning", () => value.warning === undefined
      || isBoundedString(value.warning, 4_096, true)],
    ["skipped_has_chunks", () => value.kind !== "skipped"
      || (Array.isArray(value.chunks) && value.chunks.length === 0)],
    ["indexed_missing_hash", () => value.kind !== "indexed" || value.content_hash !== null],
  ];
  for (const [name, check] of checks) {
    let passed: boolean;
    try {
      passed = check();
    } catch {
      return name;
    }
    if (!passed) return name;
  }
  return null;
}


function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function isBoundedString(value: unknown, maximum: number, allowEmpty = false): value is string {
  return typeof value === "string"
    && value.length <= maximum
    && (allowEmpty || value.length > 0);
}

function isBoundedStrings(
  value: unknown,
  maximumItems: number,
  maximumCharacters: number,
  allowEmpty = false,
): value is string[] {
  return Array.isArray(value)
    && value.length <= maximumItems
    && value.every((item) => isBoundedString(item, maximumCharacters, allowEmpty));
}

function isNonNegativeSafeInteger(value: unknown): value is number {
  return typeof value === "number" && Number.isSafeInteger(value) && value >= 0;
}

function isRetrieval(value: unknown): boolean {
  return isRecord(value)
    && hasExactKeys(value, ["filename", "stem", "aliases"])
    && isBoundedString(value.filename, 4_096)
    && isBoundedString(value.stem, 4_096)
    && isBoundedStrings(value.aliases, 256, 1_024);
}

function isPreparedChunk(value: unknown): boolean {
  return isRecord(value)
    && hasExactKeys(value, ["chunk", "heading_text", "technical_identifiers"])
    && isChunk(value.chunk)
    && isBoundedString(value.heading_text, 8_192, true)
    && isBoundedStrings(value.technical_identifiers, 1_024, 1_024);
}

function hasExactKeys(value: Record<string, unknown>, keys: readonly string[]): boolean {
  const actual = Object.keys(value);
  return actual.length === keys.length && actual.every((key) => keys.includes(key));
}

function isChunk(value: unknown): boolean {
  return isRecord(value)
    && hasExactKeys(value, [
      "chunk_id",
      "vault_id",
      "room",
      "path",
      "heading_path",
      "content",
      "frontmatter",
      "links_out",
      "mtime",
      "content_hash",
      "chunking_version",
    ])
    && isBoundedString(value.chunk_id, 128)
    && isBoundedString(value.vault_id, 1_024)
    && (value.room === null || isBoundedString(value.room, 1_024))
    && isBoundedString(value.path, 4_096)
    && isBoundedStrings(value.heading_path, 64, 1_024)
    && isBoundedString(value.content, 16_384)
    && isFrontmatter(value.frontmatter)
    && isBoundedStrings(value.links_out, 4_096, 4_096)
    && isNonNegativeSafeInteger(value.mtime)
    && isBoundedString(value.content_hash, 128)
    && isNonNegativeSafeInteger(value.chunking_version);
}

function isFrontmatter(value: unknown): boolean {
  if (!isRecord(value)) return false;
  const allowed = ["title", "description", "tags", "status", "date"];
  if (Object.keys(value).some((key) => !allowed.includes(key))) return false;
  for (const key of ["title", "description", "status", "date"] as const) {
    if (value[key] !== undefined && !isBoundedString(value[key], 1_024, true)) return false;
  }
  return value.tags === undefined || isBoundedStrings(value.tags, 256, 1_024, true);
}
