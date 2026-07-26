// SPDX-FileCopyrightText: 2026 cybersader
// SPDX-License-Identifier: GPL-3.0-only
//
// The desktop cache store. It writes OUTSIDE the vault and outside the vault's
// configuration directory, under the machine-local OS cache root, and it
// reports unavailable — truthfully, with a distinct reason — whenever no safe
// machine-local root exists. There is no branch in this file that can fall
// back to vault-relative storage.

import * as fs from "fs";
import * as fsPromises from "fs/promises";
import * as os from "os";
import * as path from "path";

import {
  CACHE_POINTER_VERSION,
  CacheStoreError,
  MAX_CACHE_BLOB_BYTES,
  MAX_POINTER_BYTES,
  MAX_RETAINED_GENERATIONS,
  imageFileName,
  imageRelativePath,
  isCacheIdentityEnvelope,
  isGenerationId,
  isNonNegativeSafeInteger,
  isRecord,
  isSha256Hex,
  type CacheIdentityEnvelope,
  type CacheLoad,
  type CacheRecord,
  type CacheStoreAvailability,
  type CacheStorePort,
  type CacheWrite,
} from "./cache-store";
import {
  foldPathForComparison,
  isNetworkMountedPath,
  isPathWithin,
  isWindowsNetworkPath,
  resolveCacheRoot,
  resolverFor,
  type CacheRootInputs,
} from "./cache-root";
import { deriveVaultCacheIdentity } from "./vault-identity";
import {
  resolveCanonicalVaultPath,
  type VaultLocationIo,
  type VaultLocationSource,
} from "./vault-location";

const DIRECTORY_MODE = 0o700;
const FILE_MODE = 0o600;
const PROBE_PREFIX = ".store-probe-";
const TEMP_MARKER = ".tmp-";
const STALE_LOCK_MS = 15 * 60 * 1000;
const PROBE_BYTES = 32;

export interface CacheFileStats {
  isFile(): boolean;
  isDirectory(): boolean;
  isSymbolicLink(): boolean;
  size: number;
  mode: number;
  mtimeMs: number;
}

export interface CacheFileHandle {
  write(bytes: Uint8Array): Promise<void>;
  readInto(target: Uint8Array): Promise<number>;
  chmod(mode: number): Promise<void>;
  sync(): Promise<void>;
  close(): Promise<void>;
}

/**
 * Every filesystem call the store makes, named so a test can assert the fsync
 * ordering and inject a failure at a chosen step without monkey-patching "fs".
 */
export interface CacheFileSystem {
  mkdir(target: string, options: { recursive: true; mode: number }): Promise<void>;
  chmod(target: string, mode: number): Promise<void>;
  lstat(target: string): Promise<CacheFileStats>;
  readdir(target: string): Promise<string[]>;
  open(target: string, flags: string, mode?: number): Promise<CacheFileHandle>;
  rename(from: string, to: string): Promise<void>;
  unlink(target: string): Promise<void>;
  rm(target: string, options: { recursive: true; force: true }): Promise<void>;
}

export interface LocalCacheStoreOptions {
  readonly canonicalVaultPath: string;
  /** `Vault.configDir`. Never hardcoded: Obsidian allows a non-default name. */
  readonly vaultConfigDirName: string;
  readonly rootInputs?: CacheRootInputs;
  /** Tests only. Still probed and still subject to the containment refusal. */
  readonly rootOverride?: string;
  readonly fs?: CacheFileSystem;
  readonly maxBlobBytes?: number;
  readonly now?: () => number;
  readonly randomSuffix?: () => string;
  /** Linux mount table, injected so the network screen is testable. */
  readonly readMountInfo?: () => string | null;
}

interface CachePointer {
  readonly pointerVersion: typeof CACHE_POINTER_VERSION;
  readonly vaultIdentity: string;
  readonly generationId: string;
  readonly previousGenerationId: string | null;
  readonly file: string;
  readonly byteLength: number;
  readonly sha256: string;
  readonly identity: CacheIdentityEnvelope;
  readonly writtenAtMs: number;
}

