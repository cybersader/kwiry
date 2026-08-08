// SPDX-FileCopyrightText: 2026 cybersader
// SPDX-License-Identifier: GPL-3.0-only

import { describe, expect, it } from "vitest";

import type { ExtractionCoverage, SourceFormat, SourceLocator } from "../src/api";
import type {
  BackendProfile,
  BackendSearchHit,
  CandidateWindowFacts,
  SearchExecution,
} from "../src/backend";
import {
  groupedSearchHitLimit,
  groupSearchExecution,
} from "../src/grouped-search";

interface HitOptions {
  profile?: BackendProfile;
  backendInstanceId?: string;
  vaultId?: string;
  score?: number;
  format?: SourceFormat;
  coverage?: ExtractionCoverage;
  locator?: SourceLocator | null;
  headingPath?: string[];
}

function hit(chunkId: string, path: string, options: HitOptions = {}): BackendSearchHit {
  return {
    chunk_id: chunkId,
    vault_id: options.vaultId ?? "vault-1",
    path,
    format: options.format ?? "markdown",
    coverage: options.coverage ?? "indexed-complete",
    locator: options.locator ?? null,
    heading_path: options.headingPath ?? [`Heading ${chunkId}`],
    score: options.score ?? 100,
    excerpt: [
      { text: `before ${chunkId}`, highlighted: false },
      { text: chunkId, highlighted: true },
    ],
    frontmatter: { title: `Title ${chunkId}`, tags: [chunkId] },
    origin: {
      profile: options.profile ?? "daemon",
      backendInstanceId: options.backendInstanceId ?? "backend-1",
      vaultId: options.vaultId ?? "vault-1",
    },
  };
}

function execution(
  hits: BackendSearchHit[],
  candidateWindow: CandidateWindowFacts = {
    state: "exhausted",
    candidateCount: hits.length,
    candidateLimit: 512,
  },
): SearchExecution {
  return {
    backend: {
      profile: "daemon",
      instanceId: "backend-1",
      label: "Daemon",
      boundVaultId: "vault-1",
    },
    requestedMode: "lexical",
    effectiveMode: "lexical",
    generation: "generation-1",
    candidateWindow,
    response: { hits, next_cursor: null },
  };
}

