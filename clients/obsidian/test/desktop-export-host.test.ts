// SPDX-FileCopyrightText: 2026 cybersader
// SPDX-License-Identifier: GPL-3.0-only

import {
  existsSync,
  lstatSync,
  mkdirSync,
  mkdtempSync,
  readFileSync,
  readdirSync,
  rmSync,
  symlinkSync,
  writeFileSync,
} from "fs";
import * as os from "os";
import * as path from "path";

import { afterEach, beforeEach, describe, expect, it } from "vitest";

import {
  createDesktopDiagnosticsExportHost,
  createProductionDesktopDiagnosticsExportHost,
  nodeDesktopExportFileSystem,
  type DesktopDiagnosticsExportDependencies,
  type DesktopExportFileHandle,
  type DesktopExportFileStats,
  type DesktopExportFileSystem,
  type DesktopSaveDialog,
} from "../src/diagnostics/desktop-export-host";

let workspace: string;
let vaultRoot: string;
let outsideRoot: string;
let downloadsRoot: string;

beforeEach(() => {
  workspace = mkdtempSync(path.join(os.tmpdir(), "kwiry-diagnostics-export-"));
  vaultRoot = path.join(workspace, "private-vault");
  outsideRoot = path.join(workspace, "outside");
  downloadsRoot = path.join(workspace, "downloads");
  mkdirSync(vaultRoot);
  mkdirSync(outsideRoot);
  mkdirSync(downloadsRoot);
});

afterEach(() => {
  rmSync(workspace, { recursive: true, force: true });
});

interface RecordedCall {
  readonly operation: string;
  readonly target: string;
}

class RecordingFileSystem implements DesktopExportFileSystem {
  readonly calls: RecordedCall[] = [];
  failure: ((call: RecordedCall) => boolean) | null = null;
  partialWriteBytes: number | null = null;

  constructor(private readonly inner = nodeDesktopExportFileSystem()) {}

  lstat(target: string): Promise<DesktopExportFileStats> {
    this.record("lstat", target);
    return this.inner.lstat(target);
  }

  realpath(target: string): Promise<string> {
    this.record("realpath", target);
    return this.inner.realpath(target);
  }

  async open(target: string, flags: string, mode?: number): Promise<DesktopExportFileHandle> {
    this.record(`open:${flags}`, target);
    const handle = await this.inner.open(target, flags, mode);
    return {
      write: async (bytes, offset, length) => {
        this.record("write", target);
        const bounded = this.partialWriteBytes === null
          ? length
          : Math.min(length, this.partialWriteBytes);
        return handle.write(bytes, offset, bounded);
      },
      chmod: async (fileMode) => {
        this.record("chmod", target);
        await handle.chmod(fileMode);
      },
      sync: async () => {
        this.record("sync", target);
        await handle.sync();
      },
      close: async () => {
        this.record("close", target);
        await handle.close();
      },
    };
  }

  async rename(from: string, to: string): Promise<void> {
    this.record("rename", to);
    await this.inner.rename(from, to);
  }

  async unlink(target: string): Promise<void> {
    this.record("unlink", target);
    await this.inner.unlink(target);
  }

  private record(operation: string, target: string): void {
    const call = { operation, target };
    this.calls.push(call);
    if (this.failure?.(call)) throw new Error("injected diagnostics export failure");
  }
}

function dialogResult(
  result: { readonly canceled: boolean; readonly filePath?: string },
  calls: Array<Parameters<DesktopSaveDialog["showSaveDialog"]>[0]> = [],
): DesktopSaveDialog {
  return {
    showSaveDialog: async (options) => {
      calls.push(options);
      return result;
    },
  };
}

function dependencies(
  dialog: DesktopSaveDialog,
  fileSystem: DesktopExportFileSystem,
  overrides: Partial<DesktopDiagnosticsExportDependencies> = {},
): DesktopDiagnosticsExportDependencies {
  return {
    fs: fileSystem,
    dialog,
    path,
    platform: process.platform,
    defaultDirectories: [vaultRoot, downloadsRoot],
    now: () => 0,
    randomSuffix: () => "0123456789abcdef",
    ...overrides,
  };
}

