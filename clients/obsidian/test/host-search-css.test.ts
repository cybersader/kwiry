// SPDX-FileCopyrightText: 2026 cybersader
// SPDX-License-Identifier: GPL-3.0-only

import { readFileSync } from "node:fs";

import { expect, it } from "vitest";

it("keeps hidden host-search controls out of layout despite host display styles", () => {
  const styles = readFileSync(new URL("../styles.css", import.meta.url), "utf8");
  expect(styles).toMatch(/\.kwiry-host-search\s+\[hidden\]\s*\{[^}]*\bdisplay:\s*none\s*!important\s*;/u);
});
