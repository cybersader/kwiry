// SPDX-FileCopyrightText: 2026 cybersader
// SPDX-License-Identifier: GPL-3.0-only

import { describe, expect, it } from "vitest";

import {
  foldPathForComparison,
  foldPathForIdentity,
  isNetworkMountedPath,
  isPathWithin,
  resolveCacheRoot,
  type CacheRootInputs,
} from "../src/cache/cache-root";
import { deriveVaultCacheIdentity } from "../src/cache/vault-identity";

// Everything the resolver reads is injected, so no case here ever touches a
// real LOCALAPPDATA, XDG_CACHE_HOME, ~/Library/Caches or ~/.cache.
function inputs(
  platform: NodeJS.Platform,
  env: Record<string, string | undefined> = {},
  homedir: () => string = () => "/home/user",
): CacheRootInputs {
  return { platform, env, homedir };
}

describe("resolveCacheRoot", () => {
  it("resolves the machine-local OS cache root per platform", () => {
    expect(resolveCacheRoot(inputs("win32", { LOCALAPPDATA: "C:\\Users\\u\\AppData\\Local" })))
      .toEqual({ kind: "root", path: "C:\\Users\\u\\AppData\\Local\\kwiry\\obsidian-cache" });

    // XDG_CACHE_HOME is deliberately ignored on macOS.
    expect(resolveCacheRoot(inputs("darwin", { XDG_CACHE_HOME: "/tmp/xdg" }, () => "/Users/u")))
      .toEqual({ kind: "root", path: "/Users/u/Library/Caches/kwiry/obsidian-cache" });

    expect(resolveCacheRoot(inputs("linux", { XDG_CACHE_HOME: "/var/tmp/xdg" })))
      .toEqual({ kind: "root", path: "/var/tmp/xdg/kwiry/obsidian-cache" });

    expect(resolveCacheRoot(inputs("linux", {}, () => "/home/u")))
      .toEqual({ kind: "root", path: "/home/u/.cache/kwiry/obsidian-cache" });

    expect(resolveCacheRoot(inputs("freebsd")))
      .toEqual({ kind: "unavailable", reason: "unsupported_platform" });
  });

  // Every refusal is returned with its own reason, and none of them silently
  // substitutes a different root. This is the fail-able capability proof.
  it.each([
    [
      "a roaming-only Windows profile",
      inputs("win32", { APPDATA: "C:\\Users\\u\\AppData\\Roaming" }),
      "no_machine_local_root",
    ],
    ["an empty LOCALAPPDATA", inputs("win32", { LOCALAPPDATA: "   " }), "no_machine_local_root"],
    [
      "a relative LOCALAPPDATA",
      inputs("win32", { LOCALAPPDATA: "AppData\\Local" }),
      "root_not_absolute",
    ],
    [
      "a UNC LOCALAPPDATA",
      inputs("win32", { LOCALAPPDATA: "\\\\fileserver\\profiles\\u" }),
      "root_not_machine_local",
    ],
    [
      "a relative XDG_CACHE_HOME",
      inputs("linux", { XDG_CACHE_HOME: "relative/cache" }),
      "root_not_absolute",
    ],
    [
      "a relative home directory",
      inputs("linux", {}, () => "relative/home"),
      "root_not_absolute",
    ],
    [
      "a home directory that cannot be resolved",
      inputs("linux", {}, () => {
        throw new Error("no home");
      }),
      "no_machine_local_root",
    ],
  ])("refuses %s", (_name, given, reason) => {
    expect(resolveCacheRoot(given)).toEqual({ kind: "unavailable", reason });
  });

  // A refusal must never degrade into ~/.cache or into the roaming profile.
  it("never substitutes another root when the configured one is refused", () => {
    const roaming = resolveCacheRoot(inputs(
      "win32",
      { APPDATA: "C:\\Users\\u\\AppData\\Roaming" },
      () => "C:\\Users\\u",
    ));
    expect(roaming).toMatchObject({ kind: "unavailable" });
    expect(JSON.stringify(roaming)).not.toContain("Roaming");

    const relativeXdg = resolveCacheRoot(inputs("linux", { XDG_CACHE_HOME: "cache" }));
    expect(relativeXdg).toMatchObject({ kind: "unavailable" });
    expect(JSON.stringify(relativeXdg)).not.toContain(".cache");
  });
});

describe("isPathWithin", () => {
  it("compares whole segments rather than string prefixes", () => {
    expect(isPathWithin("/home/u/vault", "/home/u/vault/notes", "linux")).toBe(true);
    expect(isPathWithin("/home/u/vault", "/home/u/vault", "linux")).toBe(true);
    // The prefix trap: "/home/u/vaultsomething" is not inside "/home/u/vault".
    expect(isPathWithin("/home/u/vault", "/home/u/vaultsomething", "linux")).toBe(false);
    expect(isPathWithin("/home/u/vault", "/home/u", "linux")).toBe(false);
  });

  // Containment must follow the TARGET platform, not the running one. A
  // backslash-separated win32 pair compared with POSIX arithmetic answers
  // `false` for everything, which is how a refusal keeps passing review while
  // never firing.
  it("does win32 path arithmetic for win32 regardless of the running platform", () => {
    expect(isPathWithin("c:\\vaults\\notes", "c:\\vaults\\notes\\cache", "win32")).toBe(true);
    expect(isPathWithin("c:\\vaults\\notes", "c:\\vaults\\notesx", "win32")).toBe(false);
    expect(isPathWithin("c:\\vaults\\notes", "d:\\vaults\\notes\\cache", "win32")).toBe(false);
  });
});

