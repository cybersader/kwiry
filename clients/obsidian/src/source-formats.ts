// SPDX-FileCopyrightText: 2026 cybersader
// SPDX-License-Identifier: GPL-3.0-only

export const SOURCE_FORMATS = [
  "markdown",
  "text",
  "base",
  "canvas",
  "docx",
  "pdf",
] as const;

export type SourceFormat = typeof SOURCE_FORMATS[number];

export const EXTRACTABLE_SOURCE_FORMATS = [
  "markdown",
  "text",
  "base",
  "canvas",
  "docx",
] as const satisfies readonly SourceFormat[];

export type ExtractableSourceFormat = typeof EXTRACTABLE_SOURCE_FORMATS[number];

const EXTRACTABLE_SOURCE_FORMAT_SET: ReadonlySet<SourceFormat> = new Set(
  EXTRACTABLE_SOURCE_FORMATS,
);

// Mirrors kwiry_core::source::SOURCE_PREPARATION_SCHEMA_VERSION. This belongs in
// the policy fingerprint so a preparation-schema change always invalidates a
// cache even when the enabled extension set is unchanged.
export const SOURCE_PREPARATION_SCHEMA_VERSION = 7 as const;

export const IN_PLUGIN_SOURCE_SUPPORT_DESCRIPTION =
  "Indexes enabled, extractable sources from the active vault. Markdown, plain text, Base, Canvas, and DOCX are available; PDF remains unavailable until its extractor ships and its bytes are not read. This profile is lexical-only and never reads the daemon token.";

export interface EnabledSourceFormats {
  markdown: boolean;
  text: boolean;
  base: boolean;
  canvas: boolean;
  docx: boolean;
  pdf: boolean;
}

export const DEFAULT_ENABLED_SOURCE_FORMATS: Readonly<EnabledSourceFormats> = Object.freeze({
  markdown: true,
  text: true,
  base: true,
  canvas: true,
  docx: true,
  pdf: false,
});

const FORMAT_BY_EXTENSION: Readonly<Record<string, SourceFormat>> = Object.freeze({
  md: "markdown",
  markdown: "markdown",
  mdx: "markdown",
  txt: "text",
  base: "base",
  canvas: "canvas",
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
 * schema, and the sorted enabled-format set. Object property order is never an
 * input, and disabled formats are represented by their absence from the set.
 */
export async function formatPolicyFingerprint(
  enabled: Readonly<EnabledSourceFormats>,
): Promise<string> {
  const enabledSet = SOURCE_FORMATS.filter(
    (format) => isSourceFormatEnabled(format, enabled),
  ).sort().join(",");
  const material = [
    "kwiry-source-format-policy-v1",
    `source-preparation-schema=${SOURCE_PREPARATION_SCHEMA_VERSION}`,
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
