// SPDX-FileCopyrightText: 2026 cybersader
// SPDX-License-Identifier: GPL-3.0-only

import { TextDecoder } from "node:util";

const LOCAL_SIGNATURE = 0x04034b50;
const CENTRAL_SIGNATURE = 0x02014b50;
const END_SIGNATURE = 0x06054b50;
const STORE_METHOD = 0;
const FIXED_DOS_TIME = 0;
const FIXED_DOS_DATE = 33;
const utf8 = new TextDecoder("utf-8", { fatal: true });

export class StoredZipError extends Error {
  constructor(code) {
    super(code);
    this.name = "StoredZipError";
    this.code = code;
  }
}

/** Builds a deterministic UTF-8, store-only ZIP with fixed timestamps. */
export function buildStoredZip(entries) {
  if (!Array.isArray(entries) || entries.length === 0 || entries.length > 65_535) {
    throw new StoredZipError("zip_entry_count_invalid");
  }
  const names = new Set();
  const locals = [];
  const central = [];
  let offset = 0;
  for (const entry of entries) {
    const name = validateEntryName(entry?.name, { flat: false });
    if (names.has(name)) throw new StoredZipError("zip_entry_duplicate");
    names.add(name);
    const bytes = Buffer.from(entry?.bytes ?? []);
    if (bytes.byteLength > 0xffff_ffff) throw new StoredZipError("zip_entry_too_large");
    const nameBytes = Buffer.from(name, "utf8");
    if (nameBytes.byteLength > 65_535) throw new StoredZipError("zip_entry_name_too_long");
    const crc = crc32(bytes);

    const local = Buffer.alloc(30 + nameBytes.length);
    local.writeUInt32LE(LOCAL_SIGNATURE, 0);
    local.writeUInt16LE(20, 4);
    local.writeUInt16LE(0, 6);
    local.writeUInt16LE(STORE_METHOD, 8);
    local.writeUInt16LE(FIXED_DOS_TIME, 10);
    local.writeUInt16LE(FIXED_DOS_DATE, 12);
    local.writeUInt32LE(crc, 14);
    local.writeUInt32LE(bytes.length, 18);
    local.writeUInt32LE(bytes.length, 22);
    local.writeUInt16LE(nameBytes.length, 26);
    local.writeUInt16LE(0, 28);
    nameBytes.copy(local, 30);
    locals.push(local, bytes);

    const header = Buffer.alloc(46 + nameBytes.length);
    header.writeUInt32LE(CENTRAL_SIGNATURE, 0);
    header.writeUInt16LE(20, 4);
    header.writeUInt16LE(20, 6);
    header.writeUInt16LE(0, 8);
    header.writeUInt16LE(STORE_METHOD, 10);
    header.writeUInt16LE(FIXED_DOS_TIME, 12);
    header.writeUInt16LE(FIXED_DOS_DATE, 14);
    header.writeUInt32LE(crc, 16);
    header.writeUInt32LE(bytes.length, 20);
    header.writeUInt32LE(bytes.length, 24);
    header.writeUInt16LE(nameBytes.length, 28);
    header.writeUInt16LE(0, 30);
    header.writeUInt16LE(0, 32);
    header.writeUInt16LE(0, 34);
    header.writeUInt16LE(0, 36);
    header.writeUInt32LE(0, 38);
    header.writeUInt32LE(offset, 42);
    nameBytes.copy(header, 46);
    central.push(header);
    offset += local.length + bytes.length;
  }

  const centralBytes = Buffer.concat(central);
  const end = Buffer.alloc(22);
  end.writeUInt32LE(END_SIGNATURE, 0);
  end.writeUInt16LE(0, 4);
  end.writeUInt16LE(0, 6);
  end.writeUInt16LE(entries.length, 8);
  end.writeUInt16LE(entries.length, 10);
  end.writeUInt32LE(centralBytes.length, 12);
  end.writeUInt32LE(offset, 16);
  end.writeUInt16LE(0, 20);
  return Buffer.concat([...locals, centralBytes, end]);
}

/**
 * Parses the deliberately narrow ZIP dialect produced above. Every structural
 * fact is checked before entry bytes or names are returned to a caller.
 */
