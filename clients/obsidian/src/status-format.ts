// SPDX-FileCopyrightText: 2026 cybersader
// SPDX-License-Identifier: GPL-3.0-only

import type { BackendStatus } from "./backend";
import { formatIndexProgress } from "./index-progress-format";

export function formatStatus(status: BackendStatus): string {
  if (status.progress) return `Kwiry: ${formatIndexProgress(status.progress)}`;

  const omitted = (status.quarantinedSources ?? 0) + (status.unreadableSources ?? 0);
  if (status.searchable) {
    if (omitted > 0) {
      return `Kwiry: Ready · ${omitted} note${omitted === 1 ? "" : "s"} incomplete`;
    }
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
