// SPDX-FileCopyrightText: 2026 cybersader
// SPDX-License-Identifier: GPL-3.0-only

import { Notice, Plugin } from "obsidian";
import workerSource from "virtual:kwiry-worker-source";

import { ObsidianActiveVaultSource } from "../../active-vault-source";
import { D5cOwnerModal } from "./live-modal";
import { D5cOwnerService } from "./live-service";
import { formatOwnerStatus } from "./live-view";

export default class KwiryD5cOwnerPlugin extends Plugin {
  private service: D5cOwnerService | null = null;
  private statusBar: HTMLElement | null = null;
  private statusUnsubscribe: (() => void) | null = null;
  private readonly modals = new Set<D5cOwnerModal>();
  private pluginEpoch = 0;

  onload(): void {
    const epoch = ++this.pluginEpoch;
    this.addCommand({
      id: "open-text-vs-balanced",
      name: "Open Text vs Balanced search",
      callback: () => this.openComparison(),
    });
    this.addCommand({
      id: "rebuild-local-index",
      name: "Rebuild local comparison index",
      callback: () => this.rebuild(),
    });
    this.addRibbonIcon("search", "Text vs Balanced", () => this.openComparison());
    this.statusBar = this.addStatusBarItem();
    this.statusBar.setText("Kwiry: Starting index…");
    this.app.workspace.onLayoutReady(() => {
      if (epoch !== this.pluginEpoch) return;
      this.startService();
    });
  }

  onunload(): void {
    this.pluginEpoch += 1;
    this.statusUnsubscribe?.();
    this.statusUnsubscribe = null;
    for (const modal of this.modals) modal.close();
    this.modals.clear();
    const service = this.service;
    this.service = null;
    void service?.dispose().catch(() => undefined);
  }

  private startService(): void {
    if (this.service) return;
    try {
      const service = new D5cOwnerService({
        source: new ObsidianActiveVaultSource(this.app.vault),
        workerSource,
      });
      this.service = service;
      this.statusUnsubscribe = service.subscribe((status) => {
        this.statusBar?.setText(formatOwnerStatus(status));
      });
      service.start();
    } catch {
      this.statusBar?.setText("Kwiry: Index unavailable");
      new Notice("Kwiry: the local comparison index could not be started.");
    }
  }

  private openComparison(): void {
    const service = this.service;
    if (!service) {
      new Notice("Kwiry: the local comparison index is still starting.");
      return;
    }
    let modal!: D5cOwnerModal;
    modal = new D5cOwnerModal(this.app, service, () => this.modals.delete(modal));
    this.modals.add(modal);
    modal.open();
  }

  private rebuild(): void {
    const service = this.service;
    if (!service) {
      new Notice("Kwiry: the local comparison index is still starting.");
      return;
    }
    service.rebuild();
  }
}
