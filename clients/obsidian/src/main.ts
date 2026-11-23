// SPDX-FileCopyrightText: 2026 cybersader
// SPDX-License-Identifier: GPL-3.0-only
//
// Kwiry Search — one explicit presentation client for daemon and in-plugin
// retrieval backends. Parsing, ranking, indexing, and fallback policy remain
// outside the UI.

import { Notice, Platform, Plugin, apiVersion, requestUrl } from "obsidian";
import workerSource from "virtual:kwiry-worker-source";

import { ACTIVE_VAULT_ID, ObsidianActiveVaultSource } from "./active-vault-source";
import type { Transport } from "./api";
import { BackendManager } from "./backend-manager";
import {
  type BackendIdentity,
  type BackendStatus,
  type ManagedBackend,
  type SearchProfile,
  DAEMON_HOST_UNAVAILABLE,
  KwiryBackendError,
} from "./backend";
import type { HostBackendIdentity, HostBackendStatus } from "./backends/host-search-backend";
import { DaemonBackend } from "./backends/daemon-backend";
import {
  InPluginLexicalBackend,
  type InPluginBackendStartupObservation,
} from "./backends/in-plugin-lexical-backend";
import { createInPluginCacheCustody, type InPluginCacheCustody } from "./cache/build-cache-options";
import type { VaultCacheDeletion } from "./cache/local-cache-store";
import { hasDaemonTokenFileSupport, readDaemonToken } from "./credentials";
import { createProductionDesktopDiagnosticsExportHost } from "./diagnostics/desktop-export-host";
import type { DiagnosticsExportResult } from "./diagnostics/export-contract";
import { createProductionFullReportClipboardHost } from "./diagnostics/full-report-clipboard-host";
import { PluginDiagnostics } from "./diagnostics/plugin-diagnostics";
import { StartupTimeline } from "./diagnostics/startup-timeline";
import {
  collectBoundedDiagnosticSummary,
  diagnosticCategoryGroup,
  serializeDiagnosticExport,
} from "./diagnostics/log";
import type { VaultActivityReport } from "./backends/in-plugin-index-controller";
import type {
  DiagnosticDetails,
  DiagnosticEventBuilder,
  DiagnosticEventCode,
  DiagnosticLevel,
  DiagnosticSourceGeneration,
  DiagnosticTextValue,
} from "./diagnostics/log";
import { createPrivateTools, type PrivateTools } from "./internal/private-tools";
import { LatestRequestEpoch } from "./latest-request-epoch";
import {
  HOST_SEARCH_STARTING,
  hostInvalidationMessage,
  hostSearchSituation,
  hostSearchSituationMessage,
  lookupHostSearch,
} from "./host-search";
import {
  HOST_AVAILABLE_EVENT,
  LocalIndexingWatch,
  localIndexingMessage,
  type LocalIndexingState,
} from "./host-services";
import { HostSearchModal } from "./host-search-modal";
import { createPublicSearchApi, type KwirySearchApiV1 } from "./public-api";
import { KwirySearchModal } from "./search-modal";
import { corePolicyFingerprint, enabledSourceFormatList } from "./source-formats";
import {
  createStatusBarRenderer,
  type StatusBarRenderer,
} from "./status-bar-render";
import { formatStatus } from "./status-format";
import { SOURCE_FORMATS } from "./worker/protocol";
import {
  AUTOMATIC_BACKEND_PROFILE,
  DEFAULT_SETTINGS,
  DERIVED_DATA_DELETED_MESSAGE,
  type DerivedDataDeletionReason,
  loadSettings,
  storedBackendProfileChosen,
  type DiagnosticsLogLevel,
  type KwiryPluginSettings,
} from "./settings";
import { KwirySettingTab } from "./settings-tab";

const STATUS_POLL_MS = 30_000;
const HOVER_LINK_SOURCE = "kwiry-search";

const obsidianTransport: Transport = async ({ url, method, headers, body }) => {
  const response = await requestUrl({ url, method, headers, body, throw: false });
  return { status: response.status, text: response.text, headers: response.headers };
};

const VAULT_ACTIVITY_INTERVAL_MS = 5_000;
const DERIVED_DATA_RECORD_FAILED =
  `Kwiry: ${DERIVED_DATA_DELETED_MESSAGE} The plugin settings could not record this.`;
const DERIVED_DATA_DELETE_FAILED =
  "Kwiry: This workspace's policy does not allow stored indexes, but the local index cache "
  + "could not be deleted. It will be tried again at the next start.";

export default class KwiryPlugin extends Plugin {
  settings: KwiryPluginSettings = DEFAULT_SETTINGS;
  // The daemon uses desktop loopback transport and a local token file, not the vault adapter.
  readonly daemonBackendAvailable = Platform.isDesktopApp
    && typeof requestUrl === "function"
    && hasDaemonTokenFileSupport();
  hoverPreviewSource: string | null = null;
  private readonly backendInvalidationCallbacks = new Set<() => void>();
  private readonly hostSearchModals = new Set<HostSearchModal>();
  private hostActivationFailed = false;
  /** Stops following the host search service; set only while host search is active. */
  private hostStatusFollow: (() => void) | null = null;
  /** True only when a person chose the backend. An automatic choice is
   * recomputed at every start and never saved. */
  private backendProfileChosen = false;
  private localIndexing: LocalIndexingWatch | null = null;
  private hostEvents: AbortController | null = null;
  private layoutReady = false;
  /** The running in-plugin backend keeps a persistent cache. */
  private inPluginCacheActive = false;
  /** Owns every persistent cache store this page opens, and deletes the cache. */
  private cacheCustody: InPluginCacheCustody | null = null;
  /** Deletions run one after another, never two at once. */
  private derivedDataDeletion: Promise<void> = Promise.resolve();
  /** Settles when `onload` has finished, so a deletion never records into half-loaded settings. */
  private loadFinished: Promise<void> = Promise.resolve();
  private settingTab: KwirySettingTab | null = null;
  private savingBackendProfile = false;
  private settingsWrites = Promise.resolve();
  private statusBar: HTMLElement | null = null;
  private statusBarRenderer: StatusBarRenderer | null = null;
  private backendManager!: BackendManager;
  private activeBackendIdentity: BackendIdentity | HostBackendIdentity | null = null;
  private statusUnsubscribe: (() => void) | null = null;
  private pluginEpoch = 0;
  private activationEpoch = 0;
  private lastDiagnosticStatus = "";
  private readonly statusRefresh = new LatestRequestEpoch();
  private readonly diagnostics = new PluginDiagnostics(DEFAULT_SETTINGS.diagnosticsLogLevel);
  private readonly diagnosticsExportHost = createProductionDesktopDiagnosticsExportHost();
  private readonly fullReportClipboardHost = createProductionFullReportClipboardHost();
  private startupTimeline: StartupTimeline | null = null;
  private privateTools: PrivateTools = createPrivateTools(this, undefined);
  private sourcePolicyHash: string | null = null;
  private readonly publicSearch = createPublicSearchApi({
    currentBackend: async () => this.backendManager?.current(),
    hostSearchSelected: () => this.settings.backendProfile === "host",
    defaultMode: () => this.settings.defaultMode,
  });

