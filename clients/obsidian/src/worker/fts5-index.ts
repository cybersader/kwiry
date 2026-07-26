// SPDX-FileCopyrightText: 2026 cybersader
// SPDX-License-Identifier: GPL-3.0-only

import { CACHE_SCHEMA_VERSION, MAX_EXPORT_BLOB_BYTES } from "./protocol";
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
// A skipped source costs no chunks and no indexed bytes, so neither existing
// bound can constrain how many source rows a generation accumulates. The
// freshness table needs its own ceiling.
const MAX_INDEX_SOURCES = 200_000;

export interface Fts5IndexLimits {
  maxChunks: number;
  maxIndexedTextBytes: number;
  maxSources?: number;
  maxExportBytes?: number;
}

interface ResolvedFts5IndexLimits {
  maxChunks: number;
  maxIndexedTextBytes: number;
  maxSources: number;
  maxExportBytes: number;
}

const DEFAULT_INDEX_LIMITS: Fts5IndexLimits = {
  maxChunks: MAX_INDEX_CHUNKS,
  maxIndexedTextBytes: MAX_INDEXED_TEXT_BYTES,
  maxSources: MAX_INDEX_SOURCES,
  maxExportBytes: MAX_EXPORT_BLOB_BYTES,
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
  capi: {
    /**
     * Serializes the database into a fresh JS-heap buffer copied out of WASM
     * memory. The live database is untouched, so the returned bytes may be
     * transferred without disturbing the serving generation.
     */
    sqlite3_js_db_export(db: unknown, schema?: string): Uint8Array;
  };
}

// Any edit to the tables below changes the cache image format and must bump
// `CACHE_SCHEMA_VERSION` in ./protocol, which the export identity envelope
// carries: an image whose value differs from the running build's is not
// restorable. `PRAGMA user_version` stamps the same number into the SQLite
// header, so a restored image declares its own schema version before anything
// outside it has to be trusted.
//
// `sources` is the freshness table a later restore reconciles against, so it
// records every prepared source — including the ones the chunker skipped —
// with the facts the Rust adapter produced. It never records a fact the host
// or TypeScript derived.
//
// `mtime_nanos` is TEXT, not INTEGER: it is a u128 of up to 39 digits, which a
// 64-bit SQLite INTEGER would silently truncate, corrupting exactly the
// comparison the restore path depends on.
//
// `content_hash` is nullable because one skip outcome genuinely has no hash:
// a source over the 10 MiB file ceiling is refused before it is read, and that
// length is reachable through the Worker protocol.
const SCHEMA_SQL = `
PRAGMA user_version = ${requireSchemaVersionLiteral(CACHE_SCHEMA_VERSION)};

CREATE TABLE sources (
  source_key TEXT PRIMARY KEY,
  vault_id TEXT NOT NULL,
  path TEXT NOT NULL,
  outcome TEXT NOT NULL CHECK(outcome IN ('indexed','skipped')),
  content_hash TEXT,
  byte_length INTEGER NOT NULL CHECK(byte_length >= 0),
  mtime_nanos TEXT NOT NULL
    CHECK(mtime_nanos <> '' AND mtime_nanos NOT GLOB '*[^0-9]*'),
  chunk_count INTEGER NOT NULL CHECK(chunk_count >= 0),
  indexed_bytes INTEGER NOT NULL CHECK(indexed_bytes >= 0),
  CHECK(outcome = 'skipped' OR content_hash IS NOT NULL),
  CHECK(outcome = 'indexed' OR (chunk_count = 0 AND indexed_bytes = 0)),
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

const SOURCE_COLUMNS_SQL = `
source_key, vault_id, path, outcome, content_hash, byte_length,
mtime_nanos, chunk_count, indexed_bytes
`;

const SELECT_SOURCE_BY_KEY_SQL = `
SELECT ${SOURCE_COLUMNS_SQL}
FROM sources
WHERE source_key = ?
`;

const SELECT_SOURCE_BY_IDENTITY_SQL = `
SELECT ${SOURCE_COLUMNS_SQL}
FROM sources
WHERE vault_id = ? AND path = ?
`;

const INSERT_SOURCE_SQL = `
INSERT INTO sources(${SOURCE_COLUMNS_SQL}) VALUES(?, ?, ?, ?, ?, ?, ?, ?, ?)
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
//
// The `sources` half is new with the freshness table: the per-source chunk
// tallies are the numbers a restore diff will trust, so they are checked
// against the real chunk rows rather than assumed.
const RECONCILE_SQL = `
SELECT
  (SELECT count(*) FROM chunks) AS chunks,
  (SELECT count(*) FROM chunks_fts) AS fts,
  (SELECT count(*) FROM chunks_fts f LEFT JOIN chunks c ON c.rowid = f.rowid
     WHERE c.rowid IS NULL) AS orphan_fts,
  (SELECT count(*) FROM chunks c LEFT JOIN chunks_fts f ON f.rowid = c.rowid
     WHERE f.rowid IS NULL) AS missing_fts,
  (SELECT count(*) FROM sources) AS sources,
  (SELECT count(*) FROM sources WHERE outcome = 'indexed') AS indexed_sources,
  (SELECT COALESCE(SUM(chunk_count), 0) FROM sources) AS source_chunks,
  (SELECT count(*) FROM chunks c LEFT JOIN sources s ON s.source_key = c.source_key
     WHERE s.source_key IS NULL) AS orphan_chunks,
  (SELECT count(*) FROM sources s WHERE s.outcome = 'skipped'
     AND EXISTS(SELECT 1 FROM chunks c WHERE c.source_key = s.source_key))
    AS skipped_with_chunks
`;

