// SPDX-FileCopyrightText: 2026 cybersader
// SPDX-License-Identifier: GPL-3.0-only

import type { BackendIdentity, BackendSearchHit } from "./backend";
import { pathMatchesFormat } from "./vault-path";

export type OpenNavigationIntent = "source" | "section";

export interface OpenTarget {
  path: string;
  subpath?: string;
  /**
   * Set only when `subpath` is a PDF page jump, carrying the 1-based page the
   * jump asks for. The subpath alone cannot be checked after the fact — Obsidian
   * accepts any ephemeral state and silently ignores what a view does not
   * understand — so the host keeps the page here in order to say what it aimed
   * at when the opened view turns out not to be one that can honour it.
   */
  page?: number;
}

/**
 * Obsidian's built-in PDF view type. `#page=N` is its documented link syntax,
 * and it is the only view known to consume a page jump; a vault whose `.pdf`
 * extension is claimed by another view gets the file opened and told so.
 */
export const PDF_VIEW_TYPE = "pdf";

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
  intent: OpenNavigationIntent,
): OpenTarget {
  if (intent === "source") return { path: hit.path };
  if (hit.format === "markdown") {
    const heading = hit.heading_path.at(-1);
    return heading ? { path: hit.path, subpath: `#${heading}` } : { path: hit.path };
  }
  if (hit.format === "base" && hit.locator?.kind === "base_view") {
    return { path: hit.path, subpath: `#${hit.locator.view}` };
  }
  // A PDF has no headings, so an explicitly selected section uses its page
  // locator. Source-level navigation returned above intentionally opens the file
  // generally rather than inheriting a representative match's page.
  if (hit.format === "pdf" && hit.locator?.kind === "pdf_page") {
    return {
      path: hit.path,
      subpath: `#page=${hit.locator.page}`,
      page: hit.locator.page,
    };
  }
  return { path: hit.path };
}

/**
 * What to tell the user when an open could not land where the result was found,
 * or `null` when there is nothing to report.
 *
 * Only the *view* is checkable. `openFile` resolves whether or not the view
 * consumed the ephemeral state, so a page jump into Obsidian's own PDF view is
 * reported as achieved on the strength of `#page=N` being that view's
 * documented syntax. What this refuses to do is stay silent when the file
 * opened in a view that certainly cannot honour a page jump — that is the case
 * where the result would otherwise appear to have opened at page one on purpose.
 */
export function pageNavigationShortfall(
  target: OpenTarget,
  openedViewType: string | null,
): string | null {
  if (target.page === undefined) return null;
  if (openedViewType === PDF_VIEW_TYPE) return null;
  return `opened this PDF, but the view showing it cannot jump to page ${target.page}.`;
}

export function validateOpenResult(
  hit: BackendSearchHit,
  activeBackend: BackendIdentity,
  daemonCurrentVaultId: string,
  intent: OpenNavigationIntent,
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

  return { ok: true, ...openTargetForHit(hit, intent) };
}

export {
  isNormalizedMarkdownPath,
  isNormalizedVaultFilePath,
  pathMatchesFormat,
} from "./vault-path";
