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
  const omissions = omissionStatus(status);
  let detail: string;
  if (status.issue) {
    // Show the counter alongside the issue while work is in flight. Reporting
    // the issue alone leaves a first build — where "no cached index" is both
    // expected and long-running on a network vault — indistinguishable from a
    // stall for its entire duration.
    detail = progress === null
      ? status.issue.safeMessage
      : `${status.issue.safeMessage} ${progress}`;
  } else if (progress !== null) {
    detail = progress;
  } else {
    const modes = status.capabilities.supportedModes.join("/");
    const phase = omissions === null
      ? status.phase === "ready" && status.dirty ? "building" : status.phase
      : "degraded";
    detail = `${phase} (${status.chunks} chunks, ${modes})`;
  }
  const issueDescribesOmissions = status.issue?.code === "sources_quarantined"
    || status.issue?.code === "sources_unreadable";
  const omissionSuffix = omissions === null || issueDescribesOmissions ? "" : ` · ${omissions}`;
  return `kwiry: ${profile} · ${detail}${omissionSuffix}`;
}

function omissionStatus(status: BackendStatus): string | null {
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
