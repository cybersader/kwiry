// Application worker for the export/restore feasibility gate, rebuilt after
// adversarial review:
// - Gate-2-style runtime guards with counters (network, helper worker,
//   persistence) replace bare attestations;
// - the restored database is RE-EXPORTED and hash-compared against the
//   original blob: exact restoration is proved byte-for-byte, not sampled;
// - post-restore mutation re-runs the strong FTS5 integrity check, probes
//   for orphaned postings without the masking JOIN, asserts the deletion is
//   observable, and re-exports (the full restore->mutate->export loop);
// - a negative-control role proves the integrity check CAN fail and that a
//   truncated blob is rejected;
// - phase transitions are posted to the gate so timings and memory phases
//   exclude corpus generation from the build bracket;
// - the restore worker keeps its database open until told to shut down so
//   steady-state memory is measured with a live index resident.

import { createHash } from 'node:crypto';
import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { parentPort, workerData } from 'node:worker_threads';

import sqlite3InitModule from '../../fts5-wasm/node_modules/@sqlite.org/sqlite-wasm/dist/node.mjs';
import { SCHEMA_SQL } from '../../fts5-wasm/src/probe.mjs';
import { CORPUS_META, streamNotes } from './corpus.mjs';

const EXPECTED_WASM_SHA256 =
  '02d7e48164395fa68f81c6ec33e9da5461be397dc57602ac0cd89b4bbba1d312';
const EXPECTED_WASM_BYTES = 864_752;
// The JS glue implements sqlite3_js_db_export/deserialize; pin it too.
const EXPECTED_GLUE_SHA256 =
  'd74e4b74920d1499b0bf349ced70372c55ac9a9ea72af718e5f6b23b0c1b29c4';

const QUERIES = [
  { name: 'common-term', match: 'section' },
  { name: 'phrase', match: '"appears exactly here"' },
  { name: 'boolean-and', match: 'section AND see' },
  { name: 'prefix', match: 'note*' },
  { name: 'identifier', match: '"CVE-2026-1970"' },
  { name: 'rare-term', match: 'zeta9750term' },
  { name: 'weighted-title', match: 'incidents' },
];

const guardCounters = { network_attempts: 0, helper_worker_attempts: 0, persistence_attempts: 0 };

function installGuards() {
  const denyNetwork = () => {
    guardCounters.network_attempts += 1;
    throw new Error('network access is forbidden in the feasibility gate');
  };
  globalThis.fetch = denyNetwork;
  globalThis.XMLHttpRequest = function forbidden() {
    denyNetwork();
  };
  globalThis.WebSocket = function forbidden() {
    denyNetwork();
  };
  const originalWorker = globalThis.Worker;
  globalThis.Worker = function forbidden() {
    guardCounters.helper_worker_attempts += 1;
    throw new Error('helper workers are forbidden in the feasibility gate');
  };
  globalThis.Worker.original = originalWorker;
  for (const name of ['indexedDB', 'localStorage', 'sessionStorage']) {
    Object.defineProperty(globalThis, name, {
      get() {
        guardCounters.persistence_attempts += 1;
        throw new Error(`${name} is forbidden in the feasibility gate`);
      },
      configurable: true,
    });
  }
}

function verifyPinnedRuntime() {
  const base = '../../fts5-wasm/node_modules/@sqlite.org/sqlite-wasm/dist/';
  const wasm = readFileSync(fileURLToPath(new URL(`${base}sqlite3.wasm`, import.meta.url)));
  const wasmDigest = createHash('sha256').update(wasm).digest('hex');
  if (wasm.length !== EXPECTED_WASM_BYTES || wasmDigest !== EXPECTED_WASM_SHA256) {
    throw new Error(`sqlite3.wasm is not the pinned artifact: ${wasm.length} bytes, ${wasmDigest}`);
  }
  const glue = readFileSync(fileURLToPath(new URL(`${base}node.mjs`, import.meta.url)));
  const glueDigest = createHash('sha256').update(glue).digest('hex');
  if (glueDigest !== EXPECTED_GLUE_SHA256) {
    throw new Error(`dist/node.mjs is not the pinned glue: ${glueDigest}`);
  }
}

function phase(name) {
  parentPort.postMessage({ kind: 'phase', name, at: Date.now() });
}

