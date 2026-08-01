// SPDX-FileCopyrightText: 2026 cybersader
// SPDX-License-Identifier: GPL-3.0-only

import { MarkdownView, Modal, Notice, TFile, type App } from "obsidian";
import { LatestRequestEpoch } from "../../latest-request-epoch";
import { isNormalizedMarkdownPath } from "../../open-result";
import type { IndexControllerStatus } from "../../backends/in-plugin-index-controller";
import { resolveUniqueHeadingPosition } from "./heading-position";
import { coverageMessage, noMatchesMessage, resultFocusKey } from "./live-view";
import {
  D5cOwnerService,
  D5cOwnerServiceError,
  comparisonTargetKey,
  coverageFromStatus,
  type D5cCoverage,
  type D5cOwnerCandidate,
  type D5cOwnerComparison,
} from "./live-service";

const SEARCH_DEBOUNCE_MS = 180;

export class D5cOwnerModal extends Modal {
  private readonly requests = new LatestRequestEpoch();
  private inputEl: HTMLInputElement | null = null;
  private coverageEl: HTMLElement | null = null;
  private outputEl: HTMLElement | null = null;
  private statusEl: HTMLElement | null = null;
  private searchTimer: number | null = null;
  private unsubscribe: (() => void) | null = null;
  private lastTargetKey: string | null = null;
  private renderedTargetKey: string | null = null;
  private focusRestoreKey: string | null = null;
  private closed = false;

  constructor(
    app: App,
    private readonly service: D5cOwnerService,
    private readonly didClose: () => void,
  ) {
    super(app);
  }

  onOpen(): void {
    this.setTitle("Text vs Balanced");
    this.contentEl.empty();
    this.contentEl.addClass("kwiry-d5c-live");
    this.contentEl.createEl("p", {
      cls: "kwiry-d5c-live-boundary",
      text: "Local active-vault experiment. Text stays unchanged; Balanced only reorders equally strong text matches.",
    });
    const search = this.contentEl.createDiv({ cls: "kwiry-d5c-live-search" });
    this.inputEl = search.createEl("input", {
      type: "search",
      placeholder: "Search this vault…",
      attr: { "aria-label": "Search this vault" },
    });
    this.inputEl.addEventListener("input", () => this.scheduleSearch());
    const copy = search.createEl("button", { text: "Copy technical summary" });
    copy.addEventListener("click", () => void this.copyTechnicalSummary());
    this.coverageEl = this.contentEl.createDiv({ cls: "kwiry-d5c-live-coverage" });
    this.statusEl = this.contentEl.createDiv({ cls: "kwiry-d5c-live-status" });
    this.outputEl = this.contentEl.createDiv({ cls: "kwiry-d5c-live-output" });
    this.renderPrompt();
    this.unsubscribe = this.service.subscribe((status) => this.handleStatus(status));
    window.setTimeout(() => this.inputEl?.focus(), 0);
  }

  onClose(): void {
    if (this.closed) return;
    this.closed = true;
    this.requests.invalidate();
    if (this.searchTimer !== null) window.clearTimeout(this.searchTimer);
    this.searchTimer = null;
    this.unsubscribe?.();
    this.unsubscribe = null;
    this.contentEl.empty();
    this.didClose();
  }

  private handleStatus(status: IndexControllerStatus): void {
    const coverage = coverageFromStatus(status);
    const key = comparisonTargetKey(status);
    if (this.renderedTargetKey !== null && key !== this.renderedTargetKey) {
      this.clearRenderedComparison("Index updated; waiting for current results…");
    }
    this.renderCoverage(coverage);
    if (key === null) {
      this.requests.invalidate();
      this.lastTargetKey = null;
      return;
    }
    if (key === this.lastTargetKey) return;
    this.lastTargetKey = key;
    if (this.inputEl?.value.trim()) this.scheduleSearch(40);
  }

  private scheduleSearch(delay = SEARCH_DEBOUNCE_MS): void {
    if (this.searchTimer !== null) window.clearTimeout(this.searchTimer);
    this.searchTimer = null;
    const query = this.inputEl?.value.trim() ?? "";
    if (!query) {
      this.requests.invalidate();
      this.renderPrompt();
      return;
    }
    this.searchTimer = window.setTimeout(() => {
      this.searchTimer = null;
      void this.runSearch(query);
    }, delay);
  }