  get api(): KwirySearchApiV1 {
    return this.publicSearch.api;
  }

  async onload(): Promise<void> {
    const pluginEpoch = ++this.pluginEpoch;
    this.startupTimeline = this.createStartupTimeline(pluginEpoch);
    let finishLoad!: () => void;
    this.loadFinished = new Promise((resolve) => { finishLoad = resolve; });
    try {
      await this.loadPlugin(pluginEpoch);
    } finally {
      finishLoad();
    }
  }

  private async loadPlugin(pluginEpoch: number): Promise<void> {
    await this.diagnostics.capture("info", "plugin.load", { pluginEpoch }, async (event) => {
      try {
        const storedData: unknown = await this.loadData();
        if (pluginEpoch !== this.pluginEpoch) {
          event.set({ outcome: "cancelled" });
          return;
        }
        // Presence and absence are looked at again at every start and never saved;
        // only a backend a person chose is kept.
        this.backendProfileChosen = storedBackendProfileChosen(storedData);
        this.settings = loadSettings(storedData, this.daemonBackendAvailable,
          !this.backendProfileChosen && hostSearchSituation() === "available");
        // Before the first policy look: a stricter first answer deletes through it.
        this.cacheCustody = createInPluginCacheCustody(this.app.vault);
        this.localIndexing = new LocalIndexingWatch((state) => this.onLocalIndexingChanged(state, pluginEpoch));
        this.localIndexing.refresh();
        this.hostEvents?.abort();
        this.hostEvents = new AbortController();
        window.addEventListener(HOST_AVAILABLE_EVENT, () => this.onHostAvailable(pluginEpoch),
          { signal: this.hostEvents.signal });
        this.startupTimeline?.setProfile(this.settings.backendProfile);
        event.set({ profile: this.settings.backendProfile });
        // The core identity is independent of the enabled source formats.
        if (this.settings.backendProfile !== "host") {
          this.sourcePolicyHash = await corePolicyFingerprint();
        }
        this.privateTools = createPrivateTools(this, storedData);
        this.diagnostics.setLevel(this.settings.diagnosticsLogLevel);
        if (pluginEpoch !== this.pluginEpoch) {
          event.set({ outcome: "cancelled" });
          this.startupTimeline?.finish("cancelled", "plugin_unloaded");
          return;
        }
        this.backendManager = new BackendManager({
          daemon: (instanceId) => new DaemonBackend({
            instanceId,
            baseUrl: this.settings.daemonUrl,
            currentVaultId: optionalString(this.settings.daemonCurrentVaultId),
            tokenProvider: () => readDaemonToken(this.settings.tokenFilePath),
            transport: obsidianTransport,
            hostAvailable: this.daemonBackendAvailable,
          }),
          in_plugin: (instanceId) => {
            // Under a host, local indexing may keep nothing beyond the page:
            // the persistent cache is switched off explicitly, not by capability.
            const cache = this.localIndexing?.state.kind === "ungoverned"
              ? this.cacheCustody?.options(this.requireSourcePolicyHash())
              : undefined;
            this.inPluginCacheActive = cache !== undefined;
            const activationEpoch = this.activationEpoch;
            return new InPluginLexicalBackend({
              instanceId,
              activeVaultId: ACTIVE_VAULT_ID,
              source: new ObsidianActiveVaultSource(
                this.app.vault,
                { ...this.settings.enabledSourceFormats },
              ),
              enabledSourceFormats: enabledSourceFormatList(this.settings.enabledSourceFormats),
              workerSource,
              ...(cache === undefined ? {} : { cache }),
              onDiagnosticFailure: (
                { subsystem, reason, errorName, workerCode, workerStage, defectField, nonError },
              ) => {
                void this.diagnostics.capture("error", "failure.caught", {
                  profile: "in_plugin",
                  subsystem,
                  reason,
                  // A Worker protocol code names the fault exactly; the error
                  // class name is only a fallback for a thrown Error.
                  code: workerCode ?? "other",
                  ...(workerStage === undefined ? {} : { stage: workerStage }),
                  errorName: defectField ?? errorName,
                  operation: "build",
                  recoverable: !nonError,
                }, () => undefined);
              },
              onStartupObservation: (observation) => {
                this.observeInPluginStartup(observation, pluginEpoch, activationEpoch);
              },
              onVaultActivity: (activity) => {
                this.observeVaultActivity(activity);
              },
            });
          },
        }, this.diagnostics);

        // Older hosts keep search usable without native Page Preview support.
        if (typeof this.registerHoverLinkSource === "function") {
          this.registerHoverLinkSource(HOVER_LINK_SOURCE, {
            display: this.manifest.name,
            defaultMod: true,
          });
          this.hoverPreviewSource = HOVER_LINK_SOURCE;
        }
        this.settingTab = new KwirySettingTab(this.app, this);
        this.addSettingTab(this.settingTab);
        this.privateTools.register();
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
        this.statusBarRenderer = createStatusBarRenderer(this.statusBar);
        this.setStatusBarText(this.settings.backendProfile === "host"
          ? `Kwiry: ${HOST_SEARCH_STARTING}` : "kwiry: starting…");
        this.registerInterval(
          window.setInterval(() => void this.refreshStatus(), STATUS_POLL_MS),
        );
        // Wait for Obsidian to finish populating its file list before indexing.
        // getFiles() returns only what the vault has cached so far, and on
        // a network-backed vault that enumeration is still in flight during
        // onload: indexing here snapshots a nearly empty vault, reports ready, and
        // finds nothing. onLayoutReady fires once the initial scan is complete.
        // Vault event listeners are still registered inside the backend before its
        // first await, so changes during the wait are not lost.
        this.app.workspace.onLayoutReady(() => {
          if (pluginEpoch !== this.pluginEpoch) return;
          this.startupTimeline?.markLayoutReady();
          this.layoutReady = true;
          void this.activateBackendProfile();
        });
        this.startupTimeline?.markPluginLoadComplete();
      } catch (error) {
        this.startupTimeline?.finish("failed", "plugin_load_failed");
        event.set(diagnosticErrorDetails(error));
        throw error;
      }
    });
  }

