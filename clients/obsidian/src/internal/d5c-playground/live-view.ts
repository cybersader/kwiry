// SPDX-FileCopyrightText: 2026 cybersader
// SPDX-License-Identifier: GPL-3.0-only

import type { IndexControllerStatus } from "../../backends/in-plugin-index-controller";
import { formatIndexProgress } from "../../index-progress-format";
import type { D5cCoverage } from "./live-service";

export function coverageMessage(coverage: D5cCoverage): string {
  switch (coverage.kind) {
    case "starting":
      return "Starting index… Search becomes available after the first completed batch.";
    case "partial": {
      const percent = coverage.total > 0
        ? Math.floor((coverage.processed / coverage.total) * 100)
        : 0;
      return `Partial results — indexing ${coverage.processed}/${coverage.total} (${percent}%). Results cover only files indexed so far.`;
    }
    case "complete":
      return "Complete index.";
    case "updating":
      return coverage.omittedNotes === 0
        ? "Current indexed results — vault changes are still being reconciled."
        : `Current incomplete results — ${coverage.omittedNotes} note${coverage.omittedNotes === 1 ? " is" : "s are"} missing while vault changes are reconciled.`;
    case "incomplete":
      return `Incomplete index — ${coverage.omittedNotes} note${coverage.omittedNotes === 1 ? "" : "s"} could not be indexed.`;
    case "unavailable":
      return "Index unavailable.";
  }
}

export function noMatchesMessage(coverage: D5cCoverage): string {
  if (coverage.kind === "partial") return "No matches in the partial index yet.";
  if (coverage.kind === "incomplete" || coverage.kind === "updating") {
    return "No matches in the currently indexed results.";
  }
  return "No matches.";
}

export function resultFocusKey(
  panel: "text" | "balanced",
  ordinal: number,
  path: string,
  headingPath: readonly string[],
): string {
  return JSON.stringify([panel, ordinal, path, headingPath]);
}

export function formatOwnerStatus(status: IndexControllerStatus): string {
  if (status.stage === "failed" || status.stage === "disposed") {
    return "Kwiry: Index unavailable";
  }
  const progress = status.progress;
  if (progress) {
    const stage = status.stage === "replay" || status.stage === "rebuild"
      ? status.stage
      : "snapshot";
    return `Kwiry: ${formatIndexProgress({ ...progress, stage })}`;
  }
  if (status.searchable) {
    const omitted = status.quarantinedSources + status.unreadableSources;
    return omitted === 0
      ? "Kwiry: Ready"
      : `Kwiry: Ready · ${omitted} note${omitted === 1 ? "" : "s"} incomplete`;
  }
  return "Kwiry: Starting index…";
}
