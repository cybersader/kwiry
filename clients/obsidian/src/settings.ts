// SPDX-FileCopyrightText: 2026 cybersader
// SPDX-License-Identifier: GPL-3.0-only

import type { SearchMode } from "./api";

export interface KwiryPluginSettings {
  daemonUrl: string;
  /** Absolute path to the daemon's bearer-token file. The token itself is
   * never persisted in plugin data. */
  tokenFilePath: string;
  defaultMode: SearchMode;
  resultLimit: number;
  /** vault_id filter applied to every search; empty = all vaults. */
  vaultId: string;
  showRibbonIcon: boolean;
}

export const DEFAULT_SETTINGS: KwiryPluginSettings = {
  daemonUrl: "http://127.0.0.1:32189",
  tokenFilePath: "",
  defaultMode: "hybrid",
  resultLimit: 20,
  vaultId: "",
  showRibbonIcon: true,
};

/** Merges stored data over defaults, discarding unknown keys. */
export function loadSettings(stored: unknown): KwiryPluginSettings {
  const settings = { ...DEFAULT_SETTINGS };
  if (typeof stored !== "object" || stored === null) {
    return settings;
  }
  const source = stored as Record<string, unknown>;
  if (typeof source.daemonUrl === "string") settings.daemonUrl = source.daemonUrl;
  if (typeof source.tokenFilePath === "string") settings.tokenFilePath = source.tokenFilePath;
  if (source.defaultMode === "lexical" || source.defaultMode === "semantic" || source.defaultMode === "hybrid") {
    settings.defaultMode = source.defaultMode;
  }
  if (typeof source.resultLimit === "number" && Number.isInteger(source.resultLimit)) {
    settings.resultLimit = Math.min(100, Math.max(1, source.resultLimit));
  }
  if (typeof source.vaultId === "string") settings.vaultId = source.vaultId;
  if (typeof source.showRibbonIcon === "boolean") settings.showRibbonIcon = source.showRibbonIcon;
  return settings;
}
