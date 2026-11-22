// SPDX-FileCopyrightText: 2026 cybersader
// SPDX-License-Identifier: GPL-3.0-only

import {
  DAEMON_HOST_UNAVAILABLE,
  DAEMON_HOST_UNAVAILABLE_GUIDANCE,
  type BackendStatus,
} from "./backend";
import type { HostBackendStatus } from "./backends/host-search-backend";
import { HOST_SEARCH_GUIDANCE, HOST_SEARCH_LABEL } from "./host-search";
import { formatIndexProgressSummary } from "./index-progress-format";

export function formatStatus(status: BackendStatus | HostBackendStatus): string {
  if ("kind" in status) {
    return `Kwiry: ${status.available ? HOST_SEARCH_LABEL : HOST_SEARCH_GUIDANCE}`;
  }
  if (status.issue?.code === DAEMON_HOST_UNAVAILABLE.code) {
    return `Kwiry: ${DAEMON_HOST_UNAVAILABLE_GUIDANCE}`;
  }
  if (status.progress) return `Kwiry: ${formatIndexProgressSummary(status.progress)}`;

  const partial = status.sourceFormatCounts === undefined
    ? 0
    : Object.values(status.sourceFormatCounts)
      .reduce((total, counts) => total + counts["indexed-partial"], 0);
  const omitted = (status.quarantinedSources ?? 0) + (status.unreadableSources ?? 0);
  if (status.searchable) {
    const details: string[] = [];
    if (partial > 0) {
      details.push(`${partial} partial-extraction source${partial === 1 ? "" : "s"}`);
    }
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
