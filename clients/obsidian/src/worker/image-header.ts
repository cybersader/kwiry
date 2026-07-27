// SPDX-FileCopyrightText: 2026 cybersader
// SPDX-License-Identifier: GPL-3.0-only

const SQLITE_MAGIC = "SQLite format 3\0";

export interface SQLiteImageHeader {
  pageSize: number;
  pageCount: number;
  wal: boolean;
  writeVersion: number;
  readVersion: number;
}

/** O(1) structural guard ported from the proven plain-block prototype. */
export function validateSQLiteImage(bytes: Uint8Array): SQLiteImageHeader {
  if (!(bytes instanceof Uint8Array)) throw new TypeError("database image must be a Uint8Array");
  if (bytes.byteLength < 512) {
    throw new Error(`not a SQLite image: ${bytes.byteLength} bytes, minimum header is 512`);
  }
  for (let index = 0; index < 16; index += 1) {
    if (bytes[index] !== SQLITE_MAGIC.charCodeAt(index)) {
      throw new Error(`not a SQLite image: header magic mismatch at byte ${index}`);
    }
  }
  const view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
  let pageSize = view.getUint16(16);
  if (pageSize === 1) pageSize = 65_536;
  if (pageSize < 512 || pageSize > 65_536 || (pageSize & (pageSize - 1)) !== 0) {
    throw new Error(`corrupt SQLite image: illegal page size ${pageSize}`);
  }
  if (bytes.byteLength % pageSize !== 0) {
    throw new Error(
      `truncated SQLite image: ${bytes.byteLength} bytes is not a multiple of page size ${pageSize}`,
    );
  }
  const writeVersion = bytes[18]!;
  const readVersion = bytes[19]!;
  if (writeVersion < 1 || writeVersion > 2 || readVersion < 1 || readVersion > 2) {
    throw new Error(
      `corrupt SQLite image: illegal file format versions write=${writeVersion} read=${readVersion}`,
    );
  }
  const changeCounter = view.getUint32(24);
  const headerPages = view.getUint32(28);
  const validFor = view.getUint32(92);
  if (headerPages !== 0 && changeCounter === validFor) {
    const expected = headerPages * pageSize;
    if (expected !== bytes.byteLength) {
      throw new Error(
        `truncated SQLite image: header declares ${headerPages} pages x ${pageSize} = `
        + `${expected} bytes, blob has ${bytes.byteLength}`,
      );
    }
  }
  return {
    pageSize,
    pageCount: bytes.byteLength / pageSize,
    wal: writeVersion === 2 || readVersion === 2,
    writeVersion,
    readVersion,
  };
}
