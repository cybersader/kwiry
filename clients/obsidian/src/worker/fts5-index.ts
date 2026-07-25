// SPDX-FileCopyrightText: 2026 cybersader
// SPDX-License-Identifier: GPL-3.0-only

import { FTS_HIGHLIGHT_END, FTS_HIGHLIGHT_START } from "../excerpt";
import type { SourceRemoval, WorkerSearchHit } from "./protocol";
import { bindMetadataProbe, bindSearchPlan } from "./query-binder";
import type {
  MatchPlan,
  MetadataProbePlan,
  PreparedChunk,
  SourcePreparation,
} from "./rust-adapter";

const MAX_INDEX_CHUNKS = 100_000;
const MAX_INDEXED_TEXT_BYTES = 256 * 1024 * 1024;

export interface Fts5IndexLimits {
  maxChunks: number;
  maxIndexedTextBytes: number;
}

const DEFAULT_INDEX_LIMITS: Fts5IndexLimits = {
  maxChunks: MAX_INDEX_CHUNKS,
  maxIndexedTextBytes: MAX_INDEXED_TEXT_BYTES,
};

export class IndexCapacityError extends Error {
  constructor() {
    super("in-memory corpus limit exceeded");
    this.name = "IndexCapacityError";
  }
}

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
  byte_length INTEGER NOT NULL,
  chunk_count INTEGER NOT NULL,
  indexed_bytes INTEGER NOT NULL,
  UNIQUE(vault_id, path)
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

const SELECT_SOURCE_BY_KEY_SQL = `
SELECT source_key, vault_id, path, chunk_count, indexed_bytes
FROM sources
WHERE source_key = ?
`;

const SELECT_SOURCE_BY_IDENTITY_SQL = `
SELECT source_key, vault_id, path, chunk_count, indexed_bytes
FROM sources
WHERE vault_id = ? AND path = ?
`;

