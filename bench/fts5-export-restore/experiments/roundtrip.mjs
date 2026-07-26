// Block-VFS round trip: does the JS-side block store reproduce a valid,
// byte-comparable image without any sqlite3_js_db_export() wasm copy?
import fs from 'node:fs';
import crypto from 'node:crypto';
import { deflateRawSync, inflateRawSync } from 'node:zlib';
import sqlite3InitModule from '../../fts5-wasm/node_modules/@sqlite.org/sqlite-wasm/dist/node.mjs';

const path = process.argv[2];
const BLOCK = 65536;
const sqlite3 = await sqlite3InitModule({ print: () => {}, printErr: () => {} });
const capi = sqlite3.capi, wasm = sqlite3.wasm;
const M = (n) => +(n / 1048576).toFixed(1);
const sha = (b) => crypto.createHash('sha256').update(b).digest('hex').slice(0, 16);

const files = new Map();
function mkfile(name, bytes) {
  const blocks = new Map();
  if (bytes) for (let o = 0, i = 0; o < bytes.byteLength; o += BLOCK, i++) blocks.set(i, new Uint8Array(bytes.subarray(o, Math.min(o + BLOCK, bytes.byteLength))));
  const f = { name, size: bytes ? bytes.byteLength : 0, blocks };
  files.set(name, f);
  return f;
}
function dump(f) {
  const out = new Uint8Array(f.size);
  for (const [i, b] of f.blocks) { const off = i * BLOCK; if (off >= f.size) continue; out.set(b.subarray(0, Math.min(b.length, f.size - off)), off); }
  return out;
}
function blk(f, i) { let b = f.blocks.get(i); if (!b) { b = new Uint8Array(BLOCK); f.blocks.set(i, b); } return b; }

const vfsName = 'jsblk';
const V = new capi.sqlite3_vfs(), IO = new capi.sqlite3_io_methods();
IO.$iVersion = 1; V.$iVersion = 2; V.$szOsFile = capi.sqlite3_file.structInfo.sizeof; V.$mxPathname = 1024;
const H = new Map(); const K = (p) => String(p); const u8 = () => wasm.heap8u();
const io = {
  xClose(p) { H.delete(K(p)); return 0; },
  xRead(p, pDest, n, off) {
    const f = H.get(K(p)).f, N = Number(n), o = Number(off), d = Number(pDest);
    const dst = u8().subarray(d, d + N);
    let done = 0;
    while (done < N) {
      const gi = Math.floor((o + done) / BLOCK), gs = (o + done) % BLOCK, k = Math.min(N - done, BLOCK - gs);
      const b = f.blocks.get(gi);
      if (!b) { dst.fill(0, done, N); return capi.SQLITE_IOERR_SHORT_READ; }
      dst.set(b.subarray(gs, gs + k), done); done += k;
    }
    return 0;
  },
  xWrite(p, pSrc, n, off) {
    const f = H.get(K(p)).f, N = Number(n), o = Number(off), s = Number(pSrc);
    const src = u8().subarray(s, s + N);
    let done = 0;
    while (done < N) {
      const gi = Math.floor((o + done) / BLOCK), gs = (o + done) % BLOCK, k = Math.min(N - done, BLOCK - gs);
      blk(f, gi).set(src.subarray(done, done + k), gs); done += k;
    }
    if (o + N > f.size) f.size = o + N;
    return 0;
  },
  xTruncate(p, sz) { const f = H.get(K(p)).f; f.size = Number(sz); const last = Math.floor(f.size / BLOCK); for (const k of [...f.blocks.keys()]) if (k > last) f.blocks.delete(k); return 0; },
  xSync() { return 0; },
  xFileSize(p, pSz) { wasm.poke(pSz, BigInt(H.get(K(p)).f.size), 'i64'); return 0; },
  xLock() { return 0; }, xUnlock() { return 0; },
  xCheckReservedLock(p, o) { wasm.poke(o, 0, 'i32'); return 0; },
  xFileControl() { return capi.SQLITE_NOTFOUND; },
  xSectorSize() { return 4096; },
  xDeviceCharacteristics() { return capi.SQLITE_IOCAP_ATOMIC | capi.SQLITE_IOCAP_SAFE_APPEND | capi.SQLITE_IOCAP_SEQUENTIAL | capi.SQLITE_IOCAP_POWERSAFE_OVERWRITE; },
};
const vm = {
  xOpen(pVfs, zName, pFile, flags, pOut) {
    const nm = zName ? wasm.cstrToJs(zName) : '/tmp-' + Math.random();
    let f = files.get(nm);
    if (!f) { if (!(flags & capi.SQLITE_OPEN_CREATE)) return capi.SQLITE_CANTOPEN; f = mkfile(nm, null); }
    H.set(K(pFile), { f, flags });
    const sf = new capi.sqlite3_file(pFile); sf.$pMethods = IO.pointer; sf.dispose();
    if (pOut) wasm.poke(pOut, flags, 'i32');
    return 0;
  },
  xDelete(v, z) { files.delete(wasm.cstrToJs(z)); return 0; },
  xAccess(v, z, fl, pOut) { wasm.poke(pOut, files.has(wasm.cstrToJs(z)) ? 1 : 0, 'i32'); return 0; },
  xFullPathname(v, z, n, o) { return wasm.cstrncpy(o, z, n) < n ? 0 : capi.SQLITE_CANTOPEN; },
  xCurrentTime(v, o) { wasm.poke(o, 2440587.5 + Date.now() / 864e5, 'double'); return 0; },
  xCurrentTimeInt64(v, o) { wasm.poke(o, BigInt(Math.round(2440587.5 * 864e5 + Date.now())), 'i64'); return 0; },
  xGetLastError() { return 0; },
};
{ const pD = capi.sqlite3_vfs_find(null); if (pD) { const d = new capi.sqlite3_vfs(pD); V.$xRandomness = d.$xRandomness; V.$xSleep = d.$xSleep; d.dispose(); } }
sqlite3.vfs.installVfs({ io: { struct: IO, methods: io }, vfs: { struct: V, methods: vm, name: vfsName } });

