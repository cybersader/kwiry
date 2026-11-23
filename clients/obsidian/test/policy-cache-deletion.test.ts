// SPDX-FileCopyrightText: 2026 cybersader
// SPDX-License-Identifier: GPL-3.0-only
//
// Owner decision A2: on a stricter local indexing answer the plugin deletes
// the index cache it stored for this vault, and nothing else.

import { createHash } from "node:crypto";
import {
  existsSync,
  mkdirSync,
  mkdtempSync,
  readdirSync,
  readFileSync,
  rmSync,
  statSync,
  symlinkSync,
  utimesSync,
  writeFileSync,
} from "node:fs";
import * as os from "node:os";
import * as path from "node:path";

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { createInPluginCacheCustody } from "../src/cache/build-cache-options";
import type {
  CacheStoreBundleAvailability,
  CacheStoreBundlePort,
  CacheWrite,
} from "../src/cache/cache-store";
import {
  deleteLocalVaultCache,
  deleteVaultCache,
  nodeCacheFileSystem,
  openLocalCacheStore,
  type CacheFileSystem,
  type VaultCacheDeletion,
} from "../src/cache/local-cache-store";
import { deriveVaultCacheIdentity } from "../src/cache/vault-identity";
import { WORKER_PROTOCOL_VERSION } from "../src/worker/protocol";

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

/** Real file system with chosen calls replaced. */
function fileSystem(overrides: Partial<CacheFileSystem>): CacheFileSystem {
  return { ...nodeCacheFileSystem(), ...overrides };
}

function removeWith(fs: CacheFileSystem): Promise<VaultCacheDeletion> {
  return deleteLocalVaultCache({
    canonicalVaultPath: vaultPath,
    vaultConfigDirName: ".obsidian",
    rootOverride: rootPath,
    fs,
  });
}

function leftovers(): string[] {
  return readdirSync(path.join(rootPath, "vaults")).filter((entry) => entry.includes(".deleting-"));
}

describe("deleting under the writer lock (review N1, N2, N4)", () => {
  it("N1: a failed removal leaves no writer lock at the vault's path, and the next deletion sweeps what is left", async () => {
    storeCache();
    const failing = fileSystem({ rm: async () => { throw new Error("synthetic removal failure"); } });
    await expect(removeWith(failing)).resolves.toEqual({ kind: "failed" });
    expect(existsSync(path.join(vaultCacheDirectory(), "writer.lock"))).toBe(false);
    for (const entry of leftovers()) {
      expect(existsSync(path.join(rootPath, "vaults", entry, "writer.lock"))).toBe(false);
    }
    await expect(remove()).resolves.toEqual({ kind: "deleted" });
    expect({ target: existsSync(vaultCacheDirectory()), leftovers: leftovers() })
      .toEqual({ target: false, leftovers: [] });
  });

  it("N1: a directory that changed under the lock is refused and the lock is released", async () => {
    storeCache();
    const target = vaultCacheDirectory();
    const real = nodeCacheFileSystem();
    let locked = false;
    const swapped = fileSystem({
      open: async (file, flags, mode) => {
        if (file === path.join(target, "writer.lock")) locked = true;
        return real.open(file, flags, mode);
      },
      realpath: async (file) => locked && file === target ? `${file}-elsewhere` : real.realpath(file),
    });
    await expect(removeWith(swapped)).resolves.toEqual({ kind: "refused" });
    expect(existsSync(path.join(target, "writer.lock"))).toBe(false);
    expect(existsSync(path.join(target, "current.json"))).toBe(true);
  });

  it("N2: the vault directory leaves its path in one rename before anything inside it is removed", async () => {
    storeCache();
    const target = vaultCacheDirectory();
    const real = nodeCacheFileSystem();
    const calls: string[] = [];
    const recording = fileSystem({
      rename: async (from, to) => {
        calls.push(`rename ${path.relative(rootPath, from)} -> ${path.relative(rootPath, to)}`);
        await real.rename(from, to);
      },
      rm: async (file, options) => {
        // Nothing is removed while the directory is still at the vault's path.
        expect(existsSync(target)).toBe(false);
        calls.push(`rm ${path.relative(rootPath, file)}`);
        await real.rm(file, options);
      },
    });
    await expect(removeWith(recording)).resolves.toEqual({ kind: "deleted" });
    expect(calls).toHaveLength(2);
    expect(calls[0]).toMatch(new RegExp(`^rename vaults/${identity} -> vaults/\\.${identity}\\.deleting-[a-z0-9-]+$`, "u"));
    expect(calls[1]).toBe(`rm ${calls[0]!.split(" -> ")[1]}`);
    expect(leftovers()).toEqual([]);
  });

  it("N2: sweeps only this vault's leftovers, and never through a symlink", async () => {
    storeCache();
    const vaults = path.join(rootPath, "vaults");
    const own = path.join(vaults, `.${identity}.deleting-abc123`);
    const other = path.join(vaults, `.${"f".repeat(64)}.deleting-abc123`);
    mkdirSync(own);
    writeFileSync(path.join(own, "g0.kwc"), "image");
    mkdirSync(other);
    writeFileSync(path.join(other, "g0.kwc"), "image");
    const outside = path.join(workspace, "outside-leftover");
    mkdirSync(outside);
    writeFileSync(path.join(outside, "precious.txt"), "keep");
    symlinkSync(outside, path.join(vaults, `.${identity}.deleting-linked`), "dir");
    await expect(remove()).resolves.toEqual({ kind: "deleted" });
    expect(existsSync(own)).toBe(false);
    expect(existsSync(path.join(other, "g0.kwc"))).toBe(true);
    expect(readFileSync(path.join(outside, "precious.txt"), "utf8")).toBe("keep");
  });

  it("N2: a leftover alone is swept on the next deletion", async () => {
    const own = path.join(rootPath, "vaults", `.${identity}.deleting-abc123`);
    mkdirSync(own, { recursive: true });
    writeFileSync(path.join(own, "g0.kwc"), "image");
    await expect(remove()).resolves.toEqual({ kind: "deleted" });
    expect(existsSync(own)).toBe(false);
  });

  it("N4: a symlinked cache root is unavailable, not refused, and is left alone", async () => {
    const realRoot = path.join(workspace, "real-root");
    mkdirSync(path.join(realRoot, "vaults", identity), { recursive: true });
    writeFileSync(path.join(realRoot, "vaults", identity, "current.json"), "{}");
    mkdirSync(path.dirname(rootPath), { recursive: true });
    symlinkSync(realRoot, rootPath, "dir");
    await expect(remove()).resolves.toEqual({ kind: "unavailable", reason: "root_not_a_directory" });
    expect(existsSync(path.join(realRoot, "vaults", identity, "current.json"))).toBe(true);
  });
});

