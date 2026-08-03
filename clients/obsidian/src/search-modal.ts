// SPDX-FileCopyrightText: 2026 cybersader
// SPDX-License-Identifier: GPL-3.0-only
//
// Modal UX (keyboard flow and result-row structure) is informed by Omnisearch
// (https://github.com/scambier/obsidian-omnisearch), GPL-3.0.

import { Notice, Platform, SuggestModal, TFile } from "obsidian";
import type { SearchMode } from "./api";
import type { BackendSearchHit, BackendStatus, SearchBackend } from "./backend";
import type KwiryPlugin from "./main";
import { emptyStateMessage } from "./empty-state";
import {
  captureLinkInsertionTarget,
  deepestMatchedHeading,
  insertMarkdownLink,
  type LinkInsertionKind,
  type LinkInsertionTarget,
} from "./link-insertion";
import { validateOpenResult, type OpenTarget } from "./open-result";
import { progressLine } from "./progress-line";
import { nextSearchMode, selectSupportedMode, selectedSearchModeOptions } from "./search-mode";
import {
  SEARCH_SHORTCUT_BINDINGS,
  searchShortcutAction,
  type SearchShortcutAction,
  type SearchShortcutPlatform,
} from "./search-shortcuts";
import { SearchSessionController } from "./search-session";

interface ModalResult {
  hit: BackendSearchHit;
}

type OpenPlacement = "current" | "tab" | "split";

export class KwirySearchModal extends SuggestModal<ModalResult> {
  private readonly session: SearchSessionController;
  private mode: SearchMode;
  private readonly modeButtons = new Map<SearchMode, HTMLButtonElement>();
  private lastErrorCode: string | null = null;
  private progressEl: HTMLElement | null = null;
  private progressTimer: number | null = null;
  private progressFailureRecorded = false;
  private readonly shortcutPlatform: SearchShortcutPlatform;
  private readonly linkInsertionTarget: LinkInsertionTarget | null;

  constructor(
    private readonly plugin: KwiryPlugin,
    private readonly backend: SearchBackend,
    status: BackendStatus,
  ) {
    super(plugin.app);
    this.shortcutPlatform = Platform.isMacOS ? "macos" : "other";
    this.linkInsertionTarget = this.captureLinkInsertionTarget();
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
    this.createProgressLine(status);
    this.setInstructions(
      SEARCH_SHORTCUT_BINDINGS.map(({ command, purpose }) => ({ command, purpose })),
    );
    for (const binding of SEARCH_SHORTCUT_BINDINGS) {
      if (!binding.register) continue;
      this.scope.register([...binding.modifiers], binding.key, (event) => {
        event.preventDefault();
        const action = this.shortcutAction(event);
        if (action === "move-down" || action === "move-up") {
          this.moveActiveSuggestion(action);
        } else if (action === "cycle-mode") {
          this.selectMode(nextSearchMode(this.mode, this.session.supportedModes));
        } else if (action !== null) {
          this.selectActiveSuggestion(event);
        }
        return false;
      });
    }
  }

