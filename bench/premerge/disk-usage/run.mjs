#!/usr/bin/env node

// Native derived-state disk-usage harness. This intentionally drives the
// shipped kwiry CLI/daemon instead of linking private lifecycle internals, so
// the measurement covers the real lock, storage probe, copy-aside, Tantivy,
// durable publication, pointer replacement, retention, and watcher paths.

import {
  closeSync,
  existsSync,
  fsyncSync,
  lstatSync,
  mkdirSync,
  openSync,
  readFileSync,
  readdirSync,
  readlinkSync,
  renameSync,
  rmSync,
  statSync,
  unlinkSync,
  writeFileSync,
  writeSync,
} from 'node:fs';
import { createHash, randomBytes } from 'node:crypto';
import { execFileSync, spawn } from 'node:child_process';
import { dirname, extname, join, relative, resolve, sep } from 'node:path';
import { fileURLToPath } from 'node:url';
import { performance } from 'node:perf_hooks';
import process from 'node:process';
import os from 'node:os';

import { streamNotes } from '../../fts5-export-restore/src/corpus.mjs';

const SCRIPT_DIR = dirname(fileURLToPath(import.meta.url));
const REPO_ROOT = resolve(SCRIPT_DIR, '../../..');
const DAEMON_DIR = join(REPO_ROOT, 'daemon');
const EVIDENCE_PATH = join(SCRIPT_DIR, 'evidence', 'latest.json');
const DEFAULT_INTERVAL_MS = 10;
const CALIBRATION_BYTES = 64 * 1024 * 1024;
const ACTIVE_CHILDREN = new Set();
const DATA_ROOTS = new Map();
let MEASUREMENT_FILESYSTEM = null;
const sleep = (ms) => new Promise((resolvePromise) => setTimeout(resolvePromise, ms));

function signalChildGroup(child, signal) {
  if (!child || child.exitCode !== null) return;
  try {
    if (process.platform === 'win32') child.kill(signal);
    else process.kill(-child.pid, signal);
  } catch {
    try {
      child.kill(signal);
    } catch {
      // The process already exited.
    }
  }
}

function stopAllChildGroups(signal) {
  for (const child of ACTIVE_CHILDREN) signalChildGroup(child, signal);
}

for (const signal of ['SIGINT', 'SIGTERM']) {
  process.on(signal, () => {
    stopAllChildGroups(signal);
    setTimeout(() => process.exit(signal === 'SIGINT' ? 130 : 143), 100);
  });
}

function parseArgs(argv) {
  const options = {
    suite: 'core',
    repeats: 1,
    intervalMs: DEFAULT_INTERVAL_MS,
    keepWork: false,
    binary: null,
    output: EVIDENCE_PATH,
    includeRenameDelete: false,
  };
  for (let index = 0; index < argv.length; index += 1) {
    const arg = argv[index];
    if (arg === '--suite') options.suite = argv[++index];
    else if (arg === '--repeats') options.repeats = Number.parseInt(argv[++index], 10);
    else if (arg === '--interval-ms') options.intervalMs = Number.parseInt(argv[++index], 10);
    else if (arg === '--binary') options.binary = resolve(argv[++index]);
    else if (arg === '--output') options.output = resolve(argv[++index]);
    else if (arg === '--keep-work') options.keepWork = true;
    else if (arg === '--include-rename-delete') options.includeRenameDelete = true;
    else if (arg === '--help' || arg === '-h') {
      console.log(`Usage: node bench/premerge/disk-usage/run.mjs [options]\n\n` +
        `  --suite controls|smoke|core|full  Scenario set (default: core)\n` +
        `  --repeats N                       Clean-build repetitions (default: 1)\n` +
        `  --interval-ms N                   Delay after each complete scan (default: 10)\n` +
        `  --binary PATH                     Use an existing kwiry binary\n` +
        `  --output PATH                     JSON evidence path\n` +
        `  --keep-work                       Preserve temporary corpus/data roots\n` +
        `  --include-rename-delete           Run the bounded 2,500 rename/delete stress case\n\n` +
        `core runs controls, fixture smoke, Desktop clean builds at 100/1,000/10,000\n` +
        `notes, saturated rebuild, one reconcile, no-op proof, and repeated growth.\n` +
        `full additionally runs OpenClast partition builds. The rename/delete stress case is\n` +
        `opt-in because a measured run exposed a product-side reconciliation stall.`);
      process.exit(0);
    } else {
      throw new Error(`unknown argument: ${arg}`);
    }
  }
  if (!['controls', 'smoke', 'core', 'full'].includes(options.suite)) {
    throw new Error(`invalid --suite: ${options.suite}`);
  }
  if (!Number.isInteger(options.repeats) || options.repeats < 1 || options.repeats > 10) {
    throw new Error('--repeats must be an integer from 1 to 10');
  }
  if (!Number.isInteger(options.intervalMs) || options.intervalMs < 0 || options.intervalMs > 1000) {
    throw new Error('--interval-ms must be an integer from 0 to 1000');
  }
  return options;
}

function commandText(command, args) {
  return [command, ...args].map((part) => JSON.stringify(part)).join(' ');
}

function run(command, args, options = {}) {
  const result = execFileSync(command, args, {
    cwd: options.cwd,
    encoding: 'utf8',
    stdio: options.stdio ?? ['ignore', 'pipe', 'pipe'],
    env: { ...process.env, ...(options.env ?? {}) },
  });
  return typeof result === 'string' ? result.trim() : '';
}

function filesystemIdentity(path) {
  const [type, id, transferBlockSize, fundamentalBlockSize] = run(
    'stat',
    ['-f', '-c', '%T|%i|%s|%S', path],
  ).split('|');
  return {
    type,
    id,
    transferBlockSize: Number.parseInt(transferBlockSize, 10),
    fundamentalBlockSize: Number.parseInt(fundamentalBlockSize, 10),
  };
}

function sameFilesystem(left, right) {
  return left.type === right.type &&
    left.id === right.id &&
    left.fundamentalBlockSize === right.fundamentalBlockSize;
}

function registerDataRoot(path, label) {
  const root = resolve(path);
  if (!MEASUREMENT_FILESYSTEM) throw new Error('measurement filesystem was not initialized');
  mkdirSync(root, { recursive: true });
  if (DATA_ROOTS.has(root)) throw new Error(`data root registered twice: ${label}`);
  const markerName = `.kwiry-disk-harness-root-${randomBytes(16).toString('hex')}`;
  writeFileSync(join(root, markerName), '');
  const filesystem = filesystemIdentity(root);
  if (!sameFilesystem(filesystem, MEASUREMENT_FILESYSTEM)) {
    throw new Error(
      `data root ${label} is on ${filesystem.type}/${filesystem.id}, not measurement filesystem ` +
      `${MEASUREMENT_FILESYSTEM.type}/${MEASUREMENT_FILESYSTEM.id}`,
    );
  }
  const registration = { label, markerName, filesystem };
  DATA_ROOTS.set(root, registration);
  return registration;
}

