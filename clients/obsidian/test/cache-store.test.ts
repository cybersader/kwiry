// SPDX-FileCopyrightText: 2026 cybersader
// SPDX-License-Identifier: GPL-3.0-only

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
  writeFileSync,
} from "node:fs";
import * as os from "node:os";
import * as path from "node:path";

import { afterEach, beforeEach, describe, expect, it } from "vitest";

import {
  CacheStoreError,
  MAX_CACHE_BLOB_BYTES,
  type CacheIdentityEnvelope,
  type CacheLoad,
  type CacheStorePort,
  type CacheWrite,
} from "../src/cache/cache-store";
import { MAX_EXPORT_BLOB_BYTES, WORKER_PROTOCOL_VERSION } from "../src/worker/protocol";
import {
  nodeCacheFileSystem,
  openLocalCacheStore,
  openVaultCacheStore,
  type CacheFileHandle,
  type CacheFileStats,
  type CacheFileSystem,
} from "../src/cache/local-cache-store";
import { deriveVaultCacheIdentity } from "../src/cache/vault-identity";

let workspace: string;
let vaultPath: string;
let rootPath: string;
let vaultIdentity: string;

beforeEach(() => {
  workspace = mkdtempSync(path.join(os.tmpdir(), "kwiry-cache-store-"));
  // Deliberately distinctive: the leak scan searches stored bytes for this
  // name, so a generic word like "vault" would collide with the store's own
  // field names and make the scan pass for the wrong reason.
  vaultPath = path.join(workspace, "Meridian-Private-Notes");
  rootPath = path.join(workspace, "os-cache");
  vaultIdentity = deriveVaultCacheIdentity({
    platform: process.platform,
    canonicalVaultPath: vaultPath,
  });
});

afterEach(() => {
  rmSync(workspace, { recursive: true, force: true });
});

interface RecordedCall {
  readonly op: string;
  readonly path: string;
}

/**
 * Delegating facade. It records every call so the fsync ordering can be
 * asserted as a real subsequence, and it can fail a chosen call so a crash
 * between the staged write and its rename is reproducible rather than imagined.
 */
class RecordingFileSystem implements CacheFileSystem {
  readonly calls: RecordedCall[] = [];
  failure: ((call: RecordedCall) => boolean) | null = null;

  constructor(private readonly inner: CacheFileSystem = nodeCacheFileSystem()) {}

  get log(): string[] {
    return this.calls.map((call) => `${call.op}:${call.path}`);
  }

  private record(op: string, target: string): void {
    const call = { op, path: target };
    this.calls.push(call);
    if (this.failure?.(call)) throw new Error(`injected failure at ${op}:${target}`);
  }

  async mkdir(target: string, options: { recursive: true; mode: number }): Promise<void> {
    this.record("mkdir", target);
    await this.inner.mkdir(target, options);
  }

  async chmod(target: string, mode: number): Promise<void> {
    this.record("chmod", target);
    await this.inner.chmod(target, mode);
  }

  lstat(target: string): Promise<CacheFileStats> {
    this.record("lstat", target);
    return this.inner.lstat(target);
  }

  realpath(target: string): Promise<string> {
    this.record("realpath", target);
    return this.inner.realpath(target);
  }

  readdir(target: string): Promise<string[]> {
    this.record("readdir", target);
    return this.inner.readdir(target);
  }

  async open(target: string, flags: string, mode?: number): Promise<CacheFileHandle> {
    this.record(`open(${flags})`, target);
    const handle = await this.inner.open(target, flags, mode);
    const record = (op: string) => this.record(op, target);
    return {
      write: async (bytes) => {
        record("write");
        await handle.write(bytes);
      },
      readInto: (buffer) => handle.readInto(buffer),
      chmod: (fileMode) => handle.chmod(fileMode),
      sync: async () => {
        record("sync");
        await handle.sync();
      },
      close: () => handle.close(),
    };
  }

  async rename(from: string, to: string): Promise<void> {
    this.record("rename", to);
    await this.inner.rename(from, to);
  }

  async unlink(target: string): Promise<void> {
    this.record("unlink", target);
    await this.inner.unlink(target);
  }

  async rm(target: string, options: { recursive: true; force: true }): Promise<void> {
    this.record("rm", target);
    await this.inner.rm(target, options);
  }
}

function envelope(overrides: Partial<CacheIdentityEnvelope> = {}): CacheIdentityEnvelope {
  return {
    protocol_version: WORKER_PROTOCOL_VERSION,
    cache_schema_version: 1,
    chunking_version: 1,
    sqlite_version: "3.53.0",
    sqlite_wasm_sha256: "b".repeat(64),
    rust_wasm_sha256: "c".repeat(64),
    plugin_id: "kwiry-search",
    plugin_version: "0.1.0",
    cache_identity: vaultIdentity,
    source_policy_hash: "d".repeat(64),
    ...overrides,
  };
}

function write(generationId: string, bytes: Uint8Array): CacheWrite {
  return {
    generationId,
    byteLength: bytes.byteLength,
    sha256: createHash("sha256").update(bytes).digest("hex"),
    identity: envelope(),
    bytes,
  };
}

