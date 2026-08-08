// SPDX-FileCopyrightText: 2026 cybersader
// SPDX-License-Identifier: GPL-3.0-only
import { describe, expect, it } from "vitest";

import {
  RESERVED_EXCERPT_MARKERS,
  flattenExcerpt,
  parseExcerpt,
  sanitizeExcerptText,
} from "../src/excerpt";

describe("parseExcerpt", () => {
  it("splits daemon highlight tags into segments", () => {
    expect(parseExcerpt("say <b>hello</b> world")).toEqual([
      { text: "say ", highlighted: false },
      { text: "hello", highlighted: true },
      { text: " world", highlighted: false },
    ]);
  });

  it("treats non-highlight markup as literal text, preventing injection", () => {
    const segments = parseExcerpt('<img src=x onerror=alert(1)> and <script>x</script>');
    expect(segments.every((s) => !s.highlighted)).toBe(true);
    expect(segments.map((s) => s.text).join("")).toContain("<img src=x onerror=alert(1)>");
  });

  it("handles unclosed highlight tags as literal", () => {
    expect(parseExcerpt("broken <b>tail")).toEqual([
      { text: "broken <b>tail", highlighted: false },
    ]);
  });

  it("decodes tantivy-escaped entities", () => {
    expect(parseExcerpt("a &amp; b &lt;c&gt;")).toEqual([
      { text: "a & b <c>", highlighted: false },
    ]);
  });

  it("handles plain semantic fallback excerpts", () => {
    expect(parseExcerpt("just plain text")).toEqual([
      { text: "just plain text", highlighted: false },
    ]);
  });
});

describe("sanitizeExcerptText", () => {
  it("neutralizes every reserved private marker a note could contain", () => {
    const forged = `${RESERVED_EXCERPT_MARKERS[0]}forged${RESERVED_EXCERPT_MARKERS[1]} tail`;
    const sanitized = sanitizeExcerptText(forged);
    for (const marker of RESERVED_EXCERPT_MARKERS) {
      expect(sanitized).not.toContain(marker);
    }
    expect(sanitized).toContain("forged");
    expect(sanitized).toHaveLength(forged.length);
  });
});

describe("flattenExcerpt", () => {
  it("collapses newlines and runs of whitespace", () => {
    const flat = flattenExcerpt([{ text: "a\n\n  b\tc", highlighted: false }]);
    expect(flat[0]!.text).toBe("a b c");
  });
});
