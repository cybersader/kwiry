// SPDX-FileCopyrightText: 2026 cybersader
// SPDX-License-Identifier: GPL-3.0-only
//
// Desktop diagnostics authority lives here and nowhere in the Worker graph.
// Paths remain inside this adapter; callers receive only fixed outcomes.

import { randomBytes } from "crypto";
import * as fsPromises from "fs/promises";
import * as path from "path";

const FILE_MODE = 0o600;
const MAX_EXPORT_BYTES = 16 * 1_024 * 1_024;

export type DesktopDiagnosticsExportResult =
  | { readonly kind: "saved" }
  | { readonly kind: "cancelled" }
  | { readonly kind: "inside_vault" }
  | { readonly kind: "unsafe_destination" }
  | { readonly kind: "unavailable" }
  | { readonly kind: "write_failed" };

export interface DesktopDiagnosticsExportRequest {
  readonly vaultRoot: string;
  readonly chunks: Iterable<Uint8Array> | AsyncIterable<Uint8Array>;
}

export interface DesktopDiagnosticsExportHost {
  save(request: DesktopDiagnosticsExportRequest): Promise<DesktopDiagnosticsExportResult>;
}

export interface DesktopExportFileStats {
  isFile(): boolean;
  isDirectory(): boolean;
  isSymbolicLink(): boolean;
}

export interface DesktopExportFileHandle {
  write(
    bytes: Uint8Array,
    offset: number,
    length: number,
  ): Promise<{ readonly bytesWritten: number }>;
  chmod(mode: number): Promise<void>;
  sync(): Promise<void>;
  close(): Promise<void>;
}

export interface DesktopExportFileSystem {
  lstat(target: string): Promise<DesktopExportFileStats>;
  realpath(target: string): Promise<string>;
  open(target: string, flags: string, mode?: number): Promise<DesktopExportFileHandle>;
  rename(from: string, to: string): Promise<void>;
  unlink(target: string): Promise<void>;
}

export interface DesktopSaveDialog {
  showSaveDialog(options: {
    readonly title: string;
    readonly buttonLabel: string;
    readonly defaultPath?: string;
    readonly filters: readonly [{ readonly name: string; readonly extensions: readonly ["txt"] }];
    readonly properties: readonly ["showOverwriteConfirmation", "createDirectory"];
  }): Promise<{ readonly canceled: boolean; readonly filePath?: string }>;
}

export interface DesktopExportPathApi {
  readonly sep: string;
  basename(target: string): string;
  dirname(target: string): string;
  isAbsolute(target: string): boolean;
  join(...parts: string[]): string;
  relative(from: string, to: string): string;
}

export interface DesktopDiagnosticsExportDependencies {
  readonly fs: DesktopExportFileSystem;
  readonly dialog: DesktopSaveDialog;
  readonly path: DesktopExportPathApi;
  readonly platform: NodeJS.Platform;
  readonly defaultDirectories: readonly string[];
  readonly now: () => number;
  readonly randomSuffix: () => string;
}

export interface DesktopElectronRemote {
  readonly dialog?: DesktopSaveDialog;
  readonly app?: {
    getPath(name: "downloads" | "documents" | "desktop"): string;
  };
}

interface ElectronShape {
  readonly remote?: DesktopElectronRemote;
}

export function createDesktopDiagnosticsExportHost(
  dependencies: DesktopDiagnosticsExportDependencies,
): DesktopDiagnosticsExportHost {
  return {
    save: (request) => saveDiagnosticsExport(request, dependencies),
  };
}

export function createProductionDesktopDiagnosticsExportHost(
  loadRemote: () => DesktopElectronRemote | null = loadElectronRemote,
): DesktopDiagnosticsExportHost {
  try {
    const remote = loadRemote();
    if (!remote?.dialog || !remote.app) return unavailableHost();
    const defaultDirectories = (["downloads", "documents", "desktop"] as const).flatMap((name) => {
      try {
        const value = remote.app!.getPath(name);
        return typeof value === "string" && value.length > 0 ? [value] : [];
      } catch {
        return [];
      }
    });
    return createDesktopDiagnosticsExportHost({
      fs: nodeDesktopExportFileSystem(),
      dialog: remote.dialog,
      path,
      platform: process.platform,
      defaultDirectories,
      now: Date.now,
      randomSuffix: () => randomBytes(16).toString("hex"),
    });
  } catch {
    return unavailableHost();
  }
}

