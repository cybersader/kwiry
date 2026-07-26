// SPDX-FileCopyrightText: 2026 cybersader
// SPDX-License-Identifier: GPL-3.0-only
//
// Machine-local cache root resolution. Every input is injected — platform,
// environment, home directory — so the rules are properties a test can drive
// rather than facts about whichever machine happens to run the suite.
//
// Every refusal is RETURNED, never thrown and never substituted. There is no
// branch anywhere in this module that can produce a vault-relative root.

import * as path from "path";

import type { CacheStoreUnavailableReason } from "./cache-store";

export const CACHE_ROOT_SEGMENTS = ["kwiry", "obsidian-cache"] as const;

export interface CacheRootInputs {
  readonly platform: NodeJS.Platform;
  readonly env: Readonly<Record<string, string | undefined>>;
  readonly homedir: () => string;
}

export type CacheRootResolution =
  | { readonly kind: "root"; readonly path: string }
  | { readonly kind: "unavailable"; readonly reason: CacheStoreUnavailableReason };

/** Path arithmetic follows the TARGET platform, not the running one. */
export function resolverFor(platform: NodeJS.Platform): path.PlatformPath {
  return platform === "win32" ? path.win32 : path.posix;
}

export function resolveCacheRoot(inputs: CacheRootInputs): CacheRootResolution {
  const base = resolveBase(inputs);
  if (base.kind !== "root") return base;
  return {
    kind: "root",
    path: resolverFor(inputs.platform).join(base.path, ...CACHE_ROOT_SEGMENTS),
  };
}

function resolveBase(inputs: CacheRootInputs): CacheRootResolution {
  switch (inputs.platform) {
    case "win32":
      return resolveWindowsBase(inputs);
    case "darwin":
      // XDG_CACHE_HOME is deliberately ignored on macOS: the OS cache location
      // is ~/Library/Caches, and honouring an XDG variable here would move the
      // derived state somewhere the platform does not treat as a cache.
      return joinHome(inputs, ["Library", "Caches"]);
    case "linux":
      return resolveLinuxBase(inputs);
    default:
      return { kind: "unavailable", reason: "unsupported_platform" };
  }
}

function resolveWindowsBase(inputs: CacheRootInputs): CacheRootResolution {
  const local = trimmed(inputs.env.LOCALAPPDATA);
  // APPDATA is never a fallback. A roaming profile synchronises between
  // machines, and derived state that follows a user to another machine is
  // exactly what a machine-local cache must not be.
  if (local === null) return { kind: "unavailable", reason: "no_machine_local_root" };
  if (!path.win32.isAbsolute(local)) {
    return { kind: "unavailable", reason: "root_not_absolute" };
  }
  if (isWindowsNetworkPath(local)) {
    return { kind: "unavailable", reason: "root_not_machine_local" };
  }
  return { kind: "root", path: local };
}

function resolveLinuxBase(inputs: CacheRootInputs): CacheRootResolution {
  const configured = trimmed(inputs.env.XDG_CACHE_HOME);
  if (configured !== null) {
    // A set-but-relative XDG_CACHE_HOME is a refusal, not a fall-through to
    // ~/.cache: silently writing somewhere other than where the user pointed
    // is the silent-fallback pattern this design forbids.
    return path.posix.isAbsolute(configured)
      ? { kind: "root", path: configured }
      : { kind: "unavailable", reason: "root_not_absolute" };
  }
  return joinHome(inputs, [".cache"]);
}

function joinHome(inputs: CacheRootInputs, segments: readonly string[]): CacheRootResolution {
  let home: unknown;
  try {
    home = inputs.homedir();
  } catch {
    return { kind: "unavailable", reason: "no_machine_local_root" };
  }
  const resolved = typeof home === "string" ? trimmed(home) : null;
  if (resolved === null) return { kind: "unavailable", reason: "no_machine_local_root" };
  const resolver = resolverFor(inputs.platform);
  if (!resolver.isAbsolute(resolved)) {
    return { kind: "unavailable", reason: "root_not_absolute" };
  }
  return { kind: "root", path: resolver.join(resolved, ...segments) };
}

/**
 * UNC paths (`\\server\share`, `\\?\UNC\...`) are remote by construction.
 *
 * Declared gap: a Windows mapped drive letter backed by SMB is NOT detectable
 * from here — that needs `GetDriveTypeW`, which plugin JavaScript cannot reach.
 * macOS `MNT_LOCAL` is likewise unreachable. Linux network filesystems are
 * screened separately from the mount table.
 */
export function isWindowsNetworkPath(value: string): boolean {
  const normalized = value.replaceAll("/", "\\");
  return normalized.startsWith("\\\\");
}

