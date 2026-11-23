// SPDX-FileCopyrightText: 2026 cybersader
// SPDX-License-Identifier: GPL-3.0-only

import { App, PluginSettingTab, Setting } from "obsidian";

import type KwiryPlugin from "./main";
import type { SearchMode } from "./api";
import { DAEMON_HOST_UNAVAILABLE_GUIDANCE } from "./backend";
import { HOST_SEARCH_GUIDANCE, HOST_SEARCH_LABEL, lookupHostSearch } from "./host-search";
import {
  SOURCE_ROW_LIMIT_SETTING_DESCRIPTION,
  SOURCE_ROW_LIMIT_SETTING_NAME,
  type DiagnosticsReportLevel,
  type DiagnosticsReportScope,
} from "./settings";
import {
  IN_PLUGIN_SOURCE_SUPPORT_DESCRIPTION,
  SOURCE_FORMATS,
  isSourceFormatExtractable,
  sourceFormatDescription,
  type SourceFormat,
} from "./source-formats";

export class KwirySettingTab extends PluginSettingTab {
  private changingBackend = false;
  private shown = false;

  constructor(
    app: App,
    private readonly plugin: KwiryPlugin,
  ) {
    super(app, plugin);
  }

  hide(): void {
    this.shown = false;
    super.hide();
  }

  /** Redraws an open tab, for example when a fresh install finishes starting. */
  redrawIfShown(): void {
    if (this.shown) this.display();
  }

  display(): void {
    const { containerEl } = this;
    this.shown = true;
    containerEl.empty();

    // Until a fresh install knows whether host search exists, its profile is only
    // a provisional fallback; offering no controls means nothing can save it.
    if (this.plugin.startingUp) {
      new Setting(containerEl).setName("Kwiry is starting; settings are available in a few seconds.");
      return;
    }

    const daemonAvailable = this.plugin.daemonBackendAvailable;
    const hostAvailable = lookupHostSearch() !== null;
    const hostSelected = this.plugin.settings.backendProfile === "host";
    new Setting(containerEl)
      .setName("Search backend")
      .setDesc(hostSelected ? "Host search never switches to another backend on failure."
        : daemonAvailable
          ? "Backend selection is explicit. A daemon failure never activates in-plugin search."
          : DAEMON_HOST_UNAVAILABLE_GUIDANCE)
      .addDropdown((dropdown) => {
        dropdown
          .addOptions({
            daemon: daemonAvailable ? "Daemon" : "Daemon (unavailable)",
            in_plugin: "In-plugin · Lexical",
            ...(hostAvailable || hostSelected ? { host: HOST_SEARCH_LABEL } : {}),
          })
          .setValue(this.plugin.settings.backendProfile)
          .setDisabled(this.changingBackend)
          .onChange(async (value) => {
            if (this.changingBackend || (value !== "daemon" && value !== "in_plugin" && value !== "host")) {
              dropdown.setValue(this.plugin.settings.backendProfile);
              return;
            }
            this.changingBackend = true;
            dropdown.setDisabled(true);
            try {
              await this.plugin.changeBackendProfile(value);
            } finally {
              this.changingBackend = false;
              this.display();
            }
          });
        if (!daemonAvailable) {
          const daemonOption = dropdown.selectEl.querySelector<HTMLOptionElement>('option[value="daemon"]');
          if (daemonOption) daemonOption.disabled = true;
        }
      });

    if (hostSelected) {
      new Setting(containerEl).setName(HOST_SEARCH_LABEL).setDesc(hostAvailable
        ? "Search runs in the host. This plugin does not build an index in this mode."
        : HOST_SEARCH_GUIDANCE);
    } else if (this.plugin.settings.backendProfile === "daemon") {
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

    if (!hostSelected) {
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
    }

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
        "Changing a format reuses compatible cached sources, removes disabled formats before publication, and reconciles newly enabled files. A source or cache schema upgrade may require one complete rebuild.",
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
      .setName("Report level")
      .setDesc(
        "Which events the copied summary and exported report contain. This can narrow the log but never recreate events that were not recorded.",
      )
      .addDropdown((dropdown) =>
        dropdown
          .addOptions({
            debug: "Everything recorded",
            info: "Info and above",
            warn: "Warnings and errors",
            error: "Errors only",
          })
          .setValue(this.plugin.settings.diagnosticsReportLevel)
          .onChange(async (value) => {
            this.plugin.settings.diagnosticsReportLevel = value as DiagnosticsReportLevel;
            await this.plugin.saveSettings();
          }),
      );

    new Setting(containerEl)
      .setName("Report scope")
      .setDesc("Restrict both diagnostics actions to one area.")
      .addDropdown((dropdown) =>
        dropdown
          .addOptions({
            all: "Everything",
            indexing: "Indexing, cache, and Worker",
            search: "Search",
            startup: "Startup and backend",
            failures: "Failures only",
          })
          .setValue(this.plugin.settings.diagnosticsReportScope)
          .onChange(async (value) => {
            this.plugin.settings.diagnosticsReportScope = value as DiagnosticsReportScope;
            await this.plugin.saveSettings();
          }),
      );

    new Setting(containerEl)
      .setName("Diagnostic report")
      .setDesc(
        "Copy a sanitized summary capped at 64 KiB, or export the full sanitized report. When desktop file export is unavailable, full export copies at most 16 MiB to the clipboard; paste it into a text file outside the vault and clear the clipboard when finished.",
      )
      .addButton((button) =>
        button
          .setButtonText("Copy summary")
          .onClick(() => void this.plugin.copyDiagnostics()),
      )
      .addButton((button) =>
        button
          .setButtonText("Export full report…")
          .onClick(() => void this.plugin.exportDiagnosticsFile()),
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
    case "excalidraw": return "Excalidraw";
    case "excel": return "Excel";
    case "html": return "HTML";
  }
}
