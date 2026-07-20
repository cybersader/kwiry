// SPDX-FileCopyrightText: 2026 cybersader
// SPDX-License-Identifier: GPL-3.0-only
import esbuild from "esbuild";
import process from "process";
import builtins from "builtin-modules";

const banner = `/*
Kwiry Search — Obsidian client for the kwiry search daemon.
Copyright (C) 2026 cybersader

This program is free software: you can redistribute it and/or modify
it under the terms of the GNU General Public License as published by
the Free Software Foundation, version 3.

This program is distributed in the hope that it will be useful, but
WITHOUT ANY WARRANTY; without even the implied warranty of
MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE. See the GNU
General Public License for more details: https://www.gnu.org/licenses/gpl-3.0.html

Portions adapted from Omnisearch (https://github.com/scambier/obsidian-omnisearch),
Copyright Simon Cambier and contributors, GPL-3.0.
Source for this build: https://github.com/cybersader/kwiry (clients/obsidian).
*/`;

const production = process.argv[2] === "production";

const context = await esbuild.context({
  banner: { js: banner },
  entryPoints: ["src/main.ts"],
  bundle: true,
  external: ["obsidian", "electron", ...builtins],
  format: "cjs",
  target: "es2022",
  logLevel: "info",
  sourcemap: production ? false : "inline",
  treeShaking: true,
  outfile: "main.js",
  legalComments: "inline",
});

if (production) {
  await context.rebuild();
  process.exit(0);
} else {
  await context.watch();
}
