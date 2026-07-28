// SPDX-FileCopyrightText: 2026 cybersader
// SPDX-License-Identifier: GPL-3.0-only

import { describe, expect, it } from "vitest";

import { canonicalMtimeNanos } from "../src/active-vault-source";

/// The exact check the Rust ABI applies. A value failing this is refused as
/// source_rejected, and the Worker aborts the whole batch rather than the one
/// source, so a single bad timestamp prevents any indexing at all.
const ABI_PATTERN = /^[0-9]{1,39}$/u;

describe("canonicalMtimeNanos", () => {
  it("converts an ordinary timestamp unchanged", () => {
    expect(canonicalMtimeNanos(1785253671659)).toBe("1785253671659000000");
  });

  it("produces an ABI-valid string for every hostile input", () => {
    // Field regression: a production vault on a network share failed every
    // build with source_rejected at the rust stage. A pre-epoch mtime made
    // this emit a leading minus; NaN and Infinity made BigInt throw.
    for (const hostile of [-1, -86_400_000, NaN, Infinity, -Infinity, 1e30, 0]) {
      const result = canonicalMtimeNanos(hostile);
      expect(ABI_PATTERN.test(result), `rejected for input ${hostile}: ${result}`).toBe(true);
    }
  });

  it("never throws, whatever the filesystem reports", () => {
    for (const hostile of [NaN, Infinity, -Infinity, Number.MAX_VALUE]) {
      expect(() => canonicalMtimeNanos(hostile)).not.toThrow();
    }
  });

  it("clamps rather than inventing a future timestamp", () => {
    // Clamping low is safe: the source looks stale, so it is read and hashed
    // instead of being skipped as unchanged.
    expect(canonicalMtimeNanos(-86_400_000)).toBe("0");
    expect(canonicalMtimeNanos(NaN)).toBe("0");
  });
});
