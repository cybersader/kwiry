// SPDX-FileCopyrightText: 2026 cybersader
// SPDX-License-Identifier: GPL-3.0-only

import { Setting } from "obsidian";

import type KwiryPlugin from "../../main";
import type { PrivateTools } from "../private-tools";
import { D5cPlaygroundModal } from "./modal";
import {
  isD5cExplanationLevel,
  loadD5cPlaygroundSettings,
  prepareD5cStoredData,
} from "./settings";

export interface D5cPlaygroundDependencies {
  workerSource: string;
  fixtureCorpus: unknown;
}

export function createD5cPlaygroundTools(
  plugin: KwiryPlugin,
  stored: unknown,
  dependencies: D5cPlaygroundDependencies,
): PrivateTools {
  const settings = loadD5cPlaygroundSettings(stored);
  const modals = new Set<D5cPlaygroundModal>();
  return {
    register(): void {
      plugin.addCommand({
        id: "open-private-d5c-balanced-playground",
        name: "Open private D5C Balanced playground",
        callback: () => {
          let modal!: D5cPlaygroundModal;
          modal = new D5cPlaygroundModal(
            plugin,
            dependencies.workerSource,
            dependencies.fixtureCorpus,
            settings.explanation_level,
            () => modals.delete(modal),
          );
          modals.add(modal);
          modal.open();
        },
      });
    },
    renderSettings(containerEl: HTMLElement): void {
      new Setting(containerEl).setName("Private tools").setHeading();
      new Setting(containerEl)
        .setName("Balanced playground explanations")
        .setDesc(
          "Controls the Rust-owned structural explanation projection in the fixture-only private playground.",
        )
        .addDropdown((dropdown) =>
          dropdown
            .addOptions({ off: "Off", summary: "Summary", rules: "Rules" })
            .setValue(settings.explanation_level)
            .onChange(async (value) => {
              settings.explanation_level = isD5cExplanationLevel(value) ? value : "summary";
              await plugin.saveSettings();
            }),
        );
    },
    prepareStoredData(normalSettings) {
      return prepareD5cStoredData(normalSettings, settings);
    },
    dispose(): void {
      for (const modal of modals) modal.close();
      modals.clear();
    },
  };
}
