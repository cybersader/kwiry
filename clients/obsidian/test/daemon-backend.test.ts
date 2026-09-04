// SPDX-FileCopyrightText: 2026 cybersader
// SPDX-License-Identifier: GPL-3.0-only

import { describe, expect, it } from "vitest";

import type { Transport } from "../src/api";
import { DaemonBackend } from "../src/backends/daemon-backend";

const TOKEN = "A".repeat(43);
const SOURCE_FORMAT_COUNTS = {
  markdown: {
    "indexed-complete": 2,
    "indexed-partial": 0,
    "skipped-no-extractable-text": 0,
    unreadable: 0,
    quarantined: 0,
  },
  text: {
    "indexed-complete": 0,
    "indexed-partial": 0,
    "skipped-no-extractable-text": 0,
    unreadable: 0,
    quarantined: 0,
  },
  base: {
    "indexed-complete": 0,
    "indexed-partial": 0,
    "skipped-no-extractable-text": 0,
    unreadable: 0,
    quarantined: 0,
  },
  canvas: {
    "indexed-complete": 0,
    "indexed-partial": 0,
    "skipped-no-extractable-text": 0,
    unreadable: 0,
    quarantined: 0,
  },
  docx: {
    "indexed-complete": 0,
    "indexed-partial": 0,
    "skipped-no-extractable-text": 0,
    unreadable: 0,
    quarantined: 0,
  },
  pdf: {
    "indexed-complete": 0,
    "indexed-partial": 0,
    "skipped-no-extractable-text": 0,
    unreadable: 0,
    quarantined: 0,
  },
  excalidraw: {
    "indexed-complete": 0,
    "indexed-partial": 0,
    "skipped-no-extractable-text": 0,
    unreadable: 0,
    quarantined: 0,
  },
  excel: {
    "indexed-complete": 0,
    "indexed-partial": 0,
    "skipped-no-extractable-text": 0,
    unreadable: 0,
    quarantined: 0,
  },
  html: {
    "indexed-complete": 0,
    "indexed-partial": 0,
    "skipped-no-extractable-text": 0,
    unreadable: 0,
    quarantined: 0,
  },
};
const STATUS = {
  state: "ready",
  version: "0.1.0",
  generation: "generation-1",
  chunking_version: 1,
  documents: 2,
  chunks: 4,
  source_format_counts: SOURCE_FORMAT_COUNTS,
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
  format: "markdown",
  coverage: "indexed-complete",
  locator: null,
  heading_path: ["Heading"],
  score: 1,
  excerpt: "before <b>match</b> after",
  frontmatter: {},
};

