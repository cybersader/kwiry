// SPDX-FileCopyrightText: 2026 cybersader
// SPDX-License-Identifier: GPL-3.0-only

import { readFileSync, readdirSync } from "node:fs";
import { fileURLToPath } from "node:url";

import { afterEach, describe, expect, it, vi } from "vitest";

import {
  HOST_AVAILABLE_EVENT,
  HOST_SLOT_KEY,
  LocalIndexingWatch,
  SHARED_CODES,
  decidingLevelLabel,
  decodeLocalIndexing,
  localIndexingMessage,
  lookupHostService,
  readHostError,
  type LocalIndexingState,
} from "../src/host-services";

const SRC = fileURLToPath(new URL("../src/", import.meta.url));

function realm(slot: unknown): Record<string, unknown> {
  return slot === undefined ? {} : { [HOST_SLOT_KEY]: slot };
}

describe("slot lookup: three situations", () => {
  it("no slot means no host", () => {
    expect(lookupHostService("search", 1, realm(undefined))).toEqual({ kind: "no_host" });
  });

  it("a slot without the service means a host without the service", () => {
    const get = vi.fn(() => undefined);
    expect(lookupHostService("search", 1, realm({ get }))).toEqual({ kind: "without_service" });
    expect(get.mock.calls).toEqual([["search", 1]]);
  });

  it("returns the service object the slot gives, calling get on the slot", () => {
    const service = Object.freeze({ major: 1, minor: 0 });
    const slot = { get(this: unknown, name: string, major: number) {
      expect(this).toBe(slot);
      return name === "search" && major === 1 ? service : undefined;
    } };
    const found = lookupHostService("search", 1, realm(slot));
    expect(found.kind === "available" && found.service).toBe(service);
  });

  it.each([
    ["null", null],
    ["a number", 7],
    ["a string", "slot"],
    ["no get", {}],
    ["a non-function get", { get: "search" }],
    ["a throwing get", { get() { throw new Error("stub failure"); } }],
    ["a primitive service", { get: () => 1 }],
    ["an array service", { get: () => [] }],
    ["a throwing getter", Object.defineProperty({}, "get", { get() { throw new Error("stub failure"); } })],
  ])("a malformed slot (%s) still counts as a host without the service", (_name, slot) => {
    expect(lookupHostService("search", 1, realm(slot))).toEqual({ kind: "without_service" });
  });

  it("a function-valued slot with a callable get is a broken slot, not a slot", () => {
    const service = Object.freeze({ major: 1, minor: 0 });
    const get = vi.fn(() => service);
    const slot = Object.assign(() => undefined, { get });
    expect(lookupHostService("search", 1, realm(slot))).toEqual({ kind: "without_service" });
    expect(get).not.toHaveBeenCalled();
  });

  it("a throwing key read counts as a host, never as no host", () => {
    const hostile = Object.defineProperty({}, HOST_SLOT_KEY, { get() { throw new Error("stub failure"); } });
    expect(lookupHostService("search", 1, hostile)).toEqual({ kind: "without_service" });
  });

  it("looks on window, not globalThis", () => {
    vi.stubGlobal("window", {});
    vi.stubGlobal(HOST_SLOT_KEY, { get: () => ({}) });
    expect(lookupHostService("search", 1)).toEqual({ kind: "no_host" });
    vi.unstubAllGlobals();
  });

  it("keeps the slot key and event name in one module, marked provisional", () => {
    const owners: string[] = [];
    const walk = (dir: string) => {
      for (const entry of readdirSync(dir, { withFileTypes: true })) {
        const path = `${dir}${entry.name}`;
        if (entry.isDirectory()) walk(`${path}/`);
        else if (/\.ts$/u.test(entry.name) && readFileSync(path, "utf8").includes(HOST_SLOT_KEY)) owners.push(path);
      }
    };
    walk(SRC);
    expect(owners).toEqual([`${SRC}host-services.ts`]);
    const source = readFileSync(`${SRC}host-services.ts`, "utf8");
    expect(source).toContain("PROVISIONAL");
    expect(HOST_AVAILABLE_EVENT).toBe(`${HOST_SLOT_KEY}:available`);
  });
});

