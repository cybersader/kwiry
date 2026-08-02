// SPDX-FileCopyrightText: 2026 cybersader
// SPDX-License-Identifier: GPL-3.0-only

import { describe, expect, it } from "vitest";

import {
  encodeExactIdentifierMatch,
  encodeExactIdentifierToken,
} from "../src/worker/exact-identifier-token";

describe("exact identifier FTS token encoding", () => {
  it.each([
    ["f", "zmy"],
    ["fo", "zmzxq"],
    ["foo", "zmzxw6"],
    ["foob", "zmzxw6yq"],
    ["fooba", "zmzxw6ytb"],
    ["foobar", "zmzxw6ytboi"],
  ])("uses unpadded lowercase RFC 4648 base32 for %j", (value, expected) => {
    expect(encodeExactIdentifierToken(value)).toBe(expected);
  });

  it("encodes punctuation, whitespace, combining marks, emoji, and non-BMP scalars as one token", () => {
    const values = [
      "rfc 9110",
      "cve-2026-1234",
      "product/v2.4.1",
      "é",
      "🚀",
      "𐐷",
      "mixed\tidentifier:value@example.com",
    ];
    const tokens = values.map(encodeExactIdentifierToken);
    expect(tokens.every((token) => /^z[a-z2-7]+$/u.test(token))).toBe(true);
    expect(new Set(tokens).size).toBe(values.length);
    expect(encodeExactIdentifierMatch(values)).toBe(tokens.join(" "));
  });

  it("is injective at the maximum accepted identifier length without truncation", () => {
    const left = `${"a".repeat(4_095)}x`;
    const right = `${"a".repeat(4_095)}y`;
    const leftToken = encodeExactIdentifierToken(left);
    const rightToken = encodeExactIdentifierToken(right);
    expect(leftToken).not.toBe(rightToken);
    expect(leftToken.length).toBe(1 + Math.ceil(4_096 * 8 / 5));
    expect(rightToken.length).toBe(leftToken.length);
  });

  it("rejects empty and ill-formed UTF-16 input instead of introducing replacement collisions", () => {
    expect(() => encodeExactIdentifierToken("")).toThrow(/nonempty Unicode scalar/u);
    expect(() => encodeExactIdentifierToken("\ud800")).toThrow(/Unicode scalar/u);
    expect(() => encodeExactIdentifierToken("\udc00")).toThrow(/Unicode scalar/u);
  });
});