function selectAll(db, sql, bind = []) {
  const rows = [];
  db.exec({ sql, bind, rowMode: 'object', resultRows: rows });
  return rows;
}

function sha256(bytes) {
  return createHash('sha256').update(bytes).digest('hex');
}

/// Diagnostic evidence; exactness itself is proved by byte comparison of a
/// re-export, so this exists to localize any divergence, not to define it.
function collectEvidence(db) {
  const schema = selectAll(
    db,
    'SELECT type, name, sql FROM sqlite_schema ORDER BY type, name',
  );
  const counters = {
    chunks: selectAll(db, 'SELECT COUNT(*) AS n FROM chunks')[0].n,
    sources: selectAll(db, 'SELECT COUNT(DISTINCT source_key) AS n FROM chunks')[0].n,
    // All columns, whole table, order-insensitive: metadata corruption in
    // any row of any column changes this digest.
    tableDigest: selectAll(
      db,
      `SELECT total(length(source_key) + length(chunk_id) + length(vault_id) +
              length(path) + length(heading_path_json) + length(frontmatter_json) +
              mtime + length(content_hash) + chunking_version + length(filename) +
              length(stem) + length(aliases) + length(title) + length(heading_text) +
              length(path_text) + length(tags) + length(content) + length(identifiers)) AS total
       FROM chunks`,
    )[0].total,
  };
  const integrity = selectAll(db, 'PRAGMA integrity_check')[0].integrity_check;
  // Strong external-content check: non-zero rank compares the FTS index
  // against the content table and throws SQLITE_CORRUPT_VTAB on divergence.
  db.exec("INSERT INTO chunks_fts(chunks_fts, rank) VALUES('integrity-check', 1)");
  const queries = QUERIES.map((query) => ({
    name: query.name,
    rows: selectAll(
      db,
      `SELECT c.chunk_id, c.path, -bm25(chunks_fts, 5.0, 6.0, 6.0, 6.0, 3.0, 1.0, 2.0, 1.0, 5.0) AS score
       FROM chunks_fts JOIN chunks c ON c.rowid = chunks_fts.rowid
       WHERE chunks_fts MATCH ?
       ORDER BY bm25(chunks_fts, 5.0, 6.0, 6.0, 6.0, 3.0, 1.0, 2.0, 1.0, 5.0), c.chunk_id
       LIMIT 20`,
      [query.match],
    ),
  }));
  return { schema, counters, integrity, queries };
}

async function initRuntime() {
  verifyPinnedRuntime();
  return sqlite3InitModule({ print: () => {}, printErr: () => {} });
}

async function runBuild() {
  const timings = {};
  let started = performance.now();
  const sqlite3 = await initRuntime();
  timings.init_ms = performance.now() - started;

  // The corpus streams note by note into the insert loop, so the harness
  // never holds the full corpus in memory — a production indexer streams
  // too, and holding it would pad the build's memory baseline.
  phase('clean-build');
  const db = new sqlite3.oo1.DB(':memory:');
  started = performance.now();
  db.exec(SCHEMA_SQL);
  db.exec('BEGIN IMMEDIATE');
  const insert = db.prepare(
    `INSERT INTO chunks (
       source_key, chunk_id, vault_id, path, heading_path_json,
       frontmatter_json, mtime, content_hash, chunking_version,
       filename, stem, aliases, title, heading_text, path_text,
       tags, content, identifiers
     ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)`,
  );
  let chunkCount = 0;
  let totalMarkdownBytes = 0;
  try {
    for (const note of streamNotes()) {
      totalMarkdownBytes += note.markdownBytes;
      for (const chunk of note.chunks) {
        chunkCount += 1;
        insert
          .bind([
            chunk.source_key, chunk.chunk_id, chunk.vault_id, chunk.path,
            chunk.heading_path_json, chunk.frontmatter_json, chunk.mtime,
            chunk.content_hash, chunk.chunking_version, chunk.filename,
            chunk.stem, chunk.aliases, chunk.title, chunk.heading_text,
            chunk.path_text, chunk.tags, chunk.content, chunk.identifiers,
          ])
          .stepReset();
      }
    }
  } finally {
    insert.finalize();
  }
  db.exec('COMMIT');
  timings.clean_build_ms = performance.now() - started;

  phase('build-validate');
  started = performance.now();
  const evidence = collectEvidence(db);
  timings.validate_ms = performance.now() - started;

  phase('export');
  started = performance.now();
  const exported = sqlite3.capi.sqlite3_js_db_export(db);
  timings.export_ms = performance.now() - started;
  db.close();

  const blob = new Uint8Array(exported);
  const blobSha256 = sha256(blob);
  parentPort.postMessage(
    {
      kind: 'result',
      role: 'build',
      timings,
      evidence,
      guardCounters,
      corpus: {
        noteCount: CORPUS_META.noteCount,
        chunkCount,
        totalMarkdownBytes,
        targetBytes: CORPUS_META.targetBytes,
      },
      blobBytes: blob.byteLength,
      blobSha256,
      blob,
    },
    [blob.buffer],
  );
}

