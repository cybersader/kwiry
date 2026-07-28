// SPDX-FileCopyrightText: 2026 cybersader
// SPDX-License-Identifier: GPL-3.0-only
//
// Search shortcut policy stays independent of Obsidian so the platform-specific
// meaning of Mod and the exact action assigned to each chord can be tested.

export type SearchShortcutAction =
  | "open-current"
  | "open-new-tab"
  | "open-new-split"
  | "open-background"
  | "insert-link"
  | "cycle-mode";

export type SearchShortcutModifier = "Mod" | "Alt";
export type SearchShortcutPlatform = "macos" | "other";

export interface SearchShortcutBinding {
  readonly modifiers: readonly SearchShortcutModifier[];
  readonly key: string;
  readonly action: SearchShortcutAction;
  readonly command: string;
  readonly purpose: string;
  readonly register: boolean;
}

export interface SearchShortcutEvent {
  readonly key: string;
  readonly ctrlKey: boolean;
  readonly metaKey: boolean;
  readonly altKey: boolean;
  readonly shiftKey: boolean;
}

/// Plain Enter remains owned by SuggestModal. The other rows are registered on
/// its existing scope so Escape and arrow navigation retain their stock logic.
export const SEARCH_SHORTCUT_BINDINGS: readonly SearchShortcutBinding[] = [
  {
    modifiers: [],
    key: "Enter",
    action: "open-current",
    command: "↵",
    purpose: "open",
    register: false,
  },
  {
    modifiers: ["Mod"],
    key: "Enter",
    action: "open-new-tab",
    command: "ctrl/cmd ↵",
    purpose: "open in new tab",
    register: true,
  },
  {
    modifiers: ["Mod", "Alt"],
    key: "Enter",
    action: "open-new-split",
    command: "ctrl/cmd alt ↵",
    purpose: "open in new split",
    register: true,
  },
  {
    modifiers: ["Mod"],
    key: "o",
    action: "open-background",
    command: "ctrl/cmd O",
    purpose: "open new tab, keep search open",
    register: true,
  },
  {
    modifiers: ["Alt"],
    key: "Enter",
    action: "insert-link",
    command: "alt ↵",
    purpose: "insert link",
    register: true,
  },
  {
    modifiers: [],
    key: "Tab",
    action: "cycle-mode",
    command: "tab",
    purpose: "cycle requested mode",
    register: true,
  },
];

/// Resolves only exact chords. In particular, Ctrl is not Cmd on macOS and
/// Meta is not Mod on Windows/Linux, so an extra physical modifier cannot turn
/// an otherwise unrelated shortcut into an action.
export function searchShortcutAction(
  event: SearchShortcutEvent,
  platform: SearchShortcutPlatform,
): SearchShortcutAction | null {
  const key = normalizeKey(event.key);
  for (const binding of SEARCH_SHORTCUT_BINDINGS) {
    if (normalizeKey(binding.key) !== key) continue;
    if (matchesModifiers(binding.modifiers, event, platform)) return binding.action;
  }
  return null;
}

function matchesModifiers(
  modifiers: readonly SearchShortcutModifier[],
  event: SearchShortcutEvent,
  platform: SearchShortcutPlatform,
): boolean {
  const mod = platform === "macos" ? event.metaKey : event.ctrlKey;
  const nonMod = platform === "macos" ? event.ctrlKey : event.metaKey;
  return mod === modifiers.includes("Mod")
    && event.altKey === modifiers.includes("Alt")
    && !event.shiftKey
    && !nonMod;
}

function normalizeKey(key: string): string {
  return key.length === 1 ? key.toLowerCase() : key;
}
