// SPDX-FileCopyrightText: 2026 cybersader
// SPDX-License-Identifier: GPL-3.0-only

import { readFileSync } from "node:fs";
import { createRequire } from "node:module";
import { runInNewContext } from "node:vm";
import { build } from "esbuild";
import { describe, expect, it } from "vitest";
import { assertCommunityMainSize, compressedRustWasmPlugin } from "../scripts/community-bundle.mjs";

const require = createRequire(import.meta.url);

describe("community plugin bundle", () => {
  it("rejects a main.js one byte over the browser host limit", () => {
    expect(() => assertCommunityMainSize(8_388_608)).not.toThrow();
    expect(() => assertCommunityMainSize(8_388_609)).toThrow(/8388609.*8388608/u);
  });

  it("inlines compressed WASM without changing its bytes or requiring webview decompression APIs", async () => {
    // Use the pinned SQLite WASM as a nontrivial fixture so this checks the
    // packer and bundled inflater without building the Rust adapter.
    const wasm = require.resolve("@sqlite.org/sqlite-wasm/sqlite3.wasm");
    const bytes = readFileSync(wasm);
    const bundled = await build({
      stdin: { contents: `import bytes from ${JSON.stringify(wasm)}; module.exports = bytes;`, resolveDir: process.cwd() },
      platform: "browser",
      format: "cjs",
      target: "es2022",
      bundle: true,
      minify: true,
      write: false,
      plugins: [compressedRustWasmPlugin({ wasm, bytes })],
    });
    expect(bundled.outputFiles).toHaveLength(1);
    const [output] = bundled.outputFiles;
    expect(output.contents.byteLength).toBeLessThan(bytes.byteLength);
    const module = { exports: {} };
    runInNewContext(output.text, { module, atob });
    expect(Buffer.from(module.exports).equals(bytes)).toBe(true);
  });
});
