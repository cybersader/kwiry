import sqlite3InitModule from '../../fts5-wasm/node_modules/@sqlite.org/sqlite-wasm/dist/node.mjs';
import { streamNotes } from '../src/corpus.mjs';
const COLS = 'filename, stem, aliases, title, heading_text, path_text, tags, content, identifiers';
const sqlite3 = await sqlite3InitModule({ print: () => {}, printErr: () => {} });
const M = (n) => +(n / 1048576).toFixed(1);
for (const opt of [', contentless_delete=1', '', ', columnsize=0']) {
  const db = new sqlite3.oo1.DB(':memory:');
  db.exec('CREATE TABLE chunks (rowid INTEGER PRIMARY KEY, source_key TEXT NOT NULL, chunk_id TEXT NOT NULL UNIQUE, path TEXT NOT NULL, mtime INTEGER NOT NULL, content_hash TEXT NOT NULL); CREATE INDEX chunks_by_source ON chunks(source_key);');
  db.exec(`CREATE VIRTUAL TABLE chunks_fts USING fts5(${COLS}, content='', ${opt}, tokenize='unicode61 remove_diacritics 2');`);
  const im = db.prepare('INSERT INTO chunks (source_key,chunk_id,path,mtime,content_hash) VALUES (?,?,?,?,?)');
  const iF = db.prepare(`INSERT INTO chunks_fts (rowid, ${COLS}) VALUES (?,?,?,?,?,?,?,?,?,?)`);
  db.exec('BEGIN IMMEDIATE');
  let r = 0;
  for (const n of streamNotes()) for (const c of n.chunks) { r++; im.bind([c.source_key, c.chunk_id, c.path, c.mtime, c.content_hash]).stepReset(); iF.bind([r, c.filename, c.stem, c.aliases, c.title, c.heading_text, c.path_text, c.tags, c.content, c.identifiers]).stepReset(); }
  im.finalize(); iF.finalize();
  db.exec('COMMIT');
  db.exec("INSERT INTO chunks_fts(chunks_fts) VALUES('optimize')");
  db.exec('VACUUM');
  let bm = 'ok', hits = 0;
  try { db.exec({ sql: "SELECT rowid FROM chunks_fts WHERE chunks_fts MATCH 'section' ORDER BY bm25(chunks_fts,5.0,6.0,6.0,6.0,3.0,1.0,2.0,1.0,5.0) LIMIT 5", rowMode: 'array', callback: () => hits++ }); } catch (e) { bm = String(e).slice(0, 70); }
  console.log(('opts=' + (opt || 'default')).padEnd(22), M(sqlite3.capi.sqlite3_js_db_export(db).byteLength), 'MiB  bm25=', bm, 'hits=', hits);
  db.close();
}
