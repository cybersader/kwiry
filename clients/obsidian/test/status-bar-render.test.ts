// SPDX-FileCopyrightText: 2026 cybersader
// SPDX-License-Identifier: GPL-3.0-only

import { describe, expect, it } from "vitest";

import { createStatusBarRenderer } from "../src/status-bar-render";

class FakeLabel {
  textContent = "";
  setTextCalls = 0;

  setText(text: string): void {
    this.textContent = text;
    this.setTextCalls += 1;
  }
}

class FakeStatusBar {
  readonly classes: string[] = [];
  readonly attributes = new Map<string, string>();
  readonly label = new FakeLabel();
  createSpanCalls = 0;
  setAttributeCalls = 0;

  addClass(name: string): void {
    this.classes.push(name);
  }

  createSpan(options: { cls: string }): HTMLSpanElement {
    expect(options).toEqual({ cls: "kwiry-status-bar__label" });
    this.createSpanCalls += 1;
    return this.label as unknown as HTMLSpanElement;
  }

  setAttribute(name: string, value: string): void {
    this.attributes.set(name, value);
    this.setAttributeCalls += 1;
  }
}

describe("createStatusBarRenderer", () => {
  it("reuses one label while complete status text changes", () => {
    const statusBar = new FakeStatusBar();
    const renderer = createStatusBarRenderer(statusBar as unknown as HTMLElement);

    renderer.render("Kwiry: Reading 8/20 (40%) · 16 in flight");
    const originalLabel = statusBar.label;
    renderer.render("Kwiry: Reading 4/20 (20%) · 04 in flight");

    expect(statusBar.classes).toEqual(["kwiry-status-bar"]);
    expect(statusBar.createSpanCalls).toBe(1);
    expect(statusBar.label).toBe(originalLabel);
    expect(statusBar.label.textContent).toBe("Kwiry: Reading 4/20 (20%) · 04 in flight");
    expect(statusBar.label.setTextCalls).toBe(2);
    expect(statusBar.attributes.get("title")).toBe(statusBar.label.textContent);
    expect(statusBar.attributes.get("aria-label")).toBe(statusBar.label.textContent);
  });

  it("keeps ordinary text accessible and suppresses duplicate writes", () => {
    const statusBar = new FakeStatusBar();
    const renderer = createStatusBarRenderer(statusBar as unknown as HTMLElement);

    renderer.render("Kwiry: Ready");
    renderer.render("Kwiry: Ready");

    expect(statusBar.label.textContent).toBe("Kwiry: Ready");
    expect(statusBar.label.setTextCalls).toBe(1);
    expect(statusBar.setAttributeCalls).toBe(2);
    expect(statusBar.attributes).toEqual(new Map([
      ["title", "Kwiry: Ready"],
      ["aria-label", "Kwiry: Ready"],
    ]));
  });
});
