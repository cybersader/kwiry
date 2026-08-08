// SPDX-FileCopyrightText: 2026 cybersader
// SPDX-License-Identifier: GPL-3.0-only
//
// Excerpt hydration for the in-plugin backend.
//
// The in-plugin FTS5 index is contentless: it stores postings, not text, so
// the Worker cannot produce snippet text and does not try to. Files are the
// authoritative source, so the host derives a bounded excerpt window from the
// vault file that the hit points at, at query time.
//
// Truthfulness rules encoded here:
//   * every highlighted segment is a literal occurrence of a query term in the
//     text that is in the file right now — highlights are never fabricated;
//   * when no query term occurs in the current text (the note was edited after
//     it was indexed, or the query carries no highlightable terms) the window
//     is still shown but nothing is marked;
//   * when the file cannot be read the excerpt is empty rather than guessed.

import { type ExcerptSegment, sanitizeExcerptText } from "./excerpt";

/** Characters of file text shown around the first match. */
export const EXCERPT_WINDOW_CHARACTERS = 240;
/** Characters of lead-in kept before the match inside that window. */
export const EXCERPT_LEAD_CHARACTERS = 80;
/** Hard cap on the assembled excerpt, far below the transport bound. */
export const MAX_HYDRATED_EXCERPT_CHARACTERS = 512;
/** Upper bound on distinct highlight terms taken from one query. */
export const MAX_HIGHLIGHT_TERMS = 32;
/**
 * Hard cap on the source characters folded while hunting for the anchor of one
 * section. `snippet()` was O(window); this runs on the Obsidian main thread, so
 * it must not become O(file) for every hit. Past the cap the window opens at
 * the top of the section with nothing marked — the same truthful degradation as
 * "the note no longer contains the term".
 */
export const MAX_ANCHOR_SCAN_CHARACTERS = 256 * 1024;

const ELLIPSIS = "…";
const BOUNDARY_SLACK = 40;
const MAX_MATCHES = 256;
/** Source characters folded at once while hunting for the anchor. */
const ANCHOR_SCAN_WINDOW = 8 * 1024;
/** Source characters re-scanned so a term straddling a window edge is still found. */
const ANCHOR_SCAN_OVERLAP = 256;
/** Source characters scanned for a closing frontmatter fence before giving up. */
const FRONTMATTER_SCAN_CHARACTERS = 64 * 1024;
const NON_ASCII = /[^\u0000-\u007f]/u;
const FTS5_OPERATORS = new Set(["AND", "OR", "NOT", "NEAR"]);
const HEADING_PATTERN = /^ {0,3}(#{1,6})\s+(.*?)\s*#*\s*$/u;

export type ExcerptSource =
  | { kind: "text"; text: string }
  | { kind: "unavailable"; reason: "missing" | "oversized" | "unstable" | "unreadable" };

/**
 * Deterministically derives highlightable terms from the raw query text.
 *
 * The Worker cannot ship the Rust query plan's terms without changing the
 * frozen hit shape, so the host re-derives them. This is presentation only:
 * a term is highlighted solely where it literally occurs in the file text, so
 * an over- or under-approximation here can never assert something false.
 */
export function extractHighlightTerms(query: string): readonly string[] {
  const terms: string[] = [];
  const seen = new Set<string>();
  // Column-filter sets name index columns, not body words.
  const stripped = query.replace(/\{[^}]*\}/gu, " ");
  for (const raw of tokenizeQuery(stripped)) {
    if (FTS5_OPERATORS.has(raw.text) && !raw.quoted) continue;
    const body = raw.quoted ? raw.text : stripFieldPrefix(raw.text);
    for (const piece of body.split(/[^\p{L}\p{N}]+/u)) {
      if (piece.length === 0) continue;
      const folded = foldText(piece);
      if (folded.length === 0 || seen.has(folded)) continue;
      seen.add(folded);
      terms.push(folded);
      if (terms.length >= MAX_HIGHLIGHT_TERMS) return terms;
    }
  }
  return terms;
}

/**
 * Builds the rendered excerpt segments for one hit from the current file text.
 * An unavailable source yields an empty excerpt: absent, never invented.
 *
 * Nothing here is allowed to be O(file) in allocations. `sanitizeExcerptText`
 * is a 1:1 BMP code-unit swap (U+E000/U+E001 → U+FFFD), so it never moves an
 * offset and can be applied to the rendered window alone instead of to the
 * whole note; and no highlight term can contain either codepoint or U+FFFD,
 * because `extractHighlightTerms` splits on everything outside `\p{L}\p{N}`.
 * Matching the raw text and rendering the sanitized window therefore agree.
 */
