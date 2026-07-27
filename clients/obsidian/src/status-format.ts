// SPDX-FileCopyrightText: 2026 cybersader
// SPDX-License-Identifier: GPL-3.0-only

import type { BackendStatus } from "./backend";

export function formatStatus(status: BackendStatus): string {
  const profile = status.identity.profile === "daemon" ? "Daemon" : "In-plugin · Lexical";
  const progress = status.progress
    ? `${status.progress.stage} ${status.progress.completed}/${
      status.progress.total === null ? "?" : String(status.progress.total)
    }`
    : null;
  if (status.issue) {
    // Show the counter alongside the issue while work is in flight. Reporting
    // the issue alone leaves a first build — where "no cached index" is both
    // expected and long-running on a network vault — indistinguishable from a
    // stall for its entire duration.
    return progress === null
      ? `kwiry: ${profile} · ${status.issue.safeMessage}`
      : `kwiry: ${profile} · ${status.issue.safeMessage} ${progress}`;
  }
  if (progress !== null) return `kwiry: ${profile} · ${progress}`;
  const modes = status.capabilities.supportedModes.join("/");
  const phase = status.phase === "ready" && status.dirty ? "building" : status.phase;
  return `kwiry: ${profile} · ${phase} (${status.chunks} chunks, ${modes})`;
}
