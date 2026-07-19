// SPDX-FileCopyrightText: 2026 cybersader
// SPDX-License-Identifier: GPL-3.0-only

import { App, PluginSettingTab, Setting } from "obsidian";

import type KwirPlugin from "./main";
import type { SearchMode } from "./api";

export class KwirSettingTab extends PluginSettingTab {
  constructor(
    app: App,
    private readonly plugin: KwirPlugin,
  ) {
    super(app, plugin);
  }

  display(): void {
    const { containerEl } = this;
    containerEl.empty();

    new Setting(containerEl).setName("Daemon").setHeading();

    new Setting(containerEl)
      .setName("Daemon URL")
      .setDesc("Local kwir daemon address. The plugin only talks to this URL.")
      .addText((text) =>
        text
          .setPlaceholder("http://127.0.0.1:32189")
          .setValue(this.plugin.settings.daemonUrl)
          .onChange(async (value) => {
            this.plugin.settings.daemonUrl = value.trim();
            await this.plugin.saveSettings();
            void this.plugin.refreshStatus();
          }),
      );

    new Setting(containerEl)
      .setName("Token file path")
      .setDesc(
        "Absolute path to the daemon's bearer-token file (printed at daemon startup). The token is read from disk on demand and never stored by the plugin.",
      )
      .addText((text) =>
        text
          .setPlaceholder("/path/to/config.token")
          .setValue(this.plugin.settings.tokenFilePath)
          .onChange(async (value) => {
            this.plugin.settings.tokenFilePath = value.trim();
            await this.plugin.saveSettings();
            void this.plugin.refreshStatus();
          }),
      );

    new Setting(containerEl).setName("Search").setHeading();

    new Setting(containerEl)
      .setName("Default mode")
      .setDesc("Semantic and hybrid need the daemon running with --semantic.")
      .addDropdown((dropdown) =>
        dropdown
          .addOptions({ lexical: "Lexical", semantic: "Semantic", hybrid: "Hybrid" })
          .setValue(this.plugin.settings.defaultMode)
          .onChange(async (value) => {
            this.plugin.settings.defaultMode = value as SearchMode;
            await this.plugin.saveSettings();
          }),
      );

    new Setting(containerEl)
      .setName("Result limit")
      .setDesc("Results per search (1–100).")
      .addText((text) =>
        text.setValue(String(this.plugin.settings.resultLimit)).onChange(async (value) => {
          const parsed = Number.parseInt(value, 10);
          if (Number.isInteger(parsed)) {
            this.plugin.settings.resultLimit = Math.min(100, Math.max(1, parsed));
            await this.plugin.saveSettings();
          }
        }),
      );

    new Setting(containerEl)
      .setName("Vault ID filter")
      .setDesc("Restrict searches to one registered kwir vault. Empty searches all.")
      .addText((text) =>
        text.setValue(this.plugin.settings.vaultId).onChange(async (value) => {
          this.plugin.settings.vaultId = value.trim();
          await this.plugin.saveSettings();
        }),
      );

    new Setting(containerEl)
      .setName("Show ribbon icon")
      .setDesc("Takes effect after reloading the plugin.")
      .addToggle((toggle) =>
        toggle.setValue(this.plugin.settings.showRibbonIcon).onChange(async (value) => {
          this.plugin.settings.showRibbonIcon = value;
          await this.plugin.saveSettings();
        }),
      );
  }
}