function verifyDataRootMarker(path, registration) {
  const root = resolve(path);
  const markerPath = join(root, registration.markerName);
  if (!existsSync(markerPath) || !lstatSync(markerPath).isFile()) {
    throw new Error(`data-root sentinel mismatch for ${registration.label}: ${registration.markerName}`);
  }
  const currentFilesystem = filesystemIdentity(root);
  if (!sameFilesystem(currentFilesystem, registration.filesystem)) {
    throw new Error(
      `data-root filesystem changed for ${registration.label}: ` +
      `${registration.filesystem.type}/${registration.filesystem.id} -> ` +
      `${currentFilesystem.type}/${currentFilesystem.id}`,
    );
  }
  return currentFilesystem;
}

function assertSuccessfulDataRoot(root, steady) {
  const registration = DATA_ROOTS.get(resolve(root));
  if (!registration) throw new Error(`unregistered data root measured: ${root}`);
  const filesystem = verifyDataRootMarker(root, registration);
  if (!existsSync(join(root, 'current.json'))) {
    throw new Error(`successful scenario has no current.json: ${registration.label}`);
  }
  if (!steady.activeGeneration) {
    throw new Error(`successful scenario has no active generation: ${registration.label}`);
  }
  const activePath = join(root, 'generations', steady.activeGeneration);
  if (!existsSync(activePath) || !lstatSync(activePath).isDirectory()) {
    throw new Error(
      `successful scenario is missing generations/${steady.activeGeneration}: ${registration.label}`,
    );
  }
  if (!steady.generations[steady.activeGeneration]) {
    throw new Error(
      `scanner did not inventory active generation ${steady.activeGeneration}: ${registration.label}`,
    );
  }
  return {
    label: registration.label,
    sentinelVerified: true,
    currentJsonVerified: true,
    activeGenerationVerified: steady.activeGeneration,
    filesystem,
  };
}

function sha256File(path) {
  return createHash('sha256').update(readFileSync(path)).digest('hex');
}

function sha256Text(text) {
  return createHash('sha256').update(text).digest('hex');
}

function fsyncFile(path) {
  const fd = openSync(path, 'r');
  try {
    fsyncSync(fd);
  } finally {
    closeSync(fd);
  }
}

function writeDurably(path, contents) {
  mkdirSync(dirname(path), { recursive: true });
  writeFileSync(path, contents);
  fsyncFile(path);
}

function reconstructMarkdown(note) {
  const first = note.chunks[0];
  const frontmatter = JSON.parse(first.frontmatter_json);
  let markdown = `---\ntitle: ${frontmatter.title}\ntags: [${frontmatter.tags.join(', ')}]\n`;
  if (first.aliases) markdown += `aliases: [${first.aliases}]\n`;
  markdown += '---\n\n';
  for (const chunk of note.chunks) {
    markdown += `## ${chunk.heading_text}\n\n${chunk.content}\n\n`;
  }
  return markdown;
}

function materializeCorpus(root, noteCount, partitions = 1) {
  rmSync(root, { recursive: true, force: true });
  mkdirSync(root, { recursive: true });
  const partitionRoots = Array.from({ length: partitions }, (_, index) => {
    const path = partitions === 1 ? root : join(root, `partition-${String(index).padStart(2, '0')}`);
    mkdirSync(path, { recursive: true });
    return path;
  });
  let files = 0;
  let chunks = 0;
  let bytes = 0;
  for (const note of streamNotes()) {
    if (files >= noteCount) break;
    const markdown = reconstructMarkdown(note);
    const measured = Buffer.byteLength(markdown, 'utf8');
    if (measured !== note.markdownBytes) {
      throw new Error(
        `corpus materialization mismatch at note ${files}: reconstructed=${measured}, emitted=${note.markdownBytes}`,
      );
    }
    const path = join(partitionRoots[files % partitions], ...note.chunks[0].path.split('/'));
    mkdirSync(dirname(path), { recursive: true });
    writeFileSync(path, markdown);
    files += 1;
    chunks += note.chunks.length;
    bytes += measured;
  }
  if (files !== noteCount) throw new Error(`requested ${noteCount} notes, materialized ${files}`);
  return { files, chunks, apparentBytes: bytes, partitions, roots: partitionRoots };
}

function copyFixtureCorpus(source, destination) {
  rmSync(destination, { recursive: true, force: true });
  mkdirSync(destination, { recursive: true });
  let files = 0;
  let bytes = 0;
  const stack = [[source, destination]];
  while (stack.length > 0) {
    const [from, to] = stack.pop();
    for (const entry of readdirSync(from, { withFileTypes: true })) {
      const sourcePath = join(from, entry.name);
      const destinationPath = join(to, entry.name);
      if (entry.isDirectory()) {
        mkdirSync(destinationPath, { recursive: true });
        stack.push([sourcePath, destinationPath]);
      } else if (entry.isFile()) {
        const contents = readFileSync(sourcePath);
        writeFileSync(destinationPath, contents);
        files += 1;
        bytes += contents.length;
      }
    }
  }
  return { files, chunks: null, apparentBytes: bytes, partitions: 1, roots: [destination] };
}

function tomlQuote(value) {
  return JSON.stringify(value);
}

function writeDesktopConfig(path, vaultRoot) {
  writeDurably(path, `version = 1\n\n[server]\nprofile = "desktop"\nbind = "127.0.0.1:0"\n\n` +
    `[[vaults]]\nvault_id = "fixture"\npath = ${tomlQuote(vaultRoot)}\n`);
}

function writeOpenClastConfig(path, vaultRoots, jwksPath) {
  let text = `version = 1\n\n[server]\nprofile = "openclast"\nbind = "127.0.0.1:0"\n\n` +
    `[auth.openclast]\ntenant_id = "disk-harness"\nissuer = "disk-harness"\n` +
    `audience = "kwiry-search"\njwks_file = ${tomlQuote(jwksPath)}\nmax_token_ttl_seconds = 60\n`;
  for (let index = 0; index < vaultRoots.length; index += 1) {
    text += `\n[[vaults]]\nvault_id = "partition-${String(index).padStart(2, '0')}"\n` +
      `path = ${tomlQuote(vaultRoots[index])}\nroom = "room-${String(index).padStart(2, '0')}"\n`;
  }
  writeDurably(path, text);
  writeDurably(
    jwksPath,
    '{"keys":[{"kty":"OKP","crv":"Ed25519","x":"2-Jj2UvNCvQiUPNYRgSi0cJSPiJI6Rs6D0UTeEpQVj8","use":"sig","key_ops":["verify"],"alg":"EdDSA","kid":"disk01"}]}',
  );
}

function readCurrentGeneration(dataRoot) {
  const path = join(dataRoot, 'current.json');
  if (!existsSync(path)) return null;
  try {
    return JSON.parse(readFileSync(path, 'utf8')).generation ?? null;
  } catch {
    return null;
  }
}

function duAllocated(path) {
  if (!existsSync(path)) return 0;
  const output = run('du', ['--block-size=1', '--summarize', '--one-file-system', path]);
  return Number.parseInt(output.split(/\s+/)[0], 10);
}

function classify(relativePath) {
  const parts = relativePath.split(sep);
  if (parts[0] === 'generations' && parts.length >= 2) {
    const generation = parts[1];
    return {
      category: generation.startsWith('.staging-') ? 'staging' : 'generation',
      generation,
      subcategory: parts[2] === 'index' ? 'index' : parts[2] === 'partitions' ? 'partitions' : 'generation-state',
    };
  }
  if (parts[0] === 'semantic') return { category: 'semantic', generation: null, subcategory: 'semantic' };
  if (parts[0] === 'models') return { category: 'models', generation: null, subcategory: 'models' };
  if (parts[0] === 'logs') return { category: 'logs', generation: null, subcategory: 'logs' };
  return { category: 'operational', generation: null, subcategory: 'operational' };
}

