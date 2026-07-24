import { createHash } from 'node:crypto';
import { readFileSync, statSync } from 'node:fs';
import { createRequire } from 'node:module';
import { fileURLToPath } from 'node:url';
import { afterEach, beforeEach, describe, expect, test } from 'vitest';

import {
  LEXICAL_PROFILE,
  MARKERS,
  QuerySyntaxError,
  assertFtsIntegrity,
  closeDatabase,
  countChunks,
  createSchema,
  deleteSource,
  openDatabase,
  parseMarkedText,
  renameSource,
  replaceSource,
  search,
  upsertChunk,
} from '../src/probe.mjs';

const EXPECTED_WASM_BYTES = 864_752;
const EXPECTED_WASM_SHA256 = '02d7e48164395fa68f81c6ec33e9da5461be397dc57602ac0cd89b4bbba1d312';
const EXPECTED_LOCK_INTEGRITY = 'sha512-PfWPWN2n+/37doa8oh2/oUXk4OOsRYZsxc1W1sDXIGb/Pu5Yrb+f2eyYpgQMGITVX7HVgxhs9P18Rc6I97ym/g==';
const require = createRequire(import.meta.url);
const PACKAGE_ROOT = fileURLToPath(new URL('..', import.meta.url));
const PACKAGE_JSON = `${PACKAGE_ROOT}/node_modules/@sqlite.org/sqlite-wasm/package.json`;
const PACKAGE_LOCK = `${PACKAGE_ROOT}/package-lock.json`;
const WASM_PATH = require.resolve('@sqlite.org/sqlite-wasm/sqlite3.wasm');

function chunk(overrides = {}) {
  const id = overrides.chunk_id ?? 'chunk-1';
  const path = overrides.path ?? `notes/${id}.md`;
  return {
    source_key: overrides.source_key ?? path,
    chunk_id: id,
    vault_id: 'vault',
    path,
    heading_path_json: '["Heading"]',
    frontmatter_json: '{}',
    mtime: 123,
    content_hash: `hash:${id}`,
    chunking_version: 1,
    filename: `${id}.md`,
    stem: id,
    aliases: '',
    title: '',
    heading_text: 'Heading',
    path_text: path,
    tags: '',
    content: '',
    identifiers: '',
    ...overrides,
  };
}

function ids(rows) {
  return rows.map((row) => row.chunk_id);
}

describe('official SQLite WASM capability', () => {
  let db;

  beforeEach(async () => {
    ({ db } = await openDatabase());
  });

  afterEach(() => {
    if (db?.pointer) closeDatabase(db);
  });

  test('pins the exact package, lock integrity, and WASM artifact', () => {
    const packageJson = JSON.parse(readFileSync(PACKAGE_JSON, 'utf8'));
    const packageLock = JSON.parse(readFileSync(PACKAGE_LOCK, 'utf8'));
    const lockEntry = packageLock.packages['node_modules/@sqlite.org/sqlite-wasm'];
    const vitestEntry = packageLock.packages['node_modules/vitest'];
    const wasm = readFileSync(WASM_PATH);

    expect(packageJson.version).toBe('3.53.0-build1');
    expect(lockEntry.version).toBe('3.53.0-build1');
    expect(vitestEntry.version).toBe('3.2.7');
    expect(lockEntry.integrity).toBe(EXPECTED_LOCK_INTEGRITY);
    expect(statSync(WASM_PATH).size).toBe(EXPECTED_WASM_BYTES);
    expect(createHash('sha256').update(wasm).digest('hex')).toBe(EXPECTED_WASM_SHA256);
  });

  test('reports SQLite 3.53.0 with FTS5 enabled', () => {
    expect(db.selectValue('SELECT sqlite_version()')).toBe('3.53.0');
    expect(db.selectValue("SELECT sqlite_compileoption_used('ENABLE_FTS5')")).toBe(1);
  });

  test('creates the external-content schema and synchronized triggers', () => {
    createSchema(db);
    upsertChunk(db, chunk({ content: 'phosphorescent' }));

    expect(db.selectValue("SELECT count(*) FROM sqlite_master WHERE type='trigger' AND name LIKE 'chunks_a%'")).toBe(3);
    expect(ids(search(db, 'phosphorescent'))).toEqual(['chunk-1']);
    expect(() => assertFtsIntegrity(db)).not.toThrow();
  });

  test('uses an in-memory database and fails explicitly after close', () => {
    expect(db.filename).toBe(':memory:');
    closeDatabase(db);
    expect(db.pointer).toBeUndefined();
    expect(() => db.selectValue('SELECT 1')).toThrow();
    db = undefined;
  });
});