export function nodeDesktopExportFileSystem(): DesktopExportFileSystem {
  return {
    lstat: async (target) => {
      const stats = await fsPromises.lstat(target);
      return {
        isFile: () => stats.isFile(),
        isDirectory: () => stats.isDirectory(),
        isSymbolicLink: () => stats.isSymbolicLink(),
      };
    },
    realpath: (target) => fsPromises.realpath(target),
    open: async (target, flags, mode) => {
      const handle = await fsPromises.open(target, flags, mode);
      return {
        write: async (bytes, offset, length) => {
          const result = await handle.write(bytes, offset, length, null);
          return { bytesWritten: result.bytesWritten };
        },
        chmod: (fileMode) => handle.chmod(fileMode),
        sync: () => handle.sync(),
        close: () => handle.close(),
      };
    },
    rename: (from, to) => fsPromises.rename(from, to),
    unlink: (target) => fsPromises.unlink(target),
  };
}

async function saveDiagnosticsExport(
  request: DesktopDiagnosticsExportRequest,
  dependencies: DesktopDiagnosticsExportDependencies,
): Promise<DesktopDiagnosticsExportResult> {
  const resolver = dependencies.path;
  if (typeof request.vaultRoot !== "string" || !resolver.isAbsolute(request.vaultRoot)) {
    return { kind: "unavailable" };
  }

  const canonicalVault = await canonicalDirectory(
    dependencies.fs,
    request.vaultRoot,
  );
  if (canonicalVault === null) return { kind: "unavailable" };

  const suggestedName = suggestedFileName(dependencies.now());
  if (suggestedName === null) return { kind: "unavailable" };
  const defaultPath = await safeDefaultPath(
    dependencies,
    canonicalVault,
    suggestedName,
  );

  let selection: { readonly canceled: boolean; readonly filePath?: string };
  try {
    selection = await dependencies.dialog.showSaveDialog({
      title: "Export Kwiry diagnostics",
      buttonLabel: "Export",
      ...(defaultPath === null ? {} : { defaultPath }),
      filters: [{ name: "Text files", extensions: ["txt"] }],
      properties: ["showOverwriteConfirmation", "createDirectory"],
    });
  } catch {
    return { kind: "unavailable" };
  }
  if (selection.canceled) return { kind: "cancelled" };
  if (typeof selection.filePath !== "string" || !resolver.isAbsolute(selection.filePath)) {
    return { kind: "unsafe_destination" };
  }

  const selectedName = resolver.basename(selection.filePath);
  if (selectedName.length === 0 || selectedName === "." || selectedName === "..") {
    return { kind: "unsafe_destination" };
  }
  const canonicalParent = await canonicalDirectory(
    dependencies.fs,
    resolver.dirname(selection.filePath),
  );
  if (canonicalParent === null) return { kind: "unsafe_destination" };
  const destination = resolver.join(canonicalParent, selectedName);
  if (isWithin(resolver, canonicalVault, destination)) return { kind: "inside_vault" };

  const existing = await existingTarget(dependencies.fs, destination);
  if (existing.kind === "invalid") return { kind: "unsafe_destination" };
  if (existing.kind === "present") {
    let canonicalTarget: string;
    try {
      canonicalTarget = await dependencies.fs.realpath(destination);
    } catch {
      return { kind: "unsafe_destination" };
    }
    if (isWithin(resolver, canonicalVault, canonicalTarget)) return { kind: "inside_vault" };
    if (existing.stats.isSymbolicLink() || !existing.stats.isFile()) {
      return { kind: "unsafe_destination" };
    }
  }

  const suffix = dependencies.randomSuffix();
  if (!/^[0-9a-z]{16,64}$/u.test(suffix)) return { kind: "write_failed" };
  const temporary = resolver.join(canonicalParent, `.${selectedName}.kwiry-tmp-${suffix}`);
  let handle: DesktopExportFileHandle | null = null;
  let handleClosed = false;
  let published = false;
  try {
    handle = await dependencies.fs.open(temporary, "wx", FILE_MODE);
    await handle.chmod(FILE_MODE);
    let totalBytes = 0;
    for await (const chunk of request.chunks) {
      if (!(chunk instanceof Uint8Array)) throw new TypeError("invalid diagnostic export chunk");
      totalBytes += chunk.byteLength;
      if (!Number.isSafeInteger(totalBytes) || totalBytes > MAX_EXPORT_BYTES) {
        throw new RangeError("diagnostic export exceeds byte limit");
      }
      let offset = 0;
      while (offset < chunk.byteLength) {
        const result = await handle.write(chunk, offset, chunk.byteLength - offset);
        if (!Number.isSafeInteger(result.bytesWritten) || result.bytesWritten <= 0
          || result.bytesWritten > chunk.byteLength - offset) {
          throw new Error("diagnostic export write stalled");
        }
        offset += result.bytesWritten;
      }
    }
    await handle.sync();
    await handle.close();
    handleClosed = true;

    if (dependencies.platform === "win32" && existing.kind === "present") {
      await dependencies.fs.unlink(destination);
    }
    await dependencies.fs.rename(temporary, destination);
    published = true;
    await syncDirectoryBestEffort(dependencies.fs, canonicalParent);
    return { kind: "saved" };
  } catch {
    return { kind: "write_failed" };
  } finally {
    if (handle !== null && !handleClosed) {
      try {
        await handle.close();
      } catch {
        // The categorical result already reports the failed export.
      }
    }
    if (!published) {
      try {
        await dependencies.fs.unlink(temporary);
      } catch {
        // The exclusive temporary name is never returned or recorded.
      }
    }
  }
}

