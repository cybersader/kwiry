// SPDX-FileCopyrightText: 2026 cybersader
// SPDX-License-Identifier: GPL-3.0-only
//
// Typed client for the kwir daemon HTTP contract (CONTRACT.md §4).
// Pure module: the HTTP transport is injected so this is testable
// outside Obsidian; production wires Obsidian's requestUrl.

export type SearchMode = "lexical" | "semantic" | "hybrid";

export interface SearchFilters {
  vault_id?: string;
  room?: string;
  path_prefix?: string;
  tags?: string[];
  frontmatter_equals?: Record<string, string>;
}

export interface SearchRequest {
  q: string;
  mode: SearchMode;
  filters?: SearchFilters;
  limit?: number;
}

export interface Frontmatter {
  title?: string;
  description?: string;
  tags?: string[];
  status?: string;
  date?: string;
}

export interface SearchHit {
  chunk_id: string;
  vault_id: string;
  path: string;
  heading_path: string[];
  score: number;
  excerpt: string;
  frontmatter: Frontmatter;
}

export interface SearchResponse {
  hits: SearchHit[];
  next_cursor: string | null;
}

export interface DaemonStatus {
  state: "starting" | "ready" | "degraded";
  version: string;
  chunks: number;
  documents: number;
  model: { name: string; version: string } | null;
}

export interface ApiErrorBody {
  code: string;
  message: string;
}

export class KwirApiError extends Error {
  constructor(
    public readonly status: number,
    public readonly code: string,
    message: string,
  ) {
    super(message);
    this.name = "KwirApiError";
  }
}

/** Injected transport; production uses Obsidian requestUrl (CORS-free). */
export interface Transport {
  (options: {
    url: string;
    method: string;
    headers: Record<string, string>;
    body?: string;
  }): Promise<{ status: number; text: string }>;
}

export interface KwirClientOptions {
  baseUrl: string;
  token: string;
  transport: Transport;
}

function normalizeBaseUrl(raw: string): string {
  const trimmed = raw.trim().replace(/\/+$/, "");
  if (!/^https?:\/\//.test(trimmed)) {
    throw new Error(`daemon URL must start with http:// or https://: ${raw}`);
  }
  return trimmed;
}

export class KwirClient {
  private readonly baseUrl: string;
  private readonly token: string;
  private readonly transport: Transport;

  constructor(options: KwirClientOptions) {
    this.baseUrl = normalizeBaseUrl(options.baseUrl);
    this.token = options.token.trim();
    this.transport = options.transport;
  }

  async search(request: SearchRequest): Promise<SearchResponse> {
    const body: SearchRequest = {
      q: request.q,
      mode: request.mode,
      limit: request.limit ?? 20,
    };
    if (request.filters && Object.keys(request.filters).length > 0) {
      body.filters = request.filters;
    }
    return (await this.call("POST", "/v0/search", body)) as SearchResponse;
  }

  async status(): Promise<DaemonStatus> {
    return (await this.call("GET", "/v0/status")) as DaemonStatus;
  }

  async health(): Promise<boolean> {
    try {
      await this.call("GET", "/v0/health", undefined, false);
      return true;
    } catch {
      return false;
    }
  }

  private async call(
    method: string,
    path: string,
    body?: unknown,
    authenticated = true,
  ): Promise<unknown> {
    const headers: Record<string, string> = {};
    if (authenticated) {
      headers["Authorization"] = `Bearer ${this.token}`;
    }
    if (body !== undefined) {
      headers["Content-Type"] = "application/json";
    }
    let response: { status: number; text: string };
    try {
      response = await this.transport({
        url: `${this.baseUrl}${path}`,
        method,
        headers,
        body: body === undefined ? undefined : JSON.stringify(body),
      });
    } catch (error) {
      throw new KwirApiError(
        0,
        "daemon_unreachable",
        `kwir daemon is unreachable at ${this.baseUrl}: ${String(error)}`,
      );
    }
    let parsed: unknown;
    try {
      parsed = JSON.parse(response.text);
    } catch {
      parsed = undefined;
    }
    if (response.status < 200 || response.status >= 300) {
      const envelope = parsed as { error?: ApiErrorBody } | undefined;
      const code = envelope?.error?.code ?? `http_${response.status}`;
      const message =
        envelope?.error?.message ?? `kwir daemon returned HTTP ${response.status}`;
      throw new KwirApiError(response.status, code, message);
    }
    if (parsed === undefined) {
      throw new KwirApiError(response.status, "invalid_response", "daemon returned non-JSON body");
    }
    return parsed;
  }
}