describe('FTS5 query behavior', () => {
  let db;

  beforeEach(async () => {
    ({ db } = await openDatabase());
    createSchema(db);
    [
      chunk({ chunk_id: 'dragon-guide', title: 'Red Dragon Guide', content: 'ancient scales and fire' }),
      chunk({ chunk_id: 'dragon-body', content: 'a red dragon guards the northern gate' }),
      chunk({ chunk_id: 'griffin', title: 'Griffin Manual', content: 'winged sentinel' }),
      chunk({ chunk_id: 'mixed', content: 'alpha beta gamma' }),
      chunk({ chunk_id: 'alpha-only', content: 'alpha delta' }),
      chunk({ chunk_id: 'beta-only', content: 'beta delta' }),
    ].forEach((value) => upsertChunk(db, value));
  });

  afterEach(() => closeDatabase(db));

  test('supports ordinary terms, phrases, prefixes, and Boolean operators', () => {
    expect(ids(search(db, 'dragon'))).toEqual(expect.arrayContaining(['dragon-guide', 'dragon-body']));
    expect(ids(search(db, '"red dragon"'))).toEqual(expect.arrayContaining(['dragon-guide', 'dragon-body']));
    expect(ids(search(db, 'grif*'))).toEqual(['griffin']);
    expect(ids(search(db, 'alpha AND beta'))).toEqual(['mixed']);
    expect(ids(search(db, 'alpha OR beta'))).toEqual(expect.arrayContaining(['mixed', 'alpha-only', 'beta-only']));
    expect(ids(search(db, 'alpha NOT beta'))).toEqual(['alpha-only']);
  });

  test('supports allowlisted column syntax', () => {
    expect(ids(search(db, 'title:griffin'))).toEqual(['griffin']);
    expect(search(db, 'content:griffin')).toEqual([]);
  });

  test('returns an explicit query error for malformed FTS syntax', () => {
    expect(() => search(db, '"unterminated')).toThrow(QuerySyntaxError);
  });

  test('binds query text instead of executing it as SQL', () => {
    expect(() => search(db, "alpha'; DROP TABLE chunks; --")).toThrow(QuerySyntaxError);
    expect(db.selectValue("SELECT count(*) FROM sqlite_master WHERE type='table' AND name='chunks'")).toBe(1);
    expect(countChunks(db)).toBe(6);
  });
});

describe('ranking and inert excerpts', () => {
  let db;

  beforeEach(async () => {
    ({ db } = await openDatabase());
    createSchema(db);
  });

  afterEach(() => closeDatabase(db));

  test('uses the fixed lexical-v1 field weights', () => {
    expect(LEXICAL_PROFILE).toEqual({
      id: 'lexical-v1',
      weights: [5, 6, 6, 6, 3, 1, 2, 1, 5],
    });

    upsertChunk(db, chunk({ chunk_id: 'body', filename: 'body.md', stem: 'body', content: 'quasar' }));
    upsertChunk(db, chunk({ chunk_id: 'filename', filename: 'quasar.md', stem: 'other', content: 'plain body' }));
    upsertChunk(db, chunk({ chunk_id: 'title', title: 'Quasar', content: 'plain body' }));
    upsertChunk(db, chunk({ chunk_id: 'alias', aliases: 'quasar', content: 'plain body' }));

    const rows = search(db, 'quasar');
    const byId = Object.fromEntries(rows.map((row) => [row.chunk_id, row.score]));
    expect(byId.filename).toBeGreaterThan(byId.body);
    expect(byId.title).toBeGreaterThan(byId.body);
    expect(byId.alias).toBeGreaterThan(byId.body);
  });

  test('returns finite scores and deterministic ordering', () => {
    upsertChunk(db, chunk({ chunk_id: 'b', path: 'notes/b.md', filename: 'same.md', stem: 'same', content: 'equal' }));
    upsertChunk(db, chunk({ chunk_id: 'a', path: 'notes/a.md', filename: 'same.md', stem: 'same', content: 'equal' }));

    const first = search(db, 'equal');
    const second = search(db, 'equal');
    expect(ids(first)).toEqual(['a', 'b']);
    expect(second).toEqual(first);
    expect(first.every((row) => Number.isFinite(row.score))).toBe(true);
  });

  test('uses inert markers while preserving markup-looking note text', () => {
    upsertChunk(db, chunk({
      chunk_id: 'hostile',
      content: '<script>alert("alpha")</script> **alpha**',
    }));

    const [row] = search(db, 'alpha');
    expect(row.highlighted_content).toContain('<script>');
    expect(row.highlighted_content).toContain(`${MARKERS.start}alpha${MARKERS.end}`);
    expect(parseMarkedText(row.highlighted_content)).toContainEqual({ marked: true, text: 'alpha' });
    expect(parseMarkedText(row.snippet).some((segment) => segment.marked)).toBe(true);
  });

  test('rejects reserved or malformed marker sequences', () => {
    expect(() => upsertChunk(db, chunk({ content: `collision ${MARKERS.start}` }))).toThrow(/reserved excerpt marker/);
    expect(() => parseMarkedText(`${MARKERS.start}open`)).toThrow(/unpaired excerpt start/);
    expect(() => parseMarkedText(`${MARKERS.end}close`)).toThrow(/unpaired excerpt end/);
    expect(() => parseMarkedText(`${MARKERS.start}nested${MARKERS.start}x${MARKERS.end}`)).toThrow(/nested/);
  });
});

