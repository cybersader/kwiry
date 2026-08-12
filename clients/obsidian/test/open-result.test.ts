// SPDX-FileCopyrightText: 2026 cybersader
// SPDX-License-Identifier: GPL-3.0-only

import { describe, expect, it } from "vitest";

import type { BackendIdentity, BackendSearchHit } from "../src/backend";
import {
  PDF_VIEW_TYPE,
  isNormalizedMarkdownPath,
  isNormalizedVaultFilePath,
  openTargetForHit,
  pageNavigationShortfall,
  pathMatchesFormat,
  validateOpenResult,
} from "../src/open-result";

const DAEMON: BackendIdentity = {
  profile: "daemon",
  instanceId: "daemon-3",
  label: "Daemon",
  boundVaultId: "notes",
};

function hit(overrides: Partial<BackendSearchHit> = {}): BackendSearchHit {
  return {
    chunk_id: "chunk-1",
    vault_id: "notes",
    path: "folder/note.md",
    format: "markdown",
    coverage: "indexed-complete",
    locator: null,
    heading_path: ["Heading"],
    score: 1,
    excerpt: [{ text: "match", highlighted: true }],
    frontmatter: {},
    origin: {
      profile: "daemon",
      backendInstanceId: "daemon-3",
      vaultId: "notes",
    },
    ...overrides,
  };
}

describe("validateOpenResult", () => {
  it("opens a daemon result only through the explicit current-vault mapping", () => {
    expect(validateOpenResult(hit(), DAEMON, "notes")).toEqual({
      ok: true,
      path: "folder/note.md",
      subpath: "#Heading",
    });
    expect(validateOpenResult(hit(), DAEMON, "")).toMatchObject({
      ok: false,
      code: "vault_mapping_required",
    });
    expect(validateOpenResult(hit({ vault_id: "other" }), DAEMON, "notes")).toMatchObject({
      ok: false,
      code: "vault_mismatch",
    });
  });

  it("rejects results from an inactive backend activation", () => {
    expect(
      validateOpenResult(
        hit({
          origin: {
            profile: "daemon",
            backendInstanceId: "daemon-2",
            vaultId: "notes",
          },
        }),
        DAEMON,
        "notes",
      ),
    ).toMatchObject({ ok: false, code: "stale_backend" });
  });

  it("requires in-plugin result identity to match the active vault", () => {
    const backend: BackendIdentity = {
      profile: "in_plugin",
      instanceId: "in_plugin-4",
      label: "In-plugin",
      boundVaultId: "active-vault",
    };
    const local = hit({
      vault_id: "active-vault",
      origin: {
        profile: "in_plugin",
        backendInstanceId: "in_plugin-4",
        vaultId: "active-vault",
      },
    });
    expect(validateOpenResult(local, backend, "ignored").ok).toBe(true);
  });

  it("carries a validated PDF page through to the open decision", () => {
    expect(validateOpenResult(
      hit({
        path: "papers/report.pdf",
        format: "pdf",
        locator: { kind: "pdf_page", page: 12 },
        heading_path: [],
      }),
      DAEMON,
      "notes",
    )).toEqual({ ok: true, path: "papers/report.pdf", subpath: "#page=12", page: 12 });
  });

  it("rejects safe paths whose extension disagrees with the declared format", () => {
    expect(validateOpenResult(hit({ format: "base" }), DAEMON, "notes")).toMatchObject({
      ok: false,
      code: "invalid_path",
    });
    expect(validateOpenResult(hit({ path: "folder/note.pdf" }), DAEMON, "notes"))
      .toMatchObject({ ok: false, code: "invalid_path" });
  });
});

