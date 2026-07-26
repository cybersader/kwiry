// experiments/matrix.mjs — mechanism x image comparison runner.
//
// Every restore mechanism (experiments/mechanisms/*.mjs) is run against every
// variant image (baseline / contentless / contentless-opt / slim) in its own
// short-lived CHILD PROCESS, one at a time, so neither wasm linear-memory
// ratcheting nor host RSS from one cell can be attributed to the next. RSS is
// sampled from outside the measured process (/proc/<pid>/statm), so the
// sampler is not part of what it measures.
//
// Discipline carried over from this bench's recorded learnings:
// - every capability check must be provably fail-able. A null/absent snippet
//   is recorded NO, not "worked"; "the query did not throw" is never accepted
//   as "the capability works" — each check asserts a measured, non-vacuous
//   property (nonzero expected hit counts, marker text CONTAINED in the
//   snippet, a marker that was present becoming absent after delete, a BM25
//   ranking that actually separates documents);
// - negative controls per mechanism prove the harness can fail, across
//   corruption CLASSES (unaligned truncation, page-aligned truncation, header
//   magic flip, illegal page size, interior page damage) rather than the one
//   cheapest class, and a positive control proves the strict validation path
//   still accepts an intact image;
// - only measured numbers are reported. Anything that could not be measured
//   is null with an error string, never a plausible-looking default;
// - the run ends in a derived VERDICT over per-cell boolean checks (the
//   scripts/gate.mjs pattern), and the exit code follows the verdict — a
//   failing lossless/reopen/integrity/capability check cannot exit 0.
//
// Usage:
//   node matrix.mjs                        # every mechanism x every image
//   node matrix.mjs --images=contentless   # subset (name or file basename)
//   node matrix.mjs --only=deserialize     # subset of mechanisms
//   node matrix.mjs --queries=200 --no-compress --no-negative
//   node matrix.mjs --reverse              # reverse mechanism order
//   MATRIX_MECHANISMS_DIR=/abs/dir node matrix.mjs
//
// Output: experiments/matrix-results.json + a compact console table.

import { createHash } from 'node:crypto';
import { existsSync, readFileSync, readdirSync, statSync, writeFileSync } from 'node:fs';
import { fork, spawnSync } from 'node:child_process';
import { deflateRawSync } from 'node:zlib';
import { basename, dirname, join, resolve } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';

const SELF_PATH = fileURLToPath(import.meta.url);
const EXPERIMENTS_DIR = dirname(SELF_PATH);
// Exact pinned runtime, resolved relative to this file (experiments/).
const RUNTIME_SPECIFIER = '../../fts5-wasm/node_modules/@sqlite.org/sqlite-wasm/dist/node.mjs';
const RUNTIME_PATH = fileURLToPath(new URL(RUNTIME_SPECIFIER, import.meta.url));
const WASM_PATH = fileURLToPath(
  new URL('../../fts5-wasm/node_modules/@sqlite.org/sqlite-wasm/dist/sqlite3.wasm', import.meta.url),
);

// The same pins src/worker.mjs enforces. Recording a hash in provenance is not
// verification: numbers measured on a swapped runtime would look identical to
// good ones, so every cell asserts these BEFORE initializing the runtime.
const EXPECTED_WASM_SHA256 = '02d7e48164395fa68f81c6ec33e9da5461be397dc57602ac0cd89b4bbba1d312';
const EXPECTED_WASM_BYTES = 864_752;
const EXPECTED_GLUE_SHA256 = 'd74e4b74920d1499b0bf349ced70372c55ac9a9ea72af718e5f6b23b0c1b29c4';

// Variant images and the existing builder that emits each one. Builders are
// invoked as-is (never reimplemented) when their output file is missing.
const IMAGES = [
  { name: 'baseline', file: 'baseline.db', builder: 'build-baseline.mjs' },
  { name: 'contentless', file: 'contentless-8192.db', builder: 'build.mjs' },
  { name: 'contentless-opt', file: 'contentless-opt.db', builder: 'build.mjs' },
  { name: 'slim', file: 'slim.db', builder: 'build.mjs' },
];

const COMPRESS_BLOCK = 65536;
const MIB = 1048576;
const M = (n, d = 1) => (n === null || n === undefined ? null : +(n / MIB).toFixed(d));

// ---------------------------------------------------------------------------
// Cell protocol (runs inside a fresh child process)
// ---------------------------------------------------------------------------

function selectAll(db, sql, bind = []) {
  const rows = [];
  db.exec({ sql, bind, rowMode: 'object', resultRows: rows });
  return rows;
}

function columnsOf(db, table) {
  return selectAll(db, `PRAGMA table_info(${table})`).map((r) => r.name);
}

function errText(error) {
  return String(error?.message ?? error).replaceAll('\n', ' ').slice(0, 240);
}

function sha256(bytes) {
  return createHash('sha256').update(bytes).digest('hex');
}

/// Assert the runtime is the pinned artifact, exactly as src/worker.mjs does.
function verifyPinnedRuntime() {
  const wasmBytes = readFileSync(WASM_PATH);
  const wasmDigest = sha256(wasmBytes);
  if (wasmBytes.length !== EXPECTED_WASM_BYTES || wasmDigest !== EXPECTED_WASM_SHA256) {
    throw new Error(`sqlite3.wasm is not the pinned artifact: ${wasmBytes.length} bytes, ${wasmDigest}`);
  }
  const glueDigest = sha256(readFileSync(RUNTIME_PATH));
  if (glueDigest !== EXPECTED_GLUE_SHA256) {
    throw new Error(`dist/node.mjs is not the pinned glue: ${glueDigest}`);
  }
  return { wasm_sha256: wasmDigest, glue_sha256: glueDigest };
}

/// Runs `probe`, which must return { pass, detail }. A thrown error is a
/// recorded NO with its message — never an unexplained absence.
function capability(name, probe) {
  try {
    const out = probe();
    return { name, pass: out.pass === true, detail: out.detail ?? null, error: null };
  } catch (error) {
    return { name, pass: false, detail: null, error: errText(error) };
  }
}

