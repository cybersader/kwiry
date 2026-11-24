// SPDX-FileCopyrightText: 2026 cybersader
// SPDX-License-Identifier: GPL-3.0-only

import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

import { afterEach, describe, expect, it, vi } from "vitest";

import { BackendManager } from "../src/backend-manager";
import { HostSearchBackend } from "../src/backends/host-search-backend";
import {
  hostInvalidationMessage,
  hostSearchFailure,
  lookupHostSearch,
  type HostSearchSession,
} from "../src/host-search";
import { HOST_SLOT_KEY } from "../src/host-services";
import { createPublicSearchApi } from "../src/public-api";

const NOTICE = "Results may be incomplete.";
const page = vi.fn();
const open = vi.fn();
const onInvalidated = vi.fn(() => () => undefined);

function service(overrides: Record<string, unknown> = {}) {
  return { major: 1, minor: 0, page, open, onInvalidated, ...overrides };
}

function hostRealm(implementation: unknown) {
  return { [HOST_SLOT_KEY]: { get: (name: string, major: number) => name === "search" && major === 1 ? implementation : undefined } };
}

function session(implementation: unknown = service()): HostSearchSession {
  const found = lookupHostSearch(hostRealm(implementation));
  if (found.situation !== "available") throw new Error(`expected search, got ${found.situation}`);
  return found.session;
}

function unexpectedRankedFactory(): never {
  throw new Error("A ranked backend must not be constructed in host mode.");
}

