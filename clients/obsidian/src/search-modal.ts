// SPDX-FileCopyrightText: 2026 cybersader
// SPDX-License-Identifier: GPL-3.0-only
//
// Modal UX (keyboard flow and result-row structure) is informed by Omnisearch
// (https://github.com/scambier/obsidian-omnisearch), GPL-3.0.

import { Notice, SuggestModal, TFile } from "obsidian";

import type { SearchMode } from "./api";
import type { BackendSearchHit, BackendStatus, SearchBackend } from "./backend";
import type KwiryPlugin from "./main";
import { emptyStateMessage } from "./empty-state";
import { validateOpenResult } from "./open-result";
import { nextSearchMode, selectSupportedMode, selectedSearchModeOptions } from "./search-mode";
import { SearchSessionController } from "./search-session";

interface ModalResult {
  hit: BackendSearchHit;
}

export class KwirySearchModal extends SuggestModal<ModalResult> {
  private readonly session: SearchSessionController;
  private mode: SearchMode;
  private readonly modeButtons = new Map<SearchMode, HTMLButtonElement>();
  private lastErrorCode: string | null = null;

  constructor(
    private readonly plugin: KwiryPlugin,
    private readonly backend: SearchBackend,
    status: BackendStatus,
  ) {
    super(plugin.app);
    this.mode = selectSupportedMode(
      backend.identity.profile === "daemon" ? plugin.settings.defaultMode : "lexical",
      status.capabilities.supportedModes,
    );
    this.session = new SearchSessionController(
      backend,
      status.capabilities.supportedModes,
      this.mode,
    );
    this.setPlaceholder("Search your notes with kwiry…");
    this.createProfileLabel(status);
    this.createModeControl();
    this.setInstructions([
      { command: "↵", purpose: "open" },
      { command: "ctrl ↵", purpose: "open in new tab" },
      { command: "tab", purpose: "cycle requested mode" },
    ]);
    this.scope.register([], "Tab", (event) => {
      event.preventDefault();
      this.selectMode(nextSearchMode(this.mode, this.session.supportedModes));
      return false;
    });
  }

  async getSuggestions(query: string): Promise<ModalResult[]> {
    const filters = this.backend.identity.profile === "daemon"
      && this.plugin.settings.vaultId.trim().length > 0
      ? { vault_id: this.plugin.settings.vaultId.trim() }
      : undefined;
    const outcome = await this.session.search(query, {
      limit: this.plugin.settings.resultLimit,
      filters,
    });
    switch (outcome.kind) {
      case "results":
        this.lastErrorCode = null;
        // A completed search that matched nothing must say so. Without
        // this, no-matches and no-query-typed render identically blank.
        this.setEmptyState(
          outcome.execution.response.hits.length === 0
            ? emptyStateMessage("no-matches", query)
            : emptyStateMessage("prompt"),
        );
        return outcome.execution.response.hits.map((hit) => ({ hit }));
      case "error":
        if (outcome.error.code !== this.lastErrorCode) {
          this.lastErrorCode = outcome.error.code;
          new Notice(`Kwiry: ${outcome.error.safeMessage}`);
        }
        this.setEmptyState(emptyStateMessage("error"));
        return [];
      case "empty":
        this.lastErrorCode = null;
        this.setEmptyState(emptyStateMessage("prompt"));
        return [];
      case "stale":
        // A superseded request must not overwrite the newer request's
        // message; leave whatever the in-flight search will set.
        return [];
    }
  }

  private setEmptyState(text: string): void {
    this.emptyStateText = text;
  }

  renderSuggestion(result: ModalResult, el: HTMLElement): void {
    const { hit } = result;
    el.addClass("kwiry-result");
    const title = el.createDiv({ cls: "kwiry-result-title" });
    title.setText(hit.frontmatter.title ?? basename(hit.path));
    const meta = el.createDiv({ cls: "kwiry-result-meta" });
    const breadcrumb = hit.heading_path.length > 0 ? ` › ${hit.heading_path.join(" › ")}` : "";
    meta.setText(`${hit.path}${breadcrumb}`);
    const excerpt = el.createDiv({ cls: "kwiry-result-excerpt" });
    for (const segment of hit.excerpt) {
      const text = segment.text.replace(/\s+/g, " ");
      if (segment.highlighted) excerpt.createEl("mark", { text });
      else excerpt.appendText(text);
    }
  }

  onChooseSuggestion(result: ModalResult, event: MouseEvent | KeyboardEvent): void {
    const activeBackend = this.plugin.getActiveBackendIdentity();
    if (!activeBackend) {
      new Notice("Kwiry: the search backend is no longer active.");
      return;
    }
    const decision = validateOpenResult(
      result.hit,
      activeBackend,
      this.plugin.settings.daemonCurrentVaultId,
    );
    if (!decision.ok) {
      new Notice(`Kwiry: ${decision.safeMessage}`);
      return;
    }

    const file = this.app.vault.getAbstractFileByPath(decision.path);
    if (!(file instanceof TFile)) {
      new Notice("Kwiry: this result is not present in the current vault.");
      return;
    }
    const newTab = event.ctrlKey || event.metaKey;
    const heading = result.hit.heading_path.at(-1);
    const leaf = this.app.workspace.getLeaf(newTab);
    void leaf.openFile(file, {
      eState: heading ? { subpath: `#${heading}` } : undefined,
    });
  }

  onClose(): void {
    this.session.dispose();
    super.onClose();
  }

  private createProfileLabel(status: BackendStatus): void {
    const profile = this.contentEl.createDiv({ cls: "kwiry-profile-label" });
    const modes = status.capabilities.supportedModes
      .map((mode) => mode[0]!.toUpperCase() + mode.slice(1))
      .join(" / ");
    profile.setText(`${status.identity.label} · ${modes}`);
    this.resultContainerEl.before(profile);
  }

  private createModeControl(): void {
    const control = this.contentEl.createDiv({ cls: "kwiry-mode-control" });
    control.createSpan({ cls: "kwiry-mode-label", text: "Requested mode" });
    const segments = control.createDiv({ cls: "kwiry-mode-segments" });
    segments.setAttribute("role", "group");
    segments.setAttribute("aria-label", "Requested search mode");

    for (const option of selectedSearchModeOptions(this.mode, this.session.supportedModes)) {
      const button = segments.createEl("button", {
        cls: "kwiry-mode-segment",
        text: option.label,
        attr: {
          type: "button",
          "aria-pressed": String(option.selected),
        },
      });
      button.addEventListener("click", () => {
        this.selectMode(option.mode);
        this.inputEl.focus();
      });
      this.modeButtons.set(option.mode, button);
    }

    this.resultContainerEl.before(control);
    this.syncModeControl();
  }

  private selectMode(mode: SearchMode): void {
    this.session.setMode(mode);
    this.mode = mode;
    this.syncModeControl();
    this.inputEl.dispatchEvent(new Event("input"));
  }

  private syncModeControl(): void {
    for (const option of selectedSearchModeOptions(this.mode, this.session.supportedModes)) {
      const button = this.modeButtons.get(option.mode);
      if (!button) continue;
      button.classList.toggle("is-selected", option.selected);
      button.setAttribute("aria-pressed", String(option.selected));
    }
  }
}

function basename(path: string): string {
  const slash = path.lastIndexOf("/");
  const name = slash >= 0 ? path.slice(slash + 1) : path;
  return name.replace(/\.md$/u, "");
}
