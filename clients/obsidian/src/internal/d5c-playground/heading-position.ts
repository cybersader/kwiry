// SPDX-FileCopyrightText: 2026 cybersader
// SPDX-License-Identifier: GPL-3.0-only

export interface HeadingPosition {
  line: number;
  ch: number;
}

export interface CachedHeadingLike {
  heading: string;
  level: number;
  position: {
    start: {
      line: number;
      col: number;
    };
  };
}

export function resolveUniqueHeadingPosition(
  headings: readonly CachedHeadingLike[],
  targetPath: readonly string[],
): HeadingPosition | null {
  if (targetPath.length === 0) return null;
  const stack: Array<{ heading: string; level: number }> = [];
  let match: HeadingPosition | null = null;
  for (const heading of headings) {
    while (stack.length > 0 && stack.at(-1)!.level >= heading.level) stack.pop();
    stack.push({ heading: heading.heading, level: heading.level });
    if (stack.length !== targetPath.length
      || stack.some((entry, index) => entry.heading !== targetPath[index])) {
      continue;
    }
    if (match !== null) return null;
    match = {
      line: heading.position.start.line,
      ch: heading.position.start.col,
    };
  }
  return match;
}
