// SPDX-FileCopyrightText: 2026 cybersader
// SPDX-License-Identifier: GPL-3.0-only

const BASE32_ALPHABET = "abcdefghijklmnopqrstuvwxyz234567";
const TOKEN_PREFIX = "z";
const utf8Encoder = new TextEncoder();

/**
 * Encodes an already-normalized exact identifier as one lossless ASCII FTS5
 * token. The alphabet contains only ASCII alphanumerics, so the pinned `ascii`
 * tokenizer cannot split it and no query operator can be introduced.
 */
export function encodeExactIdentifierToken(identifier: string): string {
  if (identifier.length === 0 || !isUnicodeScalarString(identifier)) {
    throw new Error("exact identifier must be a nonempty Unicode scalar value");
  }
  const bytes = utf8Encoder.encode(identifier);
  let token = TOKEN_PREFIX;
  let buffer = 0;
  let bits = 0;
  for (const byte of bytes) {
    buffer = (buffer << 8) | byte;
    bits += 8;
    while (bits >= 5) {
      bits -= 5;
      token += BASE32_ALPHABET[(buffer >>> bits) & 31];
      buffer &= bits === 0 ? 0 : (1 << bits) - 1;
    }
  }
  if (bits > 0) token += BASE32_ALPHABET[(buffer << (5 - bits)) & 31];
  return token;
}

export function encodeExactIdentifierMatch(identifiers: readonly string[]): string {
  return identifiers.map(encodeExactIdentifierToken).join(" ");
}

function isUnicodeScalarString(value: string): boolean {
  for (let index = 0; index < value.length; index += 1) {
    const code = value.charCodeAt(index);
    if (code < 0xd800 || code > 0xdfff) continue;
    if (code > 0xdbff || index + 1 >= value.length) return false;
    const low = value.charCodeAt(index + 1);
    if (low < 0xdc00 || low > 0xdfff) return false;
    index += 1;
  }
  return true;
}
