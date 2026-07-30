// SPDX-FileCopyrightText: 2026 cybersader
// SPDX-License-Identifier: GPL-3.0-only

import { openPlainBlockVfs, type BlockVfsHandle } from "./block-vfs";
import {
  CACHE_SCHEMA_VERSION,
  MAX_EXPORT_BLOB_BYTES,
  MAX_RECONCILIATION_SOURCES,
} from "./protocol";
import type {
  ReconciliationPlanResult,
  ReconciliationSourceMetadata,
  SourceRemoval,
  WorkerFrontmatter,
  WorkerSearchHit,
} from "./protocol";
import { isPreparedPropertyBag } from "./source-defect";
import {
  bindEvidenceProbe,
  bindSearchStage,
  requireExecutionPlanIdentity,
} from "./query-binder";
import type {
  EvidenceProbePlan,
  ExecutionPlan,
  QueryEvidenceObservation,
  PreparedChunk,
  PreparedFrontmatter,
  PreparedPropertyValue,
  SourcePreparation,
} from "./rust-adapter";

const MAX_INDEX_CHUNKS = 100_000;
export const DEFAULT_DATABASE_BYTE_LIMIT = 320 * 1024 * 1024;
// A skipped source costs no chunks, so chunk bounds cannot constrain how many
// source rows a generation accumulates. The freshness table needs its own ceiling.
const MAX_INDEX_SOURCES = 200_000;
const SQLITE_FULL = 13;

export interface Fts5IndexLimits {
  maxChunks: number;
  maxDatabaseBytes: number;
  maxSources?: number;
  maxExportBytes?: number;
}

interface ResolvedFts5IndexLimits {
  maxChunks: number;
  maxDatabaseBytes: number;
  maxSources: number;
  maxExportBytes: number;
}

