// Memory-optimization experiments over the feasibility corpus: how small
// can the cache blob (≈ the live in-memory database image, the dominant
// steady-state cost) get, and what capability does each variant trade away?
//
// Variants:
//   A baseline            — current external-content schema (stores content)
//   B detail-column       — positions dropped per column (no phrase queries)
//   C detail-none         — positions dropped entirely (no phrase, no column filter)
//   D contentless         — content NOT stored in SQLite at all; excerpts
//                           would hydrate from the vault files (which are
//                           the source of truth anyway); contentless_delete=1
//   E contentless+deflate — D plus a deflate-compressed excerpt column so
//                           excerpts need no file read at render time
//   F contentless+detail-none — D and C combined: the floor
//
// Aggregate-only output; corpus is synthetic.

import { deflateSync } from 'node:zlib';
import sqlite3InitModule from '../../fts5-wasm/node_modules/@sqlite.org/sqlite-wasm/dist/node.mjs';
import { streamNotes } from '../src/corpus.mjs';

const FTS_COLUMNS =
  'filename, stem, aliases, title, heading_text, path_text, tags, content, identifiers';

function metadataTable(withContent, withCompressed) {
  return `CREATE TABLE chunks (
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
    tags TEXT NOT NULL
    ${withContent ? ', content TEXT NOT NULL' : ''}
    ${withCompressed ? ', content_z BLOB NOT NULL' : ''}
    , identifiers TEXT NOT NULL
  );
  CREATE INDEX chunks_by_source ON chunks(source_key);`;
}

const VARIANTS = {
  'A-baseline': {
    storesContent: true,
    compressed: false,
    fts: `CREATE VIRTUAL TABLE chunks_fts USING fts5(${FTS_COLUMNS},
      content='chunks', content_rowid='rowid', tokenize='unicode61 remove_diacritics 2');`,
    externalContent: true,
  },
  'B-detail-column': {
    storesContent: true,
    compressed: false,
    fts: `CREATE VIRTUAL TABLE chunks_fts USING fts5(${FTS_COLUMNS},
      content='chunks', content_rowid='rowid', detail=column,
      tokenize='unicode61 remove_diacritics 2');`,
    externalContent: true,
  },
  'C-detail-none': {
    storesContent: true,
    compressed: false,
    fts: `CREATE VIRTUAL TABLE chunks_fts USING fts5(${FTS_COLUMNS},
      content='chunks', content_rowid='rowid', detail=none,
      tokenize='unicode61 remove_diacritics 2');`,
    externalContent: true,
  },
  'D-contentless': {
    storesContent: false,
    compressed: false,
    fts: `CREATE VIRTUAL TABLE chunks_fts USING fts5(${FTS_COLUMNS},
      content='', contentless_delete=1, tokenize='unicode61 remove_diacritics 2');`,
    externalContent: false,
  },
  'E-contentless-deflate': {
    storesContent: false,
    compressed: true,
    fts: `CREATE VIRTUAL TABLE chunks_fts USING fts5(${FTS_COLUMNS},
      content='', contentless_delete=1, tokenize='unicode61 remove_diacritics 2');`,
    externalContent: false,
  },
  'F-contentless-detail-none': {
    storesContent: false,
    compressed: false,
    fts: `CREATE VIRTUAL TABLE chunks_fts USING fts5(${FTS_COLUMNS},
      content='', contentless_delete=1, detail=none,
      tokenize='unicode61 remove_diacritics 2');`,
    externalContent: false,
  },
};

function triggersSql() {
  // Same canonical external-content triggers as the production schema,
  // content column included (only used by external-content variants).
  const cols = `${FTS_COLUMNS}`;
  const newVals = cols.split(', ').map((c) => `new.${c}`).join(', ');
  const oldVals = cols.split(', ').map((c) => `old.${c}`).join(', ');
  return `
  CREATE TRIGGER chunks_ai AFTER INSERT ON chunks BEGIN
    INSERT INTO chunks_fts(rowid, ${cols}) VALUES (new.rowid, ${newVals});
  END;
  CREATE TRIGGER chunks_ad AFTER DELETE ON chunks BEGIN
    INSERT INTO chunks_fts(chunks_fts, rowid, ${cols})
    VALUES ('delete', old.rowid, ${oldVals});
  END;`;
}

function selectAll(db, sql, bind = []) {
  const rows = [];
  db.exec({ sql, bind, rowMode: 'object', resultRows: rows });
  return rows;
}

function tryQuery(db, sql, bind) {
  try {
    return { ok: true, rows: selectAll(db, sql, bind) };
  } catch (error) {
    return { ok: false, error: String(error).slice(0, 60) };
  }
}

const sqlite3 = await sqlite3InitModule({ print: () => {}, printErr: () => {} });
const results = [];

