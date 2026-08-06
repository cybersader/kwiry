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
 * A mirrored constant rather than a value read from `abi_identity()`, because
 * `main.ts` computes the policy hash during `onload()` to decide whether a
 * cache restore is even attempted, which is before the worker and the WASM
 * module exist. `rust/kwiry-obsidian-wasm/tests/typescript_mirror.rs` asserts
 * this constant equals what the adapter reports, so the mirror cannot drift.
 */
export const EXTRACTION_POLICY_FINGERPRINT =
  "1b393b155b0af728b1ec9c9131573c105c9e7aba41ff31a4d12c824d4c73adef" as const;

export const IN_PLUGIN_SOURCE_SUPPORT_DESCRIPTION =
  "Indexes enabled, extractable sources from the active vault. Markdown, plain text, Base, Canvas, DOCX, and Excalidraw are available; PDF remains unavailable until its extractor ships and its bytes are not read. This profile is lexical-only and never reads the daemon token.";

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
      return "Unavailable until an extractor ships. Files of this format are not inventoried or read.";
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
 * SHA-256 policy identity over a domain separator, the source preparation
 * schema, the compiled extraction policy, and the sorted enabled-format set.
 * Object property order is never an input, and disabled formats are
 * represented by their absence from the set.
 *
 * The separator carries `-v2` because the digest material gained the extraction
 * policy. Adding a component already changes every digest, so the bump is not
 * load-bearing for collision resistance — it is taken because the separator is
 * the human-readable statement of which generation of policy identity this is,
 * and a silent material change inside a `-v1` label is exactly the dishonesty
 * this hash exists to prevent.
 */
export async function formatPolicyFingerprint(
  enabled: Readonly<EnabledSourceFormats>,
): Promise<string> {
  const enabledSet = SOURCE_FORMATS.filter(
    (format) => isSourceFormatEnabled(format, enabled),
  ).sort().join(",");
  const material = [
    "kwiry-source-format-policy-v2",
    `source-preparation-schema=${SOURCE_PREPARATION_SCHEMA_VERSION}`,
    `extraction-policy=${EXTRACTION_POLICY_FINGERPRINT}`,
    `enabled-formats=${enabledSet}`,
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
