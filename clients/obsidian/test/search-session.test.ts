// SPDX-FileCopyrightText: 2026 cybersader
// SPDX-License-Identifier: GPL-3.0-only

import { describe, expect, it, vi } from "vitest";

import type { SearchMode, SearchRequest } from "../src/api";
import {
  type BackendIdentity,
  type BackendStatus,
  type SearchBackend,
  type SearchExecution,
  KwiryBackendError,
} from "../src/backend";
import {
  type SearchSessionClock,
  SearchSessionController,
} from "../src/search-session";

interface Deferred<T> {
  promise: Promise<T>;
  resolve(value: T): void;
  reject(error: unknown): void;
}

function deferred<T>(): Deferred<T> {
  let resolve!: (value: T) => void;
  let reject!: (error: unknown) => void;
  const promise = new Promise<T>((accept, deny) => {
    resolve = accept;
    reject = deny;
  });
  return { promise, resolve, reject };
}

class DeferredBackend implements SearchBackend {
  readonly identity: BackendIdentity = {
    profile: "daemon",
    instanceId: "daemon-1",
    label: "Daemon",
    boundVaultId: "notes",
  };
  readonly requests: SearchRequest[] = [];
  readonly searches: Deferred<SearchExecution>[] = [];

  async initialize(): Promise<void> {}

  async status(): Promise<BackendStatus> {
    throw new Error("unused");
  }

  search(request: SearchRequest): Promise<SearchExecution> {
    this.requests.push(request);
    const search = deferred<SearchExecution>();
    this.searches.push(search);
    return search.promise;
  }

  async dispose(): Promise<void> {}

  execution(mode: SearchMode = "lexical"): SearchExecution {
    return {
      backend: this.identity,
      requestedMode: mode,
      effectiveMode: mode,
      queryPolicy: mode === "semantic"
        ? { lexical_profile: "none", scope: null, emphasis: null }
        : { lexical_profile: "lexical-v2", scope: null, emphasis: null },
      lexicalMatchQuality: mode === "lexical"
        ? { availability: "available", value: "none" }
        : { availability: "not_applicable" },
      generation: "generation-1",
      candidateWindow: {
        state: "unknown",
        candidateCount: null,
        candidateLimit: null,
      },
      diagnostics: {
        sourceGeneration: { availability: "unavailable" },
        lexicalExecution: mode === "semantic"
          ? { availability: "not_applicable" }
          : { availability: "unavailable" },
      },
      response: { hits: [], next_cursor: null },
    };
  }
}

/**
 * Deterministic, injectable stand-in for `SearchSessionClock`. At most one
 * timer is ever outstanding from the session's perspective (a newer pending
 * query always cancels the previous timer before scheduling its own), so a
 * single-slot fake is sufficient and avoids depending on real wall-clock
 * delay or vitest's global timer mocking.
 */
function createFakeClock(): { clock: SearchSessionClock; elapse(): void } {
  let scheduled: { id: number; callback: () => void } | null = null;
  let nextId = 0;
  const clock: SearchSessionClock = {
    setTimeout: (callback) => {
      const id = ++nextId;
      scheduled = { id, callback };
      return id;
    },
    clearTimeout: (handle) => {
      if (scheduled && scheduled.id === handle) scheduled = null;
    },
  };
  return {
    clock,
    /** Fires the single outstanding debounce timer, if any is armed. */
    elapse(): void {
      const current = scheduled;
      scheduled = null;
      current?.callback();
    },
  };
}

