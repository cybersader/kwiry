// SPDX-FileCopyrightText: 2026 cybersader
// SPDX-License-Identifier: GPL-3.0-only

import { Modal, type App } from "obsidian";

import type { HostSearchAccess } from "./backends/host-search-backend";
import {
  HOST_SEARCH_IDLE,
  HOST_SEARCH_LABEL,
  hostInvalidationMessage,
  hostSearchFailure,
  hostSearchSituationMessage,
  type HostSearchFailure,
  type HostSearchPage,
  type HostSearchSession,
} from "./host-search";
import { LatestRequestEpoch } from "./latest-request-epoch";

export class HostSearchModal extends Modal {
  private readonly interaction = new LatestRequestEpoch();
  private operation: AbortController | null = null;
  private activeQuery: string | null = null;
  private displayed: HostSearchPage | null = null;
  private renderRevision = 0;
  private view: ReturnType<HostSearchModal["createView"]> | null = null;
  private session: HostSearchSession | null = null;
  private stopInvalidation: (() => void) | null = null;

  constructor(
    app: App,
    private readonly host: HostSearchAccess,
    private readonly onClosed: () => void,
  ) {
    super(app);
  }

  onOpen(): void {
    this.setTitle(HOST_SEARCH_LABEL);
    this.contentEl.addClass("kwiry-host-search");
    const view = this.view = this.createView();
    const { form, input, status, next } = view;
    // The invalidation listener is registered here, before the first page.
    const unavailable = this.attach();
    status.textContent = unavailable ?? HOST_SEARCH_IDLE;
    this.draw(null);
    input.addEventListener("input", () => {
      this.cancel();
      this.activeQuery = null;
      this.draw(null);
      status.textContent = this.session ? HOST_SEARCH_IDLE : status.textContent;
      view.pageControls.disabled = false;
    });
    form.addEventListener("submit", (event) => {
      event.preventDefault();
      if (!this.view) return;
      this.draw(null);
      // A provider may have arrived, or a withdrawn one returned: look again.
      const missing = this.attach();
      if (missing !== null) {
        this.setStatus(missing, true);
        return;
      }
      status.textContent = HOST_SEARCH_IDLE;
      this.activeQuery = input.value;
      void this.requestPage(this.activeQuery);
    });
    next.addEventListener("click", () => {
      const continuation = this.displayed?.continuation;
      if (!this.operation && this.activeQuery !== null && typeof continuation === "string") {
        void this.requestPage(this.activeQuery, continuation);
      }
    });
    input.focus();
  }

  onClose(): void {
    this.clearInteraction();
    this.detach();
    this.view = null;
    this.contentEl.empty();
    this.onClosed();
  }

  /// Holds the service object while this view is on screen. Returns the
  /// message to show when no service is offered.
  private attach(): string | null {
    if (this.session) return null;
    const found = this.host.lookup();
    if (found.situation !== "available") return hostSearchSituationMessage(found.situation);
    const session = found.session;
    this.session = session;
    this.stopInvalidation = session.onInvalidated((reason) => {
      if (this.session !== session) return;
      const { message, detach } = hostInvalidationMessage(reason);
      this.clearInteraction();
      if (detach) this.detach();
      this.setStatus(message, true);
    });
    return null;
  }

  private detach(): void {
    const stop = this.stopInvalidation;
    this.stopInvalidation = null;
    this.session = null;
    stop?.();
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
    // Every element holding service-supplied text isolates its direction, so
    // embedded bidirectional controls cannot reorder the text around it.
    const room = pageControls.createEl("h3", { attr: { dir: "auto" } });
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
      const button = item.createEl("button", { text: row.title, attr: { type: "button", dir: "auto" } });
      item.createDiv({ cls: "kwiry-host-search-path", text: row.path, attr: { dir: "auto" } });
      if (row.excerpt) item.createDiv({ text: row.excerpt, attr: { dir: "auto" } });
      button.addEventListener("click", () => {
        if (!this.operation && this.renderRevision === revision) void this.openRow(index);
      });
    }
  }

  private async requestPage(query: string, continuation?: string): Promise<void> {
    const session = this.session;
    if (!session) return;
    const { signal, epoch } = this.start("Searching the host…");
    try {
      const page = await session.page(query, signal, continuation);
      if (!this.interaction.isCurrent(epoch)) return;
      this.draw(page);
      this.setStatus(page.status, continuation !== undefined);
    } catch (error) {
      if (this.interaction.isCurrent(epoch)) this.showFailure(hostSearchFailure(error));
    } finally {
      this.finish(epoch);
    }
  }

  private async openRow(index: number): Promise<void> {
    const session = this.session;
    const ref = this.displayed?.rows[index]?.noteRef;
    if (!session || ref === undefined) return;
    const { signal, epoch } = this.start("Opening the selected note…");
    try {
      await session.open(ref, signal);
      if (this.interaction.isCurrent(epoch)) this.close();
    } catch (error) {
      if (this.interaction.isCurrent(epoch)) this.showFailure(hostSearchFailure(error), index);
    } finally {
      this.finish(epoch);
    }
  }

  private showFailure(failure: HostSearchFailure, index?: number): void {
    // An ordinary denial clears this view only; whole-view loss arrives as an invalidation.
    if (failure.action === "clear" || failure.action === "access_lost") {
      this.activeQuery = null;
      this.draw(null);
    }
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
