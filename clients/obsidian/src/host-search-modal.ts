// SPDX-FileCopyrightText: 2026 cybersader
// SPDX-License-Identifier: GPL-3.0-only

import { Modal, TFile, type App } from "obsidian";

import {
  HOST_RESULT_UNAVAILABLE,
  HOST_SEARCH_ACCESS_LOST,
  HOST_SEARCH_GUIDANCE,
  HOST_SEARCH_IDLE,
  HOST_SEARCH_LABEL,
  hostSearchFailure,
  type HostSearchFailure,
  type HostSearchOperations,
  type HostSearchPage,
} from "./host-search";
import { LatestRequestEpoch } from "./latest-request-epoch";

export class HostSearchModal extends Modal {
  private readonly interaction = new LatestRequestEpoch();
  private operation: AbortController | null = null;
  private activeQuery: string | null = null;
  private displayed: HostSearchPage | null = null;
  private renderRevision = 0;
  private view: ReturnType<HostSearchModal["createView"]> | null = null;

  constructor(
    app: App,
    private readonly host: HostSearchOperations,
    private readonly available: boolean,
    private readonly onClosed: () => void,
    private readonly onAccessLost: () => void,
  ) {
    super(app);
  }

  onOpen(): void {
    this.setTitle(HOST_SEARCH_LABEL);
    this.contentEl.addClass("kwiry-host-search");
    const view = this.view = this.createView();
    const { form, input, status, next } = view;
    const initial = this.available ? HOST_SEARCH_IDLE : HOST_SEARCH_GUIDANCE;
    status.textContent = initial;
    this.draw(null);
    input.disabled = view.submit.disabled = !this.available;
    input.addEventListener("input", () => {
      this.cancel();
      this.activeQuery = null;
      this.draw(null);
      status.textContent = initial;
      view.pageControls.disabled = false;
    });
    form.addEventListener("submit", (event) => {
      event.preventDefault();
      if (!this.view || !this.available) return;
      this.draw(null);
      status.textContent = initial;
      this.activeQuery = input.value;
      void this.requestPage(this.activeQuery);
    });
    next.addEventListener("click", () => {
      const continuation = this.displayed?.continuation;
      if (!this.operation && this.activeQuery !== null && typeof continuation === "string") {
        void this.requestPage(this.activeQuery, continuation);
      }
    });
    if (this.available) input.focus();
  }

  onClose(): void {
    this.clearInteraction();
    this.view = null;
    this.contentEl.empty();
    this.onClosed();
  }

  clearAccess(): void {
    this.clearInteraction();
    this.setStatus(HOST_SEARCH_ACCESS_LOST);
  }

  private clearInteraction(): void {
    this.activeQuery = null;
    this.draw(null);
    if (this.view) {
      this.view.input.value = "";
      this.view.pageControls.disabled = false;
    }
    this.cancel();
  }

  private createView() {
    const form = this.contentEl.createEl("form", { cls: "kwiry-host-search-form" });
    const label = form.createEl("label", { text: "Search terms" });
    const input = label.createEl("input", { type: "text", attr: { autocomplete: "off", spellcheck: "false" } });
    const submit = form.createEl("button", { text: "Search", attr: { type: "submit" } });
    const status = this.contentEl.createEl("p", { attr: { role: "status", tabindex: "-1" } });
    const notice = this.contentEl.createEl("p", { attr: { role: "status" } });
    const pageControls = this.contentEl.createEl("fieldset", { cls: "kwiry-host-search-page" });
    const room = pageControls.createEl("h3");
    const results = pageControls.createEl("ul", { cls: "kwiry-host-search-results" });
    const next = pageControls.createEl("button", { text: "Next page", attr: { type: "button" } });
    next.hidden = true;
    return { form, input, submit, status, notice, pageControls, room, results, next };
  }

