// SPDX-FileCopyrightText: 2026 cybersader
// SPDX-License-Identifier: GPL-3.0-only

import type { EventRef, TAbstractFile, TFile, Vault } from "obsidian";

import {
  DEFAULT_ENABLED_SOURCE_FORMATS,
  classifySourcePath,
  isSourceFormatEnabled,
  type EnabledSourceFormats,
  type SourceFormat,
} from "./source-formats";
import type { SourceInput } from "./worker/protocol";

export const ACTIVE_VAULT_ID = "active-vault";
export const MAX_INDEXABLE_SOURCE_BYTES = 10 * 1024 * 1024;
/**
 * Temporary compatibility bound for the Markdown-only excerpt path. Worker-side
 * stored excerpt hydration removes this vault reread in the multi-format wave.
 */
export const MAX_EXCERPT_SOURCE_BYTES = 1024 * 1024;

const DEFAULT_ACTIVE_VAULT_READ_TIMEOUT_MS = 30_000;

/// Converts a millisecond mtime to the canonical nanosecond string the ABI
/// requires: digits only, at most 39 of them.
///
/// A vault on an SMB or DFS share can report a timestamp this arithmetic
/// cannot represent. A pre-epoch mtime yields a leading minus and fails the
/// digits-only check; NaN or Infinity makes BigInt throw outright. Either way
/// the Rust boundary refuses the whole batch as `source_rejected`, so one source
/// with an odd timestamp stops the entire vault from indexing.
///
/// Clamping is the right response rather than rejecting: an mtime is an
/// acceleration hint used to detect change, never authority over content. A
/// clamped value simply looks old, so the source is read and hashed rather than
/// skipped, which is the safe direction to be wrong in.
export function canonicalMtimeNanos(mtimeMs: number): string {
  if (!Number.isFinite(mtimeMs)) return "0";
  const truncated = Math.trunc(mtimeMs);
  if (truncated <= 0) return "0";
  const nanos = BigInt(truncated) * 1_000_000n;
  // 39 digits is the ABI ceiling. A timestamp that large is nonsense rather
  // than a date, so it clamps to the ceiling instead of failing the batch.
  return nanos > MAX_CANONICAL_MTIME_NANOS ? MAX_CANONICAL_MTIME_NANOS.toString() : nanos.toString();
}

const MAX_CANONICAL_MTIME_NANOS = 10n ** 39n - 1n;

export type VaultSourceEvent =
  | { kind: "upsert"; path: string }
  | { kind: "remove"; path: string }
  | { kind: "rename"; oldPath: string; path: string }
  | { kind: "rescan" };

export type SourceInspection =
  | { kind: "candidate"; path: string; format: SourceFormat; size: number; mtime: number }
  | { kind: "missing"; path: string }
  | { kind: "oversized"; path: string; format: SourceFormat; size: number; mtime: number };

export type StableSourceRead =
  | { kind: "source"; source: SourceInput }
  | { kind: "missing"; path: string }
  | { kind: "oversized"; path: string; format: SourceFormat; size: number; mtime: number }
  | { kind: "stale"; path: string };

/** A timeout carries no source identity so it is safe to aggregate in diagnostics. */
export type SourceReadOutcome = StableSourceRead | {
  kind: "timeout";
  /** Settles only when the uncancellable underlying vault read actually settles. */
  underlyingSettled: Promise<void>;
};

type BinaryReadOutcome =
  | { kind: "bytes"; buffer: ArrayBuffer }
  | Extract<SourceReadOutcome, { kind: "timeout" }>;

/**
 * Transitional presentation-only Markdown read. It remains distinct from
 * `StableSourceRead` and disappears once the Worker-owned stored excerpt path
 * is connected by the index projection slice.
 */
export type ExcerptRead =
  | { kind: "text"; path: string; text: string }
  | { kind: "missing"; path: string }
  | { kind: "oversized"; path: string }
  | { kind: "stale"; path: string };

export interface ActiveVaultSource {
  subscribe(listener: (event: VaultSourceEvent) => void): () => void;
  listSourcePaths(): readonly string[];
  inspectSource(path: string): SourceInspection;
  readSource(inspection: Extract<SourceInspection, { kind: "candidate" }>): Promise<SourceReadOutcome>;
  readExcerptText(path: string): Promise<ExcerptRead>;
}

