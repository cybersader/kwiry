// SPDX-FileCopyrightText: 2026 cybersader
// SPDX-License-Identifier: GPL-3.0-only

import { describe, expect, it, vi } from "vitest";

import {
  type ActiveVaultSource,
  type SourceInspection,
  type StableSourceRead,
  type VaultSourceEvent,
} from "../src/active-vault-source";
import { InPluginLexicalBackend } from "../src/backends/in-plugin-lexical-backend";
import { FTS_HIGHLIGHT_END, FTS_HIGHLIGHT_START } from "../src/excerpt";
import { WorkerRpcError } from "../src/worker/rpc-client";
import type { InPluginWorkerSession } from "../src/worker/session";

class FakeSource implements ActiveVaultSource {
  listener: ((event: VaultSourceEvent) => void) | null = null;
  subscriptions = 0;
  unsubscriptions = 0;

  subscribe(listener: (event: VaultSourceEvent) => void): () => void {
    if (this.listener) throw new Error("already subscribed");
    this.listener = listener;
    this.subscriptions += 1;
    return () => {
      if (!this.listener) return;
      this.listener = null;
      this.unsubscriptions += 1;
    };
  }

  listMarkdownPaths(): readonly string[] {
    return [];
  }

  inspectMarkdown(path: string): SourceInspection {
    return { kind: "missing", path };
  }

  async readMarkdown(
    inspection: Extract<SourceInspection, { kind: "candidate" }>,
  ): Promise<StableSourceRead> {
    return { kind: "missing", path: inspection.path };
  }

  emit(event: VaultSourceEvent): void {
    this.listener?.(event);
  }
}

function fakeSession(options: {
  initialize?: () => Promise<unknown>;
  search?: () => Promise<unknown>;
} = {}): InPluginWorkerSession {
  return {
    initialize: vi.fn(options.initialize ?? (async () => ({}))),
    beginBuild: vi.fn(async (generation: string) => ({
      generation,
      documents: 0,
      chunks: 0,
    })),
    addSourceBatch: vi.fn(async (generation: string) => ({
      generation,
      documents: 0,
      chunks: 0,
    })),
    applySourceChanges: vi.fn(async (
      generation: string,
      nextGeneration: string | null,
    ) => ({
      generation: nextGeneration ?? generation,
      documents: 0,
      chunks: 0,
    })),
    commitBuild: vi.fn(async (generation: string) => ({
      generation,
      documents: 0,
      chunks: 0,
    })),
    abortBuild: vi.fn(async (generation: string) => ({
      generation,
      documents: 0,
      chunks: 0,
    })),
    search: vi.fn(options.search ?? (async () => ({
      generation: "generation-1",
      hits: [{
        chunk_id: "chunk-1",
        vault_id: "active-vault",
        path: "note.md",
        heading_path: ["Heading"],
        score: 1,
        excerpt: `before ${FTS_HIGHLIGHT_START}match${FTS_HIGHLIGHT_END}`,
        frontmatter: {},
      }],
    }))),
    dispose: vi.fn(async () => ({ closed: true as const })),
    forceDispose: vi.fn(),
  } as unknown as InPluginWorkerSession;
}

function backend(
  source: FakeSource,
  sessions: InPluginWorkerSession[],
): InPluginLexicalBackend {
  let generation = 0;
  return new InPluginLexicalBackend({
    instanceId: "in_plugin-2",
    activeVaultId: "active-vault",
    source,
    workerSource: "worker source",
    createSession: () => {
      const session = sessions.shift();
      if (!session) throw new Error("missing fake session");
      return session;
    },
    nextGeneration: () => `generation-${++generation}`,
    yieldControl: () => Promise.resolve(),
  });
}

