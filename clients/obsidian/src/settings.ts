// SPDX-FileCopyrightText: 2026 cybersader
// SPDX-License-Identifier: GPL-3.0-only

import type { SearchMode } from "./api";
import type { BackendProfile } from "./backend";

export type DiagnosticsLogLevel = "off" | "error" | "info";

export interface KwiryPluginSettings {
  backendProfile: BackendProfile;
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
  diagnosticsLogLevel: DiagnosticsLogLevel;
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
  diagnosticsLogLevel: "info",
};

/** Merges stored data over defaults, discarding unknown keys. */
export function loadSettings(stored: unknown): KwiryPluginSettings {
  const settings = { ...DEFAULT_SETTINGS };
  if (typeof stored !== "object" || stored === null) {
    return settings;
  }
  const source = stored as Record<string, unknown>;
  if (source.backendProfile === "daemon" || source.backendProfile === "in_plugin") {
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
  if (
    source.diagnosticsLogLevel === "off"
    || source.diagnosticsLogLevel === "error"
    || source.diagnosticsLogLevel === "info"
  ) {
    settings.diagnosticsLogLevel = source.diagnosticsLogLevel;
  }
  return settings;
}