  onunload(): void {
    this.hostEvents?.abort();
    this.hostEvents = null;
    this.localIndexing?.dispose();
    this.layoutReady = false;
    this.publicSearch.dispose();
    this.startupTimeline?.finish("cancelled", "plugin_unloaded");
    this.pluginEpoch += 1;
    this.activationEpoch += 1;
    this.statusRefresh.invalidate();
    this.statusUnsubscribe?.();
    this.statusUnsubscribe = null;
    this.setActiveBackendIdentity(null);
    this.backendInvalidationCallbacks.clear();
    this.hoverPreviewSource = null;
    this.lastDiagnosticStatus = "";
    this.privateTools.dispose();
    const disposal = this.backendManager?.dispose();
    this.diagnostics.setLevel("off");
    this.diagnostics.clear();
    void disposal?.catch(() => undefined);
  }

  /** Local indexing answer for this page; `ungoverned` when no host slot exists. */
  get localIndexingState(): LocalIndexingState {
    return this.localIndexing?.state ?? { kind: "ungoverned" };
  }

  saveSettings(): Promise<void> {
    const pluginEpoch = this.pluginEpoch;
    return this.enqueueSettingsWrite(async () => {
      if (pluginEpoch === this.pluginEpoch) await this.persistSettings(this.settings);
    });
  }

  /// Saving daemon connection settings (URL, token file, vault IDs) while the
  /// daemon is selected is a person's deliberate choice of the daemon backend.
  saveDaemonConnectionSettings(): Promise<void> {
    if (this.settings.backendProfile === "daemon") this.backendProfileChosen = true;
    return this.saveSettings();
  }

  async changeBackendProfile(profile: SearchProfile): Promise<void> {
    if (this.savingBackendProfile) return;
    if (profile === this.settings.backendProfile) return;
    const pluginEpoch = this.pluginEpoch;
    this.savingBackendProfile = true;
    this.closeHostSearchModals();
    try {
      try {
        await this.enqueueSettingsWrite(async () => {
          if (pluginEpoch !== this.pluginEpoch) return;
          await this.persistSettings({ ...this.settings, backendProfile: profile }, true);
          if (pluginEpoch === this.pluginEpoch) {
            this.settings.backendProfile = profile;
            this.backendProfileChosen = true;
          }
        });
      } catch {
        if (pluginEpoch === this.pluginEpoch) {
          new Notice("Kwiry: Could not save the search backend selection. The previous backend is still selected.");
        }
        return;
      }
      if (pluginEpoch === this.pluginEpoch) await this.activateBackendProfile();
    } finally {
      this.savingBackendProfile = false;
    }
  }

  private enqueueSettingsWrite(write: () => Promise<void>): Promise<void> {
    const pending = this.settingsWrites.then(write);
    this.settingsWrites = pending.catch(() => undefined);
    return pending;
  }

  private async persistSettings(settings: KwiryPluginSettings, chosen = this.backendProfileChosen): Promise<void> {
    try {
      const stored = this.privateTools.prepareStoredData(settings);
      // An automatic backend is stored as "automatic" so the next start chooses again.
      await this.saveData(chosen || typeof stored !== "object" || stored === null
        ? stored : { ...stored, backendProfile: AUTOMATIC_BACKEND_PROFILE });
    } catch (error) {
      this.recordCaughtFailure("settings", "save", error);
      throw error;
    }
  }

  async onSourcePolicyChanged(): Promise<void> {
    if (this.settings.backendProfile !== "in_plugin") return;
    await this.activateBackendProfile();
  }

  renderPrivateSettings(containerEl: HTMLElement): void {
    this.privateTools.renderSettings(containerEl);
  }

  setDiagnosticsLogLevel(level: DiagnosticsLogLevel): void {
    this.diagnostics.setLevel(level);
  }

  private lastVaultActivityAtMs = 0;
  private lastVaultResurrected = 0;

  /// Vault mutation counts are recorded on a throttle: a vault fighting a sync
  /// layer can emit thousands of events, and one entry each would evict the
  /// rest of the report from the bounded ring. A resurrection is reported
  /// immediately, because that is the observation worth catching.
  private observeVaultActivity(activity: VaultActivityReport): void {
    const now = Date.now();
    const resurrectionSeen = activity.resurrected > this.lastVaultResurrected;
    if (!resurrectionSeen && now - this.lastVaultActivityAtMs < VAULT_ACTIVITY_INTERVAL_MS) {
      return;
    }
    this.lastVaultActivityAtMs = now;
    this.lastVaultResurrected = activity.resurrected;
    void this.captureDiagnostic(
      activity.resurrected > 0 ? "warn" : "info",
      "vault.event",
      {
        profile: "in_plugin",
        operation: "apply",
        subsystem: "vault_source",
        upserts: activity.upserts,
        removals: activity.removals,
        renames: activity.renames,
        rescans: activity.rescans,
        resurrected: activity.resurrected,
      },
      () => undefined,
    );
  }

  async copyDiagnostics(): Promise<void> {
    try {
      const summary = collectBoundedDiagnosticSummary(this.createDiagnosticsExportPlan());
      await navigator.clipboard.writeText(summary.text);
      const omitted = summary.omittedEntries === 0
        ? ""
        : `; ${summary.omittedEntries} retained events omitted`;
      new Notice(
        `Kwiry: diagnostics summary copied (${Math.ceil(summary.byteLength / 1_024)} KiB${omitted}).`,
      );
    } catch (error) {
      this.recordCaughtFailure("ui", "copy", error);
      new Notice("Kwiry: diagnostics summary could not be copied.");
    }
  }

