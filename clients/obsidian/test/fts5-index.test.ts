// SPDX-FileCopyrightText: 2026 cybersader
// SPDX-License-Identifier: GPL-3.0-only

import sqlite3InitModule from "@sqlite.org/sqlite-wasm";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import {
  Fts5GenerationIndex,
  IndexCapacityError,
  openFts5Generation,
  type SQLiteApi,
  type SQLiteDatabase,
} from "../src/worker/fts5-index";
import type { SourcePreparation } from "../src/worker/rust-adapter";

let sqlite: SQLiteApi;
let index: Fts5GenerationIndex;
let consoleWarning: ReturnType<typeof vi.spyOn>;
let consoleFailure: ReturnType<typeof vi.spyOn>;

beforeEach(async () => {
  consoleWarning = vi.spyOn(console, "warn").mockImplementation(() => undefined);
  consoleFailure = vi.spyOn(console, "error").mockImplementation(() => undefined);
  const initializeSqlite = sqlite3InitModule as unknown as (options: {
    print: () => void;
    printErr: () => void;
  }) => Promise<SQLiteApi>;
  sqlite = await initializeSqlite({
    print: () => undefined,
    printErr: () => undefined,
  });
  index = openFts5Generation(sqlite);
});

afterEach(() => {
  index.close();
  consoleWarning.mockRestore();
  consoleFailure.mockRestore();
});

function source(
  sourceKey: string,
  chunkId: string,
  content: string,
  title = "Title",
): SourcePreparation {
  return sourceAt(sourceKey, `${sourceKey}.md`, chunkId, content, title);
}

function sourceAt(
  sourceKey: string,
  path: string,
  chunkId: string,
  content: string,
  title = "Title",
): SourcePreparation {
  return {
    schema_version: 1,
    source_key: sourceKey,
    vault_id: "active",
    path,
    format: "markdown",
    content_hash: `hash-${sourceKey}`,
    byte_length: content.length,
    mtime: 1,
    mtime_nanos: "1000001",
    retrieval: {
      filename: path,
      stem: sourceKey,
      aliases: [],
    },
    chunks: [{
      chunk: {
        chunk_id: chunkId,
        vault_id: "active",
        room: null,
        path: path,
        heading_path: ["Heading"],
        content,
        frontmatter: { title, tags: ["test"] },
        links_out: [],
        mtime: 1,
        content_hash: `hash-${sourceKey}`,
        chunking_version: 1,
      },
      heading_text: "Heading",
      technical_identifiers: [],
    }],
    kind: "indexed",
  };
}

const anyPlan = (term: string) => ({
  schema_version: 1 as const,
  plan_id: "lexical_any_v1" as const,
  match_value: `"${term}"`,
});