describe("shared error data", () => {
  it("has exactly the ten shared codes", () => {
    expect([...SHARED_CODES].sort()).toEqual(["access_denied", "cancelled", "conflict", "declined", "invalid_request",
      "invalidated", "limited", "not_enabled", "read_only", "unavailable"]);
  });

  it.each(SHARED_CODES.map((code) => [code]))("reads the shared code %s as its own family", (code) => {
    expect(readHostError({ code, family: code })).toMatchObject({ code, family: code });
  });

  it("maps a known service code to its family", () => {
    expect(readHostError({ code: "invalid_cursor", family: "invalidated" }, { invalid_cursor: "invalidated" }))
      .toMatchObject({ code: "invalid_cursor", family: "invalidated" });
  });

  it.each([
    [{ code: "future_code", family: "limited" }, "limited"],
    [{ code: "future_code", family: "made_up" }, "unavailable"],
    [{ code: "future_code" }, "unavailable"],
    [{ family: "conflict" }, "conflict"],
    [null, "unavailable"],
    ["access_denied", "unavailable"],
    [Object.defineProperty({}, "code", { get() { throw new Error("stub failure"); } }), "unavailable"],
  ])("handles unknown codes by family: %j -> %s", (error, family) => {
    expect(readHostError(error)).toMatchObject({ code: null, family });
  });

  it("reads each field once and keeps only safe numbers", () => {
    const reads = vi.fn();
    const field = <T>(value: T) => ({ get: () => { reads(); return value; }, enumerable: true });
    const error = Object.defineProperties({}, {
      code: field("limited"), family: field("limited"), retryAfterMs: field(-1),
      limit: field({ dimension: "pageItems", maximum: Infinity, origin: "deployment" }), decidedBy: field("folder"),
    });
    expect(readHostError(error)).toEqual({ code: "limited", family: "limited", retryAfterMs: null,
      limit: { dimension: "pageItems", maximum: null, origin: "deployment" }, decidedBy: "folder" });
    expect(reads).toHaveBeenCalledTimes(5);
  });

  it.each([
    ["protocol", "default"], ["deployment", "your server"], ["organization", "your organization"],
    ["workspace", "this workspace"], ["folder", "this folder"], ["future-level", "policy"], [undefined, "policy"],
    ["__proto__", "policy"], ["toString", "policy"],
  ])("labels the deciding level %s as %s (open list)", (level, label) => {
    expect(decidingLevelLabel(level)).toBe(label);
  });
});

describe("local indexing answers", () => {
  it.each([
    [{ localIndexing: { value: "memory_only", decidedBy: "protocol" } }, { kind: "memory_only", decidedBy: "protocol" }],
    [{ localIndexing: { value: "refuse", decidedBy: "organization" } }, { kind: "refused", decidedBy: "organization", limited: false }],
    [{ localIndexing: { value: "persistent_custodied", decidedBy: "workspace" } }, { kind: "refused", decidedBy: "workspace", limited: false }],
    [{ localIndexing: { value: "memory_only", decidedBy: "deployment", limit: { dimension: "notes", maximum: 10, origin: "folder" } } },
      { kind: "refused", decidedBy: "folder", limited: true }],
    [{}, { kind: "refused", decidedBy: "protocol", limited: false }],
    [null, { kind: "refused", decidedBy: "protocol", limited: false }],
  ])("decodes %j", (snapshot, state) => {
    expect(decodeLocalIndexing(snapshot)).toEqual(state);
  });

  it("names the deciding level in the fixed wording", () => {
    expect(localIndexingMessage({ kind: "refused", decidedBy: "organization", limited: false }))
      .toBe("Local indexing is turned off by your organization.");
    expect(localIndexingMessage({ kind: "memory_only", decidedBy: "protocol" }))
      .toBe("Local indexing: memory only (default). Nothing is kept after this session.");
    expect(localIndexingMessage({ kind: "ungoverned" })).toBeNull();
  });
});