function addSize(target, stat, regular) {
  target.allocatedBytes += Number(stat.blocks * 512n);
  if (regular) target.apparentBytes += Number(stat.size);
}

function scanTree(root, options = {}) {
  const started = performance.now();
  const result = {
    monotonicMs: started,
    phase: options.phase ?? 'unknown',
    apparentBytes: 0,
    allocatedBytes: 0,
    duAllocatedBytes: 0,
    duDeltaBytes: 0,
    files: 0,
    directories: 0,
    symlinks: 0,
    hardLinkedEntries: 0,
    sparseFiles: 0,
    apparentMinusAllocatedBytes: 0,
    generationCount: 0,
    stagingCount: 0,
    races: 0,
    complete: true,
    scanDurationMs: 0,
    activeGeneration: readCurrentGeneration(root),
    generations: {},
    categories: {},
    extensions: {},
    deletedOpenFiles: [],
    inventory: options.inventory ? [] : undefined,
  };
  const seenGenerationNames = new Set();
  if (!existsSync(root)) mkdirSync(root, { recursive: true });
  const stack = [['', root]];
  while (stack.length > 0) {
    const [relativePath, absolutePath] = stack.pop();
    if (options.omitRelative && options.omitRelative(relativePath)) continue;
    let stat;
    try {
      stat = lstatSync(absolutePath, { bigint: true });
    } catch (error) {
      if (error?.code === 'ENOENT') {
        result.races += 1;
        continue;
      }
      throw error;
    }
    const regular = stat.isFile();
    const allocated = Number(stat.blocks * 512n);
    if (regular) {
      result.files += 1;
      result.apparentBytes += Number(stat.size);
      if (allocated < Number(stat.size)) result.sparseFiles += 1;
      if (stat.nlink > 1n) result.hardLinkedEntries += 1;
      const extension = extname(absolutePath) || '<none>';
      const extensionSize = result.extensions[extension] ?? { apparentBytes: 0, allocatedBytes: 0, files: 0 };
      addSize(extensionSize, stat, true);
      extensionSize.files += 1;
      result.extensions[extension] = extensionSize;
    } else if (stat.isDirectory()) {
      result.directories += 1;
    } else if (stat.isSymbolicLink()) {
      result.symlinks += 1;
    }
    result.allocatedBytes += allocated;
    const classified = classify(relativePath);
    const categorySize = result.categories[classified.category] ?? { apparentBytes: 0, allocatedBytes: 0 };
    addSize(categorySize, stat, regular);
    result.categories[classified.category] = categorySize;
    if (classified.generation) {
      seenGenerationNames.add(classified.generation);
      const generationSize = result.generations[classified.generation] ?? {
        apparentBytes: 0,
        allocatedBytes: 0,
        files: 0,
        directories: 0,
        segmentFiles: 0,
      };
      addSize(generationSize, stat, regular);
      if (regular) {
        generationSize.files += 1;
        if (['.idx', '.store', '.term', '.pos', '.fast', '.fieldnorm', '.del'].includes(extname(absolutePath))) {
          generationSize.segmentFiles += 1;
        }
      } else if (stat.isDirectory()) generationSize.directories += 1;
      result.generations[classified.generation] = generationSize;
    }
    if (options.inventory) {
      result.inventory.push({
        path: relativePath || '.',
        kind: regular ? 'file' : stat.isDirectory() ? 'directory' : stat.isSymbolicLink() ? 'symlink' : 'other',
        apparentBytes: regular ? Number(stat.size) : 0,
        allocatedBytes: allocated,
        links: Number(stat.nlink),
      });
    }
    if (stat.isDirectory()) {
      let entries;
      try {
        entries = readdirSync(absolutePath);
      } catch (error) {
        if (error?.code === 'ENOENT') {
          result.races += 1;
          continue;
        }
        throw error;
      }
      for (const entry of entries) {
        stack.push([relativePath ? join(relativePath, entry) : entry, join(absolutePath, entry)]);
      }
    }
  }
  result.generationCount = [...seenGenerationNames].filter((name) => !name.startsWith('.staging-')).length;
  result.stagingCount = [...seenGenerationNames].filter((name) => name.startsWith('.staging-')).length;
  result.apparentMinusAllocatedBytes = result.apparentBytes - result.allocatedBytes;
  try {
    result.duAllocatedBytes = duAllocated(root);
    result.duDeltaBytes = result.allocatedBytes - result.duAllocatedBytes;
  } catch {
    result.races += 1;
  }
  result.complete = result.races === 0 && result.duDeltaBytes === 0;
  result.scanDurationMs = performance.now() - started;
  return result;
}

function inferPhase(sample) {
  if (sample.stagingCount > 0) return 'staging-copy-build-or-merge';
  if (sample.generationCount >= 4) return 'published-pre-prune';
  if (sample.generationCount >= 3) return 'retained-three';
  if (sample.generationCount > 0) return 'published';
  return 'empty-or-probing';
}

