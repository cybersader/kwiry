// SPDX-FileCopyrightText: 2026 cybersader
// SPDX-License-Identifier: GPL-3.0-only
//
// The full clipboard fallback is selected only when the hardened desktop file
// host cannot be composed. It receives sanitized serialized chunks, never a
// vault, path, query, or arbitrary diagnostic annotation.

import {
  DIAGNOSTIC_EXPORT_MAX_BYTES,
  type DiagnosticsExportResult,
} from "./export-contract";

export interface FullReportClipboardRequest {
  readonly chunks: Iterable<Uint8Array>;
}

export interface FullReportClipboardHost {
  isAvailable(): boolean;
  copy(request: FullReportClipboardRequest): Promise<DiagnosticsExportResult>;
}

export interface FullReportClipboardDependencies {
  readonly writeText: ((text: string) => Promise<void>) | null;
}

export function createFullReportClipboardHost(
  dependencies: FullReportClipboardDependencies,
): FullReportClipboardHost {
  return {
    isAvailable: () => dependencies.writeText !== null,
    copy: (request) => copyFullReport(request, dependencies),
  };
}

export function createProductionFullReportClipboardHost(): FullReportClipboardHost {
  let writeText: FullReportClipboardDependencies["writeText"] = null;
  try {
    const clipboard = globalThis.navigator?.clipboard;
    if (clipboard && typeof clipboard.writeText === "function") {
      writeText = clipboard.writeText.bind(clipboard);
    }
  } catch {
    // Capability remains unavailable without exposing the platform failure.
  }
  return createFullReportClipboardHost({ writeText });
}

function copyFullReport(
  request: FullReportClipboardRequest,
  dependencies: FullReportClipboardDependencies,
): Promise<DiagnosticsExportResult> {
  if (dependencies.writeText === null) {
    return Promise.resolve({ kind: "unavailable" });
  }

  let report: string;
  try {
    const decoder = new TextDecoder("utf-8", { fatal: true });
    const parts: string[] = [];
    let totalBytes = 0;
    for (const chunk of request.chunks) {
      if (!(chunk instanceof Uint8Array)) throw new TypeError("invalid diagnostic export chunk");
      totalBytes += chunk.byteLength;
      if (!Number.isSafeInteger(totalBytes) || totalBytes > DIAGNOSTIC_EXPORT_MAX_BYTES) {
        throw new RangeError("diagnostic export exceeds byte limit");
      }
      parts.push(decoder.decode(chunk, { stream: true }));
    }
    parts.push(decoder.decode());
    report = parts.join("");
  } catch {
    return Promise.resolve({ kind: "write_failed" });
  }

  let write: Promise<void>;
  try {
    // Invoke synchronously in the settings-button activation task. Awaiting or
    // deferring before this call can revoke clipboard permission.
    write = dependencies.writeText(report);
  } catch (error) {
    return Promise.resolve(classifyClipboardFailure(error));
  }
  return Promise.resolve(write).then(
    () => ({ kind: "saved" }),
    (error: unknown) => classifyClipboardFailure(error),
  );
}

function classifyClipboardFailure(error: unknown): DiagnosticsExportResult {
  const name = errorName(error);
  if (name === "AbortError") return { kind: "cancelled" };
  if (name === "NotAllowedError" || name === "SecurityError") {
    return { kind: "unavailable" };
  }
  return { kind: "write_failed" };
}

function errorName(error: unknown): string | null {
  if (typeof error !== "object" || error === null || !("name" in error)) return null;
  const name = (error as { readonly name?: unknown }).name;
  return typeof name === "string" ? name : null;
}
