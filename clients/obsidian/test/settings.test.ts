// SPDX-FileCopyrightText: 2026 cybersader
// SPDX-License-Identifier: GPL-3.0-only
import { describe, expect, it } from "vitest";

import { DEFAULT_SETTINGS, loadSettings } from "../src/settings";
import {
  DEFAULT_ENABLED_SOURCE_FORMATS,
  IN_PLUGIN_SOURCE_SUPPORT_DESCRIPTION,
  classifySourcePath,
  formatPolicyFingerprint,
  sourceFormatDescription,
} from "../src/source-formats";

describe("loadSettings", () => {
  it("classifies the closed extension set case-insensitively and rejects unsafe paths", () => {
    expect(classifySourcePath("Notes/Example.MDX")).toBe("markdown");
    expect(classifySourcePath("notes.txt")).toBe("text");
    expect(classifySourcePath("query.base")).toBe("base");
    expect(classifySourcePath("board.canvas")).toBe("canvas");
    expect(classifySourcePath("report.docx")).toBe("docx");
    expect(classifySourcePath("paper.PDF")).toBe("pdf");
    expect(classifySourcePath("../escape.md")).toBeNull();
    expect(classifySourcePath("image.png")).toBeNull();
  });

  it("returns daemon defaults for missing or invalid data", () => {
    expect(loadSettings(undefined)).toEqual(DEFAULT_SETTINGS);
    expect(loadSettings(null)).toEqual(DEFAULT_SETTINGS);
    expect(loadSettings("garbage")).toEqual(DEFAULT_SETTINGS);
    expect(loadSettings({ backendProfile: "unknown" }).backendProfile).toBe("daemon");
  });

  it("migrates existing daemon settings and drops unknown keys", () => {
    const loaded = loadSettings({
      daemonUrl: "http://127.0.0.1:9999",
      defaultMode: "lexical",
      surprise: true,
    });
    expect(loaded.backendProfile).toBe("daemon");
    expect(loaded.daemonUrl).toBe("http://127.0.0.1:9999");
    expect(loaded.defaultMode).toBe("lexical");
    expect(loaded).not.toHaveProperty("surprise");
    expect(loaded.resultLimit).toBe(DEFAULT_SETTINGS.resultLimit);
  });

  it("preserves dormant daemon settings and explicit vault mapping", () => {
    const loaded = loadSettings({
      backendProfile: "in_plugin",
      daemonUrl: "http://127.0.0.1:3333",
      tokenFilePath: "/private/token",
      vaultId: "search-scope",
      daemonCurrentVaultId: "current-vault",
      defaultMode: "hybrid",
    });
    expect(loaded.backendProfile).toBe("in_plugin");
    expect(loaded.daemonUrl).toBe("http://127.0.0.1:3333");
    expect(loaded.tokenFilePath).toBe("/private/token");
    expect(loaded.vaultId).toBe("search-scope");
    expect(loaded.daemonCurrentVaultId).toBe("current-vault");
    expect(loaded.defaultMode).toBe("hybrid");
  });

  it("clamps result limit to the API contract range", () => {
    expect(loadSettings({ resultLimit: 0 }).resultLimit).toBe(1);
    expect(loadSettings({ resultLimit: 500 }).resultLimit).toBe(100);
    expect(loadSettings({ resultLimit: 2.5 }).resultLimit).toBe(DEFAULT_SETTINGS.resultLimit);
  });

  it("rejects invalid mode values", () => {
    expect(loadSettings({ defaultMode: "quantum" }).defaultMode).toBe(
      DEFAULT_SETTINGS.defaultMode,
    );
  });

  it("loads only supported diagnostics levels and defaults to field information", () => {
    expect(DEFAULT_SETTINGS.diagnosticsLogLevel).toBe("info");
    expect(loadSettings({ diagnosticsLogLevel: "off" }).diagnosticsLogLevel).toBe("off");
    expect(loadSettings({ diagnosticsLogLevel: "error" }).diagnosticsLogLevel).toBe("error");
    expect(loadSettings({ diagnosticsLogLevel: "verbose" }).diagnosticsLogLevel).toBe("info");
  });

  it("enables every closed source format by default and loads valid toggles only", () => {
    expect(DEFAULT_SETTINGS.enabledSourceFormats).toEqual(DEFAULT_ENABLED_SOURCE_FORMATS);
    const loaded = loadSettings({
      enabledSourceFormats: {
        markdown: false,
        text: false,
        base: true,
        canvas: "yes",
        pdf: false,
        unknown: false,
      },
    });
    expect(loaded.enabledSourceFormats).toEqual({
      markdown: false,
      text: false,
      base: true,
      canvas: true,
      docx: true,
      pdf: false,
    });
    expect(loaded.enabledSourceFormats).not.toBe(DEFAULT_SETTINGS.enabledSourceFormats);
  });

  it("describes Canvas as extractable while DOCX and PDF remain admitted-only", () => {
    expect(IN_PLUGIN_SOURCE_SUPPORT_DESCRIPTION).toContain("Base, and Canvas are extractable");
    expect(IN_PLUGIN_SOURCE_SUPPORT_DESCRIPTION).toContain(
      "PDF and DOCX are admitted but reported as not yet supported",
    );
    expect(sourceFormatDescription("canvas")).toContain("without reading referenced files");
    expect(sourceFormatDescription("docx")).toContain("not yet supported");
    expect(sourceFormatDescription("pdf")).toContain("not yet supported");
  });

  it("fingerprints schema-6 format policy deterministically and changes on a toggle", async () => {
    const first = await formatPolicyFingerprint({ ...DEFAULT_ENABLED_SOURCE_FORMATS });
    const reordered = await formatPolicyFingerprint({
      pdf: true,
      docx: true,
      canvas: true,
      base: true,
      text: true,
      markdown: true,
    });
    const withoutText = await formatPolicyFingerprint({
      ...DEFAULT_ENABLED_SOURCE_FORMATS,
      text: false,
    });
    expect(first).toBe("c32007f375c07577ac536ca290a078525a6f2f125405a803f584216daf1dad97");
    expect(first).not.toBe("9ac3d481372532c3c6259eedd2c1fdb51a3de4dd6807bf1ef8f95d4fc47fe20b");
    expect(reordered).toBe(first);
    expect(withoutText).not.toBe(first);
  });

  it("keeps normal settings unaware of private-build namespaces", () => {
    const loaded = loadSettings({
      backendProfile: "in_plugin",
      __kwiry_internal_d5c_playground: {
        schema_version: 1,
        explanation_level: "rules",
      },
    });
    expect(loaded.backendProfile).toBe("in_plugin");
    expect(loaded).not.toHaveProperty("__kwiry_internal_d5c_playground");
    expect(DEFAULT_SETTINGS).not.toHaveProperty("__kwiry_internal_d5c_playground");
  });

  it("never contains a token value field", () => {
    const keys = Object.keys(DEFAULT_SETTINGS);
    expect(keys.some((key) => key === "token" || key === "bearerToken")).toBe(false);
    expect(keys).toContain("tokenFilePath");
  });
});
