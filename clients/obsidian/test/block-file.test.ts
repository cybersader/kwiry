// SPDX-FileCopyrightText: 2026 cybersader
// SPDX-License-Identifier: GPL-3.0-only

import { describe, expect, it } from "vitest";

import { BlockFile } from "../src/worker/block-file";

describe("BlockFile", () => {
  it("reads and writes across block boundaries with a detached final block", () => {
    const input = Uint8Array.from({ length: 13 }, (_, index) => index + 1);
    const file = BlockFile.fromBytes("main", input, 8);
    input.fill(99);

    const read = new Uint8Array(10);
    expect(file.read(read, 5)).toBe(8);
    expect([...read]).toEqual([6, 7, 8, 9, 10, 11, 12, 13, 0, 0]);

    expect(file.write(new Uint8Array([41, 42, 43, 44]), 7)).toBe(4);
    expect([...file.toBytes()]).toEqual([1, 2, 3, 4, 5, 6, 7, 41, 42, 43, 44, 12, 13]);
  });

  it("implements sparse zero-fill and truncate-then-reextend semantics", () => {
    const file = new BlockFile("scratch", 8);
    file.write(new Uint8Array([7, 8]), 14);
    expect([...file.toBytes()]).toEqual([
      0, 0, 0, 0, 0, 0, 0, 0,
      0, 0, 0, 0, 0, 0, 7, 8,
    ]);

    file.truncate(7);
    file.truncate(16);
    const read = new Uint8Array(16);
    expect(file.read(read, 0)).toBe(16);
    expect([...read]).toEqual(new Array(16).fill(0));
  });

  it("releases every retained block and is safe to release twice", () => {
    const file = BlockFile.fromBytes("main", new Uint8Array(20).fill(5), 8);
    const retained = [...file.blocks.values()];
    expect(file.residentBytes()).toBe(20);
    file.release();
    file.release();
    expect(file.size).toBe(0);
    expect(file.blocks.size).toBe(0);
    expect(retained.every((block) => block.every((byte) => byte === 0))).toBe(true);
  });
});