describe("Fts5GenerationIndex", () => {
  it("indexes prepared chunks, probes metadata, and returns stored result identity", () => {
    index.replaceSource(source("alpha", "chunk-a", "portable quasar text", "Quasar Guide"));
    expect(index.documents).toBe(1);
    expect(index.chunks).toBe(1);
    expect(index.metadataProbe({
      schema_version: 1,
      plan_id: "metadata_probe_v1",
      match_value: "{filename stem aliases title heading_text} : (\"quasar\")",
    })).toBe(true);

    const hits = index.search(anyPlan("quasar"), 20);
    expect(hits).toHaveLength(1);
    expect(hits[0]).toMatchObject({
      chunk_id: "chunk-a",
      vault_id: "active",
      path: "alpha.md",
      heading_path: ["Heading"],
      frontmatter: { title: "Quasar Guide" },
    });
    // The index is contentless: it can rank and locate, but it stores no text
    // and must not pretend to have produced excerpt text.
    expect(hits[0]!.excerpt).toBe("");
  });

  it("stores no indexed field text and keeps phrase and column queries working", () => {
    const db = new sqlite.oo1.DB(":memory:", "c");
    const scoped = new Fts5GenerationIndex(db);
    try {
      scoped.replaceSource(source("alpha", "chunk-a", "portable quasar cache storage", "Quasar Guide"));

      expect(db.selectValue(
        "SELECT count(*) FROM pragma_table_info('chunks') WHERE name = 'content'",
      )).toBe(0);
      expect(db.selectValue("SELECT content FROM chunks_fts WHERE rowid = 1")).toBe(null);
      expect(db.selectValue(
        "SELECT snippet(chunks_fts, 7, '[', ']', '…', 8) FROM chunks_fts WHERE chunks_fts MATCH ?",
        ['"quasar"'],
      )).toBe(null);

      expect(scoped.search({
        schema_version: 1,
        plan_id: "lexical_explicit_v1",
        match_value: "\"quasar cache\"",
      }, 20)).toHaveLength(1);
      expect(scoped.search({
        schema_version: 1,
        plan_id: "lexical_explicit_v1",
        match_value: "\"cache quasar\"",
      }, 20)).toEqual([]);
      expect(scoped.search({
        schema_version: 1,
        plan_id: "lexical_explicit_v1",
        match_value: "title : \"quasar\"",
      }, 20)).toHaveLength(1);
      expect(scoped.search({
        schema_version: 1,
        plan_id: "lexical_explicit_v1",
        match_value: "tags : \"quasar\"",
      }, 20)).toEqual([]);
    } finally {
      scoped.close();
    }
  });

  it("deletes contentless postings when a source is replaced or removed", () => {
    const db = new sqlite.oo1.DB(":memory:", "c");
    const scoped = new Fts5GenerationIndex(db);
    try {
      scoped.replaceSource(source("alpha", "chunk-a", "firstterm"));
      expect(db.selectValue("SELECT count(*) FROM chunks_fts")).toBe(1);

      // Replacement frees rowid 1; the replacement must not reuse it while a
      // stale posting could still exist, and the old posting must be gone.
      scoped.replaceSource(source("alpha", "chunk-b", "secondterm"));
      expect(db.selectValue("SELECT count(*) FROM chunks_fts")).toBe(1);
      expect(scoped.search(anyPlan("firstterm"), 20)).toEqual([]);
      expect(db.selectValue("SELECT min(rowid) FROM chunks")).toBe(2);

      scoped.applySourceChanges([], [{ vault_id: "active", path: "alpha.md" }], true);
      expect(db.selectValue("SELECT count(*) FROM chunks_fts")).toBe(0);
      expect(db.selectValue("SELECT count(*) FROM chunks")).toBe(0);
      expect(scoped.search(anyPlan("secondterm"), 20)).toEqual([]);
      expect(() => scoped.assertIntegrity()).not.toThrow();
    } finally {
      scoped.close();
    }
  });

  it("atomically replaces a source and preserves it after a failed replacement", () => {
    index.replaceSource(source("alpha", "chunk-a", "firstterm"));
    index.replaceSource(source("alpha", "chunk-b", "secondterm"));
    expect(index.search(anyPlan("firstterm"), 20)).toEqual([]);
    expect(index.search(anyPlan("secondterm"), 20)).toHaveLength(1);

    const invalid = source("alpha", "duplicate", "thirdterm");
    invalid.chunks.push(structuredClone(invalid.chunks[0]!));
    expect(() => index.replaceSource(invalid)).toThrow();
    expect(index.search(anyPlan("secondterm"), 20)).toHaveLength(1);
    expect(index.documents).toBe(1);
    expect(index.chunks).toBe(1);
  });

  it("applies mixed removals and upserts in one batch", () => {
    index.applySourceChanges([
      source("alpha", "chunk-a", "alphaterm"),
      source("beta", "chunk-b", "betaterm"),
    ], []);

    index.applySourceChanges(
      [source("gamma", "chunk-g", "gammaterm")],
      [{ vault_id: "active", path: "beta.md" }],
      true,
    );

    expect(index.documents).toBe(2);
    expect(index.chunks).toBe(2);
    expect(index.search(anyPlan("alphaterm"), 20)).toHaveLength(1);
    expect(index.search(anyPlan("betaterm"), 20)).toEqual([]);
    expect(index.search(anyPlan("gammaterm"), 20)).toHaveLength(1);
  });

  it("renames by removing the old identity and inserting the new source", () => {
    index.replaceSource(source("alpha", "chunk-a", "oldterm"));
    index.applySourceChanges(
      [sourceAt("renamed-key", "renamed.md", "chunk-new", "newterm")],
      [{ vault_id: "active", path: "alpha.md" }],
      true,
    );

    expect(index.documents).toBe(1);
    expect(index.search(anyPlan("oldterm"), 20)).toEqual([]);
    expect(index.search(anyPlan("newterm"), 20)[0]).toMatchObject({ path: "renamed.md" });
  });

  it("rolls back every source when a later insert collides", () => {
    index.replaceSource(source("alpha", "chunk-a", "stableterm"));
    expect(() => index.applySourceChanges([
      source("beta", "chunk-b", "validterm"),
      source("gamma", "chunk-a", "collisionterm"),
    ], [], true)).toThrow();

    expect(index.documents).toBe(1);
    expect(index.chunks).toBe(1);
    expect(index.search(anyPlan("stableterm"), 20)).toHaveLength(1);
    expect(index.search(anyPlan("validterm"), 20)).toEqual([]);
    expect(index.search(anyPlan("collisionterm"), 20)).toEqual([]);
  });

  it("rejects a capacity overflow without changing rows or counters", () => {
    index.close();
    index = openFts5Generation(sqlite, {
      maxChunks: 1,
      maxIndexedTextBytes: 1_024,
    });
    index.replaceSource(source("alpha", "chunk-a", "stableterm"));

    expect(() => index.applySourceChanges([
      source("beta", "chunk-b", "overflowterm"),
    ], [], true)).toThrow(IndexCapacityError);

    expect(index.documents).toBe(1);
    expect(index.chunks).toBe(1);
    expect(index.search(anyPlan("stableterm"), 20)).toHaveLength(1);
    expect(index.search(anyPlan("overflowterm"), 20)).toEqual([]);
  });

  it("distinguishes indexed empty documents from skipped sources", () => {
    const empty = source("empty", "unused", "");
    empty.chunks = [];
    index.replaceSource(empty);
    expect(index.documents).toBe(1);
    expect(index.chunks).toBe(0);

    const skipped = structuredClone(empty);
    skipped.kind = "skipped";
    skipped.content_hash = null;
    index.replaceSource(skipped);
    expect(index.documents).toBe(0);
    expect(index.chunks).toBe(0);
  });

  it("treats a missing removal as an idempotent no-op", () => {
    index.replaceSource(source("alpha", "chunk-a", "stableterm"));
    index.applySourceChanges([], [{ vault_id: "active", path: "missing.md" }], true);
    expect(index.documents).toBe(1);
    expect(index.chunks).toBe(1);
    expect(index.search(anyPlan("stableterm"), 20)).toHaveLength(1);
  });

  it("passes the FTS5 integrity check and closes idempotently", () => {
    index.replaceSource(source("alpha", "chunk-a", "quasar"));
    expect(() => index.assertIntegrity()).not.toThrow();
    index.close();
    expect(() => index.close()).not.toThrow();
  });

  // Negative control for the publish gate. On a contentless table the FTS5
  // 'integrity-check' command can only inspect the index internally, so it
  // stays green through either desync direction; only the explicit
  // reconciliation can fail, and it must.
  it("fails the publish integrity gate when the index and metadata desync", () => {
    const orphaned = new sqlite.oo1.DB(":memory:", "c");
    const withOrphanFts = new Fts5GenerationIndex(orphaned);
    try {
      withOrphanFts.replaceSource(source("alpha", "chunk-a", "quasar"));
      orphaned.exec("DELETE FROM chunks WHERE rowid = 1");
      expect(() => orphaned.exec(
        "INSERT INTO chunks_fts(chunks_fts, rank) VALUES('integrity-check', 1)",
      )).not.toThrow();
      expect(() => withOrphanFts.assertIntegrity()).toThrow(/integrity check failed/);
    } finally {
      withOrphanFts.close();
    }

    const missing = new sqlite.oo1.DB(":memory:", "c");
    const withMissingFts = new Fts5GenerationIndex(missing);
    try {
      withMissingFts.replaceSource(source("alpha", "chunk-a", "quasar"));
      missing.exec("DELETE FROM chunks_fts WHERE rowid = 1");
      expect(() => missing.exec(
        "INSERT INTO chunks_fts(chunks_fts, rank) VALUES('integrity-check', 1)",
      )).not.toThrow();
      expect(() => withMissingFts.assertIntegrity()).toThrow(/integrity check failed/);
    } finally {
      withMissingFts.close();
    }
  });

  it("fails the publish integrity gate when stored rows disagree with the counters", () => {
    const db = new sqlite.oo1.DB(":memory:", "c");
    const scoped = new Fts5GenerationIndex(db);
    try {
      scoped.replaceSource(source("alpha", "chunk-a", "quasar"));
      db.exec("DELETE FROM chunks WHERE rowid = 1");
      db.exec("DELETE FROM chunks_fts WHERE rowid = 1");
      expect(scoped.chunks).toBe(1);
      expect(() => scoped.assertIntegrity()).toThrow(/integrity check failed/);
    } finally {
      scoped.close();
    }
  });

  it("optimizes and vacuums a generation without changing its results", () => {
    const db = new sqlite.oo1.DB(":memory:", "c");
    const scoped = new Fts5GenerationIndex(db);
    try {
      scoped.replaceSource(source("alpha", "chunk-a", "quasar"));
      scoped.replaceSource(source("beta", "chunk-b", "quasar"));
      scoped.applySourceChanges([], [{ vault_id: "active", path: "beta.md" }]);
      const before = db.selectValue("SELECT count(*) FROM chunks_fts");

      expect(() => scoped.compact()).not.toThrow();

      expect(db.selectValue("SELECT count(*) FROM chunks_fts")).toBe(before);
      expect(scoped.search(anyPlan("quasar"), 20)).toHaveLength(1);
      expect(() => scoped.assertIntegrity()).not.toThrow();
    } finally {
      scoped.close();
    }
  });

  // "Results are unchanged" is satisfied by an empty compact(); the ruling
  // requires the two statements themselves, in order, outside a transaction.
  it("issues exactly the FTS optimize and then VACUUM when compacting", () => {
    const recorder = new RecordingDatabase(new sqlite.oo1.DB(":memory:", "c"));
    const scoped = new Fts5GenerationIndex(recorder);
    try {
      scoped.replaceSource(source("alpha", "chunk-a", "quasar"));
      recorder.statements.length = 0;

      scoped.compact();

      expect(recorder.statements).toEqual([
        "INSERT INTO chunks_fts(chunks_fts) VALUES('optimize')",
        "VACUUM",
      ]);
      expect(recorder.openTransactions).toBe(0);
    } finally {
      scoped.close();
    }
  });

  // The observable half: VACUUM must actually rebuild the published image
  // without the pages a churned build freed.
  it("reclaims the pages a churned generation freed before it is published", () => {
    const db = new sqlite.oo1.DB(":memory:", "c");
    const scoped = new Fts5GenerationIndex(db);
    try {
      const filler = "quasarterm pulsarterm nebulaterm lorem ipsum dolor sit amet ".repeat(24);
      for (let index = 0; index < 40; index += 1) {
        scoped.replaceSource(source(`note-${index}`, `chunk-${index}`, `${filler} ${index}`));
      }
      scoped.applySourceChanges(
        [],
        Array.from({ length: 38 }, (_unused, index) => ({
          vault_id: "active",
          path: `note-${index}.md`,
        })),
      );

      const pagesBefore = Number(db.selectValue("PRAGMA page_count"));
      expect(Number(db.selectValue("PRAGMA freelist_count"))).toBeGreaterThan(0);

      scoped.compact();

      expect(Number(db.selectValue("PRAGMA freelist_count"))).toBe(0);
      expect(Number(db.selectValue("PRAGMA page_count"))).toBeLessThan(pagesBefore);
      expect(scoped.chunks).toBe(2);
      expect(() => scoped.assertIntegrity()).not.toThrow();
    } finally {
      scoped.close();
    }
  });

  // Seeding the allocator from `chunks` alone would hand rowid 1 straight back
  // to the new chunk, shadowing the stray posting: the contentless table
  // accepts the duplicate silently and reconciliation then sees matching
  // counts and no orphan, so the corruption becomes permanently invisible.
  it("allocates chunk rowids above an FTS-only posting so it stays visible", () => {
    const db = new sqlite.oo1.DB(":memory:", "c");
    const scoped = new Fts5GenerationIndex(db);
    try {
      db.exec("INSERT INTO chunks_fts(rowid, content) VALUES(1, 'orphanterm')");

      scoped.replaceSource(source("alpha", "chunk-a", "quasar"));

      expect(db.selectValue("SELECT min(rowid) FROM chunks")).toBe(2);
      expect(db.selectValue("SELECT count(*) FROM chunks_fts")).toBe(2);
      expect(() => scoped.assertIntegrity()).toThrow(/integrity check failed/);
    } finally {
      scoped.close();
    }
  });

  // A generation that is already published has no later commit gate, so its
  // caller asks for the reconciliation inline and a divergence must roll the
  // whole batch back rather than land in a live index.
  it("reconciles inside the transaction on request and rolls a diverged batch back", () => {
    const db = new sqlite.oo1.DB(":memory:", "c");
    const scoped = new Fts5GenerationIndex(db);
    try {
      scoped.replaceSource(source("alpha", "chunk-a", "quasar"));
      db.exec("INSERT INTO chunks_fts(rowid, content) VALUES(9000, 'orphanterm')");

      // Unverified, the divergence is carried straight through.
      scoped.applySourceChanges([source("beta", "chunk-b", "pulsarterm")], []);
      expect(db.selectValue("SELECT count(*) FROM sources")).toBe(2);
      expect(scoped.search(anyPlan("pulsarterm"), 20)).toHaveLength(1);

      // Verified, the same batch is refused and nothing of it survives.
      expect(() => scoped.applySourceChanges(
        [source("gamma", "chunk-c", "nebulaterm")],
        [],
        true,
      )).toThrow(/chunk metadata disagree/);
      expect(db.selectValue("SELECT count(*) FROM sources")).toBe(2);
      expect(db.selectValue("SELECT count(*) FROM chunks WHERE chunk_id = 'chunk-c'")).toBe(0);
      expect(scoped.search(anyPlan("nebulaterm"), 20)).toEqual([]);
    } finally {
      scoped.close();
    }
  });
});

/** Delegating wrapper that records the statements a generation issues. */
class RecordingDatabase implements SQLiteDatabase {
  readonly statements: string[] = [];
  openTransactions = 0;

  constructor(private readonly inner: SQLiteDatabase) {}

  get filename(): string {
    return this.inner.filename;
  }

  get pointer(): unknown | undefined {
    return this.inner.pointer;
  }

  exec(sql: string, options?: { bind?: readonly unknown[] }): unknown {
    this.statements.push(sql);
    // oo1 rejects an explicit `undefined` options argument.
    return options === undefined ? this.inner.exec(sql) : this.inner.exec(sql, options);
  }

  selectValue(sql: string, bind?: readonly unknown[]): unknown {
    return this.inner.selectValue(sql, bind);
  }

  selectObjects(sql: string, bind?: readonly unknown[]): Record<string, unknown>[] {
    return this.inner.selectObjects(sql, bind);
  }

  transaction<T>(qualifier: "IMMEDIATE", callback: () => T): T {
    this.openTransactions += 1;
    try {
      return this.inner.transaction(qualifier, callback);
    } finally {
      this.openTransactions -= 1;
    }
  }

  close(): void {
    this.inner.close();
  }
}