const DEFAULT_INDEX_LIMITS: Fts5IndexLimits = {
  maxChunks: MAX_INDEX_CHUNKS,
  maxDatabaseBytes: DEFAULT_DATABASE_BYTE_LIMIT,
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

export class CacheImageInvalidError extends Error {
  constructor(message = "cache image is invalid") {
    super(message);
    this.name = "CacheImageInvalidError";
  }
}

export class CacheVersionMismatchError extends Error {
  constructor() {
    super("cache schema version does not match this build");
    this.name = "CacheVersionMismatchError";
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

export interface SQLiteStruct {
  readonly pointer: number;
  dispose(): void;
  $iVersion?: number;
  $szOsFile?: number;
  $mxPathname?: number;
  $pMethods?: number;
  $xRandomness?: number;
  $xSleep?: number;
}

interface SQLiteStructConstructor {
  new (pointer?: number): SQLiteStruct;
}

interface SQLiteFileConstructor extends SQLiteStructConstructor {
  readonly structInfo: { readonly sizeof: number };
}

interface SQLiteDatabaseConstructor {
  new (filename: string, flags: string): SQLiteDatabase;
  new (options: { filename: string; flags: string }): SQLiteDatabase;
}

export interface SQLiteApi {
  oo1: { DB: SQLiteDatabaseConstructor };
  capi: {
    /**
     * Serializes the database into a fresh JS-heap buffer copied out of WASM
     * memory. The live database is untouched, so the returned bytes may be
     * transferred without disturbing the serving generation.
     */
    sqlite3_js_db_export(db: unknown, schema?: string): Uint8Array;
    sqlite3_vfs_find(name: string | null): number;
    sqlite3_vfs_unregister(pointer: number): number;
    sqlite3_vfs: SQLiteStructConstructor;
    sqlite3_io_methods: SQLiteStructConstructor;
    sqlite3_file: SQLiteFileConstructor;
    SQLITE_OK: number;
    SQLITE_IOERR: number;
    SQLITE_IOERR_READ: number;
    SQLITE_IOERR_SHORT_READ: number;
    SQLITE_IOERR_WRITE: number;
    SQLITE_IOERR_TRUNCATE: number;
    SQLITE_IOERR_FSYNC: number;
    SQLITE_IOERR_FSTAT: number;
    SQLITE_IOERR_LOCK: number;
    SQLITE_IOERR_UNLOCK: number;
    SQLITE_IOERR_CHECKRESERVEDLOCK: number;
    SQLITE_IOERR_DELETE: number;
    SQLITE_IOERR_ACCESS: number;
    SQLITE_NOTFOUND: number;
    SQLITE_CANTOPEN: number;
    SQLITE_OPEN_CREATE: number;
    SQLITE_OPEN_DELETEONCLOSE: number;
    SQLITE_IOCAP_ATOMIC: number;
    SQLITE_IOCAP_SAFE_APPEND: number;
    SQLITE_IOCAP_SEQUENTIAL: number;
    SQLITE_IOCAP_POWERSAFE_OVERWRITE: number;
  };
  wasm: {
    readonly memory: WebAssembly.Memory;
    heap8u(): Uint8Array;
    cstrToJs(pointer: number): string | null;
    cstrncpy(target: number, source: number, maximum: number): number;
    poke(pointer: number, value: number, type: "i32" | "double"): void;
    poke64(pointer: number, value: number | bigint): void;
  };
  vfs: {
    installVfs(options: {
      io: { struct: SQLiteStruct; methods: object };
      vfs: { struct: SQLiteStruct; methods: object; name: string; asDefault: boolean };
    }): void;
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
  retrieval_json TEXT NOT NULL CHECK(json_valid(retrieval_json)),
  exact_filename TEXT,
  exact_stem TEXT,
  exact_aliases_json TEXT NOT NULL CHECK(json_valid(exact_aliases_json)),
  exact_title TEXT,
  aliases_text TEXT NOT NULL,
  title_text TEXT NOT NULL,
  tags_text TEXT NOT NULL,
  chunk_count INTEGER NOT NULL CHECK(chunk_count >= 0),
  property_count INTEGER NOT NULL CHECK(property_count >= 0),
  property_scalar_count INTEGER NOT NULL CHECK(property_scalar_count >= 0),
  CHECK(outcome = 'skipped' OR content_hash IS NOT NULL),
  CHECK(outcome = 'indexed' OR (
    chunk_count = 0 AND property_count = 0 AND property_scalar_count = 0
  )),
  UNIQUE(vault_id, path)
);

-- The legacy column name remains because the frozen lexical binder selects it,
-- but it stores only compact display metadata, never the open property bag.
CREATE TABLE chunks (
  rowid INTEGER PRIMARY KEY CHECK(rowid > 0),
  source_key TEXT NOT NULL,
  chunk_id TEXT NOT NULL UNIQUE,
  vault_id TEXT NOT NULL,
  path TEXT NOT NULL,
  heading_path_json TEXT NOT NULL,
  frontmatter_json TEXT NOT NULL CHECK(json_valid(frontmatter_json)),
  heading_text TEXT NOT NULL,
  exact_heading TEXT,
  content TEXT NOT NULL,
  identifiers_json TEXT NOT NULL CHECK(json_valid(identifiers_json))
);

CREATE INDEX chunks_by_source ON chunks(source_key);
CREATE INDEX chunks_exact_heading ON chunks(exact_heading)
  WHERE exact_heading IS NOT NULL;
CREATE INDEX sources_exact_filename ON sources(exact_filename, source_key)
  WHERE exact_filename IS NOT NULL;
CREATE INDEX sources_exact_stem ON sources(exact_stem, source_key)
  WHERE exact_stem IS NOT NULL;
CREATE INDEX sources_exact_title ON sources(exact_title, source_key)
  WHERE exact_title IS NOT NULL;

-- One row per top-level property is the durable source-level projection. The
-- canonical JSON is sufficient to rebuild a returned property bag; the root
-- scalar columns avoid reparsing for rules that address the property itself.
CREATE TABLE source_properties (
  rowid INTEGER PRIMARY KEY CHECK(rowid > 0),
  source_key TEXT NOT NULL,
  property_name TEXT NOT NULL,
  value_json TEXT NOT NULL CHECK(json_valid(value_json)),
  root_type TEXT NOT NULL
    CHECK(root_type IN ('null','boolean','i64','u64','real','string','date','array','object')),
  exact_value TEXT,
  numeric_value REAL,
  date_value TEXT,
  CHECK(root_type IN ('array','object') OR exact_value IS NOT NULL),
  CHECK(root_type NOT IN ('array','object') OR (
    exact_value IS NULL AND numeric_value IS NULL AND date_value IS NULL
  )),
  CHECK(root_type IN ('i64','u64','real') OR numeric_value IS NULL),
  CHECK(root_type = 'date' OR date_value IS NULL),
  UNIQUE(source_key, property_name)
);

-- Presence never opens an FTS cursor. The property name leads this B-tree so a
-- common property remains a single bounded lookup rather than a source scan.
CREATE INDEX source_properties_presence
  ON source_properties(property_name, source_key);

-- Every scalar occurrence contributes exactly once per source. Root scalars use
-- the empty JSON Pointer; array/map leaves use deterministic escaped pointers.
CREATE TABLE source_property_scalars (
  rowid INTEGER PRIMARY KEY CHECK(rowid > 0),
  source_key TEXT NOT NULL,
  property_name TEXT NOT NULL,
  json_pointer TEXT NOT NULL,
  scalar_type TEXT NOT NULL
    CHECK(scalar_type IN ('null','boolean','i64','u64','real','string','date')),
  exact_value TEXT NOT NULL,
  numeric_value REAL,
  date_value TEXT,
  CHECK(scalar_type IN ('i64','u64','real') OR numeric_value IS NULL),
  CHECK(scalar_type = 'date' OR date_value IS NULL),
  UNIQUE(source_key, property_name, json_pointer)
);

CREATE INDEX source_property_scalars_exact
  ON source_property_scalars(property_name, scalar_type, exact_value, source_key);
CREATE INDEX source_property_scalars_path_exact
  ON source_property_scalars(
    property_name, json_pointer, scalar_type, exact_value, source_key
  );
CREATE INDEX source_property_scalars_numeric
  ON source_property_scalars(property_name, numeric_value, source_key)
  WHERE numeric_value IS NOT NULL;
CREATE INDEX source_property_scalars_date
  ON source_property_scalars(property_name, date_value, source_key)
  WHERE date_value IS NOT NULL;

-- The lexical index remains separate and contentless. Property text has its own
-- contentless table so exact/presence rules never pay FTS cost. Types occupy
-- distinct columns: the same glyphs in a string, integer, real, boolean, or ISO
-- date cannot collide. One FTS document per top-level property deduplicates
-- repeated leaves before ranking; a later deadline manager can issue, count,
-- limit, and drop one property-text query per configured rule.
CREATE VIRTUAL TABLE source_property_text_fts USING fts5(
  string_value,
  integer_value,
  real_value,
  boolean_value,
  date_value,
  content='',
  contentless_delete=1,
  tokenize='unicode61 remove_diacritics 2'
);

-- Canonical lexical inputs are source-owned or chunk-local and occur once in
-- ordinary tables. The external-content view joins them without repeating an
-- unbounded title/tag/alias projection per chunk in durable canonical state.
CREATE VIEW chunk_search AS
SELECT
  c.rowid AS rowid,
  json_extract(s.retrieval_json, '$.filename') AS filename,
  json_extract(s.retrieval_json, '$.stem') AS stem,
  s.aliases_text AS aliases,
  s.title_text AS title,
  c.heading_text AS heading_text,
  s.path AS path_text,
  s.tags_text AS tags,
  c.content AS content,
  c.identifiers_json AS identifiers
FROM chunks c JOIN sources s ON s.source_key = c.source_key;

-- External content lets FTS5's rank-1 integrity check compare every posting to
-- canonical source/chunk text. Ordinary queries still select only compact chunk
-- metadata and never hydrate the canonical content or open property rows.
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
  content='chunk_search',
  content_rowid='rowid',
  tokenize='unicode61 remove_diacritics 2'
);

`;

interface ExpectedSchemaObject {
  type: "index" | "table" | "view";
  name: string;
  table: string;
  sql: string | null;
}

// Exact objects emitted by the pinned 3.53.0 runtime for SCHEMA_SQL. Comparing
// names alone would accept a semantically different FTS declaration or an
// added trigger/table; comparing normalized SQL keeps the check fail-able.
const EXPECTED_SCHEMA_OBJECTS: readonly ExpectedSchemaObject[] = [
  { type: "index", name: "chunks_by_source", table: "chunks", sql: "CREATE INDEX chunks_by_source ON chunks(source_key)" },
  { type: "index", name: "chunks_exact_heading", table: "chunks", sql: "CREATE INDEX chunks_exact_heading ON chunks(exact_heading) WHERE exact_heading IS NOT NULL" },
  { type: "index", name: "source_properties_presence", table: "source_properties", sql: "CREATE INDEX source_properties_presence ON source_properties(property_name, source_key)" },
  { type: "index", name: "source_property_scalars_date", table: "source_property_scalars", sql: "CREATE INDEX source_property_scalars_date ON source_property_scalars(property_name, date_value, source_key) WHERE date_value IS NOT NULL" },
  { type: "index", name: "source_property_scalars_exact", table: "source_property_scalars", sql: "CREATE INDEX source_property_scalars_exact ON source_property_scalars(property_name, scalar_type, exact_value, source_key)" },
  { type: "index", name: "source_property_scalars_numeric", table: "source_property_scalars", sql: "CREATE INDEX source_property_scalars_numeric ON source_property_scalars(property_name, numeric_value, source_key) WHERE numeric_value IS NOT NULL" },
  { type: "index", name: "source_property_scalars_path_exact", table: "source_property_scalars", sql: "CREATE INDEX source_property_scalars_path_exact ON source_property_scalars(property_name, json_pointer, scalar_type, exact_value, source_key)" },
  { type: "index", name: "sources_exact_filename", table: "sources", sql: "CREATE INDEX sources_exact_filename ON sources(exact_filename, source_key) WHERE exact_filename IS NOT NULL" },
  { type: "index", name: "sources_exact_stem", table: "sources", sql: "CREATE INDEX sources_exact_stem ON sources(exact_stem, source_key) WHERE exact_stem IS NOT NULL" },
  { type: "index", name: "sources_exact_title", table: "sources", sql: "CREATE INDEX sources_exact_title ON sources(exact_title, source_key) WHERE exact_title IS NOT NULL" },
  { type: "index", name: "sqlite_autoindex_chunks_1", table: "chunks", sql: null },
  { type: "index", name: "sqlite_autoindex_source_properties_1", table: "source_properties", sql: null },
  { type: "index", name: "sqlite_autoindex_source_property_scalars_1", table: "source_property_scalars", sql: null },
  { type: "index", name: "sqlite_autoindex_sources_1", table: "sources", sql: null },
  { type: "index", name: "sqlite_autoindex_sources_2", table: "sources", sql: null },
  { type: "table", name: "chunks", table: "chunks", sql: "CREATE TABLE chunks (rowid INTEGER PRIMARY KEY CHECK(rowid > 0),source_key TEXT NOT NULL,chunk_id TEXT NOT NULL UNIQUE,vault_id TEXT NOT NULL,path TEXT NOT NULL,heading_path_json TEXT NOT NULL,frontmatter_json TEXT NOT NULL CHECK(json_valid(frontmatter_json)),heading_text TEXT NOT NULL,exact_heading TEXT,content TEXT NOT NULL,identifiers_json TEXT NOT NULL CHECK(json_valid(identifiers_json)))" },
  { type: "table", name: "chunks_fts", table: "chunks_fts", sql: "CREATE VIRTUAL TABLE chunks_fts USING fts5(filename,stem,aliases,title,heading_text,path_text,tags,content,identifiers,content='chunk_search',content_rowid='rowid',tokenize='unicode61 remove_diacritics 2')" },
  { type: "table", name: "chunks_fts_config", table: "chunks_fts_config", sql: "CREATE TABLE 'chunks_fts_config'(k PRIMARY KEY, v) WITHOUT ROWID" },
  { type: "table", name: "chunks_fts_data", table: "chunks_fts_data", sql: "CREATE TABLE 'chunks_fts_data'(id INTEGER PRIMARY KEY, block BLOB)" },
  { type: "table", name: "chunks_fts_docsize", table: "chunks_fts_docsize", sql: "CREATE TABLE 'chunks_fts_docsize'(id INTEGER PRIMARY KEY, sz BLOB)" },
  { type: "table", name: "chunks_fts_idx", table: "chunks_fts_idx", sql: "CREATE TABLE 'chunks_fts_idx'(segid, term, pgno, PRIMARY KEY(segid, term)) WITHOUT ROWID" },
  { type: "table", name: "source_properties", table: "source_properties", sql: "CREATE TABLE source_properties (rowid INTEGER PRIMARY KEY CHECK(rowid > 0),source_key TEXT NOT NULL,property_name TEXT NOT NULL,value_json TEXT NOT NULL CHECK(json_valid(value_json)),root_type TEXT NOT NULL CHECK(root_type IN ('null','boolean','i64','u64','real','string','date','array','object')),exact_value TEXT,numeric_value REAL,date_value TEXT,CHECK(root_type IN ('array','object') OR exact_value IS NOT NULL),CHECK(root_type NOT IN ('array','object') OR (exact_value IS NULL AND numeric_value IS NULL AND date_value IS NULL)),CHECK(root_type IN ('i64','u64','real') OR numeric_value IS NULL),CHECK(root_type = 'date' OR date_value IS NULL),UNIQUE(source_key, property_name))" },
  { type: "table", name: "source_property_scalars", table: "source_property_scalars", sql: "CREATE TABLE source_property_scalars (rowid INTEGER PRIMARY KEY CHECK(rowid > 0),source_key TEXT NOT NULL,property_name TEXT NOT NULL,json_pointer TEXT NOT NULL,scalar_type TEXT NOT NULL CHECK(scalar_type IN ('null','boolean','i64','u64','real','string','date')),exact_value TEXT NOT NULL,numeric_value REAL,date_value TEXT,CHECK(scalar_type IN ('i64','u64','real') OR numeric_value IS NULL),CHECK(scalar_type = 'date' OR date_value IS NULL),UNIQUE(source_key, property_name, json_pointer))" },
  { type: "table", name: "source_property_text_fts", table: "source_property_text_fts", sql: "CREATE VIRTUAL TABLE source_property_text_fts USING fts5(string_value,integer_value,real_value,boolean_value,date_value,content='',contentless_delete=1,tokenize='unicode61 remove_diacritics 2')" },
  { type: "table", name: "source_property_text_fts_config", table: "source_property_text_fts_config", sql: "CREATE TABLE 'source_property_text_fts_config'(k PRIMARY KEY, v) WITHOUT ROWID" },
  { type: "table", name: "source_property_text_fts_data", table: "source_property_text_fts_data", sql: "CREATE TABLE 'source_property_text_fts_data'(id INTEGER PRIMARY KEY, block BLOB)" },
  { type: "table", name: "source_property_text_fts_docsize", table: "source_property_text_fts_docsize", sql: "CREATE TABLE 'source_property_text_fts_docsize'(id INTEGER PRIMARY KEY, sz BLOB, origin INTEGER)" },
  { type: "table", name: "source_property_text_fts_idx", table: "source_property_text_fts_idx", sql: "CREATE TABLE 'source_property_text_fts_idx'(segid, term, pgno, PRIMARY KEY(segid, term)) WITHOUT ROWID" },
  { type: "table", name: "sources", table: "sources", sql: "CREATE TABLE sources (source_key TEXT PRIMARY KEY,vault_id TEXT NOT NULL,path TEXT NOT NULL,outcome TEXT NOT NULL CHECK(outcome IN ('indexed','skipped')),content_hash TEXT,byte_length INTEGER NOT NULL CHECK(byte_length >= 0),mtime_nanos TEXT NOT NULL CHECK(mtime_nanos <> '' AND mtime_nanos NOT GLOB '*[^0-9]*'),retrieval_json TEXT NOT NULL CHECK(json_valid(retrieval_json)),exact_filename TEXT,exact_stem TEXT,exact_aliases_json TEXT NOT NULL CHECK(json_valid(exact_aliases_json)),exact_title TEXT,aliases_text TEXT NOT NULL,title_text TEXT NOT NULL,tags_text TEXT NOT NULL,chunk_count INTEGER NOT NULL CHECK(chunk_count >= 0),property_count INTEGER NOT NULL CHECK(property_count >= 0),property_scalar_count INTEGER NOT NULL CHECK(property_scalar_count >= 0),CHECK(outcome = 'skipped' OR content_hash IS NOT NULL),CHECK(outcome = 'indexed' OR (chunk_count = 0 AND property_count = 0 AND property_scalar_count = 0)),UNIQUE(vault_id, path))" },
  { type: "view", name: "chunk_search", table: "chunk_search", sql: "CREATE VIEW chunk_search AS SELECT c.rowid AS rowid,json_extract(s.retrieval_json,'$.filename') AS filename,json_extract(s.retrieval_json,'$.stem') AS stem,s.aliases_text AS aliases,s.title_text AS title,c.heading_text AS heading_text,s.path AS path_text,s.tags_text AS tags,c.content AS content,c.identifiers_json AS identifiers FROM chunks c JOIN sources s ON s.source_key = c.source_key" },
];

const SOURCE_COLUMNS_SQL = `
source_key, vault_id, path, outcome, content_hash, byte_length,
mtime_nanos, retrieval_json, exact_filename, exact_stem, exact_aliases_json, exact_title,
aliases_text, title_text, tags_text, chunk_count, property_count, property_scalar_count
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

const SELECT_SOURCES_BY_VAULT_SQL = `
SELECT ${SOURCE_COLUMNS_SQL}
FROM sources
WHERE vault_id = ?
ORDER BY path
LIMIT ?
`;

const INSERT_SOURCE_SQL = `
INSERT INTO sources(${SOURCE_COLUMNS_SQL}) VALUES(?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
`;

const INSERT_PROPERTY_SQL = `
INSERT INTO source_properties(
  rowid, source_key, property_name, value_json, root_type,
  exact_value, numeric_value, date_value
) VALUES(?, ?, ?, ?, ?, ?, ?, ?)
`;

const INSERT_PROPERTY_SCALAR_SQL = `
INSERT INTO source_property_scalars(
  source_key, property_name, json_pointer, scalar_type,
  exact_value, numeric_value, date_value
) VALUES(?, ?, ?, ?, ?, ?, ?)
`;

const INSERT_PROPERTY_TEXT_FTS_SQL = `
INSERT INTO source_property_text_fts(
  rowid, string_value, integer_value, real_value, boolean_value, date_value
) VALUES(?, ?, ?, ?, ?, ?)
`;

const INSERT_CHUNK_SQL = `
INSERT INTO chunks(
  rowid, source_key, chunk_id, vault_id, path, heading_path_json, frontmatter_json,
  heading_text, exact_heading, content, identifiers_json
) VALUES(?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
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

const DELETE_SOURCE_PROPERTY_FTS_SQL = `
DELETE FROM source_property_text_fts
WHERE rowid IN (SELECT rowid FROM source_properties WHERE source_key = ?)
`;

const DELETE_SOURCE_CHUNKS_SQL = "DELETE FROM chunks WHERE source_key = ?";
const DELETE_SOURCE_PROPERTY_SCALARS_SQL =
  "DELETE FROM source_property_scalars WHERE source_key = ?";
const DELETE_SOURCE_PROPERTIES_SQL = "DELETE FROM source_properties WHERE source_key = ?";
const DELETE_SOURCE_SQL = "DELETE FROM sources WHERE source_key = ?";

// Seeded from BOTH tables. `chunks` alone is not enough: a posting that exists
// in `chunks_fts` without a matching metadata row is exactly the state the
// allocator has to route around, and such a posting is invisible to
// `MAX(rowid) FROM chunks`. Reusing its rowid would shadow it — the contentless
// index accepts the duplicate silently, keeps the shadowed posting alive across
// the next delete, and reconciliation would then see matching counts and no
// orphan. Allocating above both tables keeps any such posting visible as an
// orphan, which `runReconciliationCheck` does fail on.
const SELECT_MAX_CHUNK_ROWID_SQL = `
SELECT MAX(
  (SELECT COALESCE(MAX(rowid), 0) FROM chunks),
  (SELECT COALESCE(MAX(id), 0) FROM chunks_fts_docsize)
)
`;

const SELECT_MAX_PROPERTY_ROWID_SQL = `
SELECT MAX(
  (SELECT COALESCE(MAX(rowid), 0) FROM source_properties),
  (SELECT COALESCE(MAX(rowid), 0) FROM source_property_text_fts)
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
  (SELECT count(*) FROM chunks WHERE rowid <= 0) AS nonpositive_chunks,
  (SELECT count(*) FROM chunks_fts_docsize) AS fts,
  (SELECT count(*) FROM chunks_fts_docsize WHERE id <= 0) AS nonpositive_fts,
  (SELECT count(*) FROM chunks_fts_docsize f LEFT JOIN chunks c ON c.rowid = f.id
     WHERE c.rowid IS NULL) AS orphan_fts,
  (SELECT count(*) FROM chunks c LEFT JOIN chunks_fts_docsize f ON f.id = c.rowid
     WHERE f.id IS NULL) AS missing_fts,
  (SELECT count(*) FROM source_properties) AS properties,
  (SELECT count(*) FROM source_properties WHERE rowid <= 0) AS nonpositive_properties,
  (SELECT count(*) FROM source_property_scalars) AS property_scalars,
  (SELECT count(*) FROM source_property_scalars WHERE rowid <= 0) AS nonpositive_property_scalars,
  (SELECT count(*) FROM source_property_text_fts) AS property_fts,
  (SELECT count(*) FROM source_property_text_fts WHERE rowid <= 0) AS nonpositive_property_fts,
  (SELECT count(*) FROM source_property_text_fts f
     LEFT JOIN source_properties p ON p.rowid = f.rowid
     WHERE p.rowid IS NULL) AS orphan_property_fts,
  (SELECT count(*) FROM source_properties p
     LEFT JOIN source_property_text_fts f ON f.rowid = p.rowid
     WHERE f.rowid IS NULL) AS missing_property_fts,
  (SELECT count(*) FROM sources) AS sources,
  (SELECT count(*) FROM sources WHERE outcome = 'indexed') AS indexed_sources,
  (SELECT COALESCE(SUM(chunk_count), 0) FROM sources) AS source_chunks,
  (SELECT COALESCE(SUM(property_count), 0) FROM sources) AS source_properties,
  (SELECT COALESCE(SUM(property_scalar_count), 0) FROM sources) AS source_property_scalars,
  (SELECT count(*) FROM chunks c LEFT JOIN sources s ON s.source_key = c.source_key
     WHERE s.source_key IS NULL) AS orphan_chunks,
  (SELECT count(*) FROM source_properties p LEFT JOIN sources s ON s.source_key = p.source_key
     WHERE s.source_key IS NULL OR s.outcome <> 'indexed') AS orphan_properties,
  (SELECT count(*) FROM source_property_scalars p
     LEFT JOIN source_properties r
       ON r.source_key = p.source_key AND r.property_name = p.property_name
     WHERE r.rowid IS NULL) AS orphan_property_scalars,
  (SELECT count(*) FROM sources s WHERE s.outcome = 'skipped' AND (
     EXISTS(SELECT 1 FROM chunks c WHERE c.source_key = s.source_key)
     OR EXISTS(SELECT 1 FROM source_properties p WHERE p.source_key = s.source_key)
     OR EXISTS(SELECT 1 FROM source_property_scalars p WHERE p.source_key = s.source_key)
  )) AS skipped_with_rows,
  (SELECT count(*) FROM sources s WHERE
     s.chunk_count <> (SELECT count(*) FROM chunks c WHERE c.source_key = s.source_key)
     OR s.property_count <>
       (SELECT count(*) FROM source_properties p WHERE p.source_key = s.source_key)
     OR s.property_scalar_count <>
       (SELECT count(*) FROM source_property_scalars p WHERE p.source_key = s.source_key)
  ) AS source_tally_mismatches
`;

interface ExistingGenerationState {
  documents: number;
  chunks: number;
  sources: number;
  chunkingVersion: number;
  exportImage: () => Uint8Array;
  close: () => void;
}

export class Fts5GenerationIndex {
  private documentCount = 0;
  private chunkCount = 0;
  private sourceCount = 0;
  private observedChunkingVersion: number | null = null;
  private closed = false;
  private readonly limits: ResolvedFts5IndexLimits;
  private readonly effectiveDatabaseByteLimit: number;
  private readonly exportStrategy: (api: SQLiteApi) => Uint8Array;
  private readonly closeStrategy: () => void;
  private readonly compactOnExport: boolean;

  constructor(
    private readonly db: SQLiteDatabase,
    limits: Fts5IndexLimits = DEFAULT_INDEX_LIMITS,
    existing?: ExistingGenerationState,
  ) {
    this.limits = resolveIndexLimits(limits);
    this.effectiveDatabaseByteLimit = configureDatabasePageLimit(db, this.limits);
    if (existing === undefined) {
      if (db.filename !== ":memory:") throw new Error("FTS5 generation is not in memory");
      try {
        db.exec(SCHEMA_SQL);
        installVocabularyTable(db);
      } catch (error) {
        throw translateSqliteCapacityError(error);
      }
      requireDatabaseWithinLimit(db, this.effectiveDatabaseByteLimit);
      this.exportStrategy = (api) => api.capi.sqlite3_js_db_export(this.db.pointer);
      this.closeStrategy = () => this.db.close();
      this.compactOnExport = true;
      return;
    }

    installVocabularyTable(db);
    requireProjectedCounts(
      existing.documents,
      existing.chunks,
      existing.sources,
      this.limits,
    );
    requireDatabaseWithinLimit(db, this.effectiveDatabaseByteLimit);
    this.documentCount = existing.documents;
    this.chunkCount = existing.chunks;
    this.sourceCount = existing.sources;
    this.observedChunkingVersion = existing.chunks === 0 ? null : existing.chunkingVersion;
    this.exportStrategy = () => existing.exportImage();
    this.closeStrategy = existing.close;
    // The selected mechanism exports directly from JS blocks. Running VACUUM
    // before that export grows the non-shrinking WASM heap and defeats the
    // mechanism; restored images are already compact at the point they enter.
    this.compactOnExport = false;
  }

  get requiresPreExportCompaction(): boolean {
    return this.compactOnExport;
  }

  /** Indexed sources only. Skipped sources are recorded but are not documents. */
  get documents(): number {
    return this.documentCount;
  }

  get chunks(): number {
    return this.chunkCount;
  }

  /** Full durable SQLite footprint, including schema, indexes, FTS, and freelist pages. */
  get databaseBytes(): number {
    this.requireOpen();
    return measuredDatabaseBytes(this.db);
  }

  get databaseByteLimit(): number {
    return this.effectiveDatabaseByteLimit;
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
    const addedChunks = sumSafe(indexed.map((change) => change.rows.length));
    const nextDocuments = this.documentCount - removedDocuments + indexed.length;
    const nextChunks = this.chunkCount - removedChunks + addedChunks;
    const nextSources = this.sourceCount - touched.size + projected.length;
    const nextChunkingVersion = requireSingleChunkingVersion(
      this.observedChunkingVersion,
      projected,
    );
    requireProjectedCounts(nextDocuments, nextChunks, nextSources, this.limits);

    let mutationError: unknown;
    try {
      this.db.transaction("IMMEDIATE", () => {
        try {
          // Each allocator is seeded from its metadata and contentless tables before
          // any delete, so a freed rowid can never shadow a surviving posting.
          let nextChunkRowid = requireRowidSeed(
            this.db.selectValue(SELECT_MAX_CHUNK_ROWID_SQL),
          );
          let nextPropertyRowid = requireRowidSeed(
            this.db.selectValue(SELECT_MAX_PROPERTY_ROWID_SQL),
          );
          for (const stored of touched.values()) {
            // Contentless postings must be cleared before their metadata rows.
            this.db.exec(DELETE_SOURCE_FTS_SQL, { bind: [stored.source_key] });
            this.db.exec(DELETE_SOURCE_PROPERTY_FTS_SQL, { bind: [stored.source_key] });
            this.db.exec(DELETE_SOURCE_CHUNKS_SQL, { bind: [stored.source_key] });
            this.db.exec(DELETE_SOURCE_PROPERTY_SCALARS_SQL, { bind: [stored.source_key] });
            this.db.exec(DELETE_SOURCE_PROPERTIES_SQL, { bind: [stored.source_key] });
            this.db.exec(DELETE_SOURCE_SQL, { bind: [stored.source_key] });
          }
          for (const change of projected) {
            let propertyCount = 0;
            let propertyScalarCount = 0;
            if (change.preparation.kind === "indexed") {
              for (const propertyName of Object.keys(change.frontmatter).sort(comparePaths)) {
                const value = change.frontmatter[propertyName]!;
                const rowid = ++nextPropertyRowid;
                if (!Number.isSafeInteger(rowid)) {
                  throw new Error("property rowid space exhausted");
                }
                const root = projectRoot(value);
                this.db.exec(INSERT_PROPERTY_SQL, {
                  bind: [
                    rowid,
                    change.preparation.source_key,
                    propertyName,
                    canonicalJson(value),
                    root.type,
                    root.exactValue,
                    root.numericValue,
                    root.dateValue,
                  ],
                });
                const aggregate = emptyPropertyTextAggregate();
                for (const leaf of iteratePropertyScalars(value)) {
                  addPropertyText(aggregate, leaf.scalar);
                  this.db.exec(INSERT_PROPERTY_SCALAR_SQL, {
                    bind: [
                      change.preparation.source_key,
                      propertyName,
                      leaf.jsonPointer,
                      leaf.scalar.type,
                      leaf.scalar.exactValue,
                      leaf.scalar.numericValue,
                      leaf.scalar.dateValue,
                    ],
                  });
                  propertyScalarCount += 1;
                }
                this.db.exec(INSERT_PROPERTY_TEXT_FTS_SQL, {
                  bind: [rowid, ...finishPropertyTextAggregate(aggregate)],
                });
                propertyCount += 1;
              }
            }
            for (const row of change.rows) {
              const rowid = ++nextChunkRowid;
              if (!Number.isSafeInteger(rowid)) throw new Error("chunk rowid space exhausted");
              this.db.exec(INSERT_CHUNK_SQL, {
                bind: [rowid, change.preparation.source_key, ...row.metadataBind],
              });
              this.db.exec(INSERT_CHUNK_FTS_SQL, { bind: [rowid, ...row.ftsBind] });
            }
            // The owning source row lands last, after its final deterministic tallies
            // are known. A SQLITE_FULL anywhere above rolls the complete source batch back.
            this.db.exec(INSERT_SOURCE_SQL, {
              bind: [
                change.preparation.source_key,
                change.preparation.vault_id,
                change.preparation.path,
                change.preparation.kind,
                change.preparation.content_hash,
                change.preparation.byte_length,
                change.preparation.mtime_nanos,
                retrievalJson(change.preparation),
                change.preparation.normalized_exact.filename,
                change.preparation.normalized_exact.stem,
                JSON.stringify(change.preparation.normalized_exact.aliases),
                change.preparation.normalized_exact.title,
                change.preparation.kind === "indexed"
                  ? change.preparation.retrieval.aliases.join(" ")
                  : "",
                change.preparation.kind === "indexed" ? legacyTitle(change.frontmatter) : "",
                change.preparation.kind === "indexed" ? legacyTags(change.frontmatter) : "",
                change.rows.length,
                propertyCount,
                propertyScalarCount,
              ],
            });
          }
          if (verifyIntegrity) {
            this.runIntegrityCheck();
            this.runReconciliationCheck(nextChunks, nextDocuments, nextSources);
          }
          requireDatabaseWithinLimit(this.db, this.effectiveDatabaseByteLimit);
        } catch (error) {
          mutationError = error;
          throw error;
        }
      });
    } catch (error) {
      throw translateSqliteCapacityError(mutationError ?? error);
    }

    this.documentCount = nextDocuments;
    this.chunkCount = nextChunks;
    this.sourceCount = nextSources;
    this.observedChunkingVersion = nextChunkingVersion;
  }

  planReconciliation(
    vaultId: string,
    current: readonly ReconciliationSourceMetadata[],
  ): Omit<ReconciliationPlanResult, "generation"> {
    this.requireOpen();
    const rows = this.db.selectObjects(
      SELECT_SOURCES_BY_VAULT_SQL,
      [vaultId, onePastLimit(MAX_RECONCILIATION_SOURCES)],
    );
    if (rows.length > MAX_RECONCILIATION_SOURCES) throw new IndexCapacityError();
    const stored = new Map<string, StoredSource>();
    for (const row of rows) {
      const source = parseStoredSource([row]);
      if (source === null || source.vault_id !== vaultId || stored.has(source.path)) {
        throw new IndexIntegrityError("stored source inventory is invalid");
      }
      stored.set(source.path, source);
    }

    const storedSourceCount = stored.size;
    let matchedSourceCount = 0;
    const unchanged: string[] = [];
    const audit: Array<{ path: string; content_hash: string }> = [];
    const refresh: string[] = [];
    for (const source of current) {
      const previous = stored.get(source.path);
      if (previous) matchedSourceCount += 1;
      stored.delete(source.path);
      const previousWasOversized = previous?.outcome === "skipped"
        && previous.content_hash === null;
      if (previous
        && previous.byte_length === source.byte_length
        && previous.mtime_nanos === source.mtime_nanos
        && previousWasOversized === !source.indexable) {
        if (previousWasOversized) unchanged.push(source.path);
        else if (previous.content_hash !== null) {
          audit.push({ path: source.path, content_hash: previous.content_hash });
        } else {
          refresh.push(source.path);
        }
      } else {
        refresh.push(source.path);
      }
    }
    return {
      unchanged,
      audit,
      refresh,
      remove: [...stored.keys()].sort(comparePaths),
      stored_source_count: storedSourceCount,
      matched_source_count: matchedSourceCount,
    };
  }

  observeQuery(probes: readonly EvidenceProbePlan[]): QueryEvidenceObservation {
    this.requireOpen();
    let identifierProbeMatched: boolean | null = null;
    const termSupport: QueryEvidenceObservation["term_support"] = [];
    const prefixExpansions: QueryEvidenceObservation["prefix_expansions"] = [];
    for (const probe of probes) {
      const bound = bindEvidenceProbe(probe);
      const matched = Number(this.db.selectValue(bound.exists.sql, bound.exists.bind)) === 1;
      if (probe.plan_id === "identifier_metadata_v2") {
        if (identifierProbeMatched !== null) throw new Error("duplicate identifier metadata probe");
        identifierProbeMatched = matched;
        continue;
      }
      const terms = bound.prefix === null
        ? []
        : this.db.selectObjects(bound.prefix.sql, bound.prefix.bind).map((row) => {
            if (typeof row.term !== "string" || row.term.length === 0) {
              throw new Error("SQLite returned an invalid prefix expansion");
            }
            return row.term;
          });
      if (terms.length > probe.max_prefix_expansions) {
        terms.length = probe.max_prefix_expansions;
      }
      termSupport.push({
        probe_id: probe.probe_id,
        term_index: probe.term_index,
        document_frequency: matched ? 1 : 0,
        prefix_expansions: terms.length,
      });
      prefixExpansions.push({
        probe_id: probe.probe_id,
        term_index: probe.term_index,
        terms,
      });
    }
    return {
      identifier_probe_matched: identifierProbeMatched,
      term_support: termSupport,
      prefix_expansions: prefixExpansions,
    };
  }

  search(plan: ExecutionPlan, limit: number): WorkerSearchHit[] {
    this.requireOpen();
    requireExecutionPlanIdentity(plan);
    if (!Number.isSafeInteger(limit) || limit < 1 || limit > 100) {
      throw new Error("invalid FTS5 search limit");
    }
    if (plan.disposition === "empty_no_evidence") return [];
    const hits: WorkerSearchHit[] = [];
    const seen = new Set<string>();
    let candidates = 0;
    for (const stage of plan.stages) {
      if (hits.length === limit || candidates === plan.max_total_candidates) break;
      const stageLimit = Math.min(
        stage.max_candidates,
        plan.max_total_candidates - candidates,
      );
      if (stageLimit < 1) break;
      const bound = bindSearchStage(stage, stageLimit);
      const rows = this.db.selectObjects(bound.sql, bound.bind).map(parseSearchRow);
      candidates += rows.length;
      for (const hit of rows) {
        if (seen.has(hit.chunk_id)) continue;
        seen.add(hit.chunk_id);
        hits.push(hit);
        if (hits.length === limit) break;
      }
    }
    return hits;
  }

  assertIntegrity(): void {
    this.requireOpen();
    try {
      this.runIntegrityCheck();
      this.runReconciliationCheck(
        this.chunkCount,
        this.documentCount,
        this.sourceCount,
      );
      requireDatabaseWithinLimit(this.db, this.effectiveDatabaseByteLimit);
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
    const image = this.exportStrategy(api);
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
    try {
      this.db.exec("INSERT INTO chunks_fts(chunks_fts) VALUES('optimize')");
      this.db.exec(
        "INSERT INTO source_property_text_fts(source_property_text_fts) VALUES('optimize')",
      );
      this.db.exec("VACUUM");
      configureDatabasePageLimit(this.db, this.limits);
      requireDatabaseWithinLimit(this.db, this.effectiveDatabaseByteLimit);
    } catch (error) {
      throw translateSqliteCapacityError(error);
    }
  }

  close(): void {
    if (this.closed) return;
    this.closed = true;
    this.closeStrategy();
    if (this.db.pointer !== undefined) throw new Error("SQLite database remained open");
    this.documentCount = 0;
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
    try {
      this.db.exec("INSERT INTO chunks_fts(chunks_fts, rank) VALUES('integrity-check', 1)");
      this.db.exec(
        "INSERT INTO source_property_text_fts(source_property_text_fts, rank) "
          + "VALUES('integrity-check', 1)",
      );
    } catch {
      throw new IndexIntegrityError("FTS5 postings disagree with canonical content");
    }
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
      || !isNonNegativeSafeInteger(row.nonpositive_chunks)
      || !isNonNegativeSafeInteger(row.fts)
      || !isNonNegativeSafeInteger(row.nonpositive_fts)
      || !isNonNegativeSafeInteger(row.orphan_fts)
      || !isNonNegativeSafeInteger(row.missing_fts)
      || !isNonNegativeSafeInteger(row.properties)
      || !isNonNegativeSafeInteger(row.nonpositive_properties)
      || !isNonNegativeSafeInteger(row.property_scalars)
      || !isNonNegativeSafeInteger(row.nonpositive_property_scalars)
      || !isNonNegativeSafeInteger(row.property_fts)
      || !isNonNegativeSafeInteger(row.nonpositive_property_fts)
      || !isNonNegativeSafeInteger(row.orphan_property_fts)
      || !isNonNegativeSafeInteger(row.missing_property_fts)
      || !isNonNegativeSafeInteger(row.sources)
      || !isNonNegativeSafeInteger(row.indexed_sources)
      || !isNonNegativeSafeInteger(row.source_chunks)
      || !isNonNegativeSafeInteger(row.source_properties)
      || !isNonNegativeSafeInteger(row.source_property_scalars)
      || !isNonNegativeSafeInteger(row.orphan_chunks)
      || !isNonNegativeSafeInteger(row.orphan_properties)
      || !isNonNegativeSafeInteger(row.orphan_property_scalars)
      || !isNonNegativeSafeInteger(row.skipped_with_rows)
      || !isNonNegativeSafeInteger(row.source_tally_mismatches)) {
      throw new IndexIntegrityError("FTS5 reconciliation query returned invalid counts");
    }
    if (row.nonpositive_chunks !== 0
      || row.nonpositive_fts !== 0
      || row.nonpositive_properties !== 0
      || row.nonpositive_property_scalars !== 0
      || row.nonpositive_property_fts !== 0
      || row.orphan_fts !== 0
      || row.missing_fts !== 0
      || row.chunks !== row.fts
      || row.properties !== row.property_fts
      || row.chunks !== expectedChunks
      || row.sources !== expectedSources
      || row.indexed_sources !== expectedDocuments
      // Per-source tallies must sum to the real row counts. No chunk, property,
      // scalar, or contentless posting may outlive the source-level row that
      // owns it, and skipped sources may never own indexed rows.
      || row.source_chunks !== row.chunks
      || row.source_properties !== row.properties
      || row.source_property_scalars !== row.property_scalars
      || row.orphan_chunks !== 0
      || row.orphan_properties !== 0
      || row.orphan_property_scalars !== 0
      || row.orphan_property_fts !== 0
      || row.missing_property_fts !== 0
      || row.skipped_with_rows !== 0
      || row.source_tally_mismatches !== 0) {
      throw new IndexIntegrityError("FTS5 index and chunk metadata disagree");
    }
  }

  private requireOpen(): void {
    if (this.closed) throw new Error("FTS5 generation is closed");
  }
}

function installVocabularyTable(db: SQLiteDatabase): void {
  db.exec(
    "CREATE VIRTUAL TABLE temp.chunks_fts_vocab "
      + "USING fts5vocab(main, chunks_fts, 'col')",
  );
}

export function openFts5Generation(
  sqlite: SQLiteApi,
  limits: Fts5IndexLimits = DEFAULT_INDEX_LIMITS,
): Fts5GenerationIndex {
  return new Fts5GenerationIndex(new sqlite.oo1.DB(":memory:", "c"), limits);
}

/**
 * Opens an already-digest-verified image through the plain-block VFS, validates
 * every stored fact before hydrating counters, and returns an index which owns
 * the VFS registration and block store for its full lifetime.
 */
export function openRestoredFts5Generation(
  sqlite: SQLiteApi,
  bytes: Uint8Array,
  chunkingVersion: number,
  limits: Fts5IndexLimits = DEFAULT_INDEX_LIMITS,
): Fts5GenerationIndex {
  let handle: BlockVfsHandle | null = null;
  try {
    handle = openPlainBlockVfs(sqlite, bytes);
    const db = handle.db;
    const storedVersion = Number(db.selectValue("PRAGMA user_version"));
    if (storedVersion !== CACHE_SCHEMA_VERSION) throw new CacheVersionMismatchError();
    validateExactSchema(db);
    const resolvedLimits = resolveIndexLimits(limits);
    const effectiveDatabaseByteLimit = configureDatabasePageLimit(db, resolvedLimits);
    requireDatabaseWithinLimit(db, effectiveDatabaseByteLimit);
    let integrity: unknown;
    try {
      integrity = db.selectValue("PRAGMA integrity_check");
    } catch {
      throw new CacheImageInvalidError("cache image failed SQLite integrity validation");
    }
    if (integrity !== "ok") {
      throw new CacheImageInvalidError("cache image failed SQLite integrity validation");
    }
    try {
      db.exec("INSERT INTO chunks_fts(chunks_fts, rank) VALUES('integrity-check', 1)");
      db.exec(
        "INSERT INTO source_property_text_fts(source_property_text_fts, rank) "
          + "VALUES('integrity-check', 1)",
      );
    } catch {
      throw new CacheImageInvalidError("cache image failed FTS5 integrity validation");
    }
    validatePositiveRestoredRowids(db);
    const inventory = readRestoredInventory(db, limits);
    validateChunkFtsBijection(db);
    validatePropertyFtsBijection(db);
    rebuildCanonicalPropertyFts(db);
    try {
      db.exec("INSERT INTO chunks_fts(chunks_fts, rank) VALUES('integrity-check', 1)");
      db.exec(
        "INSERT INTO source_property_text_fts(source_property_text_fts, rank) "
          + "VALUES('integrity-check', 1)",
      );
    } catch (error) {
      throw translateSqliteCapacityError(error);
    }
    requireDatabaseWithinLimit(db, effectiveDatabaseByteLimit);
    const ownedHandle = handle;
    const index = new Fts5GenerationIndex(db, limits, {
      ...inventory,
      chunkingVersion,
      exportImage: () => ownedHandle.exportImage(),
      close: () => ownedHandle.close(),
    });
    try {
      index.assertIntegrity();
    } catch {
      index.close();
      throw new CacheImageInvalidError("cache image metadata and postings disagree");
    }
    handle = null;
    return index;
  } catch (error) {
    handle?.close();
    if (error instanceof CacheVersionMismatchError
      || error instanceof CacheImageInvalidError
      || error instanceof IndexCapacityError) {
      throw error;
    }
    throw error;
  }
}

function validateExactSchema(db: SQLiteDatabase): void {
  const rows = db.selectObjects(`
    SELECT type, name, tbl_name, sql
    FROM sqlite_schema
    WHERE type IN ('table', 'index', 'view', 'trigger')
    ORDER BY type, name
  `);
  if (rows.length !== EXPECTED_SCHEMA_OBJECTS.length) {
    throw new CacheImageInvalidError("cache image schema object set differs");
  }
  for (let index = 0; index < EXPECTED_SCHEMA_OBJECTS.length; index += 1) {
    const actual = rows[index];
    const expected = EXPECTED_SCHEMA_OBJECTS[index];
    if (!actual
      || !expected
      || actual.type !== expected.type
      || actual.name !== expected.name
      || actual.tbl_name !== expected.table
      || !(actual.sql === null || typeof actual.sql === "string")
      || normalizeSchemaSql(actual.sql) !== normalizeSchemaSql(expected.sql)) {
      throw new CacheImageInvalidError("cache image schema object differs");
    }
  }
}

function normalizeSchemaSql(value: string | null): string | null {
  return value === null ? null : value.replace(/\s+/gu, " ").replace(/\s*([(),=])\s*/gu, "$1").trim();
}

function readRestoredInventory(
  db: SQLiteDatabase,
  limits: Fts5IndexLimits,
): {
  documents: number;
  chunks: number;
  sources: number;
} {
  const resolvedLimits = resolveIndexLimits(limits);
  const sourceRows = db.selectObjects(
    `SELECT ${SOURCE_COLUMNS_SQL} FROM sources ORDER BY source_key LIMIT ?`,
    [onePastLimit(resolvedLimits.maxSources)],
  );
  if (sourceRows.length > resolvedLimits.maxSources) throw new IndexCapacityError();

  let documents = 0;
  const sourcesByKey = new Map<string, StoredSource>();
  for (const row of sourceRows) {
    let source: StoredSource | null;
    try {
      source = parseStoredSource([row]);
    } catch {
      throw new CacheImageInvalidError("cache source inventory is invalid");
    }
    if (source === null) throw new CacheImageInvalidError("cache source inventory is invalid");
    sourcesByKey.set(source.source_key, source);
    if (source.outcome === "indexed") documents += 1;
  }

  validateRestoredProperties(db, sourcesByKey);
  const legacyBySource = readCanonicalLegacyProjections(db, sourcesByKey);
  for (const source of sourcesByKey.values()) {
    const legacy = legacyBySource.get(source.source_key) ?? emptyLegacyProjection();
    if (JSON.stringify(source.retrieval) !== JSON.stringify(expectedRetrieval(source.path, legacy))
      || source.aliases_text !== legacy.aliases.join(" ")
      || source.title_text !== legacy.title
      || source.tags_text !== legacy.tags) {
      throw new CacheImageInvalidError("cache source retrieval metadata is invalid");
    }
    if (source.outcome === "indexed") {
      const exact = expectedSourceExact(source.retrieval, legacy.title);
      if (source.exact_filename !== exact.filename
        || source.exact_stem !== exact.stem
        || JSON.stringify(source.exact_aliases) !== JSON.stringify(exact.aliases)
        || source.exact_title !== exact.title) {
        throw new CacheImageInvalidError("cache source exact metadata is invalid");
      }
    }
  }

  // Do not count and trust chunk rows. Read at most one beyond the configured
  // ceiling, then validate every canonical chunk-local field before publication.
  const chunkRows = db.selectObjects(`
    SELECT source_key, chunk_id, vault_id, path, heading_path_json, frontmatter_json,
      heading_text, exact_heading, content, identifiers_json
    FROM chunks
    ORDER BY rowid
    LIMIT ?
  `, [onePastLimit(resolvedLimits.maxChunks)]);
  if (chunkRows.length > resolvedLimits.maxChunks) throw new IndexCapacityError();
  for (const row of chunkRows) validateRestoredChunk(row, sourcesByKey, legacyBySource);

  return {
    documents,
    chunks: chunkRows.length,
    sources: sourceRows.length,
  };
}

function validateRestoredChunk(
  row: Record<string, unknown>,
  sourcesByKey: ReadonlyMap<string, StoredSource>,
  legacyBySource: ReadonlyMap<string, LegacyProjection>,
): void {
  const identifiers = parseIdentifiersJson(row.identifiers_json);
  if (!isBoundedString(row.source_key, 128)
    || !isBoundedString(row.chunk_id, 128)
    || !isBoundedString(row.vault_id, 1_024)
    || row.vault_id.trim().length === 0
    || !isNormalizedVaultRelativePath(row.path)
    || parseHeadingPathJson(row.heading_path_json) === null
    || parseDisplayFrontmatterJson(row.frontmatter_json) === null
    || typeof row.heading_text !== "string"
    || !(row.exact_heading === null || isBoundedNormalizedExact(row.exact_heading))
    || typeof row.content !== "string"
    || identifiers === null) {
    throw new CacheImageInvalidError("cache chunk inventory is invalid");
  }
  const source = sourcesByKey.get(row.source_key);
  const legacy = legacyBySource.get(row.source_key) ?? emptyLegacyProjection();
  if (!source
    || source.outcome !== "indexed"
    || row.vault_id !== source.vault_id
    || row.path !== source.path
    || row.frontmatter_json !== displayFrontmatterJsonFromTitle(legacy.title)
    || row.exact_heading !== normalizeExactForRestore(row.heading_text)
    || JSON.stringify(identifiers) !== JSON.stringify(technicalIdentifiersForRestore(row.content))) {
    throw new CacheImageInvalidError("cache chunk identity does not match its source");
  }
}

function validatePositiveRestoredRowids(db: SQLiteDatabase): void {
  const row = db.selectObjects(`
    SELECT
      EXISTS(SELECT 1 FROM chunks WHERE rowid <= 0) AS chunks,
      EXISTS(SELECT 1 FROM chunks_fts_docsize WHERE id <= 0) AS chunks_fts,
      EXISTS(SELECT 1 FROM source_properties WHERE rowid <= 0) AS properties,
      EXISTS(SELECT 1 FROM source_property_scalars WHERE rowid <= 0) AS scalars,
      EXISTS(SELECT 1 FROM source_property_text_fts WHERE rowid <= 0) AS property_fts
  `)[0];
  if (!row || Object.values(row).some((value) => value !== 0)) {
    throw new CacheImageInvalidError("cache contains nonpositive rowids");
  }
}

function validateChunkFtsBijection(db: SQLiteDatabase): void {
  const row = db.selectObjects(`
    SELECT
      (SELECT count(*) FROM chunks) AS chunks,
      (SELECT count(*) FROM chunks_fts_docsize) AS chunk_fts,
      (SELECT count(*) FROM chunks c
       LEFT JOIN chunks_fts_docsize f ON f.id = c.rowid
       WHERE f.id IS NULL) AS missing,
      (SELECT count(*) FROM chunks_fts_docsize f
       LEFT JOIN chunks c ON c.rowid = f.id
       WHERE c.rowid IS NULL) AS orphaned
  `)[0];
  if (!row
    || !isNonNegativeSafeInteger(row.chunks)
    || !isNonNegativeSafeInteger(row.chunk_fts)
    || !isNonNegativeSafeInteger(row.missing)
    || !isNonNegativeSafeInteger(row.orphaned)
    || row.chunks !== row.chunk_fts
    || row.missing !== 0
    || row.orphaned !== 0) {
    throw new CacheImageInvalidError("cache chunk postings do not match canonical rows");
  }
}

function validatePropertyFtsBijection(db: SQLiteDatabase): void {
  const row = db.selectObjects(`
    SELECT
      (SELECT count(*) FROM source_properties) AS properties,
      (SELECT count(*) FROM source_property_text_fts) AS property_fts,
      (SELECT count(*) FROM source_properties p
       LEFT JOIN source_property_text_fts f ON f.rowid = p.rowid
       WHERE f.rowid IS NULL) AS missing,
      (SELECT count(*) FROM source_property_text_fts f
       LEFT JOIN source_properties p ON p.rowid = f.rowid
       WHERE p.rowid IS NULL) AS orphaned
  `)[0];
  if (!row
    || !isNonNegativeSafeInteger(row.properties)
    || !isNonNegativeSafeInteger(row.property_fts)
    || !isNonNegativeSafeInteger(row.missing)
    || !isNonNegativeSafeInteger(row.orphaned)
    || row.properties !== row.property_fts
    || row.missing !== 0
    || row.orphaned !== 0) {
    throw new CacheImageInvalidError("cache property postings do not match canonical rows");
  }
}

function rebuildCanonicalPropertyFts(db: SQLiteDatabase): void {
  try {
    db.transaction("IMMEDIATE", () => {
      db.exec("DELETE FROM source_property_text_fts");
      const pageSize = 100;
      let afterRowid = 0;
      while (true) {
        const rows = db.selectObjects(`
          SELECT rowid, value_json
          FROM source_properties
          WHERE rowid > ?
          ORDER BY rowid
          LIMIT ?
        `, [afterRowid, pageSize]);
        if (rows.length === 0) break;
        for (const row of rows) {
          if (!isPositiveSafeInteger(row.rowid) || typeof row.value_json !== "string") {
            throw new CacheImageInvalidError("cache property inventory is invalid");
          }
          const value = parsePropertyValueJson(row.value_json);
          if (value === undefined) {
            throw new CacheImageInvalidError("cache property inventory is invalid");
          }
          const aggregate = emptyPropertyTextAggregate();
          for (const leaf of iteratePropertyScalars(value)) addPropertyText(aggregate, leaf.scalar);
          db.exec(INSERT_PROPERTY_TEXT_FTS_SQL, {
            bind: [row.rowid, ...finishPropertyTextAggregate(aggregate)],
          });
          afterRowid = row.rowid;
        }
      }
    });
  } catch (error) {
    if (error instanceof CacheImageInvalidError) throw error;
    throw translateSqliteCapacityError(error);
  }
}

function parseIdentifiersJson(value: unknown): string[] | null {
  if (typeof value !== "string") return null;
  let parsed: unknown;
  try {
    parsed = JSON.parse(value);
  } catch {
    return null;
  }
  return Array.isArray(parsed)
    && parsed.every(isBoundedNormalizedExact)
    && JSON.stringify(parsed) === value
    ? parsed as string[]
    : null;
}

function technicalIdentifiersForRestore(source: string): string[] {
  const wordCharacters = "\\p{Alphabetic}\\p{M}\\p{Nd}\\p{Pc}\\u200C\\u200D";
  const pattern = new RegExp(
    `(^|[^${wordCharacters}])`
      + "((?:cve-\\d{4}-\\d+|rfc(?:[- ]?\\d+)|[a-z0-9][a-z0-9._/:+\\-]{1,}[a-z0-9_]))"
      + `(?=$|[^${wordCharacters}])`,
    "giu",
  );
  const seen = new Set<string>();
  const identifiers: string[] = [];
  for (const match of source.matchAll(pattern)) {
    const candidate = match[2];
    if (candidate === undefined) continue;
    const hasAlpha = /\p{Alphabetic}/u.test(candidate);
    const hasDigit = /[0-9]/u.test(candidate);
    const hasSeparator = /[-_./:+]/u.test(candidate);
    if (!hasDigit || (!hasAlpha && !hasSeparator)) continue;
    const identifier = normalizeExactForRestore(candidate);
    if (identifier !== null && !seen.has(identifier)) {
      seen.add(identifier);
      identifiers.push(identifier);
    }
  }
  return identifiers;
}

function validateRestoredProperties(
  db: SQLiteDatabase,
  sourcesByKey: ReadonlyMap<string, StoredSource>,
): void {
  const pageSize = 100;
  let afterRowid = 0;
  while (true) {
    const properties = db.selectObjects(`
      SELECT rowid, source_key, property_name, value_json, root_type,
        exact_value, numeric_value, date_value
      FROM source_properties
      WHERE rowid > ?
      ORDER BY rowid
      LIMIT ?
    `, [afterRowid, pageSize]);
    if (properties.length === 0) return;

    const expectedScalars = new Map<string, ScalarProjection>();
    const clauses: string[] = [];
    const bind: unknown[] = [];
    for (const row of properties) {
      if (!isPositiveSafeInteger(row.rowid)
        || !isBoundedString(row.source_key, 128)
        || typeof row.property_name !== "string"
        || typeof row.value_json !== "string") {
        throw new CacheImageInvalidError("cache property inventory is invalid");
      }
      const source = sourcesByKey.get(row.source_key);
      const value = parsePropertyValueJson(row.value_json);
      if (!source || source.outcome !== "indexed" || value === undefined
        || canonicalJson(value) !== row.value_json) {
        throw new CacheImageInvalidError("cache property inventory is invalid");
      }
      const root = projectRoot(value);
      if (row.root_type !== root.type
        || row.exact_value !== root.exactValue
        || row.numeric_value !== root.numericValue
        || row.date_value !== root.dateValue) {
        throw new CacheImageInvalidError("cache property projection is invalid");
      }
      for (const leaf of iteratePropertyScalars(value)) {
        expectedScalars.set(
          propertyScalarIdentity(row.source_key, row.property_name, leaf.jsonPointer),
          leaf.scalar,
        );
      }
      clauses.push("(source_key = ? AND property_name = ?)");
      bind.push(row.source_key, row.property_name);
      afterRowid = row.rowid;
    }

    const scalars = db.selectObjects(`
      SELECT rowid, source_key, property_name, json_pointer, scalar_type,
        exact_value, numeric_value, date_value
      FROM source_property_scalars
      WHERE ${clauses.join(" OR ")}
      ORDER BY source_key, property_name, json_pointer
    `, bind);
    if (scalars.length !== expectedScalars.size) {
      throw new CacheImageInvalidError("cache property scalar inventory is invalid");
    }
    for (const row of scalars) {
      if (!isPositiveSafeInteger(row.rowid)
        || typeof row.source_key !== "string"
        || typeof row.property_name !== "string"
        || typeof row.json_pointer !== "string") {
        throw new CacheImageInvalidError("cache property scalar inventory is invalid");
      }
      const key = propertyScalarIdentity(
        row.source_key,
        row.property_name,
        row.json_pointer,
      );
      const expected = expectedScalars.get(key);
      if (!expected
        || row.scalar_type !== expected.type
        || row.exact_value !== expected.exactValue
        || row.numeric_value !== expected.numericValue
        || row.date_value !== expected.dateValue) {
        throw new CacheImageInvalidError("cache property scalar projection is invalid");
      }
      expectedScalars.delete(key);
    }
    if (expectedScalars.size !== 0) {
      throw new CacheImageInvalidError("cache property scalar inventory is invalid");
    }
  }
}

interface LegacyProjection {
  title: string;
  tags: string;
  aliases: string[];
}

function emptyLegacyProjection(): LegacyProjection {
  return { title: "", tags: "", aliases: [] };
}

function readCanonicalLegacyProjections(
  db: SQLiteDatabase,
  sourcesByKey: ReadonlyMap<string, StoredSource>,
): Map<string, LegacyProjection> {
  const valuesBySource = new Map<string, PreparedFrontmatter>();
  let afterRowid = 0;
  while (true) {
    const rows = db.selectObjects(`
      SELECT rowid, source_key, property_name, value_json
      FROM source_properties
      WHERE rowid > ? AND property_name IN ('title', 'tags', 'aliases')
      ORDER BY rowid
      LIMIT 100
    `, [afterRowid]);
    if (rows.length === 0) break;
    for (const row of rows) {
      if (!isPositiveSafeInteger(row.rowid)
        || typeof row.source_key !== "string"
        || typeof row.property_name !== "string"
        || typeof row.value_json !== "string"
        || !sourcesByKey.has(row.source_key)) {
        throw new CacheImageInvalidError("cache legacy property projection is invalid");
      }
      const value = parsePropertyValueJson(row.value_json);
      if (value === undefined) {
        throw new CacheImageInvalidError("cache legacy property projection is invalid");
      }
      const values = valuesBySource.get(row.source_key) ?? {};
      values[row.property_name] = value;
      valuesBySource.set(row.source_key, values);
      afterRowid = row.rowid;
    }
  }

  const projections = new Map<string, LegacyProjection>();
  for (const source of sourcesByKey.values()) {
    const values = valuesBySource.get(source.source_key) ?? {};
    projections.set(source.source_key, {
      title: legacyTitle(values),
      tags: legacyTags(values),
      aliases: legacyAliases(values),
    });
  }
  return projections;
}

function expectedRetrieval(path: string, legacy: LegacyProjection): StoredSource["retrieval"] {
  const filename = path.split("/").at(-1) ?? path;
  const separator = filename.lastIndexOf(".");
  const stem = separator > 0 ? filename.slice(0, separator) : filename;
  return { filename, stem, aliases: legacy.aliases };
}

function expectedSourceExact(
  retrieval: StoredSource["retrieval"],
  title: string,
): { filename: string | null; stem: string | null; aliases: string[]; title: string | null } {
  return {
    filename: normalizeExactForRestore(retrieval.filename),
    stem: normalizeExactForRestore(retrieval.stem),
    aliases: retrieval.aliases
      .map(normalizeExactForRestore)
      .filter((alias): alias is string => alias !== null),
    title: normalizeExactForRestore(title),
  };
}

function normalizeExactForRestore(value: string): string | null {
  const words: string[] = [];
  let word = "";
  for (const character of value) {
    if (isRustWhitespace(character.codePointAt(0)!)) {
      if (word.length > 0) {
        words.push(word);
        word = "";
      }
    } else {
      word += character;
    }
  }
  if (word.length > 0) words.push(word);
  if (words.length === 0) return null;
  return [...words.join(" ").toLowerCase()].slice(0, 256).join("");
}

function isRustWhitespace(codePoint: number): boolean {
  return (codePoint >= 0x0009 && codePoint <= 0x000d)
    || codePoint === 0x0020
    || codePoint === 0x0085
    || codePoint === 0x00a0
    || codePoint === 0x1680
    || (codePoint >= 0x2000 && codePoint <= 0x200a)
    || codePoint === 0x2028
    || codePoint === 0x2029
    || codePoint === 0x202f
    || codePoint === 0x205f
    || codePoint === 0x3000;
}

function propertyScalarIdentity(
  sourceKey: string,
  propertyName: string,
  jsonPointer: string,
): string {
  return JSON.stringify([sourceKey, propertyName, jsonPointer]);
}

function isPositiveSafeInteger(value: unknown): value is number {
  return typeof value === "number" && Number.isSafeInteger(value) && value > 0;
}

function onePastLimit(limit: number): number {
  return limit === Number.MAX_SAFE_INTEGER ? limit : limit + 1;
}

interface ProjectedChunk {
  /** Chunk identity, display metadata, and canonical chunk-local lexical inputs. */
  metadataBind: readonly unknown[];
  /** The nine ordinary FTS field values, in declared column order. */
  ftsBind: readonly string[];
  chunkId: string;
  chunkingVersion: number;
}

interface ProjectedPreparation {
  preparation: SourcePreparation;
  frontmatter: PreparedFrontmatter;
  rows: ProjectedChunk[];
}

interface StoredSource {
  source_key: string;
  vault_id: string;
  path: string;
  outcome: "indexed" | "skipped";
  content_hash: string | null;
  byte_length: number;
  mtime_nanos: string;
  retrieval: {
    filename: string;
    stem: string;
    aliases: string[];
  };
  exact_filename: string | null;
  exact_stem: string | null;
  exact_aliases: string[];
  exact_title: string | null;
  aliases_text: string;
  title_text: string;
  tags_text: string;
  chunk_count: number;
  property_count: number;
  property_scalar_count: number;
}

function projectPreparation(preparation: SourcePreparation): ProjectedPreparation {
  const frontmatter = requireSourceFrontmatter(preparation);
  if (preparation.kind !== "indexed") {
    return { preparation, frontmatter, rows: [] };
  }

  const rows = preparation.chunks.map((chunk) => projectChunk(preparation, chunk, frontmatter));
  return { preparation, frontmatter, rows };
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

function projectChunk(
  preparation: SourcePreparation,
  prepared: PreparedChunk,
  sourceFrontmatter: PreparedFrontmatter,
): ProjectedChunk {
  const chunk = prepared.chunk;
  if (chunk.vault_id !== preparation.vault_id
    || chunk.room !== (preparation.room ?? null)
    || chunk.path !== preparation.path
    || chunk.mtime !== preparation.mtime
    || chunk.content_hash !== preparation.content_hash) {
    throw new Error("prepared chunk does not match its source");
  }
  const aliases = preparation.retrieval.aliases.join(" ");
  const title = legacyTitle(sourceFrontmatter);
  const tags = legacyTags(sourceFrontmatter);
  const fields = [
    preparation.retrieval.filename,
    preparation.retrieval.stem,
    aliases,
    title,
    prepared.heading_text,
    chunk.path,
    tags,
    chunk.content,
    JSON.stringify(prepared.technical_identifiers),
  ];
  return {
    metadataBind: [
      chunk.chunk_id,
      chunk.vault_id,
      chunk.path,
      JSON.stringify(chunk.heading_path),
      displayFrontmatterJson(sourceFrontmatter),
      prepared.heading_text,
      prepared.normalized_heading,
      chunk.content,
      JSON.stringify(prepared.technical_identifiers),
    ],
    ftsBind: fields,
    chunkId: chunk.chunk_id,
    chunkingVersion: chunk.chunking_version,
  };
}

type ScalarType = "null" | "boolean" | "i64" | "u64" | "real" | "string" | "date";
type RootType = ScalarType | "array" | "object";

interface ScalarProjection {
  type: ScalarType;
  exactValue: string;
  numericValue: number | null;
  dateValue: string | null;
  ftsValues: readonly [string, string, string, string, string];
}

function requireSourceFrontmatter(preparation: SourcePreparation): PreparedFrontmatter {
  const frontmatter = preparation.frontmatter;
  if (!isPreparedPropertyBag(frontmatter)) {
    throw new Error("prepared source properties are invalid");
  }
  return frontmatter as PreparedFrontmatter;
}

function projectRoot(value: PreparedPropertyValue): {
  type: RootType;
  exactValue: string | null;
  numericValue: number | null;
  dateValue: string | null;
} {
  if (value.type === "sequence") {
    return { type: "array", exactValue: null, numericValue: null, dateValue: null };
  }
  if (value.type === "map") {
    return { type: "object", exactValue: null, numericValue: null, dateValue: null };
  }
  const scalar = projectScalar(value);
  return {
    type: scalar.type,
    exactValue: scalar.exactValue,
    numericValue: scalar.numericValue,
    dateValue: scalar.dateValue,
  };
}

function* iteratePropertyScalars(value: PreparedPropertyValue): Generator<{
  jsonPointer: string;
  scalar: ScalarProjection;
}> {
  const pending: Array<{ value: PreparedPropertyValue; jsonPointer: string }> = [
    { value, jsonPointer: "" },
  ];
  while (pending.length > 0) {
    const item = pending.pop();
    if (!item) break;
    if (item.value.type === "sequence") {
      for (let index = item.value.value.length - 1; index >= 0; index -= 1) {
        pending.push({
          value: item.value.value[index]!,
          jsonPointer: `${item.jsonPointer}/${index}`,
        });
      }
      continue;
    }
    if (item.value.type === "map") {
      const names = Object.keys(item.value.value).sort(comparePaths);
      for (let index = names.length - 1; index >= 0; index -= 1) {
        const name = names[index]!;
        pending.push({
          value: item.value.value[name]!,
          jsonPointer: `${item.jsonPointer}/${escapeJsonPointer(name)}`,
        });
      }
      continue;
    }
    yield { jsonPointer: item.jsonPointer, scalar: projectScalar(item.value) };
  }
}

type PropertyTextAggregate = [Set<string>, Set<string>, Set<string>, Set<string>, Set<string>];

function emptyPropertyTextAggregate(): PropertyTextAggregate {
  return [new Set(), new Set(), new Set(), new Set(), new Set()];
}

function addPropertyText(values: PropertyTextAggregate, scalar: ScalarProjection): void {
  for (let index = 0; index < scalar.ftsValues.length; index += 1) {
    const value = scalar.ftsValues[index]!;
    if (value.length > 0) values[index]!.add(value);
  }
}

function finishPropertyTextAggregate(
  values: PropertyTextAggregate,
): readonly [string, string, string, string, string] {
  return [
    [...values[0]].join("\n"),
    [...values[1]].join("\n"),
    [...values[2]].join("\n"),
    [...values[3]].join("\n"),
    [...values[4]].join("\n"),
  ];
}

function projectScalar(value: Exclude<PreparedPropertyValue, {
  type: "sequence" | "map";
}>): ScalarProjection {
  if (value.type === "null") {
    return {
      type: "null",
      exactValue: "null",
      numericValue: null,
      dateValue: null,
      ftsValues: ["", "", "", "", ""],
    };
  }
  if (value.type === "boolean") {
    const exactValue = value.value ? "true" : "false";
    return {
      type: "boolean",
      exactValue,
      numericValue: null,
      dateValue: null,
      ftsValues: ["", "", "", exactValue, ""],
    };
  }
  if (value.type === "i64" || value.type === "u64") {
    const numeric = BigInt(value.value);
    const numericValue = numeric >= BigInt(Number.MIN_SAFE_INTEGER)
      && numeric <= BigInt(Number.MAX_SAFE_INTEGER)
      ? Number(numeric)
      : null;
    return {
      type: value.type,
      exactValue: value.value,
      numericValue,
      dateValue: null,
      ftsValues: ["", value.value, "", "", ""],
    };
  }
  if (value.type === "f64") {
    const numericValue = f64FromHex(value.value);
    const textValue = Number.isFinite(numericValue) ? String(numericValue) : "";
    return {
      type: "real",
      exactValue: value.value,
      numericValue: Number.isFinite(numericValue) ? numericValue : null,
      dateValue: null,
      ftsValues: ["", "", textValue, "", ""],
    };
  }
  if (isIsoCalendarDate(value.value)) {
    return {
      type: "date",
      exactValue: value.value,
      numericValue: null,
      dateValue: value.value,
      ftsValues: ["", "", "", "", value.value],
    };
  }
  return {
    type: "string",
    exactValue: value.value,
    numericValue: null,
    dateValue: null,
    ftsValues: [value.value, "", "", "", ""],
  };
}

function f64FromHex(value: string): number {
  const bytes = new ArrayBuffer(8);
  const view = new DataView(bytes);
  view.setBigUint64(0, BigInt(`0x${value}`), false);
  return view.getFloat64(0, false);
}

function isIsoCalendarDate(value: string): boolean {
  const match = /^(\d{4})-(\d{2})-(\d{2})$/u.exec(value);
  if (!match) return false;
  const year = Number(match[1]);
  const month = Number(match[2]);
  const day = Number(match[3]);
  if (month < 1 || month > 12 || day < 1) return false;
  const leap = year % 4 === 0 && (year % 100 !== 0 || year % 400 === 0);
  const days = [31, leap ? 29 : 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
  return day <= days[month - 1]!;
}

function escapeJsonPointer(value: string): string {
  return value.replace(/~/gu, "~0").replace(/\//gu, "~1");
}

function retrievalJson(preparation: SourcePreparation): string {
  return JSON.stringify({
    filename: preparation.retrieval.filename,
    stem: preparation.retrieval.stem,
    aliases: preparation.retrieval.aliases,
  });
}

function parseRetrievalJson(value: unknown): StoredSource["retrieval"] | null {
  if (typeof value !== "string") return null;
  let parsed: unknown;
  try {
    parsed = JSON.parse(value);
  } catch {
    return null;
  }
  if (typeof parsed !== "object" || parsed === null || Array.isArray(parsed)) return null;
  const record = parsed as Record<string, unknown>;
  if (Object.keys(record).sort(comparePaths).join(",") !== "aliases,filename,stem"
    || typeof record.filename !== "string"
    || typeof record.stem !== "string"
    || !Array.isArray(record.aliases)
    || !record.aliases.every((alias) => typeof alias === "string")) {
    return null;
  }
  const retrieval = {
    filename: record.filename,
    stem: record.stem,
    aliases: record.aliases as string[],
  };
  return JSON.stringify(retrieval) === value ? retrieval : null;
}

function parseExactAliasesJson(value: unknown): string[] | null {
  if (typeof value !== "string") return null;
  let parsed: unknown;
  try {
    parsed = JSON.parse(value);
  } catch {
    return null;
  }
  if (!Array.isArray(parsed)
    || !parsed.every(isBoundedNormalizedExact)) {
    return null;
  }
  return JSON.stringify(parsed) === value ? parsed : null;
}

function canonicalJson(value: PreparedPropertyValue): string {
  if (value.type === "null") return '{"type":"null"}';
  if (value.type === "sequence") {
    return `{"type":"sequence","value":[${value.value.map(canonicalJson).join(",")}]}`;
  }
  if (value.type === "map") {
    const entries = Object.keys(value.value).sort(comparePaths).map((name) => (
      `${JSON.stringify(name)}:${canonicalJson(value.value[name]!)}`
    ));
    return `{"type":"map","value":{${entries.join(",")}}}`;
  }
  return `{"type":${JSON.stringify(value.type)},"value":${JSON.stringify(value.value)}}`;
}

function legacyTitle(frontmatter: PreparedFrontmatter): string {
  return legacyScalarString(frontmatter.title) ?? "";
}

function legacyTags(frontmatter: PreparedFrontmatter): string {
  return legacyPropertyStrings(frontmatter.tags).join(" ");
}

function legacyAliases(frontmatter: PreparedFrontmatter): string[] {
  const aliases: string[] = [];
  for (const authored of legacyPropertyStrings(frontmatter.aliases)) {
    const alias = authored.trim();
    if (alias.length > 0 && !aliases.includes(alias)) aliases.push(alias);
  }
  return aliases;
}

function legacyPropertyStrings(value: PreparedPropertyValue | undefined): string[] {
  if (value?.type === "sequence") {
    return value.value.flatMap((item) => {
      const scalar = legacyScalarString(item);
      return scalar === null ? [] : [scalar];
    });
  }
  const scalar = legacyScalarString(value);
  return scalar === null ? [] : [scalar];
}

function legacyScalarString(value: PreparedPropertyValue | undefined): string | null {
  if (!value || value.type === "null" || value.type === "sequence" || value.type === "map") {
    return null;
  }
  if (value.type === "boolean") return value.value ? "true" : "false";
  if (value.type === "f64") return String(f64FromHex(value.value));
  return value.value;
}

function displayFrontmatterJson(frontmatter: PreparedFrontmatter): string {
  return displayFrontmatterJsonFromTitle(legacyTitle(frontmatter));
}

function displayFrontmatterJsonFromTitle(title: string): string {
  return title.length <= 1_024 ? JSON.stringify(title.length > 0 ? { title } : {}) : "{}";
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
  const retrieval = parseRetrievalJson(row?.retrieval_json);
  if (!row
    || !isBoundedString(row.source_key, 128)
    || typeof row.vault_id !== "string"
    || row.vault_id.trim().length < 1
    || row.vault_id.length > 1_024
    || !isNormalizedVaultRelativePath(row.path)
    || (row.outcome !== "indexed" && row.outcome !== "skipped")
    || !(row.content_hash === null
      || (typeof row.content_hash === "string"
        && row.content_hash.length > 0
        && row.content_hash.length <= 128))
    || !isNonNegativeSafeInteger(row.byte_length)
    || typeof row.mtime_nanos !== "string"
    || !/^[0-9]{1,39}$/u.test(row.mtime_nanos)
    || retrieval === null
    || !(row.exact_filename === null || isBoundedNormalizedExact(row.exact_filename))
    || !(row.exact_stem === null || isBoundedNormalizedExact(row.exact_stem))
    || parseExactAliasesJson(row.exact_aliases_json) === null
    || !(row.exact_title === null || isBoundedNormalizedExact(row.exact_title))
    || typeof row.aliases_text !== "string"
    || typeof row.title_text !== "string"
    || typeof row.tags_text !== "string"
    || !isNonNegativeSafeInteger(row.chunk_count)
    || !isNonNegativeSafeInteger(row.property_count)
    || !isNonNegativeSafeInteger(row.property_scalar_count)
    || (row.outcome === "indexed" && row.content_hash === null)
    || (row.outcome === "skipped" && (
      row.chunk_count !== 0
      || row.property_count !== 0
      || row.property_scalar_count !== 0
    ))) {
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
    retrieval,
    exact_filename: row.exact_filename,
    exact_stem: row.exact_stem,
    exact_aliases: parseExactAliasesJson(row.exact_aliases_json)!,
    exact_title: row.exact_title,
    aliases_text: row.aliases_text,
    title_text: row.title_text,
    tags_text: row.tags_text,
    chunk_count: row.chunk_count,
    property_count: row.property_count,
    property_scalar_count: row.property_scalar_count,
  };
}

function requireProjectedCounts(
  documents: number,
  chunks: number,
  sources: number,
  limits: ResolvedFts5IndexLimits,
): void {
  if (!isNonNegativeSafeInteger(documents)
    || !isNonNegativeSafeInteger(chunks)
    || !isNonNegativeSafeInteger(sources)) {
    throw new Error("source accounting is invalid");
  }
  if (chunks > limits.maxChunks || sources > limits.maxSources) {
    throw new IndexCapacityError();
  }
}

function resolveIndexLimits(limits: Fts5IndexLimits): ResolvedFts5IndexLimits {
  const resolved: ResolvedFts5IndexLimits = {
    maxChunks: limits.maxChunks,
    maxDatabaseBytes: limits.maxDatabaseBytes,
    maxSources: limits.maxSources ?? MAX_INDEX_SOURCES,
    maxExportBytes: limits.maxExportBytes ?? MAX_EXPORT_BLOB_BYTES,
  };
  for (const limit of Object.values(resolved)) {
    if (!Number.isSafeInteger(limit) || limit < 1) {
      throw new Error("FTS5 index limits must be positive safe integers");
    }
  }
  if (resolved.maxDatabaseBytes > resolved.maxExportBytes) {
    throw new Error("database byte limit must not exceed export byte limit");
  }
  return resolved;
}

function configureDatabasePageLimit(
  db: SQLiteDatabase,
  limits: ResolvedFts5IndexLimits,
): number {
  const pageSize = Number(db.selectValue("PRAGMA page_size"));
  if (!Number.isSafeInteger(pageSize) || pageSize < 512) {
    throw new Error("SQLite page size is invalid");
  }
  const maxPageCount = Math.floor(limits.maxDatabaseBytes / pageSize);
  if (maxPageCount < 1) throw new IndexCapacityError();
  const applied = Number(db.selectValue(`PRAGMA max_page_count = ${maxPageCount}`));
  if (!Number.isSafeInteger(applied) || applied < 1) {
    throw new Error("SQLite page limit was not applied");
  }
  const effectiveBytes = maxPageCount * pageSize;
  if (!Number.isSafeInteger(effectiveBytes)) {
    throw new Error("SQLite page limit is invalid");
  }
  return effectiveBytes;
}

function measuredDatabaseBytes(db: SQLiteDatabase): number {
  const pageCount = Number(db.selectValue("PRAGMA page_count"));
  const pageSize = Number(db.selectValue("PRAGMA page_size"));
  const bytes = pageCount * pageSize;
  if (!isNonNegativeSafeInteger(pageCount)
    || !isNonNegativeSafeInteger(pageSize)
    || !isNonNegativeSafeInteger(bytes)) {
    throw new IndexIntegrityError("SQLite database size is invalid");
  }
  return bytes;
}

function requireDatabaseWithinLimit(db: SQLiteDatabase, limit: number): void {
  if (measuredDatabaseBytes(db) > limit) throw new IndexCapacityError();
}

function translateSqliteCapacityError(error: unknown): unknown {
  if (error instanceof IndexCapacityError) return error;
  if (typeof error === "object" && error !== null
    && (error as { resultCode?: unknown }).resultCode === SQLITE_FULL) {
    return new IndexCapacityError();
  }
  return error;
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

function comparePaths(left: string, right: string): number {
  return left < right ? -1 : left > right ? 1 : 0;
}

function sourceIdentity(vaultId: string, path: string): string {
  return JSON.stringify([vaultId, path]);
}

function isNonNegativeSafeInteger(value: unknown): value is number {
  return typeof value === "number" && Number.isSafeInteger(value) && value >= 0;
}

function isNormalizedVaultRelativePath(value: unknown): value is string {
  return typeof value === "string"
    && value.length >= 1
    && value.length <= 4_096
    && !value.startsWith("/")
    && !value.endsWith("/")
    && !value.includes("\\")
    && !value.includes("\0")
    && value.split("/").every((component) => (
      component.length > 0 && component !== "." && component !== ".."
    ));
}

function isBoundedString(value: unknown, maximum: number, allowEmpty = false): value is string {
  return typeof value === "string"
    && value.length <= maximum
    && (allowEmpty || value.length > 0);
}

function isBoundedNormalizedExact(value: unknown): value is string {
  return typeof value === "string"
    && value.length > 0
    && [...value].length <= 256;
}

function parseHeadingPathJson(value: unknown): string[] | null {
  if (typeof value !== "string") return null;
  let parsed: unknown;
  try {
    parsed = JSON.parse(value);
  } catch {
    return null;
  }
  return Array.isArray(parsed)
    && parsed.length <= 64
    && parsed.every((heading) => isBoundedString(heading, 1_024))
    ? parsed
    : null;
}

function parseDisplayFrontmatterJson(value: unknown): WorkerFrontmatter | null {
  if (typeof value !== "string") return null;
  let parsed: unknown;
  try {
    parsed = JSON.parse(value);
  } catch {
    return null;
  }
  if (typeof parsed !== "object" || parsed === null || Array.isArray(parsed)) return null;
  const record = parsed as Record<string, unknown>;
  if (Object.keys(record).some((key) => key !== "title")) return null;
  if (record.title !== undefined
    && (typeof record.title !== "string" || record.title.length > 1_024)) {
    return null;
  }
  return record as WorkerFrontmatter;
}

function parsePropertyValueJson(value: string): PreparedPropertyValue | undefined {
  let parsed: unknown;
  try {
    parsed = JSON.parse(value);
  } catch {
    return undefined;
  }
  return isPreparedPropertyBag({ value: parsed })
    ? parsed as PreparedPropertyValue
    : undefined;
}

function parseSearchRow(row: Record<string, unknown>): WorkerSearchHit {
  const headingPath = parseHeadingPathJson(row.heading_path_json);
  const frontmatter = parseDisplayFrontmatterJson(row.frontmatter_json);
  if (!isBoundedString(row.chunk_id, 128)
    || !isBoundedString(row.vault_id, 1_024)
    || row.vault_id.trim().length === 0
    || !isNormalizedVaultRelativePath(row.path)
    || frontmatter === null
    || headingPath === null
    || typeof row.score !== "number"
    || !Number.isFinite(row.score)) {
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
  };
}
