// SPDX-FileCopyrightText: 2026 cybersader
// SPDX-License-Identifier: GPL-3.0-only

import { FTS_HIGHLIGHT_END, FTS_HIGHLIGHT_START } from "../excerpt";
import type { WorkerSearchHit } from "./protocol";
import { bindMetadataProbe, bindSearchPlan } from "./query-binder";
import type {
  MatchPlan,
  MetadataProbePlan,
  PreparedChunk,
  SourcePreparation,
} from "./rust-adapter";

const MAX_INDEX_CHUNKS = 100_000;
const MAX_INDEXED_TEXT_BYTES = 256 * 1024 * 1024;

export interface SQLiteDatabase {
  readonly filename: string;
  readonly pointer: unknown | undefined;
  exec(sql: string, options?: { bind?: readonly unknown[] }): unknown;
  selectValue(sql: string, bind?: readonly unknown[]): unknown;
  selectObjects(sql: string, bind?: readonly unknown[]): Record<string, unknown>[];
  transaction<T>(qualifier: "IMMEDIATE", callback: () => T): T;
  close(): void;
}

export interface SQLiteApi {
  oo1: {
    DB: new (filename: string, flags: string) => SQLiteDatabase;
  };
}

const SCHEMA_SQL = `
CREATE TABLE sources (
  source_key TEXT PRIMARY KEY,
  vault_id TEXT NOT NULL,
  path TEXT NOT NULL,
  byte_length INTEGER NOT NULL
);

CREATE TABLE chunks (
  rowid INTEGER PRIMARY KEY,
  source_key TEXT NOT NULL,
  chunk_id TEXT NOT NULL UNIQUE,
  vault_id TEXT NOT NULL,
  path TEXT NOT NULL,
  heading_path_json TEXT NOT NULL,
  frontmatter_json TEXT NOT NULL,
  mtime INTEGER NOT NULL,
  content_hash TEXT NOT NULL,
  chunking_version INTEGER NOT NULL,
  filename TEXT NOT NULL,
  stem TEXT NOT NULL,
  aliases TEXT NOT NULL,
  title TEXT NOT NULL,
  heading_text TEXT NOT NULL,
  path_text TEXT NOT NULL,
  tags TEXT NOT NULL,
  content TEXT NOT NULL,
  identifiers TEXT NOT NULL
);

CREATE INDEX chunks_by_source ON chunks(source_key);

CREATE VIRTUAL TABLE chunks_fts USING fts5(
  filename,
  stem,
  aliases,
  title,
  heading_text,
  path_text,
  tags,
  content,
  identifiers,
  content='chunks',
  content_rowid='rowid',
  tokenize='unicode61 remove_diacritics 2'
);

CREATE TRIGGER chunks_ai AFTER INSERT ON chunks BEGIN
  INSERT INTO chunks_fts(
    rowid, filename, stem, aliases, title, heading_text,
    path_text, tags, content, identifiers
  ) VALUES (
    new.rowid, new.filename, new.stem, new.aliases, new.title,
    new.heading_text, new.path_text, new.tags, new.content, new.identifiers
  );
END;

CREATE TRIGGER chunks_ad AFTER DELETE ON chunks BEGIN
  INSERT INTO chunks_fts(
    chunks_fts, rowid, filename, stem, aliases, title, heading_text,
    path_text, tags, content, identifiers
  ) VALUES (
    'delete', old.rowid, old.filename, old.stem, old.aliases, old.title,
    old.heading_text, old.path_text, old.tags, old.content, old.identifiers
  );
END;
`;

const INSERT_SOURCE_SQL = `
INSERT INTO sources(source_key, vault_id, path, byte_length)
VALUES(?, ?, ?, ?)
ON CONFLICT(source_key) DO UPDATE SET
  vault_id = excluded.vault_id,
  path = excluded.path,
  byte_length = excluded.byte_length
`;

const INSERT_CHUNK_SQL = `
INSERT INTO chunks(
  source_key, chunk_id, vault_id, path, heading_path_json,
  frontmatter_json, mtime, content_hash, chunking_version,
  filename, stem, aliases, title, heading_text, path_text,
  tags, content, identifiers
) VALUES(?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
`;

export class Fts5GenerationIndex {
  private readonly sourceBytes = new Map<string, number>();
  private corpusBytes = 0;
  private chunkCount = 0;
  private closed = false;

  constructor(private readonly db: SQLiteDatabase) {
    if (db.filename !== ":memory:") throw new Error("FTS5 generation is not in memory");
    db.exec(SCHEMA_SQL);
  }

  get documents(): number {
    return this.sourceBytes.size;
  }

  get chunks(): number {
    return this.chunkCount;
  }

