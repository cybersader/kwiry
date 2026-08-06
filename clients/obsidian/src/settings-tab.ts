// SPDX-FileCopyrightText: 2026 cybersader
// SPDX-License-Identifier: GPL-3.0-only

import { App, PluginSettingTab, Setting } from "obsidian";

import type KwiryPlugin from "./main";
import type { SearchMode } from "./api";
import {
  SOURCE_ROW_LIMIT_SETTING_DESCRIPTION,
  SOURCE_ROW_LIMIT_SETTING_NAME,
} from "./settings";
import {
  IN_PLUGIN_SOURCE_SUPPORT_DESCRIPTION,
  SOURCE_FORMATS,
  isSourceFormatExtractable,
  sourceFormatDescription,
  type SourceFormat,
} from "./source-formats";

export class KwirySettingTab extends PluginSettingTab {
  constructor(
    app: App,
    private readonly plugin: KwiryPlugin,
  ) {
    super(app, plugin);
  }

  display(): void {
    const { containerEl } = this;
    containerEl.empty();

    new Setting(containerEl)
      .setName("Search backend")
      .setDesc("Backend selection is explicit. A daemon failure never activates in-plugin search.")
      .addDropdown((dropdown) =>
        dropdown
          .addOptions({ daemon: "Daemon", in_plugin: "In-plugin · Lexical" })
          .setValue(this.plugin.settings.backendProfile)
          .onChange(async (value) => {
            this.plugin.settings.backendProfile = value === "in_plugin" ? "in_plugin" : "daemon";
            await this.plugin.saveSettings();
            await this.plugin.activateBackendProfile();
            this.display();
          }),
      );

    if (this.plugin.settings.backendProfile === "daemon") {
      this.renderDaemonSettings(containerEl);
    } else {
      new Setting(containerEl)
        .setName("In-plugin · Lexical")
        .setDesc(IN_PLUGIN_SOURCE_SUPPORT_DESCRIPTION);
      this.renderSourceFormatSettings(containerEl);
      new Setting(containerEl)
        .setName("Rebuild in-plugin lexical index")
        .setDesc("Build a complete replacement generation while the current index remains searchable.")
        .addButton((button) =>
          button
            .setButtonText("Rebuild")
            .onClick(() => void this.plugin.rebuildInPluginIndex()),
        );
    }

    new Setting(containerEl).setName("Search").setHeading();

    new Setting(containerEl)
      .setName(SOURCE_ROW_LIMIT_SETTING_NAME)
      .setDesc(SOURCE_ROW_LIMIT_SETTING_DESCRIPTION)
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
      .setName("Show ribbon icon")
      .setDesc("Takes effect after reloading the plugin.")
      .addToggle((toggle) =>
        toggle.setValue(this.plugin.settings.showRibbonIcon).onChange(async (value) => {
          this.plugin.settings.showRibbonIcon = value;
          await this.plugin.saveSettings();
        }),
      );

    this.renderDiagnosticsSettings(containerEl);
    this.plugin.renderPrivateSettings(containerEl);
  }

  private renderSourceFormatSettings(containerEl: HTMLElement): void {
    new Setting(containerEl)
      .setName("Indexed source formats")
      .setDesc(
        "Changing any format rebuilds the index from scratch. Search is unavailable until the rebuild completes.",
      )
      .setHeading();

    for (const format of SOURCE_FORMATS) {
      const setting = new Setting(containerEl)
        .setName(sourceFormatLabel(format))
        .setDesc(sourceFormatDescription(format));
      if (!isSourceFormatExtractable(format)) continue;
      setting.addToggle((toggle) =>
        toggle
          .setValue(this.plugin.settings.enabledSourceFormats[format])
          .onChange(async (value) => {
            if (this.plugin.settings.enabledSourceFormats[format] === value) return;
            this.plugin.settings.enabledSourceFormats[format] = value;
            await this.plugin.saveSettings();
            await this.plugin.onSourcePolicyChanged();
          }),
      );
    }
  }

