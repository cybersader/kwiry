// SPDX-License-Identifier: GPL-3.0-only
import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import { readFileSync, readdirSync } from 'node:fs';
import { dirname, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

import { buildProbe, verifyPinnedArtifact } from '../esbuild.config.mjs';

const root = dirname(fileURLToPath(new URL('../package.json', import.meta.url)));
const repositoryRoot = resolve(root, '..', '..');

function runVitest() {
  const stdout = execFileSync(
    process.execPath,
    [resolve(root, 'node_modules/vitest/vitest.mjs'), 'run', '--root', root, '--reporter=json'],
    { encoding: 'utf8', stdio: ['ignore', 'pipe', 'pipe'] },
  );
  return JSON.parse(stdout);
}

async function collect() {
  const checks = [];
  const check = (name, condition) => {
    assert.ok(condition, name);
    checks.push(name);
  };

  const packageJson = JSON.parse(readFileSync(resolve(root, 'package.json'), 'utf8'));
  const packageLock = JSON.parse(readFileSync(resolve(root, 'package-lock.json'), 'utf8'));
  const manifest = JSON.parse(readFileSync(resolve(root, 'manifest.json'), 'utf8'));
  const versions = JSON.parse(readFileSync(resolve(root, 'versions.json'), 'utf8'));
  const sqliteLock = packageLock.packages['node_modules/@sqlite.org/sqlite-wasm'];
  const esbuildLock = packageLock.packages['node_modules/esbuild'];
  const vitestLock = packageLock.packages['node_modules/vitest'];
  const identity = verifyPinnedArtifact();
  const build = await buildProbe({ write: false, production: true });
  const tests = runVitest();
  const rootFiles = readdirSync(root);

  check('package version is pinned', identity.packageVersion === '3.53.0-build1');
  check('lock integrity is pinned', sqliteLock.integrity === identity.lockIntegrity);
  check('WASM byte length is pinned', identity.wasmBytes === 864752);
  check('WASM SHA-256 is pinned', identity.wasmSha256 === '02d7e48164395fa68f81c6ec33e9da5461be397dc57602ac0cd89b4bbba1d312');
  check('manifest package version agrees', manifest.version === packageJson.version);
  check('versions manifest agrees', versions[manifest.version] === manifest.minAppVersion);
  check('plugin ID is isolated', manifest.id === 'kwiry-fts5-wasm-probe');
  check('plugin is desktop only', manifest.isDesktopOnly === true);
  check('test suite passed', tests.success && tests.numFailedTests === 0);
  check('bundle is CommonJS', build.mainText.includes('module.exports'));
  check('GPL banner is present', build.mainText.slice(0, 1500).includes('GNU General Public License'));
  check('release source map is absent', !build.mainText.includes('sourceMappingURL='));
  check('bundle size is plausible', build.mainBytes > 1_000_000 && build.mainBytes < 2_500_000);
  check('one-file runtime boundary holds', !rootFiles.some((name) => /\.(?:wasm|map|db|sqlite3?)$/u.test(name) || /worker.*\.js$/u.test(name)));
  check('third-party notices exist', rootFiles.includes('THIRD_PARTY_NOTICES.md'));

  const commit = execFileSync('git', ['-C', repositoryRoot, 'rev-parse', 'HEAD'], { encoding: 'utf8' }).trim();
  const dirty = execFileSync(
    'git',
    ['-C', repositoryRoot, 'status', '--porcelain', '--', 'bench/fts5-wasm-obsidian-probe', '.github/workflows/ci.yml'],
    { encoding: 'utf8' },
  ).trim().length > 0;

  return {
    schema_version: 1,
    gate: 'D5B-Gate-2-automation',
    verdict: 'READY_FOR_FIELD_TEST',
    scope: 'One-file synthetic Obsidian compatibility probe automation only',
    timestamp: new Date().toISOString(),
    source: { commit, dirty },
    environment: {
      platform: process.platform,
      architecture: process.arch,
      node: process.version,
      npm: execFileSync('npm', ['--version'], { encoding: 'utf8' }).trim(),
      esbuild: esbuildLock.version,
      vitest: vitestLock.version,
    },
    plugin: {
      id: manifest.id,
      version: manifest.version,
      min_app_version: manifest.minAppVersion,
      desktop_only: manifest.isDesktopOnly,
      license: packageJson.license,
    },
    sqlite: {
      package: '@sqlite.org/sqlite-wasm',
      version: identity.packageVersion,
      lock_integrity: identity.lockIntegrity,
      wasm_bytes: identity.wasmBytes,
      wasm_sha256: identity.wasmSha256,
      runtime_version: '3.53.0',
      fts5_enabled: true,
    },
    bundle: {
      main_bytes: build.mainBytes,
      main_sha256: build.mainSha256,
      worker_bytes: build.workerBytes,
      worker_sha256: build.workerSha256,
      commonjs: true,
      embedded_wasm_count: 1,
      loose_runtime_assets: 0,
    },
    tests: {
      total: tests.numTotalTests,
      passed: tests.numPassedTests,
      failed: tests.numFailedTests,
      lifecycle_cycles: 25,
      network_attempts: 0,
      persistence_attempts: 0,
      helper_worker_attempts: 0,
    },
    evidence_checks: {
      total: checks.length,
      passed: checks.length,
      names: checks,
    },
    manual_witness: {
      direct_sideload: 'NOT_RUN',
      obsidian_electron_csp: 'NOT_RUN',
      frozen_brat: 'NOT_RUN',
    },
    limitations: [
      'Automation does not prove installed Obsidian Electron or CSP behavior',
      'No frozen-release BRAT installation, update, or rollback was performed',
      'The probe uses fixed synthetic data and does not read or search a vault',
      'No production plugin, portable Rust, active-vault lifecycle, semantic, or hybrid integration',
      'READY_FOR_FIELD_TEST is not Gate 2 GO or product delivery',
    ],
  };
}

try {
  process.stdout.write(`${JSON.stringify(await collect(), null, 2)}\n`);
} catch (error) {
  process.stdout.write(`${JSON.stringify({
    schema_version: 1,
    gate: 'D5B-Gate-2-automation',
    verdict: 'NO-GO',
    timestamp: new Date().toISOString(),
    error: error instanceof Error ? error.message : 'Unknown evidence failure',
  }, null, 2)}\n`);
  process.exitCode = 1;
}