/// Derive the schema shape rather than assuming it per image name, so a new
/// variant image works without editing the runner.
function schemaShape(db) {
  const ftsSql =
    db.selectValue("SELECT sql FROM sqlite_schema WHERE type='table' AND name='chunks_fts'") ?? '';
  const metaSql =
    db.selectValue("SELECT sql FROM sqlite_schema WHERE type='table' AND name='chunks'") ?? '';
  const triggers = selectAll(db, "SELECT name FROM sqlite_schema WHERE type='trigger'").map(
    (r) => r.name,
  );
  const ftsColumns = columnsOf(db, 'chunks_fts');
  const metaColumns = metaSql ? selectAll(db, 'PRAGMA table_info(chunks)') : [];
  return {
    fts_sql: ftsSql,
    contentless: /content\s*=\s*''/.test(ftsSql),
    external_content: /content\s*=\s*'chunks'/.test(ftsSql),
    contentless_delete: /contentless_delete\s*=\s*1/.test(ftsSql),
    detail: (ftsSql.match(/detail\s*=\s*([a-z]+)/) || [null, 'full'])[1],
    fts_columns: ftsColumns,
    content_column_index: ftsColumns.indexOf('content'),
    meta_columns: metaColumns.map((c) => ({
      name: c.name,
      type: String(c.type || '').toUpperCase(),
      notnull: !!c.notnull,
      pk: !!c.pk,
    })),
    triggers,
  };
}

/// Which capabilities this schema shape SHOULD support. Derived, so a cell is
/// judged against what its image can do rather than a hard-coded list, and a
/// capability that regresses on an image that used to have it still fails.
function expectedCapabilities(shape) {
  const detail = shape.detail;
  return {
    phrase: detail === 'full',
    column_filter: detail !== 'none',
    bm25_rank: true,
    // A contentless fts5 table cannot reproduce the source text, so snippet()
    // returns NULL by construction. External-content tables can.
    snippet: !shape.contentless,
    delete_then_absent: shape.external_content || shape.contentless_delete,
    insert_then_visible: true,
  };
}

function ftsDeleteCommand(db, shape, rowid) {
  // External content: FTS5 cannot recover the old terms once the content row
  // is gone, so issue the 'delete' command with the current values first.
  const cols = shape.fts_columns;
  const row = selectAll(db, `SELECT ${cols.join(', ')} FROM chunks WHERE rowid = ?`, [rowid])[0];
  if (!row) throw new Error(`no chunks row for rowid ${rowid}`);
  const placeholders = cols.map(() => '?').join(', ');
  db.exec({
    sql: `INSERT INTO chunks_fts(chunks_fts, rowid, ${cols.join(', ')}) VALUES ('delete', ?, ${placeholders})`,
    bind: [rowid, ...cols.map((c) => row[c])],
  });
  db.exec({ sql: 'DELETE FROM chunks WHERE rowid = ?', bind: [rowid] });
  return 'external-content-delete-command';
}

function deleteRow(db, shape, rowid) {
  if (shape.external_content) return ftsDeleteCommand(db, shape, rowid);
  db.exec({ sql: 'DELETE FROM chunks_fts WHERE rowid = ?', bind: [rowid] });
  return 'contentless-delete';
}

function insertRow(db, shape, rowid, markerToken) {
  const marker = `${markerToken} inserted by the matrix runner`;
  if (shape.external_content) {
    // The baseline image carries an AFTER INSERT trigger; inserting into the
    // content table is the only correct write path there.
    const cols = shape.meta_columns.filter((c) => !(c.pk && c.name === 'rowid'));
    const values = cols.map((c) => {
      if (c.name === 'content') return marker;
      if (c.name === 'chunk_id') return `matrix-probe-${rowid}`;
      if (c.type.includes('INT')) return 1;
      return `matrixprobe`;
    });
    db.exec({
      sql: `INSERT INTO chunks (rowid, ${cols.map((c) => c.name).join(', ')}) VALUES (?, ${cols
        .map(() => '?')
        .join(', ')})`,
      bind: [rowid, ...values],
    });
    return 'content-table-insert-via-trigger';
  }
  const cols = shape.fts_columns;
  db.exec({
    sql: `INSERT INTO chunks_fts(rowid, ${cols.join(', ')}) VALUES (?, ${cols
      .map(() => '?')
      .join(', ')})`,
    bind: [rowid, ...cols.map((c) => (c === 'content' ? marker : 'matrixprobe'))],
  });
  return 'contentless-fts-insert';
}

/// Run `body` inside a write transaction, rolling back if it throws. Without
/// this an aborted mutation leaves the write transaction open, and every
/// later measurement — including the export, which the block VFSes document
/// as inconsistent mid-transaction — silently observes a torn database.
function inTransaction(db, body) {
  db.exec('BEGIN IMMEDIATE');
  try {
    const out = body();
    db.exec('COMMIT');
    return out;
  } catch (error) {
    try {
      db.exec('ROLLBACK');
    } catch {
      /* the original failure is the interesting one */
    }
    throw error;
  }
}

const QUERY_TERMS = [
  'section', 'architecture', 'incidents', 'system',
  'data', 'note', 'reference', 'runbook',
];

async function initRuntime() {
  const module = await import(RUNTIME_SPECIFIER);
  return module.default({ print: () => {}, printErr: () => {} });
}

