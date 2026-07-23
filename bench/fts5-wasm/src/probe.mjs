import sqlite3InitModule from '@sqlite.org/sqlite-wasm';

export const LEXICAL_PROFILE = Object.freeze({
  id: 'lexical-v1',
  weights: Object.freeze([5.0, 6.0, 6.0, 6.0, 3.0, 1.0, 2.0, 1.0, 5.0]),
});

export const MARKERS = Object.freeze({
  start: '',
  end: '',
  ellipsis: '…',
});

export const SCHEMA_SQL = `
CREATE TABLE chunks (
  rowid INTEGER PRIMARY KEY,
  source_key TEXT NOT NULL,
  chunk_id TEXT NOT NULL UNIQUE,
  vault_id TEXT NOT NULL,
  path TEXT NOT NULL,
  heading_path_json TEXT NOT NULL,
  frontmatter_json TEXT NOT NULL,
  mtime INTEGER NOT NULL,
  content_hash TEXT NOT NULL,
  chunking_version INTEGER NOT NULL,
  filename TEXT NOT NULL,
  stem TEXT NOT NULL,
  aliases TEXT NOT NULL,
  title TEXT NOT NULL,
  heading_text TEXT NOT NULL,
  path_text TEXT NOT NULL,
  tags TEXT NOT NULL,
  content TEXT NOT NULL,
  identifiers TEXT NOT NULL
);

CREATE INDEX chunks_by_source ON chunks(source_key);

CREATE VIRTUAL TABLE chunks_fts USING fts5(
  filename,
  stem,
  aliases,
  title,
  heading_text,
  path_text,
  tags,
  content,
  identifiers,
  content='chunks',
  content_rowid='rowid',
  tokenize='unicode61 remove_diacritics 2'
);

CREATE TRIGGER chunks_ai AFTER INSERT ON chunks BEGIN
  INSERT INTO chunks_fts(
    rowid, filename, stem, aliases, title, heading_text,
    path_text, tags, content, identifiers
  ) VALUES (
    new.rowid, new.filename, new.stem, new.aliases, new.title,
    new.heading_text, new.path_text, new.tags, new.content, new.identifiers
  );
END;

CREATE TRIGGER chunks_ad AFTER DELETE ON chunks BEGIN
  INSERT INTO chunks_fts(
    chunks_fts, rowid, filename, stem, aliases, title, heading_text,
    path_text, tags, content, identifiers
  ) VALUES (
    'delete', old.rowid, old.filename, old.stem, old.aliases, old.title,
    old.heading_text, old.path_text, old.tags, old.content, old.identifiers
  );
END;

CREATE TRIGGER chunks_au AFTER UPDATE ON chunks BEGIN
  INSERT INTO chunks_fts(
    chunks_fts, rowid, filename, stem, aliases, title, heading_text,
    path_text, tags, content, identifiers
  ) VALUES (
    'delete', old.rowid, old.filename, old.stem, old.aliases, old.title,
    old.heading_text, old.path_text, old.tags, old.content, old.identifiers
  );
  INSERT INTO chunks_fts(
    rowid, filename, stem, aliases, title, heading_text,
    path_text, tags, content, identifiers
  ) VALUES (
    new.rowid, new.filename, new.stem, new.aliases, new.title,
    new.heading_text, new.path_text, new.tags, new.content, new.identifiers
  );
END;
`;

const UPSERT_SQL = `
INSERT INTO chunks (
  source_key, chunk_id, vault_id, path, heading_path_json,
  frontmatter_json, mtime, content_hash, chunking_version,
  filename, stem, aliases, title, heading_text, path_text,
  tags, content, identifiers
) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
ON CONFLICT(chunk_id) DO UPDATE SET
  source_key = excluded.source_key,
  vault_id = excluded.vault_id,
  path = excluded.path,
  heading_path_json = excluded.heading_path_json,
  frontmatter_json = excluded.frontmatter_json,
  mtime = excluded.mtime,
  content_hash = excluded.content_hash,
  chunking_version = excluded.chunking_version,
  filename = excluded.filename,
  stem = excluded.stem,
  aliases = excluded.aliases,
  title = excluded.title,
  heading_text = excluded.heading_text,
  path_text = excluded.path_text,
  tags = excluded.tags,
  content = excluded.content,
  identifiers = excluded.identifiers
`;

const SEARCH_SQL = `
SELECT
  c.chunk_id,
  c.source_key,
  c.vault_id,
  c.path,
  c.heading_path_json,
  c.frontmatter_json,
  c.mtime,
  c.content_hash,
  c.chunking_version,
  -bm25(chunks_fts, ${LEXICAL_PROFILE.weights.join(', ')}) AS score,
  snippet(chunks_fts, 7, '${MARKERS.start}', '${MARKERS.end}', '${MARKERS.ellipsis}', 24) AS snippet,
  highlight(chunks_fts, 7, '${MARKERS.start}', '${MARKERS.end}') AS highlighted_content
FROM chunks_fts
JOIN chunks AS c ON c.rowid = chunks_fts.rowid
WHERE chunks_fts MATCH ?
ORDER BY score DESC, c.chunk_id ASC, c.path ASC
LIMIT ?
`;

export class QuerySyntaxError extends Error {
  constructor(query, cause) {
    super(`invalid FTS5 query: ${query}`, { cause });
    this.name = 'QuerySyntaxError';
    this.query = query;
  }
}

