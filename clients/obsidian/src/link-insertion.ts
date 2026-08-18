// SPDX-FileCopyrightText: 2026 cybersader
// SPDX-License-Identifier: GPL-3.0-only

import type { Editor, EditorPosition, FileManager, TFile } from "obsidian";

export type LinkInsertionKind = "note" | "section";

export interface LinkInsertionTarget {
  editor: Pick<Editor, "replaceRange" | "setCursor">;
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
  editor: Pick<Editor, "getCursor" | "getRange" | "replaceRange" | "setCursor">,
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
  // `replaceRange` leaves the caret at the start of the replaced range, so
  // typing after inserting a link would land in front of it. Put the caret
  // where a person just finished writing instead.
  target.editor.setCursor(linkInsertionEnd(target.from, link));
  return { ok: true };
}

/// The position immediately after `link` when it is inserted at `from`.
/// Generated links are normally one line, but an alias captured from a
/// multi-line selection can carry newlines, so the last line is measured
/// rather than assumed.
export function linkInsertionEnd(from: EditorPosition, link: string): EditorPosition {
  const lines = link.split("\n");
  const lastLine = lines[lines.length - 1] ?? "";
  return lines.length === 1
    ? { line: from.line, ch: from.ch + lastLine.length }
    : { line: from.line + lines.length - 1, ch: lastLine.length };
}
