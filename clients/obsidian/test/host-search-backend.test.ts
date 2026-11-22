// SPDX-FileCopyrightText: 2026 cybersader
// SPDX-License-Identifier: GPL-3.0-only

import { afterEach, describe, expect, it, vi } from "vitest";

import { BackendManager } from "../src/backend-manager";
import { HostSearchBackend } from "../src/backends/host-search-backend";
import { HOST_GLOBAL, lookupHostSearch } from "../src/host-search";
import { createPublicSearchApi } from "../src/public-api";

const page = vi.fn();
const resolveNote = vi.fn();

function hostRealm(implementation: unknown) {
  return { [HOST_GLOBAL]: { capabilities: { get: () => implementation } } };
}

function unexpectedRankedFactory(): never {
  throw new Error("A ranked backend must not be constructed in host mode.");
}

describe("host capability lifecycle", () => {
  afterEach(() => {
    vi.unstubAllGlobals();
    vi.clearAllMocks();
  });

  it.each([undefined, 2, [], { page, resolveNote: 2 }, { page: "method", resolveNote }].map((invalid) => ({ invalid })))(
    "fails closed for a non-callable implementation: $invalid", ({ invalid }) => {
      expect(lookupHostSearch(hostRealm(invalid))).toBeNull();
    },
  );

  it("fails closed if the lookup throws", () => {
    expect(lookupHostSearch({
      [HOST_GLOBAL]: { capabilities: { get() { throw new Error("stub lookup failure"); } } },
    })).toBeNull();
  });

  it("uses exact major and preserves capability and implementation receivers", async () => {
    const signal = new AbortController().signal;
    const capabilities = {
      get(name: string, major: number) {
        expect(this).toBe(capabilities);
        expect([name, major]).toEqual(["search", 1]);
        return implementation;
      },
    };
    const implementation = {
      async page(request: unknown, options: unknown) {
        expect(this).toBe(implementation);
        expect(request).toEqual({ query: "mauve lynx" });
        expect(options).toEqual({ signal });
        return { room: null, state: "building", items: [], next: null, notice: "Results may be incomplete." };
      },
      async resolveNote(ref: string, options: unknown) {
        expect(this).toBe(implementation);
        expect(ref).toBe("opaque-lynx");
        expect(options).toEqual({ signal });
        return { path: "Mauve/Lynx.md", futureField: "not part of v1" };
      },
    };
    const host = lookupHostSearch({ [HOST_GLOBAL]: { capabilities } });
    await expect(host?.page("mauve lynx", signal)).resolves.toEqual({
      roomLabel: null, rows: [], continuation: null, status: "Host search is being prepared.",
      notice: "Results may be incomplete.",
    });
    await expect(host?.resolveNote("opaque-lynx", signal)).resolves.toBe("Mauve/Lynx.md");
  });

  it.each([false, true])("never constructs another backend or dispatches through the public API (supported=%s)", async (supported) => {
    vi.stubGlobal(HOST_GLOBAL, hostRealm(supported ? { page, resolveNote } : undefined)[HOST_GLOBAL]);
    const daemon = vi.fn(unexpectedRankedFactory);
    const in_plugin = vi.fn(unexpectedRankedFactory);
    const host = vi.fn((id: string) => new HostSearchBackend(id));
    const manager = new BackendManager({ daemon, in_plugin, host });
    const backend = await manager.activate("host");
    expect(host).toHaveBeenCalledExactlyOnceWith("host-1");
    expect(daemon).not.toHaveBeenCalled();
    expect(in_plugin).not.toHaveBeenCalled();
    expect(await manager.current()).toBe(backend);
    expect(await backend.status()).toEqual({ kind: "host", identity: backend.identity, available: supported });
    expect(backend.identity).toEqual({ profile: "host", label: "Host search · Lexical", instanceId: "host-1" });
    const { api } = createPublicSearchApi({ currentBackend: () => manager.current(), defaultMode: () => "lexical" });
    await expect(api.ready()).rejects.toMatchObject({ code: "backend_unavailable" });
    await expect(api.search("mauve lynx")).rejects.toMatchObject({ code: "backend_unavailable" });
    expect(page).not.toHaveBeenCalled();
    expect(resolveNote).not.toHaveBeenCalled();
    await manager.dispose();
    expect(await backend.status()).toMatchObject({ available: false });
  });
});