export function hydrateExcerpt(
  source: ExcerptSource,
  terms: readonly string[],
  headingPath: readonly string[],
): readonly ExcerptSegment[] {
  if (source.kind !== "text") return [];
  const text = source.text;
  const section = locateSection(text, headingPath);
  const scoped = text.slice(section.start, section.end);
  if (isBlank(scoped)) return [];

  const anchorMatch = findAnchor(scoped, terms);
  const anchor = anchorMatch?.start ?? 0;
  const anchorEnd = anchorMatch?.end ?? 0;

  const window = locateWindow(scoped, anchor, anchorEnd);
  const slice = sanitizeExcerptText(scoped.slice(window.start, window.end));
  const sliceFold = foldWithMap(slice);
  const matches = findMatches(sliceFold.folded, terms);

  const segments: ExcerptSegment[] = [];
  let cursor = 0;
  for (const match of matches) {
    const start = sliceFold.map[match.start] ?? slice.length;
    const end = sourceEnd(sliceFold.map, match.end, slice.length);
    if (start < cursor) continue;
    if (start > cursor) segments.push({ text: slice.slice(cursor, start), highlighted: false });
    segments.push({ text: slice.slice(start, end), highlighted: true });
    cursor = end;
  }
  if (cursor < slice.length) {
    segments.push({ text: slice.slice(cursor), highlighted: false });
  }

  return finalizeSegments(segments, window.start > 0, window.end < scoped.length);
}

export type ExcerptHydrator = (
  path: string,
  source: ExcerptSource,
  headingPath: readonly string[],
) => readonly ExcerptSegment[];

/**
 * One search's worth of hydration.
 *
 * `hydrateExcerpt` is pure in (file text, terms, heading path), so every chunk
 * of one note that shares a heading path yields byte-identical segments. A
 * 100-hit page over a handful of large notes would otherwise re-locate and
 * re-fold the same section once per hit, synchronously, on the UI thread; the
 * memo collapses that to once per distinct (path, heading path).
 */
export function createExcerptHydrator(terms: readonly string[]): ExcerptHydrator {
  const cache = new Map<string, readonly ExcerptSegment[]>();
  return (path, source, headingPath) => {
    const key = `${path}\u0000${headingPath.join("\u0001")}`;
    const cached = cache.get(key);
    if (cached !== undefined) return cached;
    // Frozen because the same array instance is handed to several hits.
    const segments = Object.freeze(hydrateExcerpt(source, terms, headingPath));
    cache.set(key, segments);
    return segments;
  };
}

interface QueryToken {
  text: string;
  quoted: boolean;
}

function tokenizeQuery(query: string): QueryToken[] {
  const tokens: QueryToken[] = [];
  let index = 0;
  while (index < query.length) {
    const character = query[index]!;
    if (character === "\"") {
      const close = query.indexOf("\"", index + 1);
      if (close < 0) {
        tokens.push({ text: query.slice(index + 1), quoted: true });
        break;
      }
      tokens.push({ text: query.slice(index + 1, close), quoted: true });
      index = close + 1;
      continue;
    }
    if (/\s/u.test(character)) {
      index += 1;
      continue;
    }
    let end = index;
    while (end < query.length && !/\s/u.test(query[end]!) && query[end] !== "\"") end += 1;
    tokens.push({ text: query.slice(index, end), quoted: false });
    index = end;
  }
  return tokens;
}

function stripFieldPrefix(token: string): string {
  let rest = token;
  for (;;) {
    const match = /^[\p{L}_][\p{L}\p{N}_]*:/u.exec(rest);
    if (!match) return rest;
    rest = rest.slice(match[0].length);
  }
}

/** Lowercases and strips combining marks so `cafe` matches `café`, as unicode61 does. */
export function foldText(value: string): string {
  return value.normalize("NFD").replace(/\p{M}+/gu, "").toLowerCase();
}

interface FoldedText {
  folded: string;
  /** For each folded character, the index of the source character it came from. */
  map: number[];
}

function foldWithMap(value: string): FoldedText {
  const folded: string[] = [];
  const map: number[] = [];
  let index = 0;
  while (index < value.length) {
    const unit = value.charCodeAt(index);
    if (unit < 0x80) {
      // ASCII never decomposes and carries no combining marks, so its fold is
      // exactly `toLowerCase` and costs nothing; only the rest pays for NFD.
      folded.push(unit >= 0x41 && unit <= 0x5a ? String.fromCharCode(unit + 0x20) : value[index]!);
      map.push(index);
      index += 1;
      continue;
    }
    const codePoint = value.codePointAt(index)!;
    const character = String.fromCodePoint(codePoint);
    for (const piece of foldText(character)) {
      folded.push(piece);
      map.push(index);
    }
    index += character.length;
  }
  return { folded: folded.join(""), map };
}

function sourceEnd(map: readonly number[], foldedEnd: number, length: number): number {
  return foldedEnd < map.length ? map[foldedEnd]! : length;
}

interface Match {
  start: number;
  end: number;
}