describe("end to end with real stores", () => {
  function envelopeWrite(generationId: string, seed: number): CacheWrite {
    const bytes = new Uint8Array(256).fill(seed);
    return {
      generationId,
      byteLength: bytes.byteLength,
      sha256: createHash("sha256").update(bytes).digest("hex"),
      identity: {
        protocol_version: WORKER_PROTOCOL_VERSION,
        cache_schema_version: 1,
        chunking_version: 1,
        sqlite_version: "3.53.0",
        sqlite_wasm_sha256: "b".repeat(64),
        rust_wasm_sha256: "c".repeat(64),
        plugin_id: "kwiry-search",
        plugin_version: "0.1.0",
        cache_identity: identity,
        source_policy_hash: "d".repeat(64),
      },
      bytes,
    };
  }

  function snapshotOutside(): Record<string, string> {
    const files: Record<string, string> = {};
    const excluded = vaultCacheDirectory();
    const walk = (directory: string) => {
      for (const entry of readdirSync(directory)) {
        const full = path.join(directory, entry);
        if (full === excluded || entry.includes(".deleting-") || entry.startsWith(".store-probe-")) continue;
        const stats = statSync(full);
        if (stats.isDirectory()) walk(full);
        else files[path.relative(workspace, full)] = readFileSync(full, "utf8");
      }
    };
    walk(workspace);
    return files;
  }

  async function open(fs?: CacheFileSystem): Promise<CacheStoreBundlePort> {
    const availability = await openLocalCacheStore({
      canonicalVaultPath: vaultPath,
      vaultConfigDirName: ".obsidian",
      rootOverride: rootPath,
      ...(fs === undefined ? {} : { fs }),
    });
    if (availability.kind !== "available") throw new Error(`store unavailable: ${availability.reason}`);
    return availability.store;
  }

  it("deletes after a second window's write in progress finishes, and touches nothing outside the vault directory", async () => {
    // This page's store writes a generation through the custody.
    const outcomes: VaultCacheDeletion[] = [];
    let releaseWrite!: () => void;
    const writeGate = new Promise<void>((resolve) => { releaseWrite = resolve; });
    let otherWrite: Promise<unknown> = Promise.resolve();
    let duringLock: { pointer: boolean; lock: boolean } | null = null;
    const custody = createInPluginCacheCustody({ adapter: {}, configDir: ".obsidian" } as never, {
      open: () => openLocalCacheStore({
        canonicalVaultPath: vaultPath,
        vaultConfigDirName: ".obsidian",
        rootOverride: rootPath,
      }),
      remove: async () => {
        const outcome = await remove();
        outcomes.push(outcome);
        return outcome;
      },
      wait: async () => {
        duringLock ??= {
          pointer: existsSync(path.join(vaultCacheDirectory(), "current.json")),
          lock: existsSync(path.join(vaultCacheDirectory(), "writer.lock")),
        };
        releaseWrite();
        await otherWrite;
      },
    });
    const opened = await custody.options("d".repeat(64)).openStore();
    if (opened.kind !== "available") throw new Error("store unavailable");
    await opened.store.put(envelopeWrite("g1", 1));
    expect(existsSync(path.join(vaultCacheDirectory(), "current.json"))).toBe(true);

    // A second window's store is mid-write: it holds the lock and waits
    // before committing its pointer.
    const real = nodeCacheFileSystem();
    let reached!: () => void;
    const atCommit = new Promise<void>((resolve) => { reached = resolve; });
    const other = await open(fileSystem({
      rename: async (from, to) => {
        if (to.endsWith("current.json")) {
          reached();
          await writeGate;
        }
        await real.rename(from, to);
      },
    }));
    mkdirSync(path.join(rootPath, "vaults", "f".repeat(64)), { recursive: true });
    writeFileSync(path.join(rootPath, "vaults", "f".repeat(64), "current.json"), "other vault");
    writeFileSync(path.join(rootPath, "unrelated.txt"), "keep");
    otherWrite = other.put(envelopeWrite("g2", 2));
    await atCommit;
    const before = snapshotOutside();

    await expect(custody.deleteForPolicy()).resolves.toEqual({ kind: "deleted" });
    // The first attempt met the other window's lock and left everything in place.
    expect(outcomes.map(({ kind }) => kind)).toEqual(["locked", "deleted"]);
    expect(duringLock).toEqual({ pointer: true, lock: true });
    // The other window's write was not cut off.
    await expect(otherWrite).resolves.toMatchObject({ generationId: "g2" });
    await other.dispose();
    expect({ target: existsSync(vaultCacheDirectory()), leftovers: leftovers() })
      .toEqual({ target: false, leftovers: [] });
    expect(snapshotOutside()).toEqual(before);
    notesIntact();
  });
});
