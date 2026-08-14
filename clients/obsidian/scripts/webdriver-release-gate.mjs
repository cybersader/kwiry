// SPDX-FileCopyrightText: 2026 cybersader
// SPDX-License-Identifier: GPL-3.0-only

import { createHash } from "node:crypto";
import { spawn } from "node:child_process";
import { createReadStream, createWriteStream } from "node:fs";
import { createConnection } from "node:net";
import {
  chmod, copyFile, mkdir, mkdtemp, readFile, rename, rm, stat, writeFile,
} from "node:fs/promises";
import { basename, dirname, isAbsolute, resolve } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";
import { pipeline } from "node:stream/promises";
import { createGunzip } from "node:zlib";

import { describeProductionPackage } from "./production-package.mjs";
import { buildStoredZip, parseStoredZip } from "./stored-zip.mjs";
import {
  sanitizedGateFailure,
  validateWebdriverReleaseEvidence,
} from "./webdriver-release-gate-schema.mjs";

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const DEFAULT_MANIFEST = resolve(ROOT, "scripts/webdriver-release-gate-manifest.json");
const QUERY = "macro boundary";
const VBA_ORACLE = "kwiry-vba-payload-must-not-index";
const XLSM_PATH = "Spreadsheets/03-macro-boundary.xlsm";
const PLUGIN_ID = "kwiry-search";
const MAX_DOWNLOAD_BYTES = 160 * 1024 * 1024;
const DOWNLOAD_TIMEOUT_MS = 120_000;
const UI_TIMEOUT_MS = 90_000;
const SAFE_ENV_KEYS = new Set([
  "CI", "DISPLAY", "KWIRY_WEBDRIVER_RUNTIME_ASSETS", "LANG", "LC_ALL", "LD_LIBRARY_PATH", "PATH",
  "WAYLAND_DISPLAY", "XAUTHORITY",
]);
const RUNTIME_OUTPUT_TAIL_BYTES = 512;
const RUNTIME_OUTPUT_DRAIN_MS = 1_000;
const runtimeExitDiagnostics = new WeakMap();

export class WebdriverGateError extends Error {
  constructor(code) {
    super(code);
    this.name = "WebdriverGateError";
    this.code = code;
  }
}

export function validateRuntimeManifest(value, packageJson = {}) {
  const root = exactObject(value, ["schema_version", "platform", "dependencies", "runtime", "artifacts", "derived"]);
  requireEqual(root.schema_version, 1, "runtime_manifest_invalid");
  requireEqual(root.platform, "linux-x64-xvfb", "runtime_manifest_invalid");
  const dependencies = exactObject(root.dependencies, ["node", "obsidian_launcher", "selenium_webdriver"]);
  for (const key of Object.keys(dependencies)) requireVersion(dependencies[key]);
  if (packageJson.devDependencies) {
    requireEqual(packageJson.devDependencies["obsidian-launcher"], dependencies.obsidian_launcher, "runtime_manifest_invalid");
    requireEqual(packageJson.devDependencies["selenium-webdriver"], dependencies.selenium_webdriver, "runtime_manifest_invalid");
  }
  const runtime = exactObject(root.runtime, [
    "obsidian_app", "obsidian_installer", "electron", "chromium", "chromedriver",
  ]);
  for (const value of Object.values(runtime)) requireVersion(value);
  requireEqual(runtime.chromedriver, runtime.chromium, "runtime_manifest_invalid");
  const artifacts = exactObject(root.artifacts, ["obsidian_installer", "obsidian_app", "chromedriver"]);
  for (const artifact of Object.values(artifacts)) {
    const checked = exactObject(artifact, ["url", "bytes", "sha256"]);
    if (typeof checked.url !== "string" || !checked.url.startsWith("https://")
      || !Number.isInteger(checked.bytes) || checked.bytes < 1 || checked.bytes > MAX_DOWNLOAD_BYTES
      || !/^[a-f0-9]{64}$/u.test(checked.sha256)) {
      throw new WebdriverGateError("runtime_manifest_invalid");
    }
  }
  if (!artifacts.obsidian_installer.url.includes(`/v${runtime.obsidian_installer}/`)
    || !artifacts.obsidian_app.url.includes(`/v${runtime.obsidian_app}/`)
    || !artifacts.chromedriver.url.includes(`/v${runtime.electron}/`)) {
    throw new WebdriverGateError("runtime_manifest_invalid");
  }
  const derived = exactObject(root.derived, ["obsidian_app_asar", "chromedriver_binary"]);
  for (const identity of Object.values(derived)) {
    const checked = exactObject(identity, ["bytes", "sha256"]);
    if (!Number.isInteger(checked.bytes) || checked.bytes < 1 || !/^[a-f0-9]{64}$/u.test(checked.sha256)) {
      throw new WebdriverGateError("runtime_manifest_invalid");
    }
  }
  return root;
}

