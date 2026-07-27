// SPDX-FileCopyrightText: 2026 cybersader
// SPDX-License-Identifier: GPL-3.0-only

import sqlite3InitModule from "@sqlite.org/sqlite-wasm";
import { beforeAll, describe, expect, it, vi } from "vitest";

import { openFts5Generation, type SQLiteApi } from "../src/worker/fts5-index";
import { validateSQLiteImage } from "../src/worker/image-header";

let image: Uint8Array;

beforeAll(async () => {
  const warning = vi.spyOn(console, "warn").mockImplementation(() => undefined);
  const failure = vi.spyOn(console, "error").mockImplementation(() => undefined);
  try {
    const initialize = sqlite3InitModule as unknown as (options: {
      print: () => void;
      printErr: () => void;
    }) => Promise<SQLiteApi>;
    const sqlite = await initialize({ print: () => undefined, printErr: () => undefined });
    const index = openFts5Generation(sqlite);
    try {
      image = index.exportImage(sqlite);
    } finally {
      index.close();
    }
  } finally {
    warning.mockRestore();
    failure.mockRestore();
  }
});

describe("validateSQLiteImage", () => {
  it("accepts the intact exported image", () => {
    expect(validateSQLiteImage(image)).toMatchObject({ wal: false, writeVersion: 1, readVersion: 1 });
  });

  it.each([
    ["short image", () => image.subarray(0, 128)],
    ["magic mismatch", () => changed(0, image[0]! ^ 0xff)],
    ["illegal page size", () => {
      const copy = image.slice();
      copy[16] = 0;
      copy[17] = 3;
      return copy;
    }],
    ["unaligned truncation", () => image.slice(0, image.byteLength - 1)],
    ["page-aligned truncation", () => {
      const pageSize = validateSQLiteImage(image).pageSize;
      return image.slice(0, image.byteLength - pageSize);
    }],
  ])("rejects %s at the header guard", (_name, corrupt) => {
    expect(() => validateSQLiteImage(corrupt())).toThrow();
  });

  it.each([18, 19])("reports WAL format when header byte %i is version 2", (offset) => {
    const wal = changed(offset, 2);
    expect(validateSQLiteImage(wal).wal).toBe(true);
  });
});

function changed(offset: number, value: number): Uint8Array {
  const copy = image.slice();
  copy[offset] = value;
  return copy;
}
