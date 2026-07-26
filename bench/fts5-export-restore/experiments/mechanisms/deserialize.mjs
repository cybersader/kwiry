// Mechanism: sqlite3_deserialize (the current production restore path).
//
// Wraps `sqlite3_deserialize` with SQLITE_DESERIALIZE_FREEONCLOSE |
// SQLITE_DESERIALIZE_RESIZEABLE, exactly as `src/worker.mjs`'s
// `deserializeInto` does. The whole database image is copied into WASM
// linear memory and stays there for the lifetime of the handle, so the wasm
// floor is at least the image size; export copies the image a second time
// inside wasm before slicing it out to JS.
//
// Interface: see the sibling `vfs-plain.mjs` / `vfs-deflate.mjs` modules —
// all three export the same `meta` / `open()` shape so they are swappable,
// and all three share `_image-header.mjs` for validation.

import { HEADER_GUARD_CAVEAT, runQuickCheck, validateImage } from './_image-header.mjs';

export const meta = {
  name: 'deserialize',
  family: 'deserialize',
  notes:
    'sqlite3_deserialize(FREEONCLOSE|RESIZEABLE) on an :memory: handle, as in ' +
    'src/worker.mjs deserializeInto. Image lives inside wasm linear memory; ' +
    'exportBlob() uses sqlite3_js_db_export, which allocates a second full ' +
    'copy inside wasm before slicing to JS. No pragmas are applied, so the ' +
    'handle matches the production restore worker byte for byte. ' +
    HEADER_GUARD_CAVEAT,
};

export { validateImage };

/// Restore `blobBytes` through sqlite3_deserialize.
///
/// options:
///   validate — 'quick' additionally runs PRAGMA quick_check inside open()
export async function open(sqlite3, blobBytes, options = {}) {
  const image = validateImage(blobBytes);
  // Only the derived numbers are captured below: closing over `blobBytes`
  // would pin the caller's whole image in JS for the life of the handle, on
  // top of the copy sqlite3_deserialize makes inside wasm, and would inflate
  // the very axis this mechanism is being compared on.
  const imageBytes = blobBytes.byteLength;
  const capi = sqlite3.capi;
  const wasm = sqlite3.wasm;

  const db = new sqlite3.oo1.DB();
  try {
    const pointer = wasm.allocFromTypedArray(blobBytes);
    const flags =
      (capi.SQLITE_DESERIALIZE_FREEONCLOSE ?? 1) | (capi.SQLITE_DESERIALIZE_RESIZEABLE ?? 2);
    // With FREEONCLOSE set, SQLite owns `pointer` from here on — including on
    // the error paths inside sqlite3_deserialize — so this function must not
    // free it itself.
    const resultCode = capi.sqlite3_deserialize(
      db.pointer,
      'main',
      pointer,
      imageBytes,
      imageBytes,
      flags,
    );
    db.checkRc(resultCode);
    // Force page 1 and the schema to be parsed now, so a blob that survived
    // the structural check but has an unreadable schema fails inside open()
    // rather than at first query.
    db.selectValue('SELECT count(*) FROM sqlite_schema');
    if (options.validate === 'quick') runQuickCheck(db);
  } catch (error) {
    try {
      db.close();
    } catch {
      /* the original failure is the interesting one */
    }
    throw error;
  }

  let closed = false;
  const live = () => {
    if (closed) throw new Error('mechanism handle already closed');
  };

  return {
    db,
    wasmFloorProbe() {
      return wasm.memory.buffer.byteLength;
    },
    exportBlob() {
      live();
      return capi.sqlite3_js_db_export(db);
    },
    stats() {
      live();
      return {
        mechanism: meta.name,
        image_bytes: imageBytes,
        page_size: image.pageSize,
        page_count: image.pageCount,
        wal: image.wal,
        // Deliberately absent: resident_bytes. This mechanism keeps the image
        // inside wasm linear memory, so what it holds is only observable as
        // wasm growth — the runner derives it from the wasm probes rather than
        // this module reporting a number it cannot measure.
      };
    },
    close() {
      if (closed) return;
      closed = true;
      db.close();
    },
  };
}
