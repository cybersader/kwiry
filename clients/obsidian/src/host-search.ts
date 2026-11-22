// SPDX-FileCopyrightText: 2026 cybersader
// SPDX-License-Identifier: GPL-3.0-only

import { isNormalizedVaultFilePath } from "./vault-path";

// Provisional host global for the draft capability lookup.
export const HOST_GLOBAL = "openclast";
const NOTICE = "Results may be incomplete.";

export const HOST_SEARCH_LABEL = "Host search · Lexical";
export const HOST_SEARCH_GUIDANCE = "Host search is unavailable. Use a host that supports search version 1.";
export const HOST_SEARCH_IDLE = "Enter words separated by spaces to search.";
export const HOST_SEARCH_UNAVAILABLE = "Host search is unavailable.";
export const HOST_RESULT_UNAVAILABLE = "This result is no longer available to open.";

const stateLabels = {
  ready: "Results from the host",
  building: "Host search is being prepared.",
  unavailable: HOST_SEARCH_UNAVAILABLE,
  not_enabled: "Host search is not enabled for this workspace.",
} as const;

type HostSearchState = keyof typeof stateLabels;

export interface HostSearchPage {
  roomLabel: string | null;
  rows: { noteRef: string; path: string; title: string; excerpt: string }[];
  continuation: string | null;
  status: string;
  notice: typeof NOTICE;
}

interface WireSearchCapability {
  page(request: { query: string; cursor?: string }, options: { signal: AbortSignal }): Promise<unknown>;
  resolveNote(ref: string, options: { signal: AbortSignal }): Promise<unknown>;
}

export interface HostSearchOperations {
  page(query: string, signal: AbortSignal, continuation?: string): Promise<HostSearchPage>;
  resolveNote(ref: string, signal: AbortSignal): Promise<string>;
}

export const hostSearchFault = {
  cancelled: () => new HostSearchFault("cancelled"),
  unavailable: () => new HostSearchFault("search_unavailable"),
  staleResult: () => new HostSearchFault("result_unavailable"),
};

class HostSearchFault extends Error {
  constructor(readonly code: string) { super(code); }
}

// Look in the plugin realm, not the active popout.
export function lookupHostSearch(realm: object = globalThis): HostSearchOperations | null {
  try {
    const root = record((realm as Record<string, unknown>)[HOST_GLOBAL]);
    const capabilities = record(root?.capabilities);
    const get = capabilities?.get;
    if (typeof get !== "function") return null;
    const wire = record(Reflect.apply(get, capabilities, ["search", 1]));
    if (typeof wire?.page !== "function" || typeof wire.resolveNote !== "function") return null;
    const capability = wire as unknown as WireSearchCapability;
    return {
      async page(query, signal, continuation) {
        // This UI uses the host defaults and never selects a room or a page size.
        return decodePage(await capability.page({
          query,
          ...(continuation === undefined ? {} : { cursor: continuation }),
        }, { signal }));
      },
      async resolveNote(ref, signal) {
        const path = record(await capability.resolveNote(ref, { signal }))?.path;
        if (typeof path !== "string" || !isNormalizedVaultFilePath(path) || /^[a-z]:/iu.test(path)) {
          throw hostSearchFault.staleResult();
        }
        return path;
      },
    };
  } catch {
    return null;
  }
}

function record(value: unknown): Record<string, unknown> | null {
  return typeof value === "object" && value !== null && !Array.isArray(value)
    ? value as Record<string, unknown>
    : null;
}

function malformedPage(): never {
  throw hostSearchFault.unavailable();
}

function decodePage(value: unknown): HostSearchPage {
  const page = record(value);
  if (!page || page.notice !== NOTICE) return malformedPage();
  const { room, state, items, next } = page;
  if (typeof state !== "string" || !Object.hasOwn(stateLabels, state)
    || !Array.isArray(items) || (next !== null && typeof next !== "string")
    || (state !== "ready" && items.length > 0)
    || (room === null && (state === "ready" || items.length > 0))) return malformedPage();
  const roomRecord = record(room);
  const roomLabel = roomRecord?.label;
  if (room !== null && (typeof roomLabel !== "string" || typeof roomRecord?.ref !== "string")) {
    return malformedPage();
  }

  const rows: HostSearchPage["rows"] = [];
  const seen = new Set<string>();
  for (const value of items as unknown[]) {
    const hit = record(value);
    if (!hit || typeof hit.ref !== "string" || typeof hit.path !== "string"
      || typeof hit.title !== "string" || typeof hit.excerpt !== "string") return malformedPage();
    if (seen.has(hit.ref)) continue;
    seen.add(hit.ref);
    rows.push({ noteRef: hit.ref, path: hit.path, title: hit.title, excerpt: hit.excerpt });
  }
  return { roomLabel: typeof roomLabel === "string" ? roomLabel : null,
    rows, continuation: next, status: stateLabels[state as HostSearchState], notice: NOTICE };
}

export interface HostSearchFailure {
  action: "clear" | "discard_continuation" | "discard_result" | "keep";
  message?: string;
}

export function hostSearchFailure(error: unknown): HostSearchFailure {
  try {
    const failure = record(error);
    switch (failure?.code) {
      case "invalid_query":
        return { action: "clear", message: "Use words separated by spaces, without spaces at the ends." };
      case "invalid_request":
      case "unsupported_mode":
        return { action: "clear", message: "This search request is not supported by the host." };
      case "access_denied":
        return { action: "clear", message: "Access to host search was denied." };
      case "invalid_cursor":
        return { action: "discard_continuation", message: "This page can no longer be continued. Submit a new search." };
      case "result_unavailable":
        return { action: "discard_result", message: HOST_RESULT_UNAVAILABLE };
      case "limited":
        return { action: "keep", message: limitMessage(failure) };
      case "cancelled":
        return { action: "keep" };
    }
  } catch {
    // A malformed rejection cannot supply display text or escape to diagnostics.
  }
  return { action: "clear", message: HOST_SEARCH_UNAVAILABLE };
}

const limitDimensions = {
  queryBytes: "query bytes", queryTerms: "query terms", pageItems: "items per page",
  replyBytes: "reply bytes", requests: "requests", concurrency: "concurrent requests",
  deadlineMs: "milliseconds per request",
};
const limitOrigins = ["protocol", "implementation", "deployment", "organization", "folder", "workspace", "grant"];

function safeInteger(value: unknown): value is number {
  return typeof value === "number" && Number.isSafeInteger(value) && value >= 0;
}

function limitMessage(failure: Record<string, unknown>): string {
  const limit = record(failure.limit);
  let message = "Host search reached a limit.";
  if (limit && typeof limit.dimension === "string" && Object.hasOwn(limitDimensions, limit.dimension)
    && typeof limit.origin === "string" && limitOrigins.includes(limit.origin) && safeInteger(limit.maximum)) {
    const dimension = limitDimensions[limit.dimension as keyof typeof limitDimensions];
    message += ` Maximum: ${limit.maximum} ${dimension} (${limit.origin}).`;
  }
  if (safeInteger(failure.retryAfterMs)) {
    message += ` Try again in ${Math.ceil(failure.retryAfterMs / 1_000)} seconds.`;
  }
  return message;
}