const orig = new Uint8Array(fs.readFileSync(path));
mkfile('/c.db', orig);
console.log('original', M(orig.byteLength), 'MiB sha', sha(orig));

// read-only-ish session
let db = new sqlite3.oo1.DB({ filename: 'file:/c.db?vfs=' + vfsName, flags: 'c' });
db.exec('PRAGMA journal_mode=MEMORY');
const n1 = db.selectValue('select count(*) from chunks_fts');
db.close();
const after = dump(files.get('/c.db'));
console.log('after open/close: sha', sha(after), 'identical=', Buffer.compare(Buffer.from(orig), Buffer.from(after)) === 0, 'rows', n1);

// mutate, then reload the dumped image in a *fresh* store and validate
db = new sqlite3.oo1.DB({ filename: 'file:/c.db?vfs=' + vfsName, flags: 'c' });
db.exec('PRAGMA journal_mode=MEMORY');
db.exec('BEGIN');
db.exec("INSERT INTO chunks_fts(rowid,filename,stem,aliases,title,heading_text,path_text,tags,content,identifiers) VALUES (888888,'m','m','m','m','m','m','m','uniquemarkerabc token','m')");
db.exec('COMMIT');
db.close();
const mutated = dump(files.get('/c.db'));
console.log('mutated image', M(mutated.byteLength), 'MiB sha', sha(mutated));

files.delete('/c.db');
mkfile('/c.db', mutated);
db = new sqlite3.oo1.DB({ filename: 'file:/c.db?vfs=' + vfsName, flags: 'c' });
console.log('reload integrity_check', db.selectValue('pragma integrity_check'));
try { db.exec("INSERT INTO chunks_fts(chunks_fts) VALUES('integrity-check')"); console.log('fts integrity-check OK'); } catch (e) { console.log('fts integrity-check FAIL', String(e).slice(0, 80)); }
console.log('marker visible', db.selectValue("select count(*) from chunks_fts where chunks_fts match 'uniquemarkerabc'"));
console.log('rows', db.selectValue('select count(*) from chunks_fts'));
db.close();

// compressed cache format: what does the on-disk cache cost?
let z = 0; const t = performance.now();
for (let o = 0; o < mutated.byteLength; o += BLOCK) z += deflateRawSync(mutated.subarray(o, Math.min(o + BLOCK, mutated.byteLength)), { level: 6 }).byteLength;
console.log('compressed cache blob', M(z), 'MiB in', Math.round(performance.now() - t), 'ms (no wasm allocation at all)');
console.log('wasm memory MiB (never grew past init):', M(wasm.memory.buffer.byteLength));
