// SPDX-FileCopyrightText: 2026 cybersader
// SPDX-License-Identifier: GPL-3.0-only

import type { BackendStatus } from "./backend";
import { formatIndexProgress } from "./index-progress-format";

export function formatStatus(status: BackendStatus): string {
  if (status.progress) return `Kwiry: ${formatIndexProgress(status.progress)}`;

  const partial = status.sourceFormatCounts === undefined
    ? 0
    : Object.values(status.sourceFormatCounts)
      .reduce((total, counts) => total + counts["indexed-partial"], 0);
  const omitted = (status.quarantinedSources ?? 0) + (status.unreadableSources ?? 0);
  if (status.searchable) {
    const details: string[] = [];
    if (partial > 0) details.push(`${partial} source${partial === 1 ? "" : "s"} partial`);
    if (omitted > 0) details.push(`${omitted} source${omitted === 1 ? "" : "s"} incomplete`);
    if (details.length > 0) return `Kwiry: Ready · ${details.join(" · ")}`;
    return status.dirty ? "Kwiry: Ready · Updating" : "Kwiry: Ready";
  }

  if (status.phase === "unavailable"
    || status.phase === "disposed"
    || status.liveness === "unreachable"
    || status.liveness === "terminated") {
    return "Kwiry: Index unavailable";
  }
  return "Kwiry: Starting index…";
}