describe("isNetworkMountedPath", () => {
  const mountInfo = [
    "23 1 0:20 / / rw,relatime - ext4 /dev/sda1 rw",
    "44 23 0:41 / /mnt/share rw,relatime - cifs //server/share rw",
    "45 23 0:42 / /mnt/share/local rw,relatime - ext4 /dev/sdb1 rw",
  ].join("\n");

  it("screens a network-backed root by its deepest covering mount", () => {
    expect(isNetworkMountedPath("/mnt/share/kwiry", mountInfo)).toBe(true);
    // The deepest mount wins: a local filesystem mounted under a network one
    // is still local.
    expect(isNetworkMountedPath("/mnt/share/local/kwiry", mountInfo)).toBe(false);
    expect(isNetworkMountedPath("/home/u/.cache/kwiry", mountInfo)).toBe(false);
  });
});

describe("deriveVaultCacheIdentity", () => {
  it("is deterministic, path-specific, and 64 lowercase hex characters", () => {
    const first = deriveVaultCacheIdentity({
      platform: "linux",
      canonicalVaultPath: "/home/u/vault",
    });
    const again = deriveVaultCacheIdentity({
      platform: "linux",
      canonicalVaultPath: "/home/u/vault",
    });
    const other = deriveVaultCacheIdentity({
      platform: "linux",
      canonicalVaultPath: "/home/u/other",
    });

    expect(first).toMatch(/^[0-9a-f]{64}$/u);
    expect(again).toBe(first);
    expect(other).not.toBe(first);
    // The digest is never truncated: a shortened identity would invite a
    // collision argument a cache pointer should not have to answer.
    expect(first).toHaveLength(64);
  });

  it("folds case for identity on win32 only", () => {
    const windowsLower = deriveVaultCacheIdentity({
      platform: "win32",
      canonicalVaultPath: "C:\\Users\\u\\Vault",
    });
    const windowsUpper = deriveVaultCacheIdentity({
      platform: "win32",
      canonicalVaultPath: "C:\\USERS\\U\\VAULT",
    });
    expect(windowsUpper).toBe(windowsLower);

    // Linux paths are case-significant, so two spellings are two vaults.
    expect(deriveVaultCacheIdentity({
      platform: "linux",
      canonicalVaultPath: "/home/u/Vault",
    })).not.toBe(deriveVaultCacheIdentity({
      platform: "linux",
      canonicalVaultPath: "/home/u/vault",
    }));
  });

  // The safety-critical half, kept as its own case because it is the one that
  // is tempting to "simplify" back into a bug. APFS can be formatted
  // case-sensitive, so /Vaults/Notes and /Vaults/notes may be two DISTINCT
  // vaults. Folding them to one identity gives them one cache directory and one
  // pointer, and the identity-mismatch refusal cannot detect it — the two
  // identities would compare equal — so vault B's image would be served into
  // vault A as a hit, disclosing B's paths. Not folding costs at worst a
  // duplicate directory; folding costs a shared one.
  it("does not fold case for identity on darwin, where the volume may be case-sensitive", () => {
    expect(deriveVaultCacheIdentity({
      platform: "darwin",
      canonicalVaultPath: "/Users/u/Vault",
    })).not.toBe(deriveVaultCacheIdentity({
      platform: "darwin",
      canonicalVaultPath: "/Users/u/vault",
    }));
  });

  // Containment comparison KEEPS the darwin fold: refusing a root that reaches
  // the vault through a differently-cased spelling is the conservative answer
  // there, and the asymmetry between the two folds is deliberate.
  it("still folds darwin case for containment comparison", () => {
    expect(foldPathForComparison("darwin", "/Users/u/Vault"))
      .toBe(foldPathForComparison("darwin", "/users/U/vault"));
    expect(foldPathForIdentity("darwin", "/Users/u/Vault"))
      .not.toBe(foldPathForIdentity("darwin", "/users/U/vault"));
  });

  it("ignores a trailing separator and separates its inputs unambiguously", () => {
    expect(deriveVaultCacheIdentity({
      platform: "linux",
      canonicalVaultPath: "/home/u/vault/",
    })).toBe(deriveVaultCacheIdentity({
      platform: "linux",
      canonicalVaultPath: "/home/u/vault",
    }));

    // The same path on two platforms is two identities: derived state is
    // machine-local, and a platform change is a different machine.
    expect(deriveVaultCacheIdentity({
      platform: "linux",
      canonicalVaultPath: "/home/u/vault",
    })).not.toBe(deriveVaultCacheIdentity({
      platform: "darwin",
      canonicalVaultPath: "/home/u/vault",
    }));
  });
});
