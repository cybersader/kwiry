// SPDX-FileCopyrightText: 2026 cybersader
// SPDX-License-Identifier: GPL-3.0-only

import { isVaultRelativePath } from "./vault-path";

// Provisional name of the host services slot on `window`. The contract fixes the
// final name later; change it only here.
export const HOST_SERVICES_SLOT = "obsidianHostServices";

/// A host may install its services slot after this plugin loads, for example
/// from another plugin that loads later. A fresh install keeps checking for this
/// long after layout-ready before it falls back. Plugin onload calls have all run
/// by layout-ready, so the window only has to cover a provider that installs its
/// slot after asynchronous setup. Five seconds covers that with margin and delays
/// a fallback's first index build by at most five seconds.
export const LATE_HOST_WINDOW_MS = 5_000;
export const LATE_HOST_CHECK_MS = 250;
const NOTICE = "Results may be incomplete.";

export const HOST_SEARCH_LABEL = "Host search · Lexical";
export const HOST_SEARCH_GUIDANCE = "Host search is unavailable. Use a host that supports search version 1.";
export const HOST_SEARCH_STARTING = "Host search is starting…";
export const HOST_SEARCH_ACCESS_LOST = "Access to host search was denied.";
export const HOST_SEARCH_IDLE = "Enter words separated by spaces to search.";
export const HOST_SEARCH_UNAVAILABLE = "Host search is unavailable.";
export const HOST_RESULT_UNAVAILABLE = "This result is no longer available to open.";

const stateLabels = {
  ready: "Results from the host",
  building: "Host search is being prepared.",
  unavailable: HOST_SEARCH_UNAVAILABLE,
  not_enabled: "Host search is not enabled here.",
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

// Look on the plugin realm's window, not the active popout. An absent slot or
// capability is a normal answer.
export function lookupHostSearch(realm: object = window): HostSearchOperations | null {
  try {
    const slot = record((realm as Record<string, unknown>)[HOST_SERVICES_SLOT]);
    const get = slot?.get;
    if (typeof get !== "function") return null;
    const wire = record(Reflect.apply(get, slot, ["search", 1]));
    const page = wire?.page as WireSearchCapability["page"] | undefined;
    const resolveNote = wire?.resolveNote as WireSearchCapability["resolveNote"] | undefined;
    if (typeof page !== "function" || typeof resolveNote !== "function") return null;
    return {
      async page(query, signal, continuation) {
        // This UI uses the host defaults and never selects a room or a page size.
        return decodePage(await page.call(wire, {
          query,
          ...(continuation === undefined ? {} : { cursor: continuation }),
        }, { signal }));
      },
      async resolveNote(ref, signal) {
        const path = record(await resolveNote.call(wire, ref, { signal }))?.path;
        if (typeof path !== "string" || !isVaultRelativePath(path) || /^[a-z]:/iu.test(path)) {
          throw hostSearchFault.staleResult();
        }
        return path;
      },
    };
  } catch {
    return null;
  }
}

/// Checks now, then every `checkMs`, until the capability appears or `windowMs`
/// has passed, and reports the outcome once. Nothing runs after the window
/// closes. The returned function cancels a pending watch without reporting.
export function watchForHostSearch(
  report: (found: boolean) => void,
  { windowMs = LATE_HOST_WINDOW_MS, checkMs = LATE_HOST_CHECK_MS } = {},
): () => void {
  let done = false;
  let tick: number | null = null;
  const stop = () => {
    done = true;
    window.clearTimeout(deadline);
    if (tick !== null) window.clearTimeout(tick);
    tick = null;
  };
  const finish = (found: boolean) => {
    if (done) return;
    stop();
    report(found);
  };
  const check = () => {
    tick = null;
    if (done) return;
    if (lookupHostSearch() !== null) finish(true);
    else tick = window.setTimeout(check, checkMs);
  };
  const deadline = window.setTimeout(() => finish(lookupHostSearch() !== null), windowMs);
  check();
  return stop;
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
  if (!page) return malformedPage();
  const { room, state, items, next, notice } = page;
  if (notice !== NOTICE || typeof state !== "string" || !Object.hasOwn(stateLabels, state)
    || !Array.isArray(items) || (next !== null && typeof next !== "string")) return malformedPage();
  const itemCount = items.length;
  if (!safeInteger(itemCount) || (state !== "ready" && itemCount > 0)
    || (room === null && state === "ready")) return malformedPage();
  const { label: roomLabel, ref: roomRef } = record(room) ?? {};
  if (room !== null && (typeof roomLabel !== "string" || typeof roomRef !== "string")) return malformedPage();

  const rows: HostSearchPage["rows"] = [];
  const seen = new Set<string>();
  for (let index = 0; index < itemCount; index++) {
    const hit = record((items as unknown[])[index]);
    if (!hit) return malformedPage();
    const { ref, path, title, excerpt } = hit;
    if (typeof ref !== "string" || typeof path !== "string"
      || typeof title !== "string" || typeof excerpt !== "string") return malformedPage();
    if (seen.has(ref)) continue;
    seen.add(ref);
    rows.push({ noteRef: ref, path, title, excerpt });
  }
  return { roomLabel: typeof roomLabel === "string" ? roomLabel : null,
    rows, continuation: next, status: stateLabels[state as HostSearchState], notice: NOTICE };
}

export interface HostSearchFailure {
  action: "access_lost" | "clear" | "discard_continuation" | "discard_result" | "keep";
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
        return { action: "access_lost" };
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
  const { limit, retryAfterMs } = failure;
  const { dimension, origin, maximum } = record(limit) ?? {};
  let message = "Host search reached a limit.";
  if (typeof dimension === "string" && Object.hasOwn(limitDimensions, dimension)
    && typeof origin === "string" && limitOrigins.includes(origin) && safeInteger(maximum)) {
    const label = limitDimensions[dimension as keyof typeof limitDimensions];
    message += ` Maximum: ${maximum} ${label} (${origin}).`;
  }
  if (safeInteger(retryAfterMs)) {
    message += ` Try again in ${Math.ceil(retryAfterMs / 1_000)} seconds.`;
  }
  return message;
}