describe("search v1 lookup", () => {
  afterEach(() => {
    vi.unstubAllGlobals();
    vi.clearAllMocks();
  });

  it("reports the three situations", () => {
    expect(lookupHostSearch({}).situation).toBe("no_host");
    expect(lookupHostSearch(hostRealm(undefined)).situation).toBe("without_service");
    expect(lookupHostSearch(hostRealm(service())).situation).toBe("available");
  });

  it.each([
    ["a primitive", 2],
    ["an array", []],
    ["a non-callable open", service({ open: 2 })],
    ["a non-callable page", service({ page: "method" })],
    ["no invalidation hook", service({ onInvalidated: undefined })],
    ["a draft-1 shape", { page, resolveNote: open }],
    ["the wrong major", service({ major: 2 })],
    ["a non-integer minor", service({ minor: 0.5 })],
  ])("a malformed service (%s) is a host without the service", (_name, implementation) => {
    expect(lookupHostSearch(hostRealm(implementation)).situation).toBe("without_service");
  });

  it("calls page and open on the service object with exactly the closed request and options", async () => {
    const signal = new AbortController().signal;
    const implementation = service({
      async page(this: unknown, request: unknown, options: unknown) {
        expect(this).toBe(implementation);
        expect(request).toEqual({ query: "mauve lynx" });
        expect(options).toEqual({ signal });
        return { room: null, state: "building", items: [], next: null, notice: NOTICE };
      },
      async open(this: unknown, ref: unknown, options: unknown) {
        expect(this).toBe(implementation);
        expect([ref, options]).toEqual(["opaque-lynx", { signal }]);
      },
    });
    const host = session(implementation);
    await expect(host.page("mauve lynx", signal)).resolves.toEqual({
      roomLabel: null, rows: [], continuation: null, status: "Host search is being prepared.", notice: NOTICE,
    });
    await expect(host.open("opaque-lynx", signal)).resolves.toBeUndefined();
  });

  it("passes the invalidation reason through and returns a disposer that is safe to call twice", () => {
    const dispose = vi.fn(() => { throw new Error("stub failure"); });
    let registered: ((event: unknown) => void) | undefined;
    const host = session(service({ onInvalidated: (listener: (event: unknown) => void) => { registered = listener; return dispose; } }));
    const reasons: Array<string | null> = [];
    const stop = host.onInvalidated((reason) => reasons.push(reason));
    registered?.({ reason: "withdrawn" });
    registered?.({ reason: 7 });
    registered?.(null);
    stop();
    stop();
    expect(reasons).toEqual(["withdrawn", null, null]);
    expect(dispose).toHaveBeenCalledTimes(1);
  });

  it("keeps duplicate references: equality is not meaningful", async () => {
    const hit = { ref: "same", path: "Lime/A.md", title: "A", excerpt: "" };
    page.mockResolvedValueOnce({ room: { ref: "r", label: "Lime" }, state: "ready", items: [hit, hit], next: null, notice: NOTICE });
    const result = await session().page("lime", new AbortController().signal);
    expect(result.rows).toHaveLength(2);
  });

  it("snapshots page, room and hit accessors once before display", async () => {
    const secondRead = vi.fn(() => "unvalidated replacement");
    const changing = <T>(value: T) => vi.fn<() => T | string>().mockReturnValueOnce(value).mockImplementation(secondRead);
    const fields = { ref: changing("opaque-sparrow"), path: changing("Lime/Sparrow.md"),
      title: changing("Sparrow"), excerpt: changing("a **literal** leaf") };
    const room = { ref: changing("room-lime"), label: changing("Lime") };
    const accessor = <T extends Record<string, () => unknown>>(getters: T) =>
      Object.defineProperties({}, Object.fromEntries(Object.entries(getters).map(([key, get]) => [key, { get }])));
    const boundary = { room: changing(accessor(room)), state: changing("ready"), items: changing([accessor(fields)]),
      next: changing("opaque-next"), notice: changing(NOTICE), decidedBy: changing(undefined) };
    page.mockResolvedValueOnce(accessor(boundary));
    const snapshot = await session().page("lime sparrow", new AbortController().signal);
    expect({ snapshot, repeatedReads: secondRead.mock.calls.length }).toEqual({
      snapshot: { roomLabel: "Lime", rows: [{ noteRef: "opaque-sparrow", path: "Lime/Sparrow.md", title: "Sparrow",
        excerpt: "a **literal** leaf" }], continuation: "opaque-next", status: "Results from the host", notice: NOTICE },
      repeatedReads: 0,
    });
    for (const get of [...Object.values(fields), ...Object.values(room), ...Object.values(boundary)]) {
      expect(get).toHaveBeenCalledTimes(1);
    }
  });

  it.each([
    ["a missing notice", { room: null, state: "building", items: [], next: null }],
    ["a different notice", { room: null, state: "building", items: [], next: null, notice: "Done." }],
    ["rows on a known non-ready state", { room: null, state: "building", items: [{}], next: null, notice: NOTICE }],
    ["a ready page without a room", { room: null, state: "ready", items: [], next: null, notice: NOTICE }],
    ["a malformed hit", { room: { ref: "r", label: "L" }, state: "ready", items: [{ ref: "x" }], next: null, notice: NOTICE }],
  ])("rejects a malformed page (%s) whole, as unavailable", async (_name, value) => {
    page.mockResolvedValueOnce(value);
    await expect(session().page("lime", new AbortController().signal)).rejects.toMatchObject({ code: "unavailable", family: "unavailable" });
  });

  it("reads error and limit accessors once before formatting", () => {
    const secondRead = vi.fn(() => "unvalidated replacement");
    const changing = (value: unknown) => vi.fn<() => unknown>().mockReturnValueOnce(value).mockImplementation(secondRead);
    const dimension = changing("queryBytes");
    const origin = changing("organization");
    const maximum = changing(19);
    const limit = changing(Object.defineProperties({}, { dimension: { get: dimension }, origin: { get: origin }, maximum: { get: maximum } }));
    const retryAfterMs = changing(1_500);
    const code = changing("limited");
    const error = Object.defineProperties({}, { code: { get: code }, limit: { get: limit }, retryAfterMs: { get: retryAfterMs } });
    expect({ snapshot: hostSearchFailure(error), repeatedReads: secondRead.mock.calls.length }).toEqual({
      snapshot: { action: "keep", message: "Host search reached a limit. Maximum: 19 query bytes, set by your organization. Try again in 2 seconds." },
      repeatedReads: 0,
    });
    for (const get of [code, limit, retryAfterMs, dimension, origin, maximum]) expect(get).toHaveBeenCalledTimes(1);
  });

  it.each([
    ["requestBytes", 65_536, "protocol", "Host search reached a limit. Maximum: 65536 request bytes, set by default."],
    ["queryBytes", 11, "workspace", "Host search reached a limit. Maximum: 11 query bytes, set by this workspace."],
    ["a-future-dimension", 3, "protocol", "Host search reached a limit. The limit is set by default."],
  ])("names the %s limit dimension in fixed wording", (dimension, maximum, origin, message) => {
    expect(hostSearchFailure({ code: "limited", family: "limited", limit: { dimension, maximum, origin } }))
      .toEqual({ action: "keep", message });
  });

  it.each([
    ["access", false], ["workspace", false], ["lifecycle", false], ["withdrawn", true], [null, true], ["future", true],
  ])("invalidation reason %s detaches=%s", (reason, detach) => {
    expect(hostInvalidationMessage(reason).detach).toBe(detach);
  });

  it.each([false, true])("never constructs another backend or dispatches through the public API (supported=%s)", async (supported) => {
    vi.stubGlobal("window", hostRealm(supported ? service() : undefined));
    const daemon = vi.fn(unexpectedRankedFactory);
    const in_plugin = vi.fn(unexpectedRankedFactory);
    const host = vi.fn((id: string) => new HostSearchBackend(id));
    const manager = new BackendManager({ daemon, in_plugin, host });
    const backend = await manager.activate("host");
    expect(host).toHaveBeenCalledExactlyOnceWith("host-1");
    expect(daemon).not.toHaveBeenCalled();
    expect(in_plugin).not.toHaveBeenCalled();
    expect(await manager.current()).toBe(backend);
    expect(await backend.status()).toEqual({ kind: "host", identity: backend.identity,
      situation: supported ? "available" : "without_service" });
    expect(backend.identity).toEqual({ profile: "host", label: "Host search · Lexical", instanceId: "host-1" });
    const { api } = createPublicSearchApi({ currentBackend: () => manager.current(), defaultMode: () => "lexical" });
    await expect(api.ready()).rejects.toMatchObject({ code: "backend_unavailable" });
    await expect(api.search("mauve lynx")).rejects.toMatchObject({ code: "backend_unavailable" });
    expect(page).not.toHaveBeenCalled();
    expect(open).not.toHaveBeenCalled();
    await manager.dispose();
    expect(await backend.status()).toMatchObject({ situation: "without_service" });
  });

  it("no polling remains in the plugin source", () => {
    const sources = ["host-search.ts", "host-services.ts", "host-search-modal.ts", "main.ts", "settings-tab.ts",
      "backends/host-search-backend.ts"].map((file) => readFileSync(fileURLToPath(new URL(`../src/${file}`, import.meta.url)), "utf8"));
    for (const source of sources) {
      expect(source).not.toMatch(/watchForHostSearch|LATE_HOST|awaitLateHost|adoptLateHost|lateHostPending|startingUp/u);
    }
    for (const source of sources.slice(0, 3)) expect(source).not.toMatch(/setTimeout|setInterval/u);
  });
});

describe("backend manager deactivation", () => {
  it("stops the active backend without closing the manager", async () => {
    const dispose = vi.fn(async () => undefined);
    const backend = { identity: { profile: "in_plugin", instanceId: "", label: "In-plugin", boundVaultId: null },
      initialize: async () => undefined, status: async () => ({}), search: async () => ({}), dispose };
    const manager = new BackendManager({
      daemon: unexpectedRankedFactory,
      in_plugin: (instanceId) => ({ ...backend, identity: { ...backend.identity, instanceId } }) as never,
    });
    await manager.activate("in_plugin");
    await manager.deactivate();
    expect(dispose).toHaveBeenCalledTimes(1);
    await expect(manager.current()).rejects.toMatchObject({ code: "backend_not_selected" });
    await manager.activate("in_plugin");
    await expect(manager.current()).resolves.toMatchObject({ identity: { instanceId: "in_plugin-2" } });
    await manager.dispose();
  });
});