const POINTER_KEYS: readonly string[] = [
  "pointerVersion",
  "vaultIdentity",
  "generationId",
  "previousGenerationId",
  "file",
  "byteLength",
  "sha256",
  "identity",
  "writtenAtMs",
];

export function nodeCacheFileSystem(): CacheFileSystem {
  return {
    mkdir: async (target, options) => {
      await fsPromises.mkdir(target, options);
    },
    chmod: (target, mode) => fsPromises.chmod(target, mode),
    lstat: async (target) => {
      const stats = await fsPromises.lstat(target);
      return {
        isFile: () => stats.isFile(),
        isDirectory: () => stats.isDirectory(),
        isSymbolicLink: () => stats.isSymbolicLink(),
        size: stats.size,
        mode: stats.mode,
        mtimeMs: stats.mtimeMs,
      };
    },
    readdir: (target) => fsPromises.readdir(target),
    open: async (target, flags, mode) => {
      const handle = await fsPromises.open(target, flags, mode);
      return {
        write: async (bytes) => {
          let written = 0;
          while (written < bytes.byteLength) {
            const result = await handle.write(bytes, written, bytes.byteLength - written, null);
            if (result.bytesWritten <= 0) throw new Error("cache image write stalled");
            written += result.bytesWritten;
          }
        },
        readInto: async (buffer) => {
          let read = 0;
          while (read < buffer.byteLength) {
            const result = await handle.read(buffer, read, buffer.byteLength - read, read);
            if (result.bytesRead <= 0) break;
            read += result.bytesRead;
          }
          return read;
        },
        chmod: (fileMode) => handle.chmod(fileMode),
        sync: () => handle.sync(),
        close: () => handle.close(),
      };
    },
    rename: (from, to) => fsPromises.rename(from, to),
    unlink: (target) => fsPromises.unlink(target),
    rm: (target, options) => fsPromises.rm(target, options),
  };
}

export async function openLocalCacheStore(
  options: LocalCacheStoreOptions,
): Promise<CacheStoreAvailability> {
  const platform = options.rootInputs?.platform ?? process.platform;
  const io = options.fs ?? nodeCacheFileSystem();
  const canonicalVaultPath = options.canonicalVaultPath.trim();
  if (canonicalVaultPath.length === 0
    || !resolverFor(platform).isAbsolute(canonicalVaultPath)) {
    return { kind: "unavailable", reason: "vault_location_unavailable" };
  }

  let root: string;
  if (options.rootOverride !== undefined) {
    if (!resolverFor(platform).isAbsolute(options.rootOverride)) {
      return { kind: "unavailable", reason: "root_not_absolute" };
    }
    // The override skips `resolveCacheRoot`, so it must not skip the screens
    // that live inside it. The Linux mount-table screen below already runs
    // against whatever `root` ends up being; the win32 UNC refusal did not,
    // which made the override asymmetrically weaker than the resolved path.
    if (isWindowsNetworkPath(options.rootOverride) && platform === "win32") {
      return { kind: "unavailable", reason: "root_not_machine_local" };
    }
    root = options.rootOverride;
  } else {
    const resolution = resolveCacheRoot(options.rootInputs ?? {
      platform: process.platform,
      env: process.env,
      homedir: () => os.homedir(),
    });
    if (resolution.kind !== "root") return resolution;
    root = resolution.path;
  }

  // Containment refusal. Reachable in reality — a $HOME inside a vault, or an
  // XDG_CACHE_HOME pointed at one — and the answer is a refusal, never a
  // rewrite of the root to something vault-relative.
  //
  // Every step here — the fold, the join, and the containment arithmetic — uses
  // the TARGET platform. Folding for win32 and then comparing with the ambient
  // POSIX `path.relative` would answer `false` for every backslash-separated
  // pair, so the win32 refusal would look correct while never firing.
  const foldedRoot = foldPathForComparison(platform, root);
  const foldedVault = foldPathForComparison(platform, canonicalVaultPath);
  const foldedConfig = foldPathForComparison(
    platform,
    resolverFor(platform).join(canonicalVaultPath, options.vaultConfigDirName),
  );
  if (isPathWithin(foldedVault, foldedRoot, platform)
    || isPathWithin(foldedConfig, foldedRoot, platform)
    || isPathWithin(foldedRoot, foldedVault, platform)) {
    return { kind: "unavailable", reason: "root_inside_vault" };
  }

  if (platform === "linux") {
    const mountInfo = (options.readMountInfo ?? readLinuxMountInfo)();
    if (mountInfo !== null && isNetworkMountedPath(root, mountInfo)) {
      return { kind: "unavailable", reason: "root_not_machine_local" };
    }
  }

  const probe = await probeCacheRoot(io, root, platform, options.randomSuffix ?? randomSuffix);
  if (probe !== null) return { kind: "unavailable", reason: probe };

  const vaultCacheIdentity = deriveVaultCacheIdentity({ platform, canonicalVaultPath });
  const store = new LocalCacheStore({
    io,
    platform,
    root,
    vaultCacheIdentity,
    maxBlobBytes: options.maxBlobBytes ?? MAX_CACHE_BLOB_BYTES,
    now: options.now ?? (() => Date.now()),
    randomSuffix: options.randomSuffix ?? randomSuffix,
  });
  return { kind: "available", store };
}

