// SPDX-FileCopyrightText: 2026 cybersader
// SPDX-License-Identifier: GPL-3.0-only
//
// Generic layer of the host services contract: the slot lookup, the
// "provider arrived" event, shared error data and deciding levels, and the
// local indexing policy. Nothing here names a product; only the two string
// constants below come from the contract.

// PROVISIONAL: the slot key and the event name are not frozen yet. They live
// here and nowhere else; change them only in this module.
export const HOST_SLOT_KEY = "com.openclast.workspace-host";
export const HOST_AVAILABLE_EVENT = "com.openclast.workspace-host:available";

export type HostSituation =
  | { readonly kind: "no_host" }
  | { readonly kind: "without_service" }
  | { readonly kind: "available"; readonly service: Readonly<Record<string, unknown>> };

const NO_HOST: HostSituation = Object.freeze({ kind: "no_host" });
const WITHOUT_SERVICE: HostSituation = Object.freeze({ kind: "without_service" });

/// One synchronous look at the slot on this plugin's window. Never cached or
/// persisted. An undefined key means no host. A key holding anything else
/// counts as a host, even when it is malformed, so a broken slot can never
/// turn host rules off.
export function lookupHostService(name: string, major: number, realm: object = window): HostSituation {
  let slot: unknown;
  try {
    slot = Reflect.get(realm, HOST_SLOT_KEY);
  } catch {
    return WITHOUT_SERVICE;
  }
  if (slot === undefined) return NO_HOST;
  try {
    // Only a plain non-null object can be a slot; anything else, a function
    // included, is a broken slot: a host without the service.
    if (typeof slot !== "object" || slot === null) return WITHOUT_SERVICE;
    const get: unknown = Reflect.get(slot, "get");
    if (typeof get !== "function") return WITHOUT_SERVICE;
    const service: unknown = Reflect.apply(get, slot, [name, major]);
    const found = record(service);
    return found ? { kind: "available", service: found } : WITHOUT_SERVICE;
  } catch {
    return WITHOUT_SERVICE;
  }
}

/// Reads `major` and `minor` once and checks the exact major.
export function hasMajor(service: Readonly<Record<string, unknown>>, major: number): boolean {
  const { major: actual, minor } = service;
  return actual === major && typeof minor === "number" && Number.isSafeInteger(minor) && minor >= 0;
}

export function record(value: unknown): Readonly<Record<string, unknown>> | null {
  return typeof value === "object" && value !== null && !Array.isArray(value)
    ? value as Readonly<Record<string, unknown>>
    : null;
}

export const SHARED_CODES = [
  "invalid_request", "access_denied", "limited", "unavailable", "cancelled",
  "not_enabled", "invalidated", "declined", "conflict", "read_only",
] as const;
export type SharedCode = typeof SHARED_CODES[number];

export function isSharedCode(value: unknown): value is SharedCode {
  return typeof value === "string" && (SHARED_CODES as readonly string[]).includes(value);
}

export interface HostLimit {
  readonly dimension: string | null;
  readonly maximum: number | null;
  readonly origin: unknown;
}

export interface HostErrorData {
  /** A code this plugin knows, otherwise null. */
  readonly code: string | null;
  readonly family: SharedCode;
  readonly retryAfterMs: number | null;
  readonly limit: HostLimit | null;
  readonly decidedBy: unknown;
}

/// Reads a rejection once into plain fields. Unknown codes fall back to their
/// family, then to `unavailable`. Nothing from the rejection is kept as text
/// except values compared against fixed lists.
export function readHostError(
  error: unknown,
  serviceFamilies: Readonly<Record<string, SharedCode>> = {},
): HostErrorData {
  try {
    const failure = record(error);
    if (!failure) return fallbackError();
    const { code, family, retryAfterMs, limit, decidedBy } = failure;
    let knownCode: string | null = null;
    let resolved: SharedCode = "unavailable";
    if (isSharedCode(code)) {
      knownCode = code;
      resolved = code;
    } else if (typeof code === "string" && Object.hasOwn(serviceFamilies, code)) {
      knownCode = code;
      resolved = serviceFamilies[code] ?? "unavailable";
    } else if (isSharedCode(family)) {
      resolved = family;
    }
    return {
      code: knownCode,
      family: resolved,
      retryAfterMs: safeCount(retryAfterMs) ? retryAfterMs : null,
      limit: readLimit(limit),
      decidedBy,
    };
  } catch {
    return fallbackError();
  }
}

