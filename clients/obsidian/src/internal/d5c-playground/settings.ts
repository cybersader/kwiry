// SPDX-FileCopyrightText: 2026 cybersader
// SPDX-License-Identifier: GPL-3.0-only

import type { KwiryPluginSettings } from "../../settings";

export type D5cExplanationLevel = "off" | "summary" | "rules";

export interface D5cPlaygroundSettings {
  schema_version: 1;
  explanation_level: D5cExplanationLevel;
}

export const D5C_PLAYGROUND_SETTINGS_NAMESPACE = "__kwiry_internal_d5c_playground";
export const DEFAULT_D5C_PLAYGROUND_SETTINGS: D5cPlaygroundSettings = Object.freeze({
  schema_version: 1,
  explanation_level: "summary",
});

export function loadD5cPlaygroundSettings(stored: unknown): D5cPlaygroundSettings {
  if (!isRecord(stored)) return { ...DEFAULT_D5C_PLAYGROUND_SETTINGS };
  const namespace = stored[D5C_PLAYGROUND_SETTINGS_NAMESPACE];
  if (!isRecord(namespace) || namespace.schema_version !== 1) {
    return { ...DEFAULT_D5C_PLAYGROUND_SETTINGS };
  }
  return {
    schema_version: 1,
    explanation_level: isD5cExplanationLevel(namespace.explanation_level)
      ? namespace.explanation_level
      : DEFAULT_D5C_PLAYGROUND_SETTINGS.explanation_level,
  };
}

export function prepareD5cStoredData(
  settings: KwiryPluginSettings,
  playground: D5cPlaygroundSettings,
): Record<string, unknown> {
  return {
    ...settings,
    [D5C_PLAYGROUND_SETTINGS_NAMESPACE]: {
      schema_version: 1,
      explanation_level: playground.explanation_level,
    },
  };
}

export function isD5cExplanationLevel(value: unknown): value is D5cExplanationLevel {
  return value === "off" || value === "summary" || value === "rules";
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}
