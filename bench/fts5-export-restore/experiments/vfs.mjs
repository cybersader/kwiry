// Custom JS VFS on the pinned official build: db image lives in JS-side
// blocks OUTSIDE wasm linear memory; pages are copied into wasm on demand.
// Optional: blocks held deflate-compressed with an LRU of decompressed blocks.
import fs from 'node:fs';
import { deflateRawSync, inflateRawSync } from 'node:zlib';
import sqlite3InitModule from '../../fts5-wasm/node_modules/@sqlite.org/sqlite-wasm/dist/node.mjs';

const OUT='.';
const blobPath = process.argv[2] || `${OUT}/contentless-8192.db`;
const MODE = process.argv[3] || 'plain';          // plain | zip
const CACHE_SIZE = process.argv[4] || '-2000';    // KiB (negative)
const BLOCK = Number(process.argv[5] || 65536);
const LRU_BLOCKS = Number(process.argv[6] || 64);

const sqlite3 = await sqlite3InitModule({print:()=>{},printErr:()=>{}});
const capi=sqlite3.capi, wasm=sqlite3.wasm, util=sqlite3.util;
const M=n=>+(n/1048576).toFixed(1);
const marks=[]; const mark=l=>marks.push({label:l,wasm_MiB:M(wasm.memory.buffer.byteLength),rss_MiB:M(process.memoryUsage().rss),ab_MiB:M(process.memoryUsage().arrayBuffers)});
mark('init');

// ---- JS-side block store -------------------------------------------------
class BlockFile {
  constructor(name){ this.name=name; this.size=0; this.blocks=new Map(); this.z=new Map(); this.lru=new Map(); this.reads=0; this.inflates=0; }
  static fromBytes(name, bytes, compress){
    const f=new BlockFile(name); f.size=bytes.byteLength;
    for(let off=0,i=0; off<bytes.byteLength; off+=BLOCK,i++){
      const b=bytes.subarray(off, Math.min(off+BLOCK, bytes.byteLength));
      if(compress) f.z.set(i, deflateRawSync(b,{level:6}));
      else f.blocks.set(i, new Uint8Array(b));   // detached copy
    }
    return f;
  }
  compressedBytes(){ let n=0; for(const v of this.z.values()) n+=v.byteLength; return n; }
  getBlock(i){
    let b=this.blocks.get(i); if(b) return b;
    const zb=this.z.get(i);
    if(zb){ b=this.lru.get(i);
      if(b){ this.lru.delete(i); this.lru.set(i,b); return b; }
      b=new Uint8Array(inflateRawSync(zb)); this.inflates++;
      this.lru.set(i,b);
      while(this.lru.size>LRU_BLOCKS){ const k=this.lru.keys().next().value; this.lru.delete(k); }
      return b; }
    return null;
  }
  ensureWritable(i){                       // copy-on-write: dirty blocks uncompressed
    let b=this.blocks.get(i);
    if(b) return b;
    const src=this.getBlock(i);
    b=new Uint8Array(BLOCK); if(src) b.set(src.subarray(0,Math.min(src.length,BLOCK)));
    this.blocks.set(i,b); this.z.delete(i); this.lru.delete(i);
    return b;
  }
  read(dst, off){ // dst Uint8Array
    let done=0; this.reads++;
    while(done<dst.length){
      const gi=Math.floor((off+done)/BLOCK), gs=(off+done)%BLOCK;
      const n=Math.min(dst.length-done, BLOCK-gs);
      const b=this.getBlock(gi);
      if(!b) return done;                       // short read
      const avail=Math.max(0, Math.min(n, b.length-gs));
      if(avail>0) dst.set(b.subarray(gs, gs+avail), done);
      if(avail<n) return done+avail;
      done+=n;
    }
    return done;
  }
  write(src, off){
    let done=0;
    while(done<src.length){
      const gi=Math.floor((off+done)/BLOCK), gs=(off+done)%BLOCK;
      const n=Math.min(src.length-done, BLOCK-gs);
      const b=this.ensureWritable(gi);
      b.set(src.subarray(done,done+n), gs);
      done+=n;
    }
    if(off+src.length>this.size) this.size=off+src.length;
    return done;
  }
  truncate(n){ this.size=n; const last=Math.floor(n/BLOCK);
    for(const k of [...this.blocks.keys()]) if(k>last) this.blocks.delete(k);
    for(const k of [...this.z.keys()]) if(k>last) this.z.delete(k);
    this.lru.clear(); }
}
const FILES = new Map();

// ---- VFS -----------------------------------------------------------------
const vfsName='jsmem';
const jsVfs = new capi.sqlite3_vfs();
const jsIo  = new capi.sqlite3_io_methods();
jsIo.$iVersion = 1;
jsVfs.$iVersion = 2;
jsVfs.$szOsFile = capi.sqlite3_file.structInfo.sizeof;
jsVfs.$mxPathname = 1024;
const openHandles = new Map();   // pFile (as string) -> {file, flags}
const K = p => String(p);
const u8 = () => wasm.heap8u();