  replaceSource(preparation: SourcePreparation): void {
    this.requireOpen();
    const indexed = preparation.kind === "indexed";
    const projected = indexed
      ? preparation.chunks.map((chunk) => projectChunk(preparation, chunk))
      : [];
    const replacementBytes = projected.reduce((total, row) => total + row.indexedBytes, 0);
    const previousBytes = this.sourceBytes.get(preparation.source_key) ?? 0;
    const previousChunks = Number(this.db.selectValue(
      "SELECT count(*) FROM chunks WHERE source_key = ?",
      [preparation.source_key],
    ));
    const nextChunks = this.chunkCount - previousChunks + projected.length;
    const nextBytes = this.corpusBytes - previousBytes + replacementBytes;
    if (nextChunks > MAX_INDEX_CHUNKS || nextBytes > MAX_INDEXED_TEXT_BYTES) {
      throw new Error("in-memory corpus limit exceeded");
    }

    this.db.transaction("IMMEDIATE", () => {
      this.db.exec("DELETE FROM chunks WHERE source_key = ?", {
        bind: [preparation.source_key],
      });
      this.db.exec("DELETE FROM sources WHERE source_key = ?", {
        bind: [preparation.source_key],
      });
      if (!indexed) return;
      this.db.exec(INSERT_SOURCE_SQL, {
        bind: [
          preparation.source_key,
          preparation.vault_id,
          preparation.path,
          preparation.byte_length,
        ],
      });
      for (const row of projected) {
        this.db.exec(INSERT_CHUNK_SQL, { bind: row.bind });
      }
    });

    this.chunkCount = nextChunks;
    this.corpusBytes = nextBytes;
    if (indexed) this.sourceBytes.set(preparation.source_key, replacementBytes);
    else this.sourceBytes.delete(preparation.source_key);
  }

  metadataProbe(plan: MetadataProbePlan): boolean {
    this.requireOpen();
    const bound = bindMetadataProbe(plan);
    return Number(this.db.selectValue(bound.sql, bound.bind)) === 1;
  }

  search(plan: MatchPlan, limit: number): WorkerSearchHit[] {
    this.requireOpen();
    const bound = bindSearchPlan(plan, limit);
    return this.db.selectObjects(bound.sql, bound.bind).map(parseSearchRow);
  }

  assertIntegrity(): void {
    this.requireOpen();
    try {
      this.db.exec("INSERT INTO chunks_fts(chunks_fts) VALUES('integrity-check')");
    } catch {
      throw new Error("FTS5 integrity check failed");
    }
  }

  close(): void {
    if (this.closed) return;
    this.closed = true;
    this.db.close();
    if (this.db.pointer !== undefined) throw new Error("SQLite database remained open");
    this.sourceBytes.clear();
    this.corpusBytes = 0;
    this.chunkCount = 0;
  }

  private requireOpen(): void {
    if (this.closed) throw new Error("FTS5 generation is closed");
  }
}

export function openFts5Generation(sqlite: SQLiteApi): Fts5GenerationIndex {
  return new Fts5GenerationIndex(new sqlite.oo1.DB(":memory:", "c"));
}

interface ProjectedChunk {
  bind: readonly unknown[];
  indexedBytes: number;
}

function projectChunk(preparation: SourcePreparation, prepared: PreparedChunk): ProjectedChunk {
  const chunk = prepared.chunk;
  const aliases = preparation.retrieval.aliases.join(" ");
  const title = chunk.frontmatter.title ?? "";
  const tags = chunk.frontmatter.tags?.join(" ") ?? "";
  const fields = [
    preparation.retrieval.filename,
    preparation.retrieval.stem,
    aliases,
    title,
    prepared.heading_text,
    chunk.path,
    tags,
    chunk.content,
    prepared.technical_identifiers.join(" "),
  ].map(sanitizeIndexedText);
  const indexedBytes = fields.reduce(
    (total, value) => total + new TextEncoder().encode(value).byteLength,
    0,
  );
  return {
    bind: [
      preparation.source_key,
      chunk.chunk_id,
      chunk.vault_id,
      chunk.path,
      JSON.stringify(chunk.heading_path),
      JSON.stringify(chunk.frontmatter),
      chunk.mtime,
      chunk.content_hash,
      chunk.chunking_version,
      ...fields,
    ],
    indexedBytes,
  };
}

function sanitizeIndexedText(value: string): string {
  return value
    .replaceAll(FTS_HIGHLIGHT_START, "�")
    .replaceAll(FTS_HIGHLIGHT_END, "�");
}

function parseSearchRow(row: Record<string, unknown>): WorkerSearchHit {
  if (typeof row.chunk_id !== "string"
    || typeof row.vault_id !== "string"
    || typeof row.path !== "string"
    || typeof row.heading_path_json !== "string"
    || typeof row.frontmatter_json !== "string"
    || typeof row.score !== "number"
    || !Number.isFinite(row.score)
    || typeof row.excerpt !== "string") {
    throw new Error("SQLite returned an invalid search row");
  }
  const headingPath = JSON.parse(row.heading_path_json) as unknown;
  const frontmatter = JSON.parse(row.frontmatter_json) as unknown;
  if (!Array.isArray(headingPath)
    || !headingPath.every((heading) => typeof heading === "string")
    || typeof frontmatter !== "object"
    || frontmatter === null
    || Array.isArray(frontmatter)) {
    throw new Error("SQLite returned invalid stored metadata");
  }
  return {
    chunk_id: row.chunk_id,
    vault_id: row.vault_id,
    path: row.path,
    heading_path: headingPath,
    score: row.score,
    excerpt: row.excerpt,
    frontmatter,
  } as WorkerSearchHit;
}