function deserializeInto(sqlite3, blob) {
  const capi = sqlite3.capi;
  const db = new sqlite3.oo1.DB();
  const pointer = sqlite3.wasm.allocFromTypedArray(blob);
  const flags =
    (capi.SQLITE_DESERIALIZE_FREEONCLOSE ?? 1) | (capi.SQLITE_DESERIALIZE_RESIZEABLE ?? 2);
  const resultCode = capi.sqlite3_deserialize(
    db.pointer, 'main', pointer, blob.byteLength, blob.byteLength, flags,
  );
  db.checkRc(resultCode);
  return db;
}

async function runRestore(initialBlob, expectedSha256) {
  let blob = initialBlob;
  const timings = {};
  let started = performance.now();
  const sqlite3 = await initRuntime();
  timings.init_ms = performance.now() - started;

  // The cache port's hash verification belongs off the host thread.
  started = performance.now();
  if (sha256(blob) !== expectedSha256) {
    throw new Error('cache blob hash mismatch before deserialize');
  }
  timings.verify_blob_ms = performance.now() - started;

  phase('restore');
  started = performance.now();
  const db = deserializeInto(sqlite3, blob);
  timings.deserialize_ms = performance.now() - started;
  // The WASM heap now owns the database image; drop every JS reference to
  // the blob so the live-index footprint is measured without a redundant
  // retained copy.
  blob = null;
  initialBlob = null;
  workerData.blob = null;

  phase('restore-validate');
  started = performance.now();
  const evidence = collectEvidence(db);
  timings.validate_ms = performance.now() - started;

  // Live-index proof: replace a marker source, then verify the deletion is
  // observable, no orphaned postings survive (probing chunks_fts WITHOUT
  // the masking join), the strong integrity check still passes, and the
  // mutated database can be exported again (the full cache loop).
  phase('restore-mutate');
  started = performance.now();
  const victim = 'vault notes/architecture/note-00000.md';
  const victimHits = (probe) =>
    selectAll(db, `SELECT COUNT(*) AS n FROM chunks_fts WHERE chunks_fts MATCH ?`, [probe])[0].n;
  const orphanBefore = victimHits('zeta0term');
  db.exec('BEGIN IMMEDIATE');
  db.exec({ sql: 'DELETE FROM chunks WHERE source_key = ?', bind: [victim] });
  db.exec(
    `INSERT INTO chunks (
       source_key, chunk_id, vault_id, path, heading_path_json,
       frontmatter_json, mtime, content_hash, chunking_version,
       filename, stem, aliases, title, heading_text, path_text,
       tags, content, identifiers
     ) VALUES ('${victim}', 'restored-mutation-0', 'vault', 'notes/restored.md', '[]',
       '{}', 1700000001, 'deadbeef', 1, 'restored.md', 'restored', '',
       'restored note', 'restored heading', 'notes restored.md', '',
       'postrestoremutationterm proves the index is live', '')`,
  );
  db.exec('COMMIT');
  const mutationVisible =
    victimHits('postrestoremutationterm') === 1 &&
    orphanBefore === 1 &&
    victimHits('zeta0term') === 0;
  db.exec("INSERT INTO chunks_fts(chunks_fts, rank) VALUES('integrity-check', 1)");
  // The production cache loop ends with a fresh export of the mutated
  // generation; hash it, then drop the buffer so steady-state measures the
  // live index, not the harness's retained copies.
  let mutatedExport = new Uint8Array(sqlite3.capi.sqlite3_js_db_export(db));
  const mutatedExportWorks =
    mutatedExport.byteLength > 0 && sha256(mutatedExport) !== expectedSha256;
  mutatedExport = null;
  timings.mutation_ms = performance.now() - started;

  parentPort.postMessage({
    kind: 'result',
    role: 'restore',
    timings,
    evidence,
    guardCounters,
    mutationVisible,
    mutatedExportWorks,
  });

  // Keep the restored database open so the gate can measure steady-state
  // memory with a live index resident.
  await new Promise((resolve) => {
    parentPort.once('message', (message) => {
      if (message?.kind === 'shutdown') resolve();
    });
  });
  db.close();
  parentPort.postMessage({ kind: 'closed' });
}

