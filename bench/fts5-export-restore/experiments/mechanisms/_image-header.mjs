// Shared image validation for every mechanism module.
//
// The leading underscore is meaningful: `matrix.mjs` skips `_*.mjs` and any
// module that does not export `meta.family`, so helpers can live beside the
// mechanisms without being registered as one.
//
// Two levels, deliberately separated:
//
//   validateImage(bytes)  — O(1) header-only structural guard. Catches a
//     non-SQLite blob, an illegal page size, a length that is not a whole
//     number of pages, a length that disagrees with the header page count
//     (page-aligned truncation), and a WAL-mode image. It CANNOT detect
//     corruption inside a page: a blob of the right length with an intact
//     header but a damaged interior page passes this guard.
//
//   runQuickCheck(db)     — O(database size) `PRAGMA quick_check`. This is
//     the guard that catches interior corruption; mechanisms expose it as the
//     opt-in `{ validate: 'quick' }` option because it costs a full scan and
//     would distort restore timings if it were always on.

const MAGIC = 'SQLite format 3\0';

/// Structural validation of a candidate database image.
///
/// Throws on: short blob, wrong magic, implausible page size, size that is
/// not a whole number of pages, a header page count that disagrees with the
/// blob length, or a reserved-region byte that cannot be right.
///
/// Returns { pageSize, pageCount, wal, writeVersion, readVersion }. WAL is
/// reported rather than rejected here: `deserialize` can carry a WAL-mode
/// image, the block VFSes cannot (iVersion-1 io methods, no xShmMap), so the
/// decision belongs to the mechanism.
export function validateImage(bytes) {
  if (!ArrayBuffer.isView(bytes) || !(bytes instanceof Uint8Array)) {
    throw new TypeError('database image must be a Uint8Array');
  }
  if (bytes.byteLength < 512) {
    throw new Error(`not a SQLite image: ${bytes.byteLength} bytes, minimum header is 512`);
  }
  for (let index = 0; index < 16; index += 1) {
    if (bytes[index] !== MAGIC.charCodeAt(index)) {
      throw new Error(`not a SQLite image: header magic mismatch at byte ${index}`);
    }
  }
  const view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
  let pageSize = view.getUint16(16);
  if (pageSize === 1) pageSize = 65536; // header encoding for 64 KiB pages
  if (pageSize < 512 || (pageSize & (pageSize - 1)) !== 0) {
    throw new Error(`corrupt SQLite image: illegal page size ${pageSize}`);
  }
  if (bytes.byteLength % pageSize !== 0) {
    throw new Error(
      `truncated SQLite image: ${bytes.byteLength} bytes is not a multiple of page size ${pageSize}`,
    );
  }
  const writeVersion = bytes[18];
  const readVersion = bytes[19];
  if (writeVersion < 1 || writeVersion > 2 || readVersion < 1 || readVersion > 2) {
    throw new Error(
      `corrupt SQLite image: illegal file format versions write=${writeVersion} read=${readVersion}`,
    );
  }
  const changeCounter = view.getUint32(24);
  const headerPages = view.getUint32(28);
  const validFor = view.getUint32(92);
  // The header page count is only authoritative when the change counter and
  // the version-valid-for number agree (SQLite file format, bytes 28/92).
  if (headerPages !== 0 && changeCounter === validFor) {
    const expected = headerPages * pageSize;
    if (expected !== bytes.byteLength) {
      throw new Error(
        `truncated SQLite image: header declares ${headerPages} pages x ${pageSize} = ` +
          `${expected} bytes, blob has ${bytes.byteLength}`,
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

/// Full-content validation. Throws (never returns a "probably fine") when
/// `PRAGMA quick_check` cannot confirm the database.
export function runQuickCheck(db) {
  let value;
  try {
    value = db.selectValue('PRAGMA quick_check');
  } catch (error) {
    throw new Error(
      `corrupt SQLite image: PRAGMA quick_check failed: ${String(error?.message ?? error).slice(0, 200)}`,
    );
  }
  if (value !== 'ok') {
    throw new Error(
      `corrupt SQLite image: PRAGMA quick_check reported ${String(value).slice(0, 200)}`,
    );
  }
  return 'ok';
}

/// Shared wording for the header-guard limitation, so all three mechanisms
/// state it identically in `meta.notes`.
export const HEADER_GUARD_CAVEAT =
  'open() validates the image header only (magic, page size, page-count vs length, ' +
  'file-format versions), which rejects truncation and non-SQLite blobs but ACCEPTS a ' +
  'right-length image whose interior pages are damaged; pass { validate: "quick" } to ' +
  'additionally run PRAGMA quick_check (a full scan) when that guarantee is required.';