  async exportDiagnosticsFile(): Promise<void> {
    const vaultRoot = desktopVaultRoot(this.app.vault.adapter);
    const desktopVault = vaultRoot !== null && this.diagnosticsExportHost.isAvailable()
      ? vaultRoot
      : null;
    if (desktopVault === null && !this.fullReportClipboardHost.isAvailable()) {
      this.recordDiagnosticsExportFailure("unsupported_platform");
      new Notice("Kwiry: full diagnostics export is unavailable on this device.");
      return;
    }

    const plan = this.createDiagnosticsExportPlan();
    const chunks = serializeDiagnosticExport(plan);
    if (desktopVault !== null) {
      const result = await this.diagnosticsExportHost.save({
        vaultRoot: desktopVault,
        chunks,
      });
      this.presentDiagnosticsExportResult(result, "desktop");
      return;
    }

    // Invoke the clipboard before the first await so the direct settings-button
    // activation remains available to the browser permission boundary.
    const resultPromise = this.fullReportClipboardHost.copy({ chunks });
    const result = await resultPromise;
    this.presentDiagnosticsExportResult(result, "clipboard");
  }

  private presentDiagnosticsExportResult(
    result: DiagnosticsExportResult,
    transport: "desktop" | "clipboard",
  ): void {
    switch (result.kind) {
      case "saved":
        new Notice(transport === "desktop"
          ? "Kwiry: full diagnostics report exported."
          : "Kwiry: full diagnostics report copied. Paste it into a text file outside the active vault, then clear the clipboard.");
        return;
      case "cancelled":
        return;
      case "inside_vault":
        this.recordDiagnosticsExportFailure("root_inside_vault");
        new Notice("Kwiry: choose a diagnostics export location outside the active vault.");
        return;
      case "unsafe_destination":
        this.recordDiagnosticsExportFailure("unsafe_path");
        new Notice("Kwiry: the diagnostics export destination was refused.");
        return;
      case "unavailable":
        this.recordDiagnosticsExportFailure("unsupported_platform");
        new Notice("Kwiry: full diagnostics export is unavailable on this device.");
        return;
      case "write_failed":
        this.recordDiagnosticsExportFailure("write_failed");
        new Notice(transport === "desktop"
          ? "Kwiry: diagnostics could not be exported."
          : "Kwiry: full diagnostics report could not be copied.");
        return;
    }
  }

  private createDiagnosticsExportPlan() {
    const categories = diagnosticCategoryGroup(this.settings.diagnosticsReportScope);
    return this.diagnostics.createExportPlan({
      pluginVersion: this.manifest.version,
      obsidianVersion: apiVersion,
      platform: diagnosticPlatform(),
      backendProfile: this.settings.backendProfile,
    }, {
      minimumLevel: this.settings.diagnosticsReportLevel,
      ...(categories === undefined ? {} : { categories }),
    });
  }

  private recordDiagnosticsExportFailure(
    code: "root_inside_vault" | "unsafe_path" | "unsupported_platform" | "write_failed",
  ): void {
    void this.diagnostics.capture("error", "failure.caught", {
      subsystem: "ui",
      operation: "export",
      outcome: "failed",
      code,
    }, () => undefined);
  }

  clearDiagnostics(confirm = false): void {
    this.diagnostics.clear();
    if (confirm) new Notice("Kwiry: diagnostics cleared.");
  }

  diagnosticErrorDetails(error: unknown): Readonly<DiagnosticDetails> {
    return diagnosticErrorDetails(error);
  }

  captureDiagnostic<T>(
    level: DiagnosticLevel,
    code: DiagnosticEventCode,
    details: Readonly<DiagnosticDetails>,
    operation: (event: DiagnosticEventBuilder) => T | Promise<T>,
  ): Promise<T> {
    return this.diagnostics.capture(level, code, details, operation);
  }

  recordCaughtFailure(
    subsystem: NonNullable<DiagnosticDetails["subsystem"]>,
    operation: NonNullable<DiagnosticDetails["operation"]>,
    error: unknown,
    details: Readonly<DiagnosticDetails> = {},
    outcome: "failed" | "superseded" = "failed",
  ): void {
    void this.diagnostics.capture(outcome === "failed" ? "error" : "info", "failure.caught", {
      subsystem,
      operation,
      outcome,
      ...details,
      ...diagnosticErrorDetails(error),
    }, () => undefined);
  }

  private setStatusBarText(text: string): void {
    this.statusBarRenderer?.render(text);
  }

  /// The provider-arrived event: look again, and switch only if the answer changed.
  private onHostAvailable(pluginEpoch: number): void {
    if (pluginEpoch !== this.pluginEpoch) return;
    const switchToHost = !this.backendProfileChosen && !this.savingBackendProfile
      && this.settings.backendProfile !== "host" && hostSearchSituation() === "available";
    if (switchToHost) {
      // An automatic choice follows presence for this session only; nothing is saved.
      this.settings.backendProfile = "host";
      this.startupTimeline?.setProfile("host");
    }
    // After the profile is settled, so a policy answer cannot restart a backend being replaced.
    this.localIndexing?.refresh();
    if (switchToHost && this.layoutReady) void this.activateBackendProfile();
    else if (this.layoutReady) {
      this.followHostStatus(this.pluginEpoch, this.activationEpoch);
      void this.refreshStatus();
    }
    this.settingTab?.redrawIfShown();
  }

  /// A stricter answer drops what was built; a permitted one starts what waited.
  private onLocalIndexingChanged(state: LocalIndexingState, pluginEpoch: number): void {
    if (pluginEpoch !== this.pluginEpoch) return;
    this.settingTab?.redrawIfShown();
    const teardown = this.applyLocalIndexing(state);
    // Memory-only is the baseline under a host: any index this plugin stored on
    // disk, including one saved before the first answer arrived, is deleted on
    // the first stricter answer and on every later change to one.
    if (state.kind === "memory_only" || state.kind === "refused") {
      this.deleteDerivedDataForPolicy(state.kind === "refused" ? "policy_refuse" : "policy_memory_only",
        teardown, pluginEpoch);
    }
  }

  /** Returns the backend stop or restart this answer started, if any. */
  private applyLocalIndexing(state: LocalIndexingState): Promise<void> | undefined {
    if (!this.layoutReady) return undefined;
    const running = this.activeBackendIdentity?.profile === "in_plugin";
    if (this.settings.backendProfile !== "in_plugin" && !running) return undefined;
    // Stopping is never deferred, not even while a backend switch is saving:
    // it supersedes any activation in flight, and every activation re-reads policy.
    if (state.kind === "refused" || (state.kind === "pending" && this.inPluginCacheActive)) {
      // An unanswered policy cannot permit the persistent cache; a plain re-check
      // (pending with no cache) keeps an in-memory index running.
      return this.stopInPluginForPolicy(state);
    }
    if (state.kind === "memory_only" && !this.savingBackendProfile
      && this.settings.backendProfile === "in_plugin" && (!running || this.inPluginCacheActive)) {
      // Start what waited for the answer, or restart without the persistent cache.
      return this.activateBackendProfile();
    }
    return undefined;
  }