const INSERT_SOURCE_SQL = `
INSERT INTO sources(
  source_key, vault_id, path, byte_length, chunk_count, indexed_bytes
) VALUES(?, ?, ?, ?, ?, ?)
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
  private documentCount = 0;
  private corpusBytes = 0;
  private chunkCount = 0;
  private closed = false;

  constructor(
    private readonly db: SQLiteDatabase,
    private readonly limits: Fts5IndexLimits = DEFAULT_INDEX_LIMITS,
  ) {
    if (db.filename !== ":memory:") throw new Error("FTS5 generation is not in memory");
    validateIndexLimits(limits);
    db.exec(SCHEMA_SQL);
  }

  get documents(): number {
    return this.documentCount;
  }

  get chunks(): number {
    return this.chunkCount;
  }

  replaceSource(preparation: SourcePreparation): void {
    this.applySourceChanges([preparation], []);
  }

  applySourceChanges(
    preparations: readonly SourcePreparation[],
    removals: readonly SourceRemoval[],
    verifyIntegrity = false,
  ): void {
    this.requireOpen();
    if (preparations.length === 0 && removals.length === 0) {
      throw new Error("source change batch is empty");
    }

    const projected = preparations.map(projectPreparation);
    validateChangeIdentities(projected, removals);

    const touched = new Map<string, StoredSource>();
    for (const removal of removals) {
      const stored = this.selectSourceByIdentity(removal.vault_id, removal.path);
      if (stored) touched.set(stored.source_key, stored);
    }
    for (const change of projected) {
      const byIdentity = this.selectSourceByIdentity(
        change.preparation.vault_id,
        change.preparation.path,
      );
      const byKey = this.selectSourceByKey(change.preparation.source_key);
      if (byIdentity && byIdentity.source_key !== change.preparation.source_key) {
        throw new Error("stored source identity does not match its prepared key");
      }
      if (byKey && (byKey.vault_id !== change.preparation.vault_id
        || byKey.path !== change.preparation.path)) {
        throw new Error("stored source key does not match its prepared identity");
      }
      if (byIdentity) touched.set(byIdentity.source_key, byIdentity);
      if (byKey) touched.set(byKey.source_key, byKey);
    }

    const indexed = projected.filter((change) => change.preparation.kind === "indexed");
    const removedChunks = sumSafe([...touched.values()].map((source) => source.chunk_count));
    const removedBytes = sumSafe([...touched.values()].map((source) => source.indexed_bytes));
    const addedChunks = sumSafe(indexed.map((change) => change.rows.length));
    const addedBytes = sumSafe(indexed.map((change) => change.indexedBytes));
    const nextDocuments = this.documentCount - touched.size + indexed.length;
    const nextChunks = this.chunkCount - removedChunks + addedChunks;
    const nextBytes = this.corpusBytes - removedBytes + addedBytes;
    requireProjectedCounts(nextDocuments, nextChunks, nextBytes, this.limits);

    this.db.transaction("IMMEDIATE", () => {
      for (const stored of touched.values()) {
        this.db.exec("DELETE FROM chunks WHERE source_key = ?", {
          bind: [stored.source_key],
        });
        this.db.exec("DELETE FROM sources WHERE source_key = ?", {
          bind: [stored.source_key],
        });
      }
      for (const change of indexed) {
        this.db.exec(INSERT_SOURCE_SQL, {
          bind: [
            change.preparation.source_key,
            change.preparation.vault_id,
            change.preparation.path,
            change.preparation.byte_length,
            change.rows.length,
            change.indexedBytes,
          ],
        });
        for (const row of change.rows) {
          this.db.exec(INSERT_CHUNK_SQL, { bind: row.bind });
        }
      }
      if (verifyIntegrity) this.runIntegrityCheck();
    });

    this.documentCount = nextDocuments;
    this.chunkCount = nextChunks;
    this.corpusBytes = nextBytes;
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
      this.runIntegrityCheck();
    } catch {
      throw new Error("FTS5 integrity check failed");
    }
  }

  close(): void {
    if (this.closed) return;
    this.closed = true;
    this.db.close();
    if (this.db.pointer !== undefined) throw new Error("SQLite database remained open");
    this.documentCount = 0;
    this.corpusBytes = 0;
    this.chunkCount = 0;
  }

  private selectSourceByKey(sourceKey: string): StoredSource | null {
    return parseStoredSource(this.db.selectObjects(SELECT_SOURCE_BY_KEY_SQL, [sourceKey]));
  }

  private selectSourceByIdentity(vaultId: string, path: string): StoredSource | null {
    return parseStoredSource(this.db.selectObjects(
      SELECT_SOURCE_BY_IDENTITY_SQL,
      [vaultId, path],
    ));
  }

  private runIntegrityCheck(): void {
    this.db.exec("INSERT INTO chunks_fts(chunks_fts) VALUES('integrity-check')");
  }

  private requireOpen(): void {
    if (this.closed) throw new Error("FTS5 generation is closed");
  }
}

export function openFts5Generation(
  sqlite: SQLiteApi,
  limits: Fts5IndexLimits = DEFAULT_INDEX_LIMITS,
): Fts5GenerationIndex {
  return new Fts5GenerationIndex(new sqlite.oo1.DB(":memory:", "c"), limits);
}

interface ProjectedChunk {
  bind: readonly unknown[];
  indexedBytes: number;
  chunkId: string;
}

interface ProjectedPreparation {
  preparation: SourcePreparation;
  rows: ProjectedChunk[];
  indexedBytes: number;
}

interface StoredSource {
  source_key: string;
  vault_id: string;
  path: string;
  chunk_count: number;
  indexed_bytes: number;
}

function projectPreparation(preparation: SourcePreparation): ProjectedPreparation {
  const rows = preparation.kind === "indexed"
    ? preparation.chunks.map((chunk) => projectChunk(preparation, chunk))
    : [];
  return {
    preparation,
    rows,
    indexedBytes: sumSafe(rows.map((row) => row.indexedBytes)),
  };
}

function validateChangeIdentities(
  projected: readonly ProjectedPreparation[],
  removals: readonly SourceRemoval[],
): void {
  const preparationIdentities = new Set<string>();
  const preparationKeys = new Set<string>();
  const chunkIds = new Set<string>();
  for (const change of projected) {
    const identity = sourceIdentity(change.preparation.vault_id, change.preparation.path);
    if (preparationIdentities.has(identity) || preparationKeys.has(change.preparation.source_key)) {
      throw new Error("source change batch contains duplicate preparations");
    }
    preparationIdentities.add(identity);
    preparationKeys.add(change.preparation.source_key);
    for (const row of change.rows) {
      if (chunkIds.has(row.chunkId)) {
        throw new Error("source change batch contains duplicate chunk IDs");
      }
      chunkIds.add(row.chunkId);
    }
  }

  const removalIdentities = new Set<string>();
  for (const removal of removals) {
    const identity = sourceIdentity(removal.vault_id, removal.path);
    if (removalIdentities.has(identity) || preparationIdentities.has(identity)) {
      throw new Error("source change batch contains duplicate identities");
    }
    removalIdentities.add(identity);
  }
}

function projectChunk(preparation: SourcePreparation, prepared: PreparedChunk): ProjectedChunk {
  const chunk = prepared.chunk;
  if (chunk.vault_id !== preparation.vault_id
    || chunk.path !== preparation.path
    || chunk.mtime !== preparation.mtime
    || chunk.content_hash !== preparation.content_hash) {
    throw new Error("prepared chunk does not match its source");
  }
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
    chunkId: chunk.chunk_id,
  };
}

function parseStoredSource(rows: Record<string, unknown>[]): StoredSource | null {
  if (rows.length === 0) return null;
  if (rows.length !== 1) throw new Error("stored source identity is ambiguous");
  const row = rows[0];
  if (!row
    || typeof row.source_key !== "string"
    || typeof row.vault_id !== "string"
    || typeof row.path !== "string"
    || !isNonNegativeSafeInteger(row.chunk_count)
    || !isNonNegativeSafeInteger(row.indexed_bytes)) {
    throw new Error("stored source metadata is invalid");
  }
  return {
    source_key: row.source_key,
    vault_id: row.vault_id,
    path: row.path,
    chunk_count: row.chunk_count,
    indexed_bytes: row.indexed_bytes,
  };
}

function requireProjectedCounts(
  documents: number,
  chunks: number,
  bytes: number,
  limits: Fts5IndexLimits,
): void {
  if (!isNonNegativeSafeInteger(documents)
    || !isNonNegativeSafeInteger(chunks)
    || !isNonNegativeSafeInteger(bytes)) {
    throw new Error("source accounting is invalid");
  }
  if (chunks > limits.maxChunks || bytes > limits.maxIndexedTextBytes) {
    throw new IndexCapacityError();
  }
}

function validateIndexLimits(limits: Fts5IndexLimits): void {
  if (!Number.isSafeInteger(limits.maxChunks)
    || limits.maxChunks < 1
    || !Number.isSafeInteger(limits.maxIndexedTextBytes)
    || limits.maxIndexedTextBytes < 1) {
    throw new Error("FTS5 index limits must be positive safe integers");
  }
}

function sumSafe(values: readonly number[]): number {
  let total = 0;
  for (const value of values) {
    if (!isNonNegativeSafeInteger(value)) throw new Error("source accounting is invalid");
    total += value;
    if (!Number.isSafeInteger(total)) throw new Error("source accounting exceeded its limit");
  }
  return total;
}

function sourceIdentity(vaultId: string, path: string): string {
  return JSON.stringify([vaultId, path]);
}

function isNonNegativeSafeInteger(value: unknown): value is number {
  return typeof value === "number" && Number.isSafeInteger(value) && value >= 0;
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