  private async runSearch(query: string): Promise<void> {
    const epoch = this.requests.begin();
    this.setStatus("Searching one candidate pool…");
    try {
      const comparison = await this.service.compare(query);
      if (!this.requests.isCurrent(epoch) || this.inputEl?.value.trim() !== query) return;
      this.renderComparison(comparison);
      this.setStatus(comparison.movedCandidateCount === 0
        ? "Text and Balanced agree for these results."
        : `${comparison.movedCandidateCount} displayed result${comparison.movedCandidateCount === 1 ? "" : "s"} changed position.`);
    } catch (error) {
      if (!this.requests.isCurrent(epoch)) return;
      if (error instanceof D5cOwnerServiceError
        && (error.code === "index_changed" || error.code === "index_building")) {
        this.clearRenderedComparison("Index updated; waiting for the next searchable batch…");
        return;
      }
      this.setStatus("This comparison could not be completed.");
      new Notice("Kwiry: Text vs Balanced comparison failed.");
    }
  }

  private renderComparison(comparison: D5cOwnerComparison): void {
    const output = this.outputEl;
    if (!output) return;
    this.captureFocusedResult();
    output.empty();
    this.renderedTargetKey = comparison.targetKey;
    this.renderCoverage(comparison.coverage);
    if (comparison.textOrder.length === 0) {
      output.createDiv({
        cls: "kwiry-d5c-live-empty",
        text: noMatchesMessage(comparison.coverage),
      });
      this.restoreResultFocus();
      return;
    }
    const candidates = new Map(comparison.candidates.map((candidate) =>
      [candidate.ordinal, candidate] as const));
    const textRanks = new Map(comparison.textOrder.map((ordinal, rank) => [ordinal, rank + 1]));
    const panels = output.createDiv({ cls: "kwiry-d5c-live-panels" });
    this.renderPanel(panels, "Text", comparison.textOrder, candidates, textRanks, false);
    this.renderPanel(
      panels,
      "Balanced preview",
      comparison.balancedOrder,
      candidates,
      textRanks,
      true,
    );
    this.restoreResultFocus();
  }

  private renderPanel(
    container: HTMLElement,
    heading: string,
    order: readonly number[],
    candidates: ReadonlyMap<number, D5cOwnerCandidate>,
    textRanks: ReadonlyMap<number, number>,
    balanced: boolean,
  ): void {
    const panel = container.createDiv({ cls: "kwiry-d5c-live-panel" });
    panel.createEl("h3", { text: heading });
    panel.createEl("p", {
      cls: "kwiry-d5c-live-panel-note",
      text: balanced
        ? "Uses gentle recency and reference/archive folders only within tied text strength."
        : "Original lexical order.",
    });
    const list = panel.createDiv({ cls: "kwiry-d5c-live-results" });
    order.forEach((ordinal, rank) => {
      const candidate = candidates.get(ordinal);
      if (!candidate) return;
      const row = list.createDiv({ cls: "kwiry-d5c-live-result" });
      row.dataset.kwiryFocusKey = resultFocusKey(
        balanced ? "balanced" : "text",
        candidate.ordinal,
        candidate.hit.path,
        candidate.hit.heading_path,
      );
      row.setAttribute("role", "button");
      row.setAttribute("tabindex", "0");
      row.addEventListener("click", (event) => this.openCandidate(candidate, event));
      row.addEventListener("keydown", (event) => {
        if (event.key !== "Enter" && event.key !== " ") return;
        event.preventDefault();
        this.openCandidate(candidate, event);
      });
      const top = row.createDiv({ cls: "kwiry-d5c-live-result-top" });
      top.createSpan({ cls: "kwiry-d5c-live-rank", text: `#${rank + 1}` });
      top.createSpan({
        cls: "kwiry-result-title",
        text: candidate.hit.frontmatter.title ?? basename(candidate.hit.path),
      });
      const textRank = textRanks.get(ordinal);
      if (balanced && textRank !== undefined && textRank !== rank + 1) {
        top.createSpan({ cls: "kwiry-d5c-live-movement", text: `From Text #${textRank}` });
      }
      const breadcrumb = candidate.hit.heading_path.length > 0
        ? ` › ${candidate.hit.heading_path.join(" › ")}`
        : "";
      row.createDiv({ cls: "kwiry-result-meta", text: `${candidate.hit.path}${breadcrumb}` });
      const excerpt = row.createDiv({ cls: "kwiry-result-excerpt" });
      for (const segment of candidate.hit.excerpt) {
        const text = segment.text.replace(/\s+/g, " ");
        if (segment.highlighted) excerpt.createEl("mark", { text });
        else excerpt.appendText(text);
      }
      if (balanced && textRank !== undefined && textRank !== rank + 1) {
        row.createDiv({
          cls: "kwiry-d5c-live-reason",
          text: "Moved among equally strong text matches.",
        });
      }
    });
  }

