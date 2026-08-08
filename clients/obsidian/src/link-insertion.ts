// SPDX-FileCopyrightText: 2026 cybersader
// SPDX-License-Identifier: GPL-3.0-only

import type { Editor, EditorPosition, FileManager, TFile } from "obsidian";

export type LinkInsertionKind = "note" | "section";

export interface LinkInsertionTarget {
  editor: Pick<Editor, "replaceRange">;
  sourcePath: string;
  from: EditorPosition;
  to: EditorPosition;
  alias: string | undefined;
}

export type LinkInsertionOutcome =
  | { ok: true }
  | { ok: false; safeMessage: string };

export const MISSING_SECTION_HEADING_MESSAGE =
  "this result has no matched heading to link to.";

export function selectedTextAlias(selectedText: string): string | undefined {
  return selectedText.length > 0 ? selectedText : undefined;
}

export function captureLinkInsertionTarget(
  editor: Pick<Editor, "getCursor" | "getRange" | "replaceRange">,
  sourcePath: string,
): LinkInsertionTarget {
  const from = { ...editor.getCursor("from") };
  const to = { ...editor.getCursor("to") };
  return {
    editor,
    sourcePath,
    from,
    to,
    alias: selectedTextAlias(editor.getRange(from, to)),
  };
}

export function deepestMatchedHeading(headingPath: readonly string[]): string | undefined {
  return headingPath.at(-1);
}

export function insertMarkdownLink(
  fileManager: Pick<FileManager, "generateMarkdownLink">,
  file: TFile,
  target: LinkInsertionTarget,
  heading: string | undefined,
  kind: LinkInsertionKind,
): LinkInsertionOutcome {
  if (kind === "section" && !heading) {
    return { ok: false, safeMessage: MISSING_SECTION_HEADING_MESSAGE };
  }
  const subpath = kind === "section" ? `#${heading}` : undefined;
  const link = fileManager.generateMarkdownLink(
    file,
    target.sourcePath,
    subpath,
    target.alias,
  );
  target.editor.replaceRange(link, target.from, target.to);
  return { ok: true };
}
