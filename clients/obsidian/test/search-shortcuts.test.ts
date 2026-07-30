// SPDX-FileCopyrightText: 2026 cybersader
// SPDX-License-Identifier: GPL-3.0-only

import { describe, expect, it } from "vitest";

import {
  SEARCH_SHORTCUT_BINDINGS,
  searchShortcutAction,
  type SearchShortcutAction,
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

const modifierStates = Array.from({ length: 16 }, (_, bits) => ({
  ctrlKey: (bits & 1) !== 0,
  metaKey: (bits & 2) !== 0,
  altKey: (bits & 4) !== 0,
  shiftKey: (bits & 8) !== 0,
}));

function expectedAction(
  platform: SearchShortcutPlatform,
  key: string,
  event: Omit<SearchShortcutEvent, "key">,
): SearchShortcutAction | null {
  const hasMod = platform === "macos" ? event.metaKey : event.ctrlKey;
  const hasNonMod = platform === "macos" ? event.ctrlKey : event.metaKey;
  if (key === "Enter") {
    if (!hasMod && !hasNonMod && !event.altKey && !event.shiftKey) return "open-current";
    if (hasMod && !hasNonMod && !event.altKey && !event.shiftKey) return "open-new-tab";
    if (hasMod && !hasNonMod && event.altKey && !event.shiftKey) return "open-new-split";
    if (!hasMod && !hasNonMod && event.altKey && !event.shiftKey) return "insert-note-link";
    if (!hasMod && !hasNonMod && event.altKey && event.shiftKey) return "insert-section-link";
    return null;
  }
  if (key.toLowerCase() === "j") {
    return event.ctrlKey && !event.metaKey && !event.altKey && !event.shiftKey
      ? "move-down"
      : null;
  }
  if (key.toLowerCase() === "k") {
    return event.ctrlKey && !event.metaKey && !event.altKey && !event.shiftKey
      ? "move-up"
      : null;
  }
  return null;
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
        action: "insert-note-link",
        command: "alt ↵",
        purpose: "insert note link",
        register: true,
      },
      {
        modifiers: ["Alt", "Shift"],
        key: "Enter",
        action: "insert-section-link",
        command: "alt shift ↵",
        purpose: "insert matched section link",
        register: true,
      },
      {
        modifiers: ["Ctrl"],
        key: "j",
        action: "move-down",
        command: "ctrl J",
        purpose: "next result",
        register: true,
      },
      {
        modifiers: ["Ctrl"],
        key: "k",
        action: "move-up",
        command: "ctrl K",
        purpose: "previous result",
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
  it.each(["other", "macos"] as const)(
    "resolves only the intended Enter modifier combinations on %s",
    (platform) => {
      for (const modifiers of modifierStates) {
        const event = shortcut("Enter", modifiers);
        expect(searchShortcutAction(event, platform), JSON.stringify(modifiers)).toBe(
          expectedAction(platform, "Enter", modifiers),
        );
      }
    },
  );

  it.each(["other", "macos"] as const)(
    "resolves only physical Ctrl navigation chords on %s",
    (platform) => {
      for (const key of ["j", "J", "k", "K"]) {
        for (const modifiers of modifierStates) {
          const event = shortcut(key, modifiers);
          expect(searchShortcutAction(event, platform), `${key} ${JSON.stringify(modifiers)}`).toBe(
            expectedAction(platform, key, modifiers),
          );
        }
      }
    },
  );

  it.each([
    ["other", shortcut("O", { ctrlKey: true }), "open-background"],
    ["other", shortcut("Tab"), "cycle-mode"],
    ["macos", shortcut("o", { metaKey: true }), "open-background"],
    ["macos", shortcut("Tab"), "cycle-mode"],
  ] as const)("maps legacy %s %j to %s", (platform, event, action) => {
    expect(searchShortcutAction(event, platform)).toBe(action);
  });

  it.each([
    ["other", shortcut("o", { ctrlKey: true, shiftKey: true })],
    ["other", shortcut("o", { ctrlKey: true, altKey: true })],
    ["macos", shortcut("o", { metaKey: true, ctrlKey: true })],
    ["macos", shortcut("o", { metaKey: true, altKey: true })],
    ["other", shortcut("Tab", { ctrlKey: true })],
    ["macos", shortcut("Tab", { metaKey: true })],
    ["other", shortcut("p", { ctrlKey: true })],
  ] as const)("returns no action for an unmapped %s chord", (platform, event) => {
    expect(searchShortcutAction(event, platform)).toBeNull();
  });
});
