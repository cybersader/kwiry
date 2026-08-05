// SPDX-FileCopyrightText: 2026 cybersader
// SPDX-License-Identifier: GPL-3.0-only
//
// Modal UX (keyboard flow and result-row structure) is informed by Omnisearch
// (https://github.com/scambier/obsidian-omnisearch), GPL-3.0.

import { Notice, Platform, SuggestModal, TFile } from "obsidian";
import type { SearchMode } from "./api";
import type { BackendSearchHit, BackendStatus, SearchBackend } from "./backend";
import type KwiryPlugin from "./main";
import { shouldNoticeSearchError } from "./empty-state";
import {
  captureLinkInsertionTarget,
  deepestMatchedHeading,
  insertMarkdownLink,
  type LinkInsertionKind,
  type LinkInsertionTarget,
} from "./link-insertion";
import { validateOpenResult, type OpenTarget } from "./open-result";
import { nextSearchMode, selectSupportedMode, selectedSearchModeOptions } from "./search-mode";
import {
  presentBackgroundIndex,
  presentQueryStatus,
  type QueryStatusFacts,
  type QueryStatusPresentation,
} from "./search-status-presenter";
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

export const SEARCH_STATUS_ANIMATION_DELAY_MS = 180;

export class KwirySearchModal extends SuggestModal<ModalResult> {
  private readonly session: SearchSessionController;
  private mode: SearchMode;
  private readonly modeButtons = new Map<SearchMode, HTMLButtonElement>();
  private lastErrorCode: string | null = null;
  private queryStatusEl: HTMLElement | null = null;
  private indexStatusEl: HTMLElement | null = null;
  private progressTimer: number | null = null;
  private queryAnimationTimer: number | null = null;
  private progressFailureRecorded = false;
  private progressEpoch = 0;
  private requestEpoch = 0;
  private activeRequestEpoch = 0;
  private lastQueryStatusText = "";
  private lastIndexStatusText = "";
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
    this.emptyStateText = "";
    this.createProfileLabel(status);
    this.createModeControl();
    this.createStatusRail(status);
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
    const epoch = ++this.requestEpoch;
    this.beginQueryStatus(epoch, query.trim().length > 0
      ? { phase: "searching" }
      : { phase: "prompt" });
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
        case "results": {
          const resultCount = outcome.execution.response.hits.length;
          if (!this.completeQueryStatus(epoch, {
            phase: "settled",
            resultCount,
            candidateWindow: outcome.execution.candidateWindow,
          })) {
            event.set({ outcome: "superseded" });
            return [];
          }
          this.lastErrorCode = null;
          event.set({ outcome: "succeeded", resultCount });
          return outcome.execution.response.hits.map((hit) => ({ hit }));
        }
        case "error":
          if (!this.completeQueryStatus(epoch, {
            phase: "error",
            code: outcome.error.code,
            safeMessage: outcome.error.safeMessage,
          })) {
            event.set({ outcome: "superseded" });
            return [];
          }
          event.setLevel("error");
          event.set({
            outcome: "failed",
            ...this.plugin.diagnosticErrorDetails(outcome.error),
          });
          if (shouldNoticeSearchError(outcome.error.code)
            && outcome.error.code !== this.lastErrorCode) {
            new Notice(`Kwiry: ${outcome.error.safeMessage}`);
          }
          this.lastErrorCode = outcome.error.code;
          return [];
        case "empty":
          if (!this.completeQueryStatus(epoch, { phase: "prompt" })) {
            event.set({ outcome: "superseded" });
            return [];
          }
          this.lastErrorCode = null;
          event.set({ outcome: "skipped" });
          return [];
        case "stale":
          event.set({ outcome: "superseded" });
          // A superseded request must not overwrite the newer request's
          // status or clear its delayed activity indicator.
          return [];
      }
    });
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
    this.activeRequestEpoch = ++this.requestEpoch;
    this.progressEpoch += 1;
    this.clearQueryAnimationTimer();
    if (this.progressTimer !== null) {
      window.clearTimeout(this.progressTimer);
      this.progressTimer = null;
    }
    this.session.dispose();
    super.onClose();
  }

  /// Query status is a polite atomic live region. Aggregate index progress is
  /// deliberately a separate, non-live line so the 400 ms poll remains quiet.
  private createStatusRail(status: BackendStatus): void {
    const rail = this.contentEl.createDiv({ cls: "kwiry-status-rail" });
    this.queryStatusEl = rail.createDiv({ cls: "kwiry-query-status" });
    this.queryStatusEl.setAttribute("role", "status");
    this.queryStatusEl.setAttribute("aria-live", "polite");
    this.queryStatusEl.setAttribute("aria-atomic", "true");
    this.indexStatusEl = rail.createDiv({ cls: "kwiry-index-status" });
    this.resultContainerEl.before(rail);
    this.applyQueryStatus(presentQueryStatus({ phase: "prompt" }));
    this.renderProgress(status);
    this.scheduleProgressRefresh();
  }

  private scheduleProgressRefresh(): void {
    if (this.progressTimer !== null) return;
    const epoch = this.progressEpoch;
    this.progressTimer = window.setTimeout(() => {
      this.progressTimer = null;
      if (epoch !== this.progressEpoch) return;
      void this.refreshProgress(epoch);
    }, 400);
  }

  private async refreshProgress(epoch: number): Promise<void> {
    try {
      const status = await this.backend.status();
      if (epoch !== this.progressEpoch) return;
      this.renderProgress(status);
      this.progressFailureRecorded = false;
    } catch (error) {
      if (epoch !== this.progressEpoch) return;
      if (!this.progressFailureRecorded) {
        this.progressFailureRecorded = true;
        this.plugin.recordCaughtFailure("ui", "poll", error, {
          profile: this.backend.identity.profile,
        });
      }
      // A failed status poll is not worth surfacing here: the search path
      // already reports backend errors through the notice.
    } finally {
      if (epoch === this.progressEpoch) this.scheduleProgressRefresh();
    }
  }

  private beginQueryStatus(epoch: number, facts: QueryStatusFacts): void {
    this.activeRequestEpoch = epoch;
    this.clearQueryAnimationTimer();
    const presentation = presentQueryStatus(facts);
    this.applyQueryStatus(presentation);
    if (presentation.state !== "searching") return;
    this.queryAnimationTimer = window.setTimeout(() => {
      this.queryAnimationTimer = null;
      if (this.activeRequestEpoch !== epoch) return;
      this.queryStatusEl?.classList.add("is-animation-ready");
    }, SEARCH_STATUS_ANIMATION_DELAY_MS);
  }

  private completeQueryStatus(epoch: number, facts: QueryStatusFacts): boolean {
    if (epoch !== this.activeRequestEpoch) return false;
    this.clearQueryAnimationTimer();
    this.applyQueryStatus(presentQueryStatus(facts));
    return true;
  }

  private applyQueryStatus(presentation: QueryStatusPresentation): void {
    const line = this.queryStatusEl;
    if (!line) return;
    if (presentation.text !== this.lastQueryStatusText) {
      this.lastQueryStatusText = presentation.text;
      line.setText(presentation.text);
    }
    line.setAttribute("data-state", presentation.state);
    line.classList.toggle("is-searching", presentation.state === "searching");
    line.classList.toggle("is-error", presentation.state === "error");
    if (presentation.state !== "searching") line.classList.remove("is-animation-ready");
    this.resultContainerEl.setAttribute("aria-busy", String(presentation.busy));
  }

  private clearQueryAnimationTimer(): void {
    if (this.queryAnimationTimer === null) return;
    window.clearTimeout(this.queryAnimationTimer);
    this.queryAnimationTimer = null;
  }

  private renderProgress(status: BackendStatus): void {
    const line = this.indexStatusEl;
    if (!line) return;
    const presentation = presentBackgroundIndex(status);
    if (presentation.text !== this.lastIndexStatusText) {
      this.lastIndexStatusText = presentation.text;
      line.setText(presentation.text);
    }
    line.setAttribute("data-state", presentation.state);
    line.classList.toggle("has-status", presentation.state !== "quiet");
    line.classList.toggle("is-attention", presentation.state === "attention");
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