  private draw(page: HostSearchPage | null): void {
    this.displayed = page;
    const revision = ++this.renderRevision;
    if (!this.view) return;
    const { notice, room, results, next } = this.view;
    notice.textContent = page?.notice ?? "";
    room.textContent = page?.roomLabel ?? "";
    room.hidden = !page || page.roomLabel === null;
    results.empty();
    next.hidden = page === null || page.continuation === null;
    if (!page) return;
    for (const [index, row] of page.rows.entries()) {
      const item = results.createEl("li");
      const button = item.createEl("button", { text: row.title, attr: { type: "button" } });
      item.createDiv({ cls: "kwiry-host-search-path", text: row.path });
      if (row.excerpt) item.createDiv({ text: row.excerpt });
      button.addEventListener("click", () => {
        if (!this.operation && this.renderRevision === revision) void this.openRow(index);
      });
    }
  }

  private async requestPage(query: string, continuation?: string): Promise<void> {
    const { signal, epoch } = this.start("Searching the host…");
    try {
      const page = await this.host.page(query, signal, continuation);
      if (!this.interaction.isCurrent(epoch)) return;
      this.draw(page);
      this.setStatus(page.status, continuation !== undefined);
    } catch (error) {
      this.handleHostFailure(hostSearchFailure(error), epoch);
    } finally {
      this.finish(epoch);
    }
  }

  private async openRow(index: number): Promise<void> {
    const ref = this.displayed?.rows[index]?.noteRef;
    if (ref === undefined) return;
    const { signal, epoch } = this.start("Checking the selected note…");
    try {
      const path = await this.host.resolveNote(ref, signal);
      if (!this.interaction.isCurrent(epoch)) return;
      try {
        const file = this.app.vault.getAbstractFileByPath(path);
        if (!this.interaction.isCurrent(epoch)) return;
        if (!(file instanceof TFile) || file.path !== path) {
          this.showFailure({ action: "discard_result", message: HOST_RESULT_UNAVAILABLE }, index);
          return;
        }
        const leaf = this.app.workspace.getLeaf(false);
        if (!this.interaction.isCurrent(epoch)) return;
        await leaf.openFile(file);
      } catch (error) {
        if (hostSearchFailure(error).action === "access_lost") {
          this.onAccessLost();
        } else if (this.interaction.isCurrent(epoch)) {
          // We cannot establish that only the selected row became stale.
          this.clearInteraction();
          this.setStatus(HOST_RESULT_UNAVAILABLE, true);
        }
        return;
      }
      if (this.interaction.isCurrent(epoch)) this.close();
    } catch (error) {
      this.handleHostFailure(hostSearchFailure(error), epoch, index);
    } finally {
      this.finish(epoch);
    }
  }

  private handleHostFailure(failure: HostSearchFailure, epoch: number, index?: number): void {
    if (failure.action === "access_lost") this.onAccessLost();
    else if (this.interaction.isCurrent(epoch)) this.showFailure(failure, index);
  }

  private showFailure(failure: HostSearchFailure, index?: number): void {
    if (failure.action === "clear") this.draw(null);
    if (failure.action === "discard_continuation" && this.displayed) {
      this.draw({ ...this.displayed, continuation: null });
    }
    if (failure.action === "discard_result" && index !== undefined && this.displayed) {
      this.draw({ ...this.displayed, rows: this.displayed.rows.filter((_, i) => i !== index) });
    }
    this.setStatus(failure.message ?? this.displayed?.status ?? HOST_SEARCH_IDLE, failure.message !== undefined);
  }

  private setStatus(text: string, focus = false): void {
    if (!this.view) return;
    this.view.status.textContent = text;
    if (focus) this.view.status.focus();
  }

  private start(message: string): { signal: AbortSignal; epoch: number } {
    this.cancel();
    this.operation = new AbortController();
    if (this.view) this.view.pageControls.disabled = true;
    this.setStatus(message);
    return { signal: this.operation.signal, epoch: this.interaction.begin() };
  }

  private finish(epoch: number): void {
    if (!this.interaction.isCurrent(epoch)) return;
    this.operation = null;
    if (this.view) this.view.pageControls.disabled = false;
  }

  private cancel(): void {
    this.interaction.invalidate();
    const previous = this.operation;
    this.operation = null;
    previous?.abort();
  }
}
