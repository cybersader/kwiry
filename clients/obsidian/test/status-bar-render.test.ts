// SPDX-FileCopyrightText: 2026 cybersader
// SPDX-License-Identifier: GPL-3.0-only

import { describe, expect, it } from "vitest";

import { renderStatusBarText } from "../src/status-bar-render";

interface SpanSegment {
  cls: string;
  text: string;
}

class FakeStatusBar {
  segments: Array<string | SpanSegment> = [];

  setText(text: string): void {
    this.segments = [text];
  }

  empty(): void {
    this.segments = [];
  }

  appendText(text: string): void {
    this.segments.push(text);
  }

  createSpan(options: SpanSegment): HTMLSpanElement {
    this.segments.push(options);
    return {} as HTMLSpanElement;
  }
}

describe("renderStatusBarText", () => {
  it.each([
    ["Kwiry: Reading 4/20 (20%) ·  4 in flight", "04"],
    ["Kwiry: Reading 8/20 (40%) · 16 in flight", "16"],
  ])("isolates the count in a fixed-width span: %s", (text, count) => {
    const statusBar = new FakeStatusBar();

    renderStatusBarText(statusBar as unknown as HTMLElement, text);

    expect(statusBar.segments).toEqual([
      text.slice(0, text.indexOf("·") + 2),
      { cls: "kwiry-status-bar-in-flight-count", text: count },
      " in flight",
    ]);
  });

  it("uses ordinary text rendering when there is no in-flight count", () => {
    const statusBar = new FakeStatusBar();

    renderStatusBarText(statusBar as unknown as HTMLElement, "Kwiry: Ready");

    expect(statusBar.segments).toEqual(["Kwiry: Ready"]);
  });
});