describe("openTargetForHit", () => {
  it("opens Markdown at the deepest heading", () => {
    expect(openTargetForHit(hit({ heading_path: ["Top", "Deep"] }))).toEqual({
      path: "folder/note.md",
      subpath: "#Deep",
    });
    expect(openTargetForHit(hit({ heading_path: [] }))).toEqual({ path: "folder/note.md" });
  });

  it("opens a Base view from its non-ranking locator", () => {
    expect(openTargetForHit(hit({
      path: "projects.base",
      format: "base",
      locator: { kind: "base_view", view: "Active" },
      heading_path: ["Active (2)"],
    }))).toEqual({ path: "projects.base", subpath: "#Active" });
  });

  it.each([
    ["text", "notes.txt"],
    ["canvas", "board.canvas"],
    ["docx", "report.docx"],
    ["excalidraw", "sketch.excalidraw"],
    ["excel", "budget.xlsx"],
  ] as const)("opens %s results file-only", (format, path) => {
    expect(openTargetForHit(hit({ path, format, heading_path: ["Ignored"] }))).toEqual({ path });
  });

  it.each([
    ["budget.xlsx", { kind: "excel_cell" as const, sheet: "Budget", cell: "B4" }],
    ["macros.xlsm", { kind: "excel_cell" as const, sheet: "Inputs", cell: "C9" }],
  ])("keeps Excel locator metadata out of the open target for %s", (path, locator) => {
    expect(openTargetForHit(hit({
      path,
      format: "excel",
      locator,
      heading_path: [locator.sheet],
    }))).toEqual({ path });
  });

  it("opens a PDF at the page its locator names", () => {
    expect(openTargetForHit(hit({
      path: "papers/report.pdf",
      format: "pdf",
      locator: { kind: "pdf_page", page: 7 },
      heading_path: [],
    }))).toEqual({ path: "papers/report.pdf", subpath: "#page=7", page: 7 });
  });

  it("opens a PDF file-only when no page survived to the hit", () => {
    expect(openTargetForHit(hit({
      path: "papers/report.pdf",
      format: "pdf",
      locator: null,
      heading_path: [],
    }))).toEqual({ path: "papers/report.pdf" });
  });

  it("never turns a foreign locator into a page jump", () => {
    // The wire validators reject these pairings before a hit reaches here; this
    // asserts the open path does not invent navigation if one ever slipped past.
    expect(openTargetForHit(hit({
      path: "papers/report.pdf",
      format: "pdf",
      locator: { kind: "base_view", view: "Active" },
      heading_path: [],
    }))).toEqual({ path: "papers/report.pdf" });
    expect(openTargetForHit(hit({
      path: "board.canvas",
      format: "canvas",
      locator: { kind: "pdf_page", page: 3 },
      heading_path: [],
    }))).toEqual({ path: "board.canvas" });
  });

  it.each([
    {
      format: "markdown",
      path: "note.md",
      representative: { locator: null, heading_path: ["Representative"] },
      exact: { locator: null, heading_path: ["Exact"] },
      representativeTarget: { path: "note.md", subpath: "#Representative" },
      exactTarget: { path: "note.md", subpath: "#Exact" },
    },
    {
      format: "text",
      path: "note.txt",
      representative: { locator: null, heading_path: ["Representative"] },
      exact: { locator: null, heading_path: ["Exact"] },
      representativeTarget: { path: "note.txt" },
      exactTarget: { path: "note.txt" },
    },
    {
      format: "base",
      path: "projects.base",
      representative: {
        locator: { kind: "base_view" as const, view: "Representative" },
        heading_path: ["Representative (2)"],
      },
      exact: {
        locator: { kind: "base_view" as const, view: "Exact" },
        heading_path: ["Exact (1)"],
      },
      representativeTarget: { path: "projects.base", subpath: "#Representative" },
      exactTarget: { path: "projects.base", subpath: "#Exact" },
    },
    {
      format: "canvas",
      path: "board.canvas",
      representative: { locator: null, heading_path: ["Representative"] },
      exact: { locator: null, heading_path: ["Exact"] },
      representativeTarget: { path: "board.canvas" },
      exactTarget: { path: "board.canvas" },
    },
    {
      format: "excel",
      path: "budget.xlsx",
      representative: {
        locator: { kind: "excel_cell" as const, sheet: "Budget", cell: "A1" },
        heading_path: ["Budget"],
      },
      exact: {
        locator: { kind: "excel_cell" as const, sheet: "Budget", cell: "B4" },
        heading_path: ["Budget"],
      },
      representativeTarget: { path: "budget.xlsx" },
      exactTarget: { path: "budget.xlsx" },
    },
    {
      // Grouped search opens a source row from its representative hit and a
      // drilled row from its own hit. A PDF has no heading path, so the page
      // locator is the only thing that keeps those two apart.
      format: "pdf",
      path: "papers/report.pdf",
      representative: { locator: { kind: "pdf_page" as const, page: 4 }, heading_path: [] },
      exact: { locator: { kind: "pdf_page" as const, page: 19 }, heading_path: [] },
      representativeTarget: { path: "papers/report.pdf", subpath: "#page=4", page: 4 },
      exactTarget: { path: "papers/report.pdf", subpath: "#page=19", page: 19 },
    },
  ] as const)("keeps representative and exact $format targets format-correct", ({
    format,
    path,
    representative,
    exact,
    representativeTarget,
    exactTarget,
  }) => {
    expect(openTargetForHit(hit({
      path,
      format,
      locator: representative.locator,
      heading_path: [...representative.heading_path],
    }))).toEqual(representativeTarget);
    expect(openTargetForHit(hit({
      path,
      format,
      locator: exact.locator,
      heading_path: [...exact.heading_path],
    }))).toEqual(exactTarget);
  });
});