const ioMethods = {
  xClose(pFile){ const h=openHandles.get(K(pFile)); if(h){ openHandles.delete(K(pFile)); if(h.deleteOnClose) FILES.delete(h.file.name); } return 0; },
  xRead(pFile, pDest, n, offset64){
    const h=openHandles.get(K(pFile)); if(!h) return capi.SQLITE_IOERR_READ;
    const off=Number(offset64), N=Number(n), d=Number(pDest);
    const dst=u8().subarray(d, d+N);            // write straight into wasm heap
    const got=h.file.read(dst, off);
    if(got<N){ dst.fill(0, got, N); return capi.SQLITE_IOERR_SHORT_READ; }
    return 0;
  },
  xWrite(pFile, pSrc, n, offset64){
    const h=openHandles.get(K(pFile)); if(!h) return capi.SQLITE_IOERR_WRITE;
    const N=Number(n), s=Number(pSrc);
    h.file.write(u8().subarray(s, s+N), Number(offset64));
    return 0;
  },
  xTruncate(pFile, sz){ const h=openHandles.get(K(pFile)); if(h) h.file.truncate(Number(sz)); return 0; },
  xSync(pFile, flags){ return 0; },
  xFileSize(pFile, pSz){ const h=openHandles.get(K(pFile)); wasm.poke(pSz, BigInt(h?h.file.size:0), 'i64'); return 0; },
  xLock(){ return 0; }, xUnlock(){ return 0; },
  xCheckReservedLock(pFile,pOut){ wasm.poke(pOut,0,'i32'); return 0; },
  xFileControl(){ return capi.SQLITE_NOTFOUND; },
  xSectorSize(){ return 4096; },
  xDeviceCharacteristics(){ return capi.SQLITE_IOCAP_ATOMIC | capi.SQLITE_IOCAP_SAFE_APPEND | capi.SQLITE_IOCAP_SEQUENTIAL | capi.SQLITE_IOCAP_POWERSAFE_OVERWRITE; },
};
const vfsMethods = {
  xOpen(pVfs, zName, pFile, flags, pOutFlags){
    const name = zName ? wasm.cstrToJs(zName) : '/anon-'+Math.random().toString(36).slice(2);
    let f = FILES.get(name);
    if(!f){ if(!(flags & capi.SQLITE_OPEN_CREATE)) return capi.SQLITE_CANTOPEN; f=new BlockFile(name); FILES.set(name,f); }
    openHandles.set(K(pFile), {file:f, flags, deleteOnClose: !!(flags & capi.SQLITE_OPEN_DELETEONCLOSE)});
    const sf = new capi.sqlite3_file(pFile); sf.$pMethods = jsIo.pointer; sf.dispose();
    if(pOutFlags) wasm.poke(pOutFlags, flags, 'i32');
    return 0;
  },
  xDelete(pVfs, zName, sync){ FILES.delete(wasm.cstrToJs(zName)); return 0; },
  xAccess(pVfs, zName, flags, pOut){ wasm.poke(pOut, FILES.has(wasm.cstrToJs(zName))?1:0, 'i32'); return 0; },
  xFullPathname(pVfs, zName, nOut, pOut){ return wasm.cstrncpy(pOut, zName, nOut) < nOut ? 0 : capi.SQLITE_CANTOPEN; },
  xCurrentTime(pVfs,pOut){ wasm.poke(pOut, 2440587.5 + Date.now()/864e5, 'double'); return 0; },
  xCurrentTimeInt64(pVfs,pOut){ wasm.poke(pOut, BigInt(Math.round(2440587.5*864e5 + Date.now())), 'i64'); return 0; },
  xGetLastError(){ return 0; },
};
{ const pD=capi.sqlite3_vfs_find(null); if(pD){ const d=new capi.sqlite3_vfs(pD); jsVfs.$xRandomness=d.$xRandomness; jsVfs.$xSleep=d.$xSleep; d.dispose(); } }
sqlite3.vfs.installVfs({ io:{struct:jsIo, methods:ioMethods}, vfs:{struct:jsVfs, methods:vfsMethods, name:vfsName, asDefault:false} });
mark('vfs-installed');

// ---- load image into JS blocks ------------------------------------------
let raw = new Uint8Array(fs.readFileSync(blobPath));
const rawLen = raw.byteLength;
const file = BlockFile.fromBytes('/cache.db', raw, MODE==='zip');
raw = null;
FILES.set('/cache.db', file);
const imageMiB=M(rawLen), zMiB = MODE==='zip'? M(file.compressedBytes()) : null;
mark(`image-loaded(${MODE},${imageMiB}MiB${zMiB?`->${zMiB}MiB`:''})`);
if(global.gc) global.gc();
mark('after-gc');

