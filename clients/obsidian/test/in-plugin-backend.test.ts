// SPDX-FileCopyrightText: 2026 cybersader
// SPDX-License-Identifier: GPL-3.0-only

import { describe, expect, it, vi } from "vitest";

import { InPluginLexicalBackend } from "../src/backends/in-plugin-lexical-backend";
import { FTS_HIGHLIGHT_END, FTS_HIGHLIGHT_START } from "../src/excerpt";
import type { StatusResult } from "../src/worker/protocol";
import type { InPluginWorkerSession } from "../src/worker/session";

function fakeSession(status: StatusResult): InPluginWorkerSession {
  return {
    initialize: vi.fn(async () => ({
      rustAbiVersion: 1,
      sourceSchemaVersion: 1,
      querySchemaVersion: 2,
      matchPlanSchemaVersion: 1,
      sqliteVersion: "3.53.0",
      fts5Enabled: 1,
    })),
    status: vi.fn(async () => status),
    search: vi.fn(async () => ({
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
    })),
    dispose: vi.fn(async () => ({ closed: true as const })),
    forceDispose: vi.fn(),
  } as unknown as InPluginWorkerSession;
}

function backend(session: InPluginWorkerSession): InPluginLexicalBackend {
  return new InPluginLexicalBackend({
    instanceId: "in_plugin-2",
    activeVaultId: "active-vault",
    workerSource: "worker source",
    createSession: () => session,
  });
}

describe("InPluginLexicalBackend", () => {
  it("initializes only its Worker and truthfully reports an absent active generation", async () => {
    const session = fakeSession({
      phase: "building",
      searchable: false,
      active_generation: null,
      staging_generation: null,
      documents: 0,
      chunks: 0,
      dirty: true,
      rebuilding: false,
    });
    const inPlugin = backend(session);
    await inPlugin.initialize();
    await expect(inPlugin.status()).resolves.toMatchObject({
      phase: "building",
      searchable: false,
      capabilities: { supportedModes: ["lexical"], sourceScope: "active_vault" },
      issue: { code: "index_building" },
    });
    await expect(inPlugin.search({ q: "query", mode: "lexical" })).rejects.toMatchObject({
      code: "index_building",
    });
  });

  it("never accepts semantic or hybrid requests", async () => {
    const session = fakeSession({
      phase: "ready",
      searchable: true,
      active_generation: "generation-1",
      staging_generation: null,
      documents: 1,
      chunks: 1,
      dirty: false,
      rebuilding: false,
    });
    const inPlugin = backend(session);
    await inPlugin.initialize();
    await expect(inPlugin.search({ q: "query", mode: "semantic" })).rejects.toMatchObject({
      code: "mode_unavailable",
    });
  });

  it("normalizes Worker excerpts and attaches in-plugin result origin", async () => {
    const session = fakeSession({
      phase: "ready",
      searchable: true,
      active_generation: "generation-1",
      staging_generation: null,
      documents: 1,
      chunks: 1,
      dirty: false,
      rebuilding: false,
    });
    const inPlugin = backend(session);
    await inPlugin.initialize();
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

  it("disposes the Worker exactly once", async () => {
    const session = fakeSession({
      phase: "building",
      searchable: false,
      active_generation: null,
      staging_generation: null,
      documents: 0,
      chunks: 0,
      dirty: true,
      rebuilding: false,
    });
    const inPlugin = backend(session);
    await inPlugin.initialize();
    await inPlugin.dispose();
    await inPlugin.dispose();
    expect(session.dispose).toHaveBeenCalledTimes(1);
    await expect(inPlugin.status()).resolves.toMatchObject({ phase: "disposed" });
  });
});
