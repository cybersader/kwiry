// SPDX-FileCopyrightText: 2026 cybersader
// SPDX-License-Identifier: GPL-3.0-only

import { describe, expect, it } from "vitest";

import {
  nextSearchMode,
  SEARCH_MODE_OPTIONS,
  selectedSearchModeOptions,
} from "../src/search-mode";

describe("search mode helpers", () => {
  it("defines the visible modes and labels in control order", () => {
    expect(SEARCH_MODE_OPTIONS).toEqual([
      { mode: "lexical", label: "Lexical" },
      { mode: "semantic", label: "Semantic" },
      { mode: "hybrid", label: "Hybrid" },
    ]);
  });

  it("cycles through every mode and wraps to lexical", () => {
    expect(nextSearchMode("lexical")).toBe("semantic");
    expect(nextSearchMode("semantic")).toBe("hybrid");
    expect(nextSearchMode("hybrid")).toBe("lexical");
  });

  it.each(["lexical", "semantic", "hybrid"] as const)(
    "marks only the requested %s mode as selected",
    (selectedMode) => {
      const options = selectedSearchModeOptions(selectedMode);

      expect(options.filter((option) => option.selected)).toEqual([
        expect.objectContaining({ mode: selectedMode }),
      ]);
    },
  );
});