/**
 * Locates the first occurrence of any term as a bounded, chunked scan.
 *
 * Only `ANCHOR_SCAN_WINDOW` source characters are folded at a time, and the
 * scan stops at the first hit, so a match near the top of a large note costs
 * kilobytes rather than megabytes. Consecutive chunks overlap so a term
 * straddling a chunk edge is still found, and the total scan is capped: past
 * `MAX_ANCHOR_SCAN_CHARACTERS` the section is reported as unmatched, which is
 * a truthful "nothing to mark", never an invented highlight.
 */
function findAnchor(scoped: string, terms: readonly string[]): Match | null {
  if (terms.length === 0) return null;
  const limit = Math.min(scoped.length, MAX_ANCHOR_SCAN_CHARACTERS);
  let from = 0;
  while (from < limit) {
    let end = Math.min(limit, from + ANCHOR_SCAN_WINDOW);
    // The fold walks code points, so never cut a surrogate pair in half.
    if (end < limit && isHighSurrogate(scoped.charCodeAt(end - 1))) end -= 1;
    const chunk = scoped.slice(from, end);
    if (NON_ASCII.test(chunk)) {
      const fold = foldWithMap(chunk);
      const match = firstMatch(fold.folded, terms);
      if (match) {
        return {
          start: from + (fold.map[match.start] ?? 0),
          end: from + sourceEnd(fold.map, match.end, chunk.length),
        };
      }
    } else {
      // ASCII folds to `toLowerCase()` exactly — NFD is a no-op, there are no
      // combining marks, and every mapping is one code unit to one — so the
      // offset map is the identity and the whole chunk can be folded natively.
      const match = firstMatch(chunk.toLowerCase(), terms);
      if (match) return { start: from + match.start, end: from + match.end };
    }
    if (end >= limit) return null;
    let next = Math.max(from + 1, end - ANCHOR_SCAN_OVERLAP);
    if (isLowSurrogate(scoped.charCodeAt(next))) next -= 1;
    from = next;
  }
  return null;
}

function isHighSurrogate(unit: number): boolean {
  return unit >= 0xd800 && unit <= 0xdbff;
}

function isLowSurrogate(unit: number): boolean {
  return unit >= 0xdc00 && unit <= 0xdfff;
}

/** Whitespace-only test that stops at the first content character. */
function isBlank(text: string): boolean {
  for (let index = 0; index < text.length; index += 1) {
    if (!/\s/u.test(text[index]!)) return false;
  }
  return true;
}

function firstMatch(folded: string, terms: readonly string[]): Match | null {
  let best: Match | null = null;
  for (const term of terms) {
    const at = folded.indexOf(term);
    if (at < 0) continue;
    if (!best || at < best.start || (at === best.start && term.length > best.end - best.start)) {
      best = { start: at, end: at + term.length };
    }
  }
  return best;
}

function findMatches(folded: string, terms: readonly string[]): Match[] {
  const found: Match[] = [];
  for (const term of terms) {
    let from = 0;
    for (;;) {
      const at = folded.indexOf(term, from);
      if (at < 0) break;
      found.push({ start: at, end: at + term.length });
      from = at + term.length;
      if (found.length >= MAX_MATCHES) break;
    }
    if (found.length >= MAX_MATCHES) break;
  }
  found.sort((left, right) => left.start - right.start || right.end - left.end);
  const merged: Match[] = [];
  for (const match of found) {
    const last = merged.at(-1);
    if (last && match.start <= last.end) {
      if (match.end > last.end) last.end = match.end;
      continue;
    }
    merged.push({ ...match });
  }
  return merged;
}

function locateWindow(text: string, anchor: number, anchorEnd: number): Match {
  let start = Math.max(0, anchor - EXCERPT_LEAD_CHARACTERS);
  if (start > 0) {
    const boundary = nextBoundary(text, start, Math.min(anchor, start + BOUNDARY_SLACK));
    start = boundary;
  }
  let end = Math.min(text.length, start + EXCERPT_WINDOW_CHARACTERS);
  if (end < anchorEnd) end = Math.min(text.length, anchorEnd);
  if (end < text.length) {
    const boundary = previousBoundary(text, end, Math.max(anchorEnd, end - BOUNDARY_SLACK));
    end = boundary;
  }
  return { start, end };
}

function nextBoundary(text: string, from: number, limit: number): number {
  for (let index = from; index < limit; index += 1) {
    if (/\s/u.test(text[index]!)) {
      let next = index;
      while (next < limit && /\s/u.test(text[next]!)) next += 1;
      return next;
    }
  }
  return from;
}

function previousBoundary(text: string, from: number, limit: number): number {
  for (let index = from; index > limit; index -= 1) {
    if (/\s/u.test(text[index - 1]!)) return index - 1;
  }
  return from;
}

