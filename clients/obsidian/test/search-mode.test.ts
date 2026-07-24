// SPDX-FileCopyrightText: 2026 cybersader
// SPDX-License-Identifier: GPL-3.0-only

import { describe, expect, it } from "vitest";

import {
  nextSearchMode,
  SEARCH_MODE_OPTIONS,
  selectedSearchModeOptions,
  selectSupportedMode,
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

  it("renders and cycles only modes supported by the active backend", () => {
    expect(selectedSearchModeOptions("lexical", ["lexical"])).toEqual([
      { mode: "lexical", label: "Lexical", selected: true },
    ]);
    expect(nextSearchMode("lexical", ["lexical"])).toBe("lexical");
    expect(nextSearchMode("hybrid", ["lexical", "hybrid"])).toBe("lexical");
  });

  it("chooses a supported mode without rewriting the persisted preference", () => {
    expect(selectSupportedMode("hybrid", ["lexical"])).toBe("lexical");
    expect(selectSupportedMode("semantic", ["lexical", "semantic"])).toBe("semantic");
  });

  it("fails when a backend advertises no modes", () => {
    expect(() => selectSupportedMode("lexical", [])).toThrow(/no search modes/);
    expect(() => nextSearchMode("lexical", [])).toThrow(/no search modes/);
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