/**
 * Composition entry for the host: it takes the vault's adapter structurally,
 * resolves the canonical location, and opens the store. It is the only place
 * that needs to know both halves, which keeps `main.ts` free of cache logic
 * and keeps the raw vault path from travelling any further than this call.
 *
 * `rootOverride` is deliberately NOT on this surface. This is the entry the
 * host composes, and a root the caller names is a root that never went through
 * `resolveCacheRoot`. Keeping the escape hatch on `openLocalCacheStore` alone
 * means the composed path has exactly one way to obtain a root.
 */
export async function openVaultCacheStore(options: {
  readonly adapter: VaultLocationSource;
  readonly vaultConfigDirName: string;
  readonly locationIo?: VaultLocationIo;
  readonly rootInputs?: CacheRootInputs;
  readonly fs?: CacheFileSystem;
}): Promise<CacheStoreAvailability> {
  const location = resolveCanonicalVaultPath(options.adapter, options.locationIo);
  if (location.kind !== "path") return { kind: "unavailable", reason: location.reason };
  return openLocalCacheStore({
    canonicalVaultPath: location.canonicalPath,
    vaultConfigDirName: options.vaultConfigDirName,
    rootInputs: options.rootInputs,
    fs: options.fs,
  });
}

interface LocalCacheStoreDependencies {
  readonly io: CacheFileSystem;
  readonly platform: NodeJS.Platform;
  readonly root: string;
  readonly vaultCacheIdentity: string;
  readonly maxBlobBytes: number;
  readonly now: () => number;
  readonly randomSuffix: () => string;
}

class LocalCacheStore implements CacheStorePort {
  private readonly vaultDirectory: string;
  private readonly generationsDirectory: string;
  private readonly quarantineDirectory: string;
  private readonly pointerPath: string;
  private readonly lockPath: string;
  private queue: Promise<unknown> = Promise.resolve();
  private disposed = false;
  /**
   * True only between a successful `acquireLock` and its release. The lock file
   * belongs to whoever created it, so nothing in this class may unlink it
   * without first establishing that THIS instance is that whoever.
   */
  private holdsLock = false;

  constructor(private readonly deps: LocalCacheStoreDependencies) {
    this.vaultDirectory = path.join(deps.root, "vaults", deps.vaultCacheIdentity);
    this.generationsDirectory = path.join(this.vaultDirectory, "generations");
    this.quarantineDirectory = path.join(this.vaultDirectory, "quarantine");
    this.pointerPath = path.join(this.vaultDirectory, "current.json");
    this.lockPath = path.join(this.vaultDirectory, "writer.lock");
  }

  get vaultCacheIdentity(): string {
    return this.deps.vaultCacheIdentity;
  }

