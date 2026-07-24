// SPDX-License-Identifier: GPL-3.0-only
import { readFileSync, readdirSync } from 'node:fs';
import { createRequire } from 'node:module';
import { dirname, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { beforeAll, describe, expect, test } from 'vitest';

import { buildProbe, verifyPinnedArtifact } from '../esbuild.config.mjs';

const root = dirname(fileURLToPath(new URL('../package.json', import.meta.url)));
const require = createRequire(import.meta.url);
let build;

function occurrences(value, needle) {
  let count = 0;
  let offset = 0;
  while ((offset = value.indexOf(needle, offset)) >= 0) {
    count += 1;
    offset += needle.length;
  }
  return count;
}

beforeAll(async () => {
  build = await buildProbe({ write: false, production: true });
});

describe('pinned supply chain', () => {
  test('matches the exact Gate 1 artifact', () => {
    expect(verifyPinnedArtifact()).toMatchObject({
      packageVersion: '3.53.0-build1',
      lockIntegrity: 'sha512-PfWPWN2n+/37doa8oh2/oUXk4OOsRYZsxc1W1sDXIGb/Pu5Yrb+f2eyYpgQMGITVX7HVgxhs9P18Rc6I97ym/g==',
      wasmBytes: 864752,
      wasmSha256: '02d7e48164395fa68f81c6ec33e9da5461be397dc57602ac0cd89b4bbba1d312',
    });
  });

  test('selects the browser wrapper and excludes upstream helper runtimes', () => {
    const inputs = Object.keys(build.workerMetafile.inputs).map((input) => input.replaceAll('\\', '/'));
    expect(inputs.some((input) => input.endsWith('@sqlite.org/sqlite-wasm/dist/index.mjs'))).toBe(true);
    expect(inputs.some((input) => input.endsWith('@sqlite.org/sqlite-wasm/dist/node.mjs'))).toBe(false);
    expect(inputs.some((input) => input.includes('sqlite3-worker1.mjs'))).toBe(false);
    expect(inputs.some((input) => input.includes('sqlite3-opfs-async-proxy.js'))).toBe(false);
  });
});

describe('one-file artifact', () => {
  test('embeds exactly one official WASM payload', () => {
    const wasm = readFileSync(require.resolve('@sqlite.org/sqlite-wasm/sqlite3.wasm'));
    const encoded = wasm.toString('base64');
    expect(occurrences(build.workerSource, encoded)).toBe(1);
    expect(occurrences(build.mainText, encoded)).toBe(1);
  });

  test('is CommonJS with licensing and no release source map', () => {
    expect(build.mainText.slice(0, 1_500)).toContain('GNU General Public License');
    expect(build.mainText).toContain('module.exports');
    expect(build.mainText).not.toContain('sourceMappingURL=');
    expect(build.mainBytes).toBeGreaterThan(1_000_000);
    expect(build.mainBytes).toBeLessThan(2_500_000);
  });

  test('is deterministic across clean in-memory builds', async () => {
    const second = await buildProbe({ write: false, production: true });
    expect(second.workerSha256).toBe(build.workerSha256);
    expect(second.mainSha256).toBe(build.mainSha256);
    expect(second.mainBytes).toBe(build.mainBytes);
  });

  test('has no loose runtime output beside main.js', () => {
    const forbidden = readdirSync(root).filter((name) => /\.(?:wasm|map|db|sqlite3?)$/u.test(name) || /worker.*\.js$/u.test(name));
    expect(forbidden).toEqual([]);
  });
});

describe('manifest and first-party privacy policy', () => {
  test('keeps package, manifest, and versions aligned', () => {
    const packageJson = JSON.parse(readFileSync(resolve(root, 'package.json'), 'utf8'));
    const manifest = JSON.parse(readFileSync(resolve(root, 'manifest.json'), 'utf8'));
    const versions = JSON.parse(readFileSync(resolve(root, 'versions.json'), 'utf8'));
    expect(manifest.id).toBe('kwiry-fts5-wasm-probe');
    expect(manifest.isDesktopOnly).toBe(true);
    expect(manifest.version).toBe(packageJson.version);
    expect(versions[manifest.version]).toBe(manifest.minAppVersion);
  });

  test('does not import production, vault, settings, token, filesystem, or network APIs', () => {
    const firstParty = ['main.ts', 'session.ts', 'protocol.ts', 'worker.ts']
      .map((name) => readFileSync(resolve(root, 'src', name), 'utf8'))
      .join('\n');
    for (const forbidden of [
      'clients/obsidian',
      'requestUrl',
      'app.vault',
      'metadataCache',
      'loadData(',
      'saveData(',
      'node:fs',
      'tokenPath',
      'innerHTML',
      'sqlite3Worker1Promiser',
    ]) {
      expect(firstParty).not.toContain(forbidden);
    }
    expect(readFileSync(resolve(root, 'src', 'main.ts'), 'utf8')).not.toMatch(/\bfetch\s*\(|XMLHttpRequest|WebSocket|EventSource/u);
  });
});
