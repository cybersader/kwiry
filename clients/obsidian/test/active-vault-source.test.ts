// SPDX-FileCopyrightText: 2026 cybersader
// SPDX-License-Identifier: GPL-3.0-only

import type { EventRef, TAbstractFile, TFile, Vault } from "obsidian";
import { describe, expect, it, vi } from "vitest";

import {
  ACTIVE_VAULT_ID,
  MAX_EXCERPT_SOURCE_BYTES,
  MAX_INDEXABLE_SOURCE_BYTES,
  ObsidianActiveVaultSource,
  type VaultSourceEvent,
} from "../src/active-vault-source";

class FakeVault {
  readonly calls: string[] = [];
  readonly handlers = new Map<string, ((...args: unknown[]) => unknown)[]>();
  readonly files = new Map<string, TFile>();
  readonly contents = new Map<string, Uint8Array>();
  readonly readBinary = vi.fn(async (file: TFile): Promise<ArrayBuffer> => {
    this.onRead?.(file);
    const bytes = this.contents.get(file.path) ?? new Uint8Array();
    return new Uint8Array(bytes).buffer;
  });
  readonly cachedRead = vi.fn(async (file: TFile): Promise<string> => {
    this.onRead?.(file);
    return new TextDecoder().decode(this.contents.get(file.path) ?? new Uint8Array());
  });
  onRead: ((file: TFile) => void) | null = null;

  on(name: string, callback: (...args: unknown[]) => unknown): EventRef {
    this.calls.push(`on:${name}`);
    const handlers = this.handlers.get(name) ?? [];
    handlers.push(callback);
    this.handlers.set(name, handlers);
    return { name, callback } as unknown as EventRef;
  }

  offref(ref: EventRef): void {
    this.calls.push("offref");
    const stored = ref as unknown as { name: string; callback: (...args: unknown[]) => unknown };
    const handlers = this.handlers.get(stored.name) ?? [];
    this.handlers.set(stored.name, handlers.filter((handler) => handler !== stored.callback));
  }

  getMarkdownFiles(): TFile[] {
    this.calls.push("list");
    return [...this.files.values()];
  }

  getFileByPath(path: string): TFile | null {
    return this.files.get(path) ?? null;
  }

  emit(name: string, ...args: unknown[]): void {
    for (const handler of this.handlers.get(name) ?? []) handler(...args);
  }
}

function file(path: string, size = 0, mtime = 0): TFile {
  const name = path.split("/").at(-1) ?? path;
  return {
    path,
    name,
    basename: name.replace(/\.[^.]+$/u, ""),
    extension: name.includes(".") ? name.split(".").at(-1) ?? "" : "",
    stat: { ctime: mtime, mtime, size },
  } as unknown as TFile;
}

function folder(path: string): TAbstractFile {
  return { path, name: path.split("/").at(-1) ?? path } as unknown as TAbstractFile;
}

function source(fake: FakeVault): ObsidianActiveVaultSource {
  return new ObsidianActiveVaultSource(fake as unknown as Vault);
}

