// Shared block-file base for the block-VFS mechanisms.
//
// Helper module (leading underscore): `matrix.mjs` never registers it as a
// mechanism. It exists so `vfs-plain.mjs` and `vfs-deflate.mjs` cannot drift
// apart in the os-layer semantics they both depend on — the previous copies
// had already diverged.
//
// A block file holds the logical contents of one VFS file as fixed-size
// blocks in JS memory (OUTSIDE wasm linear memory). Subclasses decide how a
// block is stored; the read/write/truncate/export semantics live here.

export class BlockFileBase {
  constructor(name, blockSize) {
    this.name = name;
    this.blockSize = blockSize;
    this.size = 0;
    this.blocks = new Map(); // index -> Uint8Array (uncompressed / dirty)
    this.reads = 0;
    this.writes = 0;
  }

  /// Bytes this file currently keeps resident in JS.
  residentBytes() {
    let total = 0;
    for (const block of this.blocks.values()) total += block.byteLength;
    return total;
  }

  /// Readable view of block `index`, or null when the block does not exist.
  getBlock(index) {
    return this.blocks.get(index) ?? null;
  }

  /// Full-size, writable block `index`, materializing it if needed.
  ensureWritable(index) {
    const block = this.blocks.get(index);
    if (block && block.length === this.blockSize) return block;
    const full = new Uint8Array(this.blockSize);
    if (block) full.set(block.subarray(0, Math.min(block.length, this.blockSize)));
    this.blocks.set(index, full);
    return full;
  }

  /// Copy up to `destination.length` bytes starting at `offset`. Reads are
  /// clamped to the logical file size, so a read past EOF returns a short
  /// count (the caller turns that into SQLITE_IOERR_SHORT_READ) instead of
  /// handing back stale bytes from a padded final block.
  read(destination, offset) {
    this.reads += 1;
    const limit = Math.max(0, Math.min(destination.length, this.size - offset));
    let done = 0;
    while (done < limit) {
      const index = Math.floor((offset + done) / this.blockSize);
      const start = (offset + done) % this.blockSize;
      const want = Math.min(limit - done, this.blockSize - start);
      const block = this.getBlock(index);
      if (!block) return done;
      const available = Math.max(0, Math.min(want, block.length - start));
      if (available > 0) destination.set(block.subarray(start, start + available), done);
      if (available < want) return done + available;
      done += want;
    }
    return done;
  }

  write(source, offset) {
    let done = 0;
    this.writes += 1;
    while (done < source.length) {
      const index = Math.floor((offset + done) / this.blockSize);
      const start = (offset + done) % this.blockSize;
      const want = Math.min(source.length - done, this.blockSize - start);
      const block = this.ensureWritable(index);
      block.set(source.subarray(done, done + want), start);
      done += want;
    }
    if (offset + source.length > this.size) this.size = offset + source.length;
    return done;
  }

  truncate(size) {
    this.size = size;
    const last = Math.floor(size / this.blockSize);
    for (const key of [...this.blocks.keys()]) if (key > last) this.blocks.delete(key);
  }

  /// Reassemble the logical file contents. Blocks that were never written are
  /// read back as zeroes (a sparse region), which is what a real file does.
  toBytes() {
    const out = new Uint8Array(this.size);
    let offset = 0;
    while (offset < this.size) {
      const index = Math.floor(offset / this.blockSize);
      const start = offset % this.blockSize;
      const want = Math.min(this.size - offset, this.blockSize - start);
      const block = this.getBlock(index);
      if (block) {
        const available = Math.max(0, Math.min(want, block.length - start));
        if (available > 0) out.set(block.subarray(start, start + available), offset);
      }
      offset += want;
    }
    return out;
  }

  /// Drop every retained byte. close() calls this so a closed handle cannot
  /// keep a whole database image alive through the closure that owns it.
  release() {
    this.blocks.clear();
    this.size = 0;
  }
}