async function runStandardCell(config) {
  const { mechanismPath, imagePath, queryCount, losslessProbe } = config;
  const timings = {};
  const wasm = {};
  const runtimePins = verifyPinnedRuntime();
  let started = performance.now();
  const sqlite3 = await initRuntime();
  timings.runtime_init_ms = +(performance.now() - started).toFixed(1);
  wasm.after_init_bytes = sqlite3.wasm.memory.buffer.byteLength;
  const autocommit = (db) => sqlite3.capi.sqlite3_get_autocommit(db.pointer) !== 0;

  const mech = await import(pathToFileURL(mechanismPath).href);
  if (!mech.meta || typeof mech.open !== 'function') {
    throw new Error(`${mechanismPath} does not export the mechanism interface`);
  }

  const input = new Uint8Array(readFileSync(imagePath));
  const inputBytes = input.byteLength;
  const inputSha = sha256(input);

  started = performance.now();
  const handle = await mech.open(sqlite3, input);
  timings.open_ms = +(performance.now() - started).toFixed(1);

  // Interface conformance BEFORE anything is used, so a non-conforming module
  // cannot leak a handle or be measured through a half-present interface.
  const interfaceProblems = [];
  for (const key of ['wasmFloorProbe', 'exportBlob', 'close']) {
    if (typeof handle?.[key] !== 'function') interfaceProblems.push(`${key} is not a function`);
  }
  if (!handle?.db || typeof handle.db.exec !== 'function') {
    interfaceProblems.push('db is missing or is not an sqlite3.oo1.DB');
  }
  if (interfaceProblems.length > 0) {
    try {
      await handle?.close?.();
    } catch {
      /* the interface failure is the interesting one */
    }
    throw new Error(`mechanism handle does not conform: ${interfaceProblems.join('; ')}`);
  }

  const db = handle.db;
  wasm.after_open_bytes = handle.wasmFloorProbe();

  const shape = schemaShape(db);
  const expected = expectedCapabilities(shape);
  const rowCount = db.selectValue('SELECT count(*) FROM chunks_fts');

  // ---- capability checks (each provably fail-able) ------------------------
  const checks = [];
  checks.push(
    capability('phrase', () => {
      const n = db.selectValue(
        `SELECT count(*) FROM chunks_fts WHERE chunks_fts MATCH '"appears exactly here"'`,
      );
      return { pass: typeof n === 'number' && n > 0, detail: { hits: n } };
    }),
  );
  checks.push(
    capability('column_filter', () => {
      const n = db.selectValue(
        `SELECT count(*) FROM chunks_fts WHERE chunks_fts MATCH 'title:incidents'`,
      );
      return { pass: typeof n === 'number' && n > 0, detail: { hits: n } };
    }),
  );
  checks.push(
    capability('bm25_rank', () => {
      // An OR of three topic terms: documents differ in how many they carry,
      // so a working ranking produces more than one distinct score AND a row
      // order different from the engine's natural (rowid) order. Both are
      // gated: "returned finite scores in the order I asked SQL to sort them"
      // is not evidence that ranking works.
      const ranked = selectAll(
        db,
        `SELECT rowid AS rid, bm25(chunks_fts) AS score FROM chunks_fts
         WHERE chunks_fts MATCH 'architecture OR incidents OR runbook'
         ORDER BY bm25(chunks_fts) LIMIT 20`,
      );
      const natural = selectAll(
        db,
        `SELECT rowid AS rid FROM chunks_fts
         WHERE chunks_fts MATCH 'architecture OR incidents OR runbook' LIMIT 20`,
      ).map((r) => r.rid);
      const scores = ranked.map((r) => r.score);
      const rankedIds = ranked.map((r) => r.rid);
      const distinct = new Set(scores).size;
      const reorders =
        rankedIds.length === natural.length && rankedIds.some((id, i) => id !== natural[i]);
      return {
        pass:
          ranked.length > 0 &&
          scores.every((s) => Number.isFinite(s)) &&
          scores.every((s, i) => i === 0 || s >= scores[i - 1]) &&
          distinct > 1 &&
          reorders,
        detail: {
          rows: ranked.length,
          best: scores[0] ?? null,
          worst: scores.at(-1) ?? null,
          distinct_scores: distinct,
          reorders_vs_natural: reorders,
        },
      };
    }),
  );
  checks.push(
    capability('snippet', () => {
      if (shape.content_column_index < 0) {
        return { pass: false, detail: { reason: 'no content column in the fts table' } };
      }
      const value = db.selectValue(
        `SELECT snippet(chunks_fts, ${shape.content_column_index}, '[', ']', '…', 16)
         FROM chunks_fts WHERE chunks_fts MATCH '"appears exactly here"' LIMIT 1`,
      );
      // A null/empty snippet is a FAIL. The text must actually be there and
      // actually be marked up: strip the markers and require the phrase.
      const text = typeof value === 'string' ? value : null;
      const stripped = text === null ? '' : text.replaceAll('[', '').replaceAll(']', '');
      return {
        pass: text !== null && text.includes('[') && stripped.includes('appears exactly here'),
        detail: { snippet: text === null ? null : text.slice(0, 120) },
      };
    }),
  );

  // ---- timed ranked-query workload ---------------------------------------
  let hits = 0;
  started = performance.now();
  for (let i = 0; i < queryCount; i += 1) {
    db.exec({
      sql: `SELECT rowid FROM chunks_fts WHERE chunks_fts MATCH ?
            ORDER BY bm25(chunks_fts) LIMIT 20`,
      bind: [QUERY_TERMS[i % QUERY_TERMS.length]],
      rowMode: 'array',
      callback: () => {
        hits += 1;
      },
    });
  }
  timings.queries_ms = +(performance.now() - started).toFixed(1);
  // A zero-hit workload would make the timing meaningless; say so explicitly
  // and gate on it rather than printing a per-query time for nothing.
  const queriesNonVacuous = hits > 0 && queryCount > 0;
  const queries = {
    count: queryCount,
    hits,
    per_query_ms: queriesNonVacuous ? +(timings.queries_ms / queryCount).toFixed(3) : null,
  };

  // ---- optional lossless export probe, BEFORE any mutation ---------------
  let lossless = null;
  const autocommitBeforeLossless = autocommit(db);
  if (losslessProbe) {
    const t = performance.now();
    const dump = handle.exportBlob();
    const ms = +(performance.now() - t).toFixed(1);
    const bytes = dump.byteLength;
    const sha = sha256(dump);
    lossless = {
      export_ms: ms,
      bytes,
      byte_identical_to_input: bytes === inputBytes && sha === inputSha,
      sha256: sha,
      autocommit_at_export: autocommitBeforeLossless,
    };
  }

  // ---- mutating capability checks ----------------------------------------
  let deletePath = null;
  let insertPath = null;
  checks.push(
    capability('delete_then_absent', () => {
      const before = db.selectValue(
        `SELECT count(*) FROM chunks_fts WHERE chunks_fts MATCH 'zeta0term'`,
      );
      // If the marker was never there, the check would pass vacuously.
      if (!(before > 0)) {
        return { pass: false, detail: { before, reason: 'marker absent before delete' } };
      }
      const rowid = db.selectValue(
        `SELECT rowid FROM chunks_fts WHERE chunks_fts MATCH 'zeta0term' LIMIT 1`,
      );
      deletePath = inTransaction(db, () => deleteRow(db, shape, rowid));
      const after = db.selectValue(
        `SELECT count(*) FROM chunks_fts WHERE chunks_fts MATCH 'zeta0term'`,
      );
      return { pass: before > 0 && after === 0, detail: { before, after, rowid, via: deletePath } };
    }),
  );
  checks.push(
    capability('insert_then_visible', () => {
      const token = 'matrixprobetokenzz';
      const before = db.selectValue(`SELECT count(*) FROM chunks_fts WHERE chunks_fts MATCH ?`, [
        token,
      ]);
      if (before !== 0) return { pass: false, detail: { before, reason: 'token already present' } };
      const rowid = 9_000_001;
      insertPath = inTransaction(db, () => insertRow(db, shape, rowid, token));
      const after = db.selectValue(`SELECT count(*) FROM chunks_fts WHERE chunks_fts MATCH ?`, [
        token,
      ]);
      return { pass: before === 0 && after === 1, detail: { before, after, via: insertPath } };
    }),
  );

  // ---- integrity ----------------------------------------------------------
  const integrity = {};
  try {
    db.exec("INSERT INTO chunks_fts(chunks_fts, rank) VALUES('integrity-check', 1)");
    integrity.strong = 'ok';
  } catch (error) {
    integrity.strong = `FAIL: ${errText(error)}`;
  }
  try {
    db.exec("INSERT INTO chunks_fts(chunks_fts) VALUES('integrity-check')");
    integrity.plain = 'ok';
  } catch (error) {
    integrity.plain = `FAIL: ${errText(error)}`;
  }
  try {
    integrity.pragma = db.selectValue('PRAGMA integrity_check');
  } catch (error) {
    integrity.pragma = `FAIL: ${errText(error)}`;
  }

  wasm.after_workload_bytes = handle.wasmFloorProbe();

  // ---- export after the workload -----------------------------------------
  // An export taken inside an open write transaction is not a consistent
  // image (both block VFSes document this), so the state is recorded and
  // gated instead of assumed.
  const autocommitBeforeExport = autocommit(db);
  started = performance.now();
  const exported = handle.exportBlob();
  timings.export_ms = +(performance.now() - started).toFixed(1);
  const exportSha = sha256(exported);
  const exportInfo = {
    bytes: exported.byteLength,
    sha256: exportSha,
    // Post-mutation the image MUST differ; identical bytes would mean the
    // export did not observe the writes.
    differs_from_input: exportSha !== inputSha,
    autocommit_at_export: autocommitBeforeExport,
  };
  wasm.after_export_bytes = handle.wasmFloorProbe();
  wasm.peak_bytes = Math.max(
    wasm.after_open_bytes,
    wasm.after_workload_bytes,
    wasm.after_export_bytes,
  );

  // Optional, outside the required interface: a mechanism that holds the
  // image in a non-raw form (e.g. deflate-compressed blocks) can report what
  // it actually keeps resident. A mechanism that keeps the image inside wasm
  // cannot report that from JS, so it is derived from the wasm probes — never
  // back-filled with the raw image size, which would read as "these two store
  // the same amount".
  let storedBytes = null;
  let storedSource = null;
  let mechanismStats = null;
  if (typeof handle.stats === 'function') {
    try {
      mechanismStats = handle.stats();
    } catch (error) {
      mechanismStats = { error: errText(error) };
    }
  }
  if (typeof handle.storedBytes === 'function') {
    storedBytes = handle.storedBytes();
    storedSource = 'mechanism storedBytes()';
  } else if (mechanismStats && typeof mechanismStats.resident_bytes === 'number') {
    storedBytes = mechanismStats.resident_bytes;
    storedSource = 'mechanism stats().resident_bytes';
  } else if (mechanismStats && typeof mechanismStats.compressed_bytes === 'number') {
    storedBytes = mechanismStats.compressed_bytes;
    storedSource = 'mechanism stats().compressed_bytes';
  } else if (mech.meta.family === 'deserialize') {
    storedBytes = wasm.after_open_bytes - wasm.after_init_bytes;
    storedSource = 'measured wasm growth across open() (after_open - after_init)';
  } else {
    storedSource = 'not measurable: stats() exposed no resident/compressed bytes';
  }

  await handle.close();
  wasm.after_close_bytes = sqlite3.wasm.memory.buffer.byteLength;

  // Re-open the exported image in a second handle AFTER close(): proves the
  // export is a usable database (not just bytes of the right length) and that
  // open() is callable again in the same process once the first handle has
  // released its VFS/file registrations.
  let reopen = null;
  try {
    const second = await mech.open(sqlite3, exported);
    const n = second.db.selectValue('SELECT count(*) FROM chunks_fts');
    const markerVisible = second.db.selectValue(
      `SELECT count(*) FROM chunks_fts WHERE chunks_fts MATCH 'matrixprobetokenzz'`,
    );
    await second.close();
    reopen = { rows: n, mutation_visible: markerVisible === 1, pass: n > 0 && markerVisible === 1 };
  } catch (error) {
    reopen = { pass: false, error: errText(error) };
  }
  wasm.after_reopen_probe_bytes = sqlite3.wasm.memory.buffer.byteLength;

  // ---- derived verdict for this cell -------------------------------------
  const byName = Object.fromEntries(checks.map((c) => [c.name, c]));
  const unexpected = [];
  let capabilitiesMatch = true;
  for (const [name, want] of Object.entries(expected)) {
    const got = byName[name]?.pass === true;
    if (want && !got) capabilitiesMatch = false;
    if (!want && got) unexpected.push(name);
  }
  const cellChecks = {
    interface_conforms: true, // reaching here means it did; a failure threw above
    runtime_pinned: true, // verifyPinnedRuntime() threw otherwise
    rows_present: typeof rowCount === 'number' && rowCount > 0,
    queries_non_vacuous: queriesNonVacuous,
    capabilities_match_expectation: capabilitiesMatch,
    lossless_export_byte_identical: losslessProbe ? lossless.byte_identical_to_input === true : true,
    autocommit_before_lossless_export: autocommitBeforeLossless === true,
    autocommit_before_export: autocommitBeforeExport === true,
    export_differs_after_mutation: exportInfo.differs_from_input === true,
    reopen_exported_ok: reopen.pass === true,
    integrity_strong_ok: integrity.strong === 'ok',
    integrity_plain_ok: integrity.plain === 'ok',
    integrity_pragma_ok: integrity.pragma === 'ok',
    stored_bytes_measured: typeof storedBytes === 'number',
  };

  return {
    kind: 'standard',
    mechanism: mech.meta,
    runtime_pins: runtimePins,
    image_bytes: inputBytes,
    image_sha256: inputSha,
    stored_bytes: storedBytes,
    stored_source: storedSource,
    mechanism_stats: mechanismStats,
    row_count: rowCount,
    schema: shape,
    expected_capabilities: expected,
    unexpected_capability_passes: unexpected,
    timings,
    wasm,
    queries,
    queries_non_vacuous: queriesNonVacuous,
    checks,
    checks_derived: cellChecks,
    failed_checks: Object.entries(cellChecks)
      .filter(([, ok]) => !ok)
      .map(([name]) => name),
    pass: Object.values(cellChecks).every(Boolean),
    lossless_export_probe: lossless,
    export: exportInfo,
    reopen_exported: reopen,
    integrity,
  };
}

