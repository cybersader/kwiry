// SPDX-FileCopyrightText: 2026 cybersader
// SPDX-License-Identifier: GPL-3.0-only

export interface StatusBarRenderer {
  render(text: string): void;
}

/**
 * Creates the status item's stable DOM once, then updates only its text and
 * accessibility metadata. The root owns layout geometry; the label may
 * ellipsize visually without discarding the complete status.
 */
export function createStatusBarRenderer(statusBar: HTMLElement): StatusBarRenderer {
  statusBar.addClass("kwiry-status-bar");
  const label = statusBar.createSpan({ cls: "kwiry-status-bar__label" });
  let renderedText: string | null = null;

  return {
    render(text: string): void {
      if (text === renderedText) return;
      renderedText = text;
      label.setText(text);
      statusBar.setAttribute("title", text);
      statusBar.setAttribute("aria-label", text);
    },
  };
}
