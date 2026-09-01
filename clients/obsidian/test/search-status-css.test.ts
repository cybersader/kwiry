// SPDX-FileCopyrightText: 2026 cybersader
// SPDX-License-Identifier: GPL-3.0-only

import { readFileSync } from "node:fs";

import { describe, expect, it } from "vitest";

const styles = readFileSync(new URL("../styles.css", import.meta.url), "utf8");

describe("search status rail CSS", () => {
  it("uses one compact wrapping context bar without the former duplicate rows", () => {
    const context = rule(".kwiry-context-bar");
    expect(context).toContain("display: flex");
    expect(context).toContain("flex-wrap: wrap");
    expect(context).toContain("justify-content: space-between");
    expect(context).toContain("min-width: 0");
    expect(rule(".kwiry-result-level")).toContain("flex-wrap: wrap");
    expect(styles).not.toContain(".kwiry-profile-label");
    expect(styles).not.toContain(".kwiry-mode-control");
  });

  it("keeps field syntax inline and shows compact controls only when active", () => {
    const tools = rule(".kwiry-query-tools");
    expect(tools).toContain("flex: 0 1 auto");
    expect(tools).not.toContain("100%");
    expect(rule(".kwiry-query-controls")).toContain("display: none");
    const activeControls = rule(".kwiry-query-controls.has-controls");
    expect(activeControls).toContain("display: inline-flex");
    expect(activeControls).toContain("gap: var(--size-4-1)");
    const chip = rule(".kwiry-query-control");
    expect(chip).toContain("flex: 0 0 auto");
    expect(chip).toContain("border: 1px solid var(--background-modifier-border)");
    expect(chip).toContain("background: var(--background-secondary)");
    expect(chip).toContain("font-size: var(--font-ui-smaller)");
    expect(rule(".kwiry-query-help")).toContain("background: transparent");
  });

  it("reserves two rendered digits for the status-bar in-flight count", () => {
    const count = rule(".kwiry-status-bar-in-flight-count");
    expect(count).toContain("display: inline-block");
    expect(count).toContain("min-width: 2ch");
    expect(count).toContain("font-variant-numeric: tabular-nums");
    expect(count).toContain("text-align: right");
  });

  it("reserves two status rows instead of changing layout height", () => {
    const rail = rule(".kwiry-status-rail");
    expect(rail).toContain("display: grid");
    expect(rail).not.toContain("display: none");
    expect(rail).toContain("grid-template-rows: minmax(1.35em, auto) minmax(1.35em, auto)");
    expect(rail).toContain("min-height: calc(2.7em + 8px)");
  });

  it("keeps indexing ambient while preserving its row", () => {
    expect(styles).toMatch(/\.kwiry-index-status\s*\{[^}]*visibility: hidden/gu);
    expect(rule(".kwiry-index-status.has-status")).toContain("visibility: visible");
    expect(styles).not.toContain(".kwiry-progress-line");
  });

  it("animates only the delayed searching marker", () => {
    expect(rule(".kwiry-query-status.is-searching.is-animation-ready::before"))
      .toContain("animation: kwiry-status-spin 720ms linear infinite");
    expect(styles.match(/animation:/gu)).toHaveLength(2);
  });

  it("honors reduced-motion preferences", () => {
    expect(styles).toContain("@media (prefers-reduced-motion: reduce)");
    const reducedMotion = styles.slice(styles.indexOf("@media (prefers-reduced-motion: reduce)"));
    expect(reducedMotion).toContain("animation: none");
  });
});

function rule(selector: string): string {
  const escaped = selector.replace(/[.*+?^${}()|[\]\\]/gu, "\\$&");
  const match = styles.match(new RegExp(`${escaped}\\s*\\{([^}]*)\\}`, "u"));
  if (!match?.[1]) throw new Error(`Missing CSS rule: ${selector}`);
  return match[1];
}
