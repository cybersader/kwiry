// SPDX-FileCopyrightText: 2026 cybersader
// SPDX-License-Identifier: GPL-3.0-only

import {
  callDisposer,
  decidingLevelLabel,
  hasMajor,
  lookupHostService,
  readHostError,
  record,
  safeCount,
  type SharedCode,
} from "./host-services";

const NOTICE = "Results may be incomplete.";

export const HOST_SEARCH_LABEL = "Host search · Lexical";
export const HOST_SEARCH_NO_HOST = "Host search is unavailable: no host is providing it here.";
export const HOST_SEARCH_NOT_OFFERED = "Host search is unavailable: this host does not offer search version 1.";
export const HOST_SEARCH_STARTING = "Host search is starting…";
export const HOST_SEARCH_ACCESS_LOST = "You no longer have access to these results.";
export const HOST_SEARCH_IDLE = "Enter words separated by spaces to search.";
export const HOST_SEARCH_UNAVAILABLE = "Host search is unavailable right now.";
export const HOST_SEARCH_EXPIRED = "These results expired. Search again.";
export const HOST_SEARCH_WITHDRAWN = "Host search is no longer offered here.";
export const HOST_RESULT_UNAVAILABLE = "That note is no longer available.";

const KNOWN_STATES = ["ready", "building", "unavailable", "not_enabled"] as const;
type KnownState = typeof KNOWN_STATES[number];

const stateLabels: Readonly<Record<Exclude<KnownState, "not_enabled">, string>> = {
  ready: "Results from the host",
  building: "Host search is being prepared.",
  unavailable: HOST_SEARCH_UNAVAILABLE,
};

/** Search codes beyond the shared base, each with its family. */
export const SEARCH_CODE_FAMILIES: Readonly<Record<string, SharedCode>> = Object.freeze({
  invalid_query: "invalid_request",
  unsupported_mode: "invalid_request",
  invalid_cursor: "invalidated",
  result_unavailable: "invalidated",
});

export type HostSearchSituation = "no_host" | "without_service" | "available";

export interface HostSearchPage {
  roomLabel: string | null;
  rows: { noteRef: string; path: string; title: string; excerpt: string }[];
  continuation: string | null;
  status: string;
  notice: typeof NOTICE;
}

/** One search service object, held only while a view that uses it is on screen. */
export interface HostSearchSession {
  page(query: string, signal: AbortSignal, continuation?: string): Promise<HostSearchPage>;
  /** The host resolves and opens the note; nothing here touches vault files. */
  open(ref: string, signal: AbortSignal): Promise<void>;
  /** Returns an idempotent disposer. */
  onInvalidated(listener: (reason: string | null) => void): () => void;
}

export type HostSearchLookup =
  | { readonly situation: "no_host" | "without_service" }
  | { readonly situation: "available"; readonly session: HostSearchSession };

export const hostSearchFault = {
  cancelled: () => new HostSearchFault("cancelled"),
  unavailable: () => new HostSearchFault("unavailable"),
};

class HostSearchFault extends Error {
  readonly family: SharedCode;
  constructor(readonly code: SharedCode) {
    super(code);
    this.family = code;
  }
}

/// One synchronous look on this plugin's window, not the active popout. Absence
/// and a malformed service are normal answers; nothing is cached.
export function lookupHostSearch(realm: object = window): HostSearchLookup {
  const found = lookupHostService("search", 1, realm);
  if (found.kind !== "available") return { situation: found.kind };
  const service = found.service;
  try {
    const { page, open, onInvalidated } = service;
    if (!hasMajor(service, 1) || typeof page !== "function" || typeof open !== "function"
      || typeof onInvalidated !== "function") return { situation: "without_service" };
    return {
      situation: "available",
      session: {
        async page(query, signal, continuation) {
          // This UI uses the host defaults and never selects a room, mode or page size.
          return decodePage(await Reflect.apply(page, service, [{
            query,
            ...(continuation === undefined ? {} : { cursor: continuation }),
          }, { signal }]));
        },
        async open(ref, signal) {
          await Reflect.apply(open, service, [ref, { signal }]);
        },
        onInvalidated(listener) {
          return callDisposer(() => Reflect.apply(onInvalidated, service, [(event: unknown) => {
            let reason: unknown;
            try { reason = record(event)?.reason; } catch { reason = undefined; }
            listener(typeof reason === "string" ? reason : null);
          }]));
        },
      },
    };
  } catch {
    return { situation: "without_service" };
  }
}

export function hostSearchSituation(realm: object = window): HostSearchSituation {
  return lookupHostSearch(realm).situation;
}

export function hostSearchSituationMessage(situation: HostSearchSituation): string {
  return situation === "no_host" ? HOST_SEARCH_NO_HOST
    : situation === "without_service" ? HOST_SEARCH_NOT_OFFERED
      : HOST_SEARCH_LABEL;
}

function malformedPage(): never {
  throw hostSearchFault.unavailable();
}