function chunks(...values: string[]): Iterable<Uint8Array> {
  const encoder = new TextEncoder();
  return values.map((value) => encoder.encode(value));
}

function temporaryNames(directory: string): string[] {
  return readdirSync(directory).filter((name) => name.includes(".kwiry-tmp-"));
}

describe("desktop diagnostics export host", () => {
  it("composes the renderer remote capability and fails closed when it is absent", async () => {
    const destination = path.join(outsideRoot, "production-report.txt");
    const requestedPaths: string[] = [];
    const host = createProductionDesktopDiagnosticsExportHost(() => ({
      app: {
        getPath: (name) => {
          requestedPaths.push(name);
          return downloadsRoot;
        },
      },
      dialog: dialogResult({ canceled: false, filePath: destination }),
    }));

    await expect(host.save({ vaultRoot, chunks: chunks("safe") }))
      .resolves.toEqual({ kind: "saved" });
    expect(readFileSync(destination, "utf8")).toBe("safe");
    expect(requestedPaths).toEqual(["downloads", "documents", "desktop"]);

    let iterated = false;
    const unavailable = createProductionDesktopDiagnosticsExportHost(() => null);
    await expect(unavailable.save({
      vaultRoot,
      chunks: (function* () {
        iterated = true;
        yield new Uint8Array([1]);
      })(),
    })).resolves.toEqual({ kind: "unavailable" });
    expect(iterated).toBe(false);
  });

  it("treats cancellation as a no-op before serialization or filesystem writes", async () => {
    const fileSystem = new RecordingFileSystem();
    let iterated = false;
    const lazyChunks = (function* () {
      iterated = true;
      yield new TextEncoder().encode("must not be serialized");
    })();
    const host = createDesktopDiagnosticsExportHost(dependencies(
      dialogResult({ canceled: true }),
      fileSystem,
    ));

    await expect(host.save({ vaultRoot, chunks: lazyChunks })).resolves.toEqual({
      kind: "cancelled",
    });
    expect(iterated).toBe(false);
    expect(fileSystem.calls.some((call) => call.operation.startsWith("open:"))).toBe(false);
    expect(temporaryNames(outsideRoot)).toEqual([]);
  });

  it("uses a safe OS default and publishes complete partial writes through a synced temp file", async () => {
    const fileSystem = new RecordingFileSystem();
    fileSystem.partialWriteBytes = 2;
    const dialogCalls: Array<Parameters<DesktopSaveDialog["showSaveDialog"]>[0]> = [];
    const destination = path.join(outsideRoot, "report.txt");
    const host = createDesktopDiagnosticsExportHost(dependencies(
      dialogResult({ canceled: false, filePath: destination }, dialogCalls),
      fileSystem,
    ));

    const result = await host.save({ vaultRoot, chunks: chunks("abc", "def") });

    expect(result).toEqual({ kind: "saved" });
    expect(Object.keys(result)).toEqual(["kind"]);
    expect(readFileSync(destination, "utf8")).toBe("abcdef");
    expect(dialogCalls).toHaveLength(1);
    expect(dialogCalls[0]?.defaultPath).toBe(path.join(
      downloadsRoot,
      "kwiry-diagnostics-19700101T000000Z.txt",
    ));
    expect(temporaryNames(outsideRoot)).toEqual([]);
    if (process.platform !== "win32") {
      expect(lstatSync(destination).mode & 0o777).toBe(0o600);
    }

    const operations = fileSystem.calls.map((call) => call.operation);
    const open = operations.indexOf("open:wx");
    const chmod = operations.indexOf("chmod", open);
    const firstWrite = operations.indexOf("write", chmod);
    const sync = operations.indexOf("sync", firstWrite);
    const close = operations.indexOf("close", sync);
    const rename = operations.indexOf("rename", close);
    expect([open, chmod, firstWrite, sync, close, rename])
      .toEqual([...new Set([open, chmod, firstWrite, sync, close, rename])].sort((a, b) => a - b));
    expect(fileSystem.calls.filter((call) => call.operation === "write").length)
      .toBeGreaterThan(2);
  });

  it("rejects direct and canonicalized vault destinations without opening a file", async () => {
    const directFileSystem = new RecordingFileSystem();
    const direct = createDesktopDiagnosticsExportHost(dependencies(
      dialogResult({ canceled: false, filePath: path.join(vaultRoot, "report.txt") }),
      directFileSystem,
    ));
    await expect(direct.save({ vaultRoot, chunks: chunks("safe") }))
      .resolves.toEqual({ kind: "inside_vault" });
    expect(directFileSystem.calls.some((call) => call.operation.startsWith("open:"))).toBe(false);

    if (process.platform !== "win32") {
      const linkedParent = path.join(workspace, "linked-parent");
      symlinkSync(vaultRoot, linkedParent, "dir");
      const linkedFileSystem = new RecordingFileSystem();
      const linked = createDesktopDiagnosticsExportHost(dependencies(
        dialogResult({ canceled: false, filePath: path.join(linkedParent, "report.txt") }),
        linkedFileSystem,
      ));
      await expect(linked.save({ vaultRoot, chunks: chunks("safe") }))
        .resolves.toEqual({ kind: "inside_vault" });
      expect(existsSync(path.join(vaultRoot, "report.txt"))).toBe(false);
    }
  });

  it("rejects an existing symlink that resolves into the vault", async () => {
    if (process.platform === "win32") return;
    const privateTarget = path.join(vaultRoot, "private.txt");
    const selected = path.join(outsideRoot, "report.txt");
    writeFileSync(privateTarget, "private");
    symlinkSync(privateTarget, selected, "file");
    const fileSystem = new RecordingFileSystem();
    const host = createDesktopDiagnosticsExportHost(dependencies(
      dialogResult({ canceled: false, filePath: selected }),
      fileSystem,
    ));

    await expect(host.save({ vaultRoot, chunks: chunks("safe") }))
      .resolves.toEqual({ kind: "inside_vault" });
    expect(fileSystem.calls.some((call) => call.operation.startsWith("open:"))).toBe(false);
  });

  it("cleans the exclusive temporary file after each pre-publication failure", async () => {
    for (const operation of ["open:wx", "chmod", "write", "sync", "rename"]) {
      const caseRoot = path.join(outsideRoot, operation.replace(":", "-"));
      mkdirSync(caseRoot);
      const destination = path.join(caseRoot, "report.txt");
      const fileSystem = new RecordingFileSystem();
      let failed = false;
      fileSystem.failure = (call) => {
        if (failed || call.operation !== operation) return false;
        if (operation === "sync" && !call.target.includes(".kwiry-tmp-")) return false;
        failed = true;
        return true;
      };
      const host = createDesktopDiagnosticsExportHost(dependencies(
        dialogResult({ canceled: false, filePath: destination }),
        fileSystem,
      ));

      await expect(host.save({ vaultRoot, chunks: chunks("abcdef") }))
        .resolves.toEqual({ kind: "write_failed" });
      expect(failed, operation).toBe(true);
      expect(existsSync(destination), operation).toBe(false);
      expect(temporaryNames(caseRoot), operation).toEqual([]);
    }
  });

  it("uses target-platform path semantics for Windows containment on any runner", async () => {
    const directory: DesktopExportFileStats = {
      isFile: () => false,
      isDirectory: () => true,
      isSymbolicLink: () => false,
    };
    const fileSystem: DesktopExportFileSystem = {
      realpath: async (target) => target,
      lstat: async () => directory,
      open: async () => { throw new Error("must not open"); },
      rename: async () => { throw new Error("must not rename"); },
      unlink: async () => { throw new Error("must not unlink"); },
    };
    const host = createDesktopDiagnosticsExportHost(dependencies(
      dialogResult({
        canceled: false,
        filePath: "c:\\users\\owner\\vault\\Reports\\diagnostics.txt",
      }),
      fileSystem,
      {
        path: path.win32,
        platform: "win32",
        defaultDirectories: [],
      },
    ));

    await expect(host.save({
      vaultRoot: "C:\\Users\\Owner\\Vault",
      chunks: chunks("safe"),
    })).resolves.toEqual({ kind: "inside_vault" });
  });
});
