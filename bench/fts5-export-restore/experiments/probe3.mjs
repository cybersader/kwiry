import sqlite3InitModule from '../../fts5-wasm/node_modules/@sqlite.org/sqlite-wasm/dist/node.mjs';
const sqlite3 = await sqlite3InitModule({ print: () => {}, printErr: () => {} });
const w = sqlite3.wasm, c = sqlite3.capi;
console.log('memory.type()', typeof w.memory.type === 'function' ? w.memory.type() : 'no type()');
console.log('Memory.prototype.discard?', typeof WebAssembly.Memory.prototype.discard);
console.log('Memory.prototype.grow?', typeof WebAssembly.Memory.prototype.grow);
console.log('Memory.prototype.toResizableBuffer?', typeof WebAssembly.Memory.prototype.toResizableBuffer);
console.log('buffer.resizable?', w.memory.buffer.resizable, 'maxByteLength', w.memory.buffer.maxByteLength);
console.log('node', process.version);

// kvvfs v2: is a named, non-Storage-backed kvvfs usable in a worker?
try {
  sqlite3.kvvfs.reserve('kwiry');
  const db = new sqlite3.oo1.DB({ filename: 'file:kwiry?vfs=kvvfs', flags: 'c' });
  db.exec("CREATE TABLE t(x); CREATE VIRTUAL TABLE ft USING fts5(x);");
  db.exec('BEGIN');
  const st = db.prepare('INSERT INTO ft(x) VALUES(?)');
  for (let i = 0; i < 5000; i++) st.bind(['alpha beta gamma token' + i + ' some words here for the index']).stepReset();
  st.finalize();
  db.exec('COMMIT');
  console.log('kvvfs match count', db.selectValue("select count(*) from ft where ft match 'alpha'"));
  console.log('kvvfs estimateSize bytes', sqlite3.kvvfs.estimateSize('kwiry'));
  console.log('kvvfs page_count/page_size', db.selectValue('pragma page_count'), db.selectValue('pragma page_size'));
  console.log('wasm MiB after kvvfs build', +(w.memory.buffer.byteLength / 1048576).toFixed(1));
  const exp = sqlite3.kvvfs.export('kwiry');
  console.log('kvvfs export type', exp && exp.constructor && exp.constructor.name, 'keys', exp && Object.keys(exp).slice(0, 5));
  db.close();
} catch (e) {
  console.log('kvvfs FAILED:', String(e).slice(0, 200));
}
console.log('SQLITE_FCNTL_PERSIST_WAL', c.SQLITE_FCNTL_PERSIST_WAL, 'SQLITE_DBCONFIG_?', Object.keys(c).filter((k) => /DBCONFIG/.test(k)).length);
console.log('SQLITE_CONFIG_* available:', Object.keys(c).filter((k) => /^SQLITE_CONFIG/.test(k)).join(','));