function inspectDeletedOpenFiles(pid) {
  if (process.platform !== 'linux' || !pid) return [];
  const fdRoot = `/proc/${pid}/fd`;
  if (!existsSync(fdRoot)) return [];
  const deleted = [];
  let descriptorNames;
  try {
    descriptorNames = readdirSync(fdRoot);
  } catch {
    return deleted;
  }
  for (const name of descriptorNames) {
    try {
      const target = readlinkSync(join(fdRoot, name));
      if (target.endsWith(' (deleted)')) deleted.push(target.replace(/^.*\//, ''));
    } catch {
      // Process or descriptor raced away; the namespace sample remains usable.
    }
  }
  return deleted.sort();
}

async function sampleWhile(root, intervalMs, action, enabled = true) {
  const samples = [];
  let running = true;
  let childPid = null;
  const sampler = (async () => {
    if (!enabled) return;
    while (running) {
      const sample = scanTree(root);
      sample.phase = inferPhase(sample);
      sample.deletedOpenFiles = inspectDeletedOpenFiles(childPid);
      samples.push(sample);
      await sleep(intervalMs);
    }
  })();
  let actionResult;
  let actionError;
  try {
    actionResult = await action((pid) => { childPid = pid; });
  } catch (error) {
    actionError = error;
  } finally {
    if (enabled) {
      samples.push(scanTree(root));
      await sleep(intervalMs);
      samples.push(scanTree(root));
    }
    running = false;
    await sampler;
  }
  if (actionError) throw actionError;
  return summarizeSamples(samples, root, actionResult, enabled);
}

function summarizeSamples(samples, root, actionResult, enabled) {
  for (const sample of samples) sample.phase = inferPhase(sample);
  const complete = samples.filter((sample) => sample.complete);
  const steady = scanTree(root, { inventory: true, phase: 'post-operation-steady' });
  steady.pruningComplete = steady.stagingCount === 0 && steady.generationCount <= 3;
  steady.pruningEvidence = {
    stagingCount: steady.stagingCount,
    generationCount: steady.generationCount,
    criterion: 'no staging entries and at most active plus two validated predecessors',
  };
  if (steady.activeGeneration && !steady.pruningComplete) {
    throw new Error(
      `post-operation state is not pruned steady state: generations=${steady.generationCount}, staging=${steady.stagingCount}`,
    );
  }
  const peak = complete.reduce(
    (best, sample) => (!best || sample.allocatedBytes > best.allocatedBytes ? sample : best),
    null,
  ) ?? steady;
  let maxGapMs = 0;
  for (let index = 1; index < complete.length; index += 1) {
    maxGapMs = Math.max(maxGapMs, complete[index].monotonicMs - complete[index - 1].monotonicMs);
  }
  const dataRootValidation = assertSuccessfulDataRoot(root, steady);
  const active = steady.generations[steady.activeGeneration];
  const predecessors = Object.entries(steady.generations)
    .filter(([name]) => !name.startsWith('.staging-') && name !== steady.activeGeneration)
    .map(([generation, size]) => ({ generation, ...size }));
  return {
    samplingEnabled: enabled,
    actionResult,
    sampleCount: samples.length,
    completeSampleCount: complete.length,
    incompleteSampleCount: samples.length - complete.length,
    maxCompleteSampleGapMs: maxGapMs,
    peak: {
      observedMonotonicMs: peak.monotonicMs,
      phase: peak.phase,
      apparentBytes: peak.apparentBytes,
      allocatedBytes: peak.allocatedBytes,
      generationCount: peak.generationCount,
      stagingCount: peak.stagingCount,
      scanDurationMs: peak.scanDurationMs,
    },
    steady,
    dataRootValidation,
    activeGeneration: active,
    predecessors,
    retentionAllocatedBytes: predecessors.reduce((sum, generation) => sum + generation.allocatedBytes, 0),
    samples,
  };
}

function spawnCaptured(command, args, options = {}) {
  const child = spawn(command, args, {
    cwd: options.cwd,
    env: { ...process.env, ...(options.env ?? {}) },
    stdio: ['ignore', 'pipe', 'pipe'],
    detached: process.platform !== 'win32',
  });
  ACTIVE_CHILDREN.add(child);
  let stdout = '';
  let stderr = '';
  child.stdout.on('data', (chunk) => { stdout += chunk; });
  child.stderr.on('data', (chunk) => { stderr += chunk; });
  const done = new Promise((resolvePromise, rejectPromise) => {
    child.once('error', rejectPromise);
    child.once('exit', (code, signal) => {
      ACTIVE_CHILDREN.delete(child);
      if (code === 0) resolvePromise({ code, signal, stdout, stderr });
      else rejectPromise(new Error(
        `${commandText(command, args)} failed with code=${code} signal=${signal}\n${stderr.slice(-4000)}`,
      ));
    });
  });
  return { child, done, output: () => ({ stdout, stderr }) };
}

async function sampledIndex(binary, configPath, dataRoot, intervalMs) {
  return sampleWhile(dataRoot, intervalMs, async (setPid) => {
    const processRun = spawnCaptured(binary, ['--config', configPath, '--data-dir', dataRoot, 'index']);
    setPid(processRun.child.pid);
    const result = await processRun.done;
    return { stdout: result.stdout.trim().replaceAll(configPath, '<config>').replaceAll(dataRoot, '<data-root>') };
  });
}

function indexOnce(binary, configPath, dataRoot) {
  return run(binary, ['--config', configPath, '--data-dir', dataRoot, 'index']);
}

async function waitFor(predicate, timeoutMs, label) {
  const deadline = performance.now() + timeoutMs;
  let lastError;
  while (performance.now() < deadline) {
    try {
      const value = predicate();
      if (value) return value;
    } catch (error) {
      lastError = error;
    }
    await sleep(25);
  }
  throw new Error(`timed out waiting for ${label}${lastError ? `: ${lastError.message}` : ''}`);
}

async function startDaemon(binary, configPath, dataRoot) {
  const processRun = spawnCaptured(binary, [
    '--config', configPath,
    '--data-dir', dataRoot,
    'serve',
    '--bind', '127.0.0.1:0',
  ]);
  await waitFor(
    () => existsSync(join(dataRoot, 'connection.json')) && readCurrentGeneration(dataRoot),
    180_000,
    'daemon readiness',
  );
  return processRun;
}

async function stopDaemon(daemon) {
  signalChildGroup(daemon.child, 'SIGINT');
  await Promise.race([
    daemon.done,
    sleep(30_000).then(() => {
      signalChildGroup(daemon.child, 'SIGKILL');
      throw new Error('daemon process group did not stop within 30 seconds');
    }),
  ]);
}

function findEditableFile(corpusRoot) {
  const stack = [corpusRoot];
  while (stack.length > 0) {
    const path = stack.pop();
    for (const entry of readdirSync(path, { withFileTypes: true })) {
      const child = join(path, entry.name);
      if (entry.isDirectory()) stack.push(child);
      else if (entry.isFile() && entry.name.endsWith('.md')) {
        const text = readFileSync(child, 'utf8');
        if (text.includes('\nsee [[')) return child;
      }
    }
  }
  throw new Error('no deterministic same-length edit target found');
}

function toggleSameLengthEdit(path) {
  const before = readFileSync(path, 'utf8');
  let after;
  if (before.includes('\nsee [[')) after = before.replace('\nsee [[', '\nSEE [[');
  else if (before.includes('\nSEE [[')) after = before.replace('\nSEE [[', '\nsee [[');
  else throw new Error(`same-length marker missing in ${path}`);
  if (Buffer.byteLength(before) !== Buffer.byteLength(after)) {
    throw new Error('same-length edit changed source byte length');
  }
  writeDurably(path, after);
  return Buffer.byteLength(after);
}

async function sampledReconcile(dataRoot, corpusRoot, daemon, intervalMs, enabled = true) {
  const beforeGeneration = readCurrentGeneration(dataRoot);
  const generationsRoot = join(dataRoot, 'generations');
  const beforeGenerationNames = existsSync(generationsRoot)
    ? readdirSync(generationsRoot).filter((name) => !name.startsWith('.staging-')).sort()
    : [];
  const beforeCorpusBytes = scanTree(corpusRoot).apparentBytes;
  const editPath = findEditableFile(corpusRoot);
  const measurement = await sampleWhile(dataRoot, intervalMs, async (setPid) => {
    setPid(daemon.child.pid);
    toggleSameLengthEdit(editPath);
    let stableGeneration = null;
    let stableSince = null;
    const next = await waitFor(() => {
      const generation = readCurrentGeneration(dataRoot);
      const names = existsSync(join(dataRoot, 'generations'))
        ? readdirSync(join(dataRoot, 'generations'))
        : [];
      const staging = names.filter((name) => name.startsWith('.staging-')).length;
      const published = names.filter((name) => !name.startsWith('.staging-')).length;
      if (generation && generation !== beforeGeneration && staging === 0 && published <= 3) {
        if (stableGeneration !== generation) {
          stableGeneration = generation;
          stableSince = performance.now();
        } else if (performance.now() - stableSince >= 100) {
          return generation;
        }
      }
      return null;
    }, 180_000, 'reconcile publication and retention pruning');
    const afterGenerationNames = readdirSync(generationsRoot)
      .filter((name) => !name.startsWith('.staging-'))
      .sort();
    const addedGenerations = afterGenerationNames.filter((name) => !beforeGenerationNames.includes(name));
    if (addedGenerations.length !== 1 || addedGenerations[0] !== next) {
      throw new Error(
        `reconcile must publish exactly one generation: added=${JSON.stringify(addedGenerations)}, current=${next}`,
      );
    }
    return {
      beforeGeneration,
      afterGeneration: next,
      addedGenerations,
      beforeGenerationCount: beforeGenerationNames.length,
      afterGenerationCount: afterGenerationNames.length,
      pruningComplete: afterGenerationNames.length <= 3,
    };
  }, enabled);
  const afterCorpusBytes = scanTree(corpusRoot).apparentBytes;
  if (beforeCorpusBytes !== afterCorpusBytes) {
    throw new Error(`same-length reconcile changed corpus bytes: ${beforeCorpusBytes} -> ${afterCorpusBytes}`);
  }
  return measurement;
}

function assertPeakAboveSteady(measurement) {
  if (!(measurement.peak.allocatedBytes > measurement.steady.allocatedBytes)) {
    throw new Error(
      `publication peak assertion failed: observed peak ${measurement.peak.allocatedBytes} ` +
      `must be strictly above steady ${measurement.steady.allocatedBytes}`,
    );
  }
}

function negativeControls(workRoot, blockSize) {
  const root = join(workRoot, 'negative-controls');
  rmSync(root, { recursive: true, force: true });
  mkdirSync(root, { recursive: true });
  const before = scanTree(root, { inventory: true, phase: 'calibration-before' });
  const calibrationPath = join(root, 'calibration-64MiB.bin');
  const fd = openSync(calibrationPath, 'w');
  const block = Buffer.alloc(1024 * 1024, 0xa5);
  try {
    for (let written = 0; written < CALIBRATION_BYTES; written += block.length) writeSync(fd, block);
    fsyncSync(fd);
  } finally {
    closeSync(fd);
  }
  const fileStat = statSync(calibrationPath, { bigint: true });
  const fileApparent = Number(fileStat.size);
  const fileAllocated = Number(fileStat.blocks * 512n);
  const after = scanTree(root, { inventory: true, phase: 'calibration-after' });
  const apparentDelta = after.apparentBytes - before.apparentBytes;
  const allocatedDelta = after.allocatedBytes - before.allocatedBytes;
  const calibrationPass =
    Math.abs(apparentDelta - fileApparent) <= blockSize &&
    Math.abs(allocatedDelta - fileAllocated) <= blockSize &&
    fileApparent === CALIBRATION_BYTES && fileAllocated >= CALIBRATION_BYTES;
  if (!calibrationPass) {
    throw new Error(
      `64 MiB calibration failed: apparent delta=${apparentDelta}, file=${fileApparent}; ` +
      `allocated delta=${allocatedDelta}, file=${fileAllocated}, tolerance=${blockSize}`,
    );
  }

  const omitted = scanTree(root, {
    inventory: true,
    phase: 'fault-omit-calibration',
    omitRelative: (path) => path === 'calibration-64MiB.bin',
  });
  let omissionDetected = false;
  let omissionFailure = null;
  try {
    if (omitted.allocatedBytes !== omitted.duAllocatedBytes) {
      throw new Error(
        `scanner/du reconciliation failed as injected: scanner=${omitted.allocatedBytes}, du=${omitted.duAllocatedBytes}`,
      );
    }
  } catch (error) {
    omissionDetected = true;
    omissionFailure = error.message;
  }
  if (!omissionDetected) throw new Error('injected omission was not detected');

  const expectedDataRoot = join(root, 'expected-data-root');
  const wrongDataRoot = join(root, 'wrong-data-root');
  const registration = registerDataRoot(expectedDataRoot, 'wrong-directory-negative-control');
  mkdirSync(wrongDataRoot, { recursive: true });
  let wrongDirectoryDetected = false;
  let wrongDirectoryFailure = null;
  try {
    verifyDataRootMarker(wrongDataRoot, registration);
  } catch (error) {
    wrongDirectoryDetected = true;
    wrongDirectoryFailure = error.message;
  }
  if (!wrongDirectoryDetected) throw new Error('injected wrong data-root directory was not detected');
  DATA_ROOTS.delete(resolve(expectedDataRoot));

  unlinkSync(calibrationPath);
  return {
    calibration: {
      pass: calibrationPass,
      expectedBytes: CALIBRATION_BYTES,
      fileApparentBytes: fileApparent,
      fileAllocatedBytes: fileAllocated,
      measuredApparentDeltaBytes: apparentDelta,
      measuredAllocatedDeltaBytes: allocatedDelta,
      toleranceBytes: blockSize,
    },
    injectedOmission: {
      pass: omissionDetected,
      expectedFailure: omissionFailure,
      omittedScannerAllocatedBytes: omitted.allocatedBytes,
      fullDuAllocatedBytes: omitted.duAllocatedBytes,
    },
    injectedWrongDirectory: {
      pass: wrongDirectoryDetected,
      expectedFailure: wrongDirectoryFailure,
      expectedSentinel: registration.markerName,
    },
  };
}

function cleanScenarioRecord(name, corpus, measurement, profile, partitions, repeat) {
  return {
    name,
    kind: 'clean-build',
    profile,
    partitions,
    repeat,
    corpus: {
      files: corpus.files,
      chunks: corpus.chunks,
      apparentBytes: corpus.apparentBytes,
    },
    measurement,
    ratios: {
      steadyAllocatedPerCorpusApparent: measurement.steady.allocatedBytes / corpus.apparentBytes,
      peakAllocatedPerCorpusApparent: measurement.peak.allocatedBytes / corpus.apparentBytes,
      peakOverSteady: measurement.peak.allocatedBytes / measurement.steady.allocatedBytes,
    },
  };
}

function compactMeasurement(name, corpus, measurement) {
  return {
    scenario: name,
    corpusMiB: corpus ? corpus.apparentBytes / 1024 / 1024 : null,
    steadyApparentMiB: measurement.steady.apparentBytes / 1024 / 1024,
    steadyMiB: measurement.steady.allocatedBytes / 1024 / 1024,
    activeApparentMiB: measurement.activeGeneration ? measurement.activeGeneration.apparentBytes / 1024 / 1024 : null,
    activeMiB: measurement.activeGeneration ? measurement.activeGeneration.allocatedBytes / 1024 / 1024 : null,
    retentionApparentMiB: measurement.predecessors.reduce((sum, generation) => sum + generation.apparentBytes, 0) / 1024 / 1024,
    retentionMiB: measurement.retentionAllocatedBytes / 1024 / 1024,
    peakApparentMiB: measurement.peak.apparentBytes / 1024 / 1024,
    peakMiB: measurement.peak.allocatedBytes / 1024 / 1024,
    peakOverSteady: measurement.peak.allocatedBytes / measurement.steady.allocatedBytes,
    generations: measurement.steady.generationCount,
    samples: measurement.sampleCount,
    maxGapMs: measurement.maxCompleteSampleGapMs,
    incomplete: measurement.incompleteSampleCount,
  };
}

function appAlloc(apparent, allocated) {
  if (apparent === null || allocated === null) return '-';
  return `${apparent.toFixed(2)}/${allocated.toFixed(2)}`;
}

function median(values) {
  const ordered = [...values].sort((left, right) => left - right);
  const middle = Math.floor(ordered.length / 2);
  return ordered.length % 2 === 0
    ? (ordered[middle - 1] + ordered[middle]) / 2
    : ordered[middle];
}

function aggregateCleanBuilds(rows) {
  const groups = new Map();
  for (const row of rows.filter((candidate) => candidate.scenario.startsWith('clean-'))) {
    const key = row.scenario.replace(/-r\d+$/, '');
    const group = groups.get(key) ?? [];
    group.push(row);
    groups.set(key, group);
  }
  return [...groups.entries()].map(([scenario, group]) => ({
    scenario,
    repeats: group.length,
    steadyAllocatedMiB: {
      median: median(group.map((row) => row.steadyMiB)),
      minimum: Math.min(...group.map((row) => row.steadyMiB)),
      maximum: Math.max(...group.map((row) => row.steadyMiB)),
    },
    peakAllocatedMiB: {
      median: median(group.map((row) => row.peakMiB)),
      minimum: Math.min(...group.map((row) => row.peakMiB)),
      maximum: Math.max(...group.map((row) => row.peakMiB)),
    },
  }));
}

function printTable(rows) {
  const headers = ['scenario', 'corpus app MiB', 'steady app/alloc', 'active app/alloc', 'retained app/alloc', 'peak app/alloc', 'peak/steady', 'gens', 'samples', 'max gap ms', 'incomplete'];
  const body = rows.map((row) => [
    row.scenario,
    row.corpusMiB === null ? '-' : row.corpusMiB.toFixed(2),
    appAlloc(row.steadyApparentMiB, row.steadyMiB),
    appAlloc(row.activeApparentMiB, row.activeMiB),
    appAlloc(row.retentionApparentMiB, row.retentionMiB),
    appAlloc(row.peakApparentMiB, row.peakMiB),
    row.peakOverSteady.toFixed(3),
    String(row.generations),
    String(row.samples),
    row.maxGapMs.toFixed(1),
    String(row.incomplete),
  ]);
  const widths = headers.map((header, index) => Math.max(header.length, ...body.map((row) => row[index].length)));
  const line = (row) => row.map((cell, index) => cell.padEnd(widths[index])).join('  ').trimEnd();
  console.log(line(headers));
  console.log(widths.map((width) => '-'.repeat(width)).join('  '));
  for (const row of body) console.log(line(row));
}

function printAggregates(aggregates) {
  const repeated = aggregates.filter((aggregate) => aggregate.repeats > 1);
  if (repeated.length === 0) return;
  console.log('\nClean-build allocated MiB median [min..max]');
  for (const aggregate of repeated) {
    const steady = aggregate.steadyAllocatedMiB;
    const peak = aggregate.peakAllocatedMiB;
    console.log(
      `${aggregate.scenario}: steady ${steady.median.toFixed(2)} ` +
      `[${steady.minimum.toFixed(2)}..${steady.maximum.toFixed(2)}], ` +
      `peak ${peak.median.toFixed(2)} [${peak.minimum.toFixed(2)}..${peak.maximum.toFixed(2)}] ` +
      `(n=${aggregate.repeats})`,
    );
  }
}

function provenance(binary, options, workRoot) {
  const filesystem = filesystemIdentity(workRoot);
  MEASUREMENT_FILESYSTEM = filesystem;
  const commit = run('git', ['-C', REPO_ROOT, 'rev-parse', 'HEAD']);
  const branch = run('git', ['-C', REPO_ROOT, 'branch', '--show-current']);
  const dirty = run('git', ['-C', REPO_ROOT, 'status', '--short']);
  const coreutils = run('du', ['--version']).split('\n')[0];
  const binaryVersion = run(binary, ['--version']);
  const scriptText = readFileSync(fileURLToPath(import.meta.url), 'utf8');
  const wsl = process.platform === 'linux' && /microsoft|wsl/i.test(os.release());
  return {
    timestampUtc: new Date().toISOString(),
    platform: `${process.platform}-${process.arch}`,
    kernel: `${os.type()} ${os.release()}`,
    wsl,
    filesystem: {
      ...filesystem,
      measuredPathKind: 'temporary work root containing every data root',
      measurementScope: wsl
        ? 'guest ext allocation; does not measure Windows VHDX/NTFS physical reclamation'
        : 'namespace-visible allocation on the reported filesystem',
    },
    tools: {
      node: process.version,
      python: run('python3', ['--version']),
      rustc: run('rustc', ['--version']),
      cargo: run('cargo', ['--version']),
      coreutils,
      kwiry: binaryVersion,
    },
    git: {
      commit,
      branch,
      dirty: dirty.length > 0,
    },
    harness: {
      kind: 'Node.js stdlib script driving the built kwiry binary',
      rationale: 'exercises shipped CLI/daemon lifecycle and imports the existing deterministic corpus generator without adding a second generator',
      samplingIntervalDelayMs: options.intervalMs,
      samplingIntervalRationale: '10 ms is below watcher debounce and intended to observe staging copy/merge; actual scan-limited maximum gaps are reported per scenario',
      scriptSha256: sha256Text(scriptText),
      binarySha256: sha256File(binary),
      commandLine: [
        'node', 'bench/premerge/disk-usage/run.mjs',
        '--suite', options.suite,
        '--repeats', String(options.repeats),
        '--interval-ms', String(options.intervalMs),
        ...(options.includeRenameDelete ? ['--include-rename-delete'] : []),
      ],
    },
    limitations: [
      'Unheld Tantivy commit/merge peaks are maximum observed values, not guaranteed instantaneous maxima.',
      'Samples with ENOENT or scanner/du disagreement are incomplete and cannot establish a peak.',
      'du and namespace scans cannot charge blocks for unlinked-but-open files; /proc deleted descriptors are inventoried on Linux when visible.',
      'df deltas are not used because the filesystem is not isolated from unrelated host activity.',
      'Allocated bytes use POSIX st_blocks times 512; apparent bytes use regular-file st_size.',
      'Semantic/model scenario is not mixed into lexical retention measurements.',
    ],
  };
}

async function runCleanBuild(binary, workRoot, corpus, label, profile, partitions, repeat, intervalMs) {
  const root = join(workRoot, `clean-${label}-${profile}-${partitions}-r${repeat}`);
  const dataRoot = join(root, 'data');
  const configPath = join(root, 'config.toml');
  registerDataRoot(dataRoot, `clean-${label}-${profile}-p${partitions}-r${repeat}`);
  if (profile === 'desktop') {
    writeDesktopConfig(configPath, corpus.roots[0]);
  } else {
    writeOpenClastConfig(configPath, corpus.roots, join(root, 'jwks.json'));
  }
  const baseline = scanTree(dataRoot, { inventory: true, phase: 'empty-root' });
  const measurement = await sampledIndex(binary, configPath, dataRoot, intervalMs);
  measurement.baseline = baseline;
  return cleanScenarioRecord(`clean-${label}-${profile}-p${partitions}-r${repeat}`, corpus, measurement, profile, partitions, repeat);
}

async function runSaturatedRebuild(binary, workRoot, corpus, intervalMs) {
  const root = join(workRoot, 'saturated-rebuild');
  const dataRoot = join(root, 'data');
  const configPath = join(root, 'config.toml');
  registerDataRoot(dataRoot, 'saturated-explicit-rebuild');
  writeDesktopConfig(configPath, corpus.roots[0]);
  for (let generation = 0; generation < 3; generation += 1) indexOnce(binary, configPath, dataRoot);
  const saturated = scanTree(dataRoot, { inventory: true, phase: 'retained-three-baseline' });
  const measurement = await sampledIndex(binary, configPath, dataRoot, intervalMs);
  measurement.saturatedBaseline = saturated;
  assertPeakAboveSteady(measurement);
  return {
    name: 'saturated-explicit-rebuild',
    kind: 'saturated-rebuild',
    corpus: { files: corpus.files, chunks: corpus.chunks, apparentBytes: corpus.apparentBytes },
    measurement,
  };
}

async function runReconcileSeries(binary, workRoot, corpus, intervalMs, cycles) {
  const root = join(workRoot, `reconcile-${corpus.files}`);
  const dataRoot = join(root, 'data');
  const configPath = join(root, 'config.toml');
  registerDataRoot(dataRoot, `reconcile-growth-${corpus.files}`);
  writeDesktopConfig(configPath, corpus.roots[0]);
  for (let generation = 0; generation < 3; generation += 1) indexOnce(binary, configPath, dataRoot);
  let daemon = await startDaemon(binary, configPath, dataRoot);
  const beforeNoop = readCurrentGeneration(dataRoot);
  const beforeNoopNames = readdirSync(join(dataRoot, 'generations'))
    .filter((name) => !name.startsWith('.staging-'))
    .sort();
  await stopDaemon(daemon);
  daemon = await startDaemon(binary, configPath, dataRoot);
  const afterNoop = readCurrentGeneration(dataRoot);
  const afterNoopNames = readdirSync(join(dataRoot, 'generations'))
    .filter((name) => !name.startsWith('.staging-'))
    .sort();
  if (beforeNoop !== afterNoop || JSON.stringify(beforeNoopNames) !== JSON.stringify(afterNoopNames)) {
    await stopDaemon(daemon);
    throw new Error(`no-op boot reconciliation published a generation: ${beforeNoop} -> ${afterNoop}`);
  }

  const publications = [];
  for (let cycle = 0; cycle < cycles; cycle += 1) {
    const measurement = await sampledReconcile(dataRoot, corpus.roots[0], daemon, intervalMs, true);
    assertPeakAboveSteady(measurement);
    publications.push({ cycle: cycle + 1, measurement });
  }

  // Fault injection: perform a real copy-aside publication with sampling
  // disabled, then prove the same strict peak assertion rejects the result.
  const disabled = await sampledReconcile(dataRoot, corpus.roots[0], daemon, intervalMs, false);
  let disabledSamplingRejected = false;
  let disabledSamplingFailure = null;
  try {
    assertPeakAboveSteady(disabled);
  } catch (error) {
    disabledSamplingRejected = true;
    disabledSamplingFailure = error.message;
  }
  if (!disabledSamplingRejected) {
    await stopDaemon(daemon);
    throw new Error('peak check did not fail when sampling was disabled');
  }
  await stopDaemon(daemon);
  return {
    name: `reconcile-growth-${corpus.files}`,
    kind: 'reconcile-series',
    corpus: { files: corpus.files, chunks: corpus.chunks, apparentBytes: corpus.apparentBytes },
    noOpPass: {
      pass: beforeNoop === afterNoop && JSON.stringify(beforeNoopNames) === JSON.stringify(afterNoopNames),
      beforeGeneration: beforeNoop,
      afterGeneration: afterNoop,
      beforeGenerationNames: beforeNoopNames,
      afterGenerationNames: afterNoopNames,
    },
    publications,
    samplingDisabledNegativeControl: {
      pass: disabledSamplingRejected,
      expectedFailure: disabledSamplingFailure,
      realPublication: disabled.actionResult,
      measurement: disabled,
    },
  };
}

async function runRenameDeleteBatch(binary, workRoot, corpus, intervalMs) {
  const root = join(workRoot, 'rename-delete-10000');
  const dataRoot = join(root, 'data');
  const configPath = join(root, 'config.toml');
  registerDataRoot(dataRoot, 'rename-delete-10000');
  writeDesktopConfig(configPath, corpus.roots[0]);
  for (let generation = 0; generation < 3; generation += 1) indexOnce(binary, configPath, dataRoot);
  const daemon = await startDaemon(binary, configPath, dataRoot);
  const files = [];
  const stack = [corpus.roots[0]];
  while (stack.length > 0) {
    const directory = stack.pop();
    for (const entry of readdirSync(directory, { withFileTypes: true })) {
      const path = join(directory, entry.name);
      if (entry.isDirectory()) stack.push(path);
      else if (entry.isFile() && entry.name.endsWith('.md')) files.push(path);
    }
  }
  files.sort();
  if (files.length !== 10_000) throw new Error(`rename/delete expected 10,000 files, found ${files.length}`);
  const beforeGeneration = readCurrentGeneration(dataRoot);
  const batch = await sampleWhile(dataRoot, intervalMs, async (setPid) => {
    setPid(daemon.child.pid);
    for (const path of files.slice(0, 2_500)) renameSync(path, path.replace(/\.md$/, '-renamed.md'));
    for (const path of files.slice(2_500, 5_000)) unlinkSync(path);
    let stableGeneration = null;
    let stableSince = null;
    const afterGeneration = await waitFor(() => {
      const generation = readCurrentGeneration(dataRoot);
      const names = readdirSync(join(dataRoot, 'generations'));
      const staging = names.filter((name) => name.startsWith('.staging-')).length;
      const published = names.filter((name) => !name.startsWith('.staging-')).length;
      if (generation && generation !== beforeGeneration && staging === 0 && published <= 3) {
        if (stableGeneration !== generation) {
          stableGeneration = generation;
          stableSince = performance.now();
        } else if (performance.now() - stableSince >= 100) {
          return generation;
        }
      }
      return null;
    }, 300_000, 'rename/delete publication and retention pruning');
    return {
      beforeGeneration,
      afterGeneration,
      renamed: 2_500,
      deleted: 2_500,
      pruningComplete: true,
    };
  });
  const aging = [];
  for (let publication = 0; publication < 3; publication += 1) {
    aging.push(await sampledReconcile(dataRoot, corpus.roots[0], daemon, intervalMs, true));
  }
  await stopDaemon(daemon);
  return {
    name: 'rename-delete-10000',
    kind: 'rename-delete',
    corpusBefore: { files: corpus.files, chunks: corpus.chunks, apparentBytes: corpus.apparentBytes },
    batch,
    agingPublications: aging,
  };
}

async function main() {
  const options = parseArgs(process.argv.slice(2));
  const workRoot = join(os.tmpdir(), `kwiry-disk-usage-${process.pid}`);
  mkdirSync(workRoot, { recursive: true });
  let binary = options.binary;
  if (!binary) {
    console.error('Building lexical-only release kwiry binary...');
    run('cargo', ['build', '-p', 'kwiry', '--no-default-features', '--release'], { cwd: DAEMON_DIR, stdio: 'inherit' });
    binary = join(DAEMON_DIR, 'target', 'release', 'kwiry');
  }
  if (!existsSync(binary)) throw new Error(`kwiry binary not found: ${binary}`);
  const evidence = {
    schemaVersion: 1,
    provenance: provenance(binary, options, workRoot),
    controls: null,
    corpora: {},
    scenarios: [],
    summary: [],
  };
  const blockSize = evidence.provenance.filesystem.fundamentalBlockSize;
  evidence.controls = negativeControls(workRoot, blockSize);
  console.error('Negative controls passed: 64 MiB calibration, injected path omission, and injected wrong data root.');

  if (options.suite !== 'controls') {
    const smokeCorpus = copyFixtureCorpus(join(REPO_ROOT, 'fixtures', 'vault'), join(workRoot, 'corpus-smoke'));
    evidence.corpora.smoke = { ...smokeCorpus, roots: undefined };
    const smoke = await runCleanBuild(binary, workRoot, smokeCorpus, 'smoke', 'desktop', 1, 1, options.intervalMs);
    evidence.scenarios.push(smoke);
    evidence.summary.push(compactMeasurement(smoke.name, smoke.corpus, smoke.measurement));
  }

  const corpora = new Map();
  if (options.suite === 'core' || options.suite === 'full') {
    for (const count of [100, 1_000, 10_000]) {
      console.error(`Materializing existing deterministic corpus prefix: ${count} notes...`);
      const corpus = materializeCorpus(join(workRoot, `corpus-${count}`), count, 1);
      corpora.set(count, corpus);
      evidence.corpora[String(count)] = { files: corpus.files, chunks: corpus.chunks, apparentBytes: corpus.apparentBytes, partitions: 1 };
      for (let repeat = 1; repeat <= options.repeats; repeat += 1) {
        console.error(`Clean Desktop build: ${count} notes, repeat ${repeat}/${options.repeats}...`);
        const scenario = await runCleanBuild(binary, workRoot, corpus, String(count), 'desktop', 1, repeat, options.intervalMs);
        evidence.scenarios.push(scenario);
        evidence.summary.push(compactMeasurement(scenario.name, scenario.corpus, scenario.measurement));
      }
    }

    console.error('Measuring saturated explicit rebuild on 10,000 notes...');
    const saturated = await runSaturatedRebuild(binary, workRoot, corpora.get(10_000), options.intervalMs);
    evidence.scenarios.push(saturated);
    evidence.summary.push(compactMeasurement(saturated.name, saturated.corpus, saturated.measurement));

    const mediumCycles = options.suite === 'full' ? 20 : 4;
    const fullCycles = options.suite === 'full' ? 8 : 1;
    console.error(`Measuring ${mediumCycles} same-size reconcile cycles on 1,000 notes...`);
    const mediumGrowth = await runReconcileSeries(binary, workRoot, corpora.get(1_000), options.intervalMs, mediumCycles);
    evidence.scenarios.push(mediumGrowth);
    for (const publication of mediumGrowth.publications) {
      evidence.summary.push(compactMeasurement(
        `reconcile-1000-cycle-${publication.cycle}`,
        mediumGrowth.corpus,
        publication.measurement,
      ));
    }
    console.error(`Measuring ${fullCycles} same-size reconcile cycles on 10,000 notes...`);
    const fullGrowth = await runReconcileSeries(binary, workRoot, corpora.get(10_000), options.intervalMs, fullCycles);
    evidence.scenarios.push(fullGrowth);
    for (const publication of fullGrowth.publications) {
      evidence.summary.push(compactMeasurement(
        `reconcile-10000-cycle-${publication.cycle}`,
        fullGrowth.corpus,
        publication.measurement,
      ));
    }
  }

  if (options.suite === 'full') {
    for (const partitions of [1, 8, 32]) {
      const corpus = materializeCorpus(join(workRoot, `corpus-openclast-${partitions}`), 10_000, partitions);
      for (let repeat = 1; repeat <= options.repeats; repeat += 1) {
        console.error(`Clean OpenClast build: 10,000 notes, ${partitions} partitions, repeat ${repeat}/${options.repeats}...`);
        const scenario = await runCleanBuild(binary, workRoot, corpus, '10000', 'openclast', partitions, repeat, options.intervalMs);
        evidence.scenarios.push(scenario);
        evidence.summary.push(compactMeasurement(scenario.name, scenario.corpus, scenario.measurement));
      }
    }
    if (options.includeRenameDelete) {
      console.error('Measuring 2,500 rename + 2,500 delete batch on 10,000 notes...');
      const batch = await runRenameDeleteBatch(binary, workRoot, corpora.get(10_000), options.intervalMs);
      evidence.scenarios.push(batch);
      evidence.summary.push(compactMeasurement(batch.name, batch.corpusBefore, batch.batch));
      for (let index = 0; index < batch.agingPublications.length; index += 1) {
        evidence.summary.push(compactMeasurement(
          `rename-delete-aging-${index + 1}`,
          batch.corpusBefore,
          batch.agingPublications[index],
        ));
      }
    }
  }

  evidence.aggregates = aggregateCleanBuilds(evidence.summary);
  evidence.provenance.dataRoots = [...DATA_ROOTS.values()].map(({ label, filesystem }) => ({
    label,
    filesystem,
    matchesMeasurementFilesystem: sameFilesystem(filesystem, evidence.provenance.filesystem),
  }));
  evidence.provenance.completedUtc = new Date().toISOString();
  evidence.provenance.coverage = {
    suite: options.suite,
    semanticMeasured: false,
    crashFaultCheckpointsMeasured: false,
    renameDeleteMeasured: options.suite === 'full' && options.includeRenameDelete,
    renameDeleteStatus: options.includeRenameDelete
      ? 'requested'
      : 'not run; an earlier bounded attempt exposed a product-side stall and the stress case is now opt-in',
    note: 'Core owner questions are measured lexically. Semantic/model and private test-only publication-fault checkpoints remain explicitly outside this CLI-driven run.',
  };
  mkdirSync(dirname(options.output), { recursive: true });
  writeFileSync(options.output, `${JSON.stringify(evidence, null, 2)}\n`);
  console.log(
    `Provenance: ${evidence.provenance.timestampUtc} | ${evidence.provenance.platform} | ` +
    `${evidence.provenance.filesystem.type} block=${evidence.provenance.filesystem.fundamentalBlockSize} | ` +
    `kwiry=${evidence.provenance.tools.kwiry} | commit=${evidence.provenance.git.commit} | ` +
    `sample-delay=${evidence.provenance.harness.samplingIntervalDelayMs}ms`,
  );
  printTable(evidence.summary);
  printAggregates(evidence.aggregates);
  console.log(`\nJSON evidence: ${relative(REPO_ROOT, options.output)}`);
  if (options.keepWork) console.log(`Temporary work retained: ${workRoot}`);
  else rmSync(workRoot, { recursive: true, force: true });
}

main().catch((error) => {
  console.error(error.stack ?? error.message);
  stopAllChildGroups('SIGTERM');
  setTimeout(() => {
    stopAllChildGroups('SIGKILL');
    process.exit(1);
  }, 1_000);
});