  /// Deletes this vault's stored index cache after the backend that used it has
  /// stopped. The custody then closes every store this page opened and waits
  /// for writes already queued on them, so nothing is deleted under a write.
  private deleteDerivedDataForPolicy(
    reason: DerivedDataDeletionReason,
    teardown: Promise<void> | undefined,
    pluginEpoch: number,
  ): void {
    const custody = this.cacheCustody;
    if (custody === null) return;
    this.derivedDataDeletion = this.derivedDataDeletion.then(async () => {
      await teardown?.catch(() => undefined);
      await this.loadFinished;
      let outcome: VaultCacheDeletion;
      try {
        outcome = await custody.deleteForPolicy();
      } catch {
        outcome = { kind: "failed" };
      }
      this.recordDerivedDataDeletion(outcome);
      // A browser tab or a vault without a disk cache holds nothing: nothing to report.
      if (outcome.kind === "absent" || outcome.kind === "unavailable") return;
      if (outcome.kind !== "deleted") {
        if (pluginEpoch === this.pluginEpoch) new Notice(DERIVED_DATA_DELETE_FAILED);
        return;
      }
      // A fact about this plugin's own data, not a backend choice: recording it
      // does not conflict with the rule that only a person's explicit choices are
      // persisted. `persistSettings` still stores an automatic backend as automatic.
      this.settings.derivedDataDeleted = { at: new Date().toISOString(), reason };
      this.settingTab?.redrawIfShown();
      try {
        await this.saveSettings();
      } catch {
        // The deletion stands; only the record of it could not be saved.
        if (pluginEpoch === this.pluginEpoch) new Notice(DERIVED_DATA_RECORD_FAILED);
      }
    });
  }

  private recordDerivedDataDeletion(outcome: VaultCacheDeletion): void {
    void this.diagnostics.capture(
      outcome.kind === "deleted" || outcome.kind === "absent" || outcome.kind === "unavailable"
        ? "info" : "warn",
      "index.lifecycle",
      {
        profile: "in_plugin",
        subsystem: "cache_store",
        operation: "discard",
        outcome: outcome.kind === "deleted" ? "discarded"
          : outcome.kind === "absent" || outcome.kind === "unavailable" ? "skipped" : "failed",
        ...(outcome.kind === "unavailable" ? { code: outcome.reason }
          : outcome.kind === "locked" ? { code: "locked" }
            : outcome.kind === "refused" ? { code: "unsafe_path" }
              : outcome.kind === "failed" ? { code: "discard_failed" } : {}),
      },
      () => undefined,
    );
  }

  private async stopInPluginForPolicy(state: LocalIndexingState): Promise<void> {
    const pluginEpoch = this.pluginEpoch;
    const activationEpoch = ++this.activationEpoch;
    this.statusRefresh.invalidate();
    this.statusUnsubscribe?.();
    this.statusUnsubscribe = null;
    this.setActiveBackendIdentity(null);
    this.inPluginCacheActive = false;
    this.setStatusBarText(`Kwiry: ${localIndexingMessage(state) ?? ""}`);
    try {
      await this.backendManager.deactivate();
    } catch (error) {
      this.recordCaughtFailure("ui", "activate", error, { profile: "in_plugin", pluginEpoch, activationEpoch });
    }
  }

  async activateBackendProfile(): Promise<void> {
    const pluginEpoch = this.pluginEpoch;
    const activationEpoch = ++this.activationEpoch;
    const profile = this.settings.backendProfile;
    this.startupTimeline?.beginActivation(profile, activationEpoch);
    this.hostActivationFailed = false;
    this.statusRefresh.invalidate();
    this.statusUnsubscribe?.();
    this.statusUnsubscribe = null;
    this.setActiveBackendIdentity(null);
    this.setStatusBarText(this.settings.backendProfile === "host"
      ? `Kwiry: ${HOST_SEARCH_STARTING}` : "kwiry: starting…");

    if (profile === "in_plugin") {
      const policy = this.localIndexingState;
      if (policy.kind === "pending" || policy.kind === "refused") {
        // Nothing local starts until the answer permits it.
        this.inPluginCacheActive = false;
        await this.backendManager.deactivate().catch(() => undefined);
        if (!this.isCurrent(pluginEpoch, activationEpoch)) return;
        if (policy.kind === "refused") this.startupTimeline?.finish("failed", "backend_unavailable");
        this.setStatusBarText(`Kwiry: ${localIndexingMessage(policy) ?? ""}`);
        return;
      }
    }

    try {
      if (profile === "in_plugin") {
        const sourcePolicyHash = await corePolicyFingerprint();
        if (!this.isCurrent(pluginEpoch, activationEpoch)) return;
        this.sourcePolicyHash = sourcePolicyHash;
      }
      const backend = await this.backendManager.activate(profile);
      if (!this.isCurrent(pluginEpoch, activationEpoch)) return;
      this.bindBackend(backend, pluginEpoch, activationEpoch);
      await this.refreshStatus(pluginEpoch, activationEpoch);
    } catch (error) {
      this.recordCaughtFailure(
        "ui",
        "activate",
        error,
        { profile, pluginEpoch, activationEpoch },
        this.isCurrent(pluginEpoch, activationEpoch) ? "failed" : "superseded",
      );
      if (!this.isCurrent(pluginEpoch, activationEpoch)) return;
      if (profile === "host") {
        this.hostActivationFailed = true;
        this.startupTimeline?.finish("failed", "activation_failed");
        this.setStatusBarText(`Kwiry: ${hostSearchSituationMessage(hostSearchSituation())}`);
        return;
      }
      try {
        const backend = await this.backendManager.current();
        if (!this.isCurrent(pluginEpoch, activationEpoch)) return;
        this.bindBackend(backend, pluginEpoch, activationEpoch);
        await this.refreshStatus(pluginEpoch, activationEpoch);
      } catch (fallbackError) {
        this.recordCaughtFailure("ui", "activate", fallbackError, {
          profile: this.settings.backendProfile,
          pluginEpoch,
          activationEpoch,
        });
        if (this.isCurrent(pluginEpoch, activationEpoch)) {
          this.startupTimeline?.finish("failed", "activation_failed");
          this.setStatusBarText("kwiry: backend unavailable");
        }
      }
    }
  }

