import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import { readFileSync, statSync } from 'node:fs';
import { createRequire } from 'node:module';
import { fileURLToPath } from 'node:url';

import {
  assertFtsIntegrity,
  closeDatabase,
  createSchema,
  openDatabase,
  replaceSource,
  search,
} from '../src/probe.mjs';

const EXPECTED_WASM_BYTES = 864_752;
const EXPECTED_WASM_SHA256 = '02d7e48164395fa68f81c6ec33e9da5461be397dc57602ac0cd89b4bbba1d312';
const EXPECTED_LOCK_INTEGRITY = 'sha512-PfWPWN2n+/37doa8oh2/oUXk4OOsRYZsxc1W1sDXIGb/Pu5Yrb+f2eyYpgQMGITVX7HVgxhs9P18Rc6I97ym/g==';
const require = createRequire(import.meta.url);
const root = fileURLToPath(new URL('..', import.meta.url));
const packageJsonPath = `${root}/node_modules/@sqlite.org/sqlite-wasm/package.json`;
const packageLockPath = `${root}/package-lock.json`;
const vitestPath = `${root}/node_modules/vitest/vitest.mjs`;
const wasmPath = require.resolve('@sqlite.org/sqlite-wasm/sqlite3.wasm');

function fixture(overrides = {}) {
  const chunkId = overrides.chunk_id ?? 'evidence-chunk';
  const path = overrides.path ?? `notes/${chunkId}.md`;
  return {
    source_key: overrides.source_key ?? path,
    chunk_id: chunkId,
    vault_id: 'evidence-vault',
    path,
    heading_path_json: '["Evidence"]',
    frontmatter_json: '{}',
    mtime: 0,
    content_hash: `hash:${chunkId}`,
    chunking_version: 1,
    filename: `${chunkId}.md`,
    stem: chunkId,
    aliases: '',
    title: '',
    heading_text: 'Evidence',
    path_text: path,
    tags: '',
    content: '',
    identifiers: '',
    ...overrides,
  };
}

function runTests() {
  const stdout = execFileSync(
    process.execPath,
    [vitestPath, 'run', '--root', root, '--reporter=json'],
    { encoding: 'utf8', stdio: ['ignore', 'pipe', 'pipe'] },
  );
  return JSON.parse(stdout);
}

async function collectEvidence() {
  const packageJson = JSON.parse(readFileSync(packageJsonPath, 'utf8'));
  const packageLock = JSON.parse(readFileSync(packageLockPath, 'utf8'));
  const lockEntry = packageLock.packages['node_modules/@sqlite.org/sqlite-wasm'];
  const vitestEntry = packageLock.packages['node_modules/vitest'];
  const wasm = readFileSync(wasmPath);
  const wasmBytes = statSync(wasmPath).size;
  const wasmSha256 = createHash('sha256').update(wasm).digest('hex');
  const tests = runTests();
  const checks = [];
  const check = (name, condition) => {
    assert.ok(condition, name);
    checks.push(name);
  };

  check('official package version', packageJson.version === '3.53.0-build1');
  check('lockfile package version', lockEntry.version === '3.53.0-build1');
  check('lockfile Vitest version', vitestEntry.version === '3.2.7');
  check('lockfile package integrity', lockEntry.integrity === EXPECTED_LOCK_INTEGRITY);
  check('official WASM byte length', wasmBytes === EXPECTED_WASM_BYTES);
  check('official WASM SHA-256', wasmSha256 === EXPECTED_WASM_SHA256);
  check('Vitest suite verdict', tests.success && tests.numFailedTests === 0);

  const { db } = await openDatabase();
  const sqliteVersion = db.selectValue('SELECT sqlite_version()');
  const fts5Enabled = Number(db.selectValue("SELECT sqlite_compileoption_used('ENABLE_FTS5')"));

  try {
    check('SQLite runtime version', sqliteVersion === '3.53.0');
    check('FTS5 compile option', fts5Enabled === 1);
    check('in-memory database', db.filename === ':memory:');

    createSchema(db);
    replaceSource(db, 'evidence-source', [
      fixture({ chunk_id: 'title-hit', title: 'Evidence quasar', content: 'plain text' }),
      fixture({ chunk_id: 'body-hit', content: 'quasar' }),
    ]);
    const ranked = search(db, 'quasar');
    check('weighted BM25 ranking', ranked.length === 2 && ranked[0].chunk_id === 'title-hit');

    replaceSource(db, 'evidence-source', [fixture({ chunk_id: 'replacement', content: 'replacementterm' })]);
    check('transactional source replacement', search(db, 'quasar').length === 0 && search(db, 'replacementterm').length === 1);
    assertFtsIntegrity(db);
    checks.push('external-content integrity');
  } finally {
    closeDatabase(db);
  }

  check('clean close', db.pointer === undefined);

  return {
    timestamp: new Date().toISOString(),
    verdict: 'GO',
    scope: 'Gate 1 standalone official SQLite FTS5-WASM runtime only',
    platform: process.platform,
    architecture: process.arch,
    node_version: process.version,
    npm_version: execFileSync('npm', ['--version'], { encoding: 'utf8' }).trim(),
    package: {
      name: '@sqlite.org/sqlite-wasm',
      version: packageJson.version,
      license: packageJson.license,
      lock_integrity: lockEntry.integrity,
    },
    sqlite: {
      version: sqliteVersion,
      fts5_compile_option: fts5Enabled,
    },
    wasm: {
      bytes: wasmBytes,
      sha256: wasmSha256,
    },
    tests: {
      total: tests.numTotalTests,
      passed: tests.numPassedTests,
      failed: tests.numFailedTests,
      verdict: tests.success ? 'pass' : 'fail',
    },
    evidence_checks: {
      total: checks.length,
      passed: checks.length,
      names: checks,
    },
    limitations: [
      'No Obsidian, Electron, CSP, or BRAT compatibility proof',
      'No Worker or one-file production packaging proof',
      'No OPFS or persistent index',
      'No portable Rust or production plugin integration',
      'No active-vault indexing or real-vault performance measurement',
      'No semantic or hybrid mode',
    ],
  };
}

try {
  const evidence = await collectEvidence();
  process.stdout.write(`${JSON.stringify(evidence, null, 2)}\n`);
} catch (error) {
  process.stdout.write(`${JSON.stringify({
    timestamp: new Date().toISOString(),
    verdict: 'NO-GO',
    scope: 'Gate 1 standalone official SQLite FTS5-WASM runtime only',
    error: error instanceof Error ? error.message : String(error),
  }, null, 2)}\n`);
  process.exitCode = 1;
}
