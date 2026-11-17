// SPDX-FileCopyrightText: 2026 cybersader
// SPDX-License-Identifier: GPL-3.0-only

import { describe, expect, it, vi } from "vitest";

import type { SearchMode } from "../src/api";
import { BackendManager } from "../src/backend-manager";
import {
  KwiryBackendError,
  type BackendProfile,
  type BackendStatus,
  type SearchBackend,
  type SearchExecution,
} from "../src/backend";
import {
  createPublicSearchApi,
  KwiryApiError,
  type KwirySearchOptions,
} from "../src/public-api";

function fixture(profile: BackendProfile = "in_plugin", defaultMode: SearchMode = "hybrid") {
  const identity = {
    profile,
    instanceId: `${profile}-1`,
    label: profile === "daemon" ? "Daemon" as const : "In-plugin" as const,
    boundVaultId: "synthetic-vault",
  };
  const status: BackendStatus = {
    identity,
    phase: "ready",
    liveness: "alive",
    searchable: true,
    generation: "generation-1",
    capabilities: {
      supportedModes: profile === "daemon" ? ["lexical", "semantic", "hybrid"] : ["lexical"],
      sourceScope: profile === "daemon" ? "registered_trees" : "active_vault",
      manualRebuild: false,
    },
    documents: 2,
    chunks: 3,
    dirty: false,
    rebuilding: false,
  };
  const mode = profile === "daemon" ? defaultMode : "lexical";
  const execution: SearchExecution = {
    backend: identity,
    requestedMode: mode,
    effectiveMode: mode,
    queryPolicy: null,
    lexicalMatchQuality: { availability: "unavailable" },
    generation: "generation-1",
    candidateWindow: { state: "unknown", candidateCount: null, candidateLimit: null },
    diagnostics: {
      sourceGeneration: { availability: "unavailable" },
      lexicalExecution: { availability: "unavailable" },
    },
    response: {
      hits: [
        { path: "Notes/Orchard.md", score: 4.5, heading_path: ["Fruit", "Pears"] },
        { path: "Notes/Orchard.md", score: 3, heading_path: [] },
        { path: "Notes/Meadow.md", score: 2, heading_path: ["Flowers"] },
      ].map((hit, index) => ({
        ...hit,
        chunk_id: `chunk-${index}`,
        vault_id: "synthetic-vault",
        format: "markdown",
        coverage: "indexed-complete",
        locator: null,
        excerpt: [],
        frontmatter: {},
        origin: { profile, backendInstanceId: identity.instanceId, vaultId: "synthetic-vault" },
      })),
      next_cursor: null,
    },
  };
  const backend: SearchBackend = {
    identity,
    initialize: vi.fn(async () => undefined),
    status: vi.fn(async () => status),
    search: vi.fn(async () => execution),
    dispose: vi.fn(async () => undefined),
  };
  const currentBackend = vi.fn(async (): Promise<SearchBackend | undefined> => backend);
  const controller = createPublicSearchApi({ currentBackend, defaultMode: () => defaultMode });
  return { ...controller, backend, status, execution, currentBackend };
}