  private renderDiagnosticsSettings(containerEl: HTMLElement): void {
    new Setting(containerEl).setName("Diagnostics").setHeading();

    new Setting(containerEl)
      .setName("Log level")
      .setDesc(
        "Keeps a bounded in-memory log for field diagnosis. It is cleared when the plugin unloads and never includes note text, queries, excerpts, or credentials.",
      )
      .addDropdown((dropdown) =>
        dropdown
          .addOptions({
            off: "Off",
            error: "Errors only",
            info: "Field information",
          })
          .setValue(this.plugin.settings.diagnosticsLogLevel)
          .onChange(async (value) => {
            const level = value === "off" || value === "error" ? value : "info";
            this.plugin.settings.diagnosticsLogLevel = level;
            this.plugin.setDiagnosticsLogLevel(level);
            await this.plugin.saveSettings();
          }),
      );

    new Setting(containerEl)
      .setName("Diagnostic report")
      .setDesc("Copy the current in-memory report for a support conversation, or clear it now.")
      .addButton((button) =>
        button
          .setButtonText("Copy diagnostics")
          .onClick(() => void this.plugin.copyDiagnostics()),
      )
      .addButton((button) =>
        button
          .setButtonText("Clear diagnostics")
          .onClick(() => this.plugin.clearDiagnostics(true)),
      );
  }

  private renderDaemonSettings(containerEl: HTMLElement): void {
    new Setting(containerEl).setName("Daemon").setHeading();

    new Setting(containerEl)
      .setName("Daemon URL")
      .setDesc("Literal loopback HTTP origin with an explicit port.")
      .addText((text) =>
        text
          .setPlaceholder("http://127.0.0.1:32189")
          .setValue(this.plugin.settings.daemonUrl)
          .onChange(async (value) => {
            this.plugin.settings.daemonUrl = value.trim();
            await this.reconfigureDaemon();
          }),
      );

    new Setting(containerEl)
      .setName("Token file path")
      .setDesc(
        "Absolute path to the daemon token file. It is validated and read fresh for every authenticated request, never stored in plugin data.",
      )
      .addText((text) =>
        text
          .setPlaceholder("/path/to/config.token")
          .setValue(this.plugin.settings.tokenFilePath)
          .onChange(async (value) => {
            this.plugin.settings.tokenFilePath = value.trim();
            await this.reconfigureDaemon();
          }),
      );

    new Setting(containerEl)
      .setName("Default daemon mode")
      .setDesc("Semantic and hybrid require a daemon running with semantic support.")
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
      .setName("Search vault ID filter")
      .setDesc("Restrict daemon searches to one registered tree. Empty searches all trees.")
      .addText((text) =>
        text.setValue(this.plugin.settings.vaultId).onChange(async (value) => {
          this.plugin.settings.vaultId = value.trim();
          await this.plugin.saveSettings();
        }),
      );

    new Setting(containerEl)
      .setName("Current vault daemon ID")
      .setDesc(
        "Map this Obsidian vault to a daemon vault ID for local open actions. Results from other trees remain searchable but cannot be opened here.",
      )
      .addText((text) =>
        text.setValue(this.plugin.settings.daemonCurrentVaultId).onChange(async (value) => {
          this.plugin.settings.daemonCurrentVaultId = value.trim();
          await this.reconfigureDaemon();
        }),
      );
  }

  private async reconfigureDaemon(): Promise<void> {
    await this.plugin.saveSettings();
    await this.plugin.activateBackendProfile();
  }
}

function sourceFormatLabel(format: SourceFormat): string {
  switch (format) {
    case "markdown": return "Markdown";
    case "text": return "Plain text";
    case "base": return "Base";
    case "canvas": return "Canvas";
    case "docx": return "DOCX";
    case "pdf": return "PDF";
  }
}
