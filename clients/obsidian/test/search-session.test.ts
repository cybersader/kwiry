// SPDX-FileCopyrightText: 2026 cybersader
// SPDX-License-Identifier: GPL-3.0-only

import { describe, expect, it } from "vitest";

import type { SearchMode, SearchRequest } from "../src/api";
import {
  type BackendIdentity,
  type BackendStatus,
  type SearchBackend,
  type SearchExecution,
  KwiryBackendError,
} from "../src/backend";
import { SearchSessionController } from "../src/search-session";

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
      generation: "generation-1",
      candidateWindow: {
        state: "unknown",
        candidateCount: null,
        candidateLimit: null,
      },
      response: { hits: [], next_cursor: null },
    };
  }
}

describe("SearchSessionController", () => {
  it("trims requests and returns current results", async () => {
    const backend = new DeferredBackend();
    const session = new SearchSessionController(backend, ["lexical"], "lexical");
    const pending = session.search("  query  ", { limit: 7 });
    backend.searches[0]!.resolve(backend.execution());

    await expect(pending).resolves.toMatchObject({ kind: "results" });
    expect(backend.requests).toEqual([{ q: "query", mode: "lexical", limit: 7 }]);
  });

  it("clearing the query invalidates an older in-flight search", async () => {
    const backend = new DeferredBackend();
    const session = new SearchSessionController(backend, ["lexical"], "lexical");
    const older = session.search("older", { limit: 20 });

    await expect(session.search("   ", { limit: 20 })).resolves.toEqual({ kind: "empty" });
    backend.searches[0]!.resolve(backend.execution());
    await expect(older).resolves.toEqual({ kind: "stale" });
  });

  it("changing mode invalidates in-flight results", async () => {
    const backend = new DeferredBackend();
    const session = new SearchSessionController(
      backend,
      ["lexical", "semantic"],
      "lexical",
    );
    const pending = session.search("query", { limit: 20 });
    session.setMode("semantic");
    backend.searches[0]!.resolve(backend.execution("lexical"));

    await expect(pending).resolves.toEqual({ kind: "stale" });
    expect(session.selectedMode).toBe("semantic");
  });

  it("rejects a backend response that changes the effective mode", async () => {
    const backend = new DeferredBackend();
    const session = new SearchSessionController(
      backend,
      ["lexical", "semantic"],
      "semantic",
    );
    const pending = session.search("query", { limit: 20 });
    const execution = backend.execution("semantic");
    execution.effectiveMode = "lexical";
    backend.searches[0]!.resolve(execution);

    await expect(pending).resolves.toEqual({ kind: "stale" });
  });

  it("disposal invalidates in-flight work and blocks later searches", async () => {
    const backend = new DeferredBackend();
    const session = new SearchSessionController(backend, ["lexical"], "lexical");
    const pending = session.search("query", { limit: 20 });
    session.dispose();
    backend.searches[0]!.resolve(backend.execution());

    await expect(pending).resolves.toEqual({ kind: "stale" });
    await expect(session.search("later", { limit: 20 })).resolves.toMatchObject({
      kind: "error",
      error: { code: "disposed" },
    });
  });

  it("treats backend disposal during explicit profile switching as stale", async () => {
    const backend = new DeferredBackend();
    const session = new SearchSessionController(backend, ["lexical"], "lexical");
    const pending = session.search("query", { limit: 20 });
    backend.searches[0]!.reject(new KwiryBackendError(
      "disposed",
      "daemon",
      "lifecycle",
      false,
      "The daemon backend is disposed.",
    ));

    await expect(pending).resolves.toEqual({ kind: "stale" });
  });

  it("keeps an older rejection stale after a newer request begins", async () => {
    const backend = new DeferredBackend();
    const session = new SearchSessionController(backend, ["lexical"], "lexical");
    const older = session.search("older", { limit: 20 });
    const newer = session.search("newer", { limit: 20 });

    backend.searches[0]!.reject(new KwiryBackendError(
      "query_execution_failed",
      "daemon",
      "query",
      true,
      "The older request failed.",
    ));
    backend.searches[1]!.resolve(backend.execution());

    await expect(older).resolves.toEqual({ kind: "stale" });
    await expect(newer).resolves.toMatchObject({ kind: "results" });
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
    const session = new SearchSessionController(backend, ["lexical"], "lexical");
    const pending = session.search("query", { limit: 20 });
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
    const session = new SearchSessionController(backend, ["lexical"], "lexical");
    const pending = session.search("query", { limit: 20 });
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
    const session = new SearchSessionController(backend, ["lexical"], "lexical");
    expect(() => session.setMode("semantic")).toThrow(/unavailable/);
  });
});
