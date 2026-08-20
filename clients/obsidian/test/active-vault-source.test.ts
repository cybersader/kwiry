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
import {
  DEFAULT_ENABLED_SOURCE_FORMATS,
  type EnabledSourceFormats,
} from "../src/source-formats";

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

  getFiles(): TFile[] {
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

function source(
  fake: FakeVault,
  enabled: Readonly<EnabledSourceFormats> = DEFAULT_ENABLED_SOURCE_FORMATS,
  readTimeoutMs?: number,
): ObsidianActiveVaultSource {
  return new ObsidianActiveVaultSource(fake as unknown as Vault, enabled, readTimeoutMs);
}

describe("ObsidianActiveVaultSource", () => {
  it("registers every event before enumeration, sorts paths, and detaches exactly once", () => {
    const fake = new FakeVault();
    fake.files.set("z.md", file("z.md"));
    fake.files.set("folder/a.base", file("folder/a.base"));
    fake.files.set("notes.txt", file("notes.txt"));
    fake.files.set("board.canvas", file("board.canvas"));
    fake.files.set("ignored.png", file("ignored.png"));
    const active = source(fake);

    const stop = active.subscribe(() => undefined);
    expect(active.listSourcePaths()).toEqual([
      "board.canvas",
      "folder/a.base",
      "notes.txt",
      "z.md",
    ]);
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

  it("honors per-format admission toggles for inventory, inspection, and events", () => {
    const fake = new FakeVault();
    fake.files.set("note.md", file("note.md"));
    fake.files.set("query.base", file("query.base"));
    fake.files.set("notes.txt", file("notes.txt"));
    const enabled: EnabledSourceFormats = {
      ...DEFAULT_ENABLED_SOURCE_FORMATS,
      text: false,
      base: false,
    };
    const events: VaultSourceEvent[] = [];
    const active = source(fake, enabled);
    active.subscribe((event) => events.push(event));

    expect(active.listSourcePaths()).toEqual(["note.md"]);
    expect(active.inspectSource("notes.txt")).toEqual({ kind: "missing", path: "notes.txt" });
    fake.emit("modify", file("notes.txt"));
    fake.emit("modify", file("note.md"));
    expect(events).toEqual([{ kind: "upsert", path: "note.md" }]);
  });

  // PDF is extractable now, so admission is decided by the toggle alone. The
  // default is off, and a default-off format must be inert in exactly the way
  // an unextractable one used to be: not listed, not inspected, not emitted,
  // and above all not read.
  it("never inventories, inspects, emits, or reads PDF files while PDF is off", async () => {
    const fake = new FakeVault();
    fake.files.set("paper.PDF", file("paper.PDF", 3, 2));
    fake.contents.set("paper.PDF", new Uint8Array([5, 6, 7]));
    const events: VaultSourceEvent[] = [];
    expect(DEFAULT_ENABLED_SOURCE_FORMATS.pdf).toBe(false);
    const active = source(fake, { ...DEFAULT_ENABLED_SOURCE_FORMATS });
    active.subscribe((event) => events.push(event));

    expect(active.listSourcePaths()).toEqual([]);
    expect(active.inspectSource("paper.PDF")).toEqual({ kind: "missing", path: "paper.PDF" });
    await expect(active.readSource({
      kind: "candidate",
      path: "paper.PDF",
      format: "pdf",
      size: 3,
      mtime: 2,
    })).resolves.toEqual({ kind: "missing", path: "paper.PDF" });
    fake.emit("rename", file("renamed.pdf"), "paper.PDF");
    fake.emit("delete", file("renamed.pdf"));

    expect(events).toEqual([]);
    expect(fake.readBinary).not.toHaveBeenCalled();
  });

  it("inventories, inspects, emits, and reads PDF files once the format is on", async () => {
    const fake = new FakeVault();
    fake.files.set("paper.PDF", file("paper.PDF", 3, 2));
    fake.contents.set("paper.PDF", new Uint8Array([5, 6, 7]));
    const events: VaultSourceEvent[] = [];
    const enabled: EnabledSourceFormats = { ...DEFAULT_ENABLED_SOURCE_FORMATS, pdf: true };
    const active = source(fake, enabled);
    active.subscribe((event) => events.push(event));

    expect(active.listSourcePaths()).toEqual(["paper.PDF"]);
    expect(active.inspectSource("paper.PDF")).toEqual({
      kind: "candidate",
      path: "paper.PDF",
      format: "pdf",
      size: 3,
      mtime: 2,
    });
    await expect(active.readSource({
      kind: "candidate",
      path: "paper.PDF",
      format: "pdf",
      size: 3,
      mtime: 2,
    })).resolves.toEqual({
      kind: "source",
      source: {
        descriptor: {
          vault_id: "active-vault",
          path: "paper.PDF",
          format: "pdf",
          byte_length: 3,
          mtime: 0,
          mtime_nanos: "2000000",
        },
        bytes: new Uint8Array([5, 6, 7]),
      },
    });
    fake.emit("modify", file("paper.PDF"));
    expect(events).toEqual([{ kind: "upsert", path: "paper.PDF" }]);
  });

  it("admits HTML and HTM case-insensitively while rejecting XHTML and wrapper suffixes", async () => {
    const fake = new FakeVault();
    const htmlBytes = new TextEncoder().encode("<title>Portal</title><p>Body</p>");
    fake.files.set("site/Page.HTML", file("site/Page.HTML", htmlBytes.byteLength, 4));
    fake.contents.set("site/Page.HTML", htmlBytes);
    fake.files.set("site/legacy.htm", file("site/legacy.htm", htmlBytes.byteLength, 5));
    fake.contents.set("site/legacy.htm", htmlBytes);
    fake.files.set("site/page.xhtml", file("site/page.xhtml", htmlBytes.byteLength, 6));
    fake.files.set("site/archive.html.md", file("site/archive.html.md", htmlBytes.byteLength, 7));
    fake.contents.set("site/archive.html.md", htmlBytes);
    const active = source(fake);

    expect(active.listSourcePaths()).toEqual([
      "site/Page.HTML",
      "site/archive.html.md",
      "site/legacy.htm",
    ]);
    expect(active.inspectSource("site/Page.HTML")).toMatchObject({
      kind: "candidate",
      format: "html",
    });
    expect(active.inspectSource("site/legacy.htm")).toMatchObject({
      kind: "candidate",
      format: "html",
    });
    expect(active.inspectSource("site/page.xhtml")).toEqual({
      kind: "missing",
      path: "site/page.xhtml",
    });
    expect(active.inspectSource("site/archive.html.md")).toMatchObject({
      kind: "candidate",
      format: "markdown",
    });

    const inspection = active.inspectSource("site/legacy.htm");
    if (inspection.kind !== "candidate") throw new Error("expected HTML candidate");
    await expect(active.readSource(inspection)).resolves.toMatchObject({
      kind: "source",
      source: {
        descriptor: { path: "site/legacy.htm", format: "html" },
        bytes: htmlBytes,
      },
    });
  });

  it("inventories, inspects, emits, and reads admitted DOCX files", async () => {
    const fake = new FakeVault();
    fake.files.set("report.docx", file("report.docx", 4, 1));
    fake.contents.set("report.docx", new Uint8Array([1, 2, 3, 4]));
    const events: VaultSourceEvent[] = [];
    const active = source(fake, { ...DEFAULT_ENABLED_SOURCE_FORMATS, docx: true });
    active.subscribe((event) => events.push(event));

    expect(active.listSourcePaths()).toEqual(["report.docx"]);
    expect(active.inspectSource("report.docx")).toEqual({
      kind: "candidate",
      path: "report.docx",
      format: "docx",
      size: 4,
      mtime: 1,
    });
    await expect(active.readSource({
      kind: "candidate",
      path: "report.docx",
      format: "docx",
      size: 4,
      mtime: 1,
    })).resolves.toEqual({
      kind: "source",
      source: {
        descriptor: {
          vault_id: "active-vault",
          path: "report.docx",
          format: "docx",
          byte_length: 4,
          mtime: 0,
          mtime_nanos: "1000000",
        },
        bytes: new Uint8Array([1, 2, 3, 4]),
      },
    });
    fake.emit("modify", file("report.docx"));
    expect(events).toEqual([{ kind: "upsert", path: "report.docx" }]);
  });

  it("maps file events to immutable path intents and rescans folder delete or rename", () => {
    const fake = new FakeVault();
    const events: VaultSourceEvent[] = [];
    source(fake).subscribe((event) => events.push(event));

    fake.emit("create", file("new.md"));
    fake.emit("modify", file("new.md"));
    fake.emit("rename", file("renamed.base"), "new.md");
    fake.emit("rename", file("renamed.png"), "renamed.base");
    fake.emit("rename", file("added.txt"), "added.png");
    fake.emit("delete", file("added.txt"));
    fake.emit("create", folder("ignored-folder"));
    fake.emit("delete", folder("deleted-folder"));
    fake.emit("rename", folder("renamed-folder"), "old-folder");

    expect(events).toEqual([
      { kind: "upsert", path: "new.md" },
      { kind: "upsert", path: "new.md" },
      { kind: "rename", oldPath: "new.md", path: "renamed.base" },
      { kind: "remove", path: "renamed.base" },
      { kind: "upsert", path: "added.txt" },
      { kind: "remove", path: "added.txt" },
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
    const inspection = active.inspectSource("unicode.md");
    expect(inspection).toEqual({
      kind: "candidate",
      path: "unicode.md",
      format: "markdown",
      size: bytes.byteLength,
      mtime: 1_234,
    });
    if (inspection.kind !== "candidate") throw new Error("expected candidate");

    await expect(active.readSource(inspection)).resolves.toMatchObject({
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
    expect(fake.readBinary).toHaveBeenCalledTimes(1);
  });

  it("times out a never-resolving read without overlapping it and ignores late success", async () => {
    vi.useFakeTimers();
    try {
      const fake = new FakeVault();
      const bytes = new Uint8Array([1, 2, 3]);
      fake.files.set("slow.md", file("slow.md", bytes.byteLength, 10));
      fake.contents.set("slow.md", bytes);
      let release: ((buffer: ArrayBuffer) => void) | undefined;
      fake.readBinary.mockImplementationOnce(() => new Promise<ArrayBuffer>((resolve) => {
        release = resolve;
      }));
      const active = source(fake, DEFAULT_ENABLED_SOURCE_FORMATS, 25);
      const inspection = active.inspectSource("slow.md");
      if (inspection.kind !== "candidate") throw new Error("expected candidate");

      const first = active.readSource(inspection);
      await vi.advanceTimersByTimeAsync(25);
      const timedOut = await first;
      expect(timedOut).toMatchObject({ kind: "timeout" });
      if (timedOut.kind !== "timeout") throw new Error("expected timeout");
      const repeated = await active.readSource(inspection);
      expect(repeated).toMatchObject({ kind: "timeout" });
      if (repeated.kind !== "timeout") throw new Error("expected repeated timeout");
      expect(repeated.underlyingSettled).toBe(timedOut.underlyingSettled);
      expect(fake.readBinary).toHaveBeenCalledTimes(1);

      let underlyingDidSettle = false;
      void timedOut.underlyingSettled.then(() => {
        underlyingDidSettle = true;
      });
      await Promise.resolve();
      expect(underlyingDidSettle).toBe(false);
      release?.(new Uint8Array(bytes).buffer);
      await timedOut.underlyingSettled;
      await expect(first).resolves.toBe(timedOut);
      await expect(active.readSource(inspection)).resolves.toMatchObject({ kind: "source" });
      expect(fake.readBinary).toHaveBeenCalledTimes(2);
    } finally {
      vi.useRealTimers();
    }
  });

  it("releases a timed-out read under its original path after a concurrent rename", async () => {
    vi.useFakeTimers();
    try {
      const fake = new FakeVault();
      const bytes = new Uint8Array([1, 2, 3]);
      const sourceFile = file("slow.md", bytes.byteLength, 10);
      fake.files.set("slow.md", sourceFile);
      fake.contents.set("slow.md", bytes);
      let release: ((buffer: ArrayBuffer) => void) | undefined;
      fake.readBinary.mockImplementationOnce(() => new Promise<ArrayBuffer>((resolve) => {
        release = resolve;
      }));
      const active = source(fake, DEFAULT_ENABLED_SOURCE_FORMATS, 25);
      const inspection = active.inspectSource("slow.md");
      if (inspection.kind !== "candidate") throw new Error("expected candidate");

      const first = active.readSource(inspection);
      await vi.advanceTimersByTimeAsync(25);
      const timedOut = await first;
      expect(timedOut).toMatchObject({ kind: "timeout" });
      if (timedOut.kind !== "timeout") throw new Error("expected timeout");

      sourceFile.path = "renamed.md";
      release?.(new Uint8Array(bytes).buffer);
      await timedOut.underlyingSettled;
      sourceFile.path = "slow.md";

      await expect(active.readSource(inspection)).resolves.toMatchObject({ kind: "source" });
      expect(fake.readBinary).toHaveBeenCalledTimes(2);
    } finally {
      vi.useRealTimers();
    }
  });

  it("preserves ordinary read failures as failures rather than timeouts", async () => {
    const fake = new FakeVault();
    fake.files.set("failed.md", file("failed.md", 3, 10));
    fake.readBinary.mockRejectedValueOnce(new Error("read failed"));
    const active = source(fake, DEFAULT_ENABLED_SOURCE_FORMATS, 25);
    const inspection = active.inspectSource("failed.md");
    if (inspection.kind !== "candidate") throw new Error("expected candidate");

    await expect(active.readSource(inspection)).rejects.toThrow("read failed");
  });

  it("classifies Base and plain text sources into their descriptors", async () => {
    const fake = new FakeVault();
    const baseBytes = new TextEncoder().encode("views:\n  - name: Active\n");
    const textBytes = new TextEncoder().encode("plain text\n");
    fake.files.set("query.base", file("query.base", baseBytes.byteLength, 2_000));
    fake.contents.set("query.base", baseBytes);
    fake.files.set("notes.txt", file("notes.txt", textBytes.byteLength, 3_000));
    fake.contents.set("notes.txt", textBytes);
    const active = source(fake);

    const baseInspection = active.inspectSource("query.base");
    const textInspection = active.inspectSource("notes.txt");
    expect(baseInspection).toMatchObject({ kind: "candidate", format: "base" });
    expect(textInspection).toMatchObject({ kind: "candidate", format: "text" });
    if (baseInspection.kind !== "candidate" || textInspection.kind !== "candidate") {
      throw new Error("expected candidate sources");
    }

    await expect(active.readSource(baseInspection)).resolves.toMatchObject({
      kind: "source",
      source: { descriptor: { path: "query.base", format: "base" }, bytes: baseBytes },
    });
    await expect(active.readSource(textInspection)).resolves.toMatchObject({
      kind: "source",
      source: { descriptor: { path: "notes.txt", format: "text" }, bytes: textBytes },
    });
  });

  it("preflights oversized notes without reading them", () => {
    const fake = new FakeVault();
    fake.files.set("large.md", file("large.md", MAX_INDEXABLE_SOURCE_BYTES + 1, 1));

    expect(source(fake).inspectSource("large.md")).toEqual({
      kind: "oversized",
      path: "large.md",
      format: "markdown",
      size: MAX_INDEXABLE_SOURCE_BYTES + 1,
      mtime: 1,
    });
    expect(fake.readBinary).not.toHaveBeenCalled();
  });

  it("reports a candidate that disappears before reading as missing without reading bytes", async () => {
    const fake = new FakeVault();
    fake.files.set("gone.md", file("gone.md", 3, 10));
    const active = source(fake);
    const inspection = active.inspectSource("gone.md");
    if (inspection.kind !== "candidate") throw new Error("expected candidate");
    fake.files.delete("gone.md");

    await expect(active.readSource(inspection)).resolves.toEqual({
      kind: "missing",
      path: "gone.md",
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
    const inspection = active.inspectSource("changing.md");
    if (inspection.kind !== "candidate") throw new Error("expected candidate");

    await expect(active.readSource(inspection)).resolves.toEqual({
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

    expect(active.inspectSource("big.md")).toMatchObject({ kind: "candidate" });
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
