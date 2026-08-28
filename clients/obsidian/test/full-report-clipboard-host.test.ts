// SPDX-FileCopyrightText: 2026 cybersader
// SPDX-License-Identifier: GPL-3.0-only

import { describe, expect, it } from "vitest";

import { DIAGNOSTIC_EXPORT_MAX_BYTES } from "../src/diagnostics/export-contract";
import {
  createFullReportClipboardHost,
  type FullReportClipboardHost,
} from "../src/diagnostics/full-report-clipboard-host";

const encoder = new TextEncoder();

function hostWith(
  writeText: ((text: string) => Promise<void>) | null,
): FullReportClipboardHost {
  return createFullReportClipboardHost({ writeText });
}

function chunks(...values: string[]): Iterable<Uint8Array> {
  return values.map((value) => encoder.encode(value));
}

function namedError(name: string): Error {
  const error = new Error("clipboard operation failed");
  error.name = name;
  return error;
}

describe("full diagnostics clipboard host", () => {
  it("fails closed without clipboard authority and never serializes", async () => {
    let iterated = false;
    const host = hostWith(null);

    expect(host.isAvailable()).toBe(false);
    await expect(host.copy({
      chunks: (function* () {
        iterated = true;
        yield encoder.encode("unused");
      })(),
    })).resolves.toEqual({ kind: "unavailable" });
    expect(iterated).toBe(false);
  });

  it("invokes clipboard authority synchronously and copies split UTF-8 exactly", async () => {
    const source = encoder.encode("alpha 🚀 omega");
    let copied = "";
    let invoked = false;
    let release!: () => void;
    const pending = new Promise<void>((resolve) => {
      release = resolve;
    });
    const host = hostWith((text) => {
      invoked = true;
      copied = text;
      return pending;
    });

    expect(host.isAvailable()).toBe(true);
    const result = host.copy({ chunks: [source.subarray(0, 8), source.subarray(8)] });
    expect(invoked).toBe(true);
    expect(copied).toBe("alpha 🚀 omega");
    release();
    await expect(result).resolves.toEqual({ kind: "saved" });
  });

  it("accepts exactly the full-export byte ceiling", async () => {
    let copiedBytes = -1;
    const host = hostWith(async (text) => {
      copiedBytes = encoder.encode(text).byteLength;
    });

    await expect(host.copy({
      chunks: [new Uint8Array(DIAGNOSTIC_EXPORT_MAX_BYTES)],
    })).resolves.toEqual({ kind: "saved" });
    expect(copiedBytes).toBe(DIAGNOSTIC_EXPORT_MAX_BYTES);
  });

  it("rejects an oversized report before clipboard access", async () => {
    let writes = 0;
    const host = hostWith(async () => {
      writes += 1;
    });

    await expect(host.copy({
      chunks: [
        new Uint8Array(DIAGNOSTIC_EXPORT_MAX_BYTES),
        new Uint8Array([0]),
      ],
    })).resolves.toEqual({ kind: "write_failed" });
    expect(writes).toBe(0);
  });

  it("rejects invalid chunks and malformed UTF-8 without clipboard access", async () => {
    let writes = 0;
    const host = hostWith(async () => {
      writes += 1;
    });

    await expect(host.copy({
      chunks: ["invalid"] as unknown as Iterable<Uint8Array>,
    })).resolves.toEqual({ kind: "write_failed" });
    await expect(host.copy({
      chunks: [new Uint8Array([0xff])],
    })).resolves.toEqual({ kind: "write_failed" });
    expect(writes).toBe(0);
  });

  it.each([
    ["AbortError", "cancelled"],
    ["NotAllowedError", "unavailable"],
    ["SecurityError", "unavailable"],
    ["UnknownError", "write_failed"],
  ] as const)("maps %s to the fixed %s outcome", async (name, kind) => {
    const host = hostWith(() => Promise.reject(namedError(name)));

    const result = await host.copy({ chunks: chunks("bounded report") });

    expect(result).toEqual({ kind });
    expect(Object.keys(result)).toEqual(["kind"]);
  });

  it("maps synchronous clipboard failures without exposing their details", async () => {
    const host = hostWith(() => {
      throw namedError("OperationError");
    });

    const result = await host.copy({ chunks: chunks("bounded report") });

    expect(result).toEqual({ kind: "write_failed" });
    expect(JSON.stringify(result)).not.toContain("clipboard operation failed");
  });
});
