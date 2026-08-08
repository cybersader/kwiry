// SPDX-FileCopyrightText: 2026 cybersader
// SPDX-License-Identifier: GPL-3.0-only

import type { Vault } from "obsidian";

import type { IndexControllerCacheOptions } from "../backends/in-plugin-index-controller";
import { openVaultCacheStore } from "./local-cache-store";

export function createInPluginCacheOptions(
  vault: Vault,
  sourcePolicyHash: string,
): IndexControllerCacheOptions | undefined {
  return {
    sourcePolicyHash,
    openStore: () => openVaultCacheStore({
      adapter: vault.adapter,
      vaultConfigDirName: vault.configDir,
    }),
  };
}