  getActiveBackendIdentity(): BackendIdentity | null {
    return this.activeBackendIdentity?.profile === "host" ? null : this.activeBackendIdentity;
  }

  onBackendInvalidated(callback: () => void): () => void {
    this.backendInvalidationCallbacks.add(callback);
    return () => { this.backendInvalidationCallbacks.delete(callback); };
  }

  private setActiveBackendIdentity(identity: BackendIdentity | HostBackendIdentity | null): void {
    const changed = this.activeBackendIdentity?.instanceId !== identity?.instanceId;
    this.activeBackendIdentity = identity;
    if (changed) {
      this.releaseHostStatus();
      this.closeHostSearchModals();
      for (const callback of this.backendInvalidationCallbacks) callback();
    }
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
      this.recordBackendStatus(status, pluginEpoch, activationEpoch);
      this.setStatusBarText(formatStatus(status));
    } catch (error) {
      // The status interval starts at load, but the backend is bound only after
      // layout-ready activation, which on a large vault can come well after the
      // first tick. Until then "no backend selected" is the expected state, the
      // status bar already says "starting", and logging it as an internal error
      // only adds a false fault to field reports.
      if (this.activeBackendIdentity === null
        && error instanceof KwiryBackendError
        && error.code === "backend_not_selected") return;
      this.recordCaughtFailure("ui", "poll", error, { pluginEpoch, activationEpoch });
      if (this.isCurrent(pluginEpoch, activationEpoch)
        && this.statusRefresh.isCurrent(refreshEpoch)) {
        this.setStatusBarText("kwiry: backend unavailable");
      }
    }
  }

  async rebuildInPluginIndex(): Promise<void> {
    if (this.settings.backendProfile === "host") {
      new Notice("Kwiry: manual rebuild is available only for In-plugin · Lexical.");
      return;
    }
    if (this.policyBlocksInPlugin()) return;
    const pluginEpoch = this.pluginEpoch;
    const activationEpoch = this.activationEpoch;
    await this.diagnostics.capture("info", "index.lifecycle", {
      profile: this.settings.backendProfile,
      stage: "rebuild",
      operation: "rebuild",
      pluginEpoch,
      activationEpoch,
    }, async (event) => {
      try {
        const backend = await this.backendManager.current();
        if (!("search" in backend)) return;
        const status = await backend.status();
        if (!this.isCurrent(pluginEpoch, activationEpoch)
          || this.activeBackendIdentity?.instanceId !== backend.identity.instanceId) {
          event.set({ outcome: "cancelled" });
          return;
        }
        if (!status.capabilities.manualRebuild || !backend.rebuild) {
          event.set({ outcome: "skipped" });
          new Notice("Kwiry: manual rebuild is available only for In-plugin · Lexical.");
          return;
        }
        const result = await backend.rebuild();
        event.set({ outcome: result });
        if (this.isCurrent(pluginEpoch, activationEpoch)
          && this.activeBackendIdentity?.instanceId === backend.identity.instanceId) {
          new Notice(result === "already_building"
            ? "Kwiry: the in-plugin lexical index is already building."
            : "Kwiry: in-plugin lexical rebuild started.");
        }
      } catch (error) {
        event.complete("error", { outcome: "failed", ...diagnosticErrorDetails(error) });
        if (this.isCurrent(pluginEpoch, activationEpoch)) {
          new Notice("Kwiry: the in-plugin lexical index could not be rebuilt.");
        }
      }
    });
  }

  private bindBackend(
    backend: ManagedBackend,
    pluginEpoch: number,
    activationEpoch: number,
  ): void {
    this.setActiveBackendIdentity(backend.identity);
    this.followHostStatus(pluginEpoch, activationEpoch);
    this.lastDiagnosticStatus = "";
    this.statusUnsubscribe?.();
    this.statusUnsubscribe = "search" in backend ? backend.subscribeStatus?.((status) => {
      if (!this.isCurrent(pluginEpoch, activationEpoch)
        || this.activeBackendIdentity?.instanceId !== backend.identity.instanceId) return;
      this.statusRefresh.invalidate();
      this.recordBackendStatus(status, pluginEpoch, activationEpoch);
      this.setStatusBarText(formatStatus(status));
    }) ?? null : null;
  }

  /// While host search is the active backend, listen for the service's
  /// invalidations so the status bar never contradicts the search dialog: each
  /// one refreshes the status at once instead of at the next poll. Like the
  /// dialog, a withdrawn or unknown reason releases the service; the
  /// provider-arrived event follows it again.
  private followHostStatus(pluginEpoch: number, activationEpoch: number): void {
    if (this.hostStatusFollow !== null || !this.isCurrent(pluginEpoch, activationEpoch)
      || this.activeBackendIdentity?.profile !== "host") return;
    const found = lookupHostSearch();
    if (found.situation !== "available") return;
    let live = true;
    let stop: (() => void) | null = null;
    const release = () => {
      if (!live) return;
      live = false;
      if (this.hostStatusFollow === release) this.hostStatusFollow = null;
      stop?.();
    };
    stop = found.session.onInvalidated((reason) => {
      if (!live) return;
      if (hostInvalidationMessage(reason).detach) release();
      void this.refreshStatus(pluginEpoch, activationEpoch);
    });
    // An invalidation delivered during registration has already released it.
    if (live) this.hostStatusFollow = release;
    else stop();
  }

  private releaseHostStatus(): void {
    const release = this.hostStatusFollow;
    this.hostStatusFollow = null;
    release?.();
  }

  private async openSearch(): Promise<void> {
    if (this.savingBackendProfile) return;
    if (this.policyBlocksInPlugin()) return;
    const pluginEpoch = this.pluginEpoch;
    const activationEpoch = this.activationEpoch;
    if (this.settings.backendProfile === "host") {
      await this.openHostSearch(pluginEpoch, activationEpoch);
      return;
    }
    await this.diagnostics.capture("info", "search.lifecycle", {
      profile: this.settings.backendProfile,
      operation: "open",
      pluginEpoch,
      activationEpoch,
    }, async (event) => {
      try {
        const backend = await this.backendManager.current();
        if (!("search" in backend)) return;
        const status = await backend.status();
        if (!this.isCurrent(pluginEpoch, activationEpoch)
          || this.activeBackendIdentity?.instanceId !== backend.identity.instanceId) {
          event.set({ outcome: "cancelled" });
          return;
        }
        new KwirySearchModal(this, backend, status).open();
        event.set({
          outcome: "started",
          mode: backend.identity.profile === "daemon" ? this.settings.defaultMode : "lexical",
        });
      } catch (error) {
        event.complete("error", { outcome: "failed", ...diagnosticErrorDetails(error) });
        if (this.isCurrent(pluginEpoch, activationEpoch)) {
          new Notice("Kwiry: the selected search backend is unavailable.");
        }
      }
    });
  }

  /// Shows the policy wording when local indexing may not run for the selected in-plugin backend.
  private policyBlocksInPlugin(): boolean {
    if (this.settings.backendProfile !== "in_plugin") return false;
    const policy = this.localIndexingState;
    if (policy.kind !== "pending" && policy.kind !== "refused") return false;
    new Notice(`Kwiry: ${localIndexingMessage(policy) ?? ""}`);
    return true;
  }

  private closeHostSearchModals(): void {
    for (const modal of this.hostSearchModals) modal.close();
  }

  private async openHostSearch(pluginEpoch: number, activationEpoch: number): Promise<void> {
    if (this.activeBackendIdentity?.profile !== "host") {
      new Notice(`Kwiry: ${this.hostActivationFailed
        ? hostSearchSituationMessage(hostSearchSituation()) : HOST_SEARCH_STARTING}`);
      return;
    }
    try {
      const backend = await this.backendManager.current();
      if (!("lookup" in backend)) return;
      if (!this.isCurrent(pluginEpoch, activationEpoch) || this.savingBackendProfile
        || this.activeBackendIdentity?.instanceId !== backend.identity.instanceId) return;
      // Each modal looks the service up and listens for invalidation itself.
      const modal = new HostSearchModal(this.app, backend,
        () => { this.hostSearchModals.delete(modal); });
      this.hostSearchModals.add(modal);
      try {
        modal.open();
      } catch (error) {
        this.hostSearchModals.delete(modal);
        throw error;
      }
    } catch {
      if (this.isCurrent(pluginEpoch, activationEpoch)) {
        new Notice(`Kwiry: ${hostSearchSituationMessage(hostSearchSituation())}`);
      }
    }
  }

  private recordBackendStatus(
    status: BackendStatus | HostBackendStatus,
    pluginEpoch: number,
    activationEpoch: number,
  ): void {
    if ("kind" in status) {
      if (status.situation === "available") this.startupTimeline?.finish("succeeded", "host_search_supported");
      else this.startupTimeline?.finish("failed", "backend_unavailable");
      return;
    }
    const quarantinedSources = status.quarantinedSources ?? 0;
    const unreadableSources = status.unreadableSources ?? 0;
    const unreadableSourceCauses = status.unreadableSourceCauses ?? [];
    const quarantineFields = status.quarantineValidatorFields ?? [];
    const sourceGeneration = diagnosticBackendSourceGeneration(status);
    const progress = status.progress;
    if (isMeaningfulBackendProgress(progress) && this.isCurrent(pluginEpoch, activationEpoch)) {
      this.startupTimeline?.markFirstProgress();
    }
    if (status.identity.profile === "daemon" && this.isCurrent(pluginEpoch, activationEpoch)) {
      if (status.phase === "ready" && status.searchable && !status.dirty
        && quarantinedSources === 0 && unreadableSources === 0) {
        this.startupTimeline?.markFullyCurrent();
      } else if (status.phase === "unavailable") {
        this.startupTimeline?.finish("failed", "backend_unavailable");
      }
    }
    const omissionCode = quarantinedSources > 0
      ? diagnosticErrorCode("sources_quarantined")
      : unreadableSources > 0
        ? diagnosticErrorCode("sources_unreadable")
        : null;
    const issueCode = status.issue ? diagnosticErrorCode(status.issue.code) : omissionCode;
    const signature = [
      status.identity.instanceId,
      status.phase,
      status.liveness,
      status.searchable,
      status.dirty,
      status.rebuilding,
      status.zeroChunkSources ?? "unavailable",
      status.enabledSourceFormats?.join(",") ?? "unknown",
      diagnosticSourceFormatSignature(status),
      quarantinedSources,
      unreadableSources,
      unreadableSourceCauses.map(({ cause, count }) => `${cause}=${count}`).join(","),
      quarantineFields.join(","),
      issueCode,
      progress?.stage ?? "none",
      progress?.activity ?? "none",
      progress?.stallCategory ?? "none",
      diagnosticProgressMilestone(progress),
    ].join(":");
    if (signature === this.lastDiagnosticStatus) return;
    this.lastDiagnosticStatus = signature;
    const hasOmissions = quarantinedSources > 0 || unreadableSources > 0;
    void this.diagnostics.capture(status.issue || hasOmissions ? "warn" : "info", "index.lifecycle", {
      profile: status.identity.profile,
      phase: status.phase,
      liveness: status.liveness,
      outcome: status.phase === "disposed" ? "cancelled" : "succeeded",
      operation: "status",
      pluginEpoch,
      activationEpoch,
      ...(issueCode === null ? {} : { code: issueCode }),
      searchable: status.searchable,
      dirty: status.dirty,
      rebuilding: status.rebuilding,
      documents: status.documents,
      chunks: status.chunks,
      sourceGeneration,
      sourcesSkipped: quarantinedSources,
      sourcesFailed: unreadableSources,
      ...(progress === undefined
        ? {}
        : {
            stage: progress.stage,
            activity: progress.activity,
            completed: progress.completed,
            total: progress.total,
            inFlight: progress.inFlight,
            ...(progress.stallCategory === undefined
              ? {}
              : { stallCategory: progress.stallCategory }),
          }),
      ...(status.issue ? { recoverable: status.issue.recoverable } : {}),
    }, () => undefined);
    for (const field of quarantineFields) {
      void this.diagnostics.capture("warn", "index.lifecycle", {
        profile: status.identity.profile,
        phase: status.phase,
        outcome: "skipped",
        operation: "status",
        code: "source_rejected",
        errorName: field,
        sourcesSkipped: quarantinedSources,
      }, () => undefined);
    }
    for (const { cause, count } of unreadableSourceCauses) {
      void this.diagnostics.capture("warn", "index.lifecycle", {
        profile: status.identity.profile,
        phase: status.phase,
        outcome: "skipped",
        operation: "status",
        code: "vault_read_failed",
        failureCause: cause,
        sourcesFailed: count,
      }, () => undefined);
    }
  }

  private createStartupTimeline(pluginEpoch: number): StartupTimeline | null {
    try {
      return new StartupTimeline({
        profile: this.settings.backendProfile,
        pluginEpoch,
        activationEpoch: this.activationEpoch,
        record: (record) => this.diagnostics.recordStartup(record),
      });
    } catch {
      return null;
    }
  }

  private observeInPluginStartup(
    observation: InPluginBackendStartupObservation,
    pluginEpoch: number,
    activationEpoch: number,
  ): void {
    if (!this.isCurrent(pluginEpoch, activationEpoch)) return;
    if (observation.kind === "first_progress") {
      this.startupTimeline?.markFirstProgress();
      return;
    }
    if (observation.kind === "cache_searchable") {
      this.startupTimeline?.markCacheSearchable(observation.cacheBytes);
      return;
    }
    if (observation.kind === "fully_current") {
      this.startupTimeline?.markFullyCurrent();
      return;
    }
    this.startupTimeline?.finish(observation.outcome, observation.reason);
  }

  private requireSourcePolicyHash(): string {
    if (this.sourcePolicyHash === null) throw new Error("source policy identity is unavailable");
    return this.sourcePolicyHash;
  }

  private isCurrent(pluginEpoch: number, activationEpoch: number): boolean {
    return pluginEpoch === this.pluginEpoch && activationEpoch === this.activationEpoch;
  }
}