let db;
if (MODE==='deser'){
  const bytes=new Uint8Array(fs.readFileSync(blobPath));
  const p2=wasm.allocFromTypedArray(bytes);
  db=new sqlite3.oo1.DB(':memory:','c');
  const rc=capi.sqlite3_deserialize(db.pointer,'main',p2,bytes.byteLength,bytes.byteLength,
      capi.SQLITE_DESERIALIZE_FREEONCLOSE|capi.SQLITE_DESERIALIZE_RESIZEABLE);
  if(rc) throw new Error('deserialize rc='+rc);
} else db = new sqlite3.oo1.DB({filename:'file:/cache.db?vfs=jsmem', flags:'c'});
db.exec(`PRAGMA cache_size=${CACHE_SIZE}`);
db.exec('PRAGMA journal_mode=MEMORY');
mark('opened');
const n=db.selectValue('select count(*) from chunks_fts');
mark(`count(${n})`);
const terms=['section','aaa','architecture','incidents','note','zzz','data','system'];
let hits=0, t0=performance.now();
for(const t of terms) db.exec({sql:`SELECT rowid FROM chunks_fts WHERE chunks_fts MATCH ? ORDER BY bm25(chunks_fts) LIMIT 20`,bind:[t],rowMode:'array',callback:()=>{hits++;}});
const qms=performance.now()-t0;
mark(`queries(${hits},${Math.round(qms)}ms)`);
// repeat to get warm timing
t0=performance.now(); let hits2=0;
for(const t of terms) db.exec({sql:`SELECT rowid FROM chunks_fts WHERE chunks_fts MATCH ? ORDER BY bm25(chunks_fts) LIMIT 20`,bind:[t],rowMode:'array',callback:()=>{hits2++;}});
const qms2=performance.now()-t0;
mark(`queries-warm(${hits2},${Math.round(qms2)}ms)`);
// write path: insert + delete
try{
  db.exec('BEGIN'); db.exec(`INSERT INTO chunks_fts(rowid,filename,stem,aliases,title,heading_text,path_text,tags,content,identifiers) VALUES (999999,'x','x','x','x','x','x','x','novelmarkertoken here','x')`); db.exec('COMMIT');
  const seen=db.selectValue(`SELECT count(*) FROM chunks_fts WHERE chunks_fts MATCH 'novelmarkertoken'`);
  mark(`write-ok(seen=${seen})`);
}catch(e){ mark('write-FAILED:'+String(e).slice(0,60)); }
try{ db.exec("INSERT INTO chunks_fts(chunks_fts) VALUES('integrity-check')"); mark('fts-integrity-ok'); }catch(e){ mark('fts-integrity-FAIL:'+String(e).slice(0,40)); }
// heavier workload: 400 ranked queries over varied terms
{ const t=performance.now(); let h=0;
  for(let i=0;i<400;i++){ const t2=['section','architecture','incidents','system','data','note','reference','runbook'][i%8];
    db.exec({sql:`SELECT rowid FROM chunks_fts WHERE chunks_fts MATCH ? ORDER BY bm25(chunks_fts) LIMIT 50`,bind:[t2],rowMode:'array',callback:()=>{h++;}}); }
  mark(`heavy400(${h},${Math.round(performance.now()-t)}ms)`); }
{ const t=performance.now(); let h=0;
  db.exec({sql:`SELECT rowid FROM chunks_fts WHERE chunks_fts MATCH 'a* OR b* OR c*' ORDER BY bm25(chunks_fts) LIMIT 500`,rowMode:'array',callback:()=>{h++;}});
  mark(`prefix-wide(${h},${Math.round(performance.now()-t)}ms)`); }
if(process.env.DIGEST){ const t=performance.now();
  let acc=0; db.exec({sql:"SELECT group_concat(source_key||chunk_id||path||content_hash||coalesce(content,'')) FROM chunks",rowMode:'array',callback:r=>{acc=(r[0]||'').length;}});
  mark(`digest(${acc}chars,${Math.round(performance.now()-t)}ms)`); }
if(process.env.SNIPPET){ const t=performance.now(); let h=0;
  db.exec({sql:"SELECT snippet(chunks_fts,7,'[',']','…',10) FROM chunks_fts WHERE chunks_fts MATCH 'section' LIMIT 50",rowMode:'array',callback:()=>{h++;}});
  mark(`snippet(${h},${Math.round(performance.now()-t)}ms)`); }
db.exec('PRAGMA shrink_memory'); mark('shrink_memory');
if(global.gc) global.gc(); mark('final-gc');
console.log(JSON.stringify({MODE,CACHE_SIZE,BLOCK,LRU_BLOCKS,image_MiB:imageMiB,compressed_MiB:zMiB,
  block_reads:file.reads, inflates:file.inflates, dirty_blocks:file.blocks.size, lru_resident:file.lru.size},null,0));
console.table(marks);
