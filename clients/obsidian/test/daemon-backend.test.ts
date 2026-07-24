// SPDX-FileCopyrightText: 2026 cybersader
// SPDX-License-Identifier: GPL-3.0-only

import { describe, expect, it } from "vitest";

import type { Transport } from "../src/api";
import { DaemonBackend } from "../src/backends/daemon-backend";

const TOKEN = "A".repeat(43);
const STATUS = {
  state: "ready",
  version: "0.1.0",
  generation: "generation-1",
  chunking_version: 1,
  documents: 2,
  chunks: 4,
  last_sync: null,
  dirty: false,
  rebuilding: false,
  model: null,
  vaults: [],
};
const HIT = {
  chunk_id: "chunk-1",
  vault_id: "notes",
  path: "folder/note.md",
  heading_path: ["Heading"],
  score: 1,
  excerpt: "before <b>match</b> after",
  frontmatter: {},
};

function jsonTransport(handler: (url: string) => { status: number; body: unknown }): Transport {
  return async (options) => {
    const response = handler(options.url);
    return { status: response.status, text: JSON.stringify(response.body) };
  };
}

function backend(transport: Transport, tokenProvider = () => TOKEN): DaemonBackend {
  return new DaemonBackend({
    instanceId: "daemon-7",
    baseUrl: "http://127.0.0.1:32189",
    currentVaultId: "notes",
    tokenProvider,
    transport,
  });
}

describe("DaemonBackend", () => {
  it("maps daemon status and attaches immutable result origin", async () => {
    const transport = jsonTransport((url) => {
      if (url.endsWith("/v0/status")) {
        return {
          status: 200,
          body: {
            ...STATUS,
            model: { name: "model", version: "1" },
          },
        };
      }
      return { status: 200, body: { hits: [HIT], next_cursor: null } };
    });
    const daemon = backend(transport);

    const status = await daemon.status();
    expect(status).toMatchObject({
      phase: "ready",
      liveness: "alive",
      searchable: true,
      generation: "generation-1",
      capabilities: { supportedModes: ["lexical", "semantic", "hybrid"] },
    });

    const execution = await daemon.search({ q: "match", mode: "hybrid" });
    expect(execution.requestedMode).toBe("hybrid");
    expect(execution.effectiveMode).toBe("hybrid");
    expect(execution.response.hits[0]).toMatchObject({
      path: "folder/note.md",
      excerpt: [
        { text: "before ", highlighted: false },
        { text: "match", highlighted: true },
        { text: " after", highlighted: false },
      ],
      origin: {
        profile: "daemon",
        backendInstanceId: "daemon-7",
        vaultId: "notes",
      },
    });
  });

  it("reads the token provider for each authenticated status and search request", async () => {
    let tokenReads = 0;
    const transport = jsonTransport((url) =>
      url.endsWith("/v0/status")
        ? { status: 200, body: STATUS }
        : { status: 200, body: { hits: [], next_cursor: null } },
    );
    const daemon = backend(transport, () => {
      tokenReads += 1;
      return TOKEN;
    });

    await daemon.status();
    await daemon.search({ q: "query", mode: "lexical" });
    expect(tokenReads).toBe(2);
  });

  it("reports invalid configuration without making a transport request", async () => {
    let calls = 0;
    const daemon = new DaemonBackend({
      instanceId: "daemon-1",
      baseUrl: "https://example.com",
      currentVaultId: null,
      tokenProvider: () => TOKEN,
      transport: async () => {
        calls += 1;
        return { status: 200, text: "{}" };
      },
    });

    await expect(daemon.status()).resolves.toMatchObject({
      phase: "unavailable",
      searchable: false,
      issue: { code: "invalid_daemon_url" },
    });
    expect(calls).toBe(0);
  });

  it("does not mask authentication failure as mode unavailability", async () => {
    const daemon = backend(jsonTransport(() => ({
      status: 401,
      body: { error: { code: "unauthorized", message: "private detail" } },
    })));

    await daemon.status();
    await expect(daemon.search({ q: "query", mode: "semantic" })).rejects.toMatchObject({
      code: "authentication_failed",
      safeMessage: "Daemon authentication failed.",
    });
  });

  it("returns mode_unavailable for a ready lexical-only daemon", async () => {
    const daemon = backend(jsonTransport(() => ({ status: 200, body: STATUS })));
    await daemon.status();
    await expect(daemon.search({ q: "query", mode: "semantic" })).rejects.toMatchObject({
      code: "mode_unavailable",
    });
  });

  it("rejects a request that completes after explicit backend disposal", async () => {
    let finishSearch!: () => void;
    const transport: Transport = async (options) => {
      if (options.url.endsWith("/v0/status")) {
        return { status: 200, text: JSON.stringify(STATUS) };
      }
      await new Promise<void>((resolve) => {
        finishSearch = resolve;
      });
      return { status: 200, text: JSON.stringify({ hits: [], next_cursor: null }) };
    };
    const daemon = backend(transport);
    await daemon.status();
    const pending = daemon.search({ q: "query", mode: "lexical" });
    await Promise.resolve();
    await daemon.dispose();
    finishSearch();

    await expect(pending).rejects.toMatchObject({ code: "disposed" });
  });

  it("becomes inert after disposal", async () => {
    const daemon = backend(jsonTransport(() => ({ status: 200, body: STATUS })));
    await daemon.dispose();
    await expect(daemon.status()).resolves.toMatchObject({ phase: "disposed" });
    await expect(daemon.search({ q: "query", mode: "lexical" })).rejects.toMatchObject({
      code: "disposed",
    });
  });
});