export async function downloadPinnedArtifact(artifact, destination, {
  fetchImpl = fetch,
} = {}) {
  const controller = new AbortController();
  const timer = setTimeout(() => controller.abort(), DOWNLOAD_TIMEOUT_MS);
  try {
    const first = await fetchImpl(artifact.url, {
      redirect: "manual",
      signal: controller.signal,
      headers: { "user-agent": "kwiry-release-gate/1" },
    });
    if (![301, 302, 303, 307, 308].includes(first.status)) {
      throw new WebdriverGateError("download_identity_mismatch");
    }
    const location = first.headers.get("location");
    if (!location) throw new WebdriverGateError("download_identity_mismatch");
    const redirected = new URL(location, artifact.url);
    if (redirected.protocol !== "https:"
      || redirected.hostname !== "release-assets.githubusercontent.com") {
      throw new WebdriverGateError("download_identity_mismatch");
    }
    const response = await fetchImpl(redirected, {
      redirect: "error",
      signal: controller.signal,
      headers: { "user-agent": "kwiry-release-gate/1" },
    });
    if (!response.ok || !response.body) throw new WebdriverGateError("download_identity_mismatch");
    const declaredHeader = response.headers.get("content-length");
    const declared = declaredHeader === null ? null : Number(declaredHeader);
    if (declared !== null && (!Number.isFinite(declared) || declared !== artifact.bytes)) {
      throw new WebdriverGateError("download_identity_mismatch");
    }
    let received = 0;
    const hash = createHash("sha256");
    const stream = new TransformStream({
      transform(chunk, streamController) {
        received += chunk.byteLength;
        if (received > artifact.bytes || received > MAX_DOWNLOAD_BYTES) {
          throw new WebdriverGateError("download_identity_mismatch");
        }
        hash.update(chunk);
        streamController.enqueue(chunk);
      },
    });
    await pipeline(response.body.pipeThrough(stream), createWriteStream(destination));
    if (received !== artifact.bytes || hash.digest("hex") !== artifact.sha256) {
      throw new WebdriverGateError("download_identity_mismatch");
    }
    return destination;
  } catch (error) {
    if (error instanceof WebdriverGateError) throw error;
    throw new WebdriverGateError("download_identity_mismatch");
  } finally {
    clearTimeout(timer);
  }
}

export function buildSyntheticXlsm() {
  const xml = (value) => Buffer.from(value, "utf8");
  return buildStoredZip([
    { name: "[Content_Types].xml", bytes: xml(`<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
<Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>
<Default Extension="xml" ContentType="application/xml"/>
<Default Extension="bin" ContentType="application/vnd.ms-office.vbaProject"/>
<Override PartName="/xl/workbook.xml" ContentType="application/vnd.ms-excel.sheet.macroEnabled.main+xml"/>
<Override PartName="/xl/worksheets/sheet1.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml"/>
<Override PartName="/xl/sharedStrings.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sharedStrings+xml"/>
</Types>`) },
    { name: "_rels/.rels", bytes: xml(`<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
<Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="xl/workbook.xml"/>
</Relationships>`) },
    { name: "xl/workbook.xml", bytes: xml(`<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships">
<sheets><sheet name="Boundary" sheetId="1" r:id="rId1"/></sheets>
</workbook>`) },
    { name: "xl/_rels/workbook.xml.rels", bytes: xml(`<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
<Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet1.xml"/>
<Relationship Id="rId2" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/sharedStrings" Target="sharedStrings.xml"/>
<Relationship Id="rId3" Type="http://schemas.microsoft.com/office/2006/relationships/vbaProject" Target="vbaProject.bin"/>
</Relationships>`) },
    { name: "xl/sharedStrings.xml", bytes: xml(`<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<sst xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" count="1" uniqueCount="1"><si><t>${QUERY}</t></si></sst>`) },
    { name: "xl/worksheets/sheet1.xml", bytes: xml(`<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><sheetData><row r="1"><c r="A1" t="s"><v>0</v></c></row></sheetData></worksheet>`) },
    { name: "xl/vbaProject.bin", bytes: Buffer.from(`opaque:${VBA_ORACLE}`, "utf8") },
  ]);
}

export function buildEvidence({ candidate, manifest, manifestSha256, observed, cleanup }) {
  return validateWebdriverReleaseEvidence({
    schema_version: 1,
    kind: "kwiry_obsidian_webdriver_release_gate",
    verdict: "SELENIUM_RELEASE_GATE_PASSED",
    scope: "narrow_real_obsidian_selection_lifecycle",
    candidate: {
      version: candidate.version,
      candidate_set_sha256: candidate.candidate_set_sha256,
      file_count: candidate.file_count,
    },
    runtime_manifest: { sha256: manifestSha256 },
    runtime: {
      obsidian: manifest.runtime.obsidian_app,
      electron: observed.electron,
      chromium: observed.chromium,
      driver: observed.driver,
      selenium_webdriver: manifest.dependencies.selenium_webdriver,
      obsidian_launcher: manifest.dependencies.obsidian_launcher,
      node: process.versions.node,
      platform: manifest.platform,
    },
    isolation: {
      private_state_root: true,
      loopback_cdp: true,
      loopback_webdriver: true,
      selenium_manager_used: false,
      system_browser_used: false,
      system_driver_used: false,
    },
    scenario: {
      synthetic_xlsm: true,
      excel_explicitly_enabled: true,
      command_palette_used: true,
      webdriver_input_used: true,
      native_click_used: true,
      modal_closed: observed.modalClosed,
      stale_notices: observed.staleNotices,
      open_failure_notices: observed.openFailureNotices,
      open_file_calls: observed.openFileCalls,
      open_file_promise: observed.openFilePromise,
      expected_file_active: observed.expectedFileActive,
      vba_payload_search_results: observed.vbaPayloadSearchResults,
    },
    cleanup,
    privacy: {
      aggregate_only: true,
      paths_emitted: 0,
      queries_emitted: 0,
      note_content_emitted: 0,
      notice_text_emitted: 0,
      raw_logs_emitted: 0,
      screenshots_emitted: 0,
      stack_traces_emitted: 0,
    },
  });
}