function jsonTransport(handler: (url: string) => {
  status: number;
  body: unknown;
  headers?: Record<string, string>;
}): Transport {
  return async (options) => {
    const response = handler(options.url);
    return {
      status: response.status,
      text: JSON.stringify(response.body),
      headers: response.headers,
    };
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
    expect(execution.lexicalMatchQuality).toEqual({ availability: "not_applicable" });
    expect(execution.candidateWindow).toEqual({
      state: "unknown",
      candidateCount: null,
      candidateLimit: null,
    });
    expect(execution.response.hits[0]).toMatchObject({
      path: "folder/note.md",
      format: "markdown",
      coverage: "indexed-complete",
      locator: null,
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

  it("carries closed field-policy facts from a beta.27 daemon", async () => {
    const daemon = backend(jsonTransport((url) => url.endsWith("/v0/status")
      ? {
          status: 200,
          body: { ...STATUS, model: { name: "model", version: "1" } },
        }
      : {
          status: 200,
          body: { hits: [HIT], next_cursor: null },
          headers: {
            "x-kwiry-lexical-profile": "lexical-v2",
            "x-kwiry-field-scope": "none",
            "x-kwiry-field-emphasis": "body",
          },
        }));
    await daemon.status();

    const execution = await daemon.search({ q: ">body match", mode: "hybrid" });
    expect(execution.queryPolicy).toEqual({
      lexical_profile: "lexical-v2",
      scope: null,
      emphasis: "body",
    });
  });

  it("projects closed lexical match quality from an upgraded daemon", async () => {
    const daemon = backend(jsonTransport((url) => url.endsWith("/v0/status")
      ? { status: 200, body: STATUS }
      : {
          status: 200,
          body: { hits: [HIT], next_cursor: null },
          headers: { "X-Kwiry-Lexical-Match-Quality": "partial_only" },
        }));
    await daemon.status();

    const execution = await daemon.search({ q: "match", mode: "lexical" });
    expect(execution.lexicalMatchQuality).toEqual({
      availability: "available",
      value: "partial_only",
    });
  });

  it("rejects match quality that contradicts the effective mode", async () => {
    const daemon = backend(jsonTransport((url) => url.endsWith("/v0/status")
      ? { status: 200, body: STATUS }
      : {
          status: 200,
          body: { hits: [HIT], next_cursor: null },
          headers: { "x-kwiry-lexical-match-quality": "not_applicable" },
        }));
    await daemon.status();

    await expect(daemon.search({ q: "match", mode: "lexical" })).rejects.toMatchObject({
      code: "invalid_response",
      stage: "protocol",
    });
  });

  it("requires a beta.27 daemon for control-bearing queries without policy headers", async () => {
    const daemon = backend(jsonTransport((url) => url.endsWith("/v0/status")
      ? { status: 200, body: STATUS }
      : { status: 200, body: { hits: [HIT], next_cursor: null } }));
    await daemon.status();

    await expect(daemon.search({ q: "in:name match", mode: "lexical" }))
      .rejects.toMatchObject({
        code: "daemon_upgrade_required",
        safeMessage: "Field controls require a beta.27-compatible daemon.",
      });
  });

  it("does not infer exhaustion or a total from an exact-limit response with next_cursor null", async () => {
    const exactLimitHits = Array.from({ length: 100 }, (_, index) => ({
      ...HIT,
      chunk_id: `chunk-${index}`,
      path: `note-${index}.md`,
    }));
    const daemon = backend(jsonTransport((url) =>
      url.endsWith("/v0/status")
        ? { status: 200, body: STATUS }
        : { status: 200, body: { hits: exactLimitHits, next_cursor: null } },
    ));
    await daemon.status();

    const execution = await daemon.search({ q: "match", mode: "lexical", limit: 100 });
    expect(execution.response.hits).toHaveLength(100);
    expect(execution.lexicalMatchQuality).toEqual({ availability: "unavailable" });
    expect(execution.candidateWindow).toEqual({
      state: "unknown",
      candidateCount: null,
      candidateLimit: null,
    });
  });

  it("reports more_available only when the daemon supplies a continuation cursor", async () => {
    const daemon = backend(jsonTransport((url) =>
      url.endsWith("/v0/status")
        ? { status: 200, body: STATUS }
        : { status: 200, body: { hits: [HIT], next_cursor: "opaque-next-page" } },
    ));
    await daemon.status();

    const execution = await daemon.search({ q: "match", mode: "lexical", limit: 100 });
    expect(execution.candidateWindow).toEqual({
      state: "more_available",
      candidateCount: null,
      candidateLimit: null,
    });
  });

  it("rejects search hits with invalid coverage or format-incompatible locators", async () => {
    const invalidHits = [
      { ...HIT, coverage: "complete" },
      { ...HIT, locator: { kind: "base_view", view: "Table" } },
    ];

    for (const invalidHit of invalidHits) {
      const daemon = backend(jsonTransport((url) =>
        url.endsWith("/v0/status")
          ? { status: 200, body: STATUS }
          : { status: 200, body: { hits: [invalidHit], next_cursor: null } },
      ));
      await daemon.status();
      await expect(daemon.search({ q: "match", mode: "lexical" })).rejects.toMatchObject({
        code: "invalid_response",
      });
    }
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
