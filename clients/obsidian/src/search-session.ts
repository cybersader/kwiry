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

/**
 * Injectable timing seam for the trailing-edge debounce scheduler. The
 * default implementation delegates to the real `setTimeout`/`clearTimeout`
 * globals; tests substitute a deterministic fake so the 100 ms window never
 * depends on real wall-clock delay.
 */
export interface SearchSessionClock {
  setTimeout(callback: () => void, delayMs: number): unknown;
  clearTimeout(handle: unknown): void;
}

export interface SearchSessionSchedulerOptions {
  /** Trailing-edge debounce window in milliseconds. Defaults to 100. */
  debounceMs?: number;
  clock?: SearchSessionClock;
}

const DEFAULT_DEBOUNCE_MS = 100;

const realClock: SearchSessionClock = {
  setTimeout: (callback, delayMs) => setTimeout(callback, delayMs),
  clearTimeout: (handle) => clearTimeout(handle as Parameters<typeof clearTimeout>[0]),
};

/**
 * One scheduled-but-not-yet-dispatched search. At most one instance is ever
 * live: a newer `search()` call always supersedes and stales the previous
 * one before installing itself.
 */
interface PendingSearch {
  generation: number;
  trimmed: string;
  options: SearchSessionOptions;
  backendInstanceId: string;
  requestedMode: SearchMode;
  /** Set once the 100 ms trailing-edge window has elapsed for this entry. */
  dueReached: boolean;
  resolve: (outcome: SearchSessionOutcome) => void;
}

export class SearchSessionController {
  private generation = 0;
  private disposed = false;
  private mode: SearchMode;
  private readonly clock: SearchSessionClock;
  private readonly debounceMs: number;

  /** The single replaceable latest pending query awaiting dispatch. */
  private pending: PendingSearch | null = null;
  private timerHandle: unknown = null;
  /** True while at most one backend search request is in flight. */
  private activeInFlight = false;

  constructor(
    private readonly backend: SearchBackend,
    readonly supportedModes: readonly SearchMode[],
    initialMode: SearchMode,
    schedulerOptions: SearchSessionSchedulerOptions = {},
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
    this.clock = schedulerOptions.clock ?? realClock;
    this.debounceMs = schedulerOptions.debounceMs ?? DEFAULT_DEBOUNCE_MS;
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
    // A mode change invalidates any not-yet-dispatched query outright rather
    // than letting it dispatch under the old mode only to be discovered
    // stale afterward.
    this.clearPending();
  }

  /**
   * Schedules `query` on a 100 ms trailing-edge debounce. At most one
   * backend search is ever active; a query that arrives while one is in
   * flight becomes the single replaceable pending query and is dispatched
   * once the active request settles and its own debounce window has
   * elapsed. A newer call always resolves the previous pending call as
   * stale before replacing it. Empty query, mode changes, disposal, and
   * backend identity drift invalidate pending (not yet dispatched) work
   * without ever reaching the backend.
   */
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
    if (!trimmed) {
      this.clearPending();
      return { kind: "empty" };
    }
    if (!this.supportedModes.includes(this.mode)) {
      this.clearPending();
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

    return new Promise<SearchSessionOutcome>((resolve) => {
      // A newer pending query always resolves the older one as stale.
      this.clearPending();
      const entry: PendingSearch = {
        generation,
        trimmed,
        options,
        backendInstanceId,
        requestedMode,
        dueReached: false,
        resolve,
      };
      this.pending = entry;
      this.timerHandle = this.clock.setTimeout(() => {
        this.timerHandle = null;
        if (this.pending !== entry) return;
        entry.dueReached = true;
        this.tryDispatch();
      }, this.debounceMs);
    });
  }

  dispose(): void {
    if (this.disposed) return;
    this.disposed = true;
    this.generation += 1;
    this.clearPending();
  }

  /** Cancels and resolves the current pending query (if any) as stale. */
  private clearPending(): void {
    const entry = this.pending;
    if (!entry) return;
    this.pending = null;
    if (this.timerHandle !== null) {
      this.clock.clearTimeout(this.timerHandle);
      this.timerHandle = null;
    }
    entry.resolve({ kind: "stale" });
  }

  /**
   * Dispatches the pending query once no backend search is active and its
   * debounce window has elapsed. Backend identity drift since scheduling
   * invalidates the query without ever contacting the backend.
   */
  private tryDispatch(): void {
    if (this.activeInFlight) return;
    const entry = this.pending;
    if (!entry || !entry.dueReached) return;
    this.pending = null;

    if (entry.backendInstanceId !== this.backend.identity.instanceId) {
      entry.resolve({ kind: "stale" });
      this.tryDispatch();
      return;
    }

    this.activeInFlight = true;
    void this.runSearch(entry).finally(() => {
      this.activeInFlight = false;
      this.tryDispatch();
    });
  }

  private async runSearch(entry: PendingSearch): Promise<void> {
    try {
      const execution = await this.backend.search({
        q: entry.trimmed,
        mode: entry.requestedMode,
        limit: entry.options.limit,
        filters: entry.options.filters,
      });
      if (
        this.disposed
        || entry.generation !== this.generation
        || entry.backendInstanceId !== this.backend.identity.instanceId
        || execution.backend.instanceId !== entry.backendInstanceId
        || execution.requestedMode !== entry.requestedMode
        || execution.effectiveMode !== entry.requestedMode
      ) {
        entry.resolve({ kind: "stale" });
        return;
      }
      entry.resolve({ kind: "results", execution });
    } catch (error) {
      if (
        this.disposed
        || entry.generation !== this.generation
        || (error instanceof KwiryBackendError && error.code === "disposed")
      ) {
        entry.resolve({ kind: "stale" });
        return;
      }
      entry.resolve({
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
      });
    }
  }
}
