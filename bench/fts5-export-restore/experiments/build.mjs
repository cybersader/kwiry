// Build corpus DBs and export blobs. Variants x page_size x post-processing.
import fs from 'node:fs';
import sqlite3InitModule from '../../fts5-wasm/node_modules/@sqlite.org/sqlite-wasm/dist/node.mjs';
import { streamNotes } from '../src/corpus.mjs';

const OUT='.';
const FTS_COLUMNS='filename, stem, aliases, title, heading_text, path_text, tags, content, identifiers';
const sqlite3 = await sqlite3InitModule({print:()=>{},printErr:()=>{}});

function meta(withContent){return `CREATE TABLE chunks (
 rowid INTEGER PRIMARY KEY, source_key TEXT NOT NULL, chunk_id TEXT NOT NULL UNIQUE,
 vault_id TEXT NOT NULL, path TEXT NOT NULL, heading_path_json TEXT NOT NULL,
 frontmatter_json TEXT NOT NULL, mtime INTEGER NOT NULL, content_hash TEXT NOT NULL,
 chunking_version INTEGER NOT NULL, filename TEXT NOT NULL, stem TEXT NOT NULL,
 aliases TEXT NOT NULL, title TEXT NOT NULL, heading_text TEXT NOT NULL,
 path_text TEXT NOT NULL, tags TEXT NOT NULL${withContent?', content TEXT NOT NULL':''}, identifiers TEXT NOT NULL);
 CREATE INDEX chunks_by_source ON chunks(source_key);`;}

function build({pageSize, contentless, detail, save, optimize, vacuum, slimMeta}) {
  const db = new sqlite3.oo1.DB(':memory:');
  if (pageSize) db.exec(`PRAGMA page_size=${pageSize}`);
  db.exec(slimMeta ? `CREATE TABLE chunks (rowid INTEGER PRIMARY KEY, source_key TEXT NOT NULL, chunk_id TEXT NOT NULL UNIQUE, path TEXT NOT NULL, mtime INTEGER NOT NULL, content_hash TEXT NOT NULL);CREATE INDEX chunks_by_source ON chunks(source_key);`
                     : meta(!contentless));
  const d = detail? `, detail=${detail}`:'';
  db.exec(contentless
    ? `CREATE VIRTUAL TABLE chunks_fts USING fts5(${FTS_COLUMNS}, content='', contentless_delete=1${d}, tokenize='unicode61 remove_diacritics 2');`
    : `CREATE VIRTUAL TABLE chunks_fts USING fts5(${FTS_COLUMNS}, content='chunks', content_rowid='rowid'${d}, tokenize='unicode61 remove_diacritics 2');`);
  const insMeta = slimMeta
    ? db.prepare(`INSERT INTO chunks (source_key,chunk_id,path,mtime,content_hash) VALUES (?,?,?,?,?)`)
    : db.prepare(`INSERT INTO chunks (source_key,chunk_id,vault_id,path,heading_path_json,frontmatter_json,mtime,content_hash,chunking_version,filename,stem,aliases,title,heading_text,path_text,tags,${contentless?'':'content,'} identifiers) VALUES (${'?, '.repeat(contentless?16:17)}?)`);
  const insFts = db.prepare(`INSERT INTO chunks_fts (rowid, ${FTS_COLUMNS}) VALUES (?,?,?,?,?,?,?,?,?,?)`);
  db.exec('BEGIN IMMEDIATE');
  let rowid=0, mdBytes=0;
  for (const note of streamNotes()) for (const c of note.chunks) {
    rowid++;
    if (slimMeta) insMeta.bind([c.source_key,c.chunk_id,c.path,c.mtime,c.content_hash]).stepReset();
    else {
      const b=[c.source_key,c.chunk_id,c.vault_id,c.path,c.heading_path_json,c.frontmatter_json,c.mtime,c.content_hash,c.chunking_version,c.filename,c.stem,c.aliases,c.title,c.heading_text,c.path_text,c.tags];
      if(!contentless) b.push(c.content);
      b.push(c.identifiers); insMeta.bind(b).stepReset();
    }
    if (contentless||slimMeta) insFts.bind([rowid,c.filename,c.stem,c.aliases,c.title,c.heading_text,c.path_text,c.tags,c.content,c.identifiers]).stepReset();
  }
  insMeta.finalize(); insFts.finalize();
  db.exec('COMMIT');
  if (!contentless && !slimMeta) { /* external content triggers not used; insert fts manually */ }
  const sizes={};
  sizes.raw = sqlite3.capi.sqlite3_js_db_export(db).byteLength;
  if (optimize) { const t=performance.now(); db.exec("INSERT INTO chunks_fts(chunks_fts) VALUES('optimize')"); sizes.optimize_ms=Math.round(performance.now()-t); sizes.after_optimize=sqlite3.capi.sqlite3_js_db_export(db).byteLength; }
  if (vacuum) { const t=performance.now(); db.exec('VACUUM'); sizes.vacuum_ms=Math.round(performance.now()-t); sizes.after_vacuum=sqlite3.capi.sqlite3_js_db_export(db).byteLength; }
  if (save) { const b=sqlite3.capi.sqlite3_js_db_export(db); fs.writeFileSync(`${OUT}/${save}.db`, b); sizes.saved=`${save}.db`; }
  sizes.pageSize = db.selectValue('pragma page_size');
  sizes.pageCount = db.selectValue('pragma page_count');
  sizes.freelist = db.selectValue('pragma freelist_count');
  db.close();
  return sizes;
}

const cases = [
  {name:'A-baseline-ps8192', o:{contentless:false}},
  {name:'D-contentless-ps8192', o:{contentless:true, save:'contentless-8192'}},
  {name:'D-contentless-ps4096', o:{contentless:true, pageSize:4096}},
  {name:'D-contentless-ps16384', o:{contentless:true, pageSize:16384}},
  {name:'D-contentless-ps32768', o:{contentless:true, pageSize:32768}},
  {name:'D-contentless-ps65536', o:{contentless:true, pageSize:65536}},
  {name:'D-contentless-opt+vac', o:{contentless:true, optimize:true, vacuum:true, save:'contentless-opt'}},
  {name:'F-contentless-detail-none', o:{contentless:true, detail:'none', optimize:true, vacuum:true}},
  {name:'G-contentless-detail-col', o:{contentless:true, detail:'column', optimize:true, vacuum:true}},
  {name:'H-slimmeta-contentless', o:{contentless:true, slimMeta:true, optimize:true, vacuum:true, save:'slim'}},
  {name:'I-slimmeta-detailnone', o:{contentless:true, slimMeta:true, detail:'none', optimize:true, vacuum:true}},
];
const out=[];
for (const c of cases) { const t=performance.now(); const r=build(c.o); r.build_ms=Math.round(performance.now()-t); r.name=c.name;
  for (const k of ['raw','after_optimize','after_vacuum']) if(r[k]) r[k+'_MiB']=+(r[k]/1048576).toFixed(1);
  out.push(r); console.error(JSON.stringify(r)); }
fs.writeFileSync(`${OUT}/sizes.json`, JSON.stringify(out,null,2));
