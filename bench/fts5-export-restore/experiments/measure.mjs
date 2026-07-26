// Where does steady-state memory actually go? wasm linear memory is the
// non-shrinkable floor; measure it at each phase.
import fs from 'node:fs';
import sqlite3InitModule from '../../fts5-wasm/node_modules/@sqlite.org/sqlite-wasm/dist/node.mjs';
const OUT='.';
const blobPath = process.argv[2] || `${OUT}/contentless-8192.db`;
const cacheSize = process.argv[3] || '-16384';
const sqlite3 = await sqlite3InitModule({print:()=>{},printErr:()=>{}});
const c=sqlite3.capi,w=sqlite3.wasm;
const M=n=>+(n/1048576).toFixed(1);
const marks=[];
function mark(label){ marks.push({label, wasm_MiB:M(w.memory.buffer.byteLength), rss_MiB:M(process.memoryUsage().rss), ab_MiB:M(process.memoryUsage().arrayBuffers)}); }
mark('init');
const blob = new Uint8Array(fs.readFileSync(blobPath));
mark('blob-read-js('+M(blob.byteLength)+'MiB)');
const p = w.allocFromTypedArray(blob);
const db = new sqlite3.oo1.DB(':memory:','c');
const rc = c.sqlite3_deserialize(db.pointer,'main',p,blob.byteLength,blob.byteLength,
   c.SQLITE_DESERIALIZE_FREEONCLOSE | c.SQLITE_DESERIALIZE_RESIZEABLE);
if(rc) throw new Error('deserialize rc='+rc);
mark('deserialize');
db.exec(`PRAGMA cache_size=${cacheSize}`);
const n=db.selectValue('select count(*) from chunks_fts');
mark(`count(${n})`);
// query workload
const terms=['section','aaa','architecture','incidents','note','zzz','data','system'];
let hits=0;
for(const t of terms){
  db.exec({sql:`SELECT rowid FROM chunks_fts WHERE chunks_fts MATCH ? ORDER BY bm25(chunks_fts) LIMIT 20`,bind:[t],
    rowMode:'array', callback:()=>{hits++;}});
}
mark('queries('+hits+')');
try{ db.exec("INSERT INTO chunks_fts(chunks_fts) VALUES('integrity-check')"); mark('fts-integrity'); }catch(e){ mark('fts-integrity-FAILED'); }
try{ db.exec('PRAGMA integrity_check'); mark('integrity_check'); }catch(e){ mark('integrity_check-FAIL'); }
db.exec('PRAGMA shrink_memory');
mark('shrink_memory');
console.log('cache_size='+cacheSize, 'dbstatus CACHE_USED=', (()=>{const s=w.pstack.pointer;try{const a=w.pstack.allocPtr(2);c.sqlite3_db_status(db.pointer,c.SQLITE_DBSTATUS_CACHE_USED,a[0],a[1],0);return M(w.peek32(a[0]));}finally{w.pstack.restore(s);}})()+' MiB');
console.table(marks);
