declare module "@sqlite.org/sqlite-wasm/sqlite3.wasm" {
  const bytes: Uint8Array;
  export default bytes;
}

declare module "virtual:kwiry-worker-source" {
  const source: string;
  export default source;
}
