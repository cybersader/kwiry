// SPDX-FileCopyrightText: 2026 cybersader
// SPDX-License-Identifier: GPL-3.0-only

import { readFile } from "node:fs/promises";
import { resolve } from "node:path";
import { describe, expect, it } from "vitest";

const repositoryRoot = resolve(import.meta.dirname, "../../..");
const paths = [
  "ROADMAP.md",
  "README.md",
  "docs/product-map.md",
  "docs/roadmap/desktop-obsidian.md",
  "docs/design/obsidian-lite.md",
  "clients/obsidian/README.md",
  "docs/releases/0.6.0-beta.15.md",
];

describe("published Obsidian documentation truth", () => {
  it("does not revive known pre-beta.15 current-state claims", async () => {
    const text = (await Promise.all(paths.map(async (path) =>
      `${path}\n${await readFile(resolve(repositoryRoot, path), "utf8")}`))).join("\n");
    for (const stale of [
      "current plugin cannot search at all",
      "not a delivered plugin mode",
      "host not delivered",
      "remains daemon-backed",
      "current releases contain the daemon-backed production plugin",
      "Gate 5 active-vault indexing is implemented on PR #2",
      "It remains unmerged and not delivered",
      "Portable Rust extraction and production integration remain",
    ]) expect(text).not.toContain(stale);
  });

  it("states the published profiles and preserves owner acceptance boundaries", async () => {
    const [roadmap, plugin, product, release] = await Promise.all([
      readFile(resolve(repositoryRoot, "ROADMAP.md"), "utf8"),
      readFile(resolve(repositoryRoot, "clients/obsidian/README.md"), "utf8"),
      readFile(resolve(repositoryRoot, "docs/product-map.md"), "utf8"),
      readFile(resolve(repositoryRoot, "docs/releases/0.6.0-beta.15.md"), "utf8"),
    ]);
    for (const text of [roadmap, plugin, product, release]) {
      expect(text).toContain("beta.15");
      expect(text).toContain("In-plugin · Lexical");
      expect(text).toContain("Excel");
    }
    expect(roadmap).toContain("Property projection does not authorize ranking");
    expect(plugin).toContain("not owner acceptance");
    expect(product).toContain("does not imply accepted property ranking");
    expect(release).toContain("This release does not amend `CONTRACT.md`");
  });

  it("keeps root and plugin manifest descriptions identical", async () => {
    const [rootManifest, pluginManifest] = await Promise.all([
      readFile(resolve(repositoryRoot, "manifest.json"), "utf8").then(JSON.parse),
      readFile(resolve(repositoryRoot, "clients/obsidian/manifest.json"), "utf8").then(JSON.parse),
    ]);
    expect(rootManifest.description).toBe(pluginManifest.description);
    expect(rootManifest.description).toContain("in-plugin lexical");
    expect(rootManifest.description).toContain("semantic and hybrid");
  });
});
