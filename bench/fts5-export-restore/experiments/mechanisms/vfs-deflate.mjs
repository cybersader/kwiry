// Mechanism: JS block VFS, deflate-compressed blocks + LRU of inflated blocks.
//
// Same block VFS as `vfs-plain.mjs` (both port `../vfs.mjs`, and both now share
// `_block-file.mjs` / `_block-vfs.mjs` so the os layer cannot drift), except
// that the restored image is held as `deflateRawSync` blocks in JS memory and
// blocks are inflated on demand into a small LRU. Dirty blocks are stored
// uncompressed (copy-on-write) so writes never pay a re-compress cost, which
// means the resident cost of a mutated database drifts toward the plain
// mechanism as more blocks are touched.
//
// Node-only: uses `node:zlib` for deflateRawSync/inflateRawSync. A browser
// port would swap in CompressionStream, which is async and would change the
// xRead path — that is why this stays an experiment.

import { deflateRawSync, inflateRawSync } from 'node:zlib';

import { BlockFileBase } from './_block-file.mjs';
import { installBlockVfs } from './_block-vfs.mjs';
import { HEADER_GUARD_CAVEAT, runQuickCheck, validateImage } from './_image-header.mjs';

export const meta = {
  name: 'vfs-deflate',
  family: 'vfs',
  notes:
    'Custom sqlite3_vfs backed by deflateRawSync-compressed JS-side blocks ' +
    '(default 64 KiB, level 6) with an LRU of inflated blocks (default 64). ' +
    'Dirty blocks are stored uncompressed via copy-on-write. Applies PRAGMA ' +
    'journal_mode=MEMORY. exportBlob() reassembles from JS blocks — no wasm ' +
    'copy — and is only consistent when no transaction is open. Lossless: an ' +
    'unmutated image exports byte-identical to its input. Node-only (node:zlib). ' +
    'WAL-mode images are rejected: these io methods are iVersion 1 with no ' +
    'xShmMap. ' +
    HEADER_GUARD_CAVEAT,
};

const DEFAULT_BLOCK = 65536;
const DEFAULT_LRU_BLOCKS = 64;
const DEFAULT_LEVEL = 6;

export { validateImage };

/// A file whose clean blocks are held deflate-compressed and inflated on
/// demand into a bounded LRU; dirty blocks are held uncompressed.
class CompressedBlockFile extends BlockFileBase {
  constructor(name, blockSize, lruBlocks, level) {
    super(name, blockSize);
    this.lruBlocks = lruBlocks;
    this.level = level;
    this.compressed = new Map(); // index -> Uint8Array (deflateRaw)
    this.lru = new Map(); // index -> Uint8Array (inflated, insertion-ordered)
    this.inflates = 0;
    this.lruHits = 0;
  }

  static fromBytes(name, bytes, blockSize, lruBlocks, level) {
    const file = new CompressedBlockFile(name, blockSize, lruBlocks, level);
    file.size = bytes.byteLength;
    for (let offset = 0, index = 0; offset < bytes.byteLength; offset += blockSize, index += 1) {
      const end = Math.min(offset + blockSize, bytes.byteLength);
      file.compressed.set(index, deflateRawSync(bytes.subarray(offset, end), { level }));
    }
    return file;
  }

  compressedBytes() {
    let total = 0;
    for (const block of this.compressed.values()) total += block.byteLength;
    return total;
  }

  residentBytes() {
    let total = this.compressedBytes();
    for (const block of this.blocks.values()) total += block.byteLength;
    for (const block of this.lru.values()) total += block.byteLength;
    return total;
  }

  getBlock(index) {
    const dirty = this.blocks.get(index);
    if (dirty) return dirty;
    const packed = this.compressed.get(index);
    if (!packed) return null;
    const cached = this.lru.get(index);
    if (cached) {
      // Refresh recency: delete + re-set moves it to the end of the Map order.
      this.lru.delete(index);
      this.lru.set(index, cached);
      this.lruHits += 1;
      return cached;
    }
    const inflated = new Uint8Array(inflateRawSync(packed));
    this.inflates += 1;
    this.lru.set(index, inflated);
    while (this.lru.size > this.lruBlocks) {
      const oldest = this.lru.keys().next().value;
      this.lru.delete(oldest);
    }
    return inflated;
  }

  /// Copy-on-write: promote a block to the uncompressed dirty set.
  ensureWritable(index) {
    const dirty = this.blocks.get(index);
    if (dirty && dirty.length === this.blockSize) return dirty;
    const source = dirty ?? this.getBlock(index);
    const full = new Uint8Array(this.blockSize);
    if (source) full.set(source.subarray(0, Math.min(source.length, this.blockSize)));
    this.blocks.set(index, full);
    this.compressed.delete(index);
    this.lru.delete(index);
    return full;
  }

  truncate(size) {
    super.truncate(size);
    const last = Math.floor(size / this.blockSize);
    for (const key of [...this.compressed.keys()]) if (key > last) this.compressed.delete(key);
    this.lru.clear();
  }

  release() {
    super.release();
    this.compressed.clear();
    this.lru.clear();
  }
}

/// Restore `blobBytes` behind a private compressed block VFS.
///
/// options:
///   blockSize  — bytes per JS block (default 65536)
///   lruBlocks  — inflated blocks kept resident (default 64)
///   level      — deflate level (default 6)
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
  const lruBlocks = Number(options.lruBlocks ?? DEFAULT_LRU_BLOCKS);
  const level = Number(options.level ?? DEFAULT_LEVEL);
  if (!Number.isInteger(blockSize) || blockSize < 512) {
    throw new Error(`illegal blockSize ${options.blockSize}`);
  }
  if (!Number.isInteger(lruBlocks) || lruBlocks < 1) {
    throw new Error(`illegal lruBlocks ${options.lruBlocks}`);
  }

  const dbFileName = '/kwir-cache.db';
  let mainFile = CompressedBlockFile.fromBytes(dbFileName, blobBytes, blockSize, lruBlocks, level);

  const installed = installBlockVfs(sqlite3, {
    namePrefix: 'kwir-blk-zip',
    dbFileName,
    mainFile,
    // Scratch files (journals, temp b-trees) are never worth compressing, but
    // they use the same class so the os layer stays uniform.
    createScratchFile: (name) => new CompressedBlockFile(name, blockSize, lruBlocks, level),
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
        lru_blocks: lruBlocks,
        level,
        compressed_blocks: mainFile.compressed.size,
        dirty_blocks: mainFile.blocks.size,
        lru_resident: mainFile.lru.size,
        compressed_bytes: mainFile.compressedBytes(),
        resident_bytes: mainFile.residentBytes(),
        file_bytes: mainFile.size,
        block_reads: mainFile.reads,
        block_writes: mainFile.writes,
        inflates: mainFile.inflates,
        lru_hits: mainFile.lruHits,
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