  load(): Promise<CacheLoad> {
    return this.serialize(() => this.loadLocked());
  }

  async put(write: CacheWrite): Promise<CacheRecord> {
    // Validated before the in-process queue is entered so a rejected write can
    // never have touched a disk, and before any handle is opened so the
    // "nothing was written on rejection" assertion is structural.
    this.requireUsable();
    this.validateWrite(write);
    return this.serialize(() => this.putLocked(write));
  }

  /**
   * The reason is caller-side context only: every reason removes everything
   * this store owns, so branching on it would invent a distinction the desktop
   * store does not actually make.
   */
  discard(_reason: "corrupt" | "incompatible" | "requested"): Promise<void> {
    return this.serialize(async () => {
      this.requireUsable();
      await this.withWriterLock(async () => {
        // Pointer first: an interrupted discard degrades to a clean miss, never
        // to a pointer naming a file that is already gone.
        await this.removeQuietly(this.pointerPath);
        await this.removeTreeQuietly(this.generationsDirectory);
        await this.removeTreeQuietly(this.quarantineDirectory);
      });
    });
  }

  /**
   * Disposal releases NOTHING on disk.
   *
   * Every lock this instance takes is released in `withWriterLock`'s `finally`,
   * and `dispose` is serialized behind the same queue as those operations, so
   * by the time it runs `holdsLock` is provably false. An unconditional unlink
   * here would therefore only ever delete a lock belonging to a DIFFERENT
   * process — a second Obsidian window on the same vault would have its live
   * lock removed mid-write, admitting a second writer whose retention pass then
   * deletes the image the first one is still about to name.
   */
  dispose(): Promise<void> {
    return this.serialize(async () => {
      if (this.disposed) return;
      this.disposed = true;
      if (this.holdsLock) await this.releaseLock();
    });
  }

  private serialize<T>(operation: () => Promise<T>): Promise<T> {
    const next = this.queue.then(operation, operation);
    this.queue = next.then(
      () => undefined,
      () => undefined,
    );
    return next;
  }

  private requireUsable(): void {
    if (this.disposed) throw new CacheStoreError("disposed", "Cache store is disposed.");
  }

  private validateWrite(write: CacheWrite): void {
    if (!isGenerationId(write.generationId)) {
      throw new CacheStoreError("invalid_generation_id", "Generation identifier is invalid.");
    }
    if (!(write.bytes instanceof Uint8Array)
      || write.bytes.byteLength === 0
      || write.bytes.byteLength > this.deps.maxBlobBytes
      || write.byteLength !== write.bytes.byteLength
      || !isSha256Hex(write.sha256)) {
      throw new CacheStoreError("invalid_blob", "Cache image is invalid.");
    }
    if (!isCacheIdentityEnvelope(write.identity)
      || write.identity.cache_identity !== this.deps.vaultCacheIdentity) {
      throw new CacheStoreError("invalid_identity", "Cache identity envelope is invalid.");
    }
  }