/**
 * Linux mount-table screening. Bounded single read of a small pseudo-file; the
 * deepest mount point covering the root wins, and a known network filesystem
 * type is a refusal.
 */
export const NETWORK_FILESYSTEM_TYPES: readonly string[] = [
  "9p",
  "afs",
  "cifs",
  "drvfs",
  "fuse.sshfs",
  "nfs",
  "nfs4",
  "smb2",
  "smb3",
  "smbfs",
];

export function isNetworkMountedPath(rootPath: string, mountInfo: string): boolean {
  let best: { length: number; fstype: string } | null = null;
  for (const line of mountInfo.split("\n")) {
    const separator = line.indexOf(" - ");
    if (separator < 0) continue;
    const fields = line.slice(0, separator).split(" ");
    const mountPoint = fields[4];
    const fstype = line.slice(separator + 3).split(" ")[0];
    if (typeof mountPoint !== "string" || typeof fstype !== "string") continue;
    // mountinfo is a Linux pseudo-file, so its paths are POSIX by construction
    // regardless of which platform happens to be running the comparison.
    if (!isPathWithin(mountPoint, rootPath, "linux") && mountPoint !== rootPath) continue;
    if (best === null || mountPoint.length > best.length) {
      best = { length: mountPoint.length, fstype };
    }
  }
  return best !== null && NETWORK_FILESYSTEM_TYPES.includes(best.fstype);
}

/**
 * Segment-wise containment. A raw `startsWith` is wrong: "/vaultsomething"
 * starts with "/vault" without being inside it.
 *
 * The platform is REQUIRED rather than taken from the ambient `path`, because
 * every caller has already folded its inputs for a specific target platform.
 * Mixing a win32 fold with POSIX path arithmetic silently answers `false` for
 * every backslash-separated pair, which would make the win32 `root_inside_vault`
 * refusal untestable on a Linux runner — and untested is how it stops working.
 */
export function isPathWithin(
  container: string,
  candidate: string,
  platform: NodeJS.Platform,
): boolean {
  const resolver = resolverFor(platform);
  const relative = resolver.relative(container, candidate);
  if (relative === "") return true;
  return !relative.startsWith("..") && !resolver.isAbsolute(relative);
}

export function foldPathForComparison(platform: NodeJS.Platform, value: string): string {
  const stripped = normalizePathSeparators(platform, value);
  // Locale-independent: toLocaleLowerCase would fold differently under a
  // Turkish locale and produce two identities for one vault.
  return platform === "win32" || platform === "darwin" ? stripped.toLowerCase() : stripped;
}

/**
 * Identity folding is deliberately NARROWER than comparison folding: darwin is
 * case-folded for containment and is not case-folded here.
 *
 * macOS APFS can be formatted case-sensitive, where `/Vaults/Notes` and
 * `/Vaults/notes` are two distinct vaults. Folding them together would give two
 * vaults one identity, therefore one cache directory and one `current.json` —
 * and the identity-mismatch refusal structurally cannot catch it, because the
 * two identities would be equal. Vault B's pointer would be served to vault A
 * as a hit, disclosing B's paths and titles inside A, which is precisely what
 * an opaque identity exists to prevent.
 *
 * The failure modes are asymmetric, so the tie is broken toward not folding: on
 * a case-INsensitive volume reached through two spellings, not folding costs a
 * duplicate cache directory and one extra cold build; folding costs a SHARED
 * one. win32 folding stays — NTFS is case-insensitive by configuration default
 * and `realpathSync.native` already canonicalizes the on-disk spelling there.
 */
export function foldPathForIdentity(platform: NodeJS.Platform, value: string): string {
  const stripped = normalizePathSeparators(platform, value);
  return platform === "win32" ? stripped.toLowerCase() : stripped;
}

function normalizePathSeparators(platform: NodeJS.Platform, value: string): string {
  const separated = platform === "win32" ? value.replaceAll("/", "\\") : value;
  return stripTrailingSeparator(separated, platform);
}

export function stripTrailingSeparator(value: string, platform: NodeJS.Platform): string {
  const separator = platform === "win32" ? "\\" : "/";
  let result = value;
  while (result.length > 1 && result.endsWith(separator)) {
    const trimmedResult = result.slice(0, -1);
    // Never strip past a root ("/" or "C:\").
    if (platform === "win32" && /^[A-Za-z]:$/u.test(trimmedResult)) break;
    result = trimmedResult;
  }
  return result;
}

function trimmed(value: string | undefined): string | null {
  if (typeof value !== "string") return null;
  const result = value.trim();
  return result.length > 0 ? result : null;
}
