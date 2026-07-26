import fs from 'node:fs';
import sqlite3InitModule from '../../fts5-wasm/node_modules/@sqlite.org/sqlite-wasm/dist/node.mjs';
const sqlite3 = await sqlite3InitModule({ print: () => {}, printErr: () => {} });
const capi = sqlite3.capi, wasm = sqlite3.wasm;
const M = (n) => +(n / 1048576).toFixed(2);
const bytes = new Uint8Array(fs.readFileSync(process.argv[2]));
function open() {
  const p = wasm.allocFromTypedArray(bytes);
  const db = new sqlite3.oo1.DB(':memory:', 'c');
  const rc = capi.sqlite3_deserialize(db.pointer, 'main', p, bytes.byteLength, bytes.byteLength, capi.SQLITE_DESERIALIZE_FREEONCLOSE | capi.SQLITE_DESERIALIZE_RESIZEABLE);
  if (rc) throw new Error('rc ' + rc);
  return db;
}
const size = (db) => M(capi.sqlite3_js_db_export(db).byteLength);
console.log('start', M(bytes.byteLength));
for (const step of [
  ['vacuum only', (db) => db.exec('VACUUM')],
  ["fts optimize", (db) => db.exec("INSERT INTO chunks_fts(chunks_fts) VALUES('optimize')")],
  ["optimize+vacuum", (db) => { db.exec("INSERT INTO chunks_fts(chunks_fts) VALUES('optimize')"); db.exec('VACUUM'); }],
  ["merge=-16+vacuum", (db) => { db.exec("INSERT INTO chunks_fts(chunks_fts, rank) VALUES('merge', -16)"); db.exec('VACUUM'); }],
  ["pgsz32k+optimize+vacuum", (db) => { db.exec("INSERT INTO chunks_fts(chunks_fts) VALUES('optimize')"); db.exec('PRAGMA page_size=32768'); db.exec('VACUUM'); }],
  ["crisismerge+vacuum", (db) => { db.exec("INSERT INTO chunks_fts(chunks_fts, rank) VALUES('crisismerge', 16)"); db.exec('VACUUM'); }],
]) {
  const db = open();
  const t = performance.now();
  let err = '';
  try { step[1](db); } catch (e) { err = ' ERR:' + String(e).slice(0, 60); }
  console.log(step[0].padEnd(24), size(db), 'MiB', Math.round(performance.now() - t) + 'ms', 'wasm=' + M(wasm.memory.buffer.byteLength) + err);
  db.close();
}
