// SPDX-FileCopyrightText: 2026 cybersader
// SPDX-License-Identifier: GPL-3.0-only

import type {
  BackendIndexActivity,
  BackendIndexStallCategory,
} from "./backend";

export interface IndexProgressLike {
  stage: "snapshot" | "replay" | "rebuild" | "degraded" | "failed";
  activity: BackendIndexActivity;
  subphase?: "planning" | "verifying" | "applying";
  completed: number;
  total: number | null;
  inFlight: number;
  stallCategory?: BackendIndexStallCategory;
}

export function formatIndexProgress(progress: IndexProgressLike): string {
  const total = progress.total;
  const base = total === null
    ? unknownTotalLine(progress)
    : knownTotalLine(progress, total);
  const inFlight = progress.inFlight > 0
    ? ` · ${progress.inFlight} in flight`
    : "";
  const stall = progress.stallCategory === undefined
    ? ""
    : ` · ${stallLabel(progress.stallCategory)}`;
  return `${base}${inFlight}${stall}`;
}

function unknownTotalLine(progress: IndexProgressLike): string {
  switch (progress.activity) {
    case "inventory":
      return "Inventorying sources…";
    case "read":
      return "Reading sources…";
    case "prepare":
      return "Preparing index batches…";
    case "apply":
      return applyVerb(progress, true);
  }
}

function knownTotalLine(progress: IndexProgressLike, total: number): string {
  const percent = total > 0
    ? Math.floor((progress.completed / total) * 100)
    : 0;
  const verb = progress.activity === "inventory"
    ? "Inventory"
    : progress.activity === "read"
      ? "Reading"
      : progress.activity === "prepare"
        ? "Preparing"
        : applyVerb(progress, false);
  return `${verb} ${progress.completed}/${total} (${percent}%)`;
}

function applyVerb(progress: IndexProgressLike, continuous: boolean): string {
  if (progress.stage === "replay" && progress.subphase === "planning") {
    return continuous ? "Planning reconciliation…" : "Planning reconciliation";
  }
  if (progress.stage === "replay" && progress.subphase === "verifying") {
    return continuous ? "Verifying reconciliation…" : "Verifying";
  }
  if (progress.stage === "replay" && progress.subphase === "applying") {
    return continuous ? "Applying changes…" : "Applying changes";
  }
  if (progress.stage === "rebuild") return continuous ? "Rebuilding…" : "Rebuilding";
  return continuous ? "Applying index…" : "Applying";
}

function stallLabel(category: BackendIndexStallCategory): string {
  switch (category) {
    case "source_read_timeout":
      return "source reads timed out";
    case "source_read_capacity":
      return "source read capacity reached";
    case "worker_timeout":
      return "index worker timed out";
  }
}
