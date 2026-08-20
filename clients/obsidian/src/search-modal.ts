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
  groupSearchExecution,
  groupedSearchHitLimit,
  type GroupedSearchResult,
  type QualifiedSourceIdentity,
  type SourceSearchGroup,
} from "./grouped-search";
import {
  captureLinkInsertionTarget,
  deepestMatchedHeading,
  insertMarkdownLink,
  type LinkInsertionKind,
  type LinkInsertionTarget,
} from "./link-insertion";
import { pageNavigationShortfall, validateOpenResult, type OpenTarget } from "./open-result";
import { supportsSectionLinks } from "./source-formats";
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

type ModalResult =
  | {
    kind: "source";
    group: SourceSearchGroup;
  }
  | {
    kind: "section";
    group: SourceSearchGroup;
    hit: BackendSearchHit;
    returnedSectionIndex: number;
  };

interface SettledProjection {
  query: string;
  mode: SearchMode;
  backendInstanceId: string;
  generation: string | null;
  grouped: GroupedSearchResult;
}

type ResultView =
  | { kind: "sources" }
  | { kind: "sections"; source: QualifiedSourceIdentity };

type OpenPlacement = "current" | "tab" | "split";

interface FormatChipPresentation {
  label: string;
  accessibleLabel: string;
}

export const FORMAT_CHIP_PRESENTATIONS = {
  markdown: { label: "MD", accessibleLabel: "Markdown source format" },
  text: { label: "TXT", accessibleLabel: "Plain text source format" },
  base: { label: "BASE", accessibleLabel: "Obsidian Base source format" },
  canvas: { label: "CANVAS", accessibleLabel: "Obsidian Canvas source format" },
  excalidraw: { label: "EXCA", accessibleLabel: "Excalidraw drawing source format" },
  docx: { label: "DOCX", accessibleLabel: "Word document source format" },
  pdf: { label: "PDF", accessibleLabel: "PDF source format" },
  excel: { label: "XLSX", accessibleLabel: "Excel workbook source format" },
  html: { label: "HTML", accessibleLabel: "HTML document source format" },
} as const satisfies Record<BackendSearchHit["format"], FormatChipPresentation>;

export const SEARCH_STATUS_ANIMATION_DELAY_MS = 180;

/**
 * Title for one drilled section row. A PDF has no heading path by construction,
 * so without the page locator every drilled row of every PDF would read
 * "Match 1", "Match 2" and name nothing the user could act on. The page is a
 * label here and nowhere else: it is never added to `heading_path`, never
 * queried, never scored.
 */
export function sectionResultTitle(
  hit: Pick<BackendSearchHit, "format" | "locator" | "heading_path">,
  returnedSectionIndex: number,
): string {
  const heading = hit.heading_path.at(-1);
  if (heading !== undefined) return heading;
  if (hit.format === "pdf" && hit.locator?.kind === "pdf_page") {
    return `Page ${hit.locator.page}`;
  }
  if (hit.format === "excel" && hit.locator?.kind === "excel_cell") {
    return `${hit.locator.sheet} · ${hit.locator.cell}`;
  }
  return `Match ${returnedSectionIndex + 1}`;
}