function image(seed: number, size = 64): Uint8Array {
  return new Uint8Array(size).fill(seed);
}

async function openStore(overrides: {
  fs?: CacheFileSystem;
  maxBlobBytes?: number;
  now?: () => number;
  rootOverride?: string;
} = {}): Promise<CacheStorePort> {
  const availability = await openLocalCacheStore({
    canonicalVaultPath: vaultPath,
    vaultConfigDirName: ".obsidian",
    rootOverride: overrides.rootOverride ?? rootPath,
    fs: overrides.fs,
    maxBlobBytes: overrides.maxBlobBytes,
    now: overrides.now,
  });
  if (availability.kind !== "available") {
    throw new Error(`store unavailable: ${availability.reason}`);
  }
  return availability.store;
}

function vaultDirectory(): string {
  return path.join(rootPath, "vaults", vaultIdentity);
}

function generationsDirectory(): string {
  return path.join(vaultDirectory(), "generations");
}

function listGenerations(): string[] {
  return readdirSync(generationsDirectory()).sort();
}

function listQuarantine(): string[] {
  try {
    return readdirSync(path.join(vaultDirectory(), "quarantine")).sort();
  } catch {
    return [];
  }
}

function readPointer(): Record<string, unknown> {
  return JSON.parse(readFileSync(path.join(vaultDirectory(), "current.json"), "utf8"));
}

function writePointer(value: unknown): void {
  writeFileSync(path.join(vaultDirectory(), "current.json"), JSON.stringify(value));
}

function walkFiles(root: string): string[] {
  const found: string[] = [];
  const visit = (target: string) => {
    for (const entry of readdirSync(target, { withFileTypes: true })) {
      const child = path.join(target, entry.name);
      if (entry.isDirectory()) visit(child);
      else found.push(child);
    }
  };
  visit(root);
  return found;
}

