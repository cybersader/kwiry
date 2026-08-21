// SPDX-FileCopyrightText: 2026 cybersader
// SPDX-License-Identifier: GPL-3.0-only

import { readFileSync } from "node:fs";

import { describe, expect, it } from "vitest";

const styles = readFileSync(new URL("../styles.css", import.meta.url), "utf8");

describe("search result format-chip CSS", () => {
  it("keeps the title and visible format text on one bounded heading row", () => {
    const heading = rule(".kwiry-result-heading");
    expect(heading).toContain("display: flex");
    expect(heading).toContain("align-items: baseline");
    expect(heading).toContain("min-width: 0");

    const title = rule(".kwiry-result-title");
    expect(title).toContain("overflow: hidden");
    expect(title).toContain("text-overflow: ellipsis");
    expect(title).toContain("white-space: nowrap");
  });

  it("uses one subtle, contrast-preserving treatment for every format", () => {
    const chip = rule(".kwiry-result-format");
    expect(chip).toContain("border: 1px solid var(--background-modifier-border)");
    expect(chip).toContain("background: var(--background-secondary)");
    expect(chip).toContain("color: var(--text-normal)");
    expect(chip).not.toContain("interactive-accent");
    expect(styles).not.toMatch(/\.kwiry-result-format\[data-format/gu);
  });

  it("keeps details compact and lets highlighted matches lead a quieter excerpt", () => {
    const details = rule(".kwiry-result-details");
    expect(details).toContain("display: flex");
    expect(details).toContain("flex-wrap: wrap");
    expect(details).toContain("color: var(--text-muted)");
    expect(details).toContain("font-variant-numeric: tabular-nums");

    const meta = rule(".kwiry-result-meta");
    expect(meta).toContain("min-width: 0");
    expect(meta).toContain("text-overflow: ellipsis");

    expect(rule(".kwiry-result-context")).toContain("color: var(--text-muted)");
    expect(rule(".kwiry-result-excerpt")).toContain("color: var(--text-muted)");
    const mark = rule(".kwiry-result-excerpt mark");
    expect(mark).toContain("background-color: var(--text-highlight-bg)");
    expect(mark).toContain("color: var(--text-normal)");
    expect(styles).not.toMatch(/\.kwiry-(?:source|section)-result[^}]*background/gu);
  });
});

function rule(selector: string): string {
  const escaped = selector.replace(/[.*+?^${}()|[\]\\]/gu, "\\$&");
  const match = styles.match(new RegExp(`${escaped}\\s*\\{([^}]*)\\}`, "u"));
  if (!match?.[1]) throw new Error(`Missing CSS rule: ${selector}`);
  return match[1];
}
