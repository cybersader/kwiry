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
  type SearchBackend,
  KwiryBackendError,
} from "./backend";
import { DaemonBackend } from "./backends/daemon-backend";
import {
  InPluginLexicalBackend,
  type InPluginBackendStartupObservation,
} from "./backends/in-plugin-lexical-backend";
import { createInPluginCacheOptions } from "./cache/build-cache-options";
import { readDaemonToken } from "./credentials";
import { PluginDiagnostics } from "./diagnostics/plugin-diagnostics";
import { StartupTimeline } from "./diagnostics/startup-timeline";
import { diagnosticCategoryGroup } from "./diagnostics/log";
import type {
  DiagnosticDetails,
  DiagnosticEventBuilder,
  DiagnosticEventCode,
  DiagnosticLevel,
  DiagnosticTextValue,
} from "./diagnostics/log";
import { createPrivateTools, type PrivateTools } from "./internal/private-tools";
import { LatestRequestEpoch } from "./latest-request-epoch";
import { KwirySearchModal } from "./search-modal";
import { formatPolicyFingerprint } from "./source-formats";
import { formatStatus } from "./status-format";
import {
  DEFAULT_SETTINGS,
  loadSettings,
  type DiagnosticsLogLevel,
  type KwiryPluginSettings,
} from "./settings";
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
  private lastDiagnosticStatus = "";
  private readonly statusRefresh = new LatestRequestEpoch();
  private readonly diagnostics = new PluginDiagnostics(DEFAULT_SETTINGS.diagnosticsLogLevel);
  private startupTimeline: StartupTimeline | null = null;
  private privateTools: PrivateTools = createPrivateTools(this, undefined);
  private sourcePolicyHash: string | null = null;

  async onload(): Promise<void> {
    const pluginEpoch = ++this.pluginEpoch;
    this.startupTimeline = this.createStartupTimeline(pluginEpoch);
    await this.diagnostics.capture("info", "plugin.load", { pluginEpoch }, async (event) => {
      try {
        const storedData = await this.loadData();
        this.settings = loadSettings(storedData);
        this.sourcePolicyHash = await formatPolicyFingerprint({
          ...this.settings.enabledSourceFormats,
        });
        this.privateTools = createPrivateTools(this, storedData);
        this.diagnostics.setLevel(this.settings.diagnosticsLogLevel);
        this.startupTimeline?.setProfile(this.settings.backendProfile);
        event.set({ profile: this.settings.backendProfile });
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
          }),
          in_plugin: (instanceId) => {
            const cache = createInPluginCacheOptions(
              this.app.vault,
              this.requireSourcePolicyHash(),
            );
            const activationEpoch = this.activationEpoch;
            return new InPluginLexicalBackend({
              instanceId,
              activeVaultId: ACTIVE_VAULT_ID,
              source: new ObsidianActiveVaultSource(
                this.app.vault,
                { ...this.settings.enabledSourceFormats },
              ),
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
            });
          },
        }, this.diagnostics);

        this.addSettingTab(new KwirySettingTab(this.app, this));
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
        this.statusBar.setText("kwiry: starting…");
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
          this.startupTimeline?.markLayoutReady();
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
    this.startupTimeline?.finish("cancelled", "plugin_unloaded");
    this.pluginEpoch += 1;
    this.activationEpoch += 1;
    this.statusRefresh.invalidate();
    this.statusUnsubscribe?.();
    this.statusUnsubscribe = null;
    this.activeBackendIdentity = null;
    this.lastDiagnosticStatus = "";
    this.privateTools.dispose();
    const disposal = this.backendManager?.dispose();
    this.diagnostics.setLevel("off");
    this.diagnostics.clear();
    void disposal?.catch(() => undefined);
  }

  async saveSettings(): Promise<void> {
    try {
      await this.saveData(this.privateTools.prepareStoredData(this.settings));
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

  async copyDiagnostics(): Promise<void> {
    try {
      const categories = diagnosticCategoryGroup(this.settings.diagnosticsReportScope);
      const report = this.diagnostics.format({
        pluginVersion: this.manifest.version,
        obsidianVersion: apiVersion,
        platform: diagnosticPlatform(),
        backendProfile: this.settings.backendProfile,
      }, {
        minimumLevel: this.settings.diagnosticsReportLevel,
        ...(categories === undefined ? {} : { categories }),
        includeStructuredRecords: this.settings.diagnosticsReportDetail === "full",
      });
      await navigator.clipboard.writeText(report);
      new Notice(`Kwiry: diagnostics copied (${Math.ceil(report.length / 1024)} KB).`);
    } catch (error) {
      this.recordCaughtFailure("ui", "copy", error);
      new Notice("Kwiry: diagnostics could not be copied.");
    }
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
    void this.diagnostics.capture("error", "failure.caught", {
      subsystem,
      operation,
      outcome,
      ...details,
      ...diagnosticErrorDetails(error),
    }, () => undefined);
  }

  async activateBackendProfile(): Promise<void> {
    const pluginEpoch = this.pluginEpoch;
    const activationEpoch = ++this.activationEpoch;
    const profile = this.settings.backendProfile;
    this.startupTimeline?.beginActivation(profile, activationEpoch);
    this.statusRefresh.invalidate();
    this.statusUnsubscribe?.();
    this.statusUnsubscribe = null;
    this.activeBackendIdentity = null;
    this.statusBar?.setText("kwiry: starting…");

    try {
      if (profile === "in_plugin") {
        const sourcePolicyHash = await formatPolicyFingerprint({
          ...this.settings.enabledSourceFormats,
        });
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
      this.recordBackendStatus(status, pluginEpoch, activationEpoch);
      this.statusBar.setText(formatStatus(status));
    } catch (error) {
      this.recordCaughtFailure("ui", "poll", error, { pluginEpoch, activationEpoch });
      if (this.isCurrent(pluginEpoch, activationEpoch)
        && this.statusRefresh.isCurrent(refreshEpoch)) {
        this.statusBar.setText("kwiry: backend unavailable");
      }
    }
  }

  async rebuildInPluginIndex(): Promise<void> {
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
        event.setLevel("error");
        event.set({ outcome: "failed", ...diagnosticErrorDetails(error) });
        if (this.isCurrent(pluginEpoch, activationEpoch)) {
          new Notice("Kwiry: the in-plugin lexical index could not be rebuilt.");
        }
      }
    });
  }

  private bindBackend(
    backend: SearchBackend,
    pluginEpoch: number,
    activationEpoch: number,
  ): void {
    this.activeBackendIdentity = backend.identity;
    this.lastDiagnosticStatus = "";
    this.statusUnsubscribe?.();
    this.statusUnsubscribe = backend.subscribeStatus?.((status) => {
      if (!this.isCurrent(pluginEpoch, activationEpoch)
        || this.activeBackendIdentity?.instanceId !== backend.identity.instanceId) return;
      this.statusRefresh.invalidate();
      this.recordBackendStatus(status, pluginEpoch, activationEpoch);
      this.statusBar?.setText(formatStatus(status));
    }) ?? null;
  }

  private async openSearch(): Promise<void> {
    const pluginEpoch = this.pluginEpoch;
    const activationEpoch = this.activationEpoch;
    await this.diagnostics.capture("info", "search.lifecycle", {
      profile: this.settings.backendProfile,
      operation: "open",
      pluginEpoch,
      activationEpoch,
    }, async (event) => {
      try {
        const backend = await this.backendManager.current();
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
        event.setLevel("error");
        event.set({ outcome: "failed", ...diagnosticErrorDetails(error) });
        if (this.isCurrent(pluginEpoch, activationEpoch)) {
          new Notice("Kwiry: the selected search backend is unavailable.");
        }
      }
    });
  }

  private recordBackendStatus(
    status: BackendStatus,
    pluginEpoch: number,
    activationEpoch: number,
  ): void {
    const quarantinedSources = status.quarantinedSources ?? 0;
    const unreadableSources = status.unreadableSources ?? 0;
    const quarantineFields = status.quarantineValidatorFields ?? [];
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
      quarantinedSources,
      unreadableSources,
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
    if (unreadableSources > 0) {
      void this.diagnostics.capture("warn", "index.lifecycle", {
        profile: status.identity.profile,
        phase: status.phase,
        outcome: "skipped",
        operation: "status",
        code: "vault_read_failed",
        errorName: "vault_read",
        sourcesFailed: unreadableSources,
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
    case "mode_unavailable":
    case "explicit_query_unsupported":
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
