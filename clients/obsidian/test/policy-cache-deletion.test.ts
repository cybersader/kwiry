// SPDX-FileCopyrightText: 2026 cybersader
// SPDX-License-Identifier: GPL-3.0-only
//
// Owner decision A2: on a stricter local indexing answer the plugin deletes
// the index cache it stored for this vault, and nothing else.

import {
  existsSync,
  mkdirSync,
  mkdtempSync,
  readFileSync,
  rmSync,
  symlinkSync,
  utimesSync,
  writeFileSync,
} from "node:fs";
import * as os from "node:os";
import * as path from "node:path";

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { createInPluginCacheCustody } from "../src/cache/build-cache-options";
import type { CacheStoreBundleAvailability, CacheStoreBundlePort } from "../src/cache/cache-store";
import {
  deleteLocalVaultCache,
  deleteVaultCache,
  type VaultCacheDeletion,
} from "../src/cache/local-cache-store";
import { deriveVaultCacheIdentity } from "../src/cache/vault-identity";

let workspace: string;
let vaultPath: string;
let rootPath: string;
let identity: string;

beforeEach(() => {
  workspace = mkdtempSync(path.join(os.tmpdir(), "kwiry-policy-delete-"));
  vaultPath = path.join(workspace, "Quillwort-Synthetic-Vault");
  rootPath = path.join(workspace, "os-cache", "kwiry", "obsidian-cache");
  identity = deriveVaultCacheIdentity({ platform: process.platform, canonicalVaultPath: vaultPath });
  mkdirSync(path.join(vaultPath, ".obsidian", "plugins", "kwiry-search"), { recursive: true });
  writeFileSync(path.join(vaultPath, "Note.md"), "# Synthetic note\n");
  writeFileSync(path.join(vaultPath, ".obsidian", "plugins", "kwiry-search", "data.json"), "{}");
});

afterEach(() => {
  rmSync(workspace, { recursive: true, force: true });
});

function vaultCacheDirectory(id = identity): string {
  return path.join(rootPath, "vaults", id);
}

function storeCache(id = identity): void {
  mkdirSync(path.join(vaultCacheDirectory(id), "generations"), { recursive: true });
  mkdirSync(path.join(vaultCacheDirectory(id), "initial-build", "images"), { recursive: true });
  writeFileSync(path.join(vaultCacheDirectory(id), "current.json"), "{}");
  writeFileSync(path.join(vaultCacheDirectory(id), "generations", "g1.kwc"), "image");
  writeFileSync(path.join(vaultCacheDirectory(id), "initial-build", "images", "c1.kwc"), "image");
}

function remove(): Promise<VaultCacheDeletion> {
  return deleteLocalVaultCache({
    canonicalVaultPath: vaultPath,
    vaultConfigDirName: ".obsidian",
    rootOverride: rootPath,
  });
}

function notesIntact(): void {
  expect(readFileSync(path.join(vaultPath, "Note.md"), "utf8")).toBe("# Synthetic note\n");
  expect(readFileSync(path.join(vaultPath, ".obsidian", "plugins", "kwiry-search", "data.json"), "utf8"))
    .toBe("{}");
}

describe("deleting this vault's stored index cache", () => {
  it("removes this vault's cache directory and nothing beside it", async () => {
    storeCache();
    const other = "f".repeat(64);
    storeCache(other);
    writeFileSync(path.join(rootPath, "unrelated.txt"), "keep");
    await expect(remove()).resolves.toEqual({ kind: "deleted" });
    expect(existsSync(vaultCacheDirectory())).toBe(false);
    expect(existsSync(path.join(vaultCacheDirectory(other), "current.json"))).toBe(true);
    expect(existsSync(path.join(rootPath, "unrelated.txt"))).toBe(true);
    expect(existsSync(path.join(rootPath, "vaults"))).toBe(true);
    notesIntact();
  });

  it("reports absent, and creates nothing, when no cache was ever stored", async () => {
    await expect(remove()).resolves.toEqual({ kind: "absent" });
    expect(existsSync(path.join(workspace, "os-cache"))).toBe(false);
    mkdirSync(path.join(rootPath, "vaults"), { recursive: true });
    await expect(remove()).resolves.toEqual({ kind: "absent" });
  });

  it("refuses a vault cache directory that is a symlink and leaves its target alone", async () => {
    const outside = path.join(workspace, "outside");
    mkdirSync(outside);
    writeFileSync(path.join(outside, "precious.txt"), "keep");
    mkdirSync(path.join(rootPath, "vaults"), { recursive: true });
    symlinkSync(outside, vaultCacheDirectory(), "dir");
    await expect(remove()).resolves.toEqual({ kind: "refused" });
    expect(readFileSync(path.join(outside, "precious.txt"), "utf8")).toBe("keep");
  });

  it("refuses a symlinked vaults directory that would lead outside the cache root", async () => {
    const outside = path.join(workspace, "outside-vaults");
    mkdirSync(path.join(outside, identity), { recursive: true });
    writeFileSync(path.join(outside, identity, "precious.txt"), "keep");
    mkdirSync(rootPath, { recursive: true });
    symlinkSync(outside, path.join(rootPath, "vaults"), "dir");
    await expect(remove()).resolves.toEqual({ kind: "refused" });
    expect(readFileSync(path.join(outside, identity, "precious.txt"), "utf8")).toBe("keep");
  });

  it("does not follow a symlink inside the cache directory", async () => {
    storeCache();
    const outside = path.join(workspace, "linked-target");
    mkdirSync(outside);
    writeFileSync(path.join(outside, "precious.txt"), "keep");
    symlinkSync(outside, path.join(vaultCacheDirectory(), "generations", "escape"), "dir");
    await expect(remove()).resolves.toEqual({ kind: "deleted" });
    expect(readFileSync(path.join(outside, "precious.txt"), "utf8")).toBe("keep");
  });

  it("never deletes inside the vault, even when the cache root points there", async () => {
    const inside = path.join(vaultPath, "cache-root");
    mkdirSync(path.join(inside, "vaults", identity), { recursive: true });
    writeFileSync(path.join(inside, "vaults", identity, "current.json"), "{}");
    await expect(deleteLocalVaultCache({
      canonicalVaultPath: vaultPath,
      vaultConfigDirName: ".obsidian",
      rootOverride: inside,
    })).resolves.toEqual({ kind: "unavailable", reason: "root_inside_vault" });
    expect(existsSync(path.join(inside, "vaults", identity, "current.json"))).toBe(true);
    notesIntact();
  });

  it("leaves a cache whose writer lock another window holds, and takes over a stale lock", async () => {
    storeCache();
    const lock = path.join(vaultCacheDirectory(), "writer.lock");
    writeFileSync(lock, "{}");
    await expect(remove()).resolves.toEqual({ kind: "locked" });
    expect(existsSync(path.join(vaultCacheDirectory(), "current.json"))).toBe(true);
    const old = new Date(Date.now() - 60 * 60 * 1000);
    utimesSync(lock, old, old);
    await expect(remove()).resolves.toEqual({ kind: "deleted" });
    expect(existsSync(vaultCacheDirectory())).toBe(false);
  });

  it("reports unavailable when the vault has no file path (browser tab)", async () => {
    await expect(deleteVaultCache({ adapter: {}, vaultConfigDirName: ".obsidian" }))
      .resolves.toEqual({ kind: "unavailable", reason: "vault_location_unavailable" });
  });
});