  private async loadLocked(): Promise<CacheLoad> {
    this.requireUsable();
    let stats: CacheFileStats;
    try {
      stats = await this.deps.io.lstat(this.pointerPath);
    } catch {
      return { kind: "miss", reason: "absent" };
    }
    if (stats.isSymbolicLink() || !stats.isFile() || stats.size > MAX_POINTER_BYTES) {
      await this.removeQuietly(this.pointerPath);
      return { kind: "miss", reason: "pointer_corrupt" };
    }

    let raw: string;
    try {
      raw = await this.readBoundedFile(this.pointerPath, MAX_POINTER_BYTES);
    } catch {
      return { kind: "miss", reason: "pointer_unreadable" };
    }
    const pointer = this.parsePointer(raw);
    if (pointer === null) {
      await this.removeQuietly(this.pointerPath);
      return { kind: "miss", reason: "pointer_corrupt" };
    }
    if (pointer.vaultIdentity !== this.deps.vaultCacheIdentity
      || pointer.identity.cache_identity !== this.deps.vaultCacheIdentity) {
      // Nothing is deleted. A foreign identity means the data may belong to
      // another vault, and destroying it is strictly worse than a cold build.
      return { kind: "miss", reason: "identity_mismatch" };
    }

    const imagePath = path.join(this.generationsDirectory, imageFileName(pointer.generationId));
    let imageStats: CacheFileStats;
    try {
      imageStats = await this.deps.io.lstat(imagePath);
    } catch {
      return { kind: "miss", reason: "image_absent" };
    }
    if (imageStats.isSymbolicLink() || !imageStats.isFile()) {
      await this.quarantine(imagePath, pointer.generationId);
      return { kind: "miss", reason: "image_unreadable" };
    }
    if (imageStats.size !== pointer.byteLength) {
      await this.quarantine(imagePath, pointer.generationId);
      return { kind: "miss", reason: "image_length_mismatch" };
    }

    const buffer = new Uint8Array(pointer.byteLength);
    let read: number;
    try {
      const handle = await this.deps.io.open(imagePath, "r");
      try {
        read = await handle.readInto(buffer);
      } finally {
        await handle.close();
      }
    } catch {
      await this.quarantine(imagePath, pointer.generationId);
      return { kind: "miss", reason: "image_unreadable" };
    }
    if (read !== pointer.byteLength) {
      await this.quarantine(imagePath, pointer.generationId);
      return { kind: "miss", reason: "image_unreadable" };
    }

    return {
      kind: "hit",
      record: {
        generationId: pointer.generationId,
        byteLength: pointer.byteLength,
        sha256: pointer.sha256,
        identity: pointer.identity,
      },
      bytes: buffer,
      // Length was checked against the pointer; the digest was NOT. Stated in
      // the value so a consumer cannot treat these bytes as verified by
      // omission.
      digestVerified: false,
    };
  }

  private async putLocked(write: CacheWrite): Promise<CacheRecord> {
    this.requireUsable();
    return this.withWriterLock(async () => {
      const previous = await this.readPointerQuietly();
      await this.deps.io.mkdir(this.generationsDirectory, {
        recursive: true,
        mode: DIRECTORY_MODE,
      });
      await this.applyDirectoryMode(this.generationsDirectory);
      await this.removeTreeQuietly(this.quarantineDirectory);

      const suffix = this.deps.randomSuffix();
      const imagePath = path.join(this.generationsDirectory, imageFileName(write.generationId));
      const imageTemp = `${imagePath}${TEMP_MARKER}${suffix}`;
      const pointerTemp = `${this.pointerPath}${TEMP_MARKER}${suffix}`;

      try {
        // (1) image data durable, then (2) the rename that names it durable,
        // before the pointer is written at all.
        await this.writeFileDurably(imageTemp, write.bytes);
        await this.deps.io.rename(imageTemp, imagePath);
        await this.syncDirectory(this.generationsDirectory);

        const pointer: CachePointer = {
          pointerVersion: CACHE_POINTER_VERSION,
          vaultIdentity: this.deps.vaultCacheIdentity,
          generationId: write.generationId,
          // Recorded, not inferred from mtime: retention must not depend on
          // metadata that a copy, a restore or a clock change can rewrite.
          previousGenerationId: previous !== null && previous.generationId !== write.generationId
            ? previous.generationId
            : null,
          file: imageRelativePath(write.generationId),
          byteLength: write.byteLength,
          sha256: write.sha256,
          identity: write.identity,
          writtenAtMs: this.deps.now(),
        };
        const encoded = new TextEncoder().encode(JSON.stringify(pointer));
        if (encoded.byteLength > MAX_POINTER_BYTES) {
          throw new CacheStoreError("invalid_identity", "Cache pointer exceeded its bound.");
        }
        await this.writeFileDurably(pointerTemp, encoded);
        // (3) The commit point. Everything it names is already durable.
        await this.deps.io.rename(pointerTemp, this.pointerPath);
        await this.syncDirectory(this.vaultDirectory, true);
      } catch (error) {
        await this.removeQuietly(imageTemp);
        await this.removeQuietly(pointerTemp);
        if (error instanceof CacheStoreError) throw error;
        throw new CacheStoreError("write_failed", "Cache image could not be written.");
      }

      // Retention is hygiene and runs after the commit, so a failure here is
      // degraded housekeeping, never a failed write.
      await this.pruneQuietly(write.generationId);
      return {
        generationId: write.generationId,
        byteLength: write.byteLength,
        sha256: write.sha256,
        identity: write.identity,
      };
    });
  }

