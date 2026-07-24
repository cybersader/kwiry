// SPDX-FileCopyrightText: 2026 cybersader
// SPDX-License-Identifier: GPL-3.0-only
//
// Kwiry Search — one explicit presentation client for daemon and in-plugin
// retrieval backends. Parsing, ranking, indexing, and fallback policy remain
// outside the UI.

import { Notice, Plugin, requestUrl } from "obsidian";
import workerSource from "virtual:kwiry-worker-source";

import type { Transport } from "./api";
import { BackendManager } from "./backend-manager";
import type { BackendIdentity, BackendStatus } from "./backend";
import { DaemonBackend } from "./backends/daemon-backend";
import { InPluginLexicalBackend } from "./backends/in-plugin-lexical-backend";
import { readDaemonToken } from "./credentials";
import { KwirySearchModal } from "./search-modal";
import { DEFAULT_SETTINGS, loadSettings, type KwiryPluginSettings } from "./settings";
import { KwirySettingTab } from "./settings-tab";

const STATUS_POLL_MS = 30_000;

const obsidianTransport: Transport = async ({ url, method, headers, body }) => {
  const response = await requestUrl({ url, method, headers, body, throw: false });
  return { status: response.status, text: response.text };
};

export default class KwiryPlugin extends Plugin {
  settings: KwiryPluginSettings = DEFAULT_SETTINGS;
  private statusBar: HTMLElement | null = null;
  private backendManager!: BackendManager;
  private activeBackendIdentity: BackendIdentity | null = null;

  async onload(): Promise<void> {
    this.settings = loadSettings(await this.loadData());
    this.backendManager = new BackendManager({
      daemon: (instanceId) => new DaemonBackend({
        instanceId,
        baseUrl: this.settings.daemonUrl,
        currentVaultId: optionalString(this.settings.daemonCurrentVaultId),
        tokenProvider: () => readDaemonToken(this.settings.tokenFilePath),
        transport: obsidianTransport,
      }),
      in_plugin: (instanceId) => new InPluginLexicalBackend({
        instanceId,
        activeVaultId: null,
        workerSource,
      }),
    });
    await this.activateBackendProfile();
    this.addSettingTab(new KwirySettingTab(this.app, this));

    this.addCommand({
      id: "open-search",
      name: "Search notes",
      callback: () => void this.openSearch(),
    });
    if (this.settings.showRibbonIcon) {
      this.addRibbonIcon("search", "Kwiry search", () => void this.openSearch());
    }

    this.statusBar = this.addStatusBarItem();
    this.statusBar.setText("kwiry: …");
    this.registerInterval(
      window.setInterval(() => void this.refreshStatus(), STATUS_POLL_MS),
    );
    void this.refreshStatus();
  }

  onunload(): void {
    this.activeBackendIdentity = null;
    void this.backendManager?.dispose();
  }

  async saveSettings(): Promise<void> {
    await this.saveData(this.settings);
  }

  async activateBackendProfile(): Promise<void> {
    this.activeBackendIdentity = null;
    try {
      const backend = await this.backendManager.activate(this.settings.backendProfile);
      this.activeBackendIdentity = backend.identity;
    } catch {
      try {
        this.activeBackendIdentity = (await this.backendManager.current()).identity;
      } catch {
        this.activeBackendIdentity = null;
      }
    }
    await this.refreshStatus();
  }

  getActiveBackendIdentity(): BackendIdentity | null {
    return this.activeBackendIdentity;
  }

  async refreshStatus(): Promise<void> {
    if (!this.statusBar) return;
    try {
      const backend = await this.backendManager.current();
      this.activeBackendIdentity = backend.identity;
      this.statusBar.setText(formatStatus(await backend.status()));
    } catch {
      this.statusBar.setText("kwiry: backend unavailable");
    }
  }

  private async openSearch(): Promise<void> {
    try {
      const backend = await this.backendManager.current();
      const status = await backend.status();
      this.activeBackendIdentity = backend.identity;
      new KwirySearchModal(this, backend, status).open();
    } catch {
      new Notice("Kwiry: the selected search backend is unavailable.");
    }
  }
}

function formatStatus(status: BackendStatus): string {
  const profile = status.identity.profile === "daemon" ? "Daemon" : "In-plugin · Lexical";
  if (status.issue) return `kwiry: ${profile} · ${status.issue.safeMessage}`;
  const modes = status.capabilities.supportedModes.join("/");
  return `kwiry: ${profile} · ${status.phase} (${status.chunks} chunks, ${modes})`;
}

function optionalString(value: string): string | null {
  const trimmed = value.trim();
  return trimmed ? trimmed : null;
}
