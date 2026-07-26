// SPDX-FileCopyrightText: 2026 cybersader
// SPDX-License-Identifier: GPL-3.0-only

import { describe, expect, it } from "vitest";

import { RESERVED_EXCERPT_MARKERS } from "../src/excerpt";
import {
  MAX_ANCHOR_SCAN_CHARACTERS,
  MAX_HYDRATED_EXCERPT_CHARACTERS,
  createExcerptHydrator,
  extractHighlightTerms,
  hydrateExcerpt,
} from "../src/hydrate-excerpt";

function text(value: string) {
  return { kind: "text" as const, text: value };
}

function rendered(segments: readonly { text: string }[]): string {
  return segments.map((segment) => segment.text).join("");
}

function highlighted(segments: readonly { text: string; highlighted: boolean }[]): string[] {
  return segments.filter((segment) => segment.highlighted).map((segment) => segment.text);
}

describe("extractHighlightTerms", () => {
  it("folds case and diacritics so indexed matches stay highlightable", () => {
    expect(extractHighlightTerms("Café NAÏVE")).toEqual(["cafe", "naive"]);
  });

  it("drops field prefixes, column filter sets, wildcards and FTS5 operators", () => {
    expect(extractHighlightTerms("title:\"IIA 2 line guide\" OR content:cache*")).toEqual([
      "iia",
      "2",
      "line",
      "guide",
      "cache",
    ]);
    expect(extractHighlightTerms("{filename stem} : (quasar)")).toEqual(["quasar"]);
  });

  it("splits identifiers on non-alphanumerics the way unicode61 tokenizes them", () => {
    expect(extractHighlightTerms("cve-2026-1234")).toEqual(["cve", "2026", "1234"]);
  });

  it("keeps lowercase words that merely look like operators", () => {
    expect(extractHighlightTerms("gold or silver")).toEqual(["gold", "or", "silver"]);
  });

  it("returns nothing highlightable for a query with no word characters", () => {
    expect(extractHighlightTerms("--- ***")).toEqual([]);
  });
});