describe("pageNavigationShortfall", () => {
  const pdfTarget = { path: "papers/report.pdf", subpath: "#page=7", page: 7 };

  it("reports nothing for an open that asked for no page", () => {
    expect(pageNavigationShortfall({ path: "note.md", subpath: "#Deep" }, "markdown")).toBeNull();
    expect(pageNavigationShortfall({ path: "papers/report.pdf" }, "pdf")).toBeNull();
    expect(pageNavigationShortfall({ path: "papers/report.pdf" }, null)).toBeNull();
  });

  it("reports nothing when Obsidian's own PDF view took the page", () => {
    expect(pageNavigationShortfall(pdfTarget, PDF_VIEW_TYPE)).toBeNull();
    expect(PDF_VIEW_TYPE).toBe("pdf");
  });

  it("names the page it aimed at when the opened view cannot honour one", () => {
    // A vault whose .pdf extension is claimed by another view still opens the
    // file. Saying nothing would present page one as the found location.
    expect(pageNavigationShortfall(pdfTarget, "annotator-view"))
      .toBe("opened this PDF, but the view showing it cannot jump to page 7.");
    expect(pageNavigationShortfall(pdfTarget, null))
      .toBe("opened this PDF, but the view showing it cannot jump to page 7.");
  });
});

describe("normalized vault source paths", () => {
  it("accepts normalized relative Markdown paths", () => {
    expect(isNormalizedMarkdownPath("note.md")).toBe(true);
    expect(isNormalizedMarkdownPath("folder/Note.MD")).toBe(true);
    expect(isNormalizedMarkdownPath("folder/note.markdown")).toBe(true);
    expect(isNormalizedMarkdownPath("folder/note.mdx")).toBe(true);
  });

  it("validates generic paths separately from format agreement", () => {
    expect(isNormalizedVaultFilePath("folder/report.pdf")).toBe(true);
    expect(pathMatchesFormat("folder/report.PDF", "pdf")).toBe(true);
    expect(pathMatchesFormat("folder/report.pdf", "docx")).toBe(false);
    expect(isNormalizedVaultFilePath("folder/../report.pdf")).toBe(false);
  });

  it.each([
    "",
    "/absolute.md",
    "../outside.md",
    "folder/../outside.md",
    "folder\\note.md",
    "folder//note.md",
    "folder/./note.md",
    "folder/note.txt",
    "bad\0note.md",
  ])("rejects unsafe path %j", (path) => {
    expect(isNormalizedMarkdownPath(path)).toBe(false);
  });
});
