// SPDX-FileCopyrightText: 2026 cybersader
// SPDX-License-Identifier: GPL-3.0-only

import type { EventRef, TAbstractFile, TFile, Vault } from "obsidian";

import type { SourceInput } from "./worker/protocol";
import { isNormalizedMarkdownPath } from "./vault-path";

export const ACTIVE_VAULT_ID = "active-vault";
export const MAX_INDEXABLE_SOURCE_BYTES = 10 * 1024 * 1024;
/**
 * Excerpt hydration runs on the Obsidian main thread, once per distinct hit
 * note, so its input bound is far tighter than the indexing bound: a note may
 * be indexed up to `MAX_INDEXABLE_SOURCE_BYTES` and still be too large to fold
 * interactively. Past this bound the excerpt is reported `oversized` and
 * rendered empty — the hit itself is unaffected.
 */
export const MAX_EXCERPT_SOURCE_BYTES = 1024 * 1024;

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

/**
 * Result of a presentation-only text read used for excerpt hydration. It is
 * deliberately distinct from `StableSourceRead`: nothing indexed is derived
 * from it, and every non-`text` outcome must degrade to an empty excerpt
 * rather than to guessed content.
 */
export type ExcerptRead =
  | { kind: "text"; path: string; text: string }
  | { kind: "missing"; path: string }
  | { kind: "oversized"; path: string }
  | { kind: "stale"; path: string };

export interface ActiveVaultSource {
  subscribe(listener: (event: VaultSourceEvent) => void): () => void;
  listMarkdownPaths(): readonly string[];
  inspectMarkdown(path: string): SourceInspection;
  readMarkdown(inspection: Extract<SourceInspection, { kind: "candidate" }>): Promise<StableSourceRead>;
  readExcerptText(path: string): Promise<ExcerptRead>;
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

  /**
   * Reads current file text for excerpt display only. `cachedRead` is the
   * documented API for "content you only want to display", but it carries no
   * staleness guarantee of its own, so the same before/after stat sandwich
   * `readMarkdown` uses is repeated here.
   */
  async readExcerptText(path: string): Promise<ExcerptRead> {
    const inspection = this.inspectMarkdown(path);
    if (inspection.kind !== "candidate") return { kind: inspection.kind, path };
    if (inspection.size > MAX_EXCERPT_SOURCE_BYTES) return { kind: "oversized", path };

    const before = this.vault.getFileByPath(path);
    if (!before || !isMarkdownFile(before)) return { kind: "missing", path };
    if (!matchesInspection(before, inspection)) return { kind: "stale", path };

    const text = await this.vault.cachedRead(before);
    const after = this.vault.getFileByPath(path);
    if (!after || !isMarkdownFile(after)) return { kind: "missing", path };
    if (!matchesInspection(after, inspection)) return { kind: "stale", path };

    return { kind: "text", path, text };
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