  private async writeFileDurably(target: string, bytes: Uint8Array): Promise<void> {
    const handle = await this.deps.io.open(target, "wx", FILE_MODE);
    try {
      await handle.write(bytes);
      await handle.chmod(FILE_MODE);
      await handle.sync();
    } finally {
      await handle.close();
    }
  }

  /**
   * Directory fsync, which is what makes a rename itself durable.
   *
   * On win32 opening a directory throws and Node exposes no alternative; NTFS
   * journals the rename's metadata instead, so the guarantee is weaker there.
   * That is stated, not papered over.
   *
   * `optional` is only ever true for the sync that follows the pointer rename:
   * the commit has already happened by then, so failing the write would be a
   * lie. Every sync the crash-safety argument depends on is required.
   */
  private async syncDirectory(target: string, optional = false): Promise<void> {
    try {
      const handle = await this.deps.io.open(target, "r");
      try {
        await handle.sync();
      } finally {
        await handle.close();
      }
    } catch (error) {
      if (optional || this.deps.platform === "win32") return;
      throw error;
    }
  }

  private async withWriterLock<T>(operation: () => Promise<T>): Promise<T> {
    await this.deps.io.mkdir(this.vaultDirectory, { recursive: true, mode: DIRECTORY_MODE });
    await this.applyDirectoryMode(this.vaultDirectory);
    await this.acquireLock();
    this.holdsLock = true;
    try {
      return await operation();
    } finally {
      await this.releaseLock();
    }
  }

  /**
   * Best-effort variant for work that is not itself a write but still mutates
   * what a writer owns. It returns whether the operation ran: a lock held
   * elsewhere is a normal outcome, not a failure to report upward.
   */
  private async tryWithWriterLock(operation: () => Promise<void>): Promise<boolean> {
    try {
      await this.deps.io.mkdir(this.vaultDirectory, { recursive: true, mode: DIRECTORY_MODE });
      await this.acquireLock();
    } catch {
      return false;
    }
    this.holdsLock = true;
    try {
      await operation();
      return true;
    } finally {
      await this.releaseLock();
    }
  }

  private async releaseLock(): Promise<void> {
    this.holdsLock = false;
    await this.removeQuietly(this.lockPath);
  }

  private async acquireLock(): Promise<void> {
    if (await this.tryCreateLock()) return;
    let stale = false;
    try {
      const stats = await this.deps.io.lstat(this.lockPath);
      stale = this.deps.now() - stats.mtimeMs > STALE_LOCK_MS;
    } catch {
      stale = true;
    }
    if (!stale) {
      throw new CacheStoreError("locked", "Another writer holds the cache store.");
    }
    await this.removeQuietly(this.lockPath);
    if (await this.tryCreateLock()) return;
    throw new CacheStoreError("locked", "Another writer holds the cache store.");
  }

  private async tryCreateLock(): Promise<boolean> {
    try {
      const handle = await this.deps.io.open(this.lockPath, "wx", FILE_MODE);
      try {
        await handle.write(new TextEncoder().encode(
          JSON.stringify({ pid: process.pid, startedAtMs: this.deps.now() }),
        ));
        await handle.chmod(FILE_MODE);
      } finally {
        await handle.close();
      }
      return true;
    } catch {
      return false;
    }
  }

  private async readPointerQuietly(): Promise<CachePointer | null> {
    try {
      const stats = await this.deps.io.lstat(this.pointerPath);
      if (stats.isSymbolicLink() || !stats.isFile() || stats.size > MAX_POINTER_BYTES) return null;
      return this.parsePointer(await this.readBoundedFile(this.pointerPath, MAX_POINTER_BYTES));
    } catch {
      return null;
    }
  }

