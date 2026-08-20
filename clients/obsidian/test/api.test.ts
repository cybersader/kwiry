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
  format: "markdown",
  coverage: "indexed-complete",
  locator: null,
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

  it("parses the closed source format and Base view locator", async () => {
    const baseHit = {
      ...HIT,
      path: "projects.base",
      format: "base",
      locator: { kind: "base_view", view: "Active" },
    };
    const { transport } = mockTransport(200, { hits: [baseHit], next_cursor: null });
    await expect(client(transport).search({ q: "a", mode: "lexical" })).resolves.toMatchObject({
      hits: [{ format: "base", locator: { kind: "base_view", view: "Active" } }],
    });
  });

  it("parses a PDF page locator", async () => {
    const pdfHit = {
      ...HIT,
      path: "papers/report.pdf",
      format: "pdf",
      heading_path: [],
      locator: { kind: "pdf_page", page: 7 },
    };
    const { transport } = mockTransport(200, { hits: [pdfHit], next_cursor: null });
    await expect(client(transport).search({ q: "a", mode: "lexical" })).resolves.toMatchObject({
      hits: [{ format: "pdf", locator: { kind: "pdf_page", page: 7 } }],
    });
  });

  it("parses an HTML hit without inventing a locator", async () => {
    const htmlHit = {
      ...HIT,
      path: "site/index.htm",
      format: "html",
      heading_path: ["Reader heading"],
      locator: null,
      frontmatter: { title: "Canonical Portal" },
    };
    const { transport } = mockTransport(200, { hits: [htmlHit], next_cursor: null });
    await expect(client(transport).search({ q: "portal", mode: "lexical" })).resolves.toMatchObject({
      hits: [{
        path: "site/index.htm",
        format: "html",
        locator: null,
        frontmatter: { title: "Canonical Portal" },
      }],
    });
  });

  it("accepts source-bounded HTML titles beyond the exact-title ceiling", async () => {
    const title = "x".repeat(4_097);
    const htmlHit = {
      ...HIT,
      path: "site/index.html",
      format: "html",
      heading_path: [],
      locator: null,
      frontmatter: { title },
    };
    const { transport } = mockTransport(200, { hits: [htmlHit], next_cursor: null });
    await expect(client(transport).search({ q: "portal", mode: "lexical" })).resolves.toMatchObject({
      hits: [{ format: "html", frontmatter: { title } }],
    });
  });

  it.each([
    { format: "epub" },
    { format: "html", path: "site/index.html", locator: { kind: "pdf_page", page: 1 } },
    { locator: { kind: "base_view", view: "Active" } },
    { locator: { kind: "page", page: 3 } },
    { locator: { kind: "base_view", view: "" } },
    // A page locator on a Markdown hit: the kind is real but the pairing is not.
    { locator: { kind: "pdf_page", page: 3 } },
    // Pages are 1-based and integral; a `u32` field cannot carry these.
    { format: "pdf", path: "paper.pdf", locator: { kind: "pdf_page", page: 0 } },
    { format: "pdf", path: "paper.pdf", locator: { kind: "pdf_page", page: -1 } },
    { format: "pdf", path: "paper.pdf", locator: { kind: "pdf_page", page: 1.5 } },
    { format: "pdf", path: "paper.pdf", locator: { kind: "pdf_page", page: "3" } },
    // The exact-key rule holds on the new variant as well.
    { format: "pdf", path: "paper.pdf", locator: { kind: "pdf_page", page: 3, view: "x" } },
  ])("rejects an invalid format or locator projection: %j", async (overrides) => {
    const { transport } = mockTransport(200, {
      hits: [{ ...HIT, ...overrides }],
      next_cursor: null,
    });
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
