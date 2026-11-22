// SPDX-FileCopyrightText: 2026 cybersader
// SPDX-License-Identifier: GPL-3.0-only

import { createHash } from "node:crypto";
import { readFileSync } from "node:fs";
import { dirname } from "node:path";
import snappy from "snappyjs";

const compressedPayload = "kwiry:compressed-rust-wasm";

// Keep the .wasm input (and the uncompressed artifact identity) in the Worker
// graph. Only its on-disk representation in the bundle changes.
export function compressedRustWasmPlugin({ wasm, bytes }) {
  const notice = readFileSync(new URL("../licenses/snappyjs-MIT.txt", import.meta.url), "utf8").trimEnd();
  const packed = snappy.compress(bytes);
  const packedSha256 = createHash("sha256").update(packed).digest("hex");
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
            `import packed from ${JSON.stringify(compressedPayload)};`,
            'function decode() {',
            '  const decoder = new SnappyDecompressor(packed);',
            '  const length = decoder.readUncompressedLength();',
            `  if (length !== ${bytes.byteLength}) throw new Error("Embedded Rust WASM length mismatch");`,
            '  const decoded = new Uint8Array(length);',
            '  if (!decoder.uncompressToBuffer(decoded)) throw new Error("Embedded Rust WASM decompression failed");',
            '  return decoded;',
            '}',
            `export default { packed, packedSize: ${packed.byteLength}, packedSha256: ${JSON.stringify(packedSha256)}, decode };`,
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
        contents: packed,
        loader: "binary",
      }));
    },
  };
}