  private async readBoundedFile(target: string, maximum: number): Promise<string> {
    const handle = await this.deps.io.open(target, "r");
    try {
      const buffer = new Uint8Array(maximum + 1);
      const read = await handle.readInto(buffer);
      if (read > maximum) throw new Error("bounded file exceeded its limit");
      return new TextDecoder("utf-8", { fatal: true }).decode(buffer.subarray(0, read));
    } finally {
      await handle.close();
    }
  }

  private parsePointer(raw: string): CachePointer | null {
    let parsed: unknown;
    try {
      parsed = JSON.parse(raw);
    } catch {
      return null;
    }
    if (!isRecord(parsed)) return null;
    const keys = Object.keys(parsed);
    if (keys.length !== POINTER_KEYS.length) return null;
    if (!keys.every((key) => POINTER_KEYS.includes(key))) return null;
    if (parsed.pointerVersion !== CACHE_POINTER_VERSION
      || !isSha256Hex(parsed.vaultIdentity)
      || !isGenerationId(parsed.generationId)
      || !(parsed.previousGenerationId === null || isGenerationId(parsed.previousGenerationId))
      || !isNonNegativeSafeInteger(parsed.byteLength)
      || parsed.byteLength < 1
      || parsed.byteLength > this.deps.maxBlobBytes
      || !isSha256Hex(parsed.sha256)
      || !isCacheIdentityEnvelope(parsed.identity)
      || !isNonNegativeSafeInteger(parsed.writtenAtMs)) {
      return null;
    }
    // Compared by equality against the derived name rather than sanitized: a
    // traversal value is rejected because it is not equal, not because some
    // filter was clever enough to catch it.
    if (parsed.file !== imageRelativePath(parsed.generationId)) return null;
    return parsed as unknown as CachePointer;
  }

  /**
   * Corruption is moved, never read again, and the pointer goes with it so the
   * next load is a clean miss. Quarantine holds at most one entry and is
   * emptied at the start of the next put.
   *
   * It runs UNDER THE WRITER LOCK even though `load` is not otherwise a writer:
   * this step deletes the pointer and renames an image, and a writer in another
   * process may be committing exactly those names. When the lock is held
   * elsewhere nothing is touched and the caller still gets its miss — leaving
   * corruption in place costs one cold rebuild, while destroying a generation a
   * live writer just committed is unrecoverable.
   */
  private async quarantine(imagePath: string, generationId: string): Promise<void> {
    await this.tryWithWriterLock(async () => {
      // Re-read under the lock. Everything this load observed was observed
      // without it, so a writer may have committed in the meantime; if the
      // pointer no longer names the generation found corrupt, those
      // observations describe a state that no longer exists and nothing here
      // is safe to remove.
      const pointer = await this.readPointerQuietly();
      if (pointer !== null && pointer.generationId !== generationId) return;
      await this.removeQuietly(this.pointerPath);
      try {
        await this.deps.io.rm(this.quarantineDirectory, { recursive: true, force: true });
        await this.deps.io.mkdir(this.quarantineDirectory, {
          recursive: true,
          mode: DIRECTORY_MODE,
        });
        await this.deps.io.rename(
          imagePath,
          path.join(this.quarantineDirectory, `${generationId}-${this.deps.now()}.kwc`),
        );
      } catch {
        await this.removeQuietly(imagePath);
      }
    });
  }

  private async pruneQuietly(currentGenerationId: string): Promise<void> {
    try {
      const pointer = await this.readPointerQuietly();
      // Newest first, then TRUNCATED to the bound, so `MAX_RETAINED_GENERATIONS`
      // is what actually enforces the two-generation limit. Written as a guard
      // that returns when the set is already too large it enforced nothing: the
      // set is built from at most two names, so the condition was unreachable
      // and the real bound was the construction above it.
      const lineage = [imageFileName(currentGenerationId)];
      if (pointer?.previousGenerationId) {
        lineage.push(imageFileName(pointer.previousGenerationId));
      }
      const retained = new Set<string>(lineage.slice(0, MAX_RETAINED_GENERATIONS));
      for (const entry of await this.deps.io.readdir(this.generationsDirectory)) {
        if (retained.has(entry)) continue;
        await this.removeQuietly(path.join(this.generationsDirectory, entry));
      }
      for (const entry of await this.deps.io.readdir(this.vaultDirectory)) {
        if (!entry.includes(TEMP_MARKER)) continue;
        await this.removeQuietly(path.join(this.vaultDirectory, entry));
      }
    } catch {
      // Degraded retention only. The pointer is already committed and correct.
    }
  }

