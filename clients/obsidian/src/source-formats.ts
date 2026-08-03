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

// Mirrors kwiry_core::source::SOURCE_PREPARATION_SCHEMA_VERSION. This belongs in
// the policy fingerprint so a preparation-schema change always invalidates a
// cache even when the enabled extension set is unchanged.
export const SOURCE_PREPARATION_SCHEMA_VERSION = 5 as const;

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
  pdf: true,
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

export function isSourceFormatEnabled(
  format: SourceFormat,
  enabled: Readonly<EnabledSourceFormats>,
): boolean {
  return enabled[format];
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
  const enabledSet = SOURCE_FORMATS.filter((format) => enabled[format]).sort().join(",");
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