export async function openDatabase() {
  const sqlite3 = await sqlite3InitModule();
  const db = new sqlite3.oo1.DB(':memory:', 'c');
  return { sqlite3, db };
}

export function createSchema(db) {
  db.exec(SCHEMA_SQL);
}

export function normalizeChunk(chunk) {
  const normalized = {
    source_key: chunk.source_key,
    chunk_id: chunk.chunk_id,
    vault_id: chunk.vault_id ?? 'vault',
    path: chunk.path,
    heading_path_json: chunk.heading_path_json ?? '[]',
    frontmatter_json: chunk.frontmatter_json ?? '{}',
    mtime: chunk.mtime ?? 0,
    content_hash: chunk.content_hash ?? `hash:${chunk.chunk_id}`,
    chunking_version: chunk.chunking_version ?? 1,
    filename: chunk.filename ?? '',
    stem: chunk.stem ?? '',
    aliases: chunk.aliases ?? '',
    title: chunk.title ?? '',
    heading_text: chunk.heading_text ?? '',
    path_text: chunk.path_text ?? chunk.path,
    tags: chunk.tags ?? '',
    content: chunk.content ?? '',
    identifiers: chunk.identifiers ?? '',
  };

  for (const key of ['source_key', 'chunk_id', 'vault_id', 'path']) {
    if (typeof normalized[key] !== 'string' || normalized[key].length === 0) {
      throw new TypeError(`${key} must be a non-empty string`);
    }
  }

  for (const [key, value] of Object.entries(normalized)) {
    if (typeof value === 'string' && (value.includes(MARKERS.start) || value.includes(MARKERS.end))) {
      throw new TypeError(`${key} contains a reserved excerpt marker`);
    }
  }

  return normalized;
}

export function upsertChunk(db, chunk) {
  const value = normalizeChunk(chunk);
  db.exec(UPSERT_SQL, {
    bind: [
      value.source_key,
      value.chunk_id,
      value.vault_id,
      value.path,
      value.heading_path_json,
      value.frontmatter_json,
      value.mtime,
      value.content_hash,
      value.chunking_version,
      value.filename,
      value.stem,
      value.aliases,
      value.title,
      value.heading_text,
      value.path_text,
      value.tags,
      value.content,
      value.identifiers,
    ],
  });
  return value;
}

export function replaceSource(db, sourceKey, chunks, options = {}) {
  if (!Array.isArray(chunks)) throw new TypeError('chunks must be an array');

  return db.transaction('IMMEDIATE', () => {
    db.exec('DELETE FROM chunks WHERE source_key = ?', { bind: [sourceKey] });
    chunks.forEach((chunk, index) => {
      upsertChunk(db, { ...chunk, source_key: sourceKey });
      if (options.failAfter === index + 1) {
        throw new Error(`injected failure after ${index + 1} inserts`);
      }
    });
  });
}

export function renameSource(db, oldSourceKey, newSourceKey, chunks, options = {}) {
  if (!Array.isArray(chunks)) throw new TypeError('chunks must be an array');

  return db.transaction('IMMEDIATE', () => {
    db.exec('DELETE FROM chunks WHERE source_key = ?', { bind: [oldSourceKey] });
    chunks.forEach((chunk, index) => {
      upsertChunk(db, { ...chunk, source_key: newSourceKey });
      if (options.failAfter === index + 1) {
        throw new Error(`injected failure after ${index + 1} inserts`);
      }
    });
  });
}

export function deleteSource(db, sourceKey) {
  db.exec('DELETE FROM chunks WHERE source_key = ?', { bind: [sourceKey] });
}

export function search(db, query, limit = 20) {
  if (typeof query !== 'string' || query.length === 0) {
    throw new QuerySyntaxError(String(query), new TypeError('query must be a non-empty string'));
  }
  if (!Number.isInteger(limit) || limit < 1 || limit > 1000) {
    throw new RangeError('limit must be an integer from 1 through 1000');
  }

  try {
    return db.selectObjects(SEARCH_SQL, [query, limit]).map((row) => ({
      ...row,
      score: Number(row.score),
    }));
  } catch (error) {
    throw new QuerySyntaxError(query, error);
  }
}

export function assertFtsIntegrity(db) {
  db.exec("INSERT INTO chunks_fts(chunks_fts, rank) VALUES('integrity-check', 1)");
}

export function countChunks(db) {
  return Number(db.selectValue('SELECT count(*) FROM chunks'));
}

export function parseMarkedText(value) {
  if (typeof value !== 'string') throw new TypeError('marked text must be a string');

  const segments = [];
  let marked = false;
  let start = 0;

  for (let index = 0; index < value.length; index += 1) {
    const character = value[index];
    if (character !== MARKERS.start && character !== MARKERS.end) continue;

    if (character === MARKERS.start) {
      if (marked) throw new Error('nested excerpt start marker');
      if (index > start) segments.push({ marked: false, text: value.slice(start, index) });
      marked = true;
      start = index + 1;
    } else {
      if (!marked) throw new Error('unpaired excerpt end marker');
      segments.push({ marked: true, text: value.slice(start, index) });
      marked = false;
      start = index + 1;
    }
  }

  if (marked) throw new Error('unpaired excerpt start marker');
  if (start < value.length) segments.push({ marked: false, text: value.slice(start) });
  return segments;
}

export function closeDatabase(db) {
  db.close();
}