  private async applyDirectoryMode(target: string): Promise<void> {
    try {
      // mkdir's mode is masked by the umask; chmod is not, so owner-only
      // permissions are applied explicitly rather than hoped for.
      await this.deps.io.chmod(target, DIRECTORY_MODE);
    } catch {
      if (this.deps.platform !== "win32") throw new CacheStoreError(
        "write_failed",
        "Cache directory permissions could not be applied.",
      );
    }
  }

  private async removeQuietly(target: string): Promise<void> {
    try {
      await this.deps.io.unlink(target);
    } catch {
      // Absent is the desired state.
    }
  }

  private async removeTreeQuietly(target: string): Promise<void> {
    try {
      await this.deps.io.rm(target, { recursive: true, force: true });
    } catch {
      // Absent is the desired state.
    }
  }
}

async function probeCacheRoot(
  io: CacheFileSystem,
  root: string,
  platform: NodeJS.Platform,
  suffix: () => string,
): Promise<"root_not_a_directory" | "root_not_writable" | "root_probe_failed" | null> {
  try {
    await io.mkdir(root, { recursive: true, mode: DIRECTORY_MODE });
  } catch {
    return "root_not_writable";
  }
  try {
    await io.chmod(root, DIRECTORY_MODE);
  } catch {
    if (platform !== "win32") return "root_not_writable";
  }
  try {
    const stats = await io.lstat(root);
    if (!stats.isDirectory() || stats.isSymbolicLink()) return "root_not_a_directory";
  } catch {
    return "root_not_a_directory";
  }

  // Sweep residue from an interrupted earlier probe before running a new one.
  try {
    for (const entry of await io.readdir(root)) {
      if (!entry.startsWith(PROBE_PREFIX)) continue;
      try {
        await io.unlink(path.join(root, entry));
      } catch {
        // Best effort.
      }
    }
  } catch {
    return "root_probe_failed";
  }

  // Executable, not a guess: the store claims staged-write, fsync, atomic
  // rename and durable-replace semantics, so it proves them once at open.
  const token = suffix();
  const probeA = path.join(root, `${PROBE_PREFIX}${token}.a`);
  const probeB = path.join(root, `${PROBE_PREFIX}${token}.b`);
  try {
    const handle = await io.open(probeA, "wx", FILE_MODE);
    try {
      await handle.write(new Uint8Array(PROBE_BYTES).fill(0x6b));
      await handle.sync();
    } finally {
      await handle.close();
    }
    await io.rename(probeA, probeB);
    try {
      const directory = await io.open(root, "r");
      try {
        await directory.sync();
      } finally {
        await directory.close();
      }
    } catch {
      if (platform !== "win32") throw new Error("directory sync failed");
    }
    const stats = await io.lstat(probeB);
    if (!stats.isFile() || stats.size !== PROBE_BYTES) throw new Error("probe file damaged");
    await io.unlink(probeB);
  } catch {
    try {
      await io.unlink(probeA);
    } catch {
      // Best effort.
    }
    try {
      await io.unlink(probeB);
    } catch {
      // Best effort.
    }
    return "root_probe_failed";
  }
  return null;
}

function readLinuxMountInfo(): string | null {
  try {
    return fs.readFileSync("/proc/self/mountinfo", "utf8");
  } catch {
    return null;
  }
}

function randomSuffix(): string {
  return `${Date.now().toString(36)}-${Math.floor(Math.random() * 0xffffffff).toString(36)}`;
}
