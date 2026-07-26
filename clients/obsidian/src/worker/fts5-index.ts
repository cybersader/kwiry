// SPDX-FileCopyrightText: 2026 cybersader
// SPDX-License-Identifier: GPL-3.0-only

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

/**
 * Raised when `chunks` and `chunks_fts` disagree. Distinct from a rejected
 * source so the Worker can report a divergence as `integrity_failed` instead
 * of blaming the batch that merely revealed it.
 */
export class IndexIntegrityError extends Error {
  constructor(message: string) {
    super(message);
    this.name = "IndexIntegrityError";
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

-- Slim metadata table: exactly the columns reconciliation (source_key) and
-- result identity (chunk_id, vault_id, path, heading_path, frontmatter) need.
-- Indexed field text is NOT stored here; it lives only in the contentless
-- FTS index, and excerpt text is hydrated from the vault file by the host.
CREATE TABLE chunks (
  rowid INTEGER PRIMARY KEY,
  source_key TEXT NOT NULL,
  chunk_id TEXT NOT NULL UNIQUE,
  vault_id TEXT NOT NULL,
  path TEXT NOT NULL,
  heading_path_json TEXT NOT NULL,
  frontmatter_json TEXT NOT NULL
);

CREATE INDEX chunks_by_source ON chunks(source_key);

-- content='' + contentless_delete=1: no stored column text, but deletes are
-- supported by rowid. detail stays at the default 'full' so phrase queries,
-- column filters and NEAR() keep working. columnsize is NOT disabled:
-- columnsize=0 is rejected outright with contentless_delete=1, and bm25()
-- needs the column sizes anyway.
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
  content='',
  contentless_delete=1,
  tokenize='unicode61 remove_diacritics 2'
);
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
  rowid, source_key, chunk_id, vault_id, path, heading_path_json, frontmatter_json
) VALUES(?, ?, ?, ?, ?, ?, ?)
`;

const INSERT_CHUNK_FTS_SQL = `
INSERT INTO chunks_fts(
  rowid, filename, stem, aliases, title, heading_text,
  path_text, tags, content, identifiers
) VALUES(?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
`;

// Must run BEFORE the matching DELETE FROM chunks: the subquery reads the
// rowids out of `chunks`, so deleting the metadata rows first would silently
// orphan every FTS posting for that source.
const DELETE_SOURCE_FTS_SQL = `
DELETE FROM chunks_fts WHERE rowid IN (SELECT rowid FROM chunks WHERE source_key = ?)
`;

const DELETE_SOURCE_CHUNKS_SQL = "DELETE FROM chunks WHERE source_key = ?";

const DELETE_SOURCE_SQL = "DELETE FROM sources WHERE source_key = ?";

// Seeded from BOTH tables. `chunks` alone is not enough: a posting that exists
// in `chunks_fts` without a matching metadata row is exactly the state the
// allocator has to route around, and such a posting is invisible to
// `MAX(rowid) FROM chunks`. Reusing its rowid would shadow it — the contentless
// index accepts the duplicate silently, keeps the shadowed posting alive across
// the next delete, and reconciliation would then see matching counts and no
// orphan. Allocating above both tables keeps any such posting visible as an
// orphan, which `runReconciliationCheck` does fail on.
const SELECT_MAX_ROWID_SQL = `
SELECT MAX(
  (SELECT COALESCE(MAX(rowid), 0) FROM chunks),
  (SELECT COALESCE(MAX(rowid), 0) FROM chunks_fts)
)
`;

// A contentless FTS5 index accepts a duplicate rowid without complaint and
// keeps the shadowed posting alive across the next delete, and no
// integrity check can see it. Reconciling `chunks` against `chunks_fts` is
// the only check that can fail once the external-content cross-check is gone.
const RECONCILE_SQL = `
SELECT
  (SELECT count(*) FROM chunks) AS chunks,
  (SELECT count(*) FROM chunks_fts) AS fts,
  (SELECT count(*) FROM chunks_fts f LEFT JOIN chunks c ON c.rowid = f.rowid
     WHERE c.rowid IS NULL) AS orphan_fts,
  (SELECT count(*) FROM chunks c LEFT JOIN chunks_fts f ON f.rowid = c.rowid
     WHERE f.rowid IS NULL) AS missing_fts
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

  /**
   * `verifyIntegrity` reconciles `chunks` against `chunks_fts` inside the same
   * transaction. It defaults to off because a staging build runs many batches
   * and is gated once at `commitBuild`; callers that mutate a generation which
   * is already published have no later gate and must pass `true`.
   */
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
      // Seeded from both tables before any delete, so every rowid allocated in
      // this transaction is strictly greater than any rowid present in either
      // `chunks` or `chunks_fts` when the transaction opened. A plain INTEGER
      // PRIMARY KEY reuses freed rowids, and a reused rowid inserted into the
      // contentless index on top of a surviving posting shadows it
      // undetectably.
      let nextRowid = requireRowidSeed(this.db.selectValue(SELECT_MAX_ROWID_SQL));
      for (const stored of touched.values()) {
        // Order is load-bearing: FTS postings first (they are located through
        // `chunks`), then the metadata rows, then the source row.
        this.db.exec(DELETE_SOURCE_FTS_SQL, { bind: [stored.source_key] });
        this.db.exec(DELETE_SOURCE_CHUNKS_SQL, { bind: [stored.source_key] });
        this.db.exec(DELETE_SOURCE_SQL, { bind: [stored.source_key] });
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
          const rowid = ++nextRowid;
          if (!Number.isSafeInteger(rowid)) throw new Error("chunk rowid space exhausted");
          this.db.exec(INSERT_CHUNK_SQL, {
            bind: [rowid, change.preparation.source_key, ...row.metadataBind],
          });
          this.db.exec(INSERT_CHUNK_FTS_SQL, { bind: [rowid, ...row.ftsBind] });
        }
      }
      // Reconciliation, not the FTS structure check: the structure check
      // provably cannot observe a `chunks` / `chunks_fts` divergence, which is
      // the only failure this ordering of raw statements can introduce. Run
      // inside the transaction so a divergence rolls the whole batch back
      // instead of publishing a half-applied change.
      if (verifyIntegrity) this.runReconciliationCheck(nextChunks);
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
      this.runReconciliationCheck(this.chunkCount);
    } catch {
      throw new Error("FTS5 integrity check failed");
    }
  }

  /**
   * Pre-publication compaction. `optimize` merges the FTS b-tree segments and
   * `VACUUM` rebuilds the image without the freed pages. `VACUUM` cannot run
   * inside a transaction, so this must never be called from
   * `applySourceChanges`.
   */
  compact(): void {
    this.requireOpen();
    this.db.exec("INSERT INTO chunks_fts(chunks_fts) VALUES('optimize')");
    this.db.exec("VACUUM");
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

  /**
   * Internal FTS5 structure check only. On a contentless table there is no
   * content table to compare against, so this can never observe a `chunks` /
   * `chunks_fts` divergence — `runReconciliationCheck` is what can fail.
   */
  private runIntegrityCheck(): void {
    this.db.exec("INSERT INTO chunks_fts(chunks_fts, rank) VALUES('integrity-check', 1)");
  }

  /**
   * `expectedChunks` is passed in rather than read from `this.chunkCount`: the
   * in-transaction caller runs before the counters are advanced, and comparing
   * against the projected count also checks the projection arithmetic itself.
   */
  private runReconciliationCheck(expectedChunks: number): void {
    const rows = this.db.selectObjects(RECONCILE_SQL);
    if (rows.length !== 1) {
      throw new IndexIntegrityError("FTS5 reconciliation query returned no row");
    }
    const row = rows[0]!;
    if (!isNonNegativeSafeInteger(row.chunks)
      || !isNonNegativeSafeInteger(row.fts)
      || !isNonNegativeSafeInteger(row.orphan_fts)
      || !isNonNegativeSafeInteger(row.missing_fts)) {
      throw new IndexIntegrityError("FTS5 reconciliation query returned invalid counts");
    }
    if (row.orphan_fts !== 0
      || row.missing_fts !== 0
      || row.chunks !== row.fts
      || row.chunks !== expectedChunks) {
      throw new IndexIntegrityError("FTS5 index and chunk metadata disagree");
    }
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
  /** chunk_id, vault_id, path, heading_path_json, frontmatter_json. */
  metadataBind: readonly unknown[];
  /** The nine FTS field values, in declared column order. */
  ftsBind: readonly string[];
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
  ];
  const indexedBytes = fields.reduce(
    (total, value) => total + new TextEncoder().encode(value).byteLength,
    0,
  );
  return {
    metadataBind: [
      chunk.chunk_id,
      chunk.vault_id,
      chunk.path,
      JSON.stringify(chunk.heading_path),
      JSON.stringify(chunk.frontmatter),
    ],
    ftsBind: fields,
    indexedBytes,
    chunkId: chunk.chunk_id,
  };
}

function requireRowidSeed(value: unknown): number {
  const seed = Number(value);
  if (!Number.isSafeInteger(seed) || seed < 0) throw new Error("chunk rowid seed is invalid");
  return seed;
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

function parseSearchRow(row: Record<string, unknown>): WorkerSearchHit {
  if (typeof row.chunk_id !== "string"
    || typeof row.vault_id !== "string"
    || typeof row.path !== "string"
    || typeof row.heading_path_json !== "string"
    || typeof row.frontmatter_json !== "string"
    || typeof row.score !== "number"
    || !Number.isFinite(row.score)) {
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
    // The contentless index stores no text, so the Worker cannot produce
    // excerpt text. The frozen hit shape keeps the field; the host fills it
    // by hydrating a bounded window from the authoritative vault file.
    excerpt: "",
    frontmatter,
  } as WorkerSearchHit;
}