function fallbackError(): HostErrorData {
  return { code: null, family: "unavailable", retryAfterMs: null, limit: null, decidedBy: undefined };
}

function readLimit(value: unknown): HostLimit | null {
  const limit = record(value);
  if (!limit) return null;
  const { dimension, maximum, origin } = limit;
  return {
    dimension: typeof dimension === "string" ? dimension : null,
    maximum: safeCount(maximum) ? maximum : null,
    origin,
  };
}

export function safeCount(value: unknown): value is number {
  return typeof value === "number" && Number.isSafeInteger(value) && value >= 0;
}

const LEVEL_LABELS: Readonly<Record<string, string>> = {
  protocol: "default",
  deployment: "your server",
  organization: "your organization",
  workspace: "this workspace",
  folder: "this folder",
};

/// Short label for a deciding level. The list is open: anything else is "policy".
export function decidingLevelLabel(level: unknown): string {
  return typeof level === "string" && Object.hasOwn(LEVEL_LABELS, level)
    ? LEVEL_LABELS[level] ?? "policy"
    : "policy";
}

// ---------------------------------------------------------------------------
// Local indexing policy (policy service, major 1)

export type LocalIndexingState =
  /** No slot: this page is not governed; the plugin's own settings apply. */
  | { readonly kind: "ungoverned" }
  /** A slot exists and the policy answer is not known yet. Nothing may start. */
  | { readonly kind: "pending" }
  /** In-memory structures and Workers only; nothing that outlives the page. */
  | { readonly kind: "memory_only"; readonly decidedBy: unknown }
  /** No plugin-built index at all. */
  | { readonly kind: "refused"; readonly decidedBy: unknown; readonly limited: boolean };

export const POLICY_DEFAULT_STATE: LocalIndexingState = Object.freeze({ kind: "memory_only", decidedBy: "protocol" });

export const LOCAL_INDEXING_PENDING = "Waiting for the local indexing policy…";

export function localIndexingMessage(state: LocalIndexingState): string | null {
  switch (state.kind) {
    case "ungoverned":
      return null;
    case "pending":
      return LOCAL_INDEXING_PENDING;
    case "memory_only":
      return `Local indexing: memory only (${decidingLevelLabel(state.decidedBy)}). Nothing is kept after this session.`;
    case "refused":
      return state.limited
        ? `Local indexing has a limit set by ${decidingLevelLabel(state.decidedBy)} that this plugin cannot check, so it builds no index.`
        : `Local indexing is turned off by ${decidingLevelLabel(state.decidedBy)}.`;
  }
}

/// Decodes one policy snapshot. Unknown values, malformed answers and any size
/// ceiling this plugin cannot evaluate all mean "do not build".
export function decodeLocalIndexing(snapshot: unknown): LocalIndexingState {
  try {
    const answer = record(record(snapshot)?.localIndexing);
    if (!answer) return { kind: "refused", decidedBy: "protocol", limited: false };
    const { value, decidedBy, limit } = answer;
    if (value !== "memory_only") return { kind: "refused", decidedBy, limited: false };
    if (limit !== undefined) {
      return { kind: "refused", decidedBy: readLimit(limit)?.origin ?? decidedBy, limited: true };
    }
    return { kind: "memory_only", decidedBy };
  } catch {
    return { kind: "refused", decidedBy: "protocol", limited: false };
  }
}

/// Follows the local indexing answer for as long as the plugin runs: looks at
/// start, again whenever `refresh` is called (the provider-arrived event), and
/// listens to the policy subscription and invalidation hook while a policy
/// service is offered. Holds no slot object; holds the policy service only to
/// tell a new provider's service from the one it already follows.
export class LocalIndexingWatch {
  private stateValue: LocalIndexingState = { kind: "ungoverned" };
  private service: Readonly<Record<string, unknown>> | null = null;
  private stop: AbortController | null = null;
  private disposers: Array<() => void> = [];
  private governed = false;
  private disposed = false;
  /** The last decoded answer. A pending state (resync, unknown event) never erases it. */
  private lastAnswer: LocalIndexingState | null = null;

  constructor(
    private readonly onChange: (state: LocalIndexingState) => void,
    private readonly realm: object = window,
  ) {}

  get state(): LocalIndexingState {
    return this.stateValue;
  }

