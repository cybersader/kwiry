// SPDX-FileCopyrightText: 2026 cybersader
// SPDX-License-Identifier: GPL-3.0-only

import { classifySourcePath, type SourceFormat } from "./source-formats";

export function isNormalizedVaultFilePath(value: string): boolean {
  if (
    value.length === 0
    || value.length > 4_096
    || value.startsWith("/")
    || value.includes("\\")
    || value.includes("\0")
  ) {
    return false;
  }
  const components = value.split("/");
  return components.every(
    (component) => component.length > 0 && component !== "." && component !== "..",
  );
}

export function pathMatchesFormat(value: string, format: SourceFormat): boolean {
  return isNormalizedVaultFilePath(value) && classifySourcePath(value) === format;
}

/** Compatibility helper for call sites that specifically require Markdown. */
export function isNormalizedMarkdownPath(value: string): boolean {
  return pathMatchesFormat(value, "markdown");
}
