// SPDX-FileCopyrightText: 2026 cybersader
// SPDX-License-Identifier: GPL-3.0-only

export const SOURCE_FORMATS = [
  "markdown",
  "text",
  "base",
  "canvas",
  "docx",
  "pdf",
  "excalidraw",
] as const;

export type SourceFormat = typeof SOURCE_FORMATS[number];

export const EXTRACTABLE_SOURCE_FORMATS = [
  "markdown",
  "text",
  "base",
  "canvas",
  "docx",
  "pdf",
  "excalidraw",
] as const satisfies readonly SourceFormat[];

export type ExtractableSourceFormat = typeof EXTRACTABLE_SOURCE_FORMATS[number];

const EXTRACTABLE_SOURCE_FORMAT_SET: ReadonlySet<SourceFormat> = new Set(
  EXTRACTABLE_SOURCE_FORMATS,
);

// Mirrors kwiry_core::source::SOURCE_PREPARATION_SCHEMA_VERSION. This belongs in
// the policy fingerprint so a preparation-schema change always invalidates a
// cache even when the enabled extension set is unchanged.
export const SOURCE_PREPARATION_SCHEMA_VERSION = 9 as const;

/**
 * Mirrors `kwiry_core::policy::extraction_policy_fingerprint()` for a portable
 * build — the profile set this plugin's WASM adapter compiles.
 *
 * **Report-only.** It answers "which extractor set is this build running" for
 * diagnostics and for the daemon status surface. It is deliberately *not*
 * folded into the cache policy hash any more: a single digest over every
 * format's profile makes one format's tier change invalidate every other
 * format's rows, which is exactly the conflation `FORMAT_IDENTITIES` splits
 * apart. `rust/kwiry-obsidian-wasm/tests/typescript_mirror.rs` still asserts
 * this constant equals what the adapter reports, so the mirror cannot drift.
 */
export const EXTRACTION_POLICY_FINGERPRINT =
  "efbc627c533ae797104dcf65540dcf6f96edd7b9d96826c4bac7e93672f26ff2" as const;

/**
 * Mirrors `kwiry_core::policy::FORMAT_IDENTITY_SCHEMA_VERSION`.
 *
 * This is **core** identity: it names the *shape* of a per-format identity, so
 * a new component in that digest invalidates every row of every format. That
 * is the escape hatch which lets the per-format digest stay deliberately small.
 */
export const FORMAT_IDENTITY_SCHEMA_VERSION = 1 as const;

/**
 * Mirrors `kwiry_core::policy::format_identity_fingerprint()` for every format
 * this build compiles: SHA-256 over a domain separator, the identity schema
 * version, and exactly three facts — the format, the extraction profile
 * compiled for it, and its extractor version.
 *
 * These are **per-row** identity. Each cached `sources` row stores the identity
 * it was built under, and a row whose stored identity differs from this map's
 * value for its format is evicted at restore time; every other format's rows
 * survive untouched. Whether a format is *enabled* is never part of an
 * identity — enablement is configuration, it is never persisted, and it is
 * applied as a stateless projection instead.
 *
 * A mirrored constant rather than a value read from `abi_identity()` for the
 * same reason as the policy fingerprint: `main.ts` needs the core policy hash
 * during `onload()`, before the worker and WASM module exist. The mirror test
 * asserts both the values and the key set against the adapter, so a format
 * added in Rust cannot go missing here.
 */
export const FORMAT_IDENTITIES: Readonly<Record<SourceFormat, string>> = Object.freeze({
  markdown: "b678d0ea2d77d7a79ccc79f4f8a3a1d96aed9bb98757afb1381e5661a1fb96f7",
  text: "c89bb1c6cb87c1e6371d7d03956f1c6bf8bff605c847441c2c72d7599bbd464b",
  base: "d3eeb5a8e3246a07f0c1e41782a7f61628921f43f7afdd722f3a060104e7e079",
  canvas: "01eae3d6859de3287237e366b7fcd9f346dbab395453ef9422bcd67dc527858c",
  docx: "b4f9cff615a917e09d800c2784e17c836ef79cc767c49091818a7b1f8598a38e",
  pdf: "980924c70d64fc5de65ddc2141d043e9188f8856ec6196d30c0d5c11d363c3bc",
  excalidraw: "e1f6868bd320172f6b8d9afc3ac716e309499b065c62fa1b17ae4c2c09d98348",
});

/** The identity a row of `format` must carry to be reusable by this build. */
export function formatIdentity(format: SourceFormat): string {
  const identity = FORMAT_IDENTITIES[format];
  // Never a default: a format with no compiled identity cannot have its rows
  // proven reusable, so it must fail closed rather than borrow another's.
  if (identity === undefined) throw new Error("source format has no compiled identity");
  return identity;
}

export const IN_PLUGIN_SOURCE_SUPPORT_DESCRIPTION =
  "Indexes enabled, extractable sources from the active vault. Markdown, plain text, Base, Canvas, DOCX, Excalidraw, and PDF are available. PDF is off by default because a page is parsed and laid out rather than read, so a reference library costs far more to index than authored notes. This profile is lexical-only and never reads the daemon token.";

export interface EnabledSourceFormats {
  markdown: boolean;
  text: boolean;
  base: boolean;
  canvas: boolean;
  docx: boolean;
  pdf: boolean;
  excalidraw: boolean;
}

