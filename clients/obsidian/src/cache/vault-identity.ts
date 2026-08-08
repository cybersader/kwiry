// SPDX-FileCopyrightText: 2026 cybersader
// SPDX-License-Identifier: GPL-3.0-only
//
// The opaque vault identity. It is derived from the canonical vault location
// and is the ONLY thing about that location that ever leaves this module: the
// raw path is never persisted, never logged, never put in an error message,
// and never crosses the Worker boundary.

import { createHash } from "crypto";

import { foldPathForIdentity, resolverFor } from "./cache-root";

export const VAULT_IDENTITY_DOMAIN = "kwiry-obsidian-cache/vault-identity/v1";

export interface VaultIdentityInput {
  readonly platform: NodeJS.Platform;
  readonly canonicalVaultPath: string;
}

/**
 * 64 lowercase hex characters, never truncated. A truncated identity invites a
 * collision argument that a cache pointer should not have to answer.
 *
 * On win32 the path is case-folded, so one vault reached through
 * differently-cased spellings yields one identity. On linux — and on darwin,
 * whose APFS volumes can be formatted case-sensitive — case is significant and
 * is preserved: see `foldPathForIdentity` for why folding darwin here would let
 * two distinct vaults share one cache directory undetectably.
 */
export function deriveVaultCacheIdentity(input: VaultIdentityInput): string {
  const trimmedPath = input.canonicalVaultPath.trim();
  if (trimmedPath.length === 0) throw new Error("canonical vault path must not be empty");
  const resolver = resolverFor(input.platform);
  const normalized = foldPathForIdentity(input.platform, resolver.resolve(trimmedPath));
  // NUL-separated: a space is a legal path character, so a space-joined
  // preimage would let two different (platform, path) pairs collide.
  return createHash("sha256")
    .update([VAULT_IDENTITY_DOMAIN, input.platform, normalized].join("\u0000"), "utf8")
    .digest("hex");
}
