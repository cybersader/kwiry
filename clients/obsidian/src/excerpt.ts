// SPDX-FileCopyrightText: 2026 cybersader
// SPDX-License-Identifier: GPL-3.0-only
//
// Excerpt rendering: the daemon's lexical snippets wrap matches in <b>
// tags (Tantivy SnippetGenerator); semantic fallback excerpts are plain
// text. Never render daemon text as raw HTML — parse into typed segments
// so the UI builds DOM nodes with createEl/setText only.

export interface ExcerptSegment {
  text: string;
  highlighted: boolean;
}

const HIGHLIGHT_OPEN = "<b>";
const HIGHLIGHT_CLOSE = "</b>";
export const FTS_HIGHLIGHT_START = "";
export const FTS_HIGHLIGHT_END = "";

/**
 * Splits a daemon excerpt into plain and highlighted segments. Only the
 * daemon's own highlight element is honored; any other markup-looking
 * text is treated as literal content.
 */
export function parseExcerpt(excerpt: string): ExcerptSegment[] {
  return parseMarkedExcerpt(excerpt, HIGHLIGHT_OPEN, HIGHLIGHT_CLOSE, decodeEntities);
}

export function parseFtsExcerpt(excerpt: string): ExcerptSegment[] {
  return parseMarkedExcerpt(
    excerpt,
    FTS_HIGHLIGHT_START,
    FTS_HIGHLIGHT_END,
    (text) => text,
  );
}

function parseMarkedExcerpt(
  excerpt: string,
  startMarker: string,
  endMarker: string,
  normalize: (text: string) => string,
): ExcerptSegment[] {
  const segments: ExcerptSegment[] = [];
  let rest = excerpt;
  while (rest.length > 0) {
    const open = rest.indexOf(startMarker);
    if (open < 0) {
      segments.push({ text: normalize(rest), highlighted: false });
      break;
    }
    const close = rest.indexOf(endMarker, open + startMarker.length);
    if (close < 0) {
      segments.push({ text: normalize(rest), highlighted: false });
      break;
    }
    if (open > 0) {
      segments.push({ text: normalize(rest.slice(0, open)), highlighted: false });
    }
    segments.push({
      text: normalize(rest.slice(open + startMarker.length, close)),
      highlighted: true,
    });
    rest = rest.slice(close + endMarker.length);
  }
  return segments.filter((segment) => segment.text.length > 0);
}

/** Tantivy HTML-escapes snippet text; undo the five standard entities. */
function decodeEntities(text: string): string {
  return text
    .replace(/&lt;/g, "<")
    .replace(/&gt;/g, ">")
    .replace(/&quot;/g, '"')
    .replace(/&#x?27;|&#39;|&apos;/g, "'")
    .replace(/&amp;/g, "&");
}

/** Collapses whitespace for single-line row display. */
export function flattenExcerpt(segments: ExcerptSegment[]): ExcerptSegment[] {
  return segments.map((segment) => ({
    ...segment,
    text: segment.text.replace(/\s+/g, " "),
  }));
}