function finalizeSegments(
  segments: readonly ExcerptSegment[],
  truncatedStart: boolean,
  truncatedEnd: boolean,
): readonly ExcerptSegment[] {
  const bounded: ExcerptSegment[] = [];
  let total = 0;
  let clipped = false;
  for (const segment of segments) {
    if (segment.text.length === 0) continue;
    const room = MAX_HYDRATED_EXCERPT_CHARACTERS - total;
    if (room <= 0) {
      clipped = true;
      break;
    }
    if (segment.text.length > room) {
      bounded.push({ text: segment.text.slice(0, room), highlighted: segment.highlighted });
      total += room;
      clipped = true;
      break;
    }
    bounded.push(segment);
    total += segment.text.length;
  }

  const first = bounded[0];
  if (first && !first.highlighted) first.text = first.text.replace(/^\s+/u, "");
  const last = bounded.at(-1);
  if (last && !last.highlighted) last.text = last.text.replace(/\s+$/u, "");

  const trimmed = bounded.filter((segment) => segment.text.length > 0);
  if (trimmed.length === 0) return [];
  if (truncatedStart) trimmed.unshift({ text: ELLIPSIS, highlighted: false });
  if (truncatedEnd || clipped) trimmed.push({ text: ELLIPSIS, highlighted: false });
  return trimmed;
}

/**
 * Bounds the region of the file the excerpt window may open in.
 *
 * The removed `snippet()` read the indexed `content` column, which never
 * contained YAML frontmatter, so an unanchored window must not open on
 * `---\ntitle: …` either — the body start is skipped past a leading
 * frontmatter block.
 */
function locateSection(text: string, headingPath: readonly string[]): Match {
  const body = bodyStart(text);
  const whole = { start: body, end: text.length };
  if (headingPath.length === 0) return whole;
  let cursor = body;
  let level = 0;
  let start = body;
  for (const heading of headingPath) {
    const found = findHeading(text, cursor, heading);
    if (!found) return whole;
    cursor = found.contentStart;
    start = found.contentStart;
    level = found.level;
  }
  return { start, end: findSectionEnd(text, start, level) };
}

/**
 * Offset of the first body character: past a closing `---`/`...` fence when the
 * note opens with a YAML frontmatter block, otherwise zero. An unterminated or
 * implausibly long opening fence is not frontmatter, so the whole file is body.
 */
function bodyStart(text: string): number {
  if (!text.startsWith("---")) return 0;
  const opening = text.indexOf("\n");
  if (opening < 0 || text.slice(3, opening).trim().length !== 0) return 0;
  const limit = Math.min(text.length, FRONTMATTER_SCAN_CHARACTERS);
  let cursor = opening + 1;
  while (cursor < limit) {
    const breakAt = text.indexOf("\n", cursor);
    const stop = breakAt < 0 ? text.length : breakAt;
    const fence = text.slice(cursor, stop).trimEnd();
    if (fence === "---" || fence === "...") {
      return breakAt < 0 ? text.length : breakAt + 1;
    }
    if (breakAt < 0) return 0;
    cursor = breakAt + 1;
  }
  return 0;
}

interface HeadingLine {
  level: number;
  contentStart: number;
}

function findHeading(text: string, from: number, heading: string): HeadingLine | null {
  const wanted = heading.trim();
  for (const line of headingLines(text, from)) {
    if (line.title !== wanted) continue;
    return { level: line.level, contentStart: line.end };
  }
  return null;
}

function findSectionEnd(text: string, from: number, level: number): number {
  for (const line of headingLines(text, from)) {
    if (line.level <= level) return line.start;
  }
  return text.length;
}

interface HeadingScan {
  level: number;
  title: string;
  start: number;
  end: number;
}

/**
 * Yields only the ATX heading lines. Ordinary body lines are rejected by an
 * index comparison instead of being sliced out of the note, so scanning a large
 * file allocates per heading rather than per line.
 */
function* headingLines(text: string, from: number): Generator<HeadingScan> {
  let start = from;
  while (start <= text.length) {
    const breakAt = text.indexOf("\n", start);
    const stop = breakAt < 0 ? text.length : breakAt;
    if (opensHeading(text, start, stop)) {
      const match = HEADING_PATTERN.exec(text.slice(start, stop));
      if (match) {
        yield {
          level: match[1]!.length,
          title: match[2]!.trim(),
          start,
          end: Math.min(text.length, stop + 1),
        };
      }
    }
    if (breakAt < 0) return;
    start = breakAt + 1;
  }
}

/** Cheap prefilter matching the `^ {0,3}#` prefix of `HEADING_PATTERN`. */
function opensHeading(text: string, start: number, stop: number): boolean {
  let index = start;
  while (index < stop && index - start < 3 && text[index] === " ") index += 1;
  return index < stop && text[index] === "#";
}