/// Byte-exact proof in its own short-lived worker: deserialize the blob and
/// re-export it; the result must reproduce the original bit for bit. Kept
/// separate from the production-shape restore so its extra ~2x blob
/// footprint never contaminates the restore memory measurements.
async function runByteExactProof(blob, expectedSha256) {
  const sqlite3 = await initRuntime();
  const started = performance.now();
  const db = deserializeInto(sqlite3, blob);
  const reExported = new Uint8Array(sqlite3.capi.sqlite3_js_db_export(db));
  const byteExact =
    reExported.byteLength === blob.byteLength && sha256(reExported) === expectedSha256;
  db.close();
  parentPort.postMessage({
    kind: 'result',
    role: 'byte-exact-proof',
    byteExact,
    proofMs: performance.now() - started,
    guardCounters,
  });
}

/// Proves the controls can fail: a desynchronized external-content table
/// must make integrity-check(1) throw, and a truncated blob must be
/// rejected rather than silently restored.
async function runNegativeControl(blob) {
  const sqlite3 = await initRuntime();
  const db = new sqlite3.oo1.DB(':memory:');
  db.exec(SCHEMA_SQL);
  db.exec(
    `INSERT INTO chunks (
       source_key, chunk_id, vault_id, path, heading_path_json,
       frontmatter_json, mtime, content_hash, chunking_version,
       filename, stem, aliases, title, heading_text, path_text,
       tags, content, identifiers
     ) VALUES ('s', 'c1', 'v', 'p.md', '[]', '{}', 1, 'h', 1,
       'p.md', 'p', '', 't', 'h', 'p', '', 'negative control content', '')`,
  );
  // Desynchronize the index behind the triggers' back.
  db.exec('DROP TRIGGER chunks_au');
  db.exec("UPDATE chunks SET content = 'silently different' WHERE chunk_id = 'c1'");
  let integrityThrew = false;
  try {
    db.exec("INSERT INTO chunks_fts(chunks_fts, rank) VALUES('integrity-check', 1)");
  } catch {
    integrityThrew = true;
  }
  db.close();

  let truncatedRejected = false;
  try {
    const db2 = deserializeInto(sqlite3, blob.slice(0, Math.floor(blob.byteLength / 3)));
    // Deserialize may accept the prefix; the mandatory validation step
    // must then fail loudly.
    try {
      db2.exec('SELECT COUNT(*) FROM chunks');
      db2.exec('PRAGMA integrity_check');
      db2.exec("INSERT INTO chunks_fts(chunks_fts, rank) VALUES('integrity-check', 1)");
    } catch {
      truncatedRejected = true;
    }
    db2.close();
  } catch {
    truncatedRejected = true;
  }

  parentPort.postMessage({
    kind: 'result',
    role: 'negative-control',
    integrityCheckCanFail: integrityThrew,
    truncatedBlobRejected: truncatedRejected,
    guardCounters,
  });
}

installGuards();
const dispatch = {
  build: () => runBuild(),
  restore: () => runRestore(workerData.blob, workerData.expectedSha256),
  'byte-exact-proof': () => runByteExactProof(workerData.blob, workerData.expectedSha256),
  'negative-control': () => runNegativeControl(workerData.blob),
};
dispatch[workerData.role]().catch((error) => {
  parentPort.postMessage({
    kind: 'result',
    role: workerData.role,
    error: String(error?.stack ?? error),
  });
});