// --- negative controls ------------------------------------------------------

function headerPageSize(bytes) {
  const view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
  const raw = view.getUint16(16);
  return raw === 1 ? 65536 : raw;
}

/// Corruption classes. `expect`:
///   'reject' — open() MUST throw with a message naming the defect (gated);
///   'accept' — open() MUST succeed (positive control against a validation
///              path that rejects everything, which would make the rejection
///              controls vacuous);
///   'documented-limitation' — recorded, never gated: the cheap header guard
///              provably cannot see this class, which is exactly why the
///              opt-in quick validation exists.
function corruptionClasses(full) {
  const pageSize = headerPageSize(full);
  const pages = Math.floor(full.byteLength / pageSize);

  const unaligned = full.slice(0, Math.floor(full.byteLength / 3));
  const alignedPages = Math.max(1, Math.floor(pages / 3));
  const aligned = full.slice(0, alignedPages * pageSize);

  const magic = full.slice();
  magic[3] ^= 0xff;

  const badPageSize = full.slice();
  badPageSize[16] = 0x00;
  badPageSize[17] = 0x03; // 3 bytes per page: illegal, and not a power of two

  const damageStart = Math.floor(pages / 2) * pageSize;
  const damageBytes = Math.min(16 * pageSize, full.byteLength - damageStart);
  const zeroed = full.slice();
  zeroed.fill(0, damageStart, damageStart + damageBytes);
  const garbage = full.slice();
  for (let i = 0; i < damageBytes; i += 1) {
    garbage[damageStart + i] = (Math.imul(i + 1, 2654435761) >>> 8) & 0xff;
  }

  return [
    { name: 'intact_image', bytes: full, options: {}, expect: 'accept' },
    { name: 'intact_image_quick_validate', bytes: full, options: { validate: 'quick' }, expect: 'accept' },
    { name: 'unaligned_truncation', bytes: unaligned, options: {}, expect: 'reject' },
    { name: 'page_aligned_truncation', bytes: aligned, options: {}, expect: 'reject' },
    { name: 'header_magic_flip', bytes: magic, options: {}, expect: 'reject' },
    { name: 'illegal_page_size', bytes: badPageSize, options: {}, expect: 'reject' },
    {
      name: 'midfile_pages_zeroed',
      bytes: zeroed,
      options: {},
      expect: 'documented-limitation',
      note: 'right length, intact header, 16 interior pages zeroed',
    },
    {
      name: 'midfile_pages_zeroed_quick_validate',
      bytes: zeroed,
      options: { validate: 'quick' },
      expect: 'reject',
    },
    {
      name: 'midfile_pages_garbage',
      bytes: garbage,
      options: {},
      expect: 'documented-limitation',
      note: 'right length, intact header, 16 interior pages overwritten',
    },
    {
      name: 'midfile_pages_garbage_quick_validate',
      bytes: garbage,
      options: { validate: 'quick' },
      expect: 'reject',
    },
  ];
}