describe("groupSearchExecution", () => {
  it("groups interleaved sections by first raw occurrence without changing section order", () => {
    const a1 = hit("A1", "a.md", { score: 10 });
    const b1 = hit("B1", "b.md", { score: 9 });
    const a2 = hit("A2", "a.md", { score: 8 });
    const c1 = hit("C1", "c.md", { score: 7 });

    const grouped = groupSearchExecution(execution([a1, b1, a2, c1]), 100);

    expect(grouped.groups.map((group) => group.source.path)).toEqual(["a.md", "b.md", "c.md"]);
    expect(grouped.groups.map((group) => group.firstRawIndex)).toEqual([0, 1, 3]);
    expect(grouped.groups[0]?.sections).toEqual([a1, a2]);
    expect(grouped.groups[0]?.representative).toBe(a1);
    expect(grouped.groups[0]?.representative).toBe(grouped.groups[0]?.sections[0]);
    expect(grouped.groups[0]?.observedSectionCount).toBe(2);
  });

  it("does not aggregate-boost a later source with more sections or summed score", () => {
    const a1 = hit("A1", "a.md", { score: 10 });
    const b1 = hit("B1", "b.md", { score: 9 });
    const b2 = hit("B2", "b.md", { score: 8 });

    const grouped = groupSearchExecution(execution([a1, b1, b2]), 100);

    expect(grouped.groups.map((group) => group.source.path)).toEqual(["a.md", "b.md"]);
    expect(grouped.groups.map((group) => group.representative)).toEqual([a1, b1]);
    expect(grouped.groups.map((group) => group.firstRawIndex)).toEqual([0, 1]);
    expect(grouped.groups.map((group) => group.observedSectionCount)).toEqual([1, 2]);
  });

  it("preserves authoritative same-source order when a later section scores higher", () => {
    const a1 = hit("A1", "a.md", { score: 1 });
    const b1 = hit("B1", "b.md", { score: 7 });
    const a2 = hit("A2", "a.md", { score: 20 });

    const grouped = groupSearchExecution(execution([a1, b1, a2]), 100);

    expect(grouped.groups[0]?.sections).toEqual([a1, a2]);
    expect(grouped.groups[0]?.sections.map((section) => section.score)).toEqual([1, 20]);
    expect(grouped.groups[0]?.sections[0]).toBe(a1);
    expect(grouped.groups[0]?.sections[1]).toBe(a2);
  });

  it("retains every leading section from one source and still discovers a source inside rank 100", () => {
    const leading = Array.from({ length: 99 }, (_, index) =>
      hit(`A${index + 1}`, "a.md", { score: 100 - index }));
    const b1 = hit("B1", "b.md", { score: 1 });

    const grouped = groupSearchExecution(execution([...leading, b1]), 2);

    expect(grouped.groups.map((group) => group.source.path)).toEqual(["a.md", "b.md"]);
    expect(grouped.groups[0]?.sections).toEqual(leading);
    expect(grouped.groups[0]?.observedSectionCount).toBe(99);
    expect(grouped.groups[1]?.sections).toEqual([b1]);
    expect(grouped.facts.returnedSectionCount).toBe(100);
  });

  it("does not invent a source below the returned 100-hit window", () => {
    const rawRanking = [
      ...Array.from({ length: 100 }, (_, index) =>
        hit(`A${index + 1}`, "a.md", { score: 101 - index })),
      hit("B1", "below-window.md", { score: 0 }),
    ];
    const candidateWindow: CandidateWindowFacts = {
      state: "candidate_limit_reached",
      candidateCount: 512,
      candidateLimit: 512,
    };

    const grouped = groupSearchExecution(
      execution(rawRanking.slice(0, groupedSearchHitLimit(100)), candidateWindow),
      100,
    );

    expect(grouped.groups.map((group) => group.source.path)).toEqual(["a.md"]);
    expect(grouped.groups.flatMap((group) => group.sections)).not.toContain(rawRanking[100]);
    expect(grouped.facts).toMatchObject({
      returnedSectionCount: 100,
      observedSourceCount: 1,
      displayedSourceCount: 1,
      omittedObservedSourceCount: 0,
      candidateWindow: { state: "candidate_limit_reached" },
    });
  });

  it("keeps equal paths separate across every qualified-origin axis", () => {
    const hits = [
      hit("base", "same.md"),
      hit("profile", "same.md", { profile: "in_plugin" }),
      hit("backend", "same.md", { backendInstanceId: "backend-2" }),
      hit("vault", "same.md", { vaultId: "vault-2" }),
    ];

    const grouped = groupSearchExecution(execution(hits), 100);

    expect(grouped.groups).toHaveLength(4);
    expect(grouped.groups.map((group) => group.source)).toEqual([
      {
        profile: "daemon",
        backendInstanceId: "backend-1",
        vaultId: "vault-1",
        path: "same.md",
      },
      {
        profile: "in_plugin",
        backendInstanceId: "backend-1",
        vaultId: "vault-1",
        path: "same.md",
      },
      {
        profile: "daemon",
        backendInstanceId: "backend-2",
        vaultId: "vault-1",
        path: "same.md",
      },
      {
        profile: "daemon",
        backendInstanceId: "backend-1",
        vaultId: "vault-2",
        path: "same.md",
      },
    ]);
  });

  it("preserves hit references, scores, locators, formats, excerpts, and metadata verbatim", () => {
    const a1 = hit("A1", "a.base", {
      format: "base",
      coverage: "indexed-partial",
      locator: { kind: "base_view", view: "By status" },
      headingPath: ["Records", "Open"],
      score: 37.25,
    });
    const b1 = hit("B1", "b.canvas", {
      format: "canvas",
      headingPath: [],
      score: -4.5,
    });
    const a2 = hit("A2", "a.base", {
      format: "base",
      coverage: "indexed-partial",
      locator: { kind: "base_view", view: "Archive" },
      headingPath: ["Records", "Closed"],
      score: 11.75,
    });
    const rawHits = [a1, b1, a2];

    const grouped = groupSearchExecution(execution(rawHits), 100);
    const regroupedRawOrder = grouped.groups.flatMap((group) => group.sections);

    expect(grouped.groups[0]?.sections[0]).toBe(a1);
    expect(grouped.groups[0]?.sections[1]).toBe(a2);
    expect(grouped.groups[1]?.sections[0]).toBe(b1);
    expect(regroupedRawOrder).toEqual([a1, a2, b1]);
    expect(a1).toMatchObject({
      score: 37.25,
      locator: { kind: "base_view", view: "By status" },
      format: "base",
      coverage: "indexed-partial",
      heading_path: ["Records", "Open"],
      frontmatter: { title: "Title A1", tags: ["A1"] },
    });
    expect(a1.excerpt).toEqual([
      { text: "before A1", highlighted: false },
      { text: "A1", highlighted: true },
    ]);
    expect(rawHits).toEqual([a1, b1, a2]);
  });

  it("keeps every PDF page distinct under one source group", () => {
    // A PDF has no heading path, so the page locator is the only thing that
    // distinguishes two matches in the same document. Grouping must neither
    // merge them nor let the representative's page leak onto a drilled row.
    const page19 = hit("P19", "papers/report.pdf", {
      format: "pdf",
      locator: { kind: "pdf_page", page: 19 },
      headingPath: [],
      score: 88,
    });
    const page4 = hit("P4", "papers/report.pdf", {
      format: "pdf",
      locator: { kind: "pdf_page", page: 4 },
      headingPath: [],
      score: 12,
    });

    const grouped = groupSearchExecution(execution([page19, page4]), 100);

    expect(grouped.groups).toHaveLength(1);
    const group = grouped.groups[0]!;
    expect(group.source.path).toBe("papers/report.pdf");
    // The representative is the highest-ranked hit, so opening the source row
    // lands on the best-matching page rather than page one.
    expect(group.representative).toBe(page19);
    expect(group.sections).toEqual([page19, page4]);
    expect(group.sections.map((section) => section.locator)).toEqual([
      { kind: "pdf_page", page: 19 },
      { kind: "pdf_page", page: 4 },
    ]);
    expect(group.observedSectionCount).toBe(2);
  });

  it("does not use format or coverage as grouping or ordering inputs", () => {
    const baselineHits = [
      hit("A1", "a.md"),
      hit("B1", "b.md"),
      hit("A2", "a.md"),
    ];
    const presentationMutatedHits = [
      { ...baselineHits[0]!, format: "pdf", coverage: "unreadable" } as BackendSearchHit,
      { ...baselineHits[1]!, format: "text", coverage: "indexed-partial" } as BackendSearchHit,
      { ...baselineHits[2]!, format: "docx", coverage: "quarantined" } as BackendSearchHit,
    ];

    const baseline = groupSearchExecution(execution(baselineHits), 100);
    const mutated = groupSearchExecution(execution(presentationMutatedHits), 100);

    const projection = (result: typeof baseline) => result.groups.map((group) => ({
      path: group.source.path,
      chunkIds: group.sections.map((section) => section.chunk_id),
      firstRawIndex: group.firstRawIndex,
    }));
    expect(projection(mutated)).toEqual(projection(baseline));
  });

  it("applies only the source-row limit and reports exact local truncation", () => {
    const a1 = hit("A1", "a.md");
    const b1 = hit("B1", "b.md");
    const a2 = hit("A2", "a.md");
    const c1 = hit("C1", "c.md");

    const grouped = groupSearchExecution(execution([a1, b1, a2, c1]), 2);

    expect(grouped.groups.map((group) => group.source.path)).toEqual(["a.md", "b.md"]);
    expect(grouped.groups[0]?.sections).toEqual([a1, a2]);
    expect(grouped.facts).toMatchObject({
      returnedSectionCount: 4,
      observedSourceCount: 3,
      displayedSourceCount: 2,
      omittedObservedSourceCount: 1,
      sourceLimit: 2,
    });
  });

  it.each([
    { state: "exhausted", candidateCount: 7, candidateLimit: 512 },
    { state: "more_available", candidateCount: 7, candidateLimit: 512 },
    { state: "candidate_limit_reached", candidateCount: 512, candidateLimit: 512 },
    { state: "unknown", candidateCount: null, candidateLimit: null },
  ] as const)("passes through $state candidate-window facts unchanged", (candidateWindow) => {
    const grouped = groupSearchExecution(execution([hit("A1", "a.md")], candidateWindow), 100);

    expect(grouped.facts.candidateWindow).toBe(candidateWindow);
    expect(grouped.facts.candidateWindow).toEqual(candidateWindow);
  });

  it("handles empty input and boundary source limits deterministically", () => {
    const candidateWindow: CandidateWindowFacts = {
      state: "unknown",
      candidateCount: null,
      candidateLimit: null,
    };
    const empty = groupSearchExecution(execution([], candidateWindow), 1);
    expect(empty).toEqual({
      groups: [],
      facts: {
        returnedSectionCount: 0,
        observedSourceCount: 0,
        displayedSourceCount: 0,
        omittedObservedSourceCount: 0,
        sourceLimit: 1,
        candidateWindow,
      },
    });

    const hits = [hit("A1", "a.md"), hit("B1", "b.md")];
    expect(groupSearchExecution(execution(hits), 1).groups).toHaveLength(1);
    expect(groupSearchExecution(execution(hits), 100).groups).toHaveLength(2);
    expect(groupedSearchHitLimit(1)).toBe(100);
    expect(groupedSearchHitLimit(100)).toBe(100);
  });

  it.each([0, 101, -1, 1.5, Number.NaN, Number.POSITIVE_INFINITY])(
    "fails closed for invalid source limit %s",
    (sourceLimit) => {
      expect(() => groupedSearchHitLimit(sourceLimit)).toThrow(RangeError);
      expect(() => groupSearchExecution(execution([]), sourceLimit)).toThrow(RangeError);
    },
  );
});