describe("hydrateExcerpt", () => {
  it("windows the current file text around the first match and marks it", () => {
    const segments = hydrateExcerpt(
      text("intro line\nportable quasar cache storage\ntrailing line"),
      extractHighlightTerms("quasar"),
      [],
    );
    expect(highlighted(segments)).toEqual(["quasar"]);
    expect(rendered(segments)).toContain("portable quasar cache storage");
  });

  it("matches through diacritic folding without corrupting the rendered text", () => {
    const segments = hydrateExcerpt(
      text("the café is naïve about 東京"),
      extractHighlightTerms("cafe"),
      [],
    );
    expect(highlighted(segments)).toEqual(["café"]);
    expect(rendered(segments)).toContain("naïve about 東京");
  });

  it("scopes the window to the deepest heading in the hit path", () => {
    const note = [
      "# Alpha",
      "alpha body needle",
      "## Beta",
      "beta body needle",
      "## Gamma",
      "gamma body needle",
    ].join("\n");
    const segments = hydrateExcerpt(text(note), extractHighlightTerms("needle"), ["Alpha", "Beta"]);
    expect(rendered(segments)).toContain("beta body");
    expect(rendered(segments)).not.toContain("alpha body");
    expect(rendered(segments)).not.toContain("gamma body");
  });

  it("falls back to the whole file when the heading is no longer present", () => {
    const segments = hydrateExcerpt(
      text("# Renamed\nbody needle here"),
      extractHighlightTerms("needle"),
      ["Old Heading"],
    );
    expect(highlighted(segments)).toEqual(["needle"]);
  });

  // Truthfulness: the file may have been edited after the generation was
  // published. Show what the file says now, but never claim a highlight.
  it("never fabricates a highlight when no term occurs in the current text", () => {
    const segments = hydrateExcerpt(
      text("this note was rewritten and no longer mentions the term"),
      extractHighlightTerms("quasar"),
      [],
    );
    expect(highlighted(segments)).toEqual([]);
    expect(rendered(segments)).toContain("this note was rewritten");
  });

  it("marks nothing when the query carries no highlightable terms", () => {
    const segments = hydrateExcerpt(text("plain body text"), [], []);
    expect(highlighted(segments)).toEqual([]);
    expect(rendered(segments)).toContain("plain body text");
  });

  it.each([
    ["missing", { kind: "unavailable", reason: "missing" } as const],
    ["oversized", { kind: "unavailable", reason: "oversized" } as const],
    ["unstable", { kind: "unavailable", reason: "unstable" } as const],
    ["unreadable", { kind: "unavailable", reason: "unreadable" } as const],
  ])("returns an empty excerpt rather than inventing one when the file is %s", (_label, source) => {
    expect(hydrateExcerpt(source, extractHighlightTerms("quasar"), [])).toEqual([]);
  });

  it("returns an empty excerpt for an empty file", () => {
    expect(hydrateExcerpt(text("   \n\n"), extractHighlightTerms("quasar"), [])).toEqual([]);
  });

  // Relocated from the index: excerpt text now comes from the vault file, so
  // the anti-forgery property has to hold where that text is read.
  it("neutralizes source-owned private markers in hydrated vault text", () => {
    const forged = `${RESERVED_EXCERPT_MARKERS[0]} forged ${RESERVED_EXCERPT_MARKERS[1]} quasar`;
    const segments = hydrateExcerpt(text(forged), extractHighlightTerms("quasar"), []);
    expect(highlighted(segments)).toEqual(["quasar"]);
    for (const marker of RESERVED_EXCERPT_MARKERS) {
      expect(rendered(segments)).not.toContain(marker);
    }
    expect(rendered(segments)).toContain("forged");
  });

  it("bounds the excerpt and ellipsizes both truncated ends", () => {
    const filler = "lorem ipsum dolor sit amet ".repeat(200);
    const segments = hydrateExcerpt(
      text(`${filler}quasar ${filler}`),
      extractHighlightTerms("quasar"),
      [],
    );
    expect(highlighted(segments)).toEqual(["quasar"]);
    expect(rendered(segments).length).toBeLessThanOrEqual(
      MAX_HYDRATED_EXCERPT_CHARACTERS + 2,
    );
    expect(segments[0]!.text).toBe("…");
    expect(segments.at(-1)!.text).toBe("…");
  });

  it("is deterministic for the same file text and query", () => {
    const note = "alpha quasar beta quasar gamma";
    const terms = extractHighlightTerms("quasar");
    expect(hydrateExcerpt(text(note), terms, [])).toEqual(hydrateExcerpt(text(note), terms, []));
    expect(highlighted(hydrateExcerpt(text(note), terms, []))).toEqual(["quasar", "quasar"]);
  });

  // The removed snippet() read the indexed `content` column, which never held
  // frontmatter. An unanchored window must not open on `---\ntitle: …` either.
  it("opens an unanchored window below a frontmatter block", () => {
    const note = [
      "---",
      "title: Secret Ledger",
      "tags: [private]",
      "---",
      "the body of the note starts here",
    ].join("\n");
    const segments = hydrateExcerpt(text(note), extractHighlightTerms("quasar"), []);
    expect(rendered(segments)).toBe("the body of the note starts here");
  });

  it("still shows the whole file when an opening fence is never closed", () => {
    const note = "---\nnot: really frontmatter\nbody line without a closing fence";
    const segments = hydrateExcerpt(text(note), [], []);
    expect(rendered(segments).startsWith("---")).toBe(true);
  });

  // Hydration is synchronous work on the Obsidian main thread. Locating the
  // anchor must not become O(file): past the scan bound the window opens at the
  // top of the section with nothing marked, which is the same truthful
  // degradation as "the note no longer contains the term".
  it("bounds the anchor scan instead of folding an entire large note", () => {
    const filler = "lorem ipsum dolor sit amet ".repeat(
      Math.ceil((MAX_ANCHOR_SCAN_CHARACTERS + 4_096) / 27),
    );
    const note = `${filler}quasar tail`;
    expect(note.length).toBeGreaterThan(MAX_ANCHOR_SCAN_CHARACTERS);

    const started = performance.now();
    const segments = hydrateExcerpt(text(note), extractHighlightTerms("quasar"), []);
    const elapsed = performance.now() - started;

    expect(highlighted(segments)).toEqual([]);
    expect(rendered(segments)).toContain("lorem ipsum");
    expect(elapsed).toBeLessThan(500);
  });

  // Beyond the window an unanchored excerpt would show, so only the anchor
  // scan itself can produce these: they pin both of its folding paths.
  it("case-folds while locating an anchor past the unanchored window", () => {
    const filler = "lorem ipsum dolor sit amet ".repeat(80);
    const segments = hydrateExcerpt(
      text(`${filler}PORTABLE QUASAR CACHE tail`),
      extractHighlightTerms("quasar"),
      [],
    );
    expect(highlighted(segments)).toEqual(["QUASAR"]);
    expect(rendered(segments)).toContain("PORTABLE QUASAR CACHE");
  });

  it("diacritic-folds while locating an anchor past the unanchored window", () => {
    const filler = "lorem ipsum dolor sit amet ".repeat(80);
    const segments = hydrateExcerpt(
      text(`${filler}le café est ouvert`),
      extractHighlightTerms("cafe"),
      [],
    );
    expect(highlighted(segments)).toEqual(["café"]);
    expect(rendered(segments)).toContain("est ouvert");
  });

  it("finds a match that straddles two anchor scan windows", () => {
    // 8 KiB windows: place the term so it cannot sit inside a single one.
    const lead = "x".repeat(8 * 1024 - 3);
    const segments = hydrateExcerpt(
      text(`${lead}quasar tail`),
      extractHighlightTerms("quasar"),
      [],
    );
    expect(highlighted(segments)).toEqual(["quasar"]);
  });
});

describe("createExcerptHydrator", () => {
  it("hydrates one file and heading path once and reuses the result", () => {
    const note = "alpha quasar beta";
    let reads = 0;
    const source = {
      kind: "text" as const,
      get text() {
        reads += 1;
        return note;
      },
    };
    const hydrate = createExcerptHydrator(extractHighlightTerms("quasar"));

    const first = hydrate("note.md", source, []);
    const second = hydrate("note.md", source, []);

    expect(reads).toBe(1);
    expect(second).toBe(first);
    expect(highlighted(first)).toEqual(["quasar"]);
  });

  it("keeps distinct paths and heading paths apart", () => {
    const note = ["# Alpha", "alpha body needle", "## Beta", "beta body needle"].join("\n");
    const hydrate = createExcerptHydrator(extractHighlightTerms("needle"));

    const whole = hydrate("note.md", text(note), []);
    const scoped = hydrate("note.md", text(note), ["Alpha", "Beta"]);
    const other = hydrate("other.md", text("other needle body"), []);

    expect(rendered(whole)).toContain("alpha body");
    expect(rendered(scoped)).not.toContain("alpha body");
    expect(rendered(other)).toContain("other needle body");
  });
});