const REJECTION_WORDS =
  /truncat|corrupt|malformed|not a SQLite image|magic|page size|quick_check|unsupported|SQLITE_CORRUPT|NOTADB/i;

/// The interface requires open() to THROW on a corrupt/truncated blob, and to
/// be callable again afterwards in the same process. Both are measured across
/// several corruption classes, so a mechanism that only rejects the cheapest
/// one is visible rather than credited.
async function runNegativeCell(config) {
  const { mechanismPath, imagePath } = config;
  verifyPinnedRuntime();
  const sqlite3 = await initRuntime();
  const mech = await import(pathToFileURL(mechanismPath).href);
  const full = new Uint8Array(readFileSync(imagePath));

  const controls = [];
  for (const klass of corruptionClasses(full)) {
    const record = {
      name: klass.name,
      expect: klass.expect,
      note: klass.note ?? null,
      bytes: klass.bytes.byteLength,
      options: klass.options,
      open_threw: false,
      open_error: null,
      error_names_defect: null,
      accepted_rows: null,
      accepted_query_error: null,
    };
    const started = performance.now();
    try {
      const bad = await mech.open(sqlite3, klass.bytes, klass.options);
      try {
        if (!bad?.db || typeof bad.db.selectValue !== 'function') {
          // A handle without a usable db is not "validation threw"; it is a
          // non-conforming accept, and must not be credited as a rejection.
          record.accepted_query_error = 'mechanism returned a handle without a usable db';
        } else {
          record.accepted_rows = bad.db.selectValue('SELECT count(*) FROM chunks_fts');
        }
      } catch (error) {
        record.accepted_query_error = errText(error);
      }
      try {
        await bad?.close?.();
      } catch {
        /* ignore */
      }
    } catch (error) {
      record.open_threw = true;
      record.open_error = errText(error);
      record.error_names_defect = REJECTION_WORDS.test(record.open_error);
    }
    record.ms = +(performance.now() - started).toFixed(1);
    if (klass.expect === 'reject') {
      record.pass = record.open_threw === true && record.error_names_defect === true;
    } else if (klass.expect === 'accept') {
      record.pass = record.open_threw === false && typeof record.accepted_rows === 'number' && record.accepted_rows > 0;
    } else {
      record.pass = null; // documented limitation: recorded, never gated
    }
    controls.push(record);
  }

  // open() must still work after the failures, in the same process.
  let reopenRows = null;
  let reopenError = null;
  try {
    const good = await mech.open(sqlite3, full);
    reopenRows = good.db.selectValue('SELECT count(*) FROM chunks_fts');
    await good.close();
  } catch (error) {
    reopenError = errText(error);
  }

  const gated = controls.filter((c) => c.pass !== null);
  const derived = {
    all_gated_controls_pass: gated.every((c) => c.pass === true),
    reopen_after_failure_ok: typeof reopenRows === 'number' && reopenRows > 0,
  };
  return {
    kind: 'negative',
    mechanism: mech.meta,
    image_bytes: full.byteLength,
    page_size: headerPageSize(full),
    controls,
    reopen_after_failure_rows: reopenRows,
    reopen_error: reopenError,
    checks_derived: derived,
    failed_checks: Object.entries(derived)
      .filter(([, ok]) => !ok)
      .map(([name]) => name)
      .concat(gated.filter((c) => c.pass !== true).map((c) => `control:${c.name}`)),
    pass: Object.values(derived).every(Boolean),
  };
}

