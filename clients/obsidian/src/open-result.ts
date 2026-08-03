// SPDX-FileCopyrightText: 2026 cybersader
// SPDX-License-Identifier: GPL-3.0-only

import type { BackendIdentity, BackendSearchHit } from "./backend";
import { pathMatchesFormat } from "./vault-path";

export interface OpenTarget {
  path: string;
  subpath?: string;
}

export type OpenResultDecision =
  | ({ ok: true } & OpenTarget)
  | {
      ok: false;
      code:
        | "stale_backend"
        | "vault_mapping_required"
        | "vault_mismatch"
        | "invalid_path";
      safeMessage: string;
    };

export function openTargetForHit(
  hit: Pick<BackendSearchHit, "path" | "format" | "locator" | "heading_path">,
): OpenTarget {
  if (hit.format === "markdown") {
    const heading = hit.heading_path.at(-1);
    return heading ? { path: hit.path, subpath: `#${heading}` } : { path: hit.path };
  }
  if (hit.format === "base" && hit.locator?.kind === "base_view") {
    return { path: hit.path, subpath: `#${hit.locator.view}` };
  }
  return { path: hit.path };
}

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
  if (!pathMatchesFormat(hit.path, hit.format)) {
    return {
      ok: false,
      code: "invalid_path",
      safeMessage: "This result does not contain a safe vault-relative path for its source format.",
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

  return { ok: true, ...openTargetForHit(hit) };
}

export {
  isNormalizedMarkdownPath,
  isNormalizedVaultFilePath,
  pathMatchesFormat,
} from "./vault-path";