export function parseStoredZip(value, {
  flat = false,
  maxEntries = 256,
  maxEntryBytes = 512 * 1024 * 1024,
  maxTotalBytes = 768 * 1024 * 1024,
} = {}) {
  const bytes = Buffer.from(value);
  if (bytes.byteLength > maxTotalBytes) throw new StoredZipError("zip_total_too_large");
  const entries = [];
  const names = new Set();
  let offset = 0;

  while (offset + 4 <= bytes.length && bytes.readUInt32LE(offset) === LOCAL_SIGNATURE) {
    if (entries.length >= maxEntries) throw new StoredZipError("zip_entry_count_invalid");
    requireRange(bytes, offset, 30);
    const version = bytes.readUInt16LE(offset + 4);
    const flags = bytes.readUInt16LE(offset + 6);
    const method = bytes.readUInt16LE(offset + 8);
    const expectedCrc = bytes.readUInt32LE(offset + 14);
    const compressedSize = bytes.readUInt32LE(offset + 18);
    const size = bytes.readUInt32LE(offset + 22);
    const nameLength = bytes.readUInt16LE(offset + 26);
    const extraLength = bytes.readUInt16LE(offset + 28);
    if (version !== 20 || flags !== 0 || method !== STORE_METHOD || extraLength !== 0) {
      throw new StoredZipError("zip_entry_unsupported");
    }
    if (nameLength === 0) throw new StoredZipError("zip_entry_name_invalid");
    if (compressedSize !== size || size > maxEntryBytes) {
      throw new StoredZipError("zip_entry_size_invalid");
    }
    const nameStart = offset + 30;
    requireRange(bytes, nameStart, nameLength);
    let name;
    try {
      name = utf8.decode(bytes.subarray(nameStart, nameStart + nameLength));
    } catch {
      throw new StoredZipError("zip_entry_name_invalid");
    }
    name = validateEntryName(name, { flat });
    if (names.has(name)) throw new StoredZipError("zip_entry_duplicate");
    names.add(name);
    const dataStart = nameStart + nameLength;
    requireRange(bytes, dataStart, size);
    const entryBytes = bytes.subarray(dataStart, dataStart + size);
    if (crc32(entryBytes) !== expectedCrc) throw new StoredZipError("zip_entry_crc_invalid");
    entries.push({
      name,
      bytes: Buffer.from(entryBytes),
      crc32: expectedCrc,
      localOffset: offset,
      size,
    });
    offset = dataStart + size;
  }

  if (entries.length === 0) throw new StoredZipError("zip_entry_count_invalid");
  const centralOffset = offset;
  for (const entry of entries) {
    requireRange(bytes, offset, 46);
    if (bytes.readUInt32LE(offset) !== CENTRAL_SIGNATURE) {
      throw new StoredZipError("zip_central_invalid");
    }
    const madeBy = bytes.readUInt16LE(offset + 4);
    const version = bytes.readUInt16LE(offset + 6);
    const flags = bytes.readUInt16LE(offset + 8);
    const method = bytes.readUInt16LE(offset + 10);
    const crc = bytes.readUInt32LE(offset + 16);
    const compressedSize = bytes.readUInt32LE(offset + 20);
    const size = bytes.readUInt32LE(offset + 24);
    const nameLength = bytes.readUInt16LE(offset + 28);
    const extraLength = bytes.readUInt16LE(offset + 30);
    const commentLength = bytes.readUInt16LE(offset + 32);
    const disk = bytes.readUInt16LE(offset + 34);
    const internalAttributes = bytes.readUInt16LE(offset + 36);
    const externalAttributes = bytes.readUInt32LE(offset + 38);
    const localOffset = bytes.readUInt32LE(offset + 42);
    if (madeBy !== 20 || version !== 20 || flags !== 0 || method !== STORE_METHOD
      || extraLength !== 0 || commentLength !== 0 || disk !== 0
      || internalAttributes !== 0 || externalAttributes !== 0) {
      throw new StoredZipError("zip_central_unsupported");
    }
    const nameStart = offset + 46;
    requireRange(bytes, nameStart, nameLength);
    let name;
    try {
      name = utf8.decode(bytes.subarray(nameStart, nameStart + nameLength));
    } catch {
      throw new StoredZipError("zip_entry_name_invalid");
    }
    if (name !== entry.name || crc !== entry.crc32 || compressedSize !== entry.size
      || size !== entry.size || localOffset !== entry.localOffset) {
      throw new StoredZipError("zip_central_mismatch");
    }
    offset = nameStart + nameLength;
  }

  const centralSize = offset - centralOffset;
  requireRange(bytes, offset, 22);
  if (bytes.readUInt32LE(offset) !== END_SIGNATURE
    || bytes.readUInt16LE(offset + 4) !== 0
    || bytes.readUInt16LE(offset + 6) !== 0
    || bytes.readUInt16LE(offset + 8) !== entries.length
    || bytes.readUInt16LE(offset + 10) !== entries.length
    || bytes.readUInt32LE(offset + 12) !== centralSize
    || bytes.readUInt32LE(offset + 16) !== centralOffset
    || bytes.readUInt16LE(offset + 20) !== 0
    || offset + 22 !== bytes.length) {
    throw new StoredZipError("zip_end_invalid");
  }

  return entries.map(({ name, bytes: entryBytes }) => ({ name, bytes: entryBytes }));
}

function validateEntryName(value, { flat }) {
  if (typeof value !== "string" || value.length === 0 || value.includes("\0")
    || value.includes("\\") || value.startsWith("/") || /^[A-Za-z]:/u.test(value)) {
    throw new StoredZipError("zip_entry_name_invalid");
  }
  const components = value.split("/");
  if (components.some((component) => component.length === 0
    || component === "." || component === "..")) {
    throw new StoredZipError("zip_entry_name_invalid");
  }
  if (flat && components.length !== 1) throw new StoredZipError("zip_entry_name_invalid");
  return value;
}

function requireRange(bytes, offset, length) {
  if (!Number.isSafeInteger(offset) || !Number.isSafeInteger(length)
    || offset < 0 || length < 0 || offset + length > bytes.length) {
    throw new StoredZipError("zip_truncated");
  }
}

const CRC_TABLE = (() => {
  const table = new Uint32Array(256);
  for (let index = 0; index < 256; index += 1) {
    let value = index;
    for (let bit = 0; bit < 8; bit += 1) {
      value = value & 1 ? 0xedb88320 ^ (value >>> 1) : value >>> 1;
    }
    table[index] = value >>> 0;
  }
  return table;
})();

export function crc32(bytes) {
  let crc = 0xffffffff;
  for (const byte of bytes) crc = CRC_TABLE[(crc ^ byte) & 0xff] ^ (crc >>> 8);
  return (crc ^ 0xffffffff) >>> 0;
}
