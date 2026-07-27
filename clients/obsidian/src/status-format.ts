// SPDX-FileCopyrightText: 2026 cybersader
// SPDX-License-Identifier: GPL-3.0-only

import type { BackendStatus } from "./backend";

export function formatStatus(status: BackendStatus): string {
  const profile = status.identity.profile === "daemon" ? "Daemon" : "In-plugin · Lexical";
  // Cache/freshness issues are more important than generic progress: a restored
  // generation can be searchable while replay is still making it current.
  if (status.issue) return `kwiry: ${profile} · ${status.issue.safeMessage}`;
  if (status.progress) {
    const total = status.progress.total === null ? "?" : String(status.progress.total);
    return `kwiry: ${profile} · ${status.progress.stage} ${status.progress.completed}/${total}`;
  }
  const modes = status.capabilities.supportedModes.join("/");
  const phase = status.phase === "ready" && status.dirty ? "building" : status.phase;
  return `kwiry: ${profile} · ${phase} (${status.chunks} chunks, ${modes})`;
}