/** The view type a leaf is showing, or null when it cannot be observed. */
function openedViewType(leaf: { view?: unknown }): string | null {
  const view = leaf.view;
  if (typeof view !== "object" || view === null) return null;
  const getViewType = (view as { getViewType?: unknown }).getViewType;
  if (typeof getViewType !== "function") return null;
  const type: unknown = (getViewType as () => unknown).call(view);
  return typeof type === "string" ? type : null;
}

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
  private lastSearchable: boolean;
  private latestStatusGeneration: string | null;
  private readonly shortcutPlatform: SearchShortcutPlatform;
  private readonly linkInsertionTarget: LinkInsertionTarget | null;
  private settledProjection: SettledProjection | null = null;
  private resultView: ResultView = { kind: "sources" };
  private localProjectionRefresh = false;
  private selectionTransactionActive = false;
  private restorationTimer: number | null = null;

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
    this.lastSearchable = status.searchable;
    this.latestStatusGeneration = status.identity.instanceId === backend.identity.instanceId
      ? status.generation
      : null;
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
        } else if (action === "back-to-sources") {
          this.returnToSources();
        } else if (action !== null) {
          this.selectActiveSuggestion(event);
        }
        return false;
      });
    }
  }

  async getSuggestions(query: string): Promise<ModalResult[]> {
    if (this.localProjectionRefresh) {
      this.localProjectionRefresh = false;
      const localResults = this.projectedResults(query);
      if (localResults) return localResults;
    }

    this.invalidateProjection();
    const epoch = ++this.requestEpoch;
    this.beginQueryStatus(epoch, query.trim().length > 0
      ? { phase: "searching" }
      : { phase: "prompt" });
    const filters = this.backend.identity.profile === "daemon"
      && this.plugin.settings.vaultId.trim().length > 0
      ? { vault_id: this.plugin.settings.vaultId.trim() }
      : undefined;
    const sourceLimit = this.plugin.settings.resultLimit;
    const hitLimit = groupedSearchHitLimit(sourceLimit);
    return this.plugin.captureDiagnostic("info", "search.lifecycle", {
      profile: this.backend.identity.profile,
      mode: this.mode,
      limit: hitLimit,
      operation: "search",
      subsystem: "search_session",
    }, async (event) => {
      const outcome = await this.session.search(query, {
        limit: hitLimit,
        filters,
      });
      switch (outcome.kind) {
        case "results": {
          const grouped = groupSearchExecution(outcome.execution, sourceLimit);
          const resultCount = grouped.facts.returnedSectionCount;
          if (!this.completeQueryStatus(epoch, {
            phase: "settled",
            resultCount,
            displayedSourceCount: grouped.facts.displayedSourceCount,
            omittedObservedSourceCount: grouped.facts.omittedObservedSourceCount,
            candidateWindow: grouped.facts.candidateWindow,
          })) {
            event.set({ outcome: "superseded" });
            return [];
          }
          this.lastErrorCode = null;
          // An in-plugin search names the exact active generation that served
          // it. The serialized Worker can publish and search that generation
          // before this modal's 400 ms status poll observes it, so the current
          // execution is authoritative at this seam. Daemon executions still
          // carry a cached status generation and must not advance the modal.
          if (this.backend.identity.profile === "in_plugin") {
            this.latestStatusGeneration = outcome.execution.generation;
          }
          this.settledProjection = {
            query,
            mode: this.mode,
            backendInstanceId: outcome.execution.backend.instanceId,
            generation: outcome.execution.generation,
            grouped,
          };
          this.resultView = { kind: "sources" };
          event.set({ outcome: "succeeded", resultCount });
          return this.sourceResults(grouped);
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
          // status, clear its delayed activity indicator, or erase a newer
          // settled local projection when SuggestModal resolves out of order.
          return this.projectedResults(this.inputEl.value) ?? [];
      }
    });
  }

  renderSuggestion(result: ModalResult, el: HTMLElement): void {
    const hit = this.hitForResult(result);
    el.addClass("kwiry-result");
    el.addClass(result.kind === "source" ? "kwiry-source-result" : "kwiry-section-result");
    const heading = el.createDiv({ cls: "kwiry-result-heading" });
    const title = heading.createDiv({ cls: "kwiry-result-title" });
    if (result.kind === "source") {
      title.setText(hit.frontmatter.title ?? basename(hit.path));
    } else {
      title.setText(sectionResultTitle(hit, result.returnedSectionIndex));
    }
    const formatChip = FORMAT_CHIP_PRESENTATIONS[hit.format];
    heading.createSpan({ cls: "kwiry-result-format", text: formatChip.label }).setAttribute(
      "aria-label",
      formatChip.accessibleLabel,
    );
    const meta = el.createDiv({ cls: "kwiry-result-meta" });
    const breadcrumb = hit.heading_path.length > 0 ? ` › ${hit.heading_path.join(" › ")}` : "";
    meta.setText(`${hit.path}${breadcrumb}`);
    const context = el.createDiv({ cls: "kwiry-result-context" });
    if (result.kind === "source") {
      context.setText(returnedSectionCountText(result.group.observedSectionCount));
    } else {
      const sourceTitle = result.group.representative.frontmatter.title
        ?? basename(result.group.source.path);
      context.setText(
        `${sourceTitle} · ${returnedSectionOrdinalText(
          result.returnedSectionIndex,
          result.group.observedSectionCount,
        )}`,
      );
    }
    const excerpt = el.createDiv({ cls: "kwiry-result-excerpt" });
    for (const segment of hit.excerpt) {
      const text = segment.text.replace(/\s+/g, " ");
      if (segment.highlighted) excerpt.createEl("mark", { text });
      else excerpt.appendText(text);
    }
  }

  selectSuggestion(result: ModalResult, event: MouseEvent | KeyboardEvent): void {
    const action = this.shortcutAction(event);
    if (action === "drill-source") {
      this.drillIntoSource(result);
      return;
    }
    // SuggestModal closes before onChooseSuggestion. Background open is the one
    // action that must bypass that path while still using the active suggestion.
    if (action === "open-background") {
      this.openResult(result, "tab");
      return;
    }
    this.selectionTransactionActive = true;
    try {
      super.selectSuggestion(result, event);
    } finally {
      this.selectionTransactionActive = false;
      this.invalidateProjection();
    }
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

  private drillIntoSource(result: ModalResult): void {
    if (result.kind !== "source") return;
    const projection = this.settledProjection;
    if (!projection) return;
    const group = findSourceGroup(projection.grouped, result.group.source);
    if (!group) return;
    this.clearRestorationTimer();
    this.resultView = { kind: "sections", source: group.source };
    this.refreshLocalProjection();
  }

  private returnToSources(): void {
    if (this.resultView.kind !== "sections" || !this.settledProjection) return;
    const source = this.resultView.source;
    const generation = this.settledProjection.generation;
    const backendInstanceId = this.settledProjection.backendInstanceId;
    this.resultView = { kind: "sources" };
    this.refreshLocalProjection();
    this.restorationTimer = window.setTimeout(() => {
      this.restorationTimer = null;
      const projection = this.settledProjection;
      if (
        !projection
        || this.resultView.kind !== "sources"
        || projection.generation !== generation
        || projection.backendInstanceId !== backendInstanceId
      ) return;
      const sourceIndex = projection.grouped.groups.findIndex((group) =>
        sameSource(group.source, source));
      this.inputEl.focus();
      for (let index = 0; index < sourceIndex; index += 1) {
        this.moveActiveSuggestion("move-down");
      }
    }, 0);
  }

  private refreshLocalProjection(): void {
    this.localProjectionRefresh = true;
    this.inputEl.dispatchEvent(new Event("input", { bubbles: true }));
  }

  private projectedResults(query: string): ModalResult[] | null {
    const projection = this.settledProjection;
    if (
      !projection
      || projection.query !== query
      || projection.mode !== this.mode
      || projection.backendInstanceId !== this.backend.identity.instanceId
      || projection.generation !== this.latestStatusGeneration
    ) {
      return null;
    }
    if (this.resultView.kind === "sources") return this.sourceResults(projection.grouped);
    const group = findSourceGroup(projection.grouped, this.resultView.source);
    if (!group) return null;
    return group.sections.map((hit, returnedSectionIndex) => ({
      kind: "section",
      group,
      hit,
      returnedSectionIndex,
    }));
  }

  private sourceResults(grouped: GroupedSearchResult): ModalResult[] {
    return grouped.groups.map((group) => ({ kind: "source", group }));
  }

  private isCurrentProjectedResult(result: ModalResult): boolean {
    const currentResults = this.projectedResults(this.inputEl.value);
    return currentResults?.some((current) => current.kind === result.kind
      && current.group === result.group
      && (current.kind === "source" || (
        result.kind === "section"
        && current.hit === result.hit
        && current.returnedSectionIndex === result.returnedSectionIndex
      ))) ?? false;
  }

  private hitForResult(result: ModalResult): BackendSearchHit {
    return result.kind === "source" ? result.group.representative : result.hit;
  }

  private invalidateProjection(): void {
    this.clearRestorationTimer();
    this.settledProjection = null;
    this.resultView = { kind: "sources" };
    this.localProjectionRefresh = false;
  }

  private clearRestorationTimer(): void {
    if (this.restorationTimer === null) return;
    window.clearTimeout(this.restorationTimer);
    this.restorationTimer = null;
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
    }).then(() => {
      const shortfall = pageNavigationShortfall(validated.target, openedViewType(leaf));
      if (shortfall) new Notice(`Kwiry: ${shortfall}`);
    }).catch((error: unknown) => {
      this.plugin.recordCaughtFailure("ui", "open", error, {
        profile: this.backend.identity.profile,
      });
      new Notice("Kwiry: Obsidian could not open this file.");
    });
  }

  private insertResultLink(result: ModalResult, kind: LinkInsertionKind): void {
    const hit = this.hitForResult(result);
    const validated = this.validatedResult(result);
    if (!validated) return;
    // Note links reach any file in the vault, so only section links depend on
    // the format, and the backend registry decides which formats have headings
    // a link subpath can reach.
    if (kind === "section" && !supportsSectionLinks(hit.format)) {
      new Notice("Kwiry: this result's format has no headings a link can point to.");
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
      deepestMatchedHeading(hit.heading_path),
      kind,
    );
    if (!outcome.ok) new Notice(`Kwiry: ${outcome.safeMessage}`);
  }

  private validatedResult(result: ModalResult): { file: TFile; target: OpenTarget } | null {
    const hit = this.hitForResult(result);
    if (!this.isCurrentProjectedResult(result)) {
      new Notice("Kwiry: these search results are out of date. Wait for the refreshed results.");
      return null;
    }
    const activeBackend = this.plugin.getActiveBackendIdentity();
    if (!activeBackend) {
      new Notice("Kwiry: the search backend is no longer active.");
      return null;
    }
    const decision = validateOpenResult(
      hit,
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
    const target: OpenTarget = { path: decision.path };
    if (decision.subpath !== undefined) target.subpath = decision.subpath;
    if (decision.page !== undefined) target.page = decision.page;
    return { file, target };
  }

  onClose(): void {
    this.activeRequestEpoch = ++this.requestEpoch;
    this.progressEpoch += 1;
    if (this.selectionTransactionActive) {
      this.clearRestorationTimer();
      this.localProjectionRefresh = false;
    } else {
      this.invalidateProjection();
    }
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
    const becameSearchable = !this.lastSearchable && status.searchable;
    this.lastSearchable = status.searchable;
    this.reconcileStatusGeneration(status);
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
    if (becameSearchable) this.rerunRetainedQuery(status);
  }

  private reconcileStatusGeneration(status: BackendStatus): void {
    if (status.identity.instanceId !== this.backend.identity.instanceId) return;
    const previousGeneration = this.latestStatusGeneration;
    this.latestStatusGeneration = status.generation;
    if (status.generation === previousGeneration) return;

    const projection = this.settledProjection;
    if (
      !projection
      || projection.backendInstanceId !== status.identity.instanceId
      || projection.generation === status.generation
    ) {
      return;
    }

    this.invalidateProjection();
    this.resultContainerEl.empty();
    if (status.searchable && this.inputEl.value.trim().length > 0) {
      this.inputEl.dispatchEvent(new Event("input", { bubbles: true }));
    }
  }

  private rerunRetainedQuery(status: BackendStatus): void {
    if (status.identity.instanceId !== this.backend.identity.instanceId
      || this.lastErrorCode !== "index_building"
      || this.inputEl.value.trim().length === 0) {
      return;
    }
    // The transition can be observed by more than one status consumer, but the
    // retained query must be submitted only once for this completed build.
    this.lastErrorCode = null;
    this.inputEl.dispatchEvent(new Event("input", { bubbles: true }));
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
    this.invalidateProjection();
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

function returnedSectionCountText(count: number): string {
  return `${count} returned ${count === 1 ? "section" : "sections"}`;
}

function returnedSectionOrdinalText(index: number, count: number): string {
  return `returned section ${index + 1} of ${count}`;
}

function findSourceGroup(
  grouped: GroupedSearchResult,
  source: QualifiedSourceIdentity,
): SourceSearchGroup | undefined {
  return grouped.groups.find((group) => sameSource(group.source, source));
}

function sameSource(
  left: QualifiedSourceIdentity,
  right: QualifiedSourceIdentity,
): boolean {
  return left.profile === right.profile
    && left.backendInstanceId === right.backendInstanceId
    && left.vaultId === right.vaultId
    && left.path === right.path;
}
