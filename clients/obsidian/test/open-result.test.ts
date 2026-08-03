// SPDX-FileCopyrightText: 2026 cybersader
// SPDX-License-Identifier: GPL-3.0-only

import { describe, expect, it } from "vitest";

import type { BackendIdentity, BackendSearchHit } from "../src/backend";
import {
  isNormalizedMarkdownPath,
  isNormalizedVaultFilePath,
  openTargetForHit,
  pathMatchesFormat,
  validateOpenResult,
} from "../src/open-result";

const DAEMON: BackendIdentity = {
  profile: "daemon",
  instanceId: "daemon-3",
  label: "Daemon",
  boundVaultId: "notes",
};

function hit(overrides: Partial<BackendSearchHit> = {}): BackendSearchHit {
  return {
    chunk_id: "chunk-1",
    vault_id: "notes",
    path: "folder/note.md",
    format: "markdown",
    coverage: "indexed-complete",
    locator: null,
    heading_path: ["Heading"],
    score: 1,
    excerpt: [{ text: "match", highlighted: true }],
    frontmatter: {},
    origin: {
      profile: "daemon",
      backendInstanceId: "daemon-3",
      vaultId: "notes",
    },
    ...overrides,
  };
}

describe("validateOpenResult", () => {
  it("opens a daemon result only through the explicit current-vault mapping", () => {
    expect(validateOpenResult(hit(), DAEMON, "notes")).toEqual({
      ok: true,
      path: "folder/note.md",
      subpath: "#Heading",
    });
    expect(validateOpenResult(hit(), DAEMON, "")).toMatchObject({
      ok: false,
      code: "vault_mapping_required",
    });
    expect(validateOpenResult(hit({ vault_id: "other" }), DAEMON, "notes")).toMatchObject({
      ok: false,
      code: "vault_mismatch",
    });
  });

  it("rejects results from an inactive backend activation", () => {
    expect(
      validateOpenResult(
        hit({
          origin: {
            profile: "daemon",
            backendInstanceId: "daemon-2",
            vaultId: "notes",
          },
        }),
        DAEMON,
        "notes",
      ),
    ).toMatchObject({ ok: false, code: "stale_backend" });
  });

  it("requires in-plugin result identity to match the active vault", () => {
    const backend: BackendIdentity = {
      profile: "in_plugin",
      instanceId: "in_plugin-4",
      label: "In-plugin",
      boundVaultId: "active-vault",
    };
    const local = hit({
      vault_id: "active-vault",
      origin: {
        profile: "in_plugin",
        backendInstanceId: "in_plugin-4",
        vaultId: "active-vault",
      },
    });
    expect(validateOpenResult(local, backend, "ignored").ok).toBe(true);
  });

  it("rejects safe paths whose extension disagrees with the declared format", () => {
    expect(validateOpenResult(hit({ format: "base" }), DAEMON, "notes")).toMatchObject({
      ok: false,
      code: "invalid_path",
    });
    expect(validateOpenResult(hit({ path: "folder/note.pdf" }), DAEMON, "notes"))
      .toMatchObject({ ok: false, code: "invalid_path" });
  });
});

describe("openTargetForHit", () => {
  it("opens Markdown at the deepest heading", () => {
    expect(openTargetForHit(hit({ heading_path: ["Top", "Deep"] }))).toEqual({
      path: "folder/note.md",
      subpath: "#Deep",
    });
    expect(openTargetForHit(hit({ heading_path: [] }))).toEqual({ path: "folder/note.md" });
  });

  it("opens a Base view from its non-ranking locator", () => {
    expect(openTargetForHit(hit({
      path: "projects.base",
      format: "base",
      locator: { kind: "base_view", view: "Active" },
      heading_path: ["Active (2)"],
    }))).toEqual({ path: "projects.base", subpath: "#Active" });
  });

  it.each([
    ["text", "notes.txt"],
    ["canvas", "board.canvas"],
    ["docx", "report.docx"],
    ["pdf", "paper.pdf"],
  ] as const)("opens %s results file-only", (format, path) => {
    expect(openTargetForHit(hit({ path, format, heading_path: ["Ignored"] }))).toEqual({ path });
  });
});

describe("normalized vault source paths", () => {
  it("accepts normalized relative Markdown paths", () => {
    expect(isNormalizedMarkdownPath("note.md")).toBe(true);
    expect(isNormalizedMarkdownPath("folder/Note.MD")).toBe(true);
    expect(isNormalizedMarkdownPath("folder/note.markdown")).toBe(true);
    expect(isNormalizedMarkdownPath("folder/note.mdx")).toBe(true);
  });

  it("validates generic paths separately from format agreement", () => {
    expect(isNormalizedVaultFilePath("folder/report.pdf")).toBe(true);
    expect(pathMatchesFormat("folder/report.PDF", "pdf")).toBe(true);
    expect(pathMatchesFormat("folder/report.pdf", "docx")).toBe(false);
    expect(isNormalizedVaultFilePath("folder/../report.pdf")).toBe(false);
  });

  it.each([
    "",
    "/absolute.md",
    "../outside.md",
    "folder/../outside.md",
    "folder\\note.md",
    "folder//note.md",
    "folder/./note.md",
    "folder/note.txt",
    "bad\0note.md",
  ])("rejects unsafe path %j", (path) => {
    expect(isNormalizedMarkdownPath(path)).toBe(false);
  });
});