  refresh(): void {
    if (this.disposed) return;
    const found = lookupHostService("policy", 1, this.realm);
    if (found.kind === "no_host") {
      // Once a slot was seen in this page (for example a provider that later
      // withdrew), the last answer stands until the next start.
      if (!this.governed) this.set({ kind: "ungoverned" });
      return;
    }
    this.governed = true;
    if (found.kind === "without_service") {
      this.unfollow();
      this.set(POLICY_DEFAULT_STATE);
      return;
    }
    if (found.service === this.service) return;
    this.follow(found.service);
  }

  dispose(): void {
    this.disposed = true;
    this.unfollow();
  }

  private follow(service: Readonly<Record<string, unknown>>): void {
    this.unfollow();
    this.service = service;
    const { current, subscribe, onInvalidated } = service;
    if (!hasMajor(service, 1) || typeof current !== "function" || typeof subscribe !== "function"
      || typeof onInvalidated !== "function") {
      this.answer({ kind: "refused", decidedBy: "protocol", limited: false });
      return;
    }
    const stop = this.stop = new AbortController();
    const live = () => !this.disposed && this.stop === stop;
    let subscribed = false;
    this.set({ kind: "pending" });
    this.keep(callDisposer(() => Reflect.apply(onInvalidated, service, [(event: unknown) => {
      if (!live()) return;
      let reason: unknown;
      try { reason = record(event)?.reason; } catch { reason = undefined; }
      this.unfollow();
      // A withdrawn provider leaves no slot behind; the last answer stands for
      // the rest of the page, even across a resync that left the state pending.
      // The contract's default applies only when no answer ever arrived.
      if (reason === "withdrawn") {
        if (this.stateValue.kind === "pending") this.set(this.lastAnswer ?? POLICY_DEFAULT_STATE);
        return;
      }
      this.set({ kind: "pending" });
      this.refresh();
    }])));
    this.keep(callDisposer(() => Reflect.apply(subscribe, service, [(event: unknown) => {
      if (!live()) return;
      let type: unknown;
      let snapshot: unknown;
      try {
        const data = record(event);
        type = data?.type;
        snapshot = data?.snapshot;
      } catch {
        type = undefined;
      }
      subscribed = true;
      if (type === "snapshot" || type === "change") this.answer(decodeLocalIndexing(snapshot));
      // `resync`, and any type this plugin does not know: the answer is unknown
      // until the fresh snapshot arrives.
      else this.set({ kind: "pending" });
    }, { signal: stop.signal }])));
    void (async () => {
      try {
        const snapshot: unknown = await Reflect.apply(current, service, [{ signal: stop.signal }]);
        if (live() && !subscribed) this.answer(decodeLocalIndexing(snapshot));
      } catch {
        if (live() && !subscribed) this.answer({ kind: "refused", decidedBy: "protocol", limited: false });
      }
    })();
  }

  private keep(disposer: () => void): void {
    if (this.stop) this.disposers.push(disposer);
    else disposer();
  }

  private unfollow(): void {
    this.service = null;
    const stop = this.stop;
    this.stop = null;
    stop?.abort();
    const disposers = this.disposers;
    this.disposers = [];
    for (const dispose of disposers) dispose();
  }

  private answer(state: LocalIndexingState): void {
    if (this.disposed) return;
    this.lastAnswer = state;
    this.set(state);
  }

  private set(state: LocalIndexingState): void {
    if (this.disposed || sameState(this.stateValue, state)) return;
    this.stateValue = state;
    this.onChange(state);
  }
}

function sameState(left: LocalIndexingState, right: LocalIndexingState): boolean {
  if (left.kind !== right.kind) return false;
  if (left.kind === "memory_only" && right.kind === "memory_only") {
    return sameDecider(left.decidedBy, right.decidedBy);
  }
  if (left.kind === "refused" && right.kind === "refused") {
    return sameDecider(left.decidedBy, right.decidedBy) && left.limited === right.limited;
  }
  return true;
}

/// By value, as this plugin uses it: a repeated answer whose deciding level reads
/// the same is the same answer, even when it arrives as a fresh object. Nothing
/// beyond the fixed label is read from a host value.
function sameDecider(left: unknown, right: unknown): boolean {
  return Object.is(left, right) || decidingLevelLabel(left) === decidingLevelLabel(right);
}

/// Calls a registration function and returns an idempotent, non-throwing disposer.
export function callDisposer(register: () => unknown): () => void {
  let dispose: unknown;
  try {
    dispose = register();
  } catch {
    dispose = undefined;
  }
  let done = false;
  return () => {
    if (done) return;
    done = true;
    if (typeof dispose !== "function") return;
    try {
      (dispose as () => unknown)();
    } catch {
      // A failing disposer cannot affect this plugin.
    }
  };
}
