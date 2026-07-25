// SPDX-FileCopyrightText: 2026 cybersader
// SPDX-License-Identifier: GPL-3.0-only

import type { EventRef, TAbstractFile, TFile, Vault } from "obsidian";

import type { SourceInput } from "./worker/protocol";
import { isNormalizedMarkdownPath } from "./vault-path";

export const ACTIVE_VAULT_ID = "active-vault";
export const MAX_INDEXABLE_SOURCE_BYTES = 10 * 1024 * 1024;

export type VaultSourceEvent =
  | { kind: "upsert"; path: string }
  | { kind: "remove"; path: string }
  | { kind: "rename"; oldPath: string; path: string }
  | { kind: "rescan" };

export type SourceInspection =
  | { kind: "candidate"; path: string; size: number; mtime: number }
  | { kind: "missing"; path: string }
  | { kind: "oversized"; path: string };

export type StableSourceRead =
  | { kind: "source"; source: SourceInput }
  | { kind: "missing"; path: string }
  | { kind: "oversized"; path: string }
  | { kind: "stale"; path: string };

export interface ActiveVaultSource {
  subscribe(listener: (event: VaultSourceEvent) => void): () => void;
  listMarkdownPaths(): readonly string[];
  inspectMarkdown(path: string): SourceInspection;
  readMarkdown(inspection: Extract<SourceInspection, { kind: "candidate" }>): Promise<StableSourceRead>;
}

export class ObsidianActiveVaultSource implements ActiveVaultSource {
  private refs: EventRef[] = [];

  constructor(private readonly vault: Vault) {}

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

  listMarkdownPaths(): readonly string[] {
    return this.vault.getMarkdownFiles()
      .map((file) => file.path)
      .filter(isNormalizedMarkdownPath)
      .sort(comparePaths);
  }

  inspectMarkdown(path: string): SourceInspection {
    if (!isNormalizedMarkdownPath(path)) return { kind: "missing", path };
    const file = this.vault.getFileByPath(path);
    if (!file || !isMarkdownFile(file)) return { kind: "missing", path };
    if (file.stat.size > MAX_INDEXABLE_SOURCE_BYTES) return { kind: "oversized", path };
    return {
      kind: "candidate",
      path,
      size: file.stat.size,
      mtime: file.stat.mtime,
    };
  }

  async readMarkdown(
    inspection: Extract<SourceInspection, { kind: "candidate" }>,
  ): Promise<StableSourceRead> {
    const before = this.vault.getFileByPath(inspection.path);
    if (!before || !isMarkdownFile(before)) return { kind: "missing", path: inspection.path };
    if (!matchesInspection(before, inspection)) return { kind: "stale", path: inspection.path };

    const buffer = await this.vault.readBinary(before);
    const after = this.vault.getFileByPath(inspection.path);
    if (!after || !isMarkdownFile(after)) return { kind: "missing", path: inspection.path };
    if (!matchesInspection(after, inspection)) return { kind: "stale", path: inspection.path };

    const bytes = new Uint8Array(buffer);
    if (bytes.byteLength > MAX_INDEXABLE_SOURCE_BYTES) {
      return { kind: "oversized", path: inspection.path };
    }
    if (bytes.byteLength !== inspection.size) return { kind: "stale", path: inspection.path };

    return {
      kind: "source",
      source: {
        descriptor: {
          vault_id: ACTIVE_VAULT_ID,
          path: inspection.path,
          format: "markdown",
          byte_length: bytes.byteLength,
          mtime: Math.floor(inspection.mtime / 1_000),
          mtime_nanos: (BigInt(Math.trunc(inspection.mtime)) * 1_000_000n).toString(),
        },
        bytes,
      },
    };
  }

  private handleCreate(
    file: TAbstractFile,
    listener: (event: VaultSourceEvent) => void,
  ): void {
    if (isFile(file) && isNormalizedMarkdownPath(file.path)) {
      listener({ kind: "upsert", path: file.path });
    }
  }

  private handleModify(
    file: TAbstractFile,
    listener: (event: VaultSourceEvent) => void,
  ): void {
    if (isFile(file) && isNormalizedMarkdownPath(file.path)) {
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
    if (isNormalizedMarkdownPath(file.path)) listener({ kind: "remove", path: file.path });
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

    const oldMarkdown = isNormalizedMarkdownPath(oldPath);
    const newMarkdown = isNormalizedMarkdownPath(file.path);
    if (oldMarkdown && newMarkdown) {
      listener({ kind: "rename", oldPath, path: file.path });
    } else if (oldMarkdown) {
      listener({ kind: "remove", path: oldPath });
    } else if (newMarkdown) {
      listener({ kind: "upsert", path: file.path });
    }
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

function isMarkdownFile(file: TFile): boolean {
  return file.extension.toLowerCase() === "md" && isNormalizedMarkdownPath(file.path);
}

function comparePaths(left: string, right: string): number {
  return left < right ? -1 : left > right ? 1 : 0;
}
