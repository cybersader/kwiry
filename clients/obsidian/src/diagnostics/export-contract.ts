// SPDX-FileCopyrightText: 2026 cybersader
// SPDX-License-Identifier: GPL-3.0-only

export const DIAGNOSTIC_EXPORT_MAX_BYTES = 16 * 1_024 * 1_024;

export type DiagnosticsExportResult =
  | { readonly kind: "saved" }
  | { readonly kind: "cancelled" }
  | { readonly kind: "inside_vault" }
  | { readonly kind: "unsafe_destination" }
  | { readonly kind: "unavailable" }
  | { readonly kind: "write_failed" };