async function safeDefaultPath(
  dependencies: DesktopDiagnosticsExportDependencies,
  canonicalVault: string,
  suggestedName: string,
): Promise<string | null> {
  for (const candidate of dependencies.defaultDirectories) {
    if (!dependencies.path.isAbsolute(candidate)) continue;
    const canonical = await canonicalDirectory(dependencies.fs, candidate);
    if (canonical === null || isWithin(dependencies.path, canonicalVault, canonical)) continue;
    return dependencies.path.join(canonical, suggestedName);
  }
  return null;
}

async function canonicalDirectory(
  fileSystem: DesktopExportFileSystem,
  target: string,
): Promise<string | null> {
  try {
    const canonical = await fileSystem.realpath(target);
    const stats = await fileSystem.lstat(canonical);
    return stats.isDirectory() && !stats.isSymbolicLink() ? canonical : null;
  } catch {
    return null;
  }
}

async function existingTarget(
  fileSystem: DesktopExportFileSystem,
  target: string,
): Promise<
  | { readonly kind: "absent" }
  | { readonly kind: "present"; readonly stats: DesktopExportFileStats }
  | { readonly kind: "invalid" }
> {
  try {
    return { kind: "present", stats: await fileSystem.lstat(target) };
  } catch (error) {
    return isNotFoundError(error) ? { kind: "absent" } : { kind: "invalid" };
  }
}

function isWithin(resolver: DesktopExportPathApi, root: string, target: string): boolean {
  const relative = resolver.relative(root, target);
  return relative === "" || relative === "."
    || (relative !== ".."
      && !relative.startsWith(`..${resolver.sep}`)
      && !resolver.isAbsolute(relative));
}

function suggestedFileName(timestamp: number): string | null {
  if (!Number.isFinite(timestamp) || timestamp < 0 || timestamp > 8.64e15) return null;
  return `kwiry-diagnostics-${new Date(timestamp).toISOString()
    .replaceAll("-", "")
    .replaceAll(":", "")
    .replace(/\.\d{3}Z$/u, "Z")}.txt`;
}

async function syncDirectoryBestEffort(
  fileSystem: DesktopExportFileSystem,
  directory: string,
): Promise<void> {
  let handle: DesktopExportFileHandle | null = null;
  try {
    handle = await fileSystem.open(directory, "r");
    await handle.sync();
  } catch {
    // Directory fsync is unavailable on some desktop platforms.
  } finally {
    if (handle !== null) {
      try {
        await handle.close();
      } catch {
        // Publication already completed; no path or exception crosses the adapter.
      }
    }
  }
}

function loadElectronRemote(): DesktopElectronRemote | null {
  try {
    return require("@electron/remote") as DesktopElectronRemote;
  } catch {
    try {
      return (require("electron") as ElectronShape).remote ?? null;
    } catch {
      return null;
    }
  }
}

function unavailableHost(): DesktopDiagnosticsExportHost {
  return {
    save: async () => ({ kind: "unavailable" }),
  };
}

function isNotFoundError(error: unknown): boolean {
  return typeof error === "object" && error !== null
    && "code" in error && (error as { code?: unknown }).code === "ENOENT";
}
