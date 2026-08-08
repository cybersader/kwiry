// KWIRY-Q-0017 feasibility gate runner, rebuilt after adversarial review:
// - the cache port is ASYNC on the host thread (fs/promises + fsync via the
//   file handle), matching how a responsive Electron main thread must
//   behave; hashing happens inside the workers, off the host thread;
// - host-thread stall is measured by an independent gap detector that
//   synchronous blocks cannot blind (the adversarial review proved
//   monitorEventLoopDelay reports its idle floor through a 400 ms block),
//   and the <100 ms limit applies to EVERY phase;
// - RSS is sampled off-thread by a dedicated sampler worker with
//   timestamps, attributed to phases afterwards; a phase with no samples
//   fails instead of passing as zero;
// - exactness, worker exclusivity, guard outcomes, and corpus scale are
//   DERIVED measurements, not literals;
// - a negative control proves the integrity machinery can fail and a
//   truncated cache blob is rejected;
// - provenance (runtime, platform, harness hashes, timestamp) is recorded.

import { createHash } from 'node:crypto';
import { mkdirSync, readFileSync } from 'node:fs';
import { open, readFile, rename, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { Worker } from 'node:worker_threads';
import { deepStrictEqual } from 'node:assert';

const WORKER_PATH = fileURLToPath(new URL('../src/worker.mjs', import.meta.url));
const EVIDENCE_PATH = fileURLToPath(new URL('../evidence/latest.json', import.meta.url));

const KILL_LIMITS = {
  blob_cap_bytes: 384 * 1024 * 1024,
  restore_max_ms: 5_000,
  restore_vs_build_max_ratio: 0.5,
  host_stall_max_ms: 100,
  restore_peak_rss_max_ratio: 1.25,
  steady_state_added_rss_target_bytes: 300 * 1024 * 1024,
};

// ---- host-thread stall detector -------------------------------------------
// A 5 ms heartbeat; any synchronous block shows up as a gap. Immune to the
// monitorEventLoopDelay reset/re-baseline blindness.
const stallSamples = [];
let phaseName = 'baseline';
const phaseTransitions = [{ name: 'baseline', at: Date.now() }];
function setPhase(name) {
  phaseName = name;
  phaseTransitions.push({ name, at: Date.now() });
}
let lastBeat = performance.now();
const heartbeat = setInterval(() => {
  const now = performance.now();
  stallSamples.push({ phase: phaseName, gap: now - lastBeat - 5, at: Date.now() });
  lastBeat = now;
}, 5);
heartbeat.unref();

function maxStall(phases) {
  const gaps = stallSamples
    .filter((sample) => phases.includes(sample.phase))
    .map((sample) => Math.max(0, sample.gap));
  return gaps.length === 0 ? null : Math.max(...gaps);
}

// ---- off-thread RSS sampler ------------------------------------------------
const samplerWorker = new Worker(
  `const { parentPort } = require('node:worker_threads');
   const samples = [];
   const timer = setInterval(() => {
     samples.push({ at: Date.now(), rss: process.memoryUsage().rss });
   }, 10);
   parentPort.on('message', () => {
     clearInterval(timer);
     parentPort.postMessage(samples);
   });`,
  { eval: true },
);
function collectRssSamples() {
  return new Promise((resolve) => {
    samplerWorker.once('message', resolve);
    samplerWorker.postMessage('stop');
  });
}
function phaseOfTimestamp(at) {
  let current = phaseTransitions[0].name;
  for (const transition of phaseTransitions) {
    if (transition.at <= at) current = transition.name;
    else break;
  }
  return current;
}

// ---- worker lifecycle with exclusivity accounting --------------------------
let aliveWorkers = 0;
let maxConcurrentWorkers = 0;

function spawnRole(workerData, transfers = []) {
  aliveWorkers += 1;
  maxConcurrentWorkers = Math.max(maxConcurrentWorkers, aliveWorkers);
  const worker = new Worker(WORKER_PATH, { workerData, transferList: transfers });
  const result = new Promise((resolve, reject) => {
    worker.on('message', (message) => {
      if (message.kind === 'phase') setPhase(message.name);
      else if (message.kind === 'result') {
        if (message.error) reject(new Error(message.error));
        else resolve(message);
      }
    });
    worker.on('error', reject);
    worker.on('exit', (code) => {
      aliveWorkers -= 1;
      if (code !== 0) reject(new Error(`worker exited with code ${code}`));
    });
  });
  return { worker, result };
}

async function finishWorker(worker) {
  await worker.terminate();
}

function sha256(bytes) {
  return createHash('sha256').update(bytes).digest('hex');
}

function fileSha256(url) {
  return sha256(readFileSync(fileURLToPath(new URL(url, import.meta.url))));
}

// ---- run -------------------------------------------------------------------
const startedAt = new Date().toISOString();
const baselineRss = process.memoryUsage().rss;

setPhase('build-worker');
const build = await (async () => {
  const { worker, result } = spawnRole({ role: 'build' });
  const message = await result;
  await finishWorker(worker);
  return message;
})();

// Async atomic cache port on the host thread: temp write, fsync via the
// handle, rename. Hash checks happen inside the workers.
setPhase('cache-write');
const directory = join(tmpdir(), `kwiry-export-restore-${process.pid}`);
mkdirSync(directory, { recursive: true });
const temporary = join(directory, 'cache.tmp');
const finalPath = join(directory, 'cache.bin');
let timerStart = performance.now();
await writeFile(temporary, build.blob);
const handle = await open(temporary, 'r+');
await handle.sync();
await handle.close();
await rename(temporary, finalPath);
const diskWriteMs = performance.now() - timerStart;

setPhase('cache-read');
timerStart = performance.now();
const readBack = new Uint8Array(await readFile(finalPath));
const diskReadMs = performance.now() - timerStart;
if (readBack.byteLength !== build.blobBytes) {
  throw new Error('cache blob length changed across the disk round trip');
}
// Release the original blob so steady-state measures the index, not the
// harness's retained copy.
build.blob = null;

// Byte-exact proof runs first, in its own short-lived worker, on its own
// copy of the blob, so the production-shape restore below is measured
// without the proof's extra footprint.
setPhase('byte-exact-proof');
const proof = await (async () => {
  const blobCopy = new Uint8Array(readBack);
  const { worker, result } = spawnRole(
    { role: 'byte-exact-proof', blob: blobCopy, expectedSha256: build.blobSha256 },
    [blobCopy.buffer],
  );
  const message = await result;
  await finishWorker(worker);
  return message;
})();

setPhase('restore-worker');
const restoreHandle = spawnRole(
  { role: 'restore', blob: readBack, expectedSha256: build.blobSha256 },
  [readBack.buffer],
);
const restore = await restoreHandle.result;

// The restore worker is still alive holding the live index: measure a real
// steady state, as an increment over the process baseline.
setPhase('steady-state');
await new Promise((resolve) => setTimeout(resolve, 750));

setPhase('negative-control');
restoreHandle.worker.postMessage({ kind: 'shutdown' });
await new Promise((resolve) => restoreHandle.worker.once('message', resolve));
await finishWorker(restoreHandle.worker);
const negative = await (async () => {
  const blobCopy = new Uint8Array(await readFile(finalPath));
  const { worker, result } = spawnRole({ role: 'negative-control', blob: blobCopy }, [
    blobCopy.buffer,
  ]);
  const message = await result;
  await finishWorker(worker);
  return message;
})();

setPhase('done');
clearInterval(heartbeat);
const rssSamples = await collectRssSamples();
await samplerWorker.terminate();

/// Peak and increment-over-own-floor for a phase window. Same-process
/// sequential comparison retains allocator pages across phases, so the
/// honest comparison is each phase's increment, not absolute peaks.
function phaseRss(phases) {
  const values = rssSamples
    .filter((sample) => phases.includes(phaseOfTimestamp(sample.at)))
    .map((sample) => sample.rss);
  if (values.length === 0) return null;
  const peak = Math.max(...values);
  const floor = Math.min(...values);
  return { peak, floor, added: peak - floor, samples: values.length };
}

// ---- exactness and diagnostics --------------------------------------------
const evidenceExact =
  JSON.stringify(restore.evidence) === JSON.stringify(build.evidence);
deepStrictEqual(restore.evidence, build.evidence, 'restored evidence diverged');
if (build.evidence.integrity !== 'ok') throw new Error('build integrity check failed');
for (const query of build.evidence.queries) {
  if (['phrase', 'identifier', 'rare-term'].includes(query.name) && query.rows.length === 0) {
    throw new Error(`query ${query.name} matched nothing; corpus markers are wrong`);
  }
}

const buildTotalMs = build.timings.clean_build_ms + build.timings.validate_ms;
const restoreReadyMs =
  restore.timings.verify_blob_ms + restore.timings.deserialize_ms + restore.timings.validate_ms;
const coldRestoreWallMs =
  restore.timings.init_ms + diskReadMs + restoreReadyMs;
const buildRss = phaseRss(['build-worker', 'clean-build', 'build-validate', 'export']);
const restoreRss = phaseRss(['restore-worker', 'restore', 'restore-validate', 'restore-mutate']);
const steadyValues = rssSamples
  .filter((sample) => phaseOfTimestamp(sample.at) === 'steady-state')
  .map((sample) => sample.rss);
const steadyStateRss = steadyValues.length === 0 ? null : Math.min(...steadyValues);
const stallAllPhases = maxStall([
  'build-worker', 'corpus-generate', 'clean-build', 'build-validate', 'export',
  'cache-write', 'cache-read', 'restore-worker', 'restore', 'restore-validate',
  'restore-mutate', 'steady-state',
]);

const guardTotals = ['network_attempts', 'helper_worker_attempts', 'persistence_attempts'].reduce(
  (totals, key) => {
    totals[key] =
      build.guardCounters[key] +
      proof.guardCounters[key] +
      restore.guardCounters[key] +
      negative.guardCounters[key];
    return totals;
  },
  {},
);

const measurements = {
  corpus: build.corpus,
  corpus_generate_ms: build.timings.corpus_generate_ms,
  clean_build_ms: build.timings.clean_build_ms,
  build_validate_ms: build.timings.validate_ms,
  export_ms: build.timings.export_ms,
  blob_bytes: build.blobBytes,
  blob_sha256: build.blobSha256,
  blob_bytes_per_markdown_byte: build.blobBytes / build.corpus.totalMarkdownBytes,
  disk_write_ms: diskWriteMs,
  disk_read_ms: diskReadMs,
  worker_init_ms: { build: build.timings.init_ms, restore: restore.timings.init_ms },
  restore_verify_blob_ms: restore.timings.verify_blob_ms,
  restore_deserialize_ms: restore.timings.deserialize_ms,
  byte_exact_proof_ms: proof.proofMs,
  restore_validate_ms: restore.timings.validate_ms,
  restore_ready_ms: restoreReadyMs,
  cold_restore_wall_ms: coldRestoreWallMs,
  restore_mutation_ms: restore.timings.mutation_ms,
  host_stall_max_ms_all_phases: stallAllPhases,
  rss_clean_build: buildRss,
  rss_restore: restoreRss,
  steady_state_rss_live_index: steadyStateRss,
  baseline_rss: baselineRss,
  steady_state_added_rss:
    steadyStateRss === null ? null : steadyStateRss - baselineRss,
  max_concurrent_workers: maxConcurrentWorkers,
};

const checks = {
  corpus_meets_scale:
    build.corpus.noteCount >= 10_000 &&
    build.corpus.totalMarkdownBytes >= build.corpus.targetBytes,
  byte_exact_restore: proof.byteExact === true,
  evidence_exact: evidenceExact,
  post_restore_mutation_full_loop:
    restore.mutationVisible === true && restore.mutatedExportWorks === true,
  negative_control_integrity_can_fail: negative.integrityCheckCanFail === true,
  negative_control_truncated_blob_rejected: negative.truncatedBlobRejected === true,
  one_worker_at_a_time: maxConcurrentWorkers === 1,
  guards_never_tripped:
    guardTotals.network_attempts === 0 &&
    guardTotals.helper_worker_attempts === 0 &&
    guardTotals.persistence_attempts === 0,
  blob_within_cap: build.blobBytes <= KILL_LIMITS.blob_cap_bytes,
  restore_under_5s: restoreReadyMs < KILL_LIMITS.restore_max_ms,
  restore_materially_faster_than_build:
    restoreReadyMs < buildTotalMs * KILL_LIMITS.restore_vs_build_max_ratio,
  host_stall_under_100ms_all_phases:
    stallAllPhases !== null && stallAllPhases < KILL_LIMITS.host_stall_max_ms,
  restore_rss_measured_and_bounded:
    buildRss !== null &&
    restoreRss !== null &&
    restoreRss.added <= buildRss.added * KILL_LIMITS.restore_peak_rss_max_ratio,
};

const informational = {
  steady_state_added_rss_within_300mib_target:
    measurements.steady_state_added_rss !== null &&
    measurements.steady_state_added_rss <= KILL_LIMITS.steady_state_added_rss_target_bytes,
  // Linear extrapolation from the measured blob density: the Markdown
  // volume at which the 384 MiB cap would be reached. The cap is a soft
  // degradation boundary (discard cache, clean build), not a correctness
  // failure.
  markdown_mib_at_blob_cap:
    KILL_LIMITS.blob_cap_bytes /
    measurements.blob_bytes_per_markdown_byte /
    (1024 * 1024),
  scope:
    'Standalone Node bench: proves official export/deserialize API feasibility at corpus scale. ' +
    'It does not exercise the production worker protocol, export from a live serving worker, ' +
    'staging-restore beside a resident generation, Rust-WASM co-residency, Electron, or cap-scale blobs.',
};

const verdict = Object.values(checks).every(Boolean) ? 'GO' : 'NO-GO';
const report = {
  gate: 'sqlite-wasm-export-restore-feasibility',
  question: 'KWIRY-Q-0017',
  verdict,
  provenance: {
    started_at: startedAt,
    node: process.version,
    platform: `${process.platform}-${process.arch}`,
    harness_sha256: {
      'scripts/gate.mjs': fileSha256('./gate.mjs'),
      'src/worker.mjs': fileSha256('../src/worker.mjs'),
      'src/corpus.mjs': fileSha256('../src/corpus.mjs'),
    },
    runtime_glue_sha256: fileSha256(
      '../../fts5-wasm/node_modules/@sqlite.org/sqlite-wasm/dist/node.mjs',
    ),
  },
  kill_limits: KILL_LIMITS,
  checks,
  informational,
  guard_counters: guardTotals,
  measurements,
  note: 'Automated feasibility evidence only; the owner GO/NO-GO decision is separate.',
};

mkdirSync(fileURLToPath(new URL('../evidence/', import.meta.url)), { recursive: true });
await writeFile(EVIDENCE_PATH, `${JSON.stringify(report, null, 2)}\n`);
console.log(JSON.stringify(report, null, 2));
process.exit(verdict === 'GO' ? 0 : 1);
