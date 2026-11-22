// SPDX-FileCopyrightText: 2026 cybersader
// SPDX-License-Identifier: GPL-3.0-only

import { readFileSync } from "node:fs";
import { dirname } from "node:path";
import snappy from "snappyjs";

const COMMUNITY_MAIN_LIMIT = 8 * 1024 * 1024;
const compressedPayload = "kwiry:compressed-rust-wasm";

export function assertCommunityMainSize(bytes) {
  if (bytes > COMMUNITY_MAIN_LIMIT) {
    throw new Error(`Production main.js is ${bytes} bytes; the community install limit is ${COMMUNITY_MAIN_LIMIT} bytes`);
  }
}

// Keep the .wasm input (and the uncompressed artifact identity) in the Worker
// graph. Only its on-disk representation in the bundle changes.
export function compressedRustWasmPlugin({ wasm, bytes }) {
  const notice = readFileSync(new URL("../licenses/snappyjs-MIT.txt", import.meta.url), "utf8").trimEnd();
  return {
    name: "kwiry-compressed-rust-wasm",
    setup(build) {
      build.onLoad({ filter: /\.wasm$/, namespace: "file" }, (args) => {
        if (args.path !== wasm) return undefined;
        return {
          contents: [
            `/*! snappyjs — ${notice} */`,
            // Bundle the decoder alone so the Worker does not need Node shims.
            'import { SnappyDecompressor } from "snappyjs/snappy_decompressor.js";',
            `import compressed from ${JSON.stringify(compressedPayload)};`,
            `const decoded = new Uint8Array(${bytes.byteLength});`,
            'const decoder = new SnappyDecompressor(compressed);',
            'if (decoder.readUncompressedLength() !== decoded.byteLength || !decoder.uncompressToBuffer(decoded)) {',
            '  throw new Error("Embedded Rust WASM decompression failed");',
            '}',
            'export default decoded;',
          ].join("\n"),
          loader: "js",
          resolveDir: dirname(wasm),
        };
      });
      build.onResolve({ filter: /^kwiry:compressed-rust-wasm$/ }, () => ({
        path: compressedPayload,
        namespace: "kwiry-compressed-rust-wasm",
      }));
      build.onLoad({ filter: /.*/, namespace: "kwiry-compressed-rust-wasm" }, () => ({
        contents: snappy.compress(bytes),
        loader: "binary",
      }));
    },
  };
}
