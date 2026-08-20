// SPDX-FileCopyrightText: 2026 cybersader
// SPDX-License-Identifier: GPL-3.0-only
//
// Field-level validation of a Rust source preparation, kept in its own module
// free of the WASM import so it is directly testable. The adapter cannot be
// imported in a unit test because it pulls in the Rust binary.

const SOURCE_SCHEMA_VERSION = 10;

// This mirrors Rust's call-stack safety boundary. It is deliberately the only
// property-bag bound: source byte limits already protect allocation, while a
// cardinality or string-length cap would turn a valid large note into a reason
// to reject the whole source batch.
const MAX_PROPERTY_NESTING_DEPTH = 128;

export type PropertyBagDefect =
  | "frontmatter_not_a_record"
  | "frontmatter_property_value"
  | "frontmatter_property_nesting"
  | "frontmatter_property_cycle";

/** Names the first fixed ABI field that fails validation, or null when valid. */
export function sourcePreparationDefect(value: unknown): string | null {
  if (!isRecord(value)) return "not_a_record";
  const required = [
    "schema_version",
    "source_key",
    "vault_id",
    "path",
    "format",
    "extraction_profile",
    "coverage",
    "content_hash",
    "byte_length",
    "mtime",
    "mtime_nanos",
    "retrieval",
    "normalized_exact",
    "frontmatter",
    "chunks",
    "kind",
  ];
  if (!hasRequiredAndOptionalKeys(value, required, ["room", "canonical_frontmatter", "warning"])) {
    return "preparation_fields";
  }

  const checks: readonly (readonly [string, () => boolean])[] = [
    ["schema_version", () => value.schema_version === SOURCE_SCHEMA_VERSION],
    ["source_key", () => isBoundedString(value.source_key, 128)],
    ["vault_id", () => isBoundedString(value.vault_id, 1_024)],
    ["room", () => value.room === undefined || isBoundedString(value.room, 1_024)],
    ["path", () => isBoundedString(value.path, 4_096)],
    ["format", () => isSourceFormat(value.format)],
    ["extraction_profile", () => isExtractionProfile(value.extraction_profile)],
    ["coverage", () => isExtractionCoverage(value.coverage)],
    ["content_hash", () => value.content_hash === null
      || isBoundedString(value.content_hash, 128)],
    ["byte_length", () => isNonNegativeSafeInteger(value.byte_length)],
    ["mtime", () => isNonNegativeSafeInteger(value.mtime)],
    ["mtime_nanos", () => typeof value.mtime_nanos === "string"
      && /^[0-9]{1,39}$/u.test(value.mtime_nanos)],
    ["retrieval", () => isRetrieval(value.retrieval)],
    ["normalized_exact", () => isNormalizedExact(value.normalized_exact)],
    ["canonical_frontmatter", () => canonicalFrontmatterIsValid(
      value.canonical_frontmatter,
      value.format,
      value.kind,
      value.normalized_exact,
    )],
    ["chunks_shape", () => Array.isArray(value.chunks) && value.chunks.length <= 100_000],
  ];
  for (const [name, check] of checks) {
    try {
      if (!check()) return name;
    } catch {
      return name;
    }
  }

  const frontmatterDefect = preparedPropertyBagDefect(value.frontmatter);
  if (frontmatterDefect !== null) return frontmatterDefect;

  try {
    for (const chunk of value.chunks as unknown[]) {
      const defect = preparedChunkDefect(chunk, value);
      if (defect !== null) return defect;
    }
  } catch {
    return "chunks_contents";
  }

  const trailingChecks: readonly (readonly [string, () => boolean])[] = [
    ["kind", () => value.kind === "indexed" || value.kind === "skipped"],
    ["warning", () => value.warning === undefined
      || isBoundedString(value.warning, 4_096, true)],
    ["skipped_has_chunks", () => value.kind !== "skipped"
      || (Array.isArray(value.chunks) && value.chunks.length === 0)],
    ["indexed_missing_hash", () => value.kind !== "indexed" || value.content_hash !== null],
    ["coverage", () => value.kind === "indexed"
      ? value.coverage === "indexed-complete" || value.coverage === "indexed-partial"
      : value.coverage === "skipped-no-extractable-text"
        || value.coverage === "unreadable"
        || value.coverage === "quarantined"],
  ];
  for (const [name, check] of trailingChecks) {
    try {
      if (!check()) return name;
    } catch {
      return name;
    }
  }
  return null;
}

