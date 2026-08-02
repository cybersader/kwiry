// SPDX-FileCopyrightText: 2026 cybersader
// SPDX-License-Identifier: GPL-3.0-only

export interface IndexProgressLike {
  stage: "snapshot" | "replay" | "rebuild";
  subphase?: "planning" | "verifying" | "applying";
  completed: number;
  total: number | null;
  path?: string;
}

export function formatIndexProgress(
  progress: IndexProgressLike,
  options: { includePath?: boolean } = {},
): string {
  if (progress.stage === "replay" && progress.subphase === "planning") {
    return "Planning reconciliation…";
  }
  if (progress.total === null) {
    if (progress.stage === "replay" && progress.subphase === "verifying") {
      return "Verifying reconciliation…";
    }
    if (progress.stage === "replay" && progress.subphase === "applying") {
      return "Applying changes…";
    }
    return "Starting index…";
  }
  const verb = progress.stage === "replay"
    ? progress.subphase === "verifying"
      ? "Verifying"
      : progress.subphase === "applying"
        ? "Applying changes"
        : "Reconciling"
    : progress.stage === "rebuild"
      ? "Rebuilding"
      : "Indexing";
  const percent = progress.total > 0
    ? Math.floor((progress.completed / progress.total) * 100)
    : 0;
  const path = options.includePath && progress.path !== undefined
    ? ` · ${shortenPath(progress.path)}`
    : "";
  return `${verb} ${progress.completed}/${progress.total} (${percent}%)${path}`;
}

function shortenPath(path: string, limit = 48): string {
  const name = path.replace(/\.md$/u, "");
  if (name.length <= limit) return name;
  return `…${name.slice(name.length - (limit - 1))}`;
}
