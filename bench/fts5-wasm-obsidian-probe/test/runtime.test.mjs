// SPDX-License-Identifier: GPL-3.0-only
import { readFileSync } from 'node:fs';
import { createRequire } from 'node:module';
import { Worker } from 'node:worker_threads';
import { beforeAll, describe, expect, test } from 'vitest';

import { buildProbe } from '../esbuild.config.mjs';

const require = createRequire(import.meta.url);
let workerSource;

beforeAll(async () => {
  ({ workerSource } = await buildProbe({ write: false, production: true }));
});

function adapter(source) {
  return `
    const { parentPort } = require('node:worker_threads');
    globalThis.self = globalThis;
    globalThis.postMessage = (message) => parentPort.postMessage(message);
    globalThis.addEventListener = (type, listener) => {
      if (type === 'message') parentPort.on('message', (data) => listener({ data }));
    };
    globalThis.close = () => process.exit(0);
    ${source}
  `;
}

function request(worker, id, operation, timeoutMs = 15_000) {
  return new Promise((resolve, reject) => {
    const timeout = setTimeout(() => {
      cleanup();
      reject(new Error(`worker request timed out: ${operation}`));
    }, timeoutMs);
    const onMessage = (message) => {
      if (message?.id !== id) return;
      cleanup();
      resolve(message);
    };
    const onError = (error) => {
      cleanup();
      reject(error);
    };
    const cleanup = () => {
      clearTimeout(timeout);
      worker.off('message', onMessage);
      worker.off('error', onError);
    };
    worker.on('message', onMessage);
    worker.on('error', onError);
    worker.postMessage({ version: 1, id, operation });
  });
}

async function runCycle(source = workerSource) {
  const worker = new Worker(adapter(source), { eval: true });
  try {
    const initialized = await request(worker, 1, 'initialize');
    expect(initialized).toMatchObject({
      ok: true,
      operation: 'initialize',
      result: {
        sqliteVersion: '3.53.0',
        fts5Enabled: 1,
        wasmBytes: 864752,
        database: ':memory:',
        networkAttempts: 0,
        persistenceAttempts: 0,
        helperWorkerAttempts: 0,
      },
    });

    const probed = await request(worker, 2, 'probe');
    expect(probed).toMatchObject({
      ok: true,
      operation: 'probe',
      result: {
        expectedTitle: 'Synthetic Alpha',
        finiteScore: true,
        snippetMarked: true,
        rollbackAbsent: true,
        integrityPassed: true,
      },
    });

    const disposed = await request(worker, 3, 'dispose');
    expect(disposed).toMatchObject({ ok: true, operation: 'dispose', result: { closed: true } });
    await new Promise((resolve) => worker.once('exit', resolve));
    return { initialized, probed, disposed };
  } finally {
    await worker.terminate();
  }
}

describe('exact generated Worker runtime', () => {
  test('initializes embedded SQLite, runs FTS5, rolls back, and closes', async () => {
    await runCycle();
  }, 30_000);

  test('rejects a corrupted embedded WASM payload before SQLite initialization', async () => {
    const wasm = readFileSync(require.resolve('@sqlite.org/sqlite-wasm/sqlite3.wasm'));
    const encoded = wasm.toString('base64');
    const replacement = `${encoded[0] === 'A' ? 'B' : 'A'}${encoded.slice(1)}`;
    const corrupted = workerSource.replace(encoded, replacement);
    expect(corrupted).not.toBe(workerSource);

    const worker = new Worker(adapter(corrupted), { eval: true });
    try {
      await expect(request(worker, 1, 'initialize')).resolves.toMatchObject({
        ok: false,
        error: { code: 'artifact_mismatch', stage: 'artifact' },
      });
    } finally {
      await worker.terminate();
    }
  }, 30_000);

  test('serializes initialize and dispose requests posted back-to-back', async () => {
    const worker = new Worker(adapter(workerSource), { eval: true });
    const responses = [];
    worker.on('message', (message) => responses.push(message));
    worker.postMessage({ version: 1, id: 1, operation: 'initialize' });
    worker.postMessage({ version: 1, id: 2, operation: 'dispose' });
    await new Promise((resolve, reject) => {
      const timeout = setTimeout(() => reject(new Error('serialized lifecycle timed out')), 15_000);
      worker.on('message', () => {
        if (responses.length === 2) {
          clearTimeout(timeout);
          resolve();
        }
      });
    });
    expect(responses).toMatchObject([
      { id: 1, ok: true, operation: 'initialize' },
      { id: 2, ok: true, operation: 'dispose', result: { closed: true } },
    ]);
    await worker.terminate();
  }, 30_000);

  test('completes 25 fresh lifecycle cycles without retained Workers', async () => {
    for (let cycle = 0; cycle < 25; cycle += 1) {
      await runCycle();
    }
  }, 120_000);
});