describe("InPluginLexicalBackend", () => {
  it("subscribes before returning and builds a complete initial generation in the background", async () => {
    let release!: () => void;
    const initializing = new Promise<void>((resolve) => {
      release = resolve;
    });
    const source = new FakeSource();
    const session = fakeSession({ initialize: () => initializing });
    const inPlugin = backend(source, [session]);

    await inPlugin.initialize();
    expect(source.subscriptions).toBe(1);
    await expect(inPlugin.status()).resolves.toMatchObject({
      phase: "starting",
      searchable: false,
      capabilities: {
        supportedModes: ["lexical"],
        sourceScope: "active_vault",
        manualRebuild: true,
      },
      issue: { code: "index_building" },
    });

    release();
    await vi.waitFor(async () => {
      await expect(inPlugin.status()).resolves.toMatchObject({
        phase: "ready",
        searchable: true,
        generation: "generation-1",
      });
    });
  });

  it("emits cached status immediately and starts a manual replacement build", async () => {
    const source = new FakeSource();
    const session = fakeSession();
    const inPlugin = backend(source, [session]);
    const phases: string[] = [];
    const unsubscribe = inPlugin.subscribeStatus((status) => phases.push(status.phase));

    await inPlugin.initialize();
    await vi.waitFor(() => expect(session.commitBuild).toHaveBeenCalledTimes(1));
    await inPlugin.rebuild();
    await vi.waitFor(() => expect(session.commitBuild).toHaveBeenCalledTimes(2));

    expect(phases[0]).toBe("starting");
    expect(phases).toContain("building");
    expect(phases.at(-1)).toBe("ready");
    unsubscribe();
  });

  it("keeps the old active generation searchable and drains events during rebuild", async () => {
    let release!: () => void;
    const gate = new Promise<void>((resolve) => {
      release = resolve;
    });
    const source = new FakeSource();
    const session = fakeSession();
    const originalCommit = session.commitBuild.bind(session);
    let commits = 0;
    session.commitBuild = vi.fn(async (generation: string) => {
      commits += 1;
      if (commits === 2) await gate;
      return originalCommit(generation);
    });
    const inPlugin = backend(source, [session]);
    await inPlugin.initialize();
    await vi.waitFor(async () => {
      await expect(inPlugin.status()).resolves.toMatchObject({
        searchable: true,
        generation: "generation-1",
      });
    });

    await inPlugin.rebuild();
    await vi.waitFor(async () => {
      await expect(inPlugin.status()).resolves.toMatchObject({
        phase: "building",
        searchable: true,
        generation: "generation-1",
        rebuilding: true,
      });
    });
    await expect(inPlugin.search({ q: "query", mode: "lexical" })).resolves.toMatchObject({
      generation: "generation-1",
    });
    source.emit({ kind: "remove", path: "deleted-during-rebuild.md" });
    release();
    await vi.waitFor(async () => {
      await expect(inPlugin.status()).resolves.toMatchObject({
        phase: "ready",
        generation: "generation-3",
        dirty: false,
      });
    });
  });

  it("never accepts semantic or hybrid requests", async () => {
    const source = new FakeSource();
    const inPlugin = backend(source, [fakeSession()]);
    await inPlugin.initialize();
    await vi.waitFor(async () => {
      await expect(inPlugin.status()).resolves.toMatchObject({ searchable: true });
    });

    await expect(inPlugin.search({ q: "query", mode: "semantic" })).rejects.toMatchObject({
      code: "mode_unavailable",
    });
  });

  it("normalizes Worker excerpts and attaches in-plugin result origin", async () => {
    const source = new FakeSource();
    const inPlugin = backend(source, [fakeSession()]);
    await inPlugin.initialize();
    await vi.waitFor(async () => {
      await expect(inPlugin.status()).resolves.toMatchObject({ searchable: true });
    });

    const execution = await inPlugin.search({ q: "query", mode: "lexical", limit: 10 });
    expect(execution).toMatchObject({
      requestedMode: "lexical",
      effectiveMode: "lexical",
      generation: "generation-1",
      response: {
        hits: [{
          excerpt: [
            { text: "before ", highlighted: false },
            { text: "match", highlighted: true },
          ],
          origin: {
            profile: "in_plugin",
            backendInstanceId: "in_plugin-2",
            vaultId: "active-vault",
          },
        }],
      },
    });
  });

  it("keeps the active generation searchable after a definitive live update failure", async () => {
    const source = new FakeSource();
    const session = fakeSession();
    session.applySourceChanges = vi.fn(async () => {
      throw new WorkerRpcError({
        code: "source_rejected",
        stage: "index",
        message: "private detail",
        retryable: false,
      });
    });
    const inPlugin = backend(source, [session]);
    await inPlugin.initialize();
    await vi.waitFor(async () => {
      await expect(inPlugin.status()).resolves.toMatchObject({ searchable: true });
    });

    source.emit({ kind: "remove", path: "deleted.md" });
    await vi.waitFor(async () => {
      await expect(inPlugin.status()).resolves.toMatchObject({
        phase: "degraded",
        searchable: true,
        generation: "generation-1",
        dirty: true,
        issue: {
          code: "index_update_failed",
          safeMessage: "The in-plugin lexical index could not be updated.",
        },
      });
    });
    await expect(inPlugin.search({ q: "query", mode: "lexical" })).resolves.toMatchObject({
      generation: "generation-1",
    });
  });

  it("reports capacity exhaustion while preserving the active generation", async () => {
    const source = new FakeSource();
    const session = fakeSession();
    session.applySourceChanges = vi.fn(async () => {
      throw new WorkerRpcError({
        code: "index_limit_exceeded",
        stage: "index",
        message: "private detail",
        retryable: false,
      });
    });
    const inPlugin = backend(source, [session]);
    await inPlugin.initialize();
    await vi.waitFor(async () => {
      await expect(inPlugin.status()).resolves.toMatchObject({ searchable: true });
    });

    source.emit({ kind: "remove", path: "deleted.md" });
    await vi.waitFor(async () => {
      await expect(inPlugin.status()).resolves.toMatchObject({
        phase: "degraded",
        searchable: true,
        generation: "generation-1",
        issue: {
          code: "index_limit_exceeded",
          safeMessage: "The in-plugin lexical index reached its capacity limit.",
        },
      });
    });
  });

  it("replaces the Worker after an uncertain active mutation", async () => {
    const source = new FakeSource();
    const failed = fakeSession();
    failed.applySourceChanges = vi.fn(async () => {
      throw new WorkerRpcError({
        code: "timeout",
        stage: "lifecycle",
        message: "private detail",
        retryable: true,
      });
    });
    const replacement = fakeSession();
    const inPlugin = backend(source, [failed, replacement]);
    await inPlugin.initialize();
    await vi.waitFor(async () => {
      await expect(inPlugin.status()).resolves.toMatchObject({ generation: "generation-1" });
    });

    source.emit({ kind: "remove", path: "deleted.md" });
    await vi.waitFor(async () => {
      await expect(inPlugin.status()).resolves.toMatchObject({
        phase: "ready",
        searchable: true,
        generation: "generation-3",
      });
    });

    expect(failed.forceDispose).toHaveBeenCalledTimes(1);
    expect(source.unsubscriptions).toBe(1);
    expect(source.subscriptions).toBe(2);
  });

  it("recovers an uncertain initial Worker failure through one fresh complete build", async () => {
    const source = new FakeSource();
    const failed = fakeSession({
      initialize: async () => {
        throw new WorkerRpcError({
          code: "timeout",
          stage: "lifecycle",
          message: "private detail",
          retryable: true,
        });
      },
    });
    const replacement = fakeSession();
    const inPlugin = backend(source, [failed, replacement]);

    await inPlugin.initialize();
    await vi.waitFor(async () => {
      await expect(inPlugin.status()).resolves.toMatchObject({
        phase: "ready",
        searchable: true,
        generation: "generation-1",
      });
    });

    expect(failed.forceDispose).toHaveBeenCalledTimes(1);
    expect(source.unsubscriptions).toBe(1);
    expect(source.subscriptions).toBe(2);
  });

  it("recovers when an uncertain build failure is followed by a definitive abort error", async () => {
    const source = new FakeSource();
    const failed = fakeSession();
    failed.commitBuild = vi.fn(async () => {
      throw new WorkerRpcError({
        code: "timeout",
        stage: "lifecycle",
        message: "private detail",
        retryable: true,
      });
    });
    failed.abortBuild = vi.fn(async () => {
      throw new WorkerRpcError({
        code: "invalid_state",
        stage: "index",
        message: "private detail",
        retryable: false,
      });
    });
    const replacement = fakeSession();
    const inPlugin = backend(source, [failed, replacement]);

    await inPlugin.initialize();
    await vi.waitFor(async () => {
      await expect(inPlugin.status()).resolves.toMatchObject({
        phase: "ready",
        searchable: true,
        generation: "generation-2",
      });
    });

    expect(failed.forceDispose).toHaveBeenCalledTimes(1);
  });

  it("reports a definitive replacement-build failure instead of claiming endless recovery", async () => {
    const source = new FakeSource();
    const failed = fakeSession({
      initialize: async () => {
        throw new WorkerRpcError({
          code: "worker_crashed",
          stage: "lifecycle",
          message: "private detail",
          retryable: true,
        });
      },
    });
    const replacement = fakeSession();
    replacement.beginBuild = vi.fn(async () => {
      throw new WorkerRpcError({
        code: "source_rejected",
        stage: "index",
        message: "private detail",
        retryable: false,
      });
    });
    const inPlugin = backend(source, [failed, replacement]);

    await inPlugin.initialize();
    await vi.waitFor(async () => {
      await expect(inPlugin.status()).resolves.toMatchObject({
        phase: "unavailable",
        searchable: false,
        issue: {
          code: "index_build_failed",
          safeMessage: "The in-plugin lexical index could not be built.",
        },
      });
    });
  });

  it("terminates an uncertain Worker before starting one fresh recovery build", async () => {
    const source = new FakeSource();
    const failed = fakeSession({
      search: async () => {
        throw new WorkerRpcError({
          code: "worker_crashed",
          stage: "lifecycle",
          message: "private detail",
          retryable: true,
        });
      },
    });
    const replacement = fakeSession();
    const inPlugin = backend(source, [failed, replacement]);
    await inPlugin.initialize();
    await vi.waitFor(async () => {
      await expect(inPlugin.status()).resolves.toMatchObject({ searchable: true });
    });

    await expect(inPlugin.search({ q: "query", mode: "lexical" })).rejects.toMatchObject({
      code: "worker_recovering",
      safeMessage: "In-plugin search Worker is recovering.",
    });
    expect(failed.forceDispose).toHaveBeenCalledTimes(1);
    expect(source.unsubscriptions).toBe(1);
    expect(source.subscriptions).toBe(2);
    await vi.waitFor(async () => {
      await expect(inPlugin.status()).resolves.toMatchObject({
        phase: "ready",
        searchable: true,
        generation: "generation-2",
      });
    });
  });

  it("prevents a held initial commit from publishing after disposal", async () => {
    let release!: () => void;
    const gate = new Promise<void>((resolve) => {
      release = resolve;
    });
    const source = new FakeSource();
    const session = fakeSession();
    const originalCommit = session.commitBuild.bind(session);
    session.commitBuild = vi.fn(async (generation: string) => {
      await gate;
      return originalCommit(generation);
    });
    const inPlugin = backend(source, [session]);

    await inPlugin.initialize();
    await vi.waitFor(() => expect(session.commitBuild).toHaveBeenCalledTimes(1));
    await inPlugin.dispose();
    release();
    await Promise.resolve();
    await Promise.resolve();

    expect(source.unsubscriptions).toBe(1);
    expect(session.forceDispose).toHaveBeenCalledTimes(1);
    await expect(inPlugin.status()).resolves.toMatchObject({
      phase: "disposed",
      searchable: false,
    });
  });

  it("rejects a search that completes after backend disposal", async () => {
    let release!: () => void;
    const pendingSearch = new Promise<unknown>((resolve) => {
      release = () => resolve({ generation: "generation-1", hits: [] });
    });
    const source = new FakeSource();
    const session = fakeSession({ search: () => pendingSearch });
    const inPlugin = backend(source, [session]);
    await inPlugin.initialize();
    await vi.waitFor(async () => {
      await expect(inPlugin.status()).resolves.toMatchObject({ searchable: true });
    });

    const pending = inPlugin.search({ q: "query", mode: "lexical" });
    await inPlugin.dispose();
    release();

    await expect(pending).rejects.toMatchObject({ code: "disposed" });
  });

  it("returns listener and Worker counts to zero across repeated lifecycles", async () => {
    for (let cycle = 0; cycle < 10; cycle += 1) {
      const source = new FakeSource();
      const session = fakeSession();
      const inPlugin = backend(source, [session]);
      await inPlugin.initialize();
      await vi.waitFor(async () => {
        await expect(inPlugin.status()).resolves.toMatchObject({ phase: "ready" });
      });
      await inPlugin.dispose();
      expect(source.listener).toBeNull();
      expect(source.subscriptions).toBe(1);
      expect(source.unsubscriptions).toBe(1);
      expect(session.forceDispose).toHaveBeenCalledTimes(1);
    }
  });

  it("detaches the vault and force-disposes the Worker exactly once", async () => {
    const source = new FakeSource();
    const session = fakeSession();
    const inPlugin = backend(source, [session]);
    await inPlugin.initialize();
    await inPlugin.dispose();
    await inPlugin.dispose();

    expect(source.unsubscriptions).toBe(1);
    expect(session.forceDispose).toHaveBeenCalledTimes(1);
    await expect(inPlugin.status()).resolves.toMatchObject({ phase: "disposed" });
  });
});