export async function runWebdriverReleaseGate(options, adapters = {}) {
  const deps = defaultAdapters(adapters);
  const args = validateOptions(options);
  let manifest;
  let candidate;
  let manifestBytes;
  try {
    const packageJson = JSON.parse(await readFile(resolve(ROOT, "package.json"), "utf8"));
    manifestBytes = await readFile(args.manifest);
    manifest = validateRuntimeManifest(JSON.parse(manifestBytes), packageJson);
    candidate = await describeProductionPackage({ packageRoot: args.candidate });
    requireEqual(candidate.version, args.tag, "candidate_invalid");
  } catch (error) {
    if (error instanceof WebdriverGateError) throw error;
    throw new WebdriverGateError("candidate_invalid");
  }
  const privateRoot = await deps.createPrivateRoot();
  const cleanup = {
    webdriver_quit: false,
    obsidian_reaped: false,
    verified_download_server_closed: true,
    ports_closed: false,
    private_state_removed: false,
  };
  const state = {
    driver: null,
    proc: null,
    ports: [],
    oldCwd: process.cwd(),
    oldEnv: { ...process.env },
  };
  let primaryError = null;
  let passed = null;
  try {
    const layout = await preparePrivateLayout(privateRoot);
    process.chdir(privateRoot);
    replaceEnvironment(isolatedEnvironment(layout));
    await gateStage(() => deps.prepareRuntime(layout, manifest, deps), "runtime_prepare_failed");
    await gateStage(() => deps.prepareVault(layout.vault, args.candidate), "vault_prepare_failed");
    const launched = await gateStage(() => deps.launch({ layout, manifest }), "launch_failed");
    state.proc = launched.proc;
    const cdpPort = await deps.waitForCdpPort(launched.configDir, launched.proc);
    if (!isLoopbackPort(cdpPort)) throw new WebdriverGateError("webdriver_attach_failed");
    state.ports.push(cdpPort);
    const attached = await deps.attach({ layout, manifest, cdpPort });
    state.driver = attached.driver ?? attached;
    if (attached.webdriverPort) state.ports.push(attached.webdriverPort);
    const observed = await deps.exercise({ driver: state.driver, manifest });
    assertObserved(observed, manifest);
    passed = { candidate, manifest, manifestSha256: sha256(manifestBytes), observed, cleanup };
  } catch (error) {
    primaryError = normalizeGateError(error);
    throw primaryError;
  } finally {
    let cleanupFailed = false;
    try {
      if (state.driver) await state.driver.quit();
      cleanup.webdriver_quit = true;
    } catch { cleanupFailed = true; }
    try {
      if (state.proc) await deps.reap(state.proc);
      cleanup.obsidian_reaped = !state.proc || state.proc.exitCode !== null;
    } catch { cleanupFailed = true; }
    try {
      cleanup.ports_closed = await deps.portsClosed(state.ports);
      if (!cleanup.ports_closed) cleanupFailed = true;
    } catch { cleanupFailed = true; }
    try { process.chdir(state.oldCwd); } catch { cleanupFailed = true; }
    try { replaceEnvironment(state.oldEnv); } catch { cleanupFailed = true; }
    try {
      await rm(privateRoot, { recursive: true, force: true });
      cleanup.private_state_removed = true;
    } catch { cleanupFailed = true; }
    if ((cleanupFailed || !Object.values(cleanup).every(Boolean)) && !primaryError) {
      throw new WebdriverGateError("cleanup_incomplete");
    }
  }
  if (passed === null) throw primaryError ?? new WebdriverGateError("unexpected_failure");
  return passed;
}

async function main() {
  const options = parseArgs(process.argv.slice(2));
  let result;
  try {
    result = await runWebdriverReleaseGate(options);
    const evidence = buildEvidence(result);
    await writeFile(options.evidence, `${JSON.stringify(evidence, null, 2)}\n`, { flag: "wx" });
    process.stdout.write(`${JSON.stringify({ status: "passed", evidence: "webdriver.evidence.json" })}\n`);
  } catch (error) {
    process.stderr.write(`${JSON.stringify(sanitizedGateFailure(normalizeGateError(error).code))}\n`);
    process.exitCode = 1;
  }
}