function isMeaningfulBackendProgress(progress: BackendStatus["progress"]): boolean {
  if (!progress) return false;
  if (progress.activity === "inventory") return progress.total !== null;
  if (progress.activity === "read") return progress.inFlight > 0;
  return true;
}

function diagnosticProgressMilestone(progress: BackendStatus["progress"]): string {
  if (progress === undefined) return "none";
  if (progress.total === null) return "unknown";
  if (progress.total === 0) return "empty";
  if (progress.total <= 20) return `${progress.completed}`;
  return `${Math.min(20, Math.floor((progress.completed * 20) / progress.total))}`;
}

function optionalString(value: string): string | null {
  const trimmed = value.trim();
  return trimmed ? trimmed : null;
}

function desktopVaultRoot(adapter: unknown): string | null {
  if (typeof adapter !== "object" || adapter === null || !("getBasePath" in adapter)) return null;
  const getBasePath = (adapter as { getBasePath?: unknown }).getBasePath;
  if (typeof getBasePath !== "function") return null;
  try {
    const value = getBasePath.call(adapter);
    return typeof value === "string" && value.length > 0 ? value : null;
  } catch {
    return null;
  }
}

function diagnosticBackendSourceGeneration(status: BackendStatus): DiagnosticSourceGeneration {
  if (status.sourceFormatCounts === undefined || status.zeroChunkSources === undefined) {
    return { schemaVersion: 1, availability: "unavailable" };
  }
  const enabled = status.enabledSourceFormats === undefined
    ? null
    : new Set(status.enabledSourceFormats);
  const formats = Object.create(null) as Extract<
    DiagnosticSourceGeneration,
    { availability: "available" }
  >["formats"];
  for (const format of SOURCE_FORMATS) {
    const counts = status.sourceFormatCounts[format];
    formats[format] = {
      policy: enabled === null ? "unknown" : enabled.has(format) ? "enabled" : "disabled",
      indexedComplete: counts["indexed-complete"],
      indexedPartial: counts["indexed-partial"],
      skippedNoExtractableText: counts["skipped-no-extractable-text"],
      unreadable: counts.unreadable,
      quarantined: counts.quarantined,
    };
  }
  return {
    schemaVersion: 1,
    availability: "available",
    documents: status.documents,
    chunks: status.chunks,
    zeroChunkSources: status.zeroChunkSources,
    formats,
  };
}