for (const [name, variant] of Object.entries(VARIANTS)) {
  const db = new sqlite3.oo1.DB(':memory:');
  const started = performance.now();
  db.exec(metadataTable(variant.storesContent, variant.compressed));
  db.exec(variant.fts);
  if (variant.externalContent) db.exec(triggersSql());

  const metaColumns = variant.storesContent
    ? 'content,'
    : variant.compressed
      ? 'content_z,'
      : '';
  const insertMeta = db.prepare(
    `INSERT INTO chunks (
       source_key, chunk_id, vault_id, path, heading_path_json,
       frontmatter_json, mtime, content_hash, chunking_version,
       filename, stem, aliases, title, heading_text, path_text, tags,
       ${metaColumns} identifiers
     ) VALUES (${'?, '.repeat(variant.storesContent || variant.compressed ? 17 : 16)}?)`,
  );
  const insertFts = variant.externalContent
    ? null
    : db.prepare(
        `INSERT INTO chunks_fts (rowid, ${FTS_COLUMNS})
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)`,
      );

  db.exec('BEGIN IMMEDIATE');
  let rowid = 0;
  let compressedBytes = 0;
  for (const note of streamNotes()) {
    for (const chunk of note.chunks) {
      rowid += 1;
      const base = [
        chunk.source_key, chunk.chunk_id, chunk.vault_id, chunk.path,
        chunk.heading_path_json, chunk.frontmatter_json, chunk.mtime,
        chunk.content_hash, chunk.chunking_version, chunk.filename,
        chunk.stem, chunk.aliases, chunk.title, chunk.heading_text,
        chunk.path_text, chunk.tags,
      ];
      if (variant.storesContent) base.push(chunk.content);
      else if (variant.compressed) {
        const deflated = deflateSync(Buffer.from(chunk.content, 'utf8'), { level: 6 });
        compressedBytes += deflated.byteLength;
        base.push(new Uint8Array(deflated));
      }
      base.push(chunk.identifiers);
      insertMeta.bind(base).stepReset();
      if (insertFts) {
        insertFts
          .bind([
            rowid, chunk.filename, chunk.stem, chunk.aliases, chunk.title,
            chunk.heading_text, chunk.path_text, chunk.tags, chunk.content,
            chunk.identifiers,
          ])
          .stepReset();
      }
    }
  }
  insertMeta.finalize();
  if (insertFts) insertFts.finalize();
  db.exec('COMMIT');
  const buildMs = performance.now() - started;

  const capability = {
    term: tryQuery(db, "SELECT COUNT(*) AS n FROM chunks_fts WHERE chunks_fts MATCH 'section'"),
    phrase: tryQuery(
      db,
      `SELECT COUNT(*) AS n FROM chunks_fts WHERE chunks_fts MATCH '"appears exactly here"'`,
    ),
    column_filter: tryQuery(
      db,
      "SELECT COUNT(*) AS n FROM chunks_fts WHERE chunks_fts MATCH 'title:incidents'",
    ),
    bm25_rank: tryQuery(
      db,
      `SELECT rowid FROM chunks_fts WHERE chunks_fts MATCH 'section'
       ORDER BY bm25(chunks_fts, 5.0, 6.0, 6.0, 6.0, 3.0, 1.0, 2.0, 1.0, 5.0) LIMIT 5`,
    ),
    snippet: (() => {
      const attempt = tryQuery(
        db,
        `SELECT snippet(chunks_fts, 7, '[', ']', '…', 8) AS s
         FROM chunks_fts WHERE chunks_fts MATCH 'zeta9750term' LIMIT 1`,
      );
      // A null/empty snippet is a failure: the check must demand the
      // actual highlighted marker, or contentless tables pass vacuously.
      if (!attempt.ok) return attempt;
      const text = attempt.rows?.[0]?.s;
      return typeof text === 'string' && text.includes('[zeta9750term]')
        ? { ok: true, rows: [{ n: true }] }
        : { ok: false, error: `empty snippet (${JSON.stringify(text).slice(0, 20)})` };
    })(),
    delete_visible: (() => {
      try {
        db.exec('BEGIN');
        if (variant.externalContent) {
          db.exec("DELETE FROM chunks WHERE source_key = 'vault notes/architecture/note-00000.md'");
        } else {
          const rows = selectAll(
            db,
            "SELECT rowid FROM chunks WHERE source_key = 'vault notes/architecture/note-00000.md'",
          );
          for (const row of rows) {
            db.exec({ sql: 'DELETE FROM chunks_fts WHERE rowid = ?', bind: [row.rowid] });
            db.exec({ sql: 'DELETE FROM chunks WHERE rowid = ?', bind: [row.rowid] });
          }
        }
        db.exec('COMMIT');
        const gone =
          selectAll(db, "SELECT COUNT(*) AS n FROM chunks_fts WHERE chunks_fts MATCH 'zeta0term'")[0]
            .n === 0;
        return { ok: gone };
      } catch (error) {
        return { ok: false, error: String(error).slice(0, 60) };
      }
    })(),
  };

  const exportStarted = performance.now();
  const blob = sqlite3.capi.sqlite3_js_db_export(db);
  const exportMs = performance.now() - exportStarted;
  db.close();

  results.push({
    variant: name,
    blob_bytes: blob.byteLength,
    build_ms: Math.round(buildMs),
    export_ms: Math.round(exportMs),
    compressed_content_bytes: compressedBytes || undefined,
    capability: Object.fromEntries(
      Object.entries(capability).map(([key, value]) => [
        key,
        value.ok ? (value.rows?.[0]?.n ?? value.rows?.[0]?.s ?? true) && true : `NO (${value.error ?? ''})`,
      ]),
    ),
  });
}

const baseline = results[0].blob_bytes;
for (const result of results) {
  result.blob_mib = +(result.blob_bytes / 1048576).toFixed(1);
  result.vs_baseline = +(result.blob_bytes / baseline).toFixed(3);
}
console.log(JSON.stringify(results, null, 2));
