// SPDX-FileCopyrightText: 2026 cybersader
// SPDX-License-Identifier: GPL-3.0-only

import { describe, expect, it } from "vitest";

import {
  SEARCH_SHORTCUT_BINDINGS,
  searchShortcutAction,
  type SearchShortcutEvent,
  type SearchShortcutPlatform,
} from "../src/search-shortcuts";

function shortcut(
  key: string,
  overrides: Partial<Omit<SearchShortcutEvent, "key">> = {},
): SearchShortcutEvent {
  return {
    key,
    ctrlKey: false,
    metaKey: false,
    altKey: false,
    shiftKey: false,
    ...overrides,
  };
}

describe("SEARCH_SHORTCUT_BINDINGS", () => {
  it("defines the discoverable modal surface and leaves plain Enter inherited", () => {
    expect(SEARCH_SHORTCUT_BINDINGS).toEqual([
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
    ]);
  });
});

describe("searchShortcutAction", () => {
  it.each([
    ["other", shortcut("Enter"), "open-current"],
    ["other", shortcut("Enter", { ctrlKey: true }), "open-new-tab"],
    ["other", shortcut("Enter", { ctrlKey: true, altKey: true }), "open-new-split"],
    ["other", shortcut("O", { ctrlKey: true }), "open-background"],
    ["other", shortcut("Enter", { altKey: true }), "insert-link"],
    ["other", shortcut("Tab"), "cycle-mode"],
    ["macos", shortcut("Enter"), "open-current"],
    ["macos", shortcut("Enter", { metaKey: true }), "open-new-tab"],
    ["macos", shortcut("Enter", { metaKey: true, altKey: true }), "open-new-split"],
    ["macos", shortcut("o", { metaKey: true }), "open-background"],
    ["macos", shortcut("Enter", { altKey: true }), "insert-link"],
    ["macos", shortcut("Tab"), "cycle-mode"],
  ] as const)("maps %s %j to %s", (platform, event, action) => {
    expect(searchShortcutAction(event, platform)).toBe(action);
  });

  it("treats Ctrl and Cmd as platform-specific Mod keys", () => {
    expect(searchShortcutAction(shortcut("Enter", { ctrlKey: true }), "macos")).toBeNull();
    expect(searchShortcutAction(shortcut("Enter", { metaKey: true }), "other")).toBeNull();
  });

  it.each([
    ["other", shortcut("Enter", { ctrlKey: true, shiftKey: true })],
    ["macos", shortcut("Enter", { metaKey: true, ctrlKey: true })],
    ["other", shortcut("p", { ctrlKey: true })],
    ["macos", shortcut("o", { metaKey: true, altKey: true })],
  ] as const)("returns no action for an unmapped %s chord", (platform, event) => {
    expect(searchShortcutAction(event, platform as SearchShortcutPlatform)).toBeNull();
  });
});
