// SPDX-FileCopyrightText: 2026 cybersader
// SPDX-License-Identifier: GPL-3.0-only

import { describe, expect, it } from "vitest";

import {
  resolveUniqueHeadingPosition,
  type CachedHeadingLike,
} from "../src/internal/d5c-playground/heading-position";

function heading(
  text: string,
  level: number,
  line: number,
): CachedHeadingLike {
  return {
    heading: text,
    level,
    position: { start: { line, col: 0 } },
  };
}

describe("resolveUniqueHeadingPosition", () => {
  it("distinguishes duplicate leaf headings by their full ancestor path", () => {
    const headings = [
      heading("Alpha", 1, 0),
      heading("Details", 2, 2),
      heading("Beta", 1, 5),
      heading("Details", 2, 7),
    ];

    expect(resolveUniqueHeadingPosition(headings, ["Alpha", "Details"]))
      .toEqual({ line: 2, ch: 0 });
    expect(resolveUniqueHeadingPosition(headings, ["Beta", "Details"]))
      .toEqual({ line: 7, ch: 0 });
  });

  it("fails closed when the same full path is duplicated or missing", () => {
    const headings = [
      heading("Alpha", 1, 0),
      heading("Details", 2, 2),
      heading("Alpha", 1, 5),
      heading("Details", 2, 7),
    ];

    expect(resolveUniqueHeadingPosition(headings, ["Alpha", "Details"])).toBeNull();
    expect(resolveUniqueHeadingPosition(headings, ["Missing"])).toBeNull();
  });
});
