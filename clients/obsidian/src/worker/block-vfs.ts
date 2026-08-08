// SPDX-FileCopyrightText: 2026 cybersader
// SPDX-License-Identifier: GPL-3.0-only

import { BlockFile } from "./block-file";
import type { SQLiteApi, SQLiteDatabase, SQLiteStruct } from "./fts5-index";

let instanceSequence = 0;

export interface BlockVfsHandle {
  readonly db: SQLiteDatabase;
  readonly vfsName: string;
  exportImage(): Uint8Array;
  close(): void;
}

export class BlockVfsUnavailableError extends Error {
  constructor() {
    super("required SQLite block-VFS capability is unavailable");
    this.name = "BlockVfsUnavailableError";
  }
}

const MAIN_FILE_NAME = "/kwir-cache.db";
const BLOCK_SIZE = 65_536;

type Pointer = number | bigint;
type Callback = (...args: Pointer[]) => number;

/** Near-literal TypeScript port of the measured plain-block VFS prototype. */
export function openPlainBlockVfs(sqlite: SQLiteApi, bytes: Uint8Array): BlockVfsHandle {
  const { capi, wasm } = sqlite;
  if (!sqlite.vfs
    || typeof sqlite.vfs.installVfs !== "function"
    || typeof capi.sqlite3_vfs_find !== "function"
    || typeof capi.sqlite3_vfs_unregister !== "function"
    || !capi.sqlite3_vfs
    || !capi.sqlite3_io_methods
    || !capi.sqlite3_file
    || !wasm
    || typeof wasm.heap8u !== "function"
    || typeof wasm.poke64 !== "function") {
    throw new BlockVfsUnavailableError();
  }

  let vfsName = "";
  for (let attempt = 0; attempt <= 64 && vfsName === ""; attempt += 1) {
    instanceSequence += 1;
    const candidate = `kwir-blk-plain-${instanceSequence}-${Math.random().toString(36).slice(2, 8)}`;
    if (!capi.sqlite3_vfs_find(candidate)) vfsName = candidate;
  }
  if (vfsName === "") throw new BlockVfsUnavailableError();

  let mainFile: BlockFile | null = BlockFile.fromBytes(MAIN_FILE_NAME, bytes, BLOCK_SIZE);
  const files = new Map<string, BlockFile>([[MAIN_FILE_NAME, mainFile]]);
  const handles = new Map<string, { file: BlockFile; deleteOnClose: boolean }>();
  const jsVfs = new capi.sqlite3_vfs();
  const jsIo = new capi.sqlite3_io_methods();
  jsIo.$iVersion = 1;
  jsVfs.$iVersion = 2;
  jsVfs.$szOsFile = capi.sqlite3_file.structInfo.sizeof;
  jsVfs.$mxPathname = 1024;

  const key = (pointer: Pointer): string => String(pointer);
  const heap = (): Uint8Array => wasm.heap8u();
  const safe = (fallback: number, callback: Callback): Callback => (...args) => {
    try {
      return callback(...args);
    } catch {
      return fallback;
    }
  };

  const ioMethods = {
    xClose: safe(capi.SQLITE_IOERR, (pFile) => {
      const handle = handles.get(key(pFile));
      if (handle !== undefined) {
        handles.delete(key(pFile));
        if (handle.deleteOnClose) {
          files.delete(handle.file.name);
          handle.file.release();
        }
      }
      return capi.SQLITE_OK;
    }),
    xRead: safe(capi.SQLITE_IOERR_READ, (pFile, pDest, n, offset64) => {
      const handle = handles.get(key(pFile));
      if (handle === undefined) return capi.SQLITE_IOERR_READ;
      const count = Number(n);
      const start = Number(pDest);
      const destination = heap().subarray(start, start + count);
      const read = handle.file.read(destination, Number(offset64));
      if (read < count) {
        destination.fill(0, read, count);
        return capi.SQLITE_IOERR_SHORT_READ;
      }
      return capi.SQLITE_OK;
    }),
    xWrite: safe(capi.SQLITE_IOERR_WRITE, (pFile, pSrc, n, offset64) => {
      const handle = handles.get(key(pFile));
      if (handle === undefined) return capi.SQLITE_IOERR_WRITE;
      const count = Number(n);
      const start = Number(pSrc);
      // BlockFile.write copies before this callback returns.
      handle.file.write(heap().subarray(start, start + count), Number(offset64));
      return capi.SQLITE_OK;
    }),
    xTruncate: safe(capi.SQLITE_IOERR_TRUNCATE, (pFile, size) => {
      const handle = handles.get(key(pFile));
      if (handle === undefined) return capi.SQLITE_IOERR_TRUNCATE;
      handle.file.truncate(Number(size));
      return capi.SQLITE_OK;
    }),
    xSync: safe(capi.SQLITE_IOERR_FSYNC, () => capi.SQLITE_OK),
    xFileSize: safe(capi.SQLITE_IOERR_FSTAT, (pFile, pSize) => {
      const handle = handles.get(key(pFile));
      if (handle === undefined) return capi.SQLITE_IOERR_FSTAT;
      wasm.poke64(Number(pSize), BigInt(handle.file.size));
      return capi.SQLITE_OK;
    }),
    xLock: safe(capi.SQLITE_IOERR_LOCK, () => capi.SQLITE_OK),
    xUnlock: safe(capi.SQLITE_IOERR_UNLOCK, () => capi.SQLITE_OK),
    xCheckReservedLock: safe(capi.SQLITE_IOERR_CHECKRESERVEDLOCK, (_pFile, pOut) => {
      wasm.poke(Number(pOut), 0, "i32");
      return capi.SQLITE_OK;
    }),
    xFileControl: safe(capi.SQLITE_NOTFOUND, () => capi.SQLITE_NOTFOUND),
    xSectorSize: safe(4096, () => 4096),
    xDeviceCharacteristics: safe(0, () => (
      capi.SQLITE_IOCAP_ATOMIC
      | capi.SQLITE_IOCAP_SAFE_APPEND
      | capi.SQLITE_IOCAP_SEQUENTIAL
      | capi.SQLITE_IOCAP_POWERSAFE_OVERWRITE
    )),
  };

  const anonymousName = (): string => `/anon-${Math.random().toString(36).slice(2)}`;
  const readName = (pointer: Pointer): string | null => wasm.cstrToJs(Number(pointer));
  const vfsMethods = {
    xOpen: safe(capi.SQLITE_CANTOPEN, (_pVfs, zName, pFile, flags, pOutFlags) => {
      const name = Number(zName) === 0 ? anonymousName() : readName(zName);
      if (name === null) return capi.SQLITE_CANTOPEN;
      let file = files.get(name);
      const numericFlags = Number(flags);
      if (file === undefined) {
        if ((numericFlags & capi.SQLITE_OPEN_CREATE) === 0) return capi.SQLITE_CANTOPEN;
        file = new BlockFile(name, BLOCK_SIZE);
        files.set(name, file);
      }
      handles.set(key(pFile), {
        file,
        deleteOnClose: (numericFlags & capi.SQLITE_OPEN_DELETEONCLOSE) !== 0,
      });
      const sqliteFile = new capi.sqlite3_file(Number(pFile));
      sqliteFile.$pMethods = jsIo.pointer;
      sqliteFile.dispose();
      if (Number(pOutFlags) !== 0) wasm.poke(Number(pOutFlags), numericFlags, "i32");
      return capi.SQLITE_OK;
    }),
    xDelete: safe(capi.SQLITE_IOERR_DELETE, (_pVfs, zName) => {
      const name = readName(zName);
      if (name !== null) {
        const file = files.get(name);
        files.delete(name);
        if (file !== undefined && file !== mainFile) file.release();
      }
      return capi.SQLITE_OK;
    }),
    xAccess: safe(capi.SQLITE_IOERR_ACCESS, (_pVfs, zName, _flags, pOut) => {
      const name = readName(zName);
      wasm.poke(Number(pOut), name !== null && files.has(name) ? 1 : 0, "i32");
      return capi.SQLITE_OK;
    }),
    xFullPathname: safe(capi.SQLITE_CANTOPEN, (_pVfs, zName, nOut, pOut) => (
      wasm.cstrncpy(Number(pOut), Number(zName), Number(nOut)) < Number(nOut)
        ? capi.SQLITE_OK
        : capi.SQLITE_CANTOPEN
    )),
    xCurrentTime: safe(capi.SQLITE_IOERR, (_pVfs, pOut) => {
      wasm.poke(Number(pOut), 2440587.5 + Date.now() / 86_400_000, "double");
      return capi.SQLITE_OK;
    }),
    xCurrentTimeInt64: safe(capi.SQLITE_IOERR, (_pVfs, pOut) => {
      wasm.poke64(Number(pOut), BigInt(Math.round(2440587.5 * 86_400_000 + Date.now())));
      return capi.SQLITE_OK;
    }),
    xGetLastError: safe(0, () => 0),
  };

  const defaultPointer = capi.sqlite3_vfs_find(null);
  if (defaultPointer) {
    const defaultVfs: SQLiteStruct = new capi.sqlite3_vfs(defaultPointer);
    jsVfs.$xRandomness = defaultVfs.$xRandomness;
    jsVfs.$xSleep = defaultVfs.$xSleep;
    defaultVfs.dispose();
  }

  let registered = false;
  let database: SQLiteDatabase | null = null;
  let closed = false;
  const close = (): void => {
    if (closed) return;
    closed = true;
    if (database !== null) {
      try {
        database.close();
      } catch {
        // Continue releasing the VFS and JS blocks.
      }
      database = null;
    }
    if (registered) {
      try {
        capi.sqlite3_vfs_unregister(jsVfs.pointer);
      } catch {
        // Continue releasing callback slots and blocks.
      }
      registered = false;
    }
    try {
      jsIo.dispose();
    } catch {
      // Best effort: close must be idempotent and non-throwing.
    }
    try {
      jsVfs.dispose();
    } catch {
      // Best effort.
    }
    for (const file of files.values()) file.release();
    files.clear();
    handles.clear();
    mainFile = null;
  };

  try {
    sqlite.vfs.installVfs({
      io: { struct: jsIo, methods: ioMethods },
      vfs: { struct: jsVfs, methods: vfsMethods, name: vfsName, asDefault: false },
    });
    registered = true;
  } catch {
    close();
    throw new BlockVfsUnavailableError();
  }
  try {
    database = new sqlite.oo1.DB({
      filename: `file:${MAIN_FILE_NAME}?vfs=${vfsName}`,
      flags: "c",
    });
    database.exec("PRAGMA journal_mode=MEMORY");
    database.selectValue("SELECT count(*) FROM sqlite_schema");
  } catch (error) {
    close();
    throw error;
  }

  const openedDatabase = database;
  return {
    db: openedDatabase,
    vfsName,
    exportImage(): Uint8Array {
      if (closed || mainFile === null) throw new Error("block VFS is closed");
      return mainFile.toBytes();
    },
    close,
  };
}
