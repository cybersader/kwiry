// SPDX-FileCopyrightText: 2026 cybersader
// SPDX-License-Identifier: GPL-3.0-only

import { createHash } from "node:crypto";
import { execFile } from "node:child_process";
import { chmod, mkdir, mkdtemp, readFile, rm, stat, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { dirname, resolve } from "node:path";
import { PassThrough } from "node:stream";
import { pathToFileURL } from "node:url";
import { promisify } from "node:util";
import { afterEach, describe, expect, it } from "vitest";

import {
  WebdriverGateError,
  assertObserved,
  awaitRuntimeProcessExitStage,
  buildEvidence,
  buildPinnedObsidianArgs,
  buildSyntheticXlsm,
  classifyFatalRuntimeOutput,
  classifyRuntimeProcessExit,
  classifyRuntimeOutput,
  createWebdriverPrivateRoot,
  createWebdriverRuntimeTempRoot,
  downloadPinnedArtifact,
  exerciseObsidian,
  pinnedInstallerLaunchPath,
  preparePinnedInstaller,
  prepareVerifiedRuntime,
  runWebdriverReleaseGate,
  trackRuntimeExitDiagnostics,
  validateRuntimeManifest,
} from "../scripts/webdriver-release-gate.mjs";
import { buildStoredZip, parseStoredZip } from "../scripts/stored-zip.mjs";

const repositoryRoot = resolve(import.meta.dirname, "..");
const manifestPath = resolve(repositoryRoot, "scripts/webdriver-release-gate-manifest.json");
const temporaryRoots = [];
const execFileAsync = promisify(execFile);
const HASH = "a".repeat(64);

async function createCandidateFixture() {
  const root = await mkdtemp(resolve(tmpdir(), "kwiry-webdriver-candidate-"));
  temporaryRoots.push(root);
  await mkdir(root, { recursive: true });
  for (const name of [
    "main.js", "manifest.json", "styles.css", "LICENSE", "THIRD_PARTY_NOTICES.md",
    "Apache-2.0.txt", "Emscripten-LICENSE.txt", "html-entity-provenance.json",
    "markup5ever-entities-MIT.txt", "Rust-DEPENDENCY-LICENSES.md",
    "gate5.evidence.json", "kwiry-search.zip", "SHA256SUMS",
  ]) await writeFile(resolve(root, name), name === "manifest.json"
    ? `${JSON.stringify({ id: "kwiry-search", version: "0.6.0-beta.15" })}\n`
    : `${name}\n`);
  return root;
}

function manifestFixture() {
  return {
    schema_version: 1,
    platform: "linux-x64-xvfb",
    dependencies: {
      node: process.versions.node,
      obsidian_launcher: "3.1.1",
      selenium_webdriver: "4.39.0",
    },
    runtime: {
      obsidian_app: "1.13.7",
      obsidian_installer: "1.13.7",
      electron: "43.3.0",
      chromium: "150.0.7871.212",
      chromedriver: "150.0.7871.212",
    },
    artifacts: {
      obsidian_installer: { url: "https://example.test/v1.13.7/installer", bytes: 1, sha256: HASH },
      obsidian_app: { url: "https://example.test/v1.13.7/app", bytes: 1, sha256: HASH },
      chromedriver: { url: "https://example.test/v43.3.0/driver", bytes: 1, sha256: HASH },
    },
    derived: {
      obsidian_app_asar: { bytes: 1, sha256: HASH },
      chromedriver_binary: { bytes: 1, sha256: HASH },
    },
  };
}

function observed() {
  return {
    modalClosed: true,
    staleNotices: 0,
    openFailureNotices: 0,
    openFileCalls: 1,
    openFilePromise: "resolved",
    expectedResultSelected: true,
    vbaPayloadSearchResults: 0,
    electron: "43.3.0",
    chromium: "150.0.7871.212",
    driver: "150.0.7871.212",
  };
}

afterEach(async () => {
  await Promise.all(temporaryRoots.splice(0).map((path) => rm(path, { recursive: true, force: true })));
});

describe("WebDriver release gate", () => {
  it("pins a closed reviewed runtime manifest to the package dependencies", async () => {
    const manifest = JSON.parse(await readFile(manifestPath, "utf8"));
    const packageJson = JSON.parse(await readFile(resolve(repositoryRoot, "package.json"), "utf8"));
    expect(validateRuntimeManifest(manifest, packageJson)).toEqual(manifest);
    expect(manifest.runtime).toEqual({
      obsidian_app: "1.13.7",
      obsidian_installer: "1.13.7",
      electron: "43.3.0",
      chromium: "150.0.7871.212",
      chromedriver: "150.0.7871.212",
    });
    expect(manifest.artifacts.obsidian_installer.url).not.toContain("latest");
    expect(manifest.artifacts.chromedriver.sha256).toMatch(/^[a-f0-9]{64}$/u);
  });

  it("runs the CLI entry point from a checkout path containing spaces", async () => {
    const root = await mkdtemp(resolve(tmpdir(), "kwiry webdriver cli "));
    temporaryRoots.push(root);
    const scriptDir = resolve(root, "scripts");
    await mkdir(scriptDir, { recursive: true });
    const script = resolve(scriptDir, "webdriver-release-gate.mjs");
    const modules = [
      "webdriver-release-gate.mjs", "production-package.mjs", "stored-zip.mjs",
      "webdriver-release-gate-schema.mjs", "gate5-evidence-schema.mjs", "privacy-policy.mjs",
    ];
    for (const name of modules) {
      await writeFile(resolve(scriptDir, name), await readFile(resolve(repositoryRoot, "scripts", name)));
    }
    await writeFile(resolve(root, "package.json"), await readFile(resolve(repositoryRoot, "package.json")));
    await expect(execFileAsync(process.execPath, [
      script,
      "--candidate", resolve(root, "candidate"),
      "--tag", "0.6.0-beta.16",
      "--manifest", resolve(root, "runtime.json"),
      "--evidence", resolve(root, "evidence.json"),
    ], { cwd: root })).rejects.toMatchObject({
      stderr: expect.stringContaining('"failure_stage":"candidate_invalid"'),
    });
  });

  it.each([
    ["moving app", (value) => { value.runtime.obsidian_app = "latest"; }],
    ["driver mismatch", (value) => { value.runtime.chromedriver = "150.0.7871.187"; }],
    ["dependency drift", (value) => { value.dependencies.selenium_webdriver = "4.40.0"; }],
    ["redirecting URL", (value) => { value.artifacts.obsidian_app.url = "https://example.test/latest/app"; }],
    ["unknown key", (value) => { value.runtime.extra = true; }],
  ])("rejects %s in the runtime manifest", (_name, mutate) => {
    const manifest = manifestFixture();
    mutate(manifest);
    expect(() => validateRuntimeManifest(manifest, {
      devDependencies: { "obsidian-launcher": "3.1.1", "selenium-webdriver": "4.39.0" },
    })).toThrow("runtime_manifest_invalid");
  });

  it("prepares the pinned runtime with a fresh gzip transform", async () => {
    const root = await mkdtemp(resolve(tmpdir(), "kwiry-webdriver-runtime-"));
    temporaryRoots.push(root);
    const layout = {
      downloads: resolve(root, "downloads"),
      cache: resolve(root, "cache"),
      versions: resolve(root, "versions.json"),
      driver: resolve(root, "driver", "chromedriver"),
    };
    await Promise.all([
      mkdir(layout.downloads, { recursive: true }),
      mkdir(layout.cache, { recursive: true }),
      mkdir(resolve(root, "driver"), { recursive: true }),
    ]);
    const manifest = manifestFixture();
    manifest.runtime.obsidian_installer = "1.13.4";
    const asar = Buffer.from("app-asar");
    const driver = Buffer.from("driver");
    manifest.derived.obsidian_app_asar = { bytes: asar.byteLength, sha256: sha256(asar) };
    manifest.derived.chromedriver_binary = { bytes: driver.byteLength, sha256: sha256(driver) };
    const { gzipSync } = await import("node:zlib");
    await prepareVerifiedRuntime(layout, manifest, {
      download: async (artifact, destination) => {
        const bytes = artifact === manifest.artifacts.obsidian_app
          ? gzipSync(asar)
          : artifact === manifest.artifacts.chromedriver
            ? buildStoredZip([{ name: "chromedriver", bytes: driver }])
            : Buffer.from("installer");
        await writeFile(destination, bytes);
        return destination;
      },
    });
    expect(await readFile(resolve(layout.cache, "obsidian-app", "obsidian-1.13.7.asar"))).toEqual(asar);
    const versions = JSON.parse(await readFile(layout.versions, "utf8"));
    expect(versions.metadata.schemaVersion).toBe("2.0.0");
    const Launcher = (await import("obsidian-launcher")).default;
    const launcher = new Launcher({
      cacheDir: layout.cache,
      versionsUrl: pathToFileURL(layout.versions).toString(),
      cacheDuration: Number.MAX_SAFE_INTEGER,
      interactive: false,
    });
    expect(await launcher.resolveVersion("1.13.7", "1.13.4")).toEqual(["1.13.7", "1.13.4"]);
    expect(await readFile(layout.driver)).toEqual(driver);
  });

  it("pre-extracts the pinned AppImage into the launcher cache without serving it", async () => {
    const root = await mkdtemp(resolve(tmpdir(), "kwiry-webdriver-installer-"));
    temporaryRoots.push(root);
    const archive = resolve(root, "Obsidian.AppImage");
    const executable = resolve(
      root,
      "cache/obsidian-installer/linux-x64/Obsidian-1.13.4/obsidian",
    );
    await writeFile(archive, `#!/bin/sh
set -eu
test "$1" = "--appimage-extract"
mkdir -p squashfs-root/resources
printf pinned-installer > squashfs-root/obsidian
printf '#!/bin/sh\nexit 0\n' > squashfs-root/AppRun
printf runtime-resource > squashfs-root/resources/electron.asar
chmod 700 squashfs-root/obsidian squashfs-root/AppRun
`);
    await chmod(archive, 0o600);

    await preparePinnedInstaller({
      installerArchive: archive,
      installerExecutable: executable,
    });

    expect(await readFile(executable, "utf8")).toBe("pinned-installer");
    expect((await stat(executable)).mode & 0o700).toBe(0o700);
    const launcher = pinnedInstallerLaunchPath(executable);
    expect(await readFile(launcher, "utf8")).toBe("#!/bin/sh\nexit 0\n");
    expect((await stat(launcher)).mode & 0o700).toBe(0o700);
    expect(await readFile(resolve(dirname(executable), "resources", "electron.asar"), "utf8"))
      .toBe("runtime-resource");
  });

  it("launches the pinned Electron runtime with explicit CI-safe Linux flags", () => {
    const configDir = resolve("private", "config");
    expect(buildPinnedObsidianArgs(configDir)).toEqual([
      `--user-data-dir=${configDir}`,
      "--no-sandbox",
      "--no-zygote",
      "--disable-setuid-sandbox",
      "--disable-crash-reporter",
      "--disable-gpu",
      "--disable-dev-shm-usage",
      "--ozone-platform=x11",
      "--remote-debugging-port=0",
      "--test-type=webdriver",
      "--tag=obsidian-launcher",
    ]);
  });

  it("defaults private runtime state to the repository's disposable disk-backed area", async () => {
    const previous = process.env.KWIRY_WEBDRIVER_PRIVATE_ROOT;
    delete process.env.KWIRY_WEBDRIVER_PRIVATE_ROOT;
    let privateRoot;
    try {
      privateRoot = await createWebdriverPrivateRoot();
    } finally {
      if (previous !== undefined) process.env.KWIRY_WEBDRIVER_PRIVATE_ROOT = previous;
    }
    expect(dirname(privateRoot)).toBe(resolve(repositoryRoot, ".tmp", "webdriver-private"));
    expect((await stat(privateRoot)).isDirectory()).toBe(true);
    await rm(privateRoot, { recursive: true, force: true });
  });

  it("creates private runtime state under the configured disk-backed root", async () => {
    const root = await mkdtemp(resolve(tmpdir(), "kwiry-webdriver-private-parent-"));
    temporaryRoots.push(root);
    const privateParent = resolve(root, "disk-private");
    const previous = process.env.KWIRY_WEBDRIVER_PRIVATE_ROOT;
    process.env.KWIRY_WEBDRIVER_PRIVATE_ROOT = privateParent;
    let privateRoot;
    try {
      privateRoot = await createWebdriverPrivateRoot();
    } finally {
      if (previous === undefined) delete process.env.KWIRY_WEBDRIVER_PRIVATE_ROOT;
      else process.env.KWIRY_WEBDRIVER_PRIVATE_ROOT = previous;
    }
    expect(dirname(privateRoot)).toBe(privateParent);
    expect((await stat(privateRoot)).isDirectory()).toBe(true);
    await rm(privateRoot, { recursive: true, force: true });
  });

  it("uses the private layout temp directory when no short runtime root is configured", async () => {
    const previous = process.env.KWIRY_WEBDRIVER_TMP_ROOT;
    delete process.env.KWIRY_WEBDRIVER_TMP_ROOT;
    try {
      expect(await createWebdriverRuntimeTempRoot({ tmp: "private-temp" })).toEqual({
        path: "private-temp",
        cleanupPath: null,
      });
    } finally {
      if (previous !== undefined) process.env.KWIRY_WEBDRIVER_TMP_ROOT = previous;
    }
  });

  it("creates a disposable runtime temp directory under the configured short disk-backed root", async () => {
    const root = await mkdtemp(resolve(tmpdir(), "kwiry-webdriver-runtime-temp-"));
    temporaryRoots.push(root);
    const shortParent = resolve(root, "t");
    const previous = process.env.KWIRY_WEBDRIVER_TMP_ROOT;
    process.env.KWIRY_WEBDRIVER_TMP_ROOT = shortParent;
    try {
      const runtimeTemp = await createWebdriverRuntimeTempRoot({ tmp: "private-temp" });
      expect(runtimeTemp.path).toBe(shortParent);
      expect(runtimeTemp.cleanupPath).toBe(runtimeTemp.path);
      expect((await stat(runtimeTemp.path)).isDirectory()).toBe(true);
    } finally {
      if (previous === undefined) delete process.env.KWIRY_WEBDRIVER_TMP_ROOT;
      else process.env.KWIRY_WEBDRIVER_TMP_ROOT = previous;
    }
  });

  it("refuses to take ownership of a preexisting configured runtime temp directory", async () => {
    const root = await mkdtemp(resolve(tmpdir(), "kwiry-webdriver-runtime-temp-existing-"));
    temporaryRoots.push(root);
    const shortRoot = resolve(root, "t");
    await mkdir(shortRoot, { mode: 0o700 });
    const previous = process.env.KWIRY_WEBDRIVER_TMP_ROOT;
    process.env.KWIRY_WEBDRIVER_TMP_ROOT = shortRoot;
    try {
      await expect(createWebdriverRuntimeTempRoot({ tmp: "private-temp" })).rejects.toThrow();
    } finally {
      if (previous === undefined) delete process.env.KWIRY_WEBDRIVER_TMP_ROOT;
      else process.env.KWIRY_WEBDRIVER_TMP_ROOT = previous;
    }
  });

  it("uses only identity-verified prepared runtime assets when supplied", async () => {
    const root = await mkdtemp(resolve(tmpdir(), "kwiry-webdriver-prepared-"));
    temporaryRoots.push(root);
    const prepared = resolve(root, "prepared");
    const layout = {
      downloads: resolve(root, "downloads"),
      cache: resolve(root, "cache"),
      versions: resolve(root, "versions.json"),
      driver: resolve(root, "driver", "chromedriver"),
    };
    await Promise.all([
      mkdir(prepared, { recursive: true }),
      mkdir(layout.downloads, { recursive: true }),
      mkdir(layout.cache, { recursive: true }),
      mkdir(resolve(root, "driver"), { recursive: true }),
    ]);
    const manifest = manifestFixture();
    const installer = Buffer.from("installer");
    const asar = Buffer.from("app-asar");
    const driver = Buffer.from("driver");
    const { gzipSync } = await import("node:zlib");
    const appArchive = gzipSync(asar);
    const driverArchive = Buffer.from("driver-archive");
    const preparedAssets = new Map([
      ["installer", installer],
      ["app", appArchive],
      ["driver", driverArchive],
    ]);
    for (const [name, artifact] of Object.entries(manifest.artifacts)) {
      const bytes = preparedAssets.get(name === "obsidian_installer" ? "installer" : name === "obsidian_app" ? "app" : "driver");
      artifact.bytes = bytes.byteLength;
      artifact.sha256 = sha256(bytes);
      await writeFile(resolve(prepared, new URL(artifact.url).pathname.split("/").at(-1)), bytes);
    }
    manifest.derived.obsidian_app_asar = { bytes: asar.byteLength, sha256: sha256(asar) };
    manifest.derived.chromedriver_binary = { bytes: driver.byteLength, sha256: sha256(driver) };
    await writeFile(resolve(prepared, "chromedriver"), driver);
    const previous = process.env.KWIRY_WEBDRIVER_RUNTIME_ASSETS;
    process.env.KWIRY_WEBDRIVER_RUNTIME_ASSETS = prepared;
    try {
      await prepareVerifiedRuntime(layout, manifest, {
        download: async () => { throw new Error("network download was used"); },
      });
    } finally {
      if (previous === undefined) delete process.env.KWIRY_WEBDRIVER_RUNTIME_ASSETS;
      else process.env.KWIRY_WEBDRIVER_RUNTIME_ASSETS = previous;
    }
    expect(await readFile(resolve(layout.cache, "obsidian-app", "obsidian-1.13.7.asar"))).toEqual(asar);
    expect(await readFile(layout.driver)).toEqual(driver);
  });

  it.each([
    ["loader dependency", "error while loading shared libraries: libfixture.so: cannot open shared object file", "launch_dependency_missing"],
    ["shared memory", "FATAL: platform_shared_memory_region_posix.cc startup stopped", "launch_shared_memory_unavailable"],
    ["runtime file", "FATAL: No such file or directory", "launch_runtime_file_missing"],
    ["session bus", "FATAL: dbus object_proxy startup stopped", "launch_session_bus_unavailable"],
    ["subprocess", "FATAL: spawn_subprocess.cc startup stopped", "launch_subprocess_failed"],
    ["display", "Missing X server or $DISPLAY", "launch_display_unavailable"],
    ["sandbox", "No usable sandbox!", "launch_sandbox_unavailable"],
    ["zygote", "FATAL: zygote_communication_linux.cc startup stopped", "launch_sandbox_unavailable"],
    ["GPU", "GPU process isn't usable. Goodbye.", "launch_gpu_unavailable"],
    ["single instance", "ProcessSingleton failed to create a singleton lock", "launch_instance_conflict"],
    ["singleton socket path", "FATAL: process_singleton_posix.cc Socket path is too long", "launch_singleton_socket_path_failed"],
    ["crash reporter", "chrome_crashpad_handler: --database is required", "launch_crash_reporter_unavailable"],
    ["runtime resources", "Invalid file descriptor to ICU data received", "launch_runtime_resources_unavailable"],
    ["platform runtime", "GLib-GIO-ERROR: platform setup failed", "launch_platform_runtime_failed"],
    ["V8 bootstrap", "FATAL: gin/v8_initializer.cc startup stopped", "launch_v8_bootstrap_failed"],
    ["Electron bootstrap", "FATAL: electron_main_delegate.cc startup stopped", "launch_electron_bootstrap_failed"],
    ["browser bootstrap", "FATAL: browser_main_loop.cc startup stopped", "launch_browser_bootstrap_failed"],
    ["Node bootstrap", "FATAL: node_bindings.cc startup stopped", "launch_node_bootstrap_failed"],
    ["process model", "FATAL: process_coordination.cc startup stopped", "launch_process_model_failed"],
    ["process CPU metrics", "FATAL: process_metrics_linux.cc cpu_time invalid", "launch_process_cpu_metrics_failed"],
    ["process memory metrics", "FATAL: process_metrics_linux.cc resident_pages invalid", "launch_process_memory_metrics_failed"],
    ["process metrics", "FATAL: process_metrics_posix.cc startup stopped", "launch_process_metrics_failed"],
    ["process spawn", "FATAL: process_launcher_posix.cc startup stopped", "launch_process_spawn_runtime_failed"],
    ["process handle", "FATAL: process_handle_linux.cc startup stopped", "launch_process_handle_runtime_failed"],
    ["process enumeration", "FATAL: named_process_iterator_linux.cc startup stopped", "launch_process_enumeration_failed"],
    ["process scheduling", "FATAL: process_priority.cc startup stopped", "launch_process_scheduling_failed"],
    ["process identity", "FATAL: current_process.cc startup stopped", "launch_process_identity_failed"],
    ["process platform", "FATAL: process_linux.cc startup stopped", "launch_process_platform_failed"],
    ["thread runtime", "FATAL: thread_restrictions.cc startup stopped", "launch_thread_runtime_failed"],
    ["sequence runtime", "FATAL: sequence_checker_impl.cc startup stopped", "launch_sequence_runtime_failed"],
    ["task runtime", "FATAL: task_runner.cc startup stopped", "launch_task_runtime_failed"],
    ["run loop", "FATAL: run_loop.cc startup stopped", "launch_run_loop_failed"],
    ["blocking runtime", "FATAL: scoped_blocking_call.cc startup stopped", "launch_blocking_runtime_failed"],
    ["event loop", "FATAL: message_pump_epoll.cc startup stopped", "launch_event_loop_failed"],
    ["feature initialization", "FATAL: feature_list.cc startup stopped", "launch_feature_initialization_failed"],
    ["filesystem", "FATAL: file_util_posix.cc startup stopped", "launch_filesystem_unavailable"],
    ["memory", "FATAL: partition_alloc_support.cc startup stopped", "launch_memory_unavailable"],
    ["IPC", "FATAL: mojo channel startup stopped", "launch_ipc_unavailable"],
    ["argument", "FATAL: command_line invalid flag", "launch_argument_invalid"],
    ["proxy", "FATAL: proxy startup stopped", "launch_proxy_runtime_failed"],
    ["network monitor", "FATAL: network_change_notifier startup stopped", "launch_network_monitor_failed"],
    ["socket", "FATAL: socket_posix startup stopped", "launch_socket_runtime_failed"],
    ["DevTools server", "FATAL: Cannot start http server for devtools", "launch_devtools_server_failed"],
    ["socket collision", "FATAL: bind failed: Address already in use", "launch_socket_address_in_use"],
    ["socket family", "FATAL: Address family not supported by protocol", "launch_socket_family_unavailable"],
    ["socket creation", "FATAL: CreatePlatformSocket failed", "launch_socket_creation_failed"],
    ["DNS", "FATAL: host_resolver startup stopped", "launch_dns_runtime_failed"],
    ["network", "FATAL: network service startup stopped", "launch_network_runtime_failed"],
    ["security", "FATAL: NSS crypto startup stopped", "launch_security_runtime_failed"],
    ["UI", "FATAL: aura ui_base startup stopped", "launch_ui_runtime_failed"],
    ["permission", "FATAL: operation not permitted", "launch_permission_denied"],
    ["assertion", "FATAL: CHECK failed: ready", "launch_runtime_assertion_failed"],
    ["other fatal", "FATAL: runtime initialization stopped", "launch_runtime_fatal"],
  ])("classifies %s startup diagnostics without returning raw output", (_name, output, stage) => {
    expect(classifyRuntimeOutput(output)).toBe(stage);
    expect(stage).not.toContain(output);
  });

  it("classifies only fatal lines when nonfatal output names another subsystem", () => {
    expect(classifyFatalRuntimeOutput([
      "ERROR: socket_posix.cc transient startup warning",
      "FATAL: runtime initialization stopped",
    ].join("\n"))).toBe("launch_runtime_fatal");
    expect(classifyFatalRuntimeOutput("ERROR: socket_posix.cc transient startup warning")).toBeNull();
    expect(classifyFatalRuntimeOutput([
      "FATAL: socket_posix.cc startup stopped",
      "Address family not supported by protocol",
    ].join("\n"))).toBe("launch_socket_runtime_failed");
  });

  it("keeps unknown process output private and classifies only process status", () => {
    const privateDetail = ["private runtime detail under ", "/", "home", "/example"].join("");
    expect(classifyRuntimeOutput(privateDetail)).toBeNull();
    expect(classifyRuntimeProcessExit({ exitCode: 0, signalCode: null })).toBe("launch_process_clean_exit");
    expect(classifyRuntimeProcessExit({ exitCode: 1, signalCode: null })).toBe("launch_process_error_exit");
    expect(classifyRuntimeProcessExit({ exitCode: null, signalCode: "SIGTERM" })).toBe("launch_process_terminated");
    expect(classifyRuntimeProcessExit({ exitCode: null, signalCode: "SIGUSR1" })).toBe("launch_process_signaled");
  });

  it.each([
    ["SIGABRT", "launch_process_aborted"],
    ["SIGBUS", "launch_process_bus_error"],
    ["SIGFPE", "launch_process_arithmetic_fault"],
    ["SIGILL", "launch_process_illegal_instruction"],
    ["SIGKILL", "launch_process_killed"],
    ["SIGSEGV", "launch_process_segmentation_fault"],
    ["SIGTERM", "launch_process_terminated"],
    ["SIGTRAP", "launch_process_trapped"],
  ])("maps %s to a fixed private-safe stage", (signalCode, stage) => {
    expect(classifyRuntimeProcessExit({ exitCode: null, signalCode })).toBe(stage);
  });

  it("drains buffered runtime diagnostics before reporting a process signal", async () => {
    const stderr = new PassThrough();
    const proc = { exitCode: null, signalCode: "SIGTRAP", stderr };
    trackRuntimeExitDiagnostics(proc);
    const stage = awaitRuntimeProcessExitStage(proc);
    queueMicrotask(() => stderr.end("The SUID sandbox helper binary was found, but is not configured correctly"));
    await expect(stage).resolves.toBe("launch_sandbox_unavailable");
  });

  it("classifies stdout while waiting for both runtime output pipes to drain", async () => {
    const stdout = new PassThrough();
    const stderr = new PassThrough();
    const proc = { exitCode: null, signalCode: "SIGTRAP", stdout, stderr };
    trackRuntimeExitDiagnostics(proc);
    const stage = awaitRuntimeProcessExitStage(proc);
    queueMicrotask(() => {
      stdout.end("chrome_crashpad_handler: --database is required");
      stderr.end();
    });
    await expect(stage).resolves.toBe("launch_crash_reporter_unavailable");
  });

  it("upgrades a generic fatal stage when a later chunk identifies the subsystem", async () => {
    const stdout = new PassThrough();
    const stderr = new PassThrough();
    const proc = { exitCode: null, signalCode: "SIGTRAP", stdout, stderr };
    trackRuntimeExitDiagnostics(proc);
    stdout.write("FATAL: startup stopped");
    const stage = awaitRuntimeProcessExitStage(proc);
    queueMicrotask(() => {
      stdout.end(" setuid_sandbox_host.cc");
      stderr.end();
    });
    await expect(stage).resolves.toBe("launch_sandbox_unavailable");
  });

  it("upgrades a broad network stage when a later chunk identifies the subsystem", async () => {
    const stdout = new PassThrough();
    const stderr = new PassThrough();
    const proc = { exitCode: null, signalCode: "SIGTRAP", stdout, stderr };
    trackRuntimeExitDiagnostics(proc);
    stdout.write("FATAL: network startup stopped");
    const stage = awaitRuntimeProcessExitStage(proc);
    queueMicrotask(() => {
      stdout.end(" socket_posix.cc");
      stderr.end();
    });
    await expect(stage).resolves.toBe("launch_socket_runtime_failed");
  });

  it("upgrades a broad socket stage when a later chunk identifies the bind failure", async () => {
    const stdout = new PassThrough();
    const stderr = new PassThrough();
    const proc = { exitCode: null, signalCode: "SIGTRAP", stdout, stderr };
    trackRuntimeExitDiagnostics(proc);
    stdout.write("FATAL: socket_posix.cc startup stopped");
    const stage = awaitRuntimeProcessExitStage(proc);
    queueMicrotask(() => {
      stdout.end(" Address family not supported by protocol");
      stderr.end();
    });
    await expect(stage).resolves.toBe("launch_socket_family_unavailable");
  });

  it("prioritizes a fatal line over an earlier nonfatal subsystem warning", async () => {
    const stdout = new PassThrough();
    const stderr = new PassThrough();
    const proc = { exitCode: null, signalCode: "SIGTRAP", stdout, stderr };
    trackRuntimeExitDiagnostics(proc);
    stdout.write("ERROR: socket_posix.cc transient startup warning\n");
    const stage = awaitRuntimeProcessExitStage(proc);
    queueMicrotask(() => {
      stdout.end("FATAL: runtime initialization stopped\n");
      stderr.end();
    });
    await expect(stage).resolves.toBe("launch_runtime_fatal");
  });

  it("maps plugin readiness errors to a fixed scenario stage", async () => {
    const driver = {
      wait: async () => { throw new Error("private webdriver detail"); },
    };
    await expect(exerciseObsidian({ driver, manifest: manifestFixture() }))
      .rejects.toThrow("scenario_plugin_ready_failed");
  });

  it.each([
    ["state setup", 1, "scenario_state_setup_failed"],
    ["open hook", 2, "scenario_open_hook_failed"],
    ["notice observer", 3, "scenario_notice_observer_failed"],
  ])("maps %s errors to a fixed scenario stage", async (_name, failingCall, stage) => {
    let calls = 0;
    const driver = {
      wait: async () => true,
      executeScript: async () => {
        calls += 1;
        if (calls === failingCall) throw new Error("private webdriver detail");
      },
    };
    await expect(exerciseObsidian({ driver, manifest: manifestFixture() })).rejects.toThrow(stage);
  });

  it("maps command registration timeouts to a fixed scenario stage", async () => {
    let waitCalls = 0;
    const driver = {
      wait: async () => {
        waitCalls += 1;
        if (waitCalls === 1) return true;
        throw new Error("private webdriver detail");
      },
    };
    await expect(exerciseObsidian({ driver, manifest: manifestFixture() }))
      .rejects.toThrow("scenario_command_registration_failed");
  });

  it("maps workspace leaf timeouts to a fixed scenario stage", async () => {
    let waitCalls = 0;
    const driver = {
      wait: async () => {
        waitCalls += 1;
        if (waitCalls <= 2) return true;
        throw new Error("private webdriver detail");
      },
    };
    await expect(exerciseObsidian({ driver, manifest: manifestFixture() }))
      .rejects.toThrow("scenario_leaf_ready_failed");
  });

  it("retries transient workspace leaf lookup failures", async () => {
    let waitCalls = 0;
    let scriptCalls = 0;
    const driver = {
      wait: async (condition) => {
        waitCalls += 1;
        if (waitCalls <= 2) return true;
        if (waitCalls === 3) {
          expect(await condition()).toBe(false);
          expect(await condition()).toBe(true);
          return true;
        }
        throw new Error("stop after leaf readiness");
      },
      executeScript: async () => {
        scriptCalls += 1;
        if (scriptCalls === 1) throw new Error("private transient leaf detail");
        if (scriptCalls === 2) return true;
        throw new Error("private state setup detail");
      },
    };

    await expect(exerciseObsidian({ driver, manifest: manifestFixture() }))
      .rejects.toThrow("scenario_state_setup_failed");
    expect(scriptCalls).toBe(3);
  });

  it("maps command-palette input timeouts to a fixed scenario stage", async () => {
    let waitCalls = 0;
    const actions = {
      keyDown() { return this; },
      sendKeys() { return this; },
      keyUp() { return this; },
      async perform() {},
    };
    const driver = {
      wait: async () => {
        waitCalls += 1;
        if (waitCalls <= 3) return true;
        throw new Error("private webdriver detail");
      },
      executeScript: async () => {},
      actions: () => actions,
    };
    await expect(exerciseObsidian({ driver, manifest: manifestFixture() }))
      .rejects.toThrow("scenario_palette_input_failed");
  });

  it("generates deterministic XLSM content while isolating VBA text", () => {
    const first = buildSyntheticXlsm();
    const second = buildSyntheticXlsm();
    expect(second).toEqual(first);
    const entries = new Map(parseStoredZip(first).map((entry) => [entry.name, entry.bytes]));
    expect(entries.get("xl/sharedStrings.xml").toString()).toContain("macro boundary");
    expect(entries.get("xl/vbaProject.bin").toString()).toContain("must-not-index");
    for (const [name, bytes] of entries) {
      if (name !== "xl/vbaProject.bin") expect(bytes.toString()).not.toContain("must-not-index");
    }
  });

  it("rejects an unreviewed redirect chain before consuming bytes", async () => {
    const artifact = { url: "https://example.test/v1/file", bytes: 1, sha256: sha256("x") };
    await expect(downloadPinnedArtifact(artifact, resolve(tmpdir(), "unused"), {
      fetchImpl: async () => new Response(null, { status: 302, headers: { location: "https://evil.test/file" } }),
    })).rejects.toThrow("download_identity_mismatch");
  });

  it("binds passing aggregate evidence to candidate and runtime hashes", () => {
    const evidence = buildEvidence({
      candidate: { version: "0.6.0-beta.15", candidate_set_sha256: HASH, file_count: 11 },
      manifest: manifestFixture(),
      manifestSha256: HASH,
      observed: observed(),
      cleanup: {
        webdriver_quit: true, obsidian_reaped: true, verified_download_server_closed: true,
        ports_closed: true, private_state_removed: true,
      },
    });
    expect(evidence.verdict).toBe("SELENIUM_RELEASE_GATE_PASSED");
    expect(JSON.stringify(evidence)).not.toContain("macro boundary");
  });

  it("rejects a non-production candidate before any runtime seam is called", async () => {
    const candidate = await createCandidateFixture();
    const manifestFile = resolve(candidate, "runtime.json");
    await writeFile(manifestFile, `${JSON.stringify(manifestFixture())}\n`);
    let runtimeCalls = 0;
    await expect(runWebdriverReleaseGate({
      candidate,
      manifest: manifestFile,
      evidence: resolve(candidate, "webdriver.evidence.json"),
      tag: "0.6.0-beta.15",
    }, {
      createPrivateRoot: async () => { runtimeCalls += 1; return candidate; },
    })).rejects.toThrow();
    expect(runtimeCalls).toBe(0);
  });

  it.each([
    ["stale notice", { ...observed(), staleNotices: 1 }, "stale_notice_observed"],
    ["missing open", { ...observed(), openFileCalls: 0 }, "open_not_invoked"],
    ["rejected open", { ...observed(), openFilePromise: "rejected" }, "open_promise_rejected"],
    ["unselected expected result", { ...observed(), expectedResultSelected: false }, "result_not_rendered"],
    ["runtime drift", { ...observed(), electron: "43.2.0" }, "launch_failed"],
  ])("does not accept %s from the real scenario", (_name, changed, code) => {
    expect(() => assertObserved(changed, manifestFixture())).toThrow(code);
  });
});

function sha256(value) {
  return createHash("sha256").update(value).digest("hex");
}