describe("local indexing watch", () => {
  afterEach(() => {
    vi.unstubAllGlobals();
  });

  function policyHost(answer: unknown = { value: "memory_only", decidedBy: "protocol" }) {
    const subscribers = new Set<(event: unknown) => void>();
    const invalidation = new Set<(event: unknown) => void>();
    const policy = {
      major: 1,
      minor: 0,
      current: vi.fn(async () => ({ localIndexing: answer })),
      subscribe: vi.fn((listener: (event: unknown) => void, options?: { signal?: AbortSignal }) => {
        subscribers.add(listener);
        options?.signal?.addEventListener("abort", () => subscribers.delete(listener), { once: true });
        return () => { subscribers.delete(listener); };
      }),
      onInvalidated: vi.fn((listener: (event: unknown) => void) => {
        invalidation.add(listener);
        return () => { invalidation.delete(listener); };
      }),
    };
    const window: Record<string, unknown> = {
      [HOST_SLOT_KEY]: { get: (name: string, major: number) => name === "policy" && major === 1 ? policy : undefined },
    };
    const emit = (event: unknown) => { for (const listener of [...subscribers]) listener(event); };
    const invalidate = (reason: string) => { for (const listener of [...invalidation]) listener({ reason }); };
    return { window, policy, subscribers, invalidation, emit, invalidate };
  }

  it("no slot is ungoverned and registers nothing", () => {
    const states: LocalIndexingState[] = [];
    const watch = new LocalIndexingWatch((state) => states.push(state), {});
    watch.refresh();
    expect(watch.state).toEqual({ kind: "ungoverned" });
    expect(states).toEqual([]);
  });

  it("a slot without the policy service is memory_only by default", () => {
    const watch = new LocalIndexingWatch(() => undefined, { [HOST_SLOT_KEY]: { get: () => undefined } });
    watch.refresh();
    expect(watch.state).toEqual({ kind: "memory_only", decidedBy: "protocol" });
  });

  it("is pending until the first answer, follows changes, and treats resync and unknown events as pending", async () => {
    const host = policyHost();
    const states: string[] = [];
    const watch = new LocalIndexingWatch((state) => states.push(state.kind), host.window);
    watch.refresh();
    expect(watch.state.kind).toBe("pending");
    await Promise.resolve();
    await Promise.resolve();
    expect(watch.state.kind).toBe("memory_only");
    host.emit({ type: "change", snapshot: { localIndexing: { value: "refuse", decidedBy: "folder" } } });
    expect(watch.state).toEqual({ kind: "refused", decidedBy: "folder", limited: false });
    host.emit({ type: "resync" });
    expect(watch.state.kind).toBe("pending");
    host.emit({ type: "snapshot", snapshot: { localIndexing: { value: "memory_only", decidedBy: "protocol" } } });
    host.emit({ type: "a-future-event" });
    expect(states).toEqual(["pending", "memory_only", "refused", "pending", "memory_only", "pending"]);
    watch.dispose();
    expect({ subscribers: host.subscribers.size, invalidation: host.invalidation.size }).toEqual({ subscribers: 0, invalidation: 0 });
  });

  it("a rejected current() with no subscription answer refuses", async () => {
    const host = policyHost();
    host.policy.current.mockRejectedValueOnce({ code: "unavailable", family: "unavailable" });
    const watch = new LocalIndexingWatch(() => undefined, host.window);
    watch.refresh();
    await Promise.resolve();
    await Promise.resolve();
    expect(watch.state).toEqual({ kind: "refused", decidedBy: "protocol", limited: false });
    watch.dispose();
  });

  it("a malformed policy service refuses", () => {
    const watch = new LocalIndexingWatch(() => undefined, {
      [HOST_SLOT_KEY]: { get: () => ({ major: 1, minor: 0, current: 1 }) },
    });
    watch.refresh();
    expect(watch.state.kind).toBe("refused");
  });

  it("keeps the last answer after withdrawal and re-looks after other invalidations", async () => {
    const host = policyHost({ value: "refuse", decidedBy: "organization" });
    const watch = new LocalIndexingWatch(() => undefined, host.window);
    watch.refresh();
    await Promise.resolve();
    await Promise.resolve();
    host.invalidate("lifecycle");
    expect(host.policy.subscribe).toHaveBeenCalledTimes(2);
    await Promise.resolve();
    await Promise.resolve();
    expect(watch.state.kind).toBe("refused");
    delete host.window[HOST_SLOT_KEY];
    host.invalidate("withdrawn");
    expect({ subscribers: host.subscribers.size, invalidation: host.invalidation.size }).toEqual({ subscribers: 0, invalidation: 0 });
    watch.refresh();
    expect(watch.state).toEqual({ kind: "refused", decidedBy: "organization", limited: false });
    watch.dispose();
  });

  it("review regression: withdrawal before the first answer applies the memory-only default", () => {
    const host = policyHost();
    host.policy.current.mockImplementation(() => new Promise(() => undefined));
    const watch = new LocalIndexingWatch(() => undefined, host.window);
    watch.refresh();
    expect(watch.state.kind).toBe("pending");
    delete host.window[HOST_SLOT_KEY];
    host.invalidate("withdrawn");
    expect(watch.state).toEqual({ kind: "memory_only", decidedBy: "protocol" });
    watch.refresh();
    expect(watch.state).toEqual({ kind: "memory_only", decidedBy: "protocol" });
    watch.dispose();
  });

  it.each([
    ["a resync", { type: "resync" }],
    ["an unknown event", { type: "a-future-event" }],
  ])("withdrawal after %s keeps the last refuse answer, never the looser default", async (_name, event) => {
    const host = policyHost({ value: "refuse", decidedBy: "organization" });
    const watch = new LocalIndexingWatch(() => undefined, host.window);
    watch.refresh();
    await Promise.resolve();
    await Promise.resolve();
    expect(watch.state.kind).toBe("refused");
    host.emit(event);
    expect(watch.state.kind).toBe("pending");
    delete host.window[HOST_SLOT_KEY];
    host.invalidate("withdrawn");
    expect(watch.state).toEqual({ kind: "refused", decidedBy: "organization", limited: false });
    watch.refresh();
    expect(watch.state).toEqual({ kind: "refused", decidedBy: "organization", limited: false });
    watch.dispose();
  });

  it("a refresh with the same service object does not subscribe twice", () => {
    const host = policyHost();
    const watch = new LocalIndexingWatch(() => undefined, host.window);
    watch.refresh();
    watch.refresh();
    expect(host.policy.subscribe).toHaveBeenCalledTimes(1);
    watch.dispose();
  });
});