export class ObsidianActiveVaultSource implements ActiveVaultSource {
  private refs: EventRef[] = [];
  private readonly pendingBinaryReads = new Map<string, Promise<BinaryReadOutcome>>();

  constructor(
    private readonly vault: Vault,
    private readonly enabledFormats: Readonly<EnabledSourceFormats> = DEFAULT_ENABLED_SOURCE_FORMATS,
    private readonly readTimeoutMs = DEFAULT_ACTIVE_VAULT_READ_TIMEOUT_MS,
  ) {}

  subscribe(listener: (event: VaultSourceEvent) => void): () => void {
    if (this.refs.length > 0) throw new Error("active-vault source is already subscribed");

    this.refs = [
      this.vault.on("create", (file) => this.handleCreate(file, listener)),
      this.vault.on("modify", (file) => this.handleModify(file, listener)),
      this.vault.on("delete", (file) => this.handleDelete(file, listener)),
      this.vault.on("rename", (file, oldPath) => this.handleRename(file, oldPath, listener)),
    ];

    let stopped = false;
    return () => {
      if (stopped) return;
      stopped = true;
      const refs = this.refs;
      this.refs = [];
      for (const ref of refs) this.vault.offref(ref);
    };
  }

  listSourcePaths(): readonly string[] {
    return this.vault.getFiles()
      .map((file) => file.path)
      .filter((path) => this.enabledFormat(path) !== null)
      .sort(comparePaths);
  }

  inspectSource(path: string): SourceInspection {
    const format = this.enabledFormat(path);
    if (format === null) return { kind: "missing", path };
    const file = this.vault.getFileByPath(path);
    if (!file || !isSourceFile(file, format)) return { kind: "missing", path };
    if (file.stat.size > MAX_INDEXABLE_SOURCE_BYTES) {
      return { kind: "oversized", path, format, size: file.stat.size, mtime: file.stat.mtime };
    }
    return {
      kind: "candidate",
      path,
      format,
      size: file.stat.size,
      mtime: file.stat.mtime,
    };
  }

  async readSource(
    inspection: Extract<SourceInspection, { kind: "candidate" }>,
  ): Promise<SourceReadOutcome> {
    if (!isSourceFormatEnabled(inspection.format, this.enabledFormats)) {
      return { kind: "missing", path: inspection.path };
    }
    const before = this.vault.getFileByPath(inspection.path);
    if (!before || !isSourceFile(before, inspection.format)) {
      return { kind: "missing", path: inspection.path };
    }
    if (!matchesInspection(before, inspection)) return { kind: "stale", path: inspection.path };

    const read = await this.readBinaryBounded(before);
    if (read.kind === "timeout") return read;

    const after = this.vault.getFileByPath(inspection.path);
    if (!after || !isSourceFile(after, inspection.format)) {
      return { kind: "missing", path: inspection.path };
    }
    if (!matchesInspection(after, inspection)) return { kind: "stale", path: inspection.path };

    const bytes = new Uint8Array(read.buffer);
    if (bytes.byteLength > MAX_INDEXABLE_SOURCE_BYTES) {
      return {
        kind: "oversized",
        path: inspection.path,
        format: inspection.format,
        size: bytes.byteLength,
        mtime: inspection.mtime,
      };
    }
    if (bytes.byteLength !== inspection.size) return { kind: "stale", path: inspection.path };

    return {
      kind: "source",
      source: {
        descriptor: {
          vault_id: ACTIVE_VAULT_ID,
          path: inspection.path,
          format: inspection.format,
          byte_length: bytes.byteLength,
          mtime: Math.floor(inspection.mtime / 1_000),
          mtime_nanos: canonicalMtimeNanos(inspection.mtime),
        },
        bytes,
      },
    };
  }