/**
 * Validates the JSON-shaped open property bag without imposing note-content
 * policy. The returned identifiers are safe for diagnostics: they describe the
 * failed structural rule and never include a property name or value from a note.
 */
export function propertyBagDefect(value: unknown): PropertyBagDefect | null {
  if (!isRecord(value)) return "frontmatter_not_a_record";

  try {
    const seen = new WeakSet<object>();
    seen.add(value);
    const pending: Array<{ value: unknown; depth: number }> = Object.keys(value)
      .map((name) => ({ value: value[name], depth: 1 }));

    while (pending.length > 0) {
      const item = pending.pop();
      if (item === undefined) break;
      if (item.depth > MAX_PROPERTY_NESTING_DEPTH) {
        return "frontmatter_property_nesting";
      }

      const property = item.value;
      if (property === null
        || typeof property === "string"
        || typeof property === "boolean") {
        continue;
      }
      if (typeof property === "number") {
        if (!Number.isFinite(property)) return "frontmatter_property_value";
        continue;
      }
      if (typeof property !== "object") return "frontmatter_property_value";

      // JSON is a tree. Cycles and shared object references cannot have come
      // from Rust JSON and therefore prove corruption rather than large content.
      if (seen.has(property)) return "frontmatter_property_cycle";
      seen.add(property);

      if (Array.isArray(property)) {
        for (let index = 0; index < property.length; index += 1) {
          pending.push({ value: property[index], depth: item.depth + 1 });
        }
        continue;
      }
      if (!isRecord(property)) return "frontmatter_property_value";
      for (const name of Object.keys(property)) {
        pending.push({ value: property[name], depth: item.depth + 1 });
      }
    }
  } catch {
    return "frontmatter_property_value";
  }
  return null;
}

export function isPropertyBag(value: unknown): value is Record<string, unknown> {
  return propertyBagDefect(value) === null;
}

/** Validates the explicitly tagged, lossless numeric property ABI from Rust. */
export function preparedPropertyBagDefect(value: unknown): PropertyBagDefect | null {
  if (!isRecord(value)) return "frontmatter_not_a_record";

  try {
    const seen = new WeakSet<object>();
    seen.add(value);
    const pending: Array<{ value: unknown; depth: number }> = Object.keys(value)
      .map((name) => ({ value: value[name], depth: 1 }));

    while (pending.length > 0) {
      const item = pending.pop();
      if (item === undefined) break;
      if (item.depth > MAX_PROPERTY_NESTING_DEPTH) {
        return "frontmatter_property_nesting";
      }
      if (!isRecord(item.value) || seen.has(item.value)) {
        return seen.has(item.value as object)
          ? "frontmatter_property_cycle"
          : "frontmatter_property_value";
      }
      seen.add(item.value);
      const type = item.value.type;
      if (type === "null") {
        if (!hasExactKeys(item.value, ["type"])) return "frontmatter_property_value";
        continue;
      }
      if (!hasExactKeys(item.value, ["type", "value"])) {
        return "frontmatter_property_value";
      }
      if (type === "boolean") {
        if (typeof item.value.value !== "boolean") return "frontmatter_property_value";
        continue;
      }
      if (type === "string") {
        if (typeof item.value.value !== "string") return "frontmatter_property_value";
        continue;
      }
      if (type === "i64") {
        if (!isSignedIntegerString(item.value.value)
          || !integerWithin(item.value.value, -(1n << 63n), (1n << 63n) - 1n)) {
          return "frontmatter_property_value";
        }
        continue;
      }
      if (type === "u64") {
        if (!isUnsignedIntegerString(item.value.value)
          || !integerWithin(item.value.value, 0n, (1n << 64n) - 1n)) {
          return "frontmatter_property_value";
        }
        continue;
      }
      if (type === "f64") {
        if (typeof item.value.value !== "string"
          || !/^[0-9a-f]{16}$/u.test(item.value.value)) {
          return "frontmatter_property_value";
        }
        continue;
      }
      if (type === "sequence") {
        if (!Array.isArray(item.value.value)) return "frontmatter_property_value";
        for (const child of item.value.value) {
          pending.push({ value: child, depth: item.depth + 1 });
        }
        continue;
      }
      if (type === "map") {
        if (!isRecord(item.value.value)) return "frontmatter_property_value";
        if (seen.has(item.value.value)) return "frontmatter_property_cycle";
        seen.add(item.value.value);
        for (const name of Object.keys(item.value.value)) {
          pending.push({ value: item.value.value[name], depth: item.depth + 1 });
        }
        continue;
      }
      return "frontmatter_property_value";
    }
  } catch {
    return "frontmatter_property_value";
  }
  return null;
}