export class Fts5GenerationIndex {
  private documentCount = 0;
  private corpusBytes = 0;
  private chunkCount = 0;
  private sourceCount = 0;
  private observedChunkingVersion: number | null = null;
  private closed = false;
  private readonly limits: ResolvedFts5IndexLimits;

  constructor(
    private readonly db: SQLiteDatabase,
    limits: Fts5IndexLimits = DEFAULT_INDEX_LIMITS,
  ) {
    if (db.filename !== ":memory:") throw new Error("FTS5 generation is not in memory");
    this.limits = resolveIndexLimits(limits);
    db.exec(SCHEMA_SQL);
  }

  /** Indexed sources only. Skipped sources are recorded but are not documents. */
  get documents(): number {
    return this.documentCount;
  }

  get chunks(): number {
    return this.chunkCount;
  }

  /** Every prepared source recorded in the freshness table, skips included. */
  get sources(): number {
    return this.sourceCount;
  }

  /**
   * The single chunking contract every chunk in this generation was produced
   * under, or `null` while the generation holds no chunks. A generation that
   * mixed two chunkers would produce a cache image no single chunking version
   * could describe, so `applySourceChanges` refuses the batch that would do it.
   */
  get chunkingVersion(): number | null {
    return this.observedChunkingVersion;
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

    const stored = [...touched.values()];
    const indexed = projected.filter((change) => change.preparation.kind === "indexed");
    // Only indexed rows are documents. Counting every touched row here would
    // drive `documents` negative the second time an already-recorded skipped
    // source is re-prepared, turning a benign re-scan into a rejected batch.
    const removedDocuments = stored.filter((source) => source.outcome === "indexed").length;
    const removedChunks = sumSafe(stored.map((source) => source.chunk_count));
    const removedBytes = sumSafe(stored.map((source) => source.indexed_bytes));
    const addedChunks = sumSafe(indexed.map((change) => change.rows.length));
    const addedBytes = sumSafe(indexed.map((change) => change.indexedBytes));
    const nextDocuments = this.documentCount - removedDocuments + indexed.length;
    const nextChunks = this.chunkCount - removedChunks + addedChunks;
    const nextBytes = this.corpusBytes - removedBytes + addedBytes;
    const nextSources = this.sourceCount - touched.size + projected.length;
    const nextChunkingVersion = requireSingleChunkingVersion(
      this.observedChunkingVersion,
      projected,
    );
    requireProjectedCounts(nextDocuments, nextChunks, nextBytes, nextSources, this.limits);

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
      // Every prepared source is recorded, not just the indexed ones: a
      // restore has to be able to tell "seen and skipped, still current" from
      // "never seen", and only a stored row can carry that distinction. Every
      // value bound here comes from the Rust preparation verbatim.
      for (const change of projected) {
        this.db.exec(INSERT_SOURCE_SQL, {
          bind: [
            change.preparation.source_key,
            change.preparation.vault_id,
            change.preparation.path,
            change.preparation.kind,
            change.preparation.content_hash,
            change.preparation.byte_length,
            change.preparation.mtime_nanos,
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
      if (verifyIntegrity) {
        this.runReconciliationCheck(nextChunks, nextDocuments, nextSources);
      }
    });

    this.documentCount = nextDocuments;
    this.chunkCount = nextChunks;
    this.corpusBytes = nextBytes;
    this.sourceCount = nextSources;
    this.observedChunkingVersion = nextChunkingVersion;
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
      this.runReconciliationCheck(this.chunkCount, this.documentCount, this.sourceCount);
    } catch {
      throw new Error("FTS5 integrity check failed");
    }
  }

  /**
   * Serializes the generation into a detached buffer. `sqlite3_js_db_export`
   * copies out of WASM memory, so the returned bytes can be transferred to the
   * host without touching the live, serving database.
   */
  exportImage(api: SQLiteApi): Uint8Array {
    this.requireOpen();
    const image = api.capi.sqlite3_js_db_export(this.db.pointer);
    if (!(image instanceof Uint8Array) || image.byteLength === 0) {
      throw new IndexIntegrityError("FTS5 generation produced no exportable image");
    }
    if (image.byteLength > this.limits.maxExportBytes) throw new IndexCapacityError();
    return image;
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
    this.sourceCount = 0;
    this.observedChunkingVersion = null;
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
   * The expected counts are passed in rather than read from the committed
   * fields: the in-transaction caller runs before the counters are advanced,
   * and comparing against the projected counts also checks the projection
   * arithmetic itself.
   */
  private runReconciliationCheck(
    expectedChunks: number,
    expectedDocuments: number,
    expectedSources: number,
  ): void {
    const rows = this.db.selectObjects(RECONCILE_SQL);
    if (rows.length !== 1) {
      throw new IndexIntegrityError("FTS5 reconciliation query returned no row");
    }
    const row = rows[0]!;
    if (!isNonNegativeSafeInteger(row.chunks)
      || !isNonNegativeSafeInteger(row.fts)
      || !isNonNegativeSafeInteger(row.orphan_fts)
      || !isNonNegativeSafeInteger(row.missing_fts)
      || !isNonNegativeSafeInteger(row.sources)
      || !isNonNegativeSafeInteger(row.indexed_sources)
      || !isNonNegativeSafeInteger(row.source_chunks)
      || !isNonNegativeSafeInteger(row.orphan_chunks)
      || !isNonNegativeSafeInteger(row.skipped_with_chunks)) {
      throw new IndexIntegrityError("FTS5 reconciliation query returned invalid counts");
    }
    if (row.orphan_fts !== 0
      || row.missing_fts !== 0
      || row.chunks !== row.fts
      || row.chunks !== expectedChunks
      || row.sources !== expectedSources
      || row.indexed_sources !== expectedDocuments
      // Per-source tallies must sum to the real chunk count, no chunk may
      // outlive its source row, and a skipped source may never own chunks.
      // These are what make the stored `chunk_count` trustworthy for a diff.
      || row.source_chunks !== row.chunks
      || row.orphan_chunks !== 0
      || row.skipped_with_chunks !== 0) {
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
  chunkingVersion: number;
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
  outcome: "indexed" | "skipped";
  content_hash: string | null;
  byte_length: number;
  mtime_nanos: string;
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

/**
 * A generation whose chunks came from two different chunkers cannot be
 * described by one chunking version, so its exported image could not be
 * validated on restore. The divergence is refused before the transaction opens.
 */
function requireSingleChunkingVersion(
  current: number | null,
  projected: readonly ProjectedPreparation[],
): number | null {
  let version = current;
  for (const change of projected) {
    for (const row of change.rows) {
      if (version === null) version = row.chunkingVersion;
      else if (version !== row.chunkingVersion) {
        throw new Error("source change batch mixes chunking versions");
      }
    }
  }
  return version;
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
    chunkingVersion: chunk.chunking_version,
  };
}

/**
 * `PRAGMA user_version` takes a literal, not a bind parameter, so the value is
 * interpolated. It is a build constant, but interpolating anything into SQL
 * without proving its shape is exactly how an injection is introduced later.
 */
function requireSchemaVersionLiteral(version: number): string {
  if (!Number.isSafeInteger(version) || version < 1 || version > 2_147_483_647) {
    throw new Error("cache schema version must be a positive 32-bit integer");
  }
  return String(version);
}

function requireRowidSeed(value: unknown): number {
  const seed = Number(value);
  if (!Number.isSafeInteger(seed) || seed < 0) throw new Error("chunk rowid seed is invalid");
  return seed;
}

/**
 * The read side of the freshness table. It is exact rather than permissive
 * because a restore diff reads exactly these columns: a row that survives this
 * check is a row a later slice is entitled to trust.
 */
function parseStoredSource(rows: Record<string, unknown>[]): StoredSource | null {
  if (rows.length === 0) return null;
  if (rows.length !== 1) throw new Error("stored source identity is ambiguous");
  const row = rows[0];
  if (!row
    || typeof row.source_key !== "string"
    || typeof row.vault_id !== "string"
    || typeof row.path !== "string"
    || (row.outcome !== "indexed" && row.outcome !== "skipped")
    || !(row.content_hash === null
      || (typeof row.content_hash === "string"
        && row.content_hash.length > 0
        && row.content_hash.length <= 128))
    || !isNonNegativeSafeInteger(row.byte_length)
    || typeof row.mtime_nanos !== "string"
    || !/^[0-9]{1,39}$/u.test(row.mtime_nanos)
    || !isNonNegativeSafeInteger(row.chunk_count)
    || !isNonNegativeSafeInteger(row.indexed_bytes)
    || (row.outcome === "indexed" && row.content_hash === null)
    || (row.outcome === "skipped" && (row.chunk_count !== 0 || row.indexed_bytes !== 0))) {
    throw new Error("stored source metadata is invalid");
  }
  return {
    source_key: row.source_key,
    vault_id: row.vault_id,
    path: row.path,
    outcome: row.outcome,
    content_hash: row.content_hash,
    byte_length: row.byte_length,
    mtime_nanos: row.mtime_nanos,
    chunk_count: row.chunk_count,
    indexed_bytes: row.indexed_bytes,
  };
}

function requireProjectedCounts(
  documents: number,
  chunks: number,
  bytes: number,
  sources: number,
  limits: ResolvedFts5IndexLimits,
): void {
  if (!isNonNegativeSafeInteger(documents)
    || !isNonNegativeSafeInteger(chunks)
    || !isNonNegativeSafeInteger(bytes)
    || !isNonNegativeSafeInteger(sources)) {
    throw new Error("source accounting is invalid");
  }
  if (chunks > limits.maxChunks
    || bytes > limits.maxIndexedTextBytes
    || sources > limits.maxSources) {
    throw new IndexCapacityError();
  }
}

function resolveIndexLimits(limits: Fts5IndexLimits): ResolvedFts5IndexLimits {
  const resolved: ResolvedFts5IndexLimits = {
    maxChunks: limits.maxChunks,
    maxIndexedTextBytes: limits.maxIndexedTextBytes,
    maxSources: limits.maxSources ?? MAX_INDEX_SOURCES,
    maxExportBytes: limits.maxExportBytes ?? MAX_EXPORT_BLOB_BYTES,
  };
  for (const limit of Object.values(resolved)) {
    if (!Number.isSafeInteger(limit) || limit < 1) {
      throw new Error("FTS5 index limits must be positive safe integers");
    }
  }
  return resolved;
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