  private readBinaryBounded(file: TFile): Promise<BinaryReadOutcome> {
    const path = file.path;
    const pending = this.pendingBinaryReads.get(path);
    if (pending) return pending;

    const underlying = Promise.resolve().then(() => this.vault.readBinary(file));
    const underlyingSettled = underlying.then(
      () => undefined,
      () => undefined,
    );
    let timeoutHandle: ReturnType<typeof setTimeout>;
    const timeout = new Promise<BinaryReadOutcome>((resolve) => {
      timeoutHandle = setTimeout(
        () => resolve({ kind: "timeout", underlyingSettled }),
        Math.max(1, this.readTimeoutMs),
      );
    });
    const outcome = Promise.race<BinaryReadOutcome>([
      underlying.then((buffer) => ({ kind: "bytes", buffer })),
      timeout,
    ]);
    this.pendingBinaryReads.set(path, outcome);

    const release = (): void => {
      clearTimeout(timeoutHandle);
      if (this.pendingBinaryReads.get(path) === outcome) {
        this.pendingBinaryReads.delete(path);
      }
    };
    void underlying.then(release, release);
    return outcome;
  }

  async readExcerptText(path: string): Promise<ExcerptRead> {
    const inspection = this.inspectSource(path);
    if (inspection.kind !== "candidate" || inspection.format !== "markdown") {
      return { kind: inspection.kind === "candidate" ? "missing" : inspection.kind, path };
    }
    if (inspection.size > MAX_EXCERPT_SOURCE_BYTES) return { kind: "oversized", path };

    const before = this.vault.getFileByPath(path);
    if (!before || !isSourceFile(before, "markdown")) return { kind: "missing", path };
    if (!matchesInspection(before, inspection)) return { kind: "stale", path };

    const text = await this.vault.cachedRead(before);
    const after = this.vault.getFileByPath(path);
    if (!after || !isSourceFile(after, "markdown")) return { kind: "missing", path };
    if (!matchesInspection(after, inspection)) return { kind: "stale", path };

    return { kind: "text", path, text };
  }

  private handleCreate(
    file: TAbstractFile,
    listener: (event: VaultSourceEvent) => void,
  ): void {
    if (isFile(file) && this.enabledFormat(file.path) !== null) {
      listener({ kind: "upsert", path: file.path });
    }
  }

  private handleModify(
    file: TAbstractFile,
    listener: (event: VaultSourceEvent) => void,
  ): void {
    if (isFile(file) && this.enabledFormat(file.path) !== null) {
      listener({ kind: "upsert", path: file.path });
    }
  }

  private handleDelete(
    file: TAbstractFile,
    listener: (event: VaultSourceEvent) => void,
  ): void {
    if (!isFile(file)) {
      listener({ kind: "rescan" });
      return;
    }
    if (this.enabledFormat(file.path) !== null) listener({ kind: "remove", path: file.path });
  }

  private handleRename(
    file: TAbstractFile,
    oldPath: string,
    listener: (event: VaultSourceEvent) => void,
  ): void {
    if (!isFile(file)) {
      listener({ kind: "rescan" });
      return;
    }

    const oldFormat = this.enabledFormat(oldPath);
    const newFormat = this.enabledFormat(file.path);
    if (oldFormat !== null && newFormat !== null) {
      listener({ kind: "rename", oldPath, path: file.path });
    } else if (oldFormat !== null) {
      listener({ kind: "remove", path: oldPath });
    } else if (newFormat !== null) {
      listener({ kind: "upsert", path: file.path });
    }
  }

  private enabledFormat(path: string): SourceFormat | null {
    const format = classifySourcePath(path);
    return format !== null && isSourceFormatEnabled(format, this.enabledFormats) ? format : null;
  }
}

function matchesInspection(
  file: TFile,
  inspection: Extract<SourceInspection, { kind: "candidate" }>,
): boolean {
  return file.path === inspection.path
    && file.stat.size === inspection.size
    && file.stat.mtime === inspection.mtime;
}

function isFile(file: TAbstractFile): file is TFile {
  return "extension" in file && "stat" in file;
}

function isSourceFile(file: TFile, expectedFormat: SourceFormat): boolean {
  return classifySourcePath(file.path) === expectedFormat;
}

function comparePaths(left: string, right: string): number {
  return left < right ? -1 : left > right ? 1 : 0;
}