describe("ObsidianActiveVaultSource", () => {
  it("registers every event before enumeration, sorts paths, and detaches exactly once", () => {
    const fake = new FakeVault();
    fake.files.set("z.md", file("z.md"));
    fake.files.set("folder/a.md", file("folder/a.md"));
    const active = source(fake);

    const stop = active.subscribe(() => undefined);
    expect(active.listMarkdownPaths()).toEqual(["folder/a.md", "z.md"]);
    expect(fake.calls.slice(0, 5)).toEqual([
      "on:create",
      "on:modify",
      "on:delete",
      "on:rename",
      "list",
    ]);

    stop();
    stop();
    expect(fake.calls.filter((call) => call === "offref")).toHaveLength(4);
  });

  it("maps file events to immutable path intents and rescans folder delete or rename", () => {
    const fake = new FakeVault();
    const events: VaultSourceEvent[] = [];
    source(fake).subscribe((event) => events.push(event));

    fake.emit("create", file("new.md"));
    fake.emit("modify", file("new.md"));
    fake.emit("rename", file("renamed.md"), "new.md");
    fake.emit("rename", file("renamed.txt"), "renamed.md");
    fake.emit("rename", file("added.md"), "added.txt");
    fake.emit("delete", file("added.md"));
    fake.emit("create", folder("ignored-folder"));
    fake.emit("delete", folder("deleted-folder"));
    fake.emit("rename", folder("renamed-folder"), "old-folder");

    expect(events).toEqual([
      { kind: "upsert", path: "new.md" },
      { kind: "upsert", path: "new.md" },
      { kind: "rename", oldPath: "new.md", path: "renamed.md" },
      { kind: "remove", path: "renamed.md" },
      { kind: "upsert", path: "added.md" },
      { kind: "remove", path: "added.md" },
      { kind: "rescan" },
      { kind: "rescan" },
    ]);
  });

  it("reads exact bytes and converts Obsidian millisecond timestamps", async () => {
    const fake = new FakeVault();
    const bytes = new TextEncoder().encode("# Snowman ☃\n");
    fake.files.set("unicode.md", file("unicode.md", bytes.byteLength, 1_234));
    fake.contents.set("unicode.md", bytes);
    const active = source(fake);
    const inspection = active.inspectMarkdown("unicode.md");
    expect(inspection).toEqual({
      kind: "candidate",
      path: "unicode.md",
      size: bytes.byteLength,
      mtime: 1_234,
    });
    if (inspection.kind !== "candidate") throw new Error("expected candidate");

    await expect(active.readMarkdown(inspection)).resolves.toMatchObject({
      kind: "source",
      source: {
        descriptor: {
          vault_id: ACTIVE_VAULT_ID,
          path: "unicode.md",
          format: "markdown",
          byte_length: bytes.byteLength,
          mtime: 1,
          mtime_nanos: "1234000000",
        },
        bytes,
      },
    });
  });

  it("preflights oversized notes without reading them", () => {
    const fake = new FakeVault();
    fake.files.set("large.md", file("large.md", MAX_INDEXABLE_SOURCE_BYTES + 1, 1));

    expect(source(fake).inspectMarkdown("large.md")).toEqual({
      kind: "oversized",
      path: "large.md",
    });
    expect(fake.readBinary).not.toHaveBeenCalled();
  });

  it("rejects bytes from a file that changes during the read", async () => {
    const fake = new FakeVault();
    fake.files.set("changing.md", file("changing.md", 3, 10));
    fake.contents.set("changing.md", new Uint8Array([1, 2, 3]));
    fake.onRead = () => {
      fake.files.set("changing.md", file("changing.md", 4, 11));
    };
    const active = source(fake);
    const inspection = active.inspectMarkdown("changing.md");
    if (inspection.kind !== "candidate") throw new Error("expected candidate");

    await expect(active.readMarkdown(inspection)).resolves.toEqual({
      kind: "stale",
      path: "changing.md",
    });
  });

  it("reads display text for excerpt hydration through cachedRead", async () => {
    const fake = new FakeVault();
    const bytes = new TextEncoder().encode("# Café ☃\nbody text\n");
    fake.files.set("unicode.md", file("unicode.md", bytes.byteLength, 7));
    fake.contents.set("unicode.md", bytes);

    await expect(source(fake).readExcerptText("unicode.md")).resolves.toEqual({
      kind: "text",
      path: "unicode.md",
      text: "# Café ☃\nbody text\n",
    });
    expect(fake.cachedRead).toHaveBeenCalledTimes(1);
    expect(fake.readBinary).not.toHaveBeenCalled();
  });

  it("reports missing and oversized notes without reading them for excerpts", async () => {
    const fake = new FakeVault();
    fake.files.set("large.md", file("large.md", MAX_INDEXABLE_SOURCE_BYTES + 1, 1));
    const active = source(fake);

    await expect(active.readExcerptText("gone.md")).resolves.toEqual({
      kind: "missing",
      path: "gone.md",
    });
    await expect(active.readExcerptText("large.md")).resolves.toEqual({
      kind: "oversized",
      path: "large.md",
    });
    expect(fake.cachedRead).not.toHaveBeenCalled();
  });

  // Hydration folds text synchronously on the Obsidian main thread, so its
  // input bound is far below the indexing bound: a note can be perfectly
  // indexable and still be refused as an excerpt source.
  it("refuses an excerpt source far below the indexable size bound", async () => {
    const fake = new FakeVault();
    const size = MAX_EXCERPT_SOURCE_BYTES + 1;
    expect(size).toBeLessThan(MAX_INDEXABLE_SOURCE_BYTES);
    fake.files.set("big.md", file("big.md", size, 3));
    fake.contents.set("big.md", new Uint8Array(size));
    const active = source(fake);

    expect(active.inspectMarkdown("big.md")).toMatchObject({ kind: "candidate" });
    await expect(active.readExcerptText("big.md")).resolves.toEqual({
      kind: "oversized",
      path: "big.md",
    });
    expect(fake.cachedRead).not.toHaveBeenCalled();
  });

  // cachedRead offers no staleness guarantee of its own, so the excerpt read
  // repeats the same stat sandwich the indexing read uses.
  it("rejects excerpt text from a file that changes during the read", async () => {
    const fake = new FakeVault();
    fake.files.set("changing.md", file("changing.md", 3, 10));
    fake.contents.set("changing.md", new TextEncoder().encode("abc"));
    fake.onRead = () => {
      fake.files.set("changing.md", file("changing.md", 4, 11));
    };

    await expect(source(fake).readExcerptText("changing.md")).resolves.toEqual({
      kind: "stale",
      path: "changing.md",
    });
  });
});
