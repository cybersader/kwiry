// SPDX-FileCopyrightText: 2026 cybersader
// SPDX-License-Identifier: GPL-3.0-only
//
// Kwiry Search — one explicit presentation client for daemon and in-plugin
// retrieval backends. Parsing, ranking, indexing, and fallback policy remain
// outside the UI.

import { Notice, Plugin, requestUrl } from "obsidian";
import workerSource from "virtual:kwiry-worker-source";

import { ACTIVE_VAULT_ID, ObsidianActiveVaultSource } from "./active-vault-source";
import type { Transport } from "./api";
import { BackendManager } from "./backend-manager";
import type { BackendIdentity, BackendStatus, SearchBackend } from "./backend";
import { DaemonBackend } from "./backends/daemon-backend";
import { InPluginLexicalBackend } from "./backends/in-plugin-lexical-backend";
import { readDaemonToken } from "./credentials";
import { LatestRequestEpoch } from "./latest-request-epoch";
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
  private statusUnsubscribe: (() => void) | null = null;
  private pluginEpoch = 0;
  private activationEpoch = 0;
  private readonly statusRefresh = new LatestRequestEpoch();

  async onload(): Promise<void> {
    const pluginEpoch = ++this.pluginEpoch;
    this.settings = loadSettings(await this.loadData());
    if (pluginEpoch !== this.pluginEpoch) return;
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
        activeVaultId: ACTIVE_VAULT_ID,
        source: new ObsidianActiveVaultSource(this.app.vault),
        workerSource,
      }),
    });

    this.addSettingTab(new KwirySettingTab(this.app, this));
    this.addCommand({
      id: "open-search",
      name: "Search notes",
      callback: () => void this.openSearch(),
    });
    this.addCommand({
      id: "rebuild-in-plugin-index",
      name: "Rebuild in-plugin lexical index",
      callback: () => void this.rebuildInPluginIndex(),
    });
    if (this.settings.showRibbonIcon) {
      this.addRibbonIcon("search", "Kwiry search", () => void this.openSearch());
    }

    this.statusBar = this.addStatusBarItem();
    this.statusBar.setText("kwiry: starting…");
    this.registerInterval(
      window.setInterval(() => void this.refreshStatus(), STATUS_POLL_MS),
    );
    await this.activateBackendProfile();
  }

  onunload(): void {
    this.pluginEpoch += 1;
    this.activationEpoch += 1;
    this.statusRefresh.invalidate();
    this.statusUnsubscribe?.();
    this.statusUnsubscribe = null;
    this.activeBackendIdentity = null;
    void this.backendManager?.dispose();
  }

  async saveSettings(): Promise<void> {
    await this.saveData(this.settings);
  }

  async activateBackendProfile(): Promise<void> {
    const pluginEpoch = this.pluginEpoch;
    const activationEpoch = ++this.activationEpoch;
    this.statusRefresh.invalidate();
    this.statusUnsubscribe?.();
    this.statusUnsubscribe = null;
    this.activeBackendIdentity = null;
    this.statusBar?.setText("kwiry: starting…");

    try {
      const backend = await this.backendManager.activate(this.settings.backendProfile);
      if (!this.isCurrent(pluginEpoch, activationEpoch)) return;
      this.bindBackend(backend, pluginEpoch, activationEpoch);
      await this.refreshStatus(pluginEpoch, activationEpoch);
    } catch {
      if (!this.isCurrent(pluginEpoch, activationEpoch)) return;
      try {
        const backend = await this.backendManager.current();
        if (!this.isCurrent(pluginEpoch, activationEpoch)) return;
        this.bindBackend(backend, pluginEpoch, activationEpoch);
        await this.refreshStatus(pluginEpoch, activationEpoch);
      } catch {
        if (this.isCurrent(pluginEpoch, activationEpoch)) {
          this.statusBar?.setText("kwiry: backend unavailable");
        }
      }
    }
  }

  getActiveBackendIdentity(): BackendIdentity | null {
    return this.activeBackendIdentity;
  }

  async refreshStatus(
    pluginEpoch = this.pluginEpoch,
    activationEpoch = this.activationEpoch,
  ): Promise<void> {
    if (!this.statusBar || !this.isCurrent(pluginEpoch, activationEpoch)) return;
    const refreshEpoch = this.statusRefresh.begin();
    try {
      const backend = await this.backendManager.current();
      const instanceId = backend.identity.instanceId;
      const status = await backend.status();
      if (!this.isCurrent(pluginEpoch, activationEpoch)
        || !this.statusRefresh.isCurrent(refreshEpoch)
        || this.activeBackendIdentity?.instanceId !== instanceId) return;
      this.statusBar.setText(formatStatus(status));
    } catch {
      if (this.isCurrent(pluginEpoch, activationEpoch)
        && this.statusRefresh.isCurrent(refreshEpoch)) {
        this.statusBar.setText("kwiry: backend unavailable");
      }
    }
  }

  async rebuildInPluginIndex(): Promise<void> {
    const pluginEpoch = this.pluginEpoch;
    const activationEpoch = this.activationEpoch;
    try {
      const backend = await this.backendManager.current();
      const status = await backend.status();
      if (!this.isCurrent(pluginEpoch, activationEpoch)
        || this.activeBackendIdentity?.instanceId !== backend.identity.instanceId) return;
      if (!status.capabilities.manualRebuild || !backend.rebuild) {
        new Notice("Kwiry: manual rebuild is available only for In-plugin · Lexical.");
        return;
      }
      await backend.rebuild();
      if (this.isCurrent(pluginEpoch, activationEpoch)
        && this.activeBackendIdentity?.instanceId === backend.identity.instanceId) {
        new Notice("Kwiry: in-plugin lexical rebuild started.");
      }
    } catch {
      if (this.isCurrent(pluginEpoch, activationEpoch)) {
        new Notice("Kwiry: the in-plugin lexical index could not be rebuilt.");
      }
    }
  }

  private bindBackend(
    backend: SearchBackend,
    pluginEpoch: number,
    activationEpoch: number,
  ): void {
    this.activeBackendIdentity = backend.identity;
    this.statusUnsubscribe?.();
    this.statusUnsubscribe = backend.subscribeStatus?.((status) => {
      if (!this.isCurrent(pluginEpoch, activationEpoch)
        || this.activeBackendIdentity?.instanceId !== backend.identity.instanceId) return;
      this.statusRefresh.invalidate();
      this.statusBar?.setText(formatStatus(status));
    }) ?? null;
  }

  private async openSearch(): Promise<void> {
    const pluginEpoch = this.pluginEpoch;
    const activationEpoch = this.activationEpoch;
    try {
      const backend = await this.backendManager.current();
      const status = await backend.status();
      if (!this.isCurrent(pluginEpoch, activationEpoch)
        || this.activeBackendIdentity?.instanceId !== backend.identity.instanceId) return;
      new KwirySearchModal(this, backend, status).open();
    } catch {
      if (this.isCurrent(pluginEpoch, activationEpoch)) {
        new Notice("Kwiry: the selected search backend is unavailable.");
      }
    }
  }

  private isCurrent(pluginEpoch: number, activationEpoch: number): boolean {
    return pluginEpoch === this.pluginEpoch && activationEpoch === this.activationEpoch;
  }
}

function formatStatus(status: BackendStatus): string {
  const profile = status.identity.profile === "daemon" ? "Daemon" : "In-plugin · Lexical";
  if (status.progress) {
    const total = status.progress.total === null ? "?" : String(status.progress.total);
    return `kwiry: ${profile} · ${status.progress.stage} ${status.progress.completed}/${total}`;
  }
  if (status.issue) return `kwiry: ${profile} · ${status.issue.safeMessage}`;
  const modes = status.capabilities.supportedModes.join("/");
  return `kwiry: ${profile} · ${status.phase} (${status.chunks} chunks, ${modes})`;
}

function optionalString(value: string): string | null {
  const trimmed = value.trim();
  return trimmed ? trimmed : null;
}