export async function createWebdriverPrivateRoot() {
  const configuredRoot = process.env.KWIRY_WEBDRIVER_PRIVATE_ROOT;
  const parent = configuredRoot ? resolve(configuredRoot) : resolve(ROOT, ".tmp", "webdriver-private");
  await mkdir(parent, { recursive: true, mode: 0o700 });
  await chmod(parent, 0o700);
  const path = await mkdtemp(resolve(parent, "kwiry-webdriver-release-"));
  await chmod(path, 0o700);
  return path;
}

function defaultAdapters(overrides) {
  return {
    createPrivateRoot: createWebdriverPrivateRoot,
    download: downloadPinnedArtifact,
    prepareRuntime: prepareVerifiedRuntime,
    prepareVault,
    launch: launchPinnedObsidian,
    waitForCdpPort,
    attach: attachWebdriver,
    exercise: exerciseObsidian,
    reap: reapProcess,
    portsClosed: verifyPortsClosed,
    ...overrides,
  };
}

export async function prepareVerifiedRuntime(layout, manifest, deps) {
  const downloads = {};
  const preparedRoot = process.env.KWIRY_WEBDRIVER_RUNTIME_ASSETS;
  for (const [name, artifact] of Object.entries(manifest.artifacts)) {
    const filename = basename(new URL(artifact.url).pathname);
    const path = resolve(layout.downloads, filename);
    if (preparedRoot) {
      await copyFile(resolve(preparedRoot, filename), path);
      await requireFileIdentity(path, artifact);
      downloads[name] = path;
    } else {
      downloads[name] = await deps.download(artifact, path);
    }
  }
  const appAsar = resolve(layout.cache, "obsidian-app", `obsidian-${manifest.runtime.obsidian_app}.asar`);
  await mkdir(dirname(appAsar), { recursive: true });
  await pipeline(createReadStream(downloads.obsidian_app), createGunzip(), createWriteStream(appAsar));
  await requireFileIdentity(appAsar, manifest.derived.obsidian_app_asar);

  const installerRow = {
    version: manifest.runtime.obsidian_installer,
    minInstallerVersion: manifest.runtime.obsidian_installer,
    maxInstallerVersion: manifest.runtime.obsidian_installer,
    isBeta: false,
    downloads: { appImage: manifest.artifacts.obsidian_installer.url },
    installers: { appImage: {
      digest: `sha256:${manifest.artifacts.obsidian_installer.sha256}`,
      electron: manifest.runtime.electron,
      chrome: manifest.runtime.chromium,
      platforms: ["linux-x64"],
    } },
  };
  const appRow = {
    version: manifest.runtime.obsidian_app,
    minInstallerVersion: manifest.runtime.obsidian_installer,
    maxInstallerVersion: manifest.runtime.obsidian_installer,
    isBeta: false,
    downloads: { asar: manifest.artifacts.obsidian_app.url },
    installers: {},
  };
  const versions = {
    metadata: {
      schemaVersion: "2.0.0", commitDate: "1970-01-01T00:00:00Z",
      commitSha: "0000000000000000000000000000000000000000", timestamp: "1970-01-01T00:00:00Z",
    },
    versions: manifest.runtime.obsidian_app === manifest.runtime.obsidian_installer
      ? [{
          ...appRow,
          downloads: { ...appRow.downloads, ...installerRow.downloads },
          installers: installerRow.installers,
        }]
      : [installerRow, appRow],
  };
  await writeFile(layout.versions, `${JSON.stringify(versions)}\n`);
  if (preparedRoot) {
    await copyFile(resolve(preparedRoot, "chromedriver"), layout.driver);
  } else {
    await extractChromedriver(downloads.chromedriver, layout.driver);
  }
  await requireFileIdentity(layout.driver, manifest.derived.chromedriver_binary);
  await chmod(layout.driver, 0o700);
  layout.appAsar = appAsar;
  layout.installerArchive = downloads.obsidian_installer;
  layout.installerExecutable = resolve(
    layout.cache,
    "obsidian-installer",
    "linux-x64",
    `Obsidian-${manifest.runtime.obsidian_installer}`,
    "obsidian",
  );
}

async function launchPinnedObsidian({ layout, manifest }) {
  const Launcher = (await import("obsidian-launcher")).default;
  const launcher = new Launcher({
    cacheDir: layout.cache,
    versionsUrl: pathToFileURL(layout.versions).toString(),
    cacheDuration: Number.MAX_SAFE_INTEGER,
    interactive: false,
  });
  await gateStage(() => preparePinnedInstaller(layout), "installer_prepare_failed");
  const [appVersion, installerVersion] = await gateStage(
    () => launcher.resolveVersion(manifest.runtime.obsidian_app, manifest.runtime.obsidian_installer),
    "launcher_resolve_failed",
  );
  const appPath = await gateStage(
    () => launcher.downloadApp(appVersion),
    "launcher_app_cache_failed",
  );
  const installerPath = await gateStage(
    () => launcher.downloadInstaller(installerVersion),
    "launcher_installer_cache_failed",
  );
  const vault = await gateStage(
    () => launcher.setupVault({ vault: layout.vault, copy: false }),
    "launcher_vault_setup_failed",
  );
  const configDir = await gateStage(
    () => launcher.setupConfigDir({ appVersion, installerVersion, appPath, vault }),
    "launcher_config_setup_failed",
  );
  const proc = spawn(installerPath, [
    `--user-data-dir=${configDir}`,
    "--no-sandbox",
    "--remote-debugging-address=127.0.0.1",
    "--remote-debugging-port=0",
    "--test-type=webdriver",
    "--tag=obsidian-launcher",
  ], {
    detached: true,
    stdio: ["ignore", "pipe", "pipe"],
    env: process.env,
  });
  trackRuntimeExitDiagnostics(proc);
  await gateStage(() => waitForSpawn(proc), "launcher_spawn_failed");
  return { proc, configDir, vault };
}

