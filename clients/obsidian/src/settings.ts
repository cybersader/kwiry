// SPDX-FileCopyrightText: 2026 cybersader
// SPDX-License-Identifier: GPL-3.0-only

import type { SearchMode } from "./api";
import type { SearchProfile } from "./backend";
import {
  DEFAULT_ENABLED_SOURCE_FORMATS,
  SOURCE_FORMATS,
  normalizeEnabledSourceFormats,
  type EnabledSourceFormats,
} from "./source-formats";

export type DiagnosticsLogLevel = "off" | "error" | "info";

/// Report shaping is separate from capture. Capture decides what is recorded;
/// these filters apply to both the bounded clipboard summary and full file export.
export type DiagnosticsReportLevel = "debug" | "info" | "warn" | "error";
export type DiagnosticsReportScope = "all" | "indexing" | "search" | "startup" | "failures";

export const SOURCE_ROW_LIMIT_SETTING_NAME = "Source row limit";
export const SOURCE_ROW_LIMIT_SETTING_DESCRIPTION =
  "Sources shown per search (1–100). Grouping examines up to 100 ranked sections.";

export interface KwiryPluginSettings {
  backendProfile: SearchProfile;
  daemonUrl: string;
  /** Absolute path to the daemon's bearer-token file. The token itself is
   * never persisted in plugin data. */
  tokenFilePath: string;
  defaultMode: SearchMode;
  resultLimit: number;
  /** vault_id filter applied to daemon searches; empty = all registered trees. */
  vaultId: string;
  /** Daemon vault ID that identifies the current Obsidian vault for local actions. */
  daemonCurrentVaultId: string;
  showRibbonIcon: boolean;
  enabledSourceFormats: EnabledSourceFormats;
  diagnosticsLogLevel: DiagnosticsLogLevel;
  diagnosticsReportLevel: DiagnosticsReportLevel;
  diagnosticsReportScope: DiagnosticsReportScope;
  /** A fact about this plugin's own data, not a choice: set when a stricter
   * local indexing policy made the plugin delete its stored index cache. */
  derivedDataDeleted: DerivedDataDeletionRecord | null;
}

export const DERIVED_DATA_DELETED_MESSAGE =
  "Local index cache deleted because this workspace's policy does not allow stored indexes.";

export type DerivedDataDeletionReason ="policy_memory_only" | "policy_refuse";

/** Non-identifying: a time and a fixed reason; no path, vault name or identity. */
export interface DerivedDataDeletionRecord {
  readonly at: string;
  readonly reason: DerivedDataDeletionReason;
}

export const DEFAULT_SETTINGS: KwiryPluginSettings = {
  backendProfile: "daemon",
  daemonUrl: "http://127.0.0.1:32189",
  tokenFilePath: "",
  defaultMode: "hybrid",
  resultLimit: 20,
  vaultId: "",
  daemonCurrentVaultId: "",
  showRibbonIcon: true,
  enabledSourceFormats: { ...DEFAULT_ENABLED_SOURCE_FORMATS },
  diagnosticsLogLevel: "info",
  diagnosticsReportLevel: "info",
  diagnosticsReportScope: "all",
  derivedDataDeleted: null,
};

/** Stored in place of a profile when no person chose one, so every start chooses again. */
export const AUTOMATIC_BACKEND_PROFILE = "automatic";

/// True only when stored data records a backend a person chose. Earlier builds
/// saved exactly `{ backendProfile: "host" }` on their own when a host was
/// present; that record is automatic, not a choice. Legacy settings without a
/// profile keep the original implicit daemon choice, which was the user's.
export function storedBackendProfileChosen(stored: unknown): boolean {
  if (stored === null || stored === undefined) return false;
  if (typeof stored !== "object" || Array.isArray(stored)) return true;
  const source = stored as Record<string, unknown>;
  if (source.backendProfile === AUTOMATIC_BACKEND_PROFILE) return false;
  const keys = Object.keys(source);
  return !(keys.length === 1 && keys[0] === "backendProfile" && source.backendProfile === "host");
}