describe("cache custody", () => {
  function fakeStore(dispose: () => Promise<void>): CacheStoreBundlePort {
    return { vaultCacheIdentity: identity, dispose } as unknown as CacheStoreBundlePort;
  }

  const vault = { adapter: {}, configDir: ".obsidian" } as never;

  it("does not delete while a write queued on an open store is still running", async () => {
    let finishWrite!: () => void;
    const write = new Promise<void>((resolve) => { finishWrite = resolve; });
    const events: string[] = [];
    // A store disposes only after the writes already queued on it.
    const store = fakeStore(async () => { await write; events.push("store disposed"); });
    const removeCache = vi.fn(async (): Promise<VaultCacheDeletion> => {
      events.push("deleted");
      return { kind: "deleted" };
    });
    const custody = createInPluginCacheCustody(vault, {
      open: async () => ({ kind: "available", store }),
      remove: removeCache,
    });
    await custody.options("a".repeat(64)).openStore();
    const deletion = custody.deleteForPolicy();
    await new Promise((resolve) => setImmediate(resolve));
    expect(removeCache).not.toHaveBeenCalled();
    finishWrite();
    await expect(deletion).resolves.toEqual({ kind: "deleted" });
    expect(events).toEqual(["store disposed", "deleted"]);
  });

  it("closes the cache for the page: an open in flight is disposed and later opens are refused", async () => {
    let finishOpen!: (value: CacheStoreBundleAvailability) => void;
    const dispose = vi.fn(async () => undefined);
    const custody = createInPluginCacheCustody(vault, {
      open: () => new Promise((resolve) => { finishOpen = resolve; }),
      remove: async () => ({ kind: "deleted" }),
    });
    const options = custody.options("a".repeat(64));
    const opening = options.openStore();
    const deletion = custody.deleteForPolicy();
    finishOpen({ kind: "available", store: fakeStore(dispose) });
    await expect(opening).rejects.toThrow();
    await expect(deletion).resolves.toEqual({ kind: "deleted" });
    expect(dispose).toHaveBeenCalledTimes(1);
    await expect(options.openStore()).rejects.toThrow();
  });

  it("tries a locked cache a bounded number of times", async () => {
    const removeCache = vi.fn(async (): Promise<VaultCacheDeletion> => ({ kind: "locked" }));
    const wait = vi.fn(async () => undefined);
    const custody = createInPluginCacheCustody(vault, { remove: removeCache, wait });
    await expect(custody.deleteForPolicy()).resolves.toEqual({ kind: "locked" });
    expect(removeCache).toHaveBeenCalledTimes(5);
    expect(wait).toHaveBeenCalledTimes(4);
  });

  it("deletes a real stored cache through the composed path", async () => {
    storeCache();
    const custody = createInPluginCacheCustody(vault, {
      remove: () => deleteLocalVaultCache({
        canonicalVaultPath: vaultPath,
        vaultConfigDirName: ".obsidian",
        rootOverride: rootPath,
      }),
    });
    await expect(custody.deleteForPolicy()).resolves.toEqual({ kind: "deleted" });
    expect(existsSync(vaultCacheDirectory())).toBe(false);
    notesIntact();
  });
});
