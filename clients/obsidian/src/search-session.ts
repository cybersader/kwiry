// SPDX-FileCopyrightText: 2026 cybersader
// SPDX-License-Identifier: GPL-3.0-only

import type { SearchFilters, SearchMode } from "./api";
import {
  type SearchBackend,
  type SearchExecution,
  KwiryBackendError,
} from "./backend";

export type SearchSessionOutcome =
  | { kind: "results"; execution: SearchExecution }
  | { kind: "empty" }
  | { kind: "stale" }
  | { kind: "error"; error: KwiryBackendError };

export interface SearchSessionOptions {
  limit: number;
  filters?: SearchFilters;
}

export class SearchSessionController {
  private generation = 0;
  private disposed = false;
  private mode: SearchMode;

  constructor(
    private readonly backend: SearchBackend,
    readonly supportedModes: readonly SearchMode[],
    initialMode: SearchMode,
  ) {
    if (supportedModes.length === 0) {
      throw new KwiryBackendError(
        "mode_unavailable",
        backend.identity.profile,
        "configuration",
        false,
        "The selected backend does not expose a search mode.",
      );
    }
    this.mode = supportedModes.includes(initialMode) ? initialMode : supportedModes[0]!;
  }

  get selectedMode(): SearchMode {
    return this.mode;
  }

  setMode(mode: SearchMode): void {
    if (!this.supportedModes.includes(mode)) {
      throw new KwiryBackendError(
        "mode_unavailable",
        this.backend.identity.profile,
        "query",
        false,
        "The selected search mode is unavailable for this backend.",
      );
    }
    this.generation += 1;
    this.mode = mode;
  }

  async search(query: string, options: SearchSessionOptions): Promise<SearchSessionOutcome> {
    const generation = ++this.generation;
    if (this.disposed) {
      return {
        kind: "error",
        error: new KwiryBackendError(
          "disposed",
          this.backend.identity.profile,
          "lifecycle",
          false,
          "The search session is disposed.",
        ),
      };
    }

    const trimmed = query.trim();
    if (!trimmed) return { kind: "empty" };
    if (!this.supportedModes.includes(this.mode)) {
      return {
        kind: "error",
        error: new KwiryBackendError(
          "mode_unavailable",
          this.backend.identity.profile,
          "query",
          false,
          "The selected search mode is unavailable for this backend.",
        ),
      };
    }

    const backendInstanceId = this.backend.identity.instanceId;
    const requestedMode = this.mode;
    try {
      const execution = await this.backend.search({
        q: trimmed,
        mode: requestedMode,
        limit: options.limit,
        filters: options.filters,
      });
      if (
        this.disposed
        || generation !== this.generation
        || backendInstanceId !== this.backend.identity.instanceId
        || execution.backend.instanceId !== backendInstanceId
        || execution.requestedMode !== requestedMode
        || execution.effectiveMode !== requestedMode
      ) {
        return { kind: "stale" };
      }
      return { kind: "results", execution };
    } catch (error) {
      if (
        this.disposed
        || generation !== this.generation
        || (error instanceof KwiryBackendError && error.code === "disposed")
      ) {
        return { kind: "stale" };
      }
      return {
        kind: "error",
        error: error instanceof KwiryBackendError
          ? error
          : new KwiryBackendError(
              "internal_error",
              this.backend.identity.profile,
              "protocol",
              false,
              "The selected backend could not complete the search.",
            ),
      };
    }
  }

  dispose(): void {
    if (this.disposed) return;
    this.disposed = true;
    this.generation += 1;
  }
}
