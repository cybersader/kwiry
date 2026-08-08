// SPDX-FileCopyrightText: 2026 cybersader
// SPDX-License-Identifier: GPL-3.0-only

import { buildPlugin } from "../esbuild.config.mjs";

await buildPlugin({
  write: false,
  production: false,
  internalD5cPlayground: true,
});