describe("SearchSessionController", () => {
  it("trims requests and dispatches after the debounce window elapses", async () => {
    const backend = new DeferredBackend();
    const { clock, elapse } = createFakeClock();
    const session = new SearchSessionController(backend, ["lexical"], "lexical", { clock });
    const pending = session.search("  query  ", { limit: 7 });

    expect(backend.requests).toEqual([]);
    elapse();
    expect(backend.requests).toEqual([{ q: "query", mode: "lexical", limit: 7 }]);
    backend.searches[0]!.resolve(backend.execution());

    await expect(pending).resolves.toMatchObject({ kind: "results" });
  });

  it("debounces rapid queries into a single backend request", async () => {
    const backend = new DeferredBackend();
    const { clock, elapse } = createFakeClock();
    const session = new SearchSessionController(backend, ["lexical"], "lexical", { clock });

    const first = session.search("f", { limit: 20 });
    const second = session.search("fi", { limit: 20 });
    const third = session.search("fin", { limit: 20 });

    await expect(first).resolves.toEqual({ kind: "stale" });
    await expect(second).resolves.toEqual({ kind: "stale" });
    expect(backend.requests).toEqual([]);

    elapse();
    expect(backend.requests).toEqual([{ q: "fin", mode: "lexical", limit: 20 }]);
    backend.searches[0]!.resolve(backend.execution());
    await expect(third).resolves.toMatchObject({ kind: "results" });
  });

  it("retains only the latest pending query while a backend request is active", async () => {
    const backend = new DeferredBackend();
    const { clock, elapse } = createFakeClock();
    const session = new SearchSessionController(backend, ["lexical"], "lexical", { clock });

    const active = session.search("older", { limit: 20 });
    elapse();
    expect(backend.requests).toHaveLength(1);

    const supersededPending = session.search("mid", { limit: 20 });
    const latestPending = session.search("newer", { limit: 20 });
    await expect(supersededPending).resolves.toEqual({ kind: "stale" });

    // The active request settling does not dispatch the latest pending
    // query on its own: its debounce window has not elapsed yet. The active
    // request itself is now stale too: a newer query superseded it before
    // it settled, per the preserved generation semantics.
    backend.searches[0]!.resolve(backend.execution());
    await expect(active).resolves.toEqual({ kind: "stale" });
    expect(backend.requests).toHaveLength(1);

    elapse();
    expect(backend.requests).toHaveLength(2);
    expect(backend.requests[1]).toEqual({ q: "newer", mode: "lexical", limit: 20 });
    backend.searches[1]!.resolve(backend.execution());
    await expect(latestPending).resolves.toMatchObject({ kind: "results" });
  });

  it("dispatches the latest pending query once the active request settles, even if its debounce already elapsed", async () => {
    const backend = new DeferredBackend();
    const { clock, elapse } = createFakeClock();
    const session = new SearchSessionController(backend, ["lexical"], "lexical", { clock });

    const active = session.search("older", { limit: 20 });
    elapse();
    expect(backend.requests).toHaveLength(1);

    const latestPending = session.search("newer", { limit: 20 });
    // Debounce elapses before the active request settles.
    elapse();
    expect(backend.requests).toHaveLength(1);

    // The active request is now stale: a newer query superseded it before
    // it settled, per the preserved generation semantics.
    backend.searches[0]!.resolve(backend.execution());
    await expect(active).resolves.toEqual({ kind: "stale" });

    expect(backend.requests).toHaveLength(2);
    expect(backend.requests[1]).toEqual({ q: "newer", mode: "lexical", limit: 20 });
    backend.searches[1]!.resolve(backend.execution());
    await expect(latestPending).resolves.toMatchObject({ kind: "results" });
  });

  it("clearing the query invalidates an older still-pending search without contacting the backend", async () => {
    const backend = new DeferredBackend();
    const { clock } = createFakeClock();
    const session = new SearchSessionController(backend, ["lexical"], "lexical", { clock });
    const older = session.search("older", { limit: 20 });

    await expect(session.search("   ", { limit: 20 })).resolves.toEqual({ kind: "empty" });
    await expect(older).resolves.toEqual({ kind: "stale" });
    expect(backend.requests).toEqual([]);
  });

  it("clearing the query does not cancel an already active search, which resolves stale via generation", async () => {
    const backend = new DeferredBackend();
    const { clock, elapse } = createFakeClock();
    const session = new SearchSessionController(backend, ["lexical"], "lexical", { clock });
    const older = session.search("older", { limit: 20 });
    elapse();

    await expect(session.search("   ", { limit: 20 })).resolves.toEqual({ kind: "empty" });
    backend.searches[0]!.resolve(backend.execution());
    await expect(older).resolves.toEqual({ kind: "stale" });
  });

  it("changing mode invalidates a still-pending search without contacting the backend", async () => {
    const backend = new DeferredBackend();
    const { clock } = createFakeClock();
    const session = new SearchSessionController(
      backend,
      ["lexical", "semantic"],
      "lexical",
      { clock },
    );
    const pending = session.search("query", { limit: 20 });
    session.setMode("semantic");

    await expect(pending).resolves.toEqual({ kind: "stale" });
    expect(backend.requests).toEqual([]);
    expect(session.selectedMode).toBe("semantic");
  });

  it("changing mode invalidates an active in-flight search", async () => {
    const backend = new DeferredBackend();
    const { clock, elapse } = createFakeClock();
    const session = new SearchSessionController(
      backend,
      ["lexical", "semantic"],
      "lexical",
      { clock },
    );
    const active = session.search("query", { limit: 20 });
    elapse();
    session.setMode("semantic");
    backend.searches[0]!.resolve(backend.execution("lexical"));

    await expect(active).resolves.toEqual({ kind: "stale" });
    expect(session.selectedMode).toBe("semantic");
  });

  it("rejects a backend response that changes the effective mode", async () => {
    const backend = new DeferredBackend();
    const { clock, elapse } = createFakeClock();
    const session = new SearchSessionController(
      backend,
      ["lexical", "semantic"],
      "semantic",
      { clock },
    );
    const pending = session.search("query", { limit: 20 });
    elapse();
    const execution = backend.execution("semantic");
    execution.effectiveMode = "lexical";
    backend.searches[0]!.resolve(execution);

    await expect(pending).resolves.toEqual({ kind: "stale" });
  });

  it("disposal invalidates a still-pending search without contacting the backend", async () => {
    const backend = new DeferredBackend();
    const { clock } = createFakeClock();
    const session = new SearchSessionController(backend, ["lexical"], "lexical", { clock });
    const pending = session.search("query", { limit: 20 });
    session.dispose();

    await expect(pending).resolves.toEqual({ kind: "stale" });
    expect(backend.requests).toEqual([]);
    await expect(session.search("later", { limit: 20 })).resolves.toMatchObject({
      kind: "error",
      error: { code: "disposed" },
    });
  });

  it("disposal invalidates an active in-flight search and blocks later searches", async () => {
    const backend = new DeferredBackend();
    const { clock, elapse } = createFakeClock();
    const session = new SearchSessionController(backend, ["lexical"], "lexical", { clock });
    const active = session.search("query", { limit: 20 });
    elapse();
    session.dispose();
    backend.searches[0]!.resolve(backend.execution());

    await expect(active).resolves.toEqual({ kind: "stale" });
    await expect(session.search("later", { limit: 20 })).resolves.toMatchObject({
      kind: "error",
      error: { code: "disposed" },
    });
  });

  it("treats backend disposal during explicit profile switching as stale", async () => {
    const backend = new DeferredBackend();
    const { clock, elapse } = createFakeClock();
    const session = new SearchSessionController(backend, ["lexical"], "lexical", { clock });
    const pending = session.search("query", { limit: 20 });
    elapse();
    backend.searches[0]!.reject(new KwiryBackendError(
      "disposed",
      "daemon",
      "lifecycle",
      false,
      "The daemon backend is disposed.",
    ));

    await expect(pending).resolves.toEqual({ kind: "stale" });
  });

  it("keeps an older active rejection stale after a newer request supersedes it", async () => {
    const backend = new DeferredBackend();
    const { clock, elapse } = createFakeClock();
    const session = new SearchSessionController(backend, ["lexical"], "lexical", { clock });
    const older = session.search("older", { limit: 20 });
    elapse();

    const newer = session.search("newer", { limit: 20 });
    backend.searches[0]!.reject(new KwiryBackendError(
      "query_execution_failed",
      "daemon",
      "query",
      true,
      "The older request failed.",
    ));
    await expect(older).resolves.toEqual({ kind: "stale" });

    elapse();
    expect(backend.requests).toHaveLength(2);
    backend.searches[1]!.resolve(backend.execution());
    await expect(newer).resolves.toMatchObject({ kind: "results" });
  });

  it("invalidates a pending query without contacting the backend when the backend identity drifts before dispatch", async () => {
    const backend = new DeferredBackend();
    const { clock, elapse } = createFakeClock();
    const session = new SearchSessionController(backend, ["lexical"], "lexical", { clock });
    const pending = session.search("query", { limit: 20 });
    backend.identity.instanceId = "daemon-2";
    elapse();

    await expect(pending).resolves.toEqual({ kind: "stale" });
    expect(backend.requests).toEqual([]);
  });

  it.each([
    "explicit_query_unsupported",
    "invalid_query",
    "invalid_query_plan",
    "query_execution_failed",
    "mode_unavailable",
    "index_building",
    "worker_recovering",
  ])("preserves the structured %s backend error", async (code) => {
    const backend = new DeferredBackend();
    const { clock, elapse } = createFakeClock();
    const session = new SearchSessionController(backend, ["lexical"], "lexical", { clock });
    const pending = session.search("query", { limit: 20 });
    elapse();
    const error = new KwiryBackendError(
      code,
      "daemon",
      code === "index_building" ? "index" : code === "invalid_query_plan" ? "protocol" : "query",
      code === "index_building" || code === "query_execution_failed" || code === "worker_recovering",
      "Fixed safe message.",
    );
    backend.searches[0]!.reject(error);

    await expect(pending).resolves.toEqual({ kind: "error", error });
  });

  it("maps an untyped search rejection to a distinct fixed execution failure", async () => {
    const backend = new DeferredBackend();
    const { clock, elapse } = createFakeClock();
    const session = new SearchSessionController(backend, ["lexical"], "lexical", { clock });
    const pending = session.search("query", { limit: 20 });
    elapse();
    backend.searches[0]!.reject(new Error("private query, SQL, path, or Rust detail"));

    await expect(pending).resolves.toMatchObject({
      kind: "error",
      error: {
        code: "internal_error",
        stage: "protocol",
        safeMessage: "The selected backend could not complete the search.",
      },
    });
    expect(JSON.stringify(await pending)).not.toContain("private query");
  });

  it("refuses programmatic requests for unsupported modes", () => {
    const backend = new DeferredBackend();
    const { clock } = createFakeClock();
    const session = new SearchSessionController(backend, ["lexical"], "lexical", { clock });
    expect(() => session.setMode("semantic")).toThrow(/unavailable/);
  });

  it("defaults to a real 100 ms trailing-edge debounce when no clock is injected", async () => {
    vi.useFakeTimers();
    try {
      const backend = new DeferredBackend();
      const session = new SearchSessionController(backend, ["lexical"], "lexical");
      const pending = session.search("query", { limit: 20 });

      await vi.advanceTimersByTimeAsync(99);
      expect(backend.requests).toEqual([]);

      await vi.advanceTimersByTimeAsync(1);
      expect(backend.requests).toEqual([{ q: "query", mode: "lexical", limit: 20 }]);

      backend.searches[0]!.resolve(backend.execution());
      await expect(pending).resolves.toMatchObject({ kind: "results" });
    } finally {
      vi.useRealTimers();
    }
  });
});