export const DEFAULT_ENABLED_SOURCE_FORMATS: Readonly<EnabledSourceFormats> = Object.freeze({
  markdown: true,
  text: true,
  base: true,
  canvas: true,
  docx: true,
  pdf: false,
  excalidraw: true,
});

const FORMAT_BY_EXTENSION: Readonly<Record<string, SourceFormat>> = Object.freeze({
  md: "markdown",
  markdown: "markdown",
  mdx: "markdown",
  txt: "text",
  base: "base",
  canvas: "canvas",
  excalidraw: "excalidraw",
  docx: "docx",
  pdf: "pdf",
});

/** Classifies one normalized vault-relative path into the closed format set. */
export function classifySourcePath(path: string): SourceFormat | null {
  if (!isNormalizedVaultPath(path)) return null;
  const name = path.slice(path.lastIndexOf("/") + 1);
  const separator = name.lastIndexOf(".");
  if (separator <= 0 || separator === name.length - 1) return null;
  return FORMAT_BY_EXTENSION[name.slice(separator + 1).toLowerCase()] ?? null;
}

export function isSourceFormatExtractable(
  format: SourceFormat,
): format is ExtractableSourceFormat {
  return EXTRACTABLE_SOURCE_FORMAT_SET.has(format);
}

export function isSourceFormatEnabled(
  format: SourceFormat,
  enabled: Readonly<EnabledSourceFormats>,
): boolean {
  return isSourceFormatExtractable(format) && enabled[format];
}

export function normalizeEnabledSourceFormats(
  enabled: Readonly<EnabledSourceFormats>,
): EnabledSourceFormats {
  return {
    markdown: isSourceFormatEnabled("markdown", enabled),
    text: isSourceFormatEnabled("text", enabled),
    base: isSourceFormatEnabled("base", enabled),
    canvas: isSourceFormatEnabled("canvas", enabled),
    docx: isSourceFormatEnabled("docx", enabled),
    pdf: isSourceFormatEnabled("pdf", enabled),
    excalidraw: isSourceFormatEnabled("excalidraw", enabled),
  };
}

/**
 * The enabled formats as a sorted, deduplicated list.
 *
 * This is configuration, not identity. It is never digested and never
 * persisted; it travels to the Worker so a restored image can be projected
 * down to the formats the user currently wants, before that image is published
 * as searchable.
 */
export function enabledSourceFormatList(
  enabled: Readonly<EnabledSourceFormats>,
): SourceFormat[] {
  return SOURCE_FORMATS
    .filter((format) => isSourceFormatEnabled(format, enabled))
    .slice()
    .sort();
}

export function sourceFormatDescription(format: SourceFormat): string {
  switch (format) {
    case "markdown":
      return "Extract and index Markdown notes.";
    case "text":
      return "Extract and index plain-text files.";
    case "base":
      return "Extract authored YAML configuration and named views; materialized query rows are never indexed.";
    case "canvas":
      return "Extract authored text cards, group and edge labels, URLs, and file-reference paths without reading referenced files.";
    case "excalidraw":
      return "Extract authored text, container labels, frame names, and links from Excalidraw drawings without reading referenced files.";
    case "docx":
      return "Extract body text, tables, headings, comments, notes, and headers or footers. Tracked deletions and hidden text are extracted and marked latent.";
    case "pdf":
      return "Extract text one section per page, recovering reading order from glyph positions. A page is navigation metadata, never searchable text, so PDF results carry no heading path. Encrypted documents are refused without being read, and a document using a font this profile cannot decode contributes no text at all rather than a partial reading. Off by default: pages are laid out rather than read, so a reference library costs far more to index than authored notes.";
  }
}

export function isEnabledSourcePath(
  path: string,
  enabled: Readonly<EnabledSourceFormats>,
): boolean {
  const format = classifySourcePath(path);
  return format !== null && isSourceFormatEnabled(format, enabled);
}

/**
 * SHA-256 **core** identity: a domain separator, the source preparation schema,
 * and the per-format identity schema. Nothing format-specific and nothing
 * configuration-specific belongs here.
 *
 * It takes no argument. That is the whole point of the split: the enabled set
 * used to be digest material, so toggling one format in Settings discarded a
 * complete cache; and the extraction policy used to be digest material, so one
 * format's tier change discarded every other format's rows. Both facts moved —
 * enablement to a stateless restore-time projection, per-format extraction to
 * `FORMAT_IDENTITIES` and the per-row predicate. What is left is exactly the
 * set of facts for which no row of any format is usable.
 *
 * The separator carries `-v3` because the material changed. A silent material
 * change inside a `-v2` label is exactly the dishonesty this hash exists to
 * prevent, so the generation is stated even though the digest already moves.
 */
export async function corePolicyFingerprint(): Promise<string> {
  const material = [
    "kwiry-source-core-policy-v3",
    `source-preparation-schema=${SOURCE_PREPARATION_SCHEMA_VERSION}`,
    `format-identity-schema=${FORMAT_IDENTITY_SCHEMA_VERSION}`,
  ].join("\0");
  const digest = await crypto.subtle.digest("SHA-256", new TextEncoder().encode(material));
  return [...new Uint8Array(digest)]
    .map((byte) => byte.toString(16).padStart(2, "0"))
    .join("");
}

function isNormalizedVaultPath(value: string): boolean {
  if (
    value.length === 0
    || value.length > 4_096
    || value.startsWith("/")
    || value.includes("\\")
    || value.includes("\0")
  ) {
    return false;
  }
  return value.split("/").every(
    (component) => component.length > 0 && component !== "." && component !== "..",
  );
}
