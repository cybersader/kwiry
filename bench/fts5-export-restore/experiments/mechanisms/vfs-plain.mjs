// Mechanism: JS block VFS, uncompressed 64 KiB blocks.
//
// Ports the proven prototype in `../vfs.mjs` (MODE=plain) behind the shared
// mechanism interface. The database image lives in JS-side blocks OUTSIDE
// wasm linear memory; pages are copied into the wasm heap on demand by
// xRead, so the wasm floor stays at the runtime's own footprint plus the
// SQLite page cache regardless of database size. exportBlob() reads the JS
// blocks directly, which also removes the full-image wasm copy that
// sqlite3_js_db_export needs.
//
// Each open() registers its own uniquely named sqlite3_vfs and disposes it in
// close(), so open() is repeatable within one process and several handles may
// be live at once without colliding.

import { BlockFileBase } from './_block-file.mjs';
import { installBlockVfs } from './_block-vfs.mjs';
import { HEADER_GUARD_CAVEAT, runQuickCheck, validateImage } from './_image-header.mjs';

export const meta = {
  name: 'vfs-plain',
  family: 'vfs',
  notes:
    'Custom sqlite3_vfs backed by uncompressed JS-side blocks (default 64 KiB) ' +
    'held outside wasm linear memory. Applies PRAGMA journal_mode=MEMORY (as in ' +
    'experiments/vfs.mjs) so rollback journals never round-trip through the block ' +
    'store. exportBlob() concatenates the JS blocks — no wasm copy — and is only ' +
    'consistent when no transaction is open. Lossless: an unmutated image exports ' +
    'byte-identical to its input. WAL-mode images are rejected: these io methods ' +
    'are iVersion 1 with no xShmMap. ' +
    HEADER_GUARD_CAVEAT,
};

const DEFAULT_BLOCK = 65536;

export { validateImage };

/// A file whose contents live in JS memory as fixed-size uncompressed blocks.
class BlockFile extends BlockFileBase {
  static fromBytes(name, bytes, blockSize) {
    const file = new BlockFile(name, blockSize);
    file.size = bytes.byteLength;
    for (let offset = 0, index = 0; offset < bytes.byteLength; offset += blockSize, index += 1) {
      const end = Math.min(offset + blockSize, bytes.byteLength);
      // Detached copy: the caller's blob may be released afterwards.
      file.blocks.set(index, new Uint8Array(bytes.subarray(offset, end)));
    }
    return file;
  }
}

/// Restore `blobBytes` behind a private block VFS.
///
/// options:
///   blockSize  — bytes per JS block (default 65536)
///   cacheSize  — value for PRAGMA cache_size (default: SQLite's own)
///   validate   — 'quick' additionally runs PRAGMA quick_check inside open()
export async function open(sqlite3, blobBytes, options = {}) {
  const image = validateImage(blobBytes);
  if (image.wal) {
    throw new Error(
      'unsupported SQLite image: WAL mode (header write/read version 2) needs xShmMap, ' +
        'which this iVersion-1 block VFS does not implement',
    );
  }
  const wasm = sqlite3.wasm;
  const blockSize = Number(options.blockSize ?? DEFAULT_BLOCK);
  if (!Number.isInteger(blockSize) || blockSize < 512) {
    throw new Error(`illegal blockSize ${options.blockSize}`);
  }

  const dbFileName = '/kwir-cache.db';
  let mainFile = BlockFile.fromBytes(dbFileName, blobBytes, blockSize);

  const installed = installBlockVfs(sqlite3, {
    namePrefix: 'kwir-blk-plain',
    dbFileName,
    mainFile,
    createScratchFile: (name) => new BlockFile(name, blockSize),
    cacheSize: options.cacheSize,
  });
  const { vfsName, db, teardown } = installed;

  if (options.validate === 'quick') {
    try {
      runQuickCheck(db);
    } catch (error) {
      teardown();
      mainFile = null;
      throw error;
    }
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
      return mainFile.toBytes();
    },
    stats() {
      live();
      return {
        mechanism: meta.name,
        block_size: blockSize,
        blocks: mainFile.blocks.size,
        resident_bytes: mainFile.residentBytes(),
        file_bytes: mainFile.size,
        block_reads: mainFile.reads,
        block_writes: mainFile.writes,
        vfs_name: vfsName,
      };
    },
    close() {
      if (closed) return;
      closed = true;
      // teardown() releases every block file, including mainFile; dropping
      // the reference as well means a retained handle cannot pin the image.
      teardown();
      mainFile = null;
    },
  };
}