describe("LocalCacheStore availability", () => {
  it("refuses a root inside the vault or inside its configuration directory", async () => {
    for (const inside of [
      path.join(vaultPath, "cache"),
      path.join(vaultPath, ".obsidian", "kwiry-cache"),
      vaultPath,
    ]) {
      await expect(openLocalCacheStore({
        canonicalVaultPath: vaultPath,
        vaultConfigDirName: ".obsidian",
        rootOverride: inside,
      })).resolves.toEqual({ kind: "unavailable", reason: "root_inside_vault" });
    }
    // Refusing must not have created anything: there is no branch that falls
    // back to vault-relative storage.
    expect(() => statSync(vaultPath)).toThrow();
  });

  it("honours a non-default configuration directory name", async () => {
    await expect(openLocalCacheStore({
      canonicalVaultPath: vaultPath,
      vaultConfigDirName: "my-config",
      rootOverride: path.join(vaultPath, "my-config", "cache"),
    })).resolves.toEqual({ kind: "unavailable", reason: "root_inside_vault" });
  });

  // Finding: the override skipped `resolveCacheRoot` and with it the UNC
  // refusal, while the Linux mount screen still ran against it — an asymmetric
  // bypass on the one input a caller fully controls.
  it("refuses a UNC root override exactly as it refuses a resolved UNC root", async () => {
    await expect(openLocalCacheStore({
      canonicalVaultPath: "C:\\Users\\u\\Vault",
      vaultConfigDirName: ".obsidian",
      rootInputs: { platform: "win32", env: {}, homedir: () => "C:\\Users\\u" },
      rootOverride: "\\\\server\\share\\kwiry",
    })).resolves.toEqual({ kind: "unavailable", reason: "root_not_machine_local" });

    // Forward slashes are the same path to Windows.
    await expect(openLocalCacheStore({
      canonicalVaultPath: "C:\\Users\\u\\Vault",
      vaultConfigDirName: ".obsidian",
      rootInputs: { platform: "win32", env: {}, homedir: () => "C:\\Users\\u" },
      rootOverride: "//server/share/kwiry",
    })).resolves.toEqual({ kind: "unavailable", reason: "root_not_machine_local" });
  });

  // The win32 containment refusal, driven on whatever platform runs the suite.
  // It folded for the target platform and then compared with the AMBIENT
  // `path.relative`, so on a Linux runner every backslash pair answered "not
  // contained" and this refusal could never fire.
  it("refuses a win32 root inside the vault when running on any platform", async () => {
    const rootInputs = {
      platform: "win32" as NodeJS.Platform,
      env: {},
      homedir: () => "C:\\Users\\u",
    };
    for (const inside of [
      "C:\\Users\\u\\Vault\\cache",
      "C:\\Users\\u\\Vault\\.obsidian\\kwiry-cache",
      "C:\\Users\\u\\Vault",
    ]) {
      await expect(openLocalCacheStore({
        canonicalVaultPath: "C:\\Users\\u\\Vault",
        vaultConfigDirName: ".obsidian",
        rootInputs,
        rootOverride: inside,
      })).resolves.toEqual({ kind: "unavailable", reason: "root_inside_vault" });
    }

    // The prefix trap must still not be a containment: a sibling directory
    // whose name merely starts with the vault's is not inside it. This case
    // reaching the probe rather than the refusal is what proves the assertions
    // above are not passing for the trivial reason that everything is refused.
    await expect(openLocalCacheStore({
      canonicalVaultPath: "C:\\Users\\u\\Vault",
      vaultConfigDirName: ".obsidian",
      rootInputs,
      rootOverride: "C:\\Users\\u\\VaultBackup\\cache",
    })).resolves.not.toEqual({ kind: "unavailable", reason: "root_inside_vault" });
  });

  it("refuses a vault that lives inside the cache root", async () => {
    await expect(openLocalCacheStore({
      canonicalVaultPath: path.join(rootPath, "nested-vault"),
      vaultConfigDirName: ".obsidian",
      rootOverride: rootPath,
    })).resolves.toEqual({ kind: "unavailable", reason: "root_inside_vault" });
  });

  it.skipIf(process.platform === "win32")(
    "refuses a cache root whose existing symlink ancestor resolves into the vault before mkdir",
    async () => {
      mkdirSync(vaultPath, { recursive: true });
      const linkedAncestor = path.join(workspace, "linked-cache-home");
      symlinkSync(vaultPath, linkedAncestor, "dir");
      const requestedRoot = path.join(linkedAncestor, "kwiry-search");

      await expect(openLocalCacheStore({
        canonicalVaultPath: vaultPath,
        vaultConfigDirName: ".obsidian",
        rootOverride: requestedRoot,
      })).resolves.toEqual({ kind: "unavailable", reason: "root_inside_vault" });
      expect(existsSync(path.join(vaultPath, "kwiry-search"))).toBe(false);
    },
  );

  it.skipIf(process.platform !== "win32")(
    "refuses a cache root whose existing junction ancestor resolves into the vault before mkdir",
    async () => {
      mkdirSync(vaultPath, { recursive: true });
      const junction = path.join(workspace, "linked-cache-home");
      symlinkSync(vaultPath, junction, "junction");
      const requestedRoot = path.join(junction, "kwiry-search");

      await expect(openLocalCacheStore({
        canonicalVaultPath: vaultPath,
        vaultConfigDirName: ".obsidian",
        rootOverride: requestedRoot,
      })).resolves.toEqual({ kind: "unavailable", reason: "root_inside_vault" });
      expect(existsSync(path.join(vaultPath, "kwiry-search"))).toBe(false);
    },
  );

  it("reports a probe failure truthfully instead of writing anywhere else", async () => {
    const io = new RecordingFileSystem();
    io.failure = (call) => call.op === "rename" && call.path.includes(".store-probe-");
    await expect(openLocalCacheStore({
      canonicalVaultPath: vaultPath,
      vaultConfigDirName: ".obsidian",
      rootOverride: rootPath,
      fs: io,
    })).resolves.toEqual({ kind: "unavailable", reason: "root_probe_failed" });
    // The probe cleans up after itself.
    expect(readdirSync(rootPath)).toEqual([]);
  });

  it("reports an unusable root as not writable", async () => {
    const io = new RecordingFileSystem();
    io.failure = (call) => call.op === "mkdir" && call.path === rootPath;
    await expect(openLocalCacheStore({
      canonicalVaultPath: vaultPath,
      vaultConfigDirName: ".obsidian",
      rootOverride: rootPath,
      fs: io,
    })).resolves.toEqual({ kind: "unavailable", reason: "root_not_writable" });
  });

  // A DataAdapter without getBasePath is exactly what a non-desktop host
  // provides, and it must report unavailable rather than guess a location.
  it("reports an adapter without a base path as an unavailable vault location", async () => {
    await expect(openVaultCacheStore({
      adapter: {},
      vaultConfigDirName: ".obsidian",
    })).resolves.toEqual({ kind: "unavailable", reason: "vault_location_unavailable" });

    await expect(openVaultCacheStore({
      adapter: { getBasePath: () => "relative/vault" },
      vaultConfigDirName: ".obsidian",
    })).resolves.toEqual({ kind: "unavailable", reason: "vault_location_unavailable" });
  });

  // The composition entry the host wires must offer no way to name a root that
  // never went through `resolveCacheRoot`. The assertion is the compile error:
  // if `rootOverride` were ever restored to this surface, `@ts-expect-error`
  // would itself become an error and the typecheck would fail.
  it("exposes no root override on the composition entry", async () => {
    await expect(openVaultCacheStore({
      adapter: {},
      vaultConfigDirName: ".obsidian",
      // @ts-expect-error rootOverride is deliberately absent from this surface.
      rootOverride: rootPath,
    })).resolves.toEqual({ kind: "unavailable", reason: "vault_location_unavailable" });
  });
});