describe('transactional source lifecycle', () => {
  let db;

  beforeEach(async () => {
    ({ db } = await openDatabase());
    createSchema(db);
  });

  afterEach(() => closeDatabase(db));

  test('inserts and upserts through the relational content table', () => {
    upsertChunk(db, chunk({ chunk_id: 'upsert', content: 'before' }));
    upsertChunk(db, chunk({ chunk_id: 'upsert', content: 'after' }));

    expect(search(db, 'before')).toEqual([]);
    expect(ids(search(db, 'after'))).toEqual(['upsert']);
    expect(countChunks(db)).toBe(1);
    expect(() => assertFtsIntegrity(db)).not.toThrow();
  });

  test('replaces every old source chunk atomically and idempotently', () => {
    replaceSource(db, 'source-a', [
      chunk({ chunk_id: 'old-a', content: 'obsolete' }),
      chunk({ chunk_id: 'old-b', content: 'obsolete' }),
    ]);
    const replacement = [chunk({ chunk_id: 'new-a', content: 'current' })];

    replaceSource(db, 'source-a', replacement);
    replaceSource(db, 'source-a', replacement);

    expect(search(db, 'obsolete')).toEqual([]);
    expect(ids(search(db, 'current'))).toEqual(['new-a']);
    expect(countChunks(db)).toBe(1);
    expect(() => assertFtsIntegrity(db)).not.toThrow();
  });

  test('deletes relational and searchable state', () => {
    replaceSource(db, 'source-a', [chunk({ chunk_id: 'delete-me', content: 'vanishing' })]);
    deleteSource(db, 'source-a');

    expect(countChunks(db)).toBe(0);
    expect(search(db, 'vanishing')).toEqual([]);
    expect(() => assertFtsIntegrity(db)).not.toThrow();
  });

  test('renames by atomically removing old path IDs and adding new path IDs', () => {
    replaceSource(db, 'notes/old.md', [chunk({
      chunk_id: 'old-path-id',
      path: 'notes/old.md',
      content: 'renamable',
    })]);

    renameSource(db, 'notes/old.md', 'notes/new.md', [chunk({
      chunk_id: 'new-path-id',
      path: 'notes/new.md',
      content: 'renamable',
    })]);

    const rows = search(db, 'renamable');
    expect(ids(rows)).toEqual(['new-path-id']);
    expect(rows[0].source_key).toBe('notes/new.md');
    expect(rows[0].path).toBe('notes/new.md');
    expect(() => assertFtsIntegrity(db)).not.toThrow();
  });

  test('rolls back relational and FTS state after an injected replacement failure', () => {
    replaceSource(db, 'source-a', [chunk({ chunk_id: 'stable', content: 'stableterm' })]);

    expect(() => replaceSource(db, 'source-a', [
      chunk({ chunk_id: 'partial-a', content: 'partialterm' }),
      chunk({ chunk_id: 'partial-b', content: 'partialterm' }),
    ], { failAfter: 1 })).toThrow(/injected failure/);

    expect(ids(search(db, 'stableterm'))).toEqual(['stable']);
    expect(search(db, 'partialterm')).toEqual([]);
    expect(countChunks(db)).toBe(1);
    expect(() => assertFtsIntegrity(db)).not.toThrow();
  });
});

describe('repeated lifecycle', () => {
  test('repeats create, query, close without persistent side effects', async () => {
    for (let cycle = 0; cycle < 5; cycle += 1) {
      const { db } = await openDatabase();
      expect(db.filename).toBe(':memory:');
      createSchema(db);
      upsertChunk(db, chunk({ chunk_id: `cycle-${cycle}`, content: 'repeated' }));
      expect(ids(search(db, 'repeated'))).toEqual([`cycle-${cycle}`]);
      closeDatabase(db);
      expect(() => db.selectValue('SELECT 1')).toThrow();
    }
  });
});
