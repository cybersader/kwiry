// SPDX-FileCopyrightText: 2026 cybersader
// SPDX-License-Identifier: GPL-3.0-only
import { describe, expect, it } from "vitest";

import { DEFAULT_SETTINGS, loadSettings } from "../src/settings";
import {
  DEFAULT_ENABLED_SOURCE_FORMATS,
  IN_PLUGIN_SOURCE_SUPPORT_DESCRIPTION,
  classifySourcePath,
  formatPolicyFingerprint,
  isSourceFormatEnabled,
  isSourceFormatExtractable,
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

  it("defaults only extractable formats on and migrates stored toggles honestly", () => {
    expect(DEFAULT_SETTINGS.enabledSourceFormats).toEqual(DEFAULT_ENABLED_SOURCE_FORMATS);
    expect(DEFAULT_ENABLED_SOURCE_FORMATS).toEqual({
      markdown: true,
      text: true,
      base: true,
      canvas: true,
      docx: false,
      pdf: false,
    });
    const loaded = loadSettings({
      enabledSourceFormats: {
        markdown: false,
        text: false,
        base: true,
        canvas: "yes",
        docx: true,
        pdf: true,
        unknown: false,
      },
    });
    expect(loaded.enabledSourceFormats).toEqual({
      markdown: false,
      text: false,
      base: true,
      canvas: true,
      docx: false,
      pdf: false,
    });
    expect(loaded.enabledSourceFormats).not.toBe(DEFAULT_SETTINGS.enabledSourceFormats);
    expect(isSourceFormatExtractable("canvas")).toBe(true);
    expect(isSourceFormatExtractable("docx")).toBe(false);
    expect(isSourceFormatEnabled("docx", { ...loaded.enabledSourceFormats, docx: true })).toBe(false);
  });

  it("describes Canvas as available while DOCX and PDF are unavailable and unread", () => {
    expect(IN_PLUGIN_SOURCE_SUPPORT_DESCRIPTION).toContain("Base, and Canvas are available");
    expect(IN_PLUGIN_SOURCE_SUPPORT_DESCRIPTION).toContain(
      "PDF and DOCX remain unavailable until extractors ship and their bytes are not read",
    );
    expect(sourceFormatDescription("canvas")).toContain("without reading referenced files");
    expect(sourceFormatDescription("docx")).toContain("not inventoried or read");
    expect(sourceFormatDescription("pdf")).toContain("not inventoried or read");
  });

  it("fingerprints the effective schema-6 extraction policy deterministically", async () => {
    const first = await formatPolicyFingerprint({ ...DEFAULT_ENABLED_SOURCE_FORMATS });
    const reorderedWithDormantLegacyIntent = await formatPolicyFingerprint({
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
    expect(first).toBe("c414b56f31d22f8e1fbe69f5074bc8862337d1c8ee6065b6ad0da441b4f63860");
    expect(first).not.toBe("c32007f375c07577ac536ca290a078525a6f2f125405a803f584216daf1dad97");
    expect(reorderedWithDormantLegacyIntent).toBe(first);
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
