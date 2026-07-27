// SPDX-FileCopyrightText: 2026 cybersader
// SPDX-License-Identifier: GPL-3.0-only

/**
 * One logical VFS file held in fixed-size, uncompressed JavaScript blocks.
 * The blocks live outside SQLite's WASM linear memory.
 */
export class BlockFile {
  readonly blocks = new Map<number, Uint8Array>();
  size = 0;
  reads = 0;
  writes = 0;

  constructor(
    readonly name: string,
    readonly blockSize: number,
  ) {
    if (!Number.isSafeInteger(blockSize) || blockSize < 1) {
      throw new Error("block size must be a positive integer");
    }
  }

  static fromBytes(name: string, bytes: Uint8Array, blockSize: number): BlockFile {
    const file = new BlockFile(name, blockSize);
    file.size = bytes.byteLength;
    for (let offset = 0, index = 0; offset < bytes.byteLength; offset += blockSize, index += 1) {
      const end = Math.min(offset + blockSize, bytes.byteLength);
      // Detached copy: the transferred request buffer may be released as soon as
      // restore finishes, while this file must retain independently owned bytes.
      file.blocks.set(index, new Uint8Array(bytes.subarray(offset, end)));
    }
    return file;
  }

  residentBytes(): number {
    let total = 0;
    for (const block of this.blocks.values()) total += block.byteLength;
    return total;
  }

  read(destination: Uint8Array, offset: number): number {
    this.requireRange(offset, destination.byteLength);
    this.reads += 1;
    const limit = Math.max(0, Math.min(destination.byteLength, this.size - offset));
    let done = 0;
    while (done < limit) {
      const absolute = offset + done;
      const index = Math.floor(absolute / this.blockSize);
      const start = absolute % this.blockSize;
      const want = Math.min(limit - done, this.blockSize - start);
      const block = this.blocks.get(index);
      const available = block === undefined
        ? 0
        : Math.max(0, Math.min(want, block.byteLength - start));
      if (available > 0 && block !== undefined) {
        destination.set(block.subarray(start, start + available), done);
      }
      // Missing sparse blocks and the unmaterialized suffix of a short final
      // block read as zeroes, exactly like a regular file.
      if (available < want) destination.fill(0, done + available, done + want);
      done += want;
    }
    return done;
  }

  write(source: Uint8Array, offset: number): number {
    this.requireRange(offset, source.byteLength);
    this.writes += 1;
    let done = 0;
    while (done < source.byteLength) {
      const absolute = offset + done;
      const index = Math.floor(absolute / this.blockSize);
      const start = absolute % this.blockSize;
      const want = Math.min(source.byteLength - done, this.blockSize - start);
      const block = this.ensureWritable(index);
      block.set(source.subarray(done, done + want), start);
      done += want;
    }
    this.size = Math.max(this.size, offset + source.byteLength);
    return done;
  }

  truncate(size: number): void {
    if (!Number.isSafeInteger(size) || size < 0) throw new Error("invalid file size");
    if (size < this.size) {
      const boundaryIndex = Math.floor(size / this.blockSize);
      const boundaryOffset = size % this.blockSize;
      for (const index of [...this.blocks.keys()]) {
        if (index > boundaryIndex || (boundaryOffset === 0 && index === boundaryIndex)) {
          this.blocks.delete(index);
        }
      }
      if (boundaryOffset !== 0) {
        const boundary = this.blocks.get(boundaryIndex);
        if (boundary !== undefined && boundary.byteLength > boundaryOffset) {
          boundary.fill(0, boundaryOffset);
        }
      }
    }
    this.size = size;
  }

  toBytes(): Uint8Array {
    const out = new Uint8Array(this.size);
    let offset = 0;
    while (offset < this.size) {
      const index = Math.floor(offset / this.blockSize);
      const start = offset % this.blockSize;
      const want = Math.min(this.size - offset, this.blockSize - start);
      const block = this.blocks.get(index);
      if (block !== undefined) {
        const available = Math.max(0, Math.min(want, block.byteLength - start));
        if (available > 0) out.set(block.subarray(start, start + available), offset);
      }
      offset += want;
    }
    return out;
  }

  release(): void {
    for (const block of this.blocks.values()) block.fill(0);
    this.blocks.clear();
    this.size = 0;
  }

  private ensureWritable(index: number): Uint8Array {
    const existing = this.blocks.get(index);
    if (existing !== undefined && existing.byteLength === this.blockSize) return existing;
    const block = new Uint8Array(this.blockSize);
    if (existing !== undefined) {
      block.set(existing.subarray(0, Math.min(existing.byteLength, this.blockSize)));
    }
    this.blocks.set(index, block);
    return block;
  }

  private requireRange(offset: number, length: number): void {
    if (!Number.isSafeInteger(offset)
      || offset < 0
      || !Number.isSafeInteger(length)
      || length < 0
      || !Number.isSafeInteger(offset + length)) {
      throw new Error("invalid file range");
    }
  }
}
