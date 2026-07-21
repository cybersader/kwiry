// SPDX-FileCopyrightText: 2026 cybersader
// SPDX-License-Identifier: GPL-3.0-only

import type { SearchMode } from "./api";

export interface SearchModeOption {
  mode: SearchMode;
  label: string;
}

export interface SelectedSearchModeOption extends SearchModeOption {
  selected: boolean;
}

export const SEARCH_MODE_OPTIONS: readonly SearchModeOption[] = [
  { mode: "lexical", label: "Lexical" },
  { mode: "semantic", label: "Semantic" },
  { mode: "hybrid", label: "Hybrid" },
];

export function nextSearchMode(mode: SearchMode): SearchMode {
  switch (mode) {
    case "lexical":
      return "semantic";
    case "semantic":
      return "hybrid";
    case "hybrid":
      return "lexical";
  }
}

export function selectedSearchModeOptions(
  selectedMode: SearchMode,
): SelectedSearchModeOption[] {
  return SEARCH_MODE_OPTIONS.map((option) => ({
    ...option,
    selected: option.mode === selectedMode,
  }));
}
