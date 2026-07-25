// SPDX-FileCopyrightText: 2026 cybersader
// SPDX-License-Identifier: GPL-3.0-only

import type { BackendIdentity, BackendSearchHit } from "./backend";
import { isNormalizedMarkdownPath } from "./vault-path";

export type OpenResultDecision =
  | { ok: true; path: string }
  | {
      ok: false;
      code:
        | "stale_backend"
        | "vault_mapping_required"
        | "vault_mismatch"
        | "invalid_path";
      safeMessage: string;
    };

export function validateOpenResult(
  hit: BackendSearchHit,
  activeBackend: BackendIdentity,
  daemonCurrentVaultId: string,
): OpenResultDecision {
  if (
    hit.origin.profile !== activeBackend.profile
    || hit.origin.backendInstanceId !== activeBackend.instanceId
  ) {
    return {
      ok: false,
      code: "stale_backend",
      safeMessage: "This result belongs to an inactive search backend.",
    };
  }
  if (!isNormalizedMarkdownPath(hit.path)) {
    return {
      ok: false,
      code: "invalid_path",
      safeMessage: "This result does not contain a safe vault-relative Markdown path.",
    };
  }

  if (activeBackend.profile === "daemon") {
    const mapping = daemonCurrentVaultId.trim();
    if (!mapping || activeBackend.boundVaultId !== mapping) {
      return {
        ok: false,
        code: "vault_mapping_required",
        safeMessage: "Map the current Obsidian vault to its daemon vault ID before opening results.",
      };
    }
    if (hit.vault_id !== mapping || hit.origin.vaultId !== mapping) {
      return {
        ok: false,
        code: "vault_mismatch",
        safeMessage: "This daemon result belongs to a different registered tree.",
      };
    }
  } else if (
    activeBackend.boundVaultId === null
    || hit.vault_id !== activeBackend.boundVaultId
    || hit.origin.vaultId !== activeBackend.boundVaultId
  ) {
    return {
      ok: false,
      code: "vault_mismatch",
      safeMessage: "This result does not belong to the active Obsidian vault.",
    };
  }

  return { ok: true, path: hit.path };
}

export { isNormalizedMarkdownPath } from "./vault-path";