// Child-process cell entry point.
if (process.env.MATRIX_CELL === '1' && typeof process.send === 'function') {
  process.on('message', (config) => {
    const run = config.mode === 'negative' ? runNegativeCell : runStandardCell;
    Promise.resolve()
      .then(() => run(config))
      .then(
        (result) => process.send({ kind: 'result', result }, () => process.exit(0)),
        (error) =>
          process.send({ kind: 'result', error: String(error?.stack ?? error) }, () =>
            process.exit(0),
          ),
      );
  });
}

// ---------------------------------------------------------------------------
// Main thread
// ---------------------------------------------------------------------------

const VALUE_OPTIONS = new Set(['mechanisms', 'images', 'only', 'queries', 'out', 'timeout']);
const FLAG_OPTIONS = new Set(['no-compress', 'no-negative', 'no-lossless', 'reverse']);

function parseArgs(argv) {
  const options = {
    mechanismsDir: process.env.MATRIX_MECHANISMS_DIR || join(EXPERIMENTS_DIR, 'mechanisms'),
    images: null,
    only: null,
    queries: 200,
    compress: true,
    negative: true,
    losslessProbe: true,
    reverse: false,
    cellTimeoutMs: 600_000,
    out: join(EXPERIMENTS_DIR, 'matrix-results.json'),
  };
  for (const arg of argv) {
    if (!arg.startsWith('--')) throw new Error(`unexpected argument ${arg}`);
    const equals = arg.indexOf('=');
    const key = equals === -1 ? arg.slice(2) : arg.slice(2, equals);
    const value = equals === -1 ? null : arg.slice(equals + 1);
    if (VALUE_OPTIONS.has(key) && (value === null || value === '')) {
      throw new Error(`--${key} requires a value (--${key}=…)`);
    }
    if (FLAG_OPTIONS.has(key) && value !== null) throw new Error(`--${key} takes no value`);
    const list = () => value.split(',').map((s) => s.trim()).filter(Boolean);
    const positiveInt = () => {
      const n = Number(value);
      if (!Number.isInteger(n) || n <= 0) throw new Error(`--${key} must be a positive integer`);
      return n;
    };
    if (key === 'mechanisms') options.mechanismsDir = resolve(value);
    else if (key === 'images') options.images = list();
    else if (key === 'only') options.only = list();
    else if (key === 'queries') options.queries = positiveInt();
    else if (key === 'out') options.out = resolve(value);
    else if (key === 'timeout') options.cellTimeoutMs = positiveInt();
    else if (key === 'no-compress') options.compress = false;
    else if (key === 'no-negative') options.negative = false;
    else if (key === 'no-lossless') options.losslessProbe = false;
    else if (key === 'reverse') options.reverse = true;
    else throw new Error(`unknown option --${key}`);
  }
  return options;
}

function sha256File(path) {
  return createHash('sha256').update(readFileSync(path)).digest('hex');
}

/// Missing images are produced by the EXISTING builders (never reimplemented
/// here). build.mjs / build-baseline.mjs both write into their own directory.
function ensureImages(images) {
  const builds = [];
  const missing = images.filter((image) => !existsSync(join(EXPERIMENTS_DIR, image.file)));
  const builders = [...new Set(missing.map((image) => image.builder))];
  for (const builder of builders) {
    const wants = missing.filter((i) => i.builder === builder).map((i) => i.file);
    process.stderr.write(`[matrix] missing ${wants.join(', ')} -> node ${builder}\n`);
    const started = performance.now();
    const result = spawnSync(process.execPath, [builder], {
      cwd: EXPERIMENTS_DIR,
      stdio: 'inherit',
    });
    builds.push({
      builder,
      produced_for: wants,
      status: result.status,
      ms: Math.round(performance.now() - started),
    });
    if (result.status !== 0) {
      throw new Error(`${builder} exited with status ${result.status}`);
    }
  }
  for (const image of images) {
    const path = join(EXPERIMENTS_DIR, image.file);
    if (!existsSync(path)) throw new Error(`${image.builder} did not produce ${image.file}`);
  }
  return builds;
}

function compressedBytes(path) {
  const bytes = new Uint8Array(readFileSync(path));
  let total = 0;
  for (let offset = 0; offset < bytes.byteLength; offset += COMPRESS_BLOCK) {
    total += deflateRawSync(
      bytes.subarray(offset, Math.min(offset + COMPRESS_BLOCK, bytes.byteLength)),
      { level: 6 },
    ).byteLength;
  }
  return total;
}

/// RSS of ANOTHER process, read from outside it. Sampling the process you are
/// measuring (as an in-process sampler thread does) both contaminates the
/// number and, run once for a whole matrix, carries each cell's high-water
/// mark into the next cell's floor.
const PAGE_BYTES = 4096;
function readRss(pid) {
  try {
    const statm = readFileSync(`/proc/${pid}/statm`, 'utf8').split(' ');
    return Number(statm[1]) * PAGE_BYTES;
  } catch {
    return null;
  }
}

function runCell(config, timeoutMs) {
  return new Promise((resolve) => {
    const child = fork(SELF_PATH, [], {
      env: { ...process.env, MATRIX_CELL: '1' },
      stdio: ['ignore', 'inherit', 'pipe', 'ipc'],
    });
    const rss = { floor: null, peak: null, samples: 0, source: `/proc/${child.pid}/statm` };
    let stderrText = '';
    child.stderr?.on('data', (chunk) => {
      if (stderrText.length < 8192) stderrText += chunk.toString();
    });
    const sample = () => {
      const value = readRss(child.pid);
      if (value === null) return;
      rss.samples += 1;
      if (rss.floor === null || value < rss.floor) rss.floor = value;
      if (rss.peak === null || value > rss.peak) rss.peak = value;
    };
    sample();
    const sampler = setInterval(sample, 10);
    const startedAt = Date.now();
    let settled = false;
    const finish = (payload) => {
      if (settled) return;
      settled = true;
      clearInterval(sampler);
      clearTimeout(timer);
      try {
        child.kill('SIGKILL');
      } catch {
        /* already gone */
      }
      resolve({
        ...payload,
        wall_ms: Date.now() - startedAt,
        rss:
          rss.samples > 0
            ? { ...rss, added: rss.peak - rss.floor }
            : { ...rss, added: null, unavailable: 'could not read /proc/<pid>/statm' },
        stderr_tail: stderrText ? stderrText.slice(-2000) : null,
      });
    };
    const timer = setTimeout(
      () => finish({ result: null, error: `cell timed out after ${timeoutMs} ms` }),
      timeoutMs,
    );
    child.on('message', (message) => {
      if (message?.kind !== 'result') return;
      sample();
      if (message.error) finish({ result: null, error: message.error });
      else finish({ result: message.result, error: null });
    });
    child.on('error', (error) => finish({ result: null, error: String(error?.stack ?? error) }));
    child.on('exit', (code, signal) => {
      if (!settled) {
        finish({ result: null, error: `cell process exited with code ${code} signal ${signal}` });
      }
    });
    child.send(config);
  });
}

