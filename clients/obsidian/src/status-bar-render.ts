// SPDX-FileCopyrightText: 2026 cybersader
// SPDX-License-Identifier: GPL-3.0-only

const IN_FLIGHT_COUNT = / ?([0-9]+) in flight/u;

/**
 * Renders the in-flight count in its own fixed-width element. Text padding
 * alone is not reliable because Obsidian themes can render a figure space at a
 * different width from their digits.
 */
export function renderStatusBarText(statusBar: HTMLElement, text: string): void {
  const match = IN_FLIGHT_COUNT.exec(text);
  const count = match?.[1];
  if (match === null || count === undefined) {
    statusBar.setText(text);
    return;
  }

  statusBar.empty();
  statusBar.appendText(text.slice(0, match.index));
  statusBar.createSpan({
    cls: "kwiry-status-bar-in-flight-count",
    text: count.padStart(2, "0"),
  });
  statusBar.appendText(` in flight${text.slice(match.index + match[0].length)}`);
}