export async function preparePinnedInstaller(layout) {
  const installerDirectory = dirname(layout.installerExecutable);
  const extraction = `${installerDirectory}.extract`;
  const extractedTree = resolve(extraction, "squashfs-root");
  await rm(extraction, { recursive: true, force: true });
  await mkdir(extraction, { recursive: true });
  try {
    await chmod(layout.installerArchive, 0o700);
    await runQuietProcess(layout.installerArchive, ["--appimage-extract"], extraction);
    const extractedExecutable = resolve(extractedTree, "obsidian");
    const executable = await stat(extractedExecutable);
    if (!executable.isFile() || executable.size === 0) throw new Error("installer executable empty");
    await mkdir(dirname(installerDirectory), { recursive: true });
    await rm(installerDirectory, { recursive: true, force: true });
    await rename(extractedTree, installerDirectory);
  } finally {
    await rm(extraction, { recursive: true, force: true });
  }
}

async function runQuietProcess(command, args, cwd) {
  await new Promise((resolveProcess, rejectProcess) => {
    const proc = spawn(command, args, { cwd, stdio: "ignore" });
    proc.once("error", rejectProcess);
    proc.once("close", (code) => code === 0
      ? resolveProcess()
      : rejectProcess(new Error("runtime extractor failed")));
  });
}

async function waitForSpawn(proc) {
  await new Promise((resolveSpawn, rejectSpawn) => {
    proc.once("spawn", resolveSpawn);
    proc.once("error", rejectSpawn);
  });
}

export function trackRuntimeExitDiagnostics(proc) {
  let resolveComplete;
  const complete = new Promise((resolveDiagnostics) => {
    resolveComplete = resolveDiagnostics;
  });
  const diagnostics = { stage: null, tail: "", complete };
  runtimeExitDiagnostics.set(proc, diagnostics);
  const streams = [proc.stdout, proc.stderr].filter(Boolean);
  if (streams.length === 0) {
    resolveComplete();
    return;
  }
  let pending = streams.length;
  for (const stream of streams) {
    let finished = false;
    const finish = () => {
      if (finished) return;
      finished = true;
      pending -= 1;
      if (pending === 0) resolveComplete();
    };
    stream.on("data", (chunk) => {
      if (diagnostics.stage) return;
      const decoded = chunk.toString("utf8");
      const bounded = decoded.length <= 4_096
        ? decoded
        : `${decoded.slice(0, 2_048)}${decoded.slice(-2_048)}`;
      const sample = `${diagnostics.tail}${bounded}`;
      diagnostics.stage = classifyRuntimeOutput(sample);
      diagnostics.tail = sample.slice(-RUNTIME_OUTPUT_TAIL_BYTES);
    });
    stream.once("end", finish);
    stream.once("close", finish);
    stream.once("error", finish);
  }
}

