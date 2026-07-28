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

/// Builds the progress text, or null when there is nothing in flight and the
/// line should be hidden entirely rather than shown empty.
export function progressLine(status: BackendStatus): string | null {
  const progress = status.progress;
  const omissions = omissionLine(status);
  if (!progress) return omissions;
  const verb = progress.stage === "replay" ? "Reconciling" : "Indexing";
  const counted = progress.total === null
    ? `${progress.completed}`
    : `${progress.completed}/${progress.total}`;
  const percent = progress.total !== null && progress.total > 0
    ? ` (${Math.floor((progress.completed / progress.total) * 100)}%)`
    : "";
  const path = progress.path === undefined ? "" : ` · ${shortenPath(progress.path)}`;
  const suffix = omissions === null ? "" : ` · ${omissions}`;
  return `${verb} ${counted}${percent}${path}${suffix}`;
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

/// Keeps the line to a single row. The tail of a path identifies the file;
/// the head is the least useful part to keep when truncating.
function shortenPath(path: string, limit = 48): string {
  const name = path.replace(/\.md$/u, "");
  if (name.length <= limit) return name;
  return `…${name.slice(name.length - (limit - 1))}`;
}