/// Reads every field of the page once. Unknown states are shown as unavailable
/// with no rows; result references are kept as given (equality is not meaningful).
function decodePage(value: unknown): HostSearchPage {
  const page = record(value);
  if (!page) return malformedPage();
  const { room, state, items, next, notice, decidedBy } = page;
  if (notice !== NOTICE || typeof state !== "string" || !Array.isArray(items)
    || (next !== null && typeof next !== "string")) return malformedPage();
  const known = (KNOWN_STATES as readonly string[]).includes(state) ? state as KnownState : null;
  const itemCount = items.length;
  if (!safeCount(itemCount) || (known === "ready" && room === null)) return malformedPage();
  if (known !== null && known !== "ready" && itemCount > 0) return malformedPage();
  const { label: roomLabel, ref: roomRef } = record(room) ?? {};
  if (room !== null && (typeof roomLabel !== "string" || typeof roomRef !== "string")) return malformedPage();

  const rows: HostSearchPage["rows"] = [];
  if (known === "ready") {
    for (let index = 0; index < itemCount; index++) {
      const hit = record((items as unknown[])[index]);
      if (!hit) return malformedPage();
      const { ref, path, title, excerpt } = hit;
      if (typeof ref !== "string" || typeof path !== "string"
        || typeof title !== "string" || typeof excerpt !== "string") return malformedPage();
      rows.push({ noteRef: ref, path, title, excerpt });
    }
  }
  const status = known === "not_enabled"
    ? `Host search is turned off by ${decidingLevelLabel(decidedBy)}.`
    : stateLabels[known ?? "unavailable"];
  return { roomLabel: typeof roomLabel === "string" ? roomLabel : null,
    rows, continuation: next, status, notice: NOTICE };
}

export interface HostSearchFailure {
  action: "access_lost" | "clear" | "discard_continuation" | "discard_result" | "keep";
  message?: string;
}

/// Maps a rejection by its code, or by its family when the code is unknown.
export function hostSearchFailure(error: unknown): HostSearchFailure {
  const failure = readHostError(error, SEARCH_CODE_FAMILIES);
  switch (failure.code) {
    case "invalid_query":
      return { action: "clear", message: "Use words separated by spaces, without spaces at the ends." };
    case "invalid_cursor":
      return { action: "discard_continuation", message: "This page can no longer be continued. Submit a new search." };
    case "result_unavailable":
      return { action: "discard_result", message: HOST_RESULT_UNAVAILABLE };
  }
  switch (failure.family) {
    case "invalid_request":
      return { action: "clear", message: "This search request is not supported by the host." };
    case "access_denied":
      return { action: "access_lost", message: HOST_SEARCH_ACCESS_LOST };
    case "limited":
      return { action: "keep", message: limitMessage(failure.limit, failure.retryAfterMs) };
    case "cancelled":
    case "declined":
      return { action: "keep" };
    case "invalidated":
      return { action: "clear", message: HOST_SEARCH_EXPIRED };
    case "not_enabled":
      return { action: "clear", message: `Host search is turned off by ${decidingLevelLabel(failure.decidedBy)}.` };
    case "conflict":
      return { action: "clear", message: "Changed elsewhere. Search again." };
    case "read_only":
      return { action: "clear", message: "Read-only right now." };
    case "unavailable":
      return { action: "clear", message: HOST_SEARCH_UNAVAILABLE };
  }
}

/// Fixed wording for an invalidation, by reason. Unknown reasons are a full invalidation.
export function hostInvalidationMessage(reason: string | null): { message: string; detach: boolean } {
  switch (reason) {
    case "access":
      return { message: HOST_SEARCH_ACCESS_LOST, detach: false };
    case "workspace":
      return { message: "The workspace changed. Search again.", detach: false };
    case "lifecycle":
      return { message: HOST_SEARCH_EXPIRED, detach: false };
    case "withdrawn":
      return { message: HOST_SEARCH_WITHDRAWN, detach: true };
    default:
      return { message: HOST_SEARCH_EXPIRED, detach: true };
  }
}

const limitDimensions: Readonly<Record<string, string>> = {
  requestBytes: "request bytes", queryBytes: "query bytes", queryTerms: "query terms", pageItems: "items per page",
  replyBytes: "reply bytes", requests: "requests", concurrency: "concurrent requests",
  deadlineMs: "milliseconds per request",
};

function limitMessage(
  limit: { dimension: string | null; maximum: number | null; origin: unknown } | null,
  retryAfterMs: number | null,
): string {
  let message = "Host search reached a limit.";
  if (limit) {
    const by = decidingLevelLabel(limit.origin);
    const label = limit.dimension !== null && Object.hasOwn(limitDimensions, limit.dimension)
      ? limitDimensions[limit.dimension] : undefined;
    message += label !== undefined && limit.maximum !== null
      ? ` Maximum: ${limit.maximum} ${label}, set by ${by}.`
      : ` The limit is set by ${by}.`;
  }
  if (retryAfterMs !== null) {
    message += ` Try again in ${Math.ceil(retryAfterMs / 1_000)} seconds.`;
  }
  return message;
}
