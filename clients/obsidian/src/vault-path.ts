// SPDX-FileCopyrightText: 2026 cybersader
// SPDX-License-Identifier: GPL-3.0-only

export function isNormalizedMarkdownPath(value: string): boolean {
  if (
    value.length === 0
    || value.length > 4_096
    || value.startsWith("/")
    || value.includes("\\")
    || value.includes("\0")
    || !value.toLowerCase().endsWith(".md")
  ) {
    return false;
  }
  const components = value.split("/");
  return components.every(
    (component) => component.length > 0 && component !== "." && component !== "..",
  );
}
