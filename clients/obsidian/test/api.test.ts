// SPDX-FileCopyrightText: 2026 cybersader
// SPDX-License-Identifier: GPL-3.0-only
import { describe, expect, it } from "vitest";

import { KwiryApiError, KwiryClient, type Transport } from "../src/api";

interface Call {
  url: string;
  method: string;
  headers: Record<string, string>;
  body?: string;
}

const TOKEN_A = "A".repeat(43);
const TOKEN_B = "B".repeat(43);

function mockTransport(
  status: number,
  responseBody: unknown,
): { transport: Transport; calls: Call[] } {
  const calls: Call[] = [];
  const transport: Transport = async (options) => {
    calls.push(options);
    return { status, text: JSON.stringify(responseBody) };
  };
  return { transport, calls };
}

const HIT = {
  chunk_id: "c1",
  vault_id: "notes",
  path: "a/b.md",
  heading_path: ["A", "B"],
  score: 1.5,
  excerpt: "text",
  frontmatter: {},
};

function client(transport: Transport, tokenProvider = () => TOKEN_A): KwiryClient {
  return new KwiryClient({
    baseUrl: "http://127.0.0.1:32189",
    tokenProvider,
    transport,
  });
}

describe("KwiryClient.search", () => {
  it("sends an authenticated POST with mode and limit", async () => {
    const { transport, calls } = mockTransport(200, { hits: [HIT], next_cursor: null });
    const response = await client(transport).search({ q: "query", mode: "hybrid" });

    expect(response.hits).toHaveLength(1);
    expect(calls).toHaveLength(1);
    const call = calls[0]!;
    expect(call.url).toBe("http://127.0.0.1:32189/v0/search");
    expect(call.method).toBe("POST");
    expect(call.headers["Authorization"]).toBe(`Bearer ${TOKEN_A}`);
    expect(JSON.parse(call.body!)).toEqual({ q: "query", mode: "hybrid", limit: 20 });
  });

  it("reads the token provider fresh for every authenticated request", async () => {
    const { transport, calls } = mockTransport(200, { hits: [], next_cursor: null });
    const tokens = [TOKEN_A, TOKEN_B];
    const rotating = client(transport, () => tokens.shift()!);

    await rotating.search({ q: "first", mode: "lexical" });
    await rotating.search({ q: "second", mode: "lexical" });

    expect(calls.map((call) => call.headers["Authorization"])).toEqual([
      `Bearer ${TOKEN_A}`,
      `Bearer ${TOKEN_B}`,
    ]);
  });

  it("includes filters only when non-empty", async () => {
    const { transport, calls } = mockTransport(200, { hits: [], next_cursor: null });
    const kwiry = client(transport);
    await kwiry.search({ q: "a", mode: "lexical", filters: {} });
    await kwiry.search({ q: "a", mode: "lexical", filters: { vault_id: "notes" } });
    expect(JSON.parse(calls[0]!.body!)).not.toHaveProperty("filters");
    expect(JSON.parse(calls[1]!.body!).filters).toEqual({ vault_id: "notes" });
  });

  it("maps daemon errors to typed safe messages without echoing private input", async () => {
    const { transport } = mockTransport(400, {
      error: { code: "invalid_query", message: "private sentinel query" },
    });
    const error = await client(transport)
      .search({ q: "private sentinel query", mode: "lexical" })
      .catch((caught) => caught);
    expect(error).toBeInstanceOf(KwiryApiError);
    expect(error.code).toBe("invalid_query");
    expect(error.message).not.toContain("sentinel");
  });

  it("maps transport failure to a generic daemon_unreachable error", async () => {
    const transport: Transport = async () => {
      throw new Error("private transport detail");
    };
    const error = await client(transport)
      .search({ q: "a", mode: "lexical" })
      .catch((caught) => caught);
    expect(error).toBeInstanceOf(KwiryApiError);
    expect(error.code).toBe("daemon_unreachable");
    expect(error.message).not.toContain("private transport detail");
  });

  it("rejects malformed successful response shapes", async () => {
    const { transport } = mockTransport(200, { hits: [{ ...HIT, path: 42 }], next_cursor: null });
    const error = await client(transport)
      .search({ q: "a", mode: "lexical" })
      .catch((caught) => caught);
    expect(error).toBeInstanceOf(KwiryApiError);
    expect(error.code).toBe("invalid_response");
  });

  it("rejects non-loopback and path-bearing daemon URLs", () => {
    const { transport } = mockTransport(200, {});
    expect(
      () => new KwiryClient({
        baseUrl: "https://example.com",
        tokenProvider: () => TOKEN_A,
        transport,
      }),
    ).toThrow(/loopback/i);
    expect(
      () => new KwiryClient({
        baseUrl: "http://127.0.0.1:32189/redirect",
        tokenProvider: () => TOKEN_A,
        transport,
      }),
    ).toThrow(/loopback/i);
  });

  it("health does not read a token", async () => {
    const { transport } = mockTransport(200, { status: "ok" });
    let tokenReads = 0;
    const kwiry = client(transport, () => {
      tokenReads += 1;
      return TOKEN_A;
    });
    expect(await kwiry.health()).toBe(true);
    expect(tokenReads).toBe(0);
  });
});
