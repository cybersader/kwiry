// SPDX-FileCopyrightText: 2026 cybersader
// SPDX-License-Identifier: GPL-3.0-only
//
// Resolving the canonical vault location. There is deliberately no value
// import from "obsidian" here — an `instanceof FileSystemAdapter` check would
// make this module untestable and would tie a cache concern to a UI package.
// The capability check is structural instead, which is also what makes it
// provably fail-able: a mobile or remote DataAdapter simply has no
// `getBasePath`, and the store then reports unavailable rather than guessing.

import * as fs from "fs";
import * as path from "path";

import type { CacheStoreUnavailableReason } from "./cache-store";

export interface VaultLocationSource {
  readonly getBasePath?: unknown;
}

export interface VaultLocationIo {
  realpathNative(target: string): string;
}

export type VaultLocation =
  | { readonly kind: "path"; readonly canonicalPath: string }
  | { readonly kind: "unavailable"; readonly reason: CacheStoreUnavailableReason };

export const nodeVaultLocationIo: VaultLocationIo = {
  realpathNative: (target) => fs.realpathSync.native(target),
};

export function resolveCanonicalVaultPath(
  adapter: unknown,
  io: VaultLocationIo = nodeVaultLocationIo,
): VaultLocation {
  if (typeof adapter !== "object" || adapter === null) {
    return { kind: "unavailable", reason: "vault_location_unavailable" };
  }
  const source = adapter as VaultLocationSource;
  if (typeof source.getBasePath !== "function") {
    return { kind: "unavailable", reason: "vault_location_unavailable" };
  }
  let basePath: unknown;
  try {
    basePath = (source.getBasePath as () => unknown)();
  } catch {
    return { kind: "unavailable", reason: "vault_location_unavailable" };
  }
  if (typeof basePath !== "string" || basePath.trim().length === 0) {
    return { kind: "unavailable", reason: "vault_location_unavailable" };
  }
  const trimmed = basePath.trim();
  if (!path.isAbsolute(trimmed)) {
    return { kind: "unavailable", reason: "vault_location_unavailable" };
  }
  try {
    // realpath().native resolves symlinks, junctions and 8.3 short names to the
    // one spelling the identity and the containment check must both agree on.
    const canonical = io.realpathNative(trimmed);
    if (typeof canonical !== "string" || canonical.trim().length === 0) {
      return { kind: "unavailable", reason: "vault_location_unavailable" };
    }
    return { kind: "path", canonicalPath: canonical };
  } catch {
    return { kind: "unavailable", reason: "vault_location_unavailable" };
  }
}
