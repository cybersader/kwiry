// SPDX-FileCopyrightText: 2026 cybersader
// SPDX-License-Identifier: GPL-3.0-only
//
// The one-line indexing indicator shown under the search input. Kept free of
// any Obsidian import so it is directly testable.
//
// A first build on a network vault can run for minutes. Without a visibly
// advancing line, a working index is indistinguishable from a stalled one, so
// this reports both the count and the file currently being read.

import type { BackendStatus } from "./backend";
import { formatIndexProgress } from "./index-progress-format";

/// Builds the progress text, or null when there is nothing in flight and the
/// line should be hidden entirely rather than shown empty.
export function progressLine(status: BackendStatus): string | null {
  const progress = status.progress;
  const omissions = omissionLine(status);
  if (!progress) return omissions;
  const suffix = omissions === null ? "" : ` · ${omissions}`;
  return `${formatIndexProgress(progress, { includePath: true })}${suffix}`;
}

function omissionLine(status: BackendStatus): string | null {
  const quarantined = status.quarantinedSources ?? 0;
  const unreadable = status.unreadableSources ?? 0;
  const total = quarantined + unreadable;
  if (total === 0) return null;
  const note = total === 1 ? "note may be" : "notes may be";
  const kinds = [
    quarantined === 0 ? null : `${quarantined} quarantined`,
    unreadable === 0 ? null : `${unreadable} unreadable`,
  ].filter((kind): kind is string => kind !== null).join(", ");
  return `${total} ${note} missing from search (${kinds})`;
}
