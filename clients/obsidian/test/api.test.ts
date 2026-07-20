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

describe("KwiryClient.search", () => {
  it("sends an authenticated POST with mode and limit", async () => {
    const { transport, calls } = mockTransport(200, { hits: [HIT], next_cursor: null });
    const client = new KwiryClient({
      baseUrl: "http://127.0.0.1:32189/",
      token: "secret\n",
      transport,
    });
    const response = await client.search({ q: "query", mode: "hybrid" });

    expect(response.hits).toHaveLength(1);
    expect(calls).toHaveLength(1);
    const call = calls[0]!;
    expect(call.url).toBe("http://127.0.0.1:32189/v0/search");
    expect(call.method).toBe("POST");
    expect(call.headers["Authorization"]).toBe("Bearer secret");
    const body = JSON.parse(call.body!);
    expect(body).toEqual({ q: "query", mode: "hybrid", limit: 20 });
  });

  it("includes filters only when non-empty", async () => {
    const { transport, calls } = mockTransport(200, { hits: [], next_cursor: null });
    const client = new KwiryClient({ baseUrl: "http://x.local", token: "t", transport });
    await client.search({ q: "a", mode: "lexical", filters: {} });
    await client.search({ q: "a", mode: "lexical", filters: { vault_id: "notes" } });
    expect(JSON.parse(calls[0]!.body!)).not.toHaveProperty("filters");
    expect(JSON.parse(calls[1]!.body!).filters).toEqual({ vault_id: "notes" });
  });

  it("maps daemon error envelopes to typed errors", async () => {
    const { transport } = mockTransport(501, {
      error: { code: "mode_unavailable", message: "needs --semantic" },
    });
    const client = new KwiryClient({ baseUrl: "http://x.local", token: "t", transport });
    const error = await client.search({ q: "a", mode: "semantic" }).catch((e) => e);
    expect(error).toBeInstanceOf(KwiryApiError);
    expect(error.status).toBe(501);
    expect(error.code).toBe("mode_unavailable");
  });

  it("maps transport failure to daemon_unreachable", async () => {
    const transport: Transport = async () => {
      throw new Error("ECONNREFUSED");
    };
    const client = new KwiryClient({ baseUrl: "http://x.local", token: "t", transport });
    const error = await client.search({ q: "a", mode: "lexical" }).catch((e) => e);
    expect(error).toBeInstanceOf(KwiryApiError);
    expect(error.code).toBe("daemon_unreachable");
  });

  it("rejects non-http base URLs", () => {
    const { transport } = mockTransport(200, {});
    expect(
      () => new KwiryClient({ baseUrl: "file:///etc", token: "t", transport }),
    ).toThrow(/http/);
  });

  it("health returns false instead of throwing", async () => {
    const transport: Transport = async () => {
      throw new Error("down");
    };
    const client = new KwiryClient({ baseUrl: "http://x.local", token: "t", transport });
    expect(await client.health()).toBe(false);
  });
});
