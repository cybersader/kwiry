// SPDX-FileCopyrightText: 2026 cybersader
// SPDX-License-Identifier: GPL-3.0-only

import type { KwiryPluginSettings } from "../settings";

export interface PrivateTools {
  register(): void;
  renderSettings(containerEl: HTMLElement): void;
  prepareStoredData(settings: KwiryPluginSettings): unknown;
  dispose(): void;
}

const NO_PRIVATE_TOOLS: PrivateTools = Object.freeze({
  register: () => undefined,
  renderSettings: (_containerEl: HTMLElement) => undefined,
  prepareStoredData: (settings: KwiryPluginSettings) => settings,
  dispose: () => undefined,
});

export function createPrivateTools(_plugin: unknown, _stored: unknown): PrivateTools {
  return NO_PRIVATE_TOOLS;
}