/** Merges stored data over defaults, discarding unknown keys. */
export function loadSettings(
  stored: unknown,
  daemonBackendAvailable = true,
  hostSearchAvailable = false,
): KwiryPluginSettings {
  const settings: KwiryPluginSettings = {
    ...DEFAULT_SETTINGS,
    enabledSourceFormats: { ...DEFAULT_ENABLED_SOURCE_FORMATS },
  };
  if (stored === null || stored === undefined) {
    settings.backendProfile = automaticBackendProfile(daemonBackendAvailable, hostSearchAvailable);
    return settings;
  }
  if (typeof stored !== "object") return settings;
  // Legacy settings without a profile keep the original implicit daemon choice.
  const source = stored as Record<string, unknown>;
  if (!storedBackendProfileChosen(stored)) {
    settings.backendProfile = automaticBackendProfile(daemonBackendAvailable, hostSearchAvailable);
  } else if (source.backendProfile === "daemon" || source.backendProfile === "in_plugin"
    || source.backendProfile === "host") {
    settings.backendProfile = source.backendProfile;
  }
  if (typeof source.daemonUrl === "string") settings.daemonUrl = source.daemonUrl;
  if (typeof source.tokenFilePath === "string") settings.tokenFilePath = source.tokenFilePath;
  if (
    source.defaultMode === "lexical"
    || source.defaultMode === "semantic"
    || source.defaultMode === "hybrid"
  ) {
    settings.defaultMode = source.defaultMode;
  }
  if (typeof source.resultLimit === "number" && Number.isInteger(source.resultLimit)) {
    settings.resultLimit = Math.min(100, Math.max(1, source.resultLimit));
  }
  if (typeof source.vaultId === "string") settings.vaultId = source.vaultId;
  if (typeof source.daemonCurrentVaultId === "string") {
    settings.daemonCurrentVaultId = source.daemonCurrentVaultId;
  }
  if (typeof source.showRibbonIcon === "boolean") settings.showRibbonIcon = source.showRibbonIcon;
  if (typeof source.enabledSourceFormats === "object" && source.enabledSourceFormats !== null) {
    const storedFormats = source.enabledSourceFormats as Record<string, unknown>;
    for (const format of SOURCE_FORMATS) {
      if (typeof storedFormats[format] === "boolean") {
        settings.enabledSourceFormats[format] = storedFormats[format];
      }
    }
  }
  settings.enabledSourceFormats = normalizeEnabledSourceFormats(settings.enabledSourceFormats);
  if (
    source.diagnosticsLogLevel === "off"
    || source.diagnosticsLogLevel === "error"
    || source.diagnosticsLogLevel === "info"
  ) {
    settings.diagnosticsLogLevel = source.diagnosticsLogLevel;
  }
  if (
    source.diagnosticsReportLevel === "debug"
    || source.diagnosticsReportLevel === "info"
    || source.diagnosticsReportLevel === "warn"
    || source.diagnosticsReportLevel === "error"
  ) {
    settings.diagnosticsReportLevel = source.diagnosticsReportLevel;
  }
  if (
    source.diagnosticsReportScope === "all"
    || source.diagnosticsReportScope === "indexing"
    || source.diagnosticsReportScope === "search"
    || source.diagnosticsReportScope === "startup"
    || source.diagnosticsReportScope === "failures"
  ) {
    settings.diagnosticsReportScope = source.diagnosticsReportScope;
  }
  settings.derivedDataDeleted = readDerivedDataDeletion(source.derivedDataDeleted);
  return settings;
}

function readDerivedDataDeletion(value: unknown): DerivedDataDeletionRecord | null {
  if (typeof value !== "object" || value === null || Array.isArray(value)) return null;
  const { at, reason } = value as Record<string, unknown>;
  if (typeof at !== "string" || Number.isNaN(Date.parse(at))) return null;
  if (reason !== "policy_memory_only" && reason !== "policy_refuse") return null;
  return { at, reason };
}

/// The backend for a start where no person chose one. Computed from the current
/// situation at every start; never saved.
function automaticBackendProfile(daemonBackendAvailable: boolean, hostSearchAvailable: boolean): SearchProfile {
  if (hostSearchAvailable) return "host";
  return daemonBackendAvailable ? "daemon" : "in_plugin";
}
