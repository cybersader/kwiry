// SPDX-FileCopyrightText: 2026 cybersader
// SPDX-License-Identifier: GPL-3.0-only
import { describe, expect, it } from "vitest";

import {
  DEFAULT_SETTINGS,
  SOURCE_ROW_LIMIT_SETTING_DESCRIPTION,
  SOURCE_ROW_LIMIT_SETTING_NAME,
  loadSettings,
} from "../src/settings";
import {
  identityAfterBump,
  identityAtExtractorVersion,
  identityForMaterial,
} from "./format-identity-material";
import {
  DEFAULT_ENABLED_SOURCE_FORMATS,
  EXTRACTION_POLICY_FINGERPRINT,
  EXTRACTION_PROFILES,
  EXTRACTOR_VERSIONS,
  FORMAT_IDENTITIES,
  FORMAT_IDENTITY_SCHEMA_VERSION,
  IN_PLUGIN_SOURCE_SUPPORT_DESCRIPTION,
  SOURCE_FORMATS,
  SOURCE_PREPARATION_SCHEMA_VERSION,
  classifySourcePath,
  corePolicyFingerprint,
  enabledSourceFormatList,
  formatIdentity,
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

  it("describes resultLimit as a source-row cap over the fixed ranked-section window", () => {
    expect(SOURCE_ROW_LIMIT_SETTING_NAME).toBe("Source row limit");
    expect(SOURCE_ROW_LIMIT_SETTING_DESCRIPTION).toBe(
      "Sources shown per search (1–100). Grouping examines up to 100 ranked sections.",
    );
    expect(SOURCE_ROW_LIMIT_SETTING_DESCRIPTION).not.toContain("Results per search");
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

  it("ignores the legacy report-detail setting so full clipboard reports cannot return", () => {
    for (const legacy of ["compact", "full"]) {
      const loaded = loadSettings({
        diagnosticsReportDetail: legacy,
        diagnosticsReportLevel: "warn",
        diagnosticsReportScope: "failures",
      });
      expect(loaded.diagnosticsReportLevel).toBe("warn");
      expect(loaded.diagnosticsReportScope).toBe("failures");
      expect(loaded).not.toHaveProperty("diagnosticsReportDetail");
    }
    expect(DEFAULT_SETTINGS).not.toHaveProperty("diagnosticsReportDetail");
  });

  it("defaults only extractable formats on and migrates stored toggles honestly", () => {
    expect(DEFAULT_SETTINGS.enabledSourceFormats).toEqual(DEFAULT_ENABLED_SOURCE_FORMATS);
    expect(DEFAULT_ENABLED_SOURCE_FORMATS).toEqual({
      markdown: true,
      text: true,
      base: true,
      canvas: true,
      docx: true,
      pdf: false,
      excalidraw: true,
      excel: false,
    });
    const loaded = loadSettings({
      enabledSourceFormats: {
        markdown: false,
        text: false,
        base: true,
        canvas: "yes",
        docx: true,
        pdf: true,
        excalidraw: true,
        excel: true,
        unknown: false,
      },
    });
    expect(loaded.enabledSourceFormats).toEqual({
      markdown: false,
      text: false,
      base: true,
      canvas: true,
      // DOCX is now extractable, so a stored intent to index it is honoured
      // rather than silently normalized away.
      docx: true,
      // PDF is extractable now too, and this file records the consequence
      // rather than hiding it: normalization no longer neutralizes a stored
      // `pdf: true`, so a toggle written before admission becomes live on
      // upgrade. This follows the DOCX precedent above — the stored value was
      // a real expressed intent, and the same upgrade forces a rebuild anyway
      // because the extraction-policy fingerprint moved. PDF costs far more to
      // index than DOCX, so the cost of honouring it is named in the settings
      // description rather than left for the user to discover.
      pdf: true,
      excalidraw: true,
      excel: true,
    });
    expect(loaded.enabledSourceFormats).not.toBe(DEFAULT_SETTINGS.enabledSourceFormats);
    expect(isSourceFormatExtractable("canvas")).toBe(true);
    expect(isSourceFormatExtractable("docx")).toBe(true);
    expect(isSourceFormatExtractable("pdf")).toBe(true);
    // Extractable but off by default: the toggle, not the format registry, is
    // what keeps a reference library out of a first-run index.
    expect(DEFAULT_ENABLED_SOURCE_FORMATS.pdf).toBe(false);
    expect(DEFAULT_ENABLED_SOURCE_FORMATS.excel).toBe(false);
    expect(isSourceFormatExtractable("excel")).toBe(true);
    expect(isSourceFormatEnabled("pdf", { ...loaded.enabledSourceFormats, pdf: false }))
      .toBe(false);
    expect(isSourceFormatEnabled("pdf", { ...loaded.enabledSourceFormats, pdf: true }))
      .toBe(true);
  });

  it("describes every admitted format including PDF and Excel costs", () => {
    expect(IN_PLUGIN_SOURCE_SUPPORT_DESCRIPTION).toContain(
      "DOCX, Excalidraw, PDF, and Excel are available",
    );
    expect(IN_PLUGIN_SOURCE_SUPPORT_DESCRIPTION).toContain("PDF and Excel are off by default");
    expect(IN_PLUGIN_SOURCE_SUPPORT_DESCRIPTION).not.toContain("remains unavailable");
    expect(sourceFormatDescription("canvas")).toContain("without reading referenced files");
    // Latent content is extracted but labelled, so the description must not
    // imply that hidden or deleted text is silently dropped.
    expect(sourceFormatDescription("docx")).toContain("marked latent");
    // The PDF description has to state what a user cannot otherwise find out:
    // that a page is the section unit, that the page is not searchable text,
    // that encrypted files are refused unread, and that an undecodable font
    // costs the whole document rather than part of it.
    expect(sourceFormatDescription("pdf")).toContain("one section per page");
    expect(sourceFormatDescription("pdf")).toContain("never searchable text");
    expect(sourceFormatDescription("pdf")).toContain("no heading path");
    expect(sourceFormatDescription("pdf")).toContain("Encrypted documents are refused");
    expect(sourceFormatDescription("pdf")).toContain("no text at all rather than a partial");
    expect(sourceFormatDescription("pdf")).not.toContain("Unavailable");
  });

  it("digests only core facts, so a Settings toggle no longer moves the identity", async () => {
    const first = await corePolicyFingerprint();
    const again = await corePolicyFingerprint();
    expect(again).toBe(first);
    // The whole point of the split. Under the old single fingerprint the
    // enabled set was digest material, so every one of these was a different
    // identity and every one of them discarded a complete cache.
    expect(first).toBe("ed64b7acaeaa182997208b295449aa71ba8f436f382554612347d86fb55de7e9");
    // The `-v2` digest of the shipped default set, and of that set with PDF
    // turned on. Pinned as negatives: the material changed, so a cache written
    // under either must be refused rather than silently reused.
    expect(first).not.toBe("1711871671c89fe225a8cd2043ba9aa6bd6466b4e8496fa3bef25c65d8cfcb8b");
    expect(first).not.toBe("e7a23540578a11ebe9830cec2f744716bcf8722100ebf32c01ddbd97a99e126c");
    // The schema-8 / policy-v1 and pre-PDF-admission digests. Still pinned as
    // negatives so no earlier generation of policy identity can be mistaken
    // for the current one.
    expect(first).not.toBe("090269f9386c1e36124dd493ff02688a7921f883c1cebcd9d99ffd3fc2e31029");
    expect(first).not.toBe("c32007f375c07577ac536ca290a078525a6f2f125405a803f584216daf1dad97");
    expect(first).not.toBe("0f7ed72e927b8488adde1dc323ae861017eca3d036965df7ff2df7382370f2e1");
  });

  it("gives every format a distinct identity and refuses one it cannot name", () => {
    expect(FORMAT_IDENTITY_SCHEMA_VERSION).toBe(1);
    // Mirrors `kwiry_core::policy::tests::the_shipped_format_identities_are_pinned`,
    // asserted equal to the adapter by rust/kwiry-obsidian-wasm's
    // typescript_mirror test. Pinned here too because these values decide which
    // cached rows survive a restore, and main.ts needs them before the adapter
    // exists.
    expect(FORMAT_IDENTITIES).toEqual({
      markdown: "b678d0ea2d77d7a79ccc79f4f8a3a1d96aed9bb98757afb1381e5661a1fb96f7",
      text: "c89bb1c6cb87c1e6371d7d03956f1c6bf8bff605c847441c2c72d7599bbd464b",
      base: "d3eeb5a8e3246a07f0c1e41782a7f61628921f43f7afdd722f3a060104e7e079",
      canvas: "01eae3d6859de3287237e366b7fcd9f346dbab395453ef9422bcd67dc527858c",
      docx: "b4f9cff615a917e09d800c2784e17c836ef79cc767c49091818a7b1f8598a38e",
      pdf: "980924c70d64fc5de65ddc2141d043e9188f8856ec6196d30c0d5c11d363c3bc",
      excalidraw: "e1f6868bd320172f6b8d9afc3ac716e309499b065c62fa1b17ae4c2c09d98348",
      excel: "ddfee1499472f960540644e47069db3942a572e883d2328e2b5df856dbd04889",
    });
    // Total over the compiled set: a format with no identity has no way to
    // prove a cached row of it is reusable.
    expect(Object.keys(FORMAT_IDENTITIES).sort()).toEqual([...SOURCE_FORMATS].sort());
    // Distinct, or evicting one format would evict another.
    expect(new Set(Object.values(FORMAT_IDENTITIES)).size).toBe(SOURCE_FORMATS.length);
    for (const format of SOURCE_FORMATS) {
      expect(formatIdentity(format)).toMatch(/^[0-9a-f]{64}$/u);
    }
    // Fails closed rather than borrowing another format's identity.
    expect(() => formatIdentity("unknown" as never)).toThrow();
  });

  it("derives every mirrored identity from the mirrored material", () => {
    // The pinned map above is seven opaque strings. This is what makes them
    // checkable without a WASM build: each one is re-derived from the format,
    // the compiled profile, and the extractor version, in the exact byte layout
    // `kwiry_core::policy::identity_for_material` uses. A hand-copy slip in
    // either map now fails here rather than at install time.
    for (const format of SOURCE_FORMATS) {
      expect(identityForMaterial(format, EXTRACTION_PROFILES[format], EXTRACTOR_VERSIONS[format]))
        .toBe(FORMAT_IDENTITIES[format]);
    }
    expect(Object.keys(EXTRACTOR_VERSIONS).sort()).toEqual([...SOURCE_FORMATS].sort());
    expect(Object.keys(EXTRACTION_PROFILES).sort()).toEqual([...SOURCE_FORMATS].sort());
    // The adapter compiles the portable set and nothing else; `enhanced` here
    // would mean this bundle had somehow picked up a daemon-only extractor.
    for (const format of SOURCE_FORMATS) {
      expect(EXTRACTION_PROFILES[format]).toBe("portable");
      expect(EXTRACTOR_VERSIONS[format]).toBeGreaterThanOrEqual(1);
    }
  });

  it("gives a bumped extractor version an identity no shipped format claims", () => {
    // The property the cache's eviction predicate rests on, asserted on derived
    // material rather than on a forged literal: bumping one format's extractor
    // version moves that format's identity and nothing else's, and the moved
    // value collides with no other format at any version.
    const shipped = new Set(Object.values(FORMAT_IDENTITIES));
    const derived = new Set<string>();
    for (const format of SOURCE_FORMATS) {
      expect(identityAfterBump(format)).not.toBe(FORMAT_IDENTITIES[format]);
      expect(shipped.has(identityAfterBump(format))).toBe(false);
      for (let version = 0; version <= 12; version += 1) {
        const identity = identityAtExtractorVersion(format, version);
        expect(identity).toMatch(/^[0-9a-f]{64}$/u);
        expect(derived.has(identity)).toBe(false);
        derived.add(identity);
      }
    }
    expect(derived.size).toBe(SOURCE_FORMATS.length * 13);
    // A rollback is as much a behaviour change as a bump: the predicate is
    // equality, so an older version must be refused just as firmly.
    for (const format of SOURCE_FORMATS) {
      expect(identityAtExtractorVersion(format, EXTRACTOR_VERSIONS[format] - 1))
        .not.toBe(FORMAT_IDENTITIES[format]);
    }
  });

  it("reports the enabled set as configuration, sorted and never digested", () => {
    expect(enabledSourceFormatList(DEFAULT_ENABLED_SOURCE_FORMATS))
      .toEqual(["base", "canvas", "docx", "excalidraw", "markdown", "text"]);
    expect(enabledSourceFormatList({ ...DEFAULT_ENABLED_SOURCE_FORMATS, pdf: true }))
      .toEqual(["base", "canvas", "docx", "excalidraw", "markdown", "pdf", "text"]);
    expect(enabledSourceFormatList({
      markdown: false,
      text: false,
      base: false,
      canvas: false,
      docx: false,
      pdf: false,
      excalidraw: false,
      excel: false,
    })).toEqual([]);
  });

  it("pins the shipped extraction policy to the portable PDF tier", () => {
    // Mirrors `kwiry_core::policy::SHIPPED_FINGERPRINT`, asserted equal to what
    // the WASM adapter reports by rust/kwiry-obsidian-wasm's typescript_mirror
    // test. Pinned here as well because main.ts folds this constant into the
    // policy hash during onload(), before the adapter exists — so a drifted
    // mirror would restore a cache the adapter could never have produced.
    expect(EXTRACTION_POLICY_FINGERPRINT)
      .toBe("15c0642d97954a127fc6cb7a929dd4e2361a679cf6f251c47b4e99668cb26b8a");
    // The pre-admission digest, when the adapter compiled no PDF extractor at
    // all and reported `pdf=none`. Pinned as a negative because the source
    // preparation schema is still 9 and the default enabled set is unchanged,
    // so this fingerprint is the only thing that refuses a pre-PDF cache image.
    expect(EXTRACTION_POLICY_FINGERPRINT)
      .not.toBe("1b393b155b0af728b1ec9c9131573c105c9e7aba41ff31a4d12c824d4c73adef");
    expect(SOURCE_PREPARATION_SCHEMA_VERSION).toBe(9);
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