describe("public search API v1", () => {
  it.each(["in_plugin", "daemon"] as const)("delegates %s search, preserving section order and scores", async (profile) => {
    const { api, backend } = fixture(profile);
    expect(api.version).toBe(1);
    expect(Object.isFrozen(api)).toBe(true);
    expect(Object.keys(api).sort()).toEqual(["ready", "search", "version"]);
    await expect(api.ready()).resolves.toBeUndefined();
    expect(backend.search).not.toHaveBeenCalled();
    await expect(api.search("pear blossoms")).resolves.toEqual([
      { path: "Notes/Orchard.md", score: 4.5, heading: "Pears" },
      { path: "Notes/Orchard.md", score: 3 },
      { path: "Notes/Meadow.md", score: 2, heading: "Flowers" },
    ]);
    expect(backend.search).toHaveBeenCalledExactlyOnceWith({
      q: "pear blossoms",
      mode: profile === "daemon" ? "hybrid" : "lexical",
      limit: 20,
      ...(profile === "daemon" ? { filters: { vault_id: "synthetic-vault" } } : {}),
    });
  });

  it("returns a genuine empty result without converting failures to empty results", async () => {
    const { api, execution } = fixture();
    execution.response.hits = [];
    await expect(api.search("absent term")).resolves.toEqual([]);
  });

  it.each([undefined, null, 12, {}, [], "", "  \n\t"]) ("rejects invalid query %j before touching the backend", async (query) => {
    const { api, currentBackend } = fixture();
    await expect(api.search(query as string)).rejects.toMatchObject({
      name: "KwiryApiError", code: "invalid_query",
    });
    expect(currentBackend).not.toHaveBeenCalled();
  });

  it.each([null, [], "10", 10, { limit: "10" }, { limit: null }, { limit: NaN }, { limit: Infinity }])(
    "rejects invalid options %j", async (opts) => {
      const { api, currentBackend } = fixture();
      await expect(api.search("pear", opts as KwirySearchOptions)).rejects.toBeInstanceOf(KwiryApiError);
      await expect(api.search("pear", opts as KwirySearchOptions)).rejects.toMatchObject({ code: "invalid_options" });
      expect(currentBackend).not.toHaveBeenCalled();
    },
  );

  it.each([[undefined, 20], [0, 1], [-10, 1], [2.9, 2], [100, 100], [100_000, 100]])(
    "clamps limit %s to %s at the backend boundary", async (limit, expected) => {
      const { api, backend } = fixture();
      const results = await api.search("pear", { limit });
      expect(backend.search).toHaveBeenCalledWith(expect.objectContaining({ limit: expected }));
      expect(results.length).toBeLessThanOrEqual(expected!);
    },
  );

  it("rejects an unselected manager and a missing manager explicitly", async () => {
    const { backend } = fixture();
    const manager = new BackendManager({ daemon: () => backend, in_plugin: () => backend });
    for (const currentBackend of [async () => undefined, () => manager.current()]) {
      const { api } = createPublicSearchApi({ currentBackend, defaultMode: () => "lexical" });
      await expect(api.ready()).rejects.toMatchObject({ name: "KwiryApiError", code: "backend_unavailable" });
      await expect(api.search("pear")).rejects.toMatchObject({ code: "backend_unavailable" });
    }
  });

  it("rejects an unavailable backend without searching", async () => {
    const { api, status, backend } = fixture();
    status.phase = "unavailable";
    status.searchable = false;
    await expect(api.ready()).rejects.toMatchObject({ code: "backend_unavailable" });
    await expect(api.search("pear")).rejects.toMatchObject({ code: "backend_unavailable" });
    expect(backend.search).not.toHaveBeenCalled();
  });

  it("rejects while building, then allows a searchable generation even during rebuild", async () => {
    const { api, status, backend } = fixture();
    status.phase = "building";
    status.searchable = false;
    status.rebuilding = true;
    await expect(api.ready()).rejects.toMatchObject({ code: "index_not_ready" });
    await expect(api.search("pear")).rejects.toMatchObject({ code: "index_not_ready" });
    expect(backend.search).not.toHaveBeenCalled();
    status.searchable = true;
    await expect(api.ready()).resolves.toBeUndefined();
    await expect(api.search("pear")).resolves.toHaveLength(3);
  });

  it("never falls back from an unsupported configured daemon mode", async () => {
    const { api, status, backend } = fixture("daemon");
    status.capabilities.supportedModes = ["lexical"];
    await expect(api.ready()).rejects.toMatchObject({ code: "mode_unavailable" });
    await expect(api.search("pear")).rejects.toMatchObject({ code: "mode_unavailable" });
    expect(backend.search).not.toHaveBeenCalled();
  });

  it("requires a daemon current-vault mapping", async () => {
    const { api, backend } = fixture("daemon");
    backend.identity.boundVaultId = null;
    await expect(api.ready()).rejects.toMatchObject({ code: "vault_mapping_required" });
    await expect(api.search("pear")).rejects.toMatchObject({ code: "vault_mapping_required" });
    expect(backend.search).not.toHaveBeenCalled();
  });

  it.each(["/outside.md", "../outside.md", "Notes/../outside.md", "C:/outside.md", "Notes\\outside.md", "Notes//empty.md"])(
    "rejects non-vault-relative path %s rather than dropping the hit", async (path) => {
      const { api, execution } = fixture();
      execution.response.hits[0]!.path = path;
      await expect(api.search("pear")).rejects.toMatchObject({ code: "invalid_result" });
    },
  );

  it("rejects results from another vault instead of exposing ambiguous paths", async () => {
    const { api, execution } = fixture("daemon");
    execution.response.hits[0]!.vault_id = "another-synthetic-tree";
    await expect(api.search("pear")).rejects.toMatchObject({ code: "invalid_result" });
  });

  it("rejects a backend silently changing the requested mode", async () => {
    const { api, execution } = fixture("daemon");
    execution.effectiveMode = "lexical";
    await expect(api.search("pear")).rejects.toMatchObject({ code: "mode_unavailable" });
  });

  it.each([
    ["daemon_unreachable", "backend_unavailable"],
    ["mode_unavailable", "mode_unavailable"],
    ["index_building", "index_not_ready"],
    ["invalid_query", "invalid_query"],
    ["query_execution_failed", "search_failed"],
  ])("wraps backend %s as a safe named %s error", async (backendCode, code) => {
    const { api, backend } = fixture();
    vi.mocked(backend.search).mockRejectedValue(new KwiryBackendError(
      backendCode, "in_plugin", "query", false, "synthetic-sensitive-text",
    ));
    const error = await api.search("pear").catch((error: unknown) => error);
    expect(error).toBeInstanceOf(KwiryApiError);
    expect(error).toMatchObject({ name: "KwiryApiError", code });
    expect((error as Error).message).not.toContain("synthetic-sensitive-text");
    expect(error).not.toHaveProperty("cause");
  });

  it("sanitizes unknown exceptions", async () => {
    const { api, backend } = fixture();
    vi.mocked(backend.search).mockRejectedValue(new Error("synthetic-sensitive-text"));
    await expect(api.search("pear")).rejects.toMatchObject({
      name: "KwiryApiError", code: "search_failed", message: "The selected backend could not complete the search.",
    });
  });

  it("freezes hits and copies the array so neither side can mutate the other's state", async () => {
    const { api, execution } = fixture();
    const results = await api.search("pear");
    expect(results).not.toBe(execution.response.hits);
    expect(Object.isFrozen(results[0])).toBe(true);
    expect(Reflect.set(results[0]!, "score", 99)).toBe(false);
    results.pop();
    expect(execution.response.hits).toHaveLength(3);
    execution.response.hits[0]!.path = "Notes/Changed.md";
    execution.response.hits[0]!.heading_path.push("Changed");
    expect(results[0]).toEqual({ path: "Notes/Orchard.md", score: 4.5, heading: "Pears" });
    expect(await api.search("pear")).toHaveLength(3);
  });

  it("rejects cached API calls after unload, including ready and invalid queries", async () => {
    const { api, dispose, currentBackend } = fixture();
    dispose();
    dispose();
    await expect(api.ready()).rejects.toMatchObject({ code: "plugin_unloaded" });
    await expect(api.search("pear")).rejects.toMatchObject({ code: "plugin_unloaded" });
    await expect(api.search("")).rejects.toMatchObject({ code: "plugin_unloaded" });
    expect(currentBackend).not.toHaveBeenCalled();
  });

  it.each([false, true])("rejects an in-flight search on unload (backend rejects: %s)", async (reject) => {
    const { api, backend, dispose, execution } = fixture();
    let settle!: () => void;
    vi.mocked(backend.search).mockImplementation(() => new Promise((resolve, rejectSearch) => {
      settle = () => reject ? rejectSearch(new Error("synthetic failure")) : resolve(execution);
    }));
    const result = api.search("pear");
    await vi.waitFor(() => expect(backend.search).toHaveBeenCalledOnce());
    dispose();
    settle();
    await expect(result).rejects.toMatchObject({ code: "plugin_unloaded" });
  });

  it("rejects ready when unloaded during a status check", async () => {
    const { api, backend, dispose, status } = fixture();
    let settle!: () => void;
    vi.mocked(backend.status).mockImplementation(() => new Promise((resolve) => {
      settle = () => resolve(status);
    }));
    const result = api.ready();
    await vi.waitFor(() => expect(backend.status).toHaveBeenCalledOnce());
    dispose();
    settle();
    await expect(result).rejects.toMatchObject({ code: "plugin_unloaded" });
  });

  it("rejects results if the selected backend changes during search", async () => {
    const { api, backend, currentBackend, execution } = fixture();
    vi.mocked(backend.search).mockImplementation(async () => {
      currentBackend.mockResolvedValue({ ...backend, identity: { ...backend.identity, instanceId: "replacement" } });
      return execution;
    });
    await expect(api.search("pear")).rejects.toMatchObject({ code: "backend_changed" });
  });
});