export function classifyRuntimeOutput(value) {
  if (/error while loading shared libraries|cannot open shared object file/iu.test(value)) {
    return "launch_dependency_missing";
  }
  if (/missing x server|failed to connect to the display|platform failed to initialize/iu.test(value)) {
    return "launch_display_unavailable";
  }
  if (/no usable sandbox|suid sandbox|running as root without --no-sandbox|apparmor_restrict_unprivileged_userns/iu.test(value)) {
    return "launch_sandbox_unavailable";
  }
  if (/gpu process (?:isn't usable|launch failed)|failed to launch gpu process/iu.test(value)) {
    return "launch_gpu_unavailable";
  }
  if (/processsingleton|singleton lock|another instance is already running/iu.test(value)) {
    return "launch_instance_conflict";
  }
  if (/crashpad_handler: --database is required|failed to (?:initialize|start) crashpad|crash reporter.*failed/iu.test(value)) {
    return "launch_crash_reporter_unavailable";
  }
  if (/invalid file descriptor to icu data|failed to load (?:icu data|resources\.pak)|unable to load locale/iu.test(value)) {
    return "launch_runtime_resources_unavailable";
  }
  if (/\bglib-(?:error|gio-error)\b|gdk-.*error/iu.test(value)) {
    return "launch_platform_runtime_failed";
  }
  if (/\bfatal:/iu.test(value)) return "launch_runtime_fatal";
  return null;
}

export function classifyRuntimeProcessExit(proc) {
  const diagnostic = runtimeExitDiagnostics.get(proc)?.stage;
  if (diagnostic) return diagnostic;
  const signalStages = new Map([
    ["SIGABRT", "launch_process_aborted"],
    ["SIGBUS", "launch_process_bus_error"],
    ["SIGFPE", "launch_process_arithmetic_fault"],
    ["SIGILL", "launch_process_illegal_instruction"],
    ["SIGKILL", "launch_process_killed"],
    ["SIGSEGV", "launch_process_segmentation_fault"],
    ["SIGTERM", "launch_process_terminated"],
    ["SIGTRAP", "launch_process_trapped"],
  ]);
  if (proc.signalCode !== null) {
    return signalStages.get(proc.signalCode) ?? "launch_process_signaled";
  }
  if (proc.exitCode === 0) return "launch_process_clean_exit";
  return "launch_process_error_exit";
}

export async function awaitRuntimeProcessExitStage(proc) {
  const complete = runtimeExitDiagnostics.get(proc)?.complete;
  if (complete) {
    await Promise.race([
      complete,
      new Promise((resolveDrain) => setTimeout(resolveDrain, RUNTIME_OUTPUT_DRAIN_MS)),
    ]);
  }
  return classifyRuntimeProcessExit(proc);
}

async function attachWebdriver({ layout, manifest, cdpPort }) {
  const chrome = await import("selenium-webdriver/chrome.js");
  const service = new chrome.ServiceBuilder(layout.driver)
    .setLoopback(true)
    .setPort(0)
    .setEnvironment({ ...process.env })
    .setStdio("ignore")
    .build();
  const configured = new chrome.Options().debuggerAddress(`127.0.0.1:${cdpPort}`);
  const driver = chrome.Driver.createSession(configured, service);
  const capabilities = await driver.getCapabilities();
  const actual = capabilities.get("chrome")?.chromedriverVersion?.split(" ")[0];
  requireEqual(actual, manifest.runtime.chromedriver, "webdriver_attach_failed");
  const webdriverUrl = new URL(await service.address());
  if (webdriverUrl.hostname !== "127.0.0.1" && webdriverUrl.hostname !== "localhost") {
    await driver.quit();
    throw new WebdriverGateError("webdriver_attach_failed");
  }
  return { driver, webdriverPort: Number(webdriverUrl.port) };
}

async function exerciseObsidian({ driver, manifest }) {
  const selenium = await import("selenium-webdriver");
  const { By, Key, until } = selenium;
  await driver.wait(async () => driver.executeScript(`return Boolean(window.app?.plugins?.enabledPlugins?.has(${JSON.stringify(PLUGIN_ID)}));`), UI_TIMEOUT_MS);
  await driver.executeScript(`
    (() => {
      const state = { calls: 0, promise: "none", stale: 0, failure: 0 };
      const leafProto = Object.getPrototypeOf(window.app.workspace.getLeaf(false));
      const originalOpenFile = leafProto.openFile;
      leafProto.openFile = function(file, options) {
        state.calls += 1;
        try {
          const returned = originalOpenFile.call(this, file, options);
          Promise.resolve(returned).then(() => { state.promise = "resolved"; }, () => { state.promise = "rejected"; });
          return returned;
        } catch (error) {
          state.promise = "rejected";
          throw error;
        }
      };
      const observer = new MutationObserver(() => {
        for (const node of document.querySelectorAll('.notice')) {
          const text = node.textContent ?? '';
          if (text === 'Kwiry: these search results are out of date. Wait for the refreshed results.') state.stale += 1;
          if (text === 'Kwiry: Obsidian could not open this file.') state.failure += 1;
        }
      });
      observer.observe(document.body, { childList: true, subtree: true });
      window.__kwiryWebdriverGate = state;
    })();
  `);
  await driver.actions().keyDown(Key.CONTROL).sendKeys("p").keyUp(Key.CONTROL).perform();
  const commandInput = await driver.wait(until.elementLocated(By.css(".prompt-input")), UI_TIMEOUT_MS);
  await commandInput.sendKeys("Kwiry Search: Search notes");
  const command = await driver.wait(until.elementLocated(By.xpath("//*[contains(@class,'suggestion-item') and contains(.,'Kwiry Search: Search notes')]")), UI_TIMEOUT_MS);
  await command.click();
  const queryInput = await driver.wait(until.elementLocated(By.css(".prompt-input")), UI_TIMEOUT_MS);
  await queryInput.sendKeys(QUERY);
  const row = await driver.wait(until.elementLocated(By.xpath("//*[contains(@class,'suggestion-item') and contains(.,'03-macro-boundary.xlsm')]")), UI_TIMEOUT_MS);
  await row.click();
  await driver.wait(async () => !(await driver.findElements(By.css(".modal-container"))).length, UI_TIMEOUT_MS);
  await driver.actions().keyDown(Key.CONTROL).sendKeys("p").keyUp(Key.CONTROL).perform();
  const secondCommandInput = await driver.wait(until.elementLocated(By.css(".prompt-input")), UI_TIMEOUT_MS);
  await secondCommandInput.sendKeys("Kwiry Search: Search notes");
  const secondCommand = await driver.wait(until.elementLocated(By.xpath("//*[contains(@class,'suggestion-item') and contains(.,'Kwiry Search: Search notes')]")), UI_TIMEOUT_MS);
  await secondCommand.click();
  const vbaInput = await driver.wait(until.elementLocated(By.css(".prompt-input")), UI_TIMEOUT_MS);
  await vbaInput.sendKeys(VBA_ORACLE);
  await driver.sleep(1_000);
  const vbaPayloadSearchResults = (await driver.findElements(By.css(".suggestion-item"))).length;
  await driver.actions().sendKeys(Key.ESCAPE).perform();
  const observation = await driver.executeScript(`
    return {
      ...window.__kwiryWebdriverGate,
      modalClosed: !document.querySelector('.modal-container'),
      expectedFileActive: window.app.workspace.getActiveFile()?.path === ${JSON.stringify(XLSM_PATH)},
      electron: process.versions.electron,
      chromium: process.versions.chrome,
    };
  `);
  const capabilities = await driver.getCapabilities();
  return {
    modalClosed: observation.modalClosed,
    staleNotices: observation.stale,
    openFailureNotices: observation.failure,
    openFileCalls: observation.calls,
    openFilePromise: observation.promise,
    expectedFileActive: observation.expectedFileActive,
    vbaPayloadSearchResults: Number(vbaPayloadSearchResults),
    electron: observation.electron,
    chromium: observation.chromium,
    driver: capabilities.get("chrome")?.chromedriverVersion?.split(" ")[0],
  };
}

export function assertObserved(observed, manifest) {
  requireEqual(observed.electron, manifest.runtime.electron, "launch_failed");
  requireEqual(observed.chromium, manifest.runtime.chromium, "launch_failed");
  requireEqual(observed.driver, manifest.runtime.chromedriver, "webdriver_attach_failed");
  if (observed.staleNotices !== 0) throw new WebdriverGateError("stale_notice_observed");
  if (observed.openFileCalls !== 1) throw new WebdriverGateError("open_not_invoked");
  if (observed.openFilePromise !== "resolved" || !observed.expectedFileActive) {
    throw new WebdriverGateError("open_promise_rejected");
  }
  if (!observed.modalClosed || observed.openFailureNotices !== 0 || observed.vbaPayloadSearchResults !== 0) {
    throw new WebdriverGateError("result_not_rendered");
  }
}

async function preparePrivateLayout(privateRoot) {
  const layout = {
    root: privateRoot,
    home: resolve(privateRoot, "home"),
    tmp: resolve(privateRoot, "tmp"),
    config: resolve(privateRoot, "config"),
    cache: resolve(privateRoot, "cache"),
    data: resolve(privateRoot, "data"),
    downloads: resolve(privateRoot, "downloads"),
    vault: resolve(privateRoot, "vault"),
    plugin: resolve(privateRoot, "plugin"),
    versions: resolve(privateRoot, "versions.json"),
    driver: resolve(privateRoot, "driver", "chromedriver"),
  };
  await Promise.all(Object.values(layout).filter((value) => typeof value === "string" && !value.endsWith(".json") && !value.endsWith("chromedriver")).map((path) => mkdir(path, { recursive: true })));
  await mkdir(dirname(layout.driver), { recursive: true });
  return layout;
}

export async function prepareVault(vault, candidate) {
  const pluginDir = resolve(vault, ".obsidian/plugins", PLUGIN_ID);
  await mkdir(pluginDir, { recursive: true });
  for (const name of ["main.js", "manifest.json", "styles.css"]) {
    await copyFile(resolve(candidate, name), resolve(pluginDir, name));
  }
  await writeFile(resolve(pluginDir, "data.json"), `${JSON.stringify({
    backendProfile: "in_plugin", daemonUrl: "http://127.0.0.1:32189", tokenFilePath: "",
    defaultMode: "lexical", resultLimit: 20, vaultId: "", daemonCurrentVaultId: "",
    showRibbonIcon: true,
    enabledSourceFormats: {
      markdown: true, text: true, base: true, canvas: true, docx: true,
      pdf: false, excalidraw: true, excel: true,
    },
    diagnosticsLogLevel: "off", diagnosticsReportLevel: "error",
    diagnosticsReportScope: "failures", diagnosticsReportDetail: "compact",
  }, null, 2)}\n`);
  await mkdir(resolve(vault, ".obsidian"), { recursive: true });
  await writeFile(resolve(vault, ".obsidian/community-plugins.json"), `${JSON.stringify([PLUGIN_ID])}\n`);
  await mkdir(resolve(vault, dirname(XLSM_PATH)), { recursive: true });
  await writeFile(resolve(vault, XLSM_PATH), buildSyntheticXlsm());
}

function isolatedEnvironment(layout) {
  const env = {};
  for (const key of SAFE_ENV_KEYS) if (process.env[key]) env[key] = process.env[key];
  return {
    ...env,
    HOME: layout.home,
    TMPDIR: layout.tmp,
    XDG_CONFIG_HOME: layout.config,
    XDG_CACHE_HOME: layout.cache,
    XDG_DATA_HOME: layout.data,
    OBSIDIAN_CACHE: layout.cache,
  };
}

function replaceEnvironment(next) {
  for (const key of Object.keys(process.env)) delete process.env[key];
  Object.assign(process.env, next);
}

async function extractChromedriver(archivePath, output) {
  const entries = parseStoredZip(await readFile(archivePath), { maxEntries: 8, maxEntryBytes: 128 * 1024 * 1024 });
  const drivers = entries.filter(({ name }) => name === "chromedriver");
  if (drivers.length !== 1) throw new WebdriverGateError("download_identity_mismatch");
  await writeFile(output, drivers[0].bytes, { flag: "wx", mode: 0o700 });
}

async function waitForCdpPort(configDir, proc) {
  const path = resolve(configDir, "DevToolsActivePort");
  const deadline = Date.now() + UI_TIMEOUT_MS;
  while (Date.now() < deadline) {
    if (proc.exitCode !== null || proc.signalCode !== null) {
      throw new WebdriverGateError(await awaitRuntimeProcessExitStage(proc));
    }
    try {
      const [port] = (await readFile(path, "utf8")).trim().split("\n");
      const parsed = Number(port);
      if (isLoopbackPort(parsed)) return parsed;
    } catch {}
    await new Promise((resolve) => setTimeout(resolve, 100));
  }
  throw new WebdriverGateError("cdp_ready_timeout");
}

function isLoopbackPort(port) {
  return Number.isInteger(port) && port >= 1 && port <= 65_535;
}

async function reapProcess(proc) {
  if (proc.exitCode !== null) return;
  try { process.kill(-proc.pid, "SIGTERM"); } catch {}
  await Promise.race([
    new Promise((resolve) => proc.once("exit", resolve)),
    new Promise((resolve) => setTimeout(resolve, 5_000)),
  ]);
  if (proc.exitCode === null) {
    try { process.kill(-proc.pid, "SIGKILL"); } catch {}
    await Promise.race([
      new Promise((resolveExit) => proc.once("exit", resolveExit)),
      new Promise((resolveTimeout) => setTimeout(resolveTimeout, 5_000)),
    ]);
  }
  if (proc.exitCode === null) throw new WebdriverGateError("cleanup_incomplete");
}

async function verifyPortsClosed(ports) {
  for (const port of new Set(ports.filter(isLoopbackPort))) {
    if (await canConnectLoopback(port)) return false;
  }
  return true;
}

async function canConnectLoopback(port) {
  return new Promise((resolveConnection) => {
    const socket = createConnection({ host: "127.0.0.1", port });
    const finish = (connected) => {
      socket.destroy();
      resolveConnection(connected);
    };
    socket.setTimeout(500);
    socket.once("connect", () => finish(true));
    socket.once("timeout", () => finish(false));
    socket.once("error", () => finish(false));
  });
}

function validateOptions(options) {
  if (!options || typeof options.candidate !== "string" || typeof options.manifest !== "string"
    || typeof options.evidence !== "string" || !isAbsolute(options.candidate) || !isAbsolute(options.manifest)
    || !isAbsolute(options.evidence) || !/^[0-9]+\.[0-9]+\.[0-9]+(?:-[0-9A-Za-z.-]+)?$/u.test(options.tag)) {
    throw new WebdriverGateError("candidate_invalid");
  }
  return { ...options, candidate: resolve(options.candidate), manifest: resolve(options.manifest), evidence: resolve(options.evidence) };
}

function parseArgs(args) {
  const values = { manifest: DEFAULT_MANIFEST };
  for (let index = 0; index < args.length; index += 2) {
    const key = args[index];
    const value = args[index + 1];
    if (!value || !["--candidate", "--tag", "--manifest", "--evidence"].includes(key)) throw new WebdriverGateError("candidate_invalid");
    values[key.slice(2)] = key === "--tag" ? value : resolve(value);
  }
  return validateOptions(values);
}

async function gateStage(operation, code) {
  try {
    return await operation();
  } catch (error) {
    if (error instanceof WebdriverGateError) throw error;
    throw new WebdriverGateError(code);
  }
}

function normalizeGateError(error) {
  return error instanceof WebdriverGateError ? error : new WebdriverGateError("unexpected_failure");
}

function exactObject(value, keys) {
  if (typeof value !== "object" || value === null || Array.isArray(value)
    || JSON.stringify(Object.keys(value).sort()) !== JSON.stringify([...keys].sort())) {
    throw new WebdriverGateError("runtime_manifest_invalid");
  }
  return value;
}

function requireVersion(value) {
  if (typeof value !== "string" || !/^[0-9]+(?:\.[0-9]+){1,3}(?:-[0-9A-Za-z.-]+)?$/u.test(value)) {
    throw new WebdriverGateError("runtime_manifest_invalid");
  }
}

function requireEqual(actual, expected, code) {
  if (actual !== expected) throw new WebdriverGateError(code);
}

async function requireFileIdentity(path, expected) {
  const bytes = await readFile(path);
  if (bytes.byteLength !== expected.bytes || sha256(bytes) !== expected.sha256) {
    throw new WebdriverGateError("download_identity_mismatch");
  }
}

function sha256(value) {
  return createHash("sha256").update(value).digest("hex");
}

if (process.argv[1] === fileURLToPath(import.meta.url)) await main();
