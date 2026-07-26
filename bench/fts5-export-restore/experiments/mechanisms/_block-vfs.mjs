// Shared sqlite3_vfs wiring for the block-VFS mechanisms.
//
// Helper module (leading underscore): never registered as a mechanism.
// `vfs-plain.mjs` and `vfs-deflate.mjs` differ only in how a block is stored,
// so the VFS/io struct wiring, the private-namespace registration and the
// teardown live here once. Ported from the working prototype in `../vfs.mjs`.

let instanceSeq = 0;

/// Register a private, uniquely named VFS backed by `mainFile` and open a
/// database on it.
///
/// config:
///   namePrefix        — VFS name prefix (a sequence + random suffix is added)
///   dbFileName        — the VFS path the main database occupies
///   mainFile          — BlockFileBase subclass holding the restored image
///   createScratchFile — (name) => BlockFileBase for journals/temp files
///   cacheSize         — optional PRAGMA cache_size value
///
/// Returns { vfsName, db, files, teardown }. `teardown()` is idempotent and
/// releases the database handle, the VFS registration, the wasm function
/// table entries created by installVfs, and the scratch files.
export function installBlockVfs(sqlite3, config) {
  const { namePrefix, dbFileName, mainFile, createScratchFile, cacheSize } = config;
  const capi = sqlite3.capi;
  const wasm = sqlite3.wasm;

  // --- private namespace ---------------------------------------------------
  let vfsName = '';
  for (let attempt = 0; !vfsName; attempt += 1) {
    instanceSeq += 1;
    const candidate = `${namePrefix}-${instanceSeq}-${Math.random().toString(36).slice(2, 8)}`;
    if (!capi.sqlite3_vfs_find(candidate)) vfsName = candidate;
    if (attempt > 64) throw new Error('could not find an unused VFS name');
  }

  const files = new Map();
  files.set(dbFileName, mainFile);

  // --- VFS + io methods ----------------------------------------------------
  const jsVfs = new capi.sqlite3_vfs();
  const jsIo = new capi.sqlite3_io_methods();
  jsIo.$iVersion = 1;
  jsVfs.$iVersion = 2;
  jsVfs.$szOsFile = capi.sqlite3_file.structInfo.sizeof;
  jsVfs.$mxPathname = 1024;

  const handles = new Map(); // String(pFile) -> { file, deleteOnClose }
  const key = (pointer) => String(pointer);
  const heap = () => wasm.heap8u(); // re-fetch: the heap detaches on growth

  const ioMethods = {
    xClose(pFile) {
      const handle = handles.get(key(pFile));
      if (handle) {
        handles.delete(key(pFile));
        if (handle.deleteOnClose) files.delete(handle.file.name);
      }
      return 0;
    },
    xRead(pFile, pDest, n, offset64) {
      const handle = handles.get(key(pFile));
      if (!handle) return capi.SQLITE_IOERR_READ;
      const count = Number(n);
      const destination = heap().subarray(Number(pDest), Number(pDest) + count);
      const got = handle.file.read(destination, Number(offset64));
      if (got < count) {
        destination.fill(0, got, count);
        return capi.SQLITE_IOERR_SHORT_READ;
      }
      return 0;
    },
    xWrite(pFile, pSrc, n, offset64) {
      const handle = handles.get(key(pFile));
      if (!handle) return capi.SQLITE_IOERR_WRITE;
      const count = Number(n);
      const source = heap().subarray(Number(pSrc), Number(pSrc) + count);
      handle.file.write(source, Number(offset64));
      return 0;
    },
    xTruncate(pFile, size) {
      const handle = handles.get(key(pFile));
      if (handle) handle.file.truncate(Number(size));
      return 0;
    },
    xSync() {
      return 0;
    },
    xFileSize(pFile, pSize) {
      const handle = handles.get(key(pFile));
      wasm.poke(pSize, BigInt(handle ? handle.file.size : 0), 'i64');
      return 0;
    },
    xLock() {
      return 0;
    },
    xUnlock() {
      return 0;
    },
    xCheckReservedLock(pFile, pOut) {
      wasm.poke(pOut, 0, 'i32');
      return 0;
    },
    xFileControl() {
      return capi.SQLITE_NOTFOUND;
    },
    xSectorSize() {
      return 4096;
    },
    xDeviceCharacteristics() {
      return (
        capi.SQLITE_IOCAP_ATOMIC |
        capi.SQLITE_IOCAP_SAFE_APPEND |
        capi.SQLITE_IOCAP_SEQUENTIAL |
        capi.SQLITE_IOCAP_POWERSAFE_OVERWRITE
      );
    },
  };

  const vfsMethods = {
    xOpen(pVfs, zName, pFile, flags, pOutFlags) {
      const name = zName ? wasm.cstrToJs(zName) : `/anon-${Math.random().toString(36).slice(2)}`;
      let file = files.get(name);
      if (!file) {
        if (!(flags & capi.SQLITE_OPEN_CREATE)) return capi.SQLITE_CANTOPEN;
        file = createScratchFile(name);
        files.set(name, file);
      }
      handles.set(key(pFile), {
        file,
        deleteOnClose: !!(flags & capi.SQLITE_OPEN_DELETEONCLOSE),
      });
      const sf = new capi.sqlite3_file(pFile);
      sf.$pMethods = jsIo.pointer;
      sf.dispose();
      if (pOutFlags) wasm.poke(pOutFlags, flags, 'i32');
      return 0;
    },
    xDelete(pVfs, zName) {
      files.delete(wasm.cstrToJs(zName));
      return 0;
    },
    xAccess(pVfs, zName, flags, pOut) {
      wasm.poke(pOut, files.has(wasm.cstrToJs(zName)) ? 1 : 0, 'i32');
      return 0;
    },
    xFullPathname(pVfs, zName, nOut, pOut) {
      return wasm.cstrncpy(pOut, zName, nOut) < nOut ? 0 : capi.SQLITE_CANTOPEN;
    },
    xCurrentTime(pVfs, pOut) {
      wasm.poke(pOut, 2440587.5 + Date.now() / 864e5, 'double');
      return 0;
    },
    xCurrentTimeInt64(pVfs, pOut) {
      wasm.poke(pOut, BigInt(Math.round(2440587.5 * 864e5 + Date.now())), 'i64');
      return 0;
    },
    xGetLastError() {
      return 0;
    },
  };

  // Borrow randomness/sleep from the default VFS rather than reimplementing.
  {
    const pDefault = capi.sqlite3_vfs_find(null);
    if (pDefault) {
      const dflt = new capi.sqlite3_vfs(pDefault);
      jsVfs.$xRandomness = dflt.$xRandomness;
      jsVfs.$xSleep = dflt.$xSleep;
      dflt.dispose();
    }
  }

  let registered = false;
  let db = null;
  let tornDown = false;
  const teardown = () => {
    if (tornDown) return;
    tornDown = true;
    if (db) {
      try {
        db.close();
      } catch {
        /* keep tearing down */
      }
      db = null;
    }
    if (registered) {
      capi.sqlite3_vfs_unregister(jsVfs.pointer);
      registered = false;
    }
    // dispose() also uninstalls the wasm function-table entries that
    // installVfs created, so repeated open()/close() cycles do not grow the
    // function table.
    try {
      jsIo.dispose();
    } catch {
      /* ignore */
    }
    try {
      jsVfs.dispose();
    } catch {
      /* ignore */
    }
    for (const file of files.values()) {
      try {
        file.release();
      } catch {
        /* ignore */
      }
    }
    files.clear();
    handles.clear();
  };

  try {
    sqlite3.vfs.installVfs({
      io: { struct: jsIo, methods: ioMethods },
      vfs: { struct: jsVfs, methods: vfsMethods, name: vfsName, asDefault: false },
    });
    registered = true;
    db = new sqlite3.oo1.DB({ filename: `file:${dbFileName}?vfs=${vfsName}`, flags: 'c' });
    if (cacheSize !== undefined) db.exec(`PRAGMA cache_size=${Number(cacheSize)}`);
    db.exec('PRAGMA journal_mode=MEMORY');
    // Force page 1 and the schema to be parsed inside open().
    db.selectValue('SELECT count(*) FROM sqlite_schema');
  } catch (error) {
    teardown();
    throw error;
  }

  return { vfsName, db, files, teardown };
}