function diagnosticSourceFormatSignature(status: BackendStatus): string {
  if (status.sourceFormatCounts === undefined) return "unavailable";
  return SOURCE_FORMATS.map((format) => {
    const counts = status.sourceFormatCounts![format];
    return `${format}=${counts["indexed-complete"]},${counts["indexed-partial"]},${counts["skipped-no-extractable-text"]},${counts.unreadable},${counts.quarantined}`;
  }).join(";");
}

function diagnosticPlatform(): "android" | "ios" | "linux" | "macos" | "windows" | "unknown" {
  if (Platform.isAndroidApp) return "android";
  if (Platform.isIosApp) return "ios";
  if (typeof process === "undefined") return "unknown";
  if (process.platform === "linux") return "linux";
  if (process.platform === "darwin") return "macos";
  if (process.platform === "win32") return "windows";
  return "unknown";
}

function diagnosticErrorDetails(error: unknown): Readonly<DiagnosticDetails> {
  if (error instanceof KwiryBackendError) {
    return {
      code: diagnosticErrorCode(error.code),
      stage: error.stage,
      retryable: error.retryable,
      // A catch-all code says only that search failed. The classified cause is
      // what makes the report actionable without asking for logs.
      ...(error.failureCause === undefined ? {} : { failureCause: error.failureCause }),
    };
  }
  const code = typeof error === "object" && error !== null && "code" in error
    ? (error as { code?: unknown }).code
    : undefined;
  return { code: diagnosticErrorCode(code) };
}

function diagnosticErrorCode(code: unknown): DiagnosticTextValue {
  switch (code) {
    case "sources_quarantined":
      return "source_rejected";
    case "sources_unreadable":
      return "vault_read_failed";
    case "disposed":
    case "daemon_unreachable":
    case DAEMON_HOST_UNAVAILABLE.code:
    case "daemon_upgrade_required":
    case "mode_unavailable":
    case "explicit_query_unsupported":
    case "invalid_field_control":
    case "invalid_query":
    case "invalid_query_plan":
    case "query_execution_failed":
    case "worker_failed":
    case "worker_recovering":
    case "vault_read_failed":
    case "index_build_failed":
    case "index_update_failed":
    case "index_limit_exceeded":
    case "index_reconciling":
    case "cache_partially_reused":
    case "index_building":
    case "cache_absent":
    case "cache_unavailable":
    case "cache_corrupt":
    case "cache_incompatible":
    case "cache_restore_unavailable":
    case "cache_discard_failed":
    case "cache_save_failed":
      return code;
    default:
      return "internal_error";
  }
}
