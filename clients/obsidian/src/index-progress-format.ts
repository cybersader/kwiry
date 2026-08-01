// SPDX-FileCopyrightText: 2026 cybersader
// SPDX-License-Identifier: GPL-3.0-only

export interface IndexProgressLike {
  stage: "snapshot" | "replay" | "rebuild";
  completed: number;
  total: number | null;
  path?: string;
}

export function formatIndexProgress(
  progress: IndexProgressLike,
  options: { includePath?: boolean } = {},
): string {
  if (progress.total === null) return "Starting index…";
  const verb = progress.stage === "replay"
    ? "Reconciling"
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