  async getSuggestions(query: string): Promise<ModalResult[]> {
    const filters = this.backend.identity.profile === "daemon"
      && this.plugin.settings.vaultId.trim().length > 0
      ? { vault_id: this.plugin.settings.vaultId.trim() }
      : undefined;
    return this.plugin.captureDiagnostic("info", "search.lifecycle", {
      profile: this.backend.identity.profile,
      mode: this.mode,
      limit: this.plugin.settings.resultLimit,
      operation: "search",
      subsystem: "search_session",
    }, async (event) => {
      const outcome = await this.session.search(query, {
        limit: this.plugin.settings.resultLimit,
        filters,
      });
      switch (outcome.kind) {
        case "results":
          this.lastErrorCode = null;
          event.set({
            outcome: "succeeded",
            resultCount: outcome.execution.response.hits.length,
          });
          // A completed search that matched nothing must say so. Without
          // this, no-matches and no-query-typed render identically blank.
          this.setEmptyState(
            outcome.execution.response.hits.length === 0
              ? emptyStateMessage("no-matches", query)
              : emptyStateMessage("prompt"),
          );
          return outcome.execution.response.hits.map((hit) => ({ hit }));
        case "error":
          event.setLevel("error");
          event.set({
            outcome: "failed",
            ...this.plugin.diagnosticErrorDetails(outcome.error),
          });
          if (outcome.error.code !== this.lastErrorCode) {
            this.lastErrorCode = outcome.error.code;
            new Notice(`Kwiry: ${outcome.error.safeMessage}`);
          }
          this.setEmptyState(emptyStateMessage("error"));
          return [];
        case "empty":
          this.lastErrorCode = null;
          event.set({ outcome: "skipped" });
          this.setEmptyState(emptyStateMessage("prompt"));
          return [];
        case "stale":
          event.set({ outcome: "superseded" });
          // A superseded request must not overwrite the newer request's
          // message; leave whatever the in-flight search will set.
          return [];
      }
    });
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

  selectSuggestion(result: ModalResult, event: MouseEvent | KeyboardEvent): void {
    // SuggestModal closes before onChooseSuggestion. Background open is the one
    // action that must bypass that path while still using the active suggestion.
    if (this.shortcutAction(event) === "open-background") {
      this.openResult(result, "tab");
      return;
    }
    super.selectSuggestion(result, event);
  }

  onChooseSuggestion(result: ModalResult, event: MouseEvent | KeyboardEvent): void {
    const action = this.shortcutAction(event);
    switch (action) {
      case "open-new-tab":
      case "open-background":
        this.openResult(result, "tab");
        break;
      case "open-new-split":
        this.openResult(result, "split");
        break;
      case "insert-note-link":
        this.insertResultLink(result, "note");
        break;
      case "insert-section-link":
        this.insertResultLink(result, "section");
        break;
      default:
        // Preserve modified mouse selection from the previous implementation;
        // keyboard chords are resolved by the table above.
        this.openResult(result, !("key" in event) && (event.ctrlKey || event.metaKey)
          ? "tab"
          : "current");
        break;
    }
  }

  private shortcutAction(event: MouseEvent | KeyboardEvent): SearchShortcutAction | null {
    if (!("key" in event)) return null;
    return searchShortcutAction(event, this.shortcutPlatform);
  }

  private moveActiveSuggestion(action: "move-down" | "move-up"): void {
    this.inputEl.dispatchEvent(new KeyboardEvent("keydown", {
      key: action === "move-down" ? "ArrowDown" : "ArrowUp",
      bubbles: true,
      cancelable: true,
    }));
  }

  private captureLinkInsertionTarget(): LinkInsertionTarget | null {
    const activeEditor = this.app.workspace.activeEditor;
    const editor = activeEditor?.editor;
    const sourceFile = activeEditor?.file;
    if (!editor || !sourceFile) return null;
    return captureLinkInsertionTarget(editor, sourceFile.path);
  }

  private openResult(result: ModalResult, placement: OpenPlacement): void {
    const validated = this.validatedResult(result);
    if (!validated) return;
    const leaf = this.app.workspace.getLeaf(placement === "current" ? false : placement);
    void leaf.openFile(validated.file, {
      eState: validated.target.subpath === undefined
        ? undefined
        : { subpath: validated.target.subpath },
    }).catch((error: unknown) => {
      this.plugin.recordCaughtFailure("ui", "open", error, {
        profile: this.backend.identity.profile,
      });
    });
  }

  private insertResultLink(result: ModalResult, kind: LinkInsertionKind): void {
    const validated = this.validatedResult(result);
    if (!validated) return;
    if (result.hit.format !== "markdown") {
      new Notice("Kwiry: link insertion is available only for Markdown results.");
      return;
    }
    const target = this.linkInsertionTarget;
    if (!target) {
      new Notice("Kwiry: open this search from a Markdown editor to insert a link.");
      return;
    }
    const outcome = insertMarkdownLink(
      this.app.fileManager,
      validated.file,
      target,
      deepestMatchedHeading(result.hit.heading_path),
      kind,
    );
    if (!outcome.ok) new Notice(`Kwiry: ${outcome.safeMessage}`);
  }

  private validatedResult(result: ModalResult): { file: TFile; target: OpenTarget } | null {
    const activeBackend = this.plugin.getActiveBackendIdentity();
    if (!activeBackend) {
      new Notice("Kwiry: the search backend is no longer active.");
      return null;
    }
    const decision = validateOpenResult(
      result.hit,
      activeBackend,
      this.plugin.settings.daemonCurrentVaultId,
    );
    if (!decision.ok) {
      new Notice(`Kwiry: ${decision.safeMessage}`);
      return null;
    }
    const file = this.app.vault.getAbstractFileByPath(decision.path);
    if (!(file instanceof TFile)) {
      new Notice("Kwiry: this result is not present in the current vault.");
      return null;
    }
    const target: OpenTarget = decision.subpath === undefined
      ? { path: decision.path }
      : { path: decision.path, subpath: decision.subpath };
    return { file, target };
  }

  onClose(): void {
    if (this.progressTimer !== null) {
      window.clearInterval(this.progressTimer);
      this.progressTimer = null;
    }
    this.session.dispose();
    super.onClose();
  }

  /// A single line under the input reporting what indexing is doing right now.
  /// It polls rather than subscribing because the modal can outlive any one
  /// status push, and it hides itself whenever nothing is in flight so a ready
  /// index shows no chrome at all.
  private createProgressLine(status: BackendStatus): void {
    const line = this.contentEl.createDiv({ cls: "kwiry-progress-line" });
    this.progressEl = line;
    this.resultContainerEl.before(line);
    this.renderProgress(status);
    this.progressTimer = window.setInterval(() => {
      void this.refreshProgress();
    }, 400);
  }

  private async refreshProgress(): Promise<void> {
    try {
      this.renderProgress(await this.backend.status());
      this.progressFailureRecorded = false;
    } catch (error) {
      if (!this.progressFailureRecorded) {
        this.progressFailureRecorded = true;
        this.plugin.recordCaughtFailure("ui", "poll", error, {
          profile: this.backend.identity.profile,
        });
      }
      // A failed status poll is not worth surfacing here: the search path
      // already reports backend errors through the notice.
    }
  }

  private renderProgress(status: BackendStatus): void {
    const line = this.progressEl;
    if (!line) return;
    const text = progressLine(status);
    line.setText(text ?? "");
    line.toggleClass("is-active", text !== null);
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
