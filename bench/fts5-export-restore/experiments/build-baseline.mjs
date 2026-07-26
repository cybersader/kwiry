// Faithful reproduction of the gate's baseline schema (external content +
// triggers), exported to disk for steady-state memory analysis.
import fs from 'node:fs';
import sqlite3InitModule from '../../fts5-wasm/node_modules/@sqlite.org/sqlite-wasm/dist/node.mjs';
import { streamNotes } from '../src/corpus.mjs';
const OUT = '.';
const COLS = 'filename, stem, aliases, title, heading_text, path_text, tags, content, identifiers';
const sqlite3 = await sqlite3InitModule({ print: () => {}, printErr: () => {} });
const db = new sqlite3.oo1.DB(':memory:');
db.exec(`CREATE TABLE chunks (
 rowid INTEGER PRIMARY KEY, source_key TEXT NOT NULL, chunk_id TEXT NOT NULL UNIQUE,
 vault_id TEXT NOT NULL, path TEXT NOT NULL, heading_path_json TEXT NOT NULL,
 frontmatter_json TEXT NOT NULL, mtime INTEGER NOT NULL, content_hash TEXT NOT NULL,
 chunking_version INTEGER NOT NULL, filename TEXT NOT NULL, stem TEXT NOT NULL,
 aliases TEXT NOT NULL, title TEXT NOT NULL, heading_text TEXT NOT NULL,
 path_text TEXT NOT NULL, tags TEXT NOT NULL, content TEXT NOT NULL, identifiers TEXT NOT NULL);
 CREATE INDEX chunks_by_source ON chunks(source_key);`);
db.exec(`CREATE VIRTUAL TABLE chunks_fts USING fts5(${COLS}, content='chunks', content_rowid='rowid', tokenize='unicode61 remove_diacritics 2');`);
const newVals = COLS.split(', ').map((c) => 'new.' + c).join(', ');
db.exec(`CREATE TRIGGER chunks_ai AFTER INSERT ON chunks BEGIN INSERT INTO chunks_fts(rowid, ${COLS}) VALUES (new.rowid, ${newVals}); END;`);
const ins = db.prepare(`INSERT INTO chunks (source_key,chunk_id,vault_id,path,heading_path_json,frontmatter_json,mtime,content_hash,chunking_version,filename,stem,aliases,title,heading_text,path_text,tags,content,identifiers) VALUES (${'?, '.repeat(17)}?)`);
db.exec('BEGIN IMMEDIATE');
for (const note of streamNotes()) for (const c of note.chunks) {
  ins.bind([c.source_key, c.chunk_id, c.vault_id, c.path, c.heading_path_json, c.frontmatter_json, c.mtime, c.content_hash, c.chunking_version, c.filename, c.stem, c.aliases, c.title, c.heading_text, c.path_text, c.tags, c.content, c.identifiers]).stepReset();
}
ins.finalize();
db.exec('COMMIT');
const blob = sqlite3.capi.sqlite3_js_db_export(db);
fs.writeFileSync(OUT + '/baseline.db', blob);
console.log('baseline blob MiB', +(blob.byteLength / 1048576).toFixed(1), 'pages', db.selectValue('pragma page_count'));
db.close();