function capFlags(result) {
  const short = {
    phrase: 'phrase',
    column_filter: 'col',
    bm25_rank: 'bm25',
    snippet: 'snip',
    delete_then_absent: 'del',
    insert_then_visible: 'ins',
  };
  return result.checks
    .map((c) => {
      const want = result.expected_capabilities?.[c.name];
      // "!" marks a result that disagrees with what this schema shape should
      // support: an unexpected N is a regression, an unexpected Y a surprise.
      const mark = want === undefined || want === c.pass ? '' : '!';
      return `${short[c.name] ?? c.name}:${c.pass ? 'Y' : 'N'}${mark}`;
    })
    .join(' ');
}

/// Pre-import each candidate: only modules that export `meta.family` are
/// mechanisms. Without this every helper file dropped in the directory becomes
/// a phantom all-null row.
async function loadMechanisms(dir, only) {
  const skipped = [];
  const selected = [];
  const names = readdirSync(dir).filter((name) => name.endsWith('.mjs')).sort();
  for (const name of names) {
    const path = join(dir, name);
    if (name.startsWith('_')) {
      skipped.push({ path, reason: 'underscore-prefixed helper module' });
      continue;
    }
    let module = null;
    try {
      module = await import(pathToFileURL(path).href);
    } catch (error) {
      skipped.push({ path, reason: `import failed: ${errText(error)}` });
      continue;
    }
    if (!module.meta || typeof module.meta.family !== 'string' || typeof module.open !== 'function') {
      skipped.push({ path, reason: 'does not export meta.family + open(): not a mechanism' });
      continue;
    }
    selected.push({ path, name: module.meta.name ?? basename(name, '.mjs'), family: module.meta.family });
  }
  const filtered = only
    ? selected.filter((m) => {
        const file = basename(m.path);
        return only.some(
          (want) => file === want || file === `${want}.mjs` || m.name === want || file.includes(want),
        );
      })
    : selected;
  return { mechanisms: filtered, skipped };
}

