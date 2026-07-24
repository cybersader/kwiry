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

export function selectSupportedMode(
  preferred: SearchMode,
  supportedModes: readonly SearchMode[],
): SearchMode {
  if (supportedModes.includes(preferred)) return preferred;
  const first = supportedModes[0];
  if (!first) throw new Error("backend exposes no search modes");
  return first;
}

export function nextSearchMode(
  mode: SearchMode,
  supportedModes: readonly SearchMode[] = SEARCH_MODE_OPTIONS.map((option) => option.mode),
): SearchMode {
  const options = SEARCH_MODE_OPTIONS.filter((option) => supportedModes.includes(option.mode));
  if (options.length === 0) throw new Error("backend exposes no search modes");
  const index = options.findIndex((option) => option.mode === mode);
  return options[(index + 1 + options.length) % options.length]!.mode;
}

export function selectedSearchModeOptions(
  selectedMode: SearchMode,
  supportedModes: readonly SearchMode[] = SEARCH_MODE_OPTIONS.map((option) => option.mode),
): SelectedSearchModeOption[] {
  return SEARCH_MODE_OPTIONS
    .filter((option) => supportedModes.includes(option.mode))
    .map((option) => ({
      ...option,
      selected: option.mode === selectedMode,
    }));
}