export function isPreparedPropertyBag(value: unknown): value is Record<string, unknown> {
  return preparedPropertyBagDefect(value) === null;
}

function isSignedIntegerString(value: unknown): value is string {
  return typeof value === "string" && /^(?:0|-[1-9][0-9]*|[1-9][0-9]*)$/u.test(value);
}

function isUnsignedIntegerString(value: unknown): value is string {
  return typeof value === "string" && /^(?:0|[1-9][0-9]*)$/u.test(value);
}

function integerWithin(value: string, minimum: bigint, maximum: bigint): boolean {
  const integer = BigInt(value);
  return integer >= minimum && integer <= maximum;
}

function preparedChunkDefect(
  value: unknown,
  owner: Record<string, unknown>,
): string | null {
  if (!isRecord(value)
    || !hasRequiredAndOptionalKeys(
      value,
      ["chunk", "heading_text", "normalized_heading", "technical_identifiers"],
      ["content_role", "source_locator"],
    )
    || !isRecord(value.chunk)) {
    return "chunks_contents";
  }
  const chunk = value.chunk;
  if (!hasExactKeys(chunk, [
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
    || !isBoundedString(chunk.chunk_id, 128)
    || !isBoundedString(chunk.vault_id, 1_024)
    || (chunk.room !== null && !isBoundedString(chunk.room, 1_024))
    || !isBoundedString(chunk.path, 4_096)
    || !isStringArray(chunk.heading_path)
    || (owner.format === "html" && (
      chunk.heading_path.length > 6
      || chunk.heading_path.some((heading) => !isBoundedUtf8String(heading, 1_024))
    ))
    || !isBoundedString(chunk.content, 16_384, true)
    || !isStringArray(chunk.links_out)
    || !isNonNegativeSafeInteger(chunk.mtime)
    || !isBoundedString(chunk.content_hash, 128)
    || !isNonNegativeSafeInteger(chunk.chunking_version)
    || !isBoundedString(value.heading_text, 8_192, true)
    || !(value.normalized_heading === null
      || isBoundedNormalizedExact(value.normalized_heading))
    || !isStringArray(value.technical_identifiers)
    || (value.content_role !== undefined
      && value.content_role !== "primary"
      && value.content_role !== "supporting"
      && value.content_role !== "latent")) {
    return "chunks_contents";
  }
  if (isRoleTaggedFormat(owner.format)
    && taggedContentRoleFromChunkId(chunk.chunk_id) !== (value.content_role ?? "primary")) {
    return "chunks_content_role";
  }
  if (value.source_locator !== undefined
    && !locatorMatchesOwnerFormat(value.source_locator, owner.format)) {
    return "chunks_source_locator";
  }
  if (chunk.vault_id !== owner.vault_id
    || chunk.room !== (owner.room ?? null)
    || chunk.path !== owner.path
    || chunk.mtime !== owner.mtime
    || chunk.content_hash !== owner.content_hash) {
    return "chunks_source_correlation";
  }
  return propertyBagDefect(chunk.frontmatter);
}

function taggedContentRoleFromChunkId(
  value: unknown,
): "primary" | "supporting" | "latent" | null {
  if (typeof value !== "string" || !/^[0-9a-f]{64}$/u.test(value)) return null;
  const nibble = Number.parseInt(value[0]!, 16);
  if (nibble <= 3) return "primary";
  if (nibble <= 7) return "supporting";
  if (nibble <= 11) return "latent";
  return null;
}

function isRoleTaggedFormat(value: unknown): boolean {
  return value === "excel" || value === "html";
}

function canonicalFrontmatterIsValid(
  value: unknown,
  format: unknown,
  kind: unknown,
  normalizedExact: unknown,
): boolean {
  if (value === undefined) {
    // An indexed HTML source with a normalized canonical title necessarily
    // carries the compact canonical field in schema 10. Body-only HTML and all
    // skipped outcomes legitimately omit it.
    return format !== "html"
      || kind !== "indexed"
      || (isRecord(normalizedExact) && normalizedExact.title === null);
  }
  if (format !== "html" || kind !== "indexed" || !isRecord(value)) return false;
  // Schema 10's HTML producer emits exactly one canonical field. Accepting the
  // rest of the compact Frontmatter vocabulary would admit a preparation this
  // adapter cannot produce and could promote latent metadata into title lanes.
  if (!hasExactKeys(value, ["title"])
    || !isBoundedUtf8String(value.title, 1_048_576)) {
    return false;
  }
  return true;
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function isSourceFormat(value: unknown): boolean {
  return value === "markdown"
    || value === "text"
    || value === "base"
    || value === "canvas"
    || value === "docx"
    || value === "pdf"
    || value === "excalidraw"
    || value === "excel"
    || value === "html";
}

// Mirrors kwiry_core::policy::ExtractionProfile. Recorded on every preparation
// so a row can name the extractor set that produced it; the plugin only ever
// produces `portable`, and a preparation claiming anything else did not come
// from this adapter.
function isExtractionProfile(value: unknown): boolean {
  return value === "none" || value === "portable" || value === "enhanced";
}

function isExtractionCoverage(value: unknown): boolean {
  return value === "indexed-complete"
    || value === "indexed-partial"
    || value === "skipped-no-extractable-text"
    || value === "unreadable"
    || value === "quarantined";
}

/**
 * A chunk locator is valid only when its kind is the one its owning source's
 * format produces: `base_view` for a Base, `pdf_page` for a PDF. A locator on
 * any other format did not come from this adapter.
 */
function locatorMatchesOwnerFormat(value: unknown, format: unknown): boolean {
  if (!isRecord(value)) return false;
  if (hasExactKeys(value, ["kind", "view"])) {
    return value.kind === "base_view"
      && isBoundedString(value.view, 4_096)
      && format === "base";
  }
  if (hasExactKeys(value, ["kind", "page"])) {
    // Bounded by the Rust field's `u32` domain, not the extractor's page
    // ceiling: the ceiling is policy and may move, the type domain cannot.
    return value.kind === "pdf_page"
      && typeof value.page === "number"
      && Number.isSafeInteger(value.page)
      && value.page >= 1
      && value.page <= 4_294_967_295
      && format === "pdf";
  }
  if (hasExactKeys(value, ["kind", "sheet", "cell"])) {
    return value.kind === "excel_cell"
      && isBoundedString(value.sheet, 4_096)
      && isBoundedString(value.cell, 32)
      && format === "excel";
  }
  return false;
}

function isBoundedString(value: unknown, maximum: number, allowEmpty = false): value is string {
  return typeof value === "string"
    && value.length <= maximum
    && (allowEmpty || value.length > 0);
}

function isNonNegativeSafeInteger(value: unknown): value is number {
  return typeof value === "number" && Number.isSafeInteger(value) && value >= 0;
}

function isRetrieval(value: unknown): boolean {
  return isRecord(value)
    && hasExactKeys(value, ["filename", "stem", "aliases"])
    && isBoundedString(value.filename, 4_096)
    && isBoundedString(value.stem, 4_096)
    && isStringArray(value.aliases);
}

function isNormalizedExact(value: unknown): boolean {
  return isRecord(value)
    && hasExactKeys(value, ["filename", "stem", "aliases", "title"])
    && (value.filename === null || isBoundedNormalizedExact(value.filename))
    && (value.stem === null || isBoundedNormalizedExact(value.stem))
    && (value.title === null || isBoundedNormalizedExact(value.title))
    && Array.isArray(value.aliases)
    && value.aliases.every(isBoundedNormalizedExact);
}

function isBoundedNormalizedExact(value: unknown): value is string {
  return isBoundedUtf8String(value, 4_096);
}

function isBoundedUtf8String(value: unknown, maximumBytes: number, allowEmpty = false): value is string {
  return typeof value === "string"
    && (allowEmpty || value.length > 0)
    && new TextEncoder().encode(value).byteLength <= maximumBytes;
}

function hasExactKeys(value: Record<string, unknown>, keys: readonly string[]): boolean {
  const actual = Object.keys(value);
  return actual.length === keys.length && actual.every((key) => keys.includes(key));
}

function hasRequiredAndOptionalKeys(
  value: Record<string, unknown>,
  required: readonly string[],
  optional: readonly string[],
): boolean {
  const actual = Object.keys(value);
  return required.every((key) => actual.includes(key))
    && actual.every((key) => required.includes(key) || optional.includes(key));
}

/**
 * Every element is a string. Deliberately unbounded in count: no array length
 * demonstrates a malformed ABI response, while one rejected source aborts the
 * whole batch.
 */
function isStringArray(value: unknown): value is string[] {
  if (!Array.isArray(value)) return false;
  for (let index = 0; index < value.length; index += 1) {
    if (typeof value[index] !== "string") return false;
  }
  return true;
}
