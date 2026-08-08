// SPDX-FileCopyrightText: 2026 cybersader
// SPDX-License-Identifier: GPL-3.0-only
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import esbuild from "esbuild";

const root = dirname(fileURLToPath(import.meta.url));
await esbuild.build({
  entryPoints: [resolve(root, "src/main.ts")],
  outfile: resolve(root, "main.js"),
  bundle: true,
  format: "cjs",
  platform: "browser",
  target: "es2022",
  external: ["obsidian", "electron"],
  logLevel: "info",
  sourcemap: false,
  minify: false,
});