  private openCandidate(candidate: D5cOwnerCandidate, event: MouseEvent | KeyboardEvent): void {
    if (!isNormalizedMarkdownPath(candidate.hit.path)) {
      new Notice("Kwiry: this result does not contain a safe Markdown path.");
      return;
    }
    const file = this.app.vault.getAbstractFileByPath(candidate.hit.path);
    if (!(file instanceof TFile)) {
      new Notice("Kwiry: this result is no longer present in the vault.");
      return;
    }
    const headingPath = candidate.hit.heading_path;
    const position = headingPath.length === 0
      ? null
      : resolveUniqueHeadingPosition(
        this.app.metadataCache.getFileCache(file)?.headings ?? [],
        headingPath,
      );
    if (headingPath.length > 0 && position === null) {
      new Notice("Kwiry: this result's matched heading is no longer uniquely available.");
      return;
    }
    const placement = event.ctrlKey || event.metaKey ? "tab" : false;
    const leaf = this.app.workspace.getLeaf(placement);
    void leaf.openFile(file).then(() => {
      if (position === null || !(leaf.view instanceof MarkdownView)) return;
      leaf.view.editor.setCursor(position);
      leaf.view.editor.scrollIntoView({ from: position, to: position }, true);
    }).catch(() => new Notice("Kwiry: this result could not be opened."));
  }

  private clearRenderedComparison(message: string): void {
    this.captureFocusedResult();
    this.renderedTargetKey = null;
    this.outputEl?.empty();
    this.outputEl?.createDiv({
      cls: "kwiry-d5c-live-empty",
      text: message,
    });
    this.setStatus("");
  }

  private captureFocusedResult(): void {
    const output = this.outputEl;
    const active = document.activeElement;
    if (!output || !(active instanceof HTMLElement) || !output.contains(active)) return;
    const row = active.closest<HTMLElement>("[data-kwiry-focus-key]");
    this.focusRestoreKey = row?.dataset.kwiryFocusKey ?? null;
  }

  private restoreResultFocus(): void {
    const key = this.focusRestoreKey;
    if (key === null) return;
    this.focusRestoreKey = null;
    const rows = this.outputEl
      ? Array.from(this.outputEl.querySelectorAll<HTMLElement>("[data-kwiry-focus-key]"))
      : [];
    for (const row of rows) {
      if (row.dataset.kwiryFocusKey !== key) continue;
      row.focus();
      return;
    }
    this.inputEl?.focus();
  }

  private renderCoverage(coverage: D5cCoverage): void {
    this.coverageEl?.setText(coverageMessage(coverage));
    this.coverageEl?.toggleClass("is-partial", coverage.kind === "partial");
    this.coverageEl?.toggleClass("is-updating", coverage.kind === "updating");
    this.coverageEl?.toggleClass("is-incomplete", coverage.kind === "incomplete");
    this.coverageEl?.toggleClass("is-unavailable", coverage.kind === "unavailable");
  }

  private renderPrompt(): void {
    this.renderedTargetKey = null;
    this.focusRestoreKey = null;
    this.outputEl?.empty();
    this.outputEl?.createDiv({
      cls: "kwiry-d5c-live-empty",
      text: "Type a normal search to compare Text and Balanced.",
    });
    this.setStatus("");
  }

  private setStatus(text: string): void {
    this.statusEl?.setText(text);
  }

  private async copyTechnicalSummary(): Promise<void> {
    try {
      await navigator.clipboard.writeText(`${JSON.stringify(this.service.technicalSummary(), null, 2)}\n`);
      new Notice("Kwiry: aggregate technical summary copied.");
    } catch {
      new Notice("Kwiry: technical summary could not be copied.");
    }
  }
}

function basename(path: string): string {
  const name = path.slice(path.lastIndexOf("/") + 1);
  return name.endsWith(".md") ? name.slice(0, -3) : name;
}
