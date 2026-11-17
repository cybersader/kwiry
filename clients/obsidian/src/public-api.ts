// SPDX-FileCopyrightText: 2026 cybersader
// SPDX-License-Identifier: GPL-3.0-only

import type { SearchMode } from "./api";
import { KwiryBackendError, type SearchBackend } from "./backend";
import { validateOpenResult } from "./open-result";

export interface KwirySearchOptions {
  /** Defaults to 20; finite numbers are floored and clamped to 1–100. */
  limit?: number;
}

export interface KwirySearchResult {
  /** Normalized path relative to the current Obsidian vault. */
  path: string;
  /** Backend-owned score; not comparable across modes or backends. */
  score: number;
  /** The matched Markdown section's final heading, when present. */
  heading?: string;
}

/** Additive changes retain version 1; breaking changes require a new version. */
export interface KwirySearchApiV1 {
  readonly version: 1;
  /** Checks current availability; rejects while no searchable index exists. */
  ready(): Promise<void>;
  search(query: string, opts?: KwirySearchOptions): Promise<KwirySearchResult[]>;
}

export type KwiryApiErrorCode =
  | "invalid_query"
  | "invalid_options"
  | "backend_unavailable"
  | "index_not_ready"
  | "mode_unavailable"
  | "vault_mapping_required"
  | "invalid_result"
  | "backend_changed"
  | "plugin_unloaded"
  | "search_failed";

const ERROR_MESSAGES: Record<KwiryApiErrorCode, string> = {
  invalid_query: "Provide a non-empty string query.",
  invalid_options: "Provide an options object with a finite numeric limit.",
  backend_unavailable: "The selected search backend is unavailable.",
  index_not_ready: "The selected backend does not yet have a searchable index.",
  mode_unavailable: "The configured search mode is unavailable; no fallback was attempted.",
  vault_mapping_required: "Map the current Obsidian vault to its daemon vault ID in Kwiry settings.",
  invalid_result: "The backend returned a result that cannot be exposed as a current-vault hit.",
  backend_changed: "The selected search backend changed during the request; retry the call.",
  plugin_unloaded: "Kwiry Search has unloaded.",
  search_failed: "The selected backend could not complete the search.",
};

export class KwiryApiError extends Error {
  constructor(public readonly code: KwiryApiErrorCode) {
    super(ERROR_MESSAGES[code]);
    this.name = "KwiryApiError";
  }
}

/** Internal wiring only: the published api object contains no host references. */
export function createPublicSearchApi(host: {
  currentBackend(): Promise<SearchBackend | undefined>;
  defaultMode(): SearchMode;
}): { api: KwirySearchApiV1; dispose(): void } {
  let unloaded = false;
  const requireLoaded = (): void => {
    if (unloaded) throw new KwiryApiError("plugin_unloaded");
  };
  const requireCurrent = async (backend: SearchBackend): Promise<void> => {
    requireLoaded();
    const current = await host.currentBackend();
    requireLoaded();
    if (current !== backend) throw new KwiryApiError("backend_changed");
  };
  const available = async () => {
    requireLoaded();
    const backend = await host.currentBackend();
    requireLoaded();
    if (!backend) throw new KwiryApiError("backend_unavailable");
    const status = await backend.status();
    await requireCurrent(backend);
    if (status.phase === "unavailable" || status.phase === "disposed") {
      throw new KwiryApiError("backend_unavailable");
    }
    if (!status.searchable) throw new KwiryApiError("index_not_ready");
    const mode = backend.identity.profile === "in_plugin" ? "lexical" : host.defaultMode();
    if (!status.capabilities.supportedModes.includes(mode)) {
      throw new KwiryApiError("mode_unavailable");
    }
    const vaultId = backend.identity.boundVaultId;
    if (!vaultId) throw new KwiryApiError("vault_mapping_required");
    return { backend, mode, vaultId };
  };
  const run = async <T>(operation: () => Promise<T>): Promise<T> => {
    try {
      requireLoaded();
      return await operation();
    } catch (error) {
      requireLoaded();
      if (error instanceof KwiryApiError) throw error;
      if (error instanceof KwiryBackendError) {
        switch (error.code) {
          case "mode_unavailable": throw new KwiryApiError("mode_unavailable");
          case "index_building":
          case "index_not_ready": throw new KwiryApiError("index_not_ready");
          case "invalid_query": throw new KwiryApiError("invalid_query");
          case "disposed":
          case "backend_not_selected":
          case "daemon_unreachable": throw new KwiryApiError("backend_unavailable");
        }
      }
      // Never expose backend exception messages, credentials, or query text.
      throw new KwiryApiError("search_failed");
    }
  };

  const api: KwirySearchApiV1 = Object.freeze({
    version: 1,
    ready: () => run(async () => { await available(); }),
    search: (query: string, opts?: KwirySearchOptions) => run(async () => {
      if (typeof query !== "string" || query.trim().length === 0) {
        throw new KwiryApiError("invalid_query");
      }
      const limit = validatedLimit(opts);
      const { backend, mode, vaultId } = await available();
      const execution = await backend.search({
        q: query,
        mode,
        limit,
        ...(backend.identity.profile === "daemon" ? { filters: { vault_id: vaultId } } : {}),
      });
      await requireCurrent(backend);
      if (execution.requestedMode !== mode || execution.effectiveMode !== mode) {
        throw new KwiryApiError("mode_unavailable");
      }
      // Preserve engine order and scores. No grouping, deduplication, or reranking.
      return execution.response.hits.slice(0, limit).map((hit) => {
        const decision = validateOpenResult(hit, backend.identity, vaultId, "section");
        if (!decision.ok || /^[a-z]:/iu.test(hit.path) || !Number.isFinite(hit.score)) {
          throw new KwiryApiError("invalid_result");
        }
        const heading = hit.format === "markdown" ? hit.heading_path.at(-1) : undefined;
        return Object.freeze({
          path: decision.path,
          score: hit.score,
          ...(heading ? { heading } : {}),
        });
      });
    }),
  });
  return { api, dispose: () => { unloaded = true; } };
}

function validatedLimit(opts: KwirySearchOptions | undefined): number {
  if (opts !== undefined && (typeof opts !== "object" || opts === null || Array.isArray(opts))) {
    throw new KwiryApiError("invalid_options");
  }
  const limit = opts?.limit === undefined ? 20 : opts.limit;
  if (typeof limit !== "number" || !Number.isFinite(limit)) {
    throw new KwiryApiError("invalid_options");
  }
  return Math.min(100, Math.max(1, Math.floor(limit)));
}