describe("LocalCacheStore", () => {
  it("round-trips an image and its envelope byte for byte", async () => {
    const store = await openStore();
    const bytes = image(0x41, 512);
    const record = await store.put(write("g1", bytes));

    expect(record).toEqual({
      generationId: "g1",
      byteLength: 512,
      sha256: createHash("sha256").update(bytes).digest("hex"),
      identity: envelope(),
    });

    const loaded = await store.load();
    expect(loaded.kind).toBe("hit");
    if (loaded.kind !== "hit") return;
    expect(Array.from(loaded.bytes)).toEqual(Array.from(bytes));
    expect(loaded.record).toEqual(record);
    // A hit says out loud that its digest was NOT checked. The store compares
    // length only, and stating that in a comment rather than in the value let a
    // consumer treat the bytes as verified by omission.
    expect(loaded.digestVerified).toBe(false);
    await store.dispose();
  });

  // The gap the flag exists to name: length is not integrity. A same-length
  // substitution passes every check the store makes.
  it("returns a length-matching but tampered image as an explicitly unverified hit", async () => {
    const store = await openStore();
    const bytes = image(0xc1, 256);
    const record = await store.put(write("g1", bytes));
    writeFileSync(path.join(generationsDirectory(), "g1.kwc"), Buffer.alloc(256, 0xc2));

    const loaded = await store.load();
    expect(loaded.kind).toBe("hit");
    if (loaded.kind !== "hit") return;
    expect(loaded.digestVerified).toBe(false);
    // The recorded digest still describes the ORIGINAL bytes, so a consumer
    // that verifies catches this — and one that does not cannot say it was
    // never told.
    expect(loaded.record.sha256).toBe(record.sha256);
    expect(createHash("sha256").update(loaded.bytes).digest("hex")).not.toBe(record.sha256);
    await store.dispose();
  });

  it("shares exactly one blob ceiling with the Worker protocol", () => {
    expect(MAX_CACHE_BLOB_BYTES).toBe(MAX_EXPORT_BLOB_BYTES);
  });

  // Checked by tsc, not at runtime, because the failure it guards against is
  // invisible at runtime: widening the field to `?: boolean` still lets the
  // store supply `false` and still lets this suite read it, while quietly
  // permitting a restore consumer to drop the acknowledgement altogether. That
  // is the entire failure the field exists to prevent, so "required" and
  // "literally false" are asserted as types.
  it("keeps the unverified-digest acknowledgement required and literally false", () => {
    type HitOf<T> = Extract<T, { readonly kind: "hit" }>;
    type IsExactly<A, B> = [A] extends [B] ? ([B] extends [A] ? true : false) : false;
    const requiredLiteralFalse: IsExactly<HitOf<CacheLoad>["digestVerified"], false> = true;
    expect(requiredLiteralFalse).toBe(true);
  });

  it("stores nothing inside the vault and leaks no path into any stored byte", async () => {
    const store = await openStore();
    await store.put(write("g1", image(0x42)));

    expect(() => statSync(vaultPath)).toThrow();
    const files = walkFiles(rootPath);
    expect(files.length).toBeGreaterThan(0);
    for (const file of files) {
      const contents = readFileSync(file, "latin1");
      for (const secret of [vaultPath, path.basename(vaultPath), workspace]) {
        expect(contents).not.toContain(secret);
      }
      // The path must not appear in the file names either.
      expect(file.slice(rootPath.length)).not.toContain(path.basename(vaultPath));
    }
    await store.dispose();
  });

  it.each(["vault identity", "generations"] as const)(
    "rejects a symlink or junction at the owned %s directory without touching its target",
    async (component) => {
      mkdirSync(vaultPath, { recursive: true });
      const io = new RecordingFileSystem();
      const store = await openStore({ fs: io });
      const vaults = path.join(rootPath, "vaults");
      mkdirSync(vaults, { recursive: true });
      const identityDirectory = path.join(vaults, vaultIdentity);
      const attackedDirectory = component === "vault identity"
        ? identityDirectory
        : path.join(identityDirectory, "generations");
      if (component === "vault identity") {
        symlinkSync(vaultPath, identityDirectory, process.platform === "win32" ? "junction" : "dir");
      } else {
        mkdirSync(identityDirectory, { recursive: true });
        symlinkSync(
          vaultPath,
          attackedDirectory,
          process.platform === "win32" ? "junction" : "dir",
        );
      }

      await expect(store.put(write("g1", image(0x42)))).rejects.toMatchObject({
        code: "unsafe_path",
      });
      expect(io.calls.some((call) => call.op === "chmod" && call.path === attackedDirectory))
        .toBe(false);
      expect(io.calls.some((call) => call.op === "open(wx)"
        && call.path.startsWith(`${attackedDirectory}${path.sep}`))).toBe(false);
      expect(readdirSync(vaultPath)).toEqual([]);
      await store.dispose();
    },
  );

  it("rethrows pointer permission and I/O failures instead of reporting cache absence", async () => {
    const io = new RecordingFileSystem();
    const store = await openStore({ fs: io });
    for (const code of ["EACCES", "EIO"] as const) {
      io.failure = (call) => {
        if (call.op !== "lstat" || !call.path.endsWith("current.json")) return false;
        const error = new Error(`injected ${code}`) as NodeJS.ErrnoException;
        error.code = code;
        throw error;
      };
      await expect(store.load()).rejects.toMatchObject({ code });
    }
    io.failure = null;
    await store.dispose();
  });

  it("writes the image durably and commits only after the pointer rename", async () => {
    const io = new RecordingFileSystem();
    const store = await openStore({ fs: io });
    io.calls.length = 0;
    await store.put(write("g1", image(0x43)));

    const imagePath = path.join(generationsDirectory(), "g1.kwc");
    const pointerPath = path.join(vaultDirectory(), "current.json");
    const relevant = io.log.filter((entry) =>
      entry.includes("g1.kwc") || entry.includes("current.json"));
    const imageTemp = relevant.find((entry) => entry.startsWith("open(wx)"))!;
    const temporaryImage = imageTemp.slice("open(wx):".length);
    const pointerTemp = relevant
      .find((entry) => entry.startsWith("open(wx)") && entry.includes("current.json"))!
      .slice("open(wx):".length);

    const expected = [
      `open(wx):${temporaryImage}`,
      `write:${temporaryImage}`,
      `sync:${temporaryImage}`,
      `rename:${imagePath}`,
      `open(wx):${pointerTemp}`,
      `write:${pointerTemp}`,
      `sync:${pointerTemp}`,
      `rename:${pointerPath}`,
    ];
    let cursor = 0;
    for (const entry of io.log) {
      if (entry === expected[cursor]) cursor += 1;
    }
    expect(cursor).toBe(expected.length);

    // No rename may be reached without its own preceding sync.
    const imageRename = io.log.indexOf(`rename:${imagePath}`);
    const pointerRename = io.log.indexOf(`rename:${pointerPath}`);
    expect(io.log.indexOf(`sync:${temporaryImage}`)).toBeLessThan(imageRename);
    expect(io.log.indexOf(`sync:${pointerTemp}`)).toBeLessThan(pointerRename);
    expect(imageRename).toBeLessThan(pointerRename);
    await store.dispose();
  });

  // A crash anywhere before the pointer rename must leave the PREVIOUS
  // generation loadable; a crash after it must leave the new one loadable.
  it.each([
    ["the image rename", (call: RecordedCall) => call.op === "rename" && call.path.endsWith("g2.kwc")],
    ["the pointer write", (call: RecordedCall) => call.op === "write" && call.path.includes("current.json")],
    ["the pointer sync", (call: RecordedCall) => call.op === "sync" && call.path.includes("current.json")],
    [
      "the pointer rename",
      (call: RecordedCall) => call.op === "rename" && call.path.endsWith("current.json"),
    ],
  ])("survives a crash at %s", async (_name, predicate) => {
    const first = image(0x51, 256);
    const second = image(0x52, 256);

    const setup = await openStore();
    await setup.put(write("g1", first));
    await setup.dispose();

    const io = new RecordingFileSystem();
    io.failure = predicate;
    const crashing = await openStore({ fs: io });
    await expect(crashing.put(write("g2", second))).rejects.toBeInstanceOf(CacheStoreError);

    // A fresh store over the same directory still loads the previous
    // generation, byte for byte.
    const recovered = await openStore();
    const loaded = await recovered.load();
    expect(loaded.kind).toBe("hit");
    if (loaded.kind !== "hit") return;
    expect(loaded.record.generationId).toBe("g1");
    expect(Array.from(loaded.bytes)).toEqual(Array.from(first));

    // And the next successful write commits normally.
    await recovered.put(write("g2", second));
    const after = await recovered.load();
    expect(after.kind).toBe("hit");
    if (after.kind !== "hit") return;
    expect(after.record.generationId).toBe("g2");
    expect(Array.from(after.bytes)).toEqual(Array.from(second));
    await recovered.dispose();
  });

  // The counterpart to the case below: a directory sync the crash-safety
  // argument depends on — the one that makes the image rename durable before
  // the pointer names it — is required, and its failure fails the write.
  it.skipIf(process.platform === "win32")(
    "fails the write when the image directory sync fails",
    async () => {
      const setup = await openStore();
      await setup.put(write("g1", image(0x5a)));
      await setup.dispose();

      const io = new RecordingFileSystem();
      io.failure = (call) => call.op === "sync" && call.path === generationsDirectory();
      const store = await openStore({ fs: io });
      await expect(store.put(write("g2", image(0x5b)))).rejects.toMatchObject({
        code: "write_failed",
      });

      await expect(store.load()).resolves.toMatchObject({
        kind: "hit",
        record: { generationId: "g1" },
      });
      await store.dispose();
    },
  );

  // Directory fsync is durability hardening applied AFTER the commit point,
  // so losing it must not lose the generation.
  it("commits the new generation even when the parent directory sync fails", async () => {
    const setup = await openStore();
    await setup.put(write("g1", image(0x61)));
    await setup.dispose();

    const io = new RecordingFileSystem();
    let renamed = false;
    io.failure = (call) => {
      if (call.op === "rename" && call.path.endsWith("current.json")) renamed = true;
      return renamed && call.op === "sync" && call.path === vaultDirectory();
    };
    const store = await openStore({ fs: io });
    await expect(store.put(write("g2", image(0x62)))).resolves.toMatchObject({
      generationId: "g2",
    });

    const loaded = await store.load();
    expect(loaded).toMatchObject({ kind: "hit", record: { generationId: "g2" } });
    await store.dispose();
  });

  it("retains exactly the current and previous generations by recorded lineage", async () => {
    const store = await openStore();
    await store.put(write("g1", image(1)));
    expect(listGenerations()).toEqual(["g1.kwc"]);

    await store.put(write("g2", image(2)));
    expect(listGenerations()).toEqual(["g1.kwc", "g2.kwc"]);
    expect(readPointer().previousGenerationId).toBe("g1");

    await store.put(write("g3", image(3)));
    expect(listGenerations()).toEqual(["g2.kwc", "g3.kwc"]);

    // Retention follows the recorded lineage, not file timestamps.
    const stale = path.join(generationsDirectory(), "g2.kwc");
    const future = Date.now() / 1000 + 86_400;
    (await import("node:fs")).utimesSync(stale, future, future);
    await store.put(write("g4", image(4)));
    expect(listGenerations()).toEqual(["g3.kwc", "g4.kwc"]);

    // A stray temporary file from an interrupted write is swept away.
    writeFileSync(path.join(generationsDirectory(), "x.kwc.tmp-abc"), "junk");
    await store.put(write("g5", image(5)));
    expect(listGenerations()).toEqual(["g4.kwc", "g5.kwc"]);
    await store.dispose();
  });

  it("keeps the write successful when retention cannot delete an old image", async () => {
    const io = new RecordingFileSystem();
    const store = await openStore({ fs: io });
    await store.put(write("g1", image(1)));
    await store.put(write("g2", image(2)));

    io.failure = (call) => call.op === "unlink" && call.path.endsWith("g1.kwc");
    await expect(store.put(write("g3", image(3)))).resolves.toMatchObject({
      generationId: "g3",
    });
    const loaded = await store.load();
    expect(loaded).toMatchObject({ kind: "hit", record: { generationId: "g3" } });
    await store.dispose();
  });

  // Nothing is deleted on an identity mismatch: the data may belong to another
  // vault, and destroying it is strictly worse than a cold rebuild.
  it("refuses a pointer written under a different vault identity and keeps the image", async () => {
    const store = await openStore();
    await store.put(write("g1", image(0x71)));
    const pointer = readPointer();
    writePointer({ ...pointer, vaultIdentity: "f".repeat(64) });

    await expect(store.load()).resolves.toEqual({ kind: "miss", reason: "identity_mismatch" });
    expect(listGenerations()).toEqual(["g1.kwc"]);
    expect(readPointer().vaultIdentity).toBe("f".repeat(64));
    await store.dispose();
  });

  it("refuses a pointer whose envelope names a different vault identity", async () => {
    const store = await openStore();
    await store.put(write("g1", image(0x72)));
    const pointer = readPointer();
    writePointer({
      ...pointer,
      identity: { ...(pointer.identity as object), cache_identity: "e".repeat(64) },
    });
    await expect(store.load()).resolves.toEqual({ kind: "miss", reason: "identity_mismatch" });
    await store.dispose();
  });

  it.each([
    ["truncated JSON", () => writeFileSync(path.join(vaultDirectory(), "current.json"), "{\"a\":")],
    ["a value of the wrong shape", () => writePointer([1, 2, 3])],
    ["an unknown extra key", () => writePointer({ ...readPointer(), extra: true })],
    ["an unsupported pointer version", () => writePointer({ ...readPointer(), pointerVersion: 2 })],
    ["a traversal file name", () => writePointer({ ...readPointer(), file: "../../escape.kwc" })],
    [
      "a file name that is not the derived one",
      () => writePointer({ ...readPointer(), file: "generations/other.kwc" }),
    ],
    [
      "a malformed generation identifier",
      () => writePointer({ ...readPointer(), generationId: "../g1" }),
    ],
    ["a malformed digest", () => writePointer({ ...readPointer(), sha256: "nope" })],
    [
      "a malformed identity envelope",
      () => writePointer({
        ...readPointer(),
        identity: { ...(readPointer().identity as object), rust_wasm_sha256: "c".repeat(63) },
      }),
    ],
    [
      "an identity envelope missing a field",
      () => {
        const { chunking_version: _dropped, ...rest } = readPointer().identity as Record<
          string,
          unknown
        >;
        writePointer({ ...readPointer(), identity: rest });
      },
    ],
    [
      "an oversized pointer",
      () => writeFileSync(path.join(vaultDirectory(), "current.json"), "x".repeat(9 * 1024)),
    ],
  ])("treats %s as a corrupt pointer and stays usable", async (_name, corrupt) => {
    const store = await openStore();
    await store.put(write("g1", image(0x81)));
    corrupt();

    await expect(store.load()).resolves.toEqual({ kind: "miss", reason: "pointer_corrupt" });
    // The pointer is removed, so the next load is a clean absence.
    await expect(store.load()).resolves.toEqual({ kind: "miss", reason: "absent" });

    // And the store still works.
    await store.put(write("g2", image(0x82)));
    await expect(store.load()).resolves.toMatchObject({
      kind: "hit",
      record: { generationId: "g2" },
    });
    await store.dispose();
  });

  it("reports a pointer naming an absent image without quarantining anything", async () => {
    const store = await openStore();
    await store.put(write("g1", image(0x91)));
    rmSync(path.join(generationsDirectory(), "g1.kwc"));

    await expect(store.load()).resolves.toEqual({ kind: "miss", reason: "image_absent" });
    await store.dispose();
  });

  it("quarantines an image whose length disagrees with its pointer", async () => {
    const store = await openStore();
    await store.put(write("g1", image(0xa1, 128)));
    writeFileSync(path.join(generationsDirectory(), "g1.kwc"), Buffer.alloc(64));

    await expect(store.load()).resolves.toEqual({
      kind: "miss",
      reason: "image_length_mismatch",
    });
    expect(listGenerations()).toEqual([]);
    expect(listQuarantine()).toHaveLength(1);
    // The pointer went with it, so the next load is a clean absence.
    await expect(store.load()).resolves.toEqual({ kind: "miss", reason: "absent" });

    // Quarantine is bounded: the next write empties it.
    await store.put(write("g2", image(0xa2)));
    expect(listQuarantine()).toEqual([]);
    await store.dispose();
  });

  it.each([
    ["", "invalid_generation_id"],
    ["..", "invalid_generation_id"],
    ["a/b", "invalid_generation_id"],
    ["a\\b", "invalid_generation_id"],
    [".hidden", "invalid_generation_id"],
    ["g".repeat(129), "invalid_generation_id"],
  ])("refuses the generation identifier %j before touching a disk", async (generationId, code) => {
    const io = new RecordingFileSystem();
    const store = await openStore({ fs: io });
    io.calls.length = 0;

    await expect(store.put(write(generationId, image(1))))
      .rejects.toMatchObject({ name: "CacheStoreError", code });
    expect(io.calls).toEqual([]);
    await store.dispose();
  });

  it.each([
    ["an empty image", () => ({ ...write("g1", image(1)), bytes: new Uint8Array(0), byteLength: 0 })],
    ["an oversized image", () => write("g1", image(1, 4_096))],
    [
      "a length that disagrees with the buffer",
      () => ({ ...write("g1", image(1)), byteLength: 63 }),
    ],
    ["a malformed digest", () => ({ ...write("g1", image(1)), sha256: "not-a-digest" })],
  ])("refuses %s before touching a disk", async (_name, build) => {
    const io = new RecordingFileSystem();
    const store = await openStore({ fs: io, maxBlobBytes: 1_024 });
    io.calls.length = 0;

    await expect(store.put(build())).rejects.toMatchObject({
      name: "CacheStoreError",
      code: "invalid_blob",
    });
    expect(io.calls).toEqual([]);
    await store.dispose();
  });

  it.each([
    ["a short digest", envelope({ rust_wasm_sha256: "c".repeat(63) })],
    ["a malformed source policy hash", envelope({ source_policy_hash: "policy" })],
    ["a non-integer version", { ...envelope(), protocol_version: 1.5 }],
    ["a foreign vault identity", envelope({ cache_identity: "9".repeat(64) })],
    ["an unknown key", { ...envelope(), unexpected: true }],
    ["a missing key", (() => {
      const { chunking_version: _dropped, ...rest } = envelope();
      return rest;
    })()],
  ])("refuses an identity envelope with %s before touching a disk", async (_name, identity) => {
    const io = new RecordingFileSystem();
    const store = await openStore({ fs: io });
    io.calls.length = 0;

    await expect(store.put({
      ...write("g1", image(1)),
      identity: identity as CacheIdentityEnvelope,
    })).rejects.toMatchObject({ name: "CacheStoreError", code: "invalid_identity" });
    expect(io.calls).toEqual([]);
    await store.dispose();
  });

  it("admits only one writer at a time and takes over a stale lock", async () => {
    const holder = await openStore();
    await holder.put(write("g1", image(1)));
    // Simulate a writer that died holding the lock.
    writeFileSync(path.join(vaultDirectory(), "writer.lock"), "{}");

    const blocked = await openStore();
    await expect(blocked.put(write("g2", image(2)))).rejects.toMatchObject({
      name: "CacheStoreError",
      code: "locked",
    });

    // A lock older than the staleness window is taken over.
    const takingOver = await openStore({ now: () => Date.now() + 60 * 60 * 1000 });
    await expect(takingOver.put(write("g2", image(2)))).resolves.toMatchObject({
      generationId: "g2",
    });
    await holder.dispose();
    await blocked.dispose();
    await takingOver.dispose();
  });

  // Disposal used to unlink the lock unconditionally. Every lock this instance
  // takes is released in `withWriterLock`'s finally and disposal is serialized
  // behind the same queue, so the only lock that unlink could ever reach
  // belonged to a DIFFERENT process — which is how a second Obsidian window on
  // the same vault deletes the image the first one is still about to name.
  it("does not remove a writer lock it does not hold", async () => {
    const holder = await openStore();
    await holder.put(write("g1", image(1)));
    const lockPath = path.join(vaultDirectory(), "writer.lock");
    // A live lock belonging to another process.
    writeFileSync(lockPath, JSON.stringify({ pid: process.pid + 1, startedAtMs: Date.now() }));

    const other = await openStore();
    await other.dispose();
    expect(existsSync(lockPath)).toBe(true);

    // And it is still doing its job: exclusion survives the unrelated disposal.
    const blocked = await openStore();
    await expect(blocked.put(write("g2", image(2)))).rejects.toMatchObject({ code: "locked" });
    await blocked.dispose();
    await holder.dispose();
    expect(existsSync(lockPath)).toBe(true);
  });

  // `load` is not a writer, but quarantine deletes the pointer and renames an
  // image — the exact names a writer in another process may be committing.
  it("leaves a corrupt image in place while another writer holds the lock", async () => {
    const store = await openStore();
    await store.put(write("g1", image(0xa1, 128)));
    writeFileSync(path.join(generationsDirectory(), "g1.kwc"), Buffer.alloc(64));
    writeFileSync(path.join(vaultDirectory(), "writer.lock"), "{}");

    // The miss is still reported truthfully; only the destructive half is held.
    await expect(store.load()).resolves.toEqual({
      kind: "miss",
      reason: "image_length_mismatch",
    });
    expect(listGenerations()).toEqual(["g1.kwc"]);
    expect(listQuarantine()).toEqual([]);
    expect(readPointer().generationId).toBe("g1");
    await store.dispose();
  });

  // The narrower race: the lock is free by the time quarantine asks for it, but
  // a writer committed in the window between the load's observations and that
  // acquisition. The observations now describe a state that no longer exists.
  it("abandons a quarantine when a writer committed while the load was deciding", async () => {
    const io = new RecordingFileSystem();
    const store = await openStore({ fs: io });
    await store.put(write("g1", image(0xb1, 128)));
    const pointer = readPointer();
    writeFileSync(path.join(generationsDirectory(), "g1.kwc"), Buffer.alloc(64));

    io.failure = (call) => {
      if (call.op === "open(wx)" && call.path.endsWith("writer.lock")) {
        writeFileSync(path.join(generationsDirectory(), "g2.kwc"), Buffer.alloc(64));
        writePointer({
          ...pointer,
          generationId: "g2",
          previousGenerationId: "g1",
          file: "generations/g2.kwc",
        });
      }
      return false;
    };

    await expect(store.load()).resolves.toEqual({
      kind: "miss",
      reason: "image_length_mismatch",
    });
    // The freshly committed generation and its pointer survive untouched.
    expect(readPointer().generationId).toBe("g2");
    expect(listGenerations()).toEqual(["g1.kwc", "g2.kwc"]);
    expect(listQuarantine()).toEqual([]);
    await store.dispose();
  });

  it("discards everything it owns, pointer first", async () => {
    const store = await openStore();
    await store.put(write("g1", image(1)));
    await store.discard("requested");

    await expect(store.load()).resolves.toEqual({ kind: "miss", reason: "absent" });
    expect(readdirSync(vaultDirectory()).sort()).toEqual([]);
    await store.dispose();
  });

  it("reports a definitive discard failure instead of claiming removal", async () => {
    const io = new RecordingFileSystem();
    const store = await openStore({ fs: io });
    await store.put(write("g1", image(1)));
    io.failure = (call) => call.op === "unlink" && call.path.endsWith("current.json");

    await expect(store.discard("corrupt")).rejects.toMatchObject({ code: "discard_failed" });
    expect(existsSync(path.join(vaultDirectory(), "current.json"))).toBe(true);
    await store.dispose();
  });

  it("refuses further work once disposed", async () => {
    const store = await openStore();
    await store.put(write("g1", image(1)));
    await store.dispose();

    await expect(store.put(write("g2", image(2)))).rejects.toMatchObject({ code: "disposed" });
    await expect(store.load()).rejects.toMatchObject({ code: "disposed" });
    await expect(store.dispose()).resolves.toBeUndefined();
  });

  it.skipIf(process.platform === "win32")(
    // Skipped on win32: POSIX permission bits are not the access-control
    // mechanism there, so asserting them would assert nothing.
    "applies owner-only permissions regardless of the umask",
    async () => {
      const previousMask = process.umask(0o022);
      try {
        const store = await openStore();
        await store.put(write("g1", image(1)));

        for (const directory of [rootPath, vaultDirectory(), generationsDirectory()]) {
          expect(statSync(directory).mode & 0o777).toBe(0o700);
        }
        expect(statSync(path.join(vaultDirectory(), "current.json")).mode & 0o777).toBe(0o600);
        expect(statSync(path.join(generationsDirectory(), "g1.kwc")).mode & 0o777).toBe(0o600);
        await store.dispose();
      } finally {
        process.umask(previousMask);
      }
    },
  );
});
