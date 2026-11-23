// SPDX-FileCopyrightText: 2026 cybersader
// SPDX-License-Identifier: GPL-3.0-only

import type { Vault } from "obsidian";

import type { IndexControllerCacheOptions } from "../backends/in-plugin-index-controller";
import type { CacheStoreBundleAvailability, CacheStoreBundlePort } from "./cache-store";
import {
  deleteVaultCache,
  openVaultCacheStore,
  type VaultCacheDeletion,
} from "./local-cache-store";

const LOCKED_RETRY_ATTEMPTS = 5;
const LOCKED_RETRY_DELAY_MS = 1_000;

/**
 * The single owner of this page's persistent index cache. Every store an
 * in-plugin backend opens goes through it, so deleting the cache can first
 * close every store this page opened and wait for any write already queued on
 * them, and only then remove the directory.
 */
export interface InPluginCacheCustody {
  /** Cache options for one in-plugin backend. Opens nothing until asked. */
  options(sourcePolicyHash: string): IndexControllerCacheOptions;
  /**
   * Stops all further use of the cache for this page, waits until every store
   * this page opened is disposed (each disposal runs after the writes already
   * queued on it), then deletes this vault's cache directory.
   */
  deleteForPolicy(): Promise<VaultCacheDeletion>;
}

export interface InPluginCacheCustodyDependencies {
  readonly open: () => Promise<CacheStoreBundleAvailability>;
  readonly remove: () => Promise<VaultCacheDeletion>;
  readonly wait: (ms: number) => Promise<void>;
}

export function createInPluginCacheCustody(
  vault: Vault,
  dependencies: Partial<InPluginCacheCustodyDependencies> = {},
): InPluginCacheCustody {
  const target = { adapter: vault.adapter, vaultConfigDirName: vault.configDir };
  return new CacheCustody({
    open: dependencies.open ?? (() => openVaultCacheStore(target)),
    remove: dependencies.remove ?? (() => deleteVaultCache(target)),
    wait: dependencies.wait ?? ((ms) => new Promise((resolve) => { window.setTimeout(resolve, ms); })),
  });
}

class CacheCustody implements InPluginCacheCustody {
  private closed = false;
  private readonly opening = new Set<Promise<unknown>>();
  private readonly stores = new Set<CacheStoreBundlePort>();

  constructor(private readonly dependencies: InPluginCacheCustodyDependencies) {}

  options(sourcePolicyHash: string): IndexControllerCacheOptions {
    return { sourcePolicyHash, openStore: () => this.openStore() };
  }

  async deleteForPolicy(): Promise<VaultCacheDeletion> {
    this.closed = true;
    // An open in flight disposes its own store once it sees the custody closed.
    await Promise.allSettled([...this.opening]);
    const stores = [...this.stores];
    this.stores.clear();
    await Promise.allSettled(stores.map((store) => store.dispose()));
    for (let attempt = 1; ; attempt += 1) {
      const outcome = await this.dependencies.remove();
      if (outcome.kind !== "locked" || attempt >= LOCKED_RETRY_ATTEMPTS) return outcome;
      await this.dependencies.wait(LOCKED_RETRY_DELAY_MS);
    }
  }

  private openStore(): Promise<CacheStoreBundleAvailability> {
    if (this.closed) return Promise.reject(closedError());
    const opening = (async () => {
      const availability = await this.dependencies.open();
      if (availability.kind !== "available") return availability;
      if (this.closed) {
        await availability.store.dispose();
        throw closedError();
      }
      this.stores.add(availability.store);
      return availability;
    })();
    this.opening.add(opening);
    void opening.then(
      () => this.opening.delete(opening),
      () => this.opening.delete(opening),
    );
    return opening;
  }
}

function closedError(): Error {
  return new Error("the persistent cache is closed for this page");
}