async function main() {
  const options = parseArgs(process.argv.slice(2));
  const startedAt = new Date().toISOString();

  if (!existsSync(options.mechanismsDir) || !statSync(options.mechanismsDir).isDirectory()) {
    throw new Error(
      `no mechanisms directory at ${options.mechanismsDir} (create experiments/mechanisms/ or pass --mechanisms=<dir>)`,
    );
  }
  if (!existsSync(WASM_PATH)) throw new Error(`pinned runtime wasm missing at ${WASM_PATH}`);
  if (!existsSync(RUNTIME_PATH)) throw new Error(`pinned runtime glue missing at ${RUNTIME_PATH}`);

  const { mechanisms, skipped } = await loadMechanisms(options.mechanismsDir, options.only);
  if (mechanisms.length === 0) throw new Error('no mechanism modules selected');
  if (options.reverse) mechanisms.reverse();
  for (const entry of skipped) {
    process.stderr.write(`[matrix] skipping ${basename(entry.path)}: ${entry.reason}\n`);
  }

  let images = IMAGES;
  if (options.images) {
    images = IMAGES.filter((image) =>
      options.images.some((want) => image.name === want || image.file === want),
    );
    if (images.length === 0) throw new Error(`no images matched ${options.images.join(',')}`);
  }

  const builds = ensureImages(images);
  const imageInfo = images.map((image) => {
    const path = join(EXPERIMENTS_DIR, image.file);
    const bytes = statSync(path).size;
    return {
      ...image,
      path,
      bytes,
      MiB: M(bytes),
      deflate64k_bytes: options.compress ? compressedBytes(path) : null,
    };
  });
  for (const image of imageInfo) {
    image.deflate64k_MiB = image.deflate64k_bytes === null ? null : M(image.deflate64k_bytes);
  }

  const cells = [];
  const negatives = [];

  for (const mechanism of mechanisms) {
    for (const image of imageInfo) {
      const cell = await runCell(
        {
          mode: 'standard',
          mechanismPath: mechanism.path,
          imagePath: image.path,
          queryCount: options.queries,
          losslessProbe: options.losslessProbe,
        },
        options.cellTimeoutMs,
      );
      cells.push({
        mechanism: mechanism.name,
        mechanism_file: mechanism.path,
        image: image.name,
        image_file: image.file,
        ...cell,
      });
      const state = cell.error ? 'ERROR' : cell.result.pass ? 'pass' : `FAIL ${cell.result.failed_checks.join(',')}`;
      process.stderr.write(
        `[matrix] ${mechanism.name} x ${image.name}: ${state} (${cell.wall_ms} ms)\n`,
      );
    }
    if (options.negative) {
      const image = imageInfo.reduce((a, b) => (a.bytes <= b.bytes ? a : b));
      const cell = await runCell(
        { mode: 'negative', mechanismPath: mechanism.path, imagePath: image.path },
        options.cellTimeoutMs,
      );
      negatives.push({
        mechanism: mechanism.name,
        mechanism_file: mechanism.path,
        image: image.name,
        ...cell,
      });
      const state = cell.error
        ? 'ERROR'
        : cell.result.pass
          ? 'all gated controls reject/accept as required'
          : `FAIL ${cell.result.failed_checks.join(',')}`;
      process.stderr.write(`[matrix] negative-control ${mechanism.name}: ${state}\n`);
    }
  }

  const table = cells.map((cell) => {
    const r = cell.result;
    const image = imageInfo.find((i) => i.name === cell.image);
    return {
      mechanism: cell.mechanism,
      image: cell.image,
      image_MiB: image.MiB,
      // What the mechanism actually keeps resident: JS bytes for the block
      // VFSes, measured wasm growth for deserialize. Null when unmeasurable —
      // never the raw image size as a stand-in.
      stored_MiB: r ? M(r.stored_bytes) : null,
      deflate64k_MiB: image.deflate64k_MiB,
      open_ms: r?.timings?.open_ms ?? null,
      queries_ms: r?.timings?.queries_ms ?? null,
      wasm_peak_MiB: r ? M(r.wasm.peak_bytes) : null,
      wasm_export_MiB: r ? M(r.wasm.after_export_bytes) : null,
      export_ms: r?.timings?.export_ms ?? null,
      rss_peak_MiB: M(cell.rss?.peak ?? null),
      rss_added_MiB: M(cell.rss?.added ?? null),
      caps: r ? capFlags(r) : `ERROR: ${String(cell.error).slice(0, 50)}`,
      verdict: r ? (r.pass ? 'PASS' : `FAIL: ${r.failed_checks.join(',')}`) : 'ERROR',
    };
  });

  // One row per (mechanism, corruption class): the measured outcome and the
  // measured message, so "rejects corrupt blobs" is never a single word.
  const negativeTable = [];
  for (const n of negatives) {
    if (n.error) {
      negativeTable.push({
        mechanism: n.mechanism,
        control: '(cell errored)',
        expect: null,
        outcome: 'ERROR',
        gated: null,
        detail: String(n.error).slice(0, 70),
      });
      continue;
    }
    for (const c of n.result.controls) {
      negativeTable.push({
        mechanism: n.mechanism,
        control: c.name,
        expect: c.expect,
        outcome: c.open_threw ? 'open() threw' : `opened, rows=${c.accepted_rows}`,
        gated: c.pass === null ? 'not gated' : c.pass ? 'PASS' : 'FAIL',
        detail: c.open_threw ? c.open_error.slice(0, 70) : (c.note ?? ''),
      });
    }
    negativeTable.push({
      mechanism: n.mechanism,
      control: 'reopen_after_failures',
      expect: 'accept',
      outcome: `rows=${n.result.reopen_after_failure_rows}`,
      gated: n.result.checks_derived.reopen_after_failure_ok ? 'PASS' : 'FAIL',
      detail: n.result.reopen_error ?? '',
    });
  }

  const cellFailures = cells.filter((c) => c.error || !c.result.pass).length;
  const negativeFailures = negatives.filter((n) => n.error || !n.result.pass).length;
  const verdict = cellFailures === 0 && negativeFailures === 0 ? 'PASS' : 'FAIL';

  const report = {
    runner: 'experiments/matrix.mjs',
    verdict,
    verdict_basis: {
      cells_total: cells.length,
      cells_failed: cellFailures,
      negative_controls_total: negatives.length,
      negative_controls_failed: negativeFailures,
      failing: [
        ...cells
          .filter((c) => c.error || !c.result.pass)
          .map((c) => `${c.mechanism} x ${c.image}: ${c.error ? 'ERROR' : c.result.failed_checks.join(',')}`),
        ...negatives
          .filter((n) => n.error || !n.result.pass)
          .map((n) => `negative ${n.mechanism}: ${n.error ? 'ERROR' : n.result.failed_checks.join(',')}`),
      ],
    },
    provenance: {
      started_at: startedAt,
      finished_at: new Date().toISOString(),
      node: process.version,
      platform: `${process.platform}-${process.arch}`,
      runtime_glue_sha256: sha256File(RUNTIME_PATH),
      runtime_wasm_sha256: sha256File(WASM_PATH),
      runtime_pins_asserted_in_every_cell: {
        wasm: EXPECTED_WASM_SHA256,
        wasm_bytes: EXPECTED_WASM_BYTES,
        glue: EXPECTED_GLUE_SHA256,
      },
      mechanism_order: mechanisms.map((m) => m.name),
      harness_sha256: {
        'experiments/matrix.mjs': sha256File(SELF_PATH),
        'src/corpus.mjs': sha256File(join(EXPERIMENTS_DIR, '..', 'src', 'corpus.mjs')),
        'experiments/build.mjs': sha256File(join(EXPERIMENTS_DIR, 'build.mjs')),
        'experiments/build-baseline.mjs': sha256File(join(EXPERIMENTS_DIR, 'build-baseline.mjs')),
        ...Object.fromEntries(
          readdirSync(options.mechanismsDir)
            .filter((name) => name.endsWith('.mjs'))
            .sort()
            .map((name) => [`mechanisms/${name}`, sha256File(join(options.mechanismsDir, name))]),
        ),
      },
    },
    options: {
      mechanisms_dir: options.mechanismsDir,
      query_count: options.queries,
      lossless_probe: options.losslessProbe,
      negative_control: options.negative,
      compressed_sizes_measured: options.compress,
      reversed_mechanism_order: options.reverse,
    },
    skipped_modules: skipped,
    notes: [
      'One cell = one mechanism x one image in its own child process, killed before the next cell, so neither wasm growth nor host RSS leaks across cells.',
      'Host RSS is sampled every 10 ms from OUTSIDE the measured process (/proc/<pid>/statm); rss_added is peak minus the floor of that same process.',
      'stored_MiB is what the mechanism keeps resident: stats().resident_bytes / compressed_bytes for the block VFSes, measured wasm growth across open() for the deserialize family, null when neither is available. deflate64k_MiB is what a 64 KiB-block deflate cache format would cost for that image, measured here, independent of any mechanism.',
      'wasm_peak_MiB is max(after_open, after_workload, after_export) — the export step is where sqlite3_js_db_export makes its second full in-wasm copy, so a floor taken before it understates the deserialize family.',
      'A capability flag is Y only when a measured, non-vacuous property held (nonzero expected hits; snippet text CONTAINED; a present marker became absent; BM25 scores that both separate documents and reorder them). "!" marks a result that disagrees with what the schema shape should support.',
      'The negative controls gate several corruption classes. Interior page damage in a right-length image is NOT detectable by the O(1) header guard and is recorded as a documented limitation, not a pass; the gated proof for that class is the opt-in { validate: "quick" } path.',
    ],
    builds,
    images: imageInfo,
    table,
    negative_table: negativeTable,
    cells,
    negative_controls: negatives,
  };

  // The evidence file is tracked: machine-absolute paths must never land
  // in it. Rewrite every repo-rooted path to a repo-relative one.
  const repoRoot = resolve(EXPERIMENTS_DIR, '../../..');
  const sanitized = JSON.parse(
    JSON.stringify(report).replaceAll(`${repoRoot}/`, ''),
  );
  writeFileSync(options.out, `${JSON.stringify(sanitized, null, 2)}\n`);
  console.table(table);
  if (negativeTable.length > 0) console.table(negativeTable);
  console.log(`verdict: ${verdict}`);
  for (const line of report.verdict_basis.failing) console.log(`  failing: ${line}`);
  console.error(`[matrix] wrote ${options.out}`);
  process.exit(verdict === 'PASS' ? 0 : 1);
}

if (process.env.MATRIX_CELL !== '1') {
  await main();
}
