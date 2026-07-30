// SPDX-FileCopyrightText: 2026 cybersader
// SPDX-License-Identifier: GPL-3.0-only

import type { FileManager, TFile } from "obsidian";
import { describe, expect, it, vi } from "vitest";

import {
  captureLinkInsertionTarget,
  deepestMatchedHeading,
  insertMarkdownLink,
  MISSING_SECTION_HEADING_MESSAGE,
  selectedTextAlias,
  type LinkInsertionKind,
  type LinkInsertionTarget,
} from "../src/link-insertion";

const file = { path: "Folder/Target.md" } as TFile;
const from = { line: 3, ch: 4 };
const to = { line: 5, ch: 6 };

function setup(alias: string | undefined = undefined): {
  fileManager: Pick<FileManager, "generateMarkdownLink">;
  generateMarkdownLink: ReturnType<typeof vi.fn>;
  replaceRange: ReturnType<typeof vi.fn>;
  target: LinkInsertionTarget;
} {
  const generateMarkdownLink = vi.fn(() => "generated link");
  const replaceRange = vi.fn();
  return {
    fileManager: { generateMarkdownLink },
    generateMarkdownLink,
    replaceRange,
    target: {
      editor: { replaceRange },
      sourcePath: "Folder/Source.md",
      from,
      to,
      alias,
    },
  };
}

describe("captureLinkInsertionTarget", () => {
  it("captures the original editor range and exact selection once", () => {
    let currentFrom = { line: 1, ch: 2 };
    let currentTo = { line: 1, ch: 9 };
    let currentSelection = " original\tselection ";
    const replaceRange = vi.fn();
    const editor = {
      getCursor: vi.fn((side: "from" | "to") => side === "from" ? currentFrom : currentTo),
      getRange: vi.fn(() => currentSelection),
      replaceRange,
    };

    const target = captureLinkInsertionTarget(editor, "Original/Source.md");
    currentFrom = { line: 20, ch: 0 };
    currentTo = { line: 20, ch: 0 };
    currentSelection = "later selection";
    const generateMarkdownLink = vi.fn(() => "captured link");

    insertMarkdownLink({ generateMarkdownLink }, file, target, "Deepest", "note");

    expect(editor.getRange).toHaveBeenCalledOnce();
    expect(editor.getRange).toHaveBeenCalledWith({ line: 1, ch: 2 }, { line: 1, ch: 9 });
    expect(generateMarkdownLink).toHaveBeenCalledWith(
      file,
      "Original/Source.md",
      undefined,
      " original\tselection ",
    );
    expect(replaceRange).toHaveBeenCalledWith(
      "captured link",
      { line: 1, ch: 2 },
      { line: 1, ch: 9 },
    );
  });
});

describe("deepestMatchedHeading", () => {
  it("returns only the deepest matched heading", () => {
    expect(deepestMatchedHeading(["Top", "Middle", "Deepest"])).toBe("Deepest");
  });

  it("returns undefined for a headingless result and preserves an empty final heading", () => {
    expect(deepestMatchedHeading([])).toBeUndefined();
    expect(deepestMatchedHeading(["Top", ""])).toBe("");
  });
});

describe("selectedTextAlias", () => {
  it.each([
    " leading and trailing ",
    "\ttabs\t",
    "line one\nline two",
    "line one\r\nline two",
    "MiXeD Case",
    "Unicode é é 漢字",
    "[Markdown] *punctuation* | # heading",
    " \tMiXeD é é\r\n ",
  ])("preserves every byte of a non-empty selection: %j", (selection) => {
    expect(selectedTextAlias(selection)).toBe(selection);
  });

  it("uses undefined only for an empty selection", () => {
    expect(selectedTextAlias("")).toBeUndefined();
    expect(selectedTextAlias(" ")).toBe(" ");
  });
});

describe("insertMarkdownLink", () => {
  it("inserts a note-level link without a heading subpath", () => {
    const alias = " original selection ";
    const { fileManager, generateMarkdownLink, replaceRange, target } = setup(alias);

    expect(insertMarkdownLink(fileManager, file, target, "Deepest", "note")).toEqual({ ok: true });
    expect(generateMarkdownLink).toHaveBeenCalledOnce();
    expect(generateMarkdownLink).toHaveBeenCalledWith(
      file,
      "Folder/Source.md",
      undefined,
      alias,
    );
    expect(generateMarkdownLink.mock.calls[0]).not.toContain("#Deepest");
    expect(replaceRange).toHaveBeenCalledOnce();
    expect(replaceRange).toHaveBeenCalledWith("generated link", from, to);
  });

  it("inserts a matched-section link using the deepest heading", () => {
    const alias = "\tMiXeD é é\r\n";
    const { fileManager, generateMarkdownLink, replaceRange, target } = setup(alias);
    const heading = deepestMatchedHeading(["Top", "Middle", "Deepest"]);

    expect(insertMarkdownLink(fileManager, file, target, heading, "section")).toEqual({ ok: true });
    expect(generateMarkdownLink).toHaveBeenCalledWith(
      file,
      "Folder/Source.md",
      "#Deepest",
      alias,
    );
    expect(replaceRange).toHaveBeenCalledWith("generated link", from, to);
  });

  it.each(["note", "section"] as const)(
    "passes the exact captured alias through for %s insertion",
    (kind: LinkInsertionKind) => {
      const alias = " \tMiXeD [é] é | #\r\n ";
      const { fileManager, generateMarkdownLink, target } = setup(alias);

      insertMarkdownLink(fileManager, file, target, "Deepest", kind);

      expect(generateMarkdownLink.mock.calls[0]?.[3]).toBe(alias);
    },
  );

  it("passes undefined for an empty captured selection", () => {
    const { fileManager, generateMarkdownLink, target } = setup(selectedTextAlias(""));

    insertMarkdownLink(fileManager, file, target, "Deepest", "note");

    expect(generateMarkdownLink.mock.calls[0]?.[3]).toBeUndefined();
  });

  it.each([undefined, ""])(
    "refuses section insertion without a non-empty matched heading",
    (heading) => {
      const privateSentinels = ["Folder/Target.md", "private query", "secret alias", "heading text"];
      const { fileManager, generateMarkdownLink, replaceRange, target } = setup("secret alias");

      const outcome = insertMarkdownLink(fileManager, file, target, heading, "section");

      expect(outcome).toEqual({ ok: false, safeMessage: MISSING_SECTION_HEADING_MESSAGE });
      expect(generateMarkdownLink).not.toHaveBeenCalled();
      expect(replaceRange).not.toHaveBeenCalled();
      for (const sentinel of privateSentinels) {
        expect(MISSING_SECTION_HEADING_MESSAGE).not.toContain(sentinel);
      }
    },
  );

  it("still inserts a headingless note link", () => {
    const { fileManager, generateMarkdownLink, replaceRange, target } = setup();

    expect(insertMarkdownLink(fileManager, file, target, undefined, "note")).toEqual({ ok: true });
    expect(generateMarkdownLink).toHaveBeenCalledWith(
      file,
      "Folder/Source.md",
      undefined,
      undefined,
    );
    expect(replaceRange).toHaveBeenCalledWith("generated link", from, to);
  });
});
