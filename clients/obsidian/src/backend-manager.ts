// SPDX-FileCopyrightText: 2026 cybersader
// SPDX-License-Identifier: GPL-3.0-only

import {
  type BackendProfile,
  type SearchBackend,
  KwiryBackendError,
} from "./backend";
import type {
  DiagnosticDetails,
  DiagnosticEventBuilder,
  DiagnosticEventCode,
  DiagnosticLevel,
  DiagnosticTextValue,
} from "./diagnostics/log";

export interface BackendFactories {
  daemon: (instanceId: string) => SearchBackend;
  in_plugin: (instanceId: string) => SearchBackend;
}

export interface BackendManagerDiagnostics {
  capture<T>(
    level: DiagnosticLevel,
    code: DiagnosticEventCode,
    details: Readonly<DiagnosticDetails>,
    operation: (event: DiagnosticEventBuilder) => T | Promise<T>,
  ): Promise<T>;
}

const NOOP_DIAGNOSTICS: BackendManagerDiagnostics = {
  capture: async (_level, _code, _details, operation) => operation({
    set: () => undefined,
    increment: () => undefined,
    setLevel: () => undefined,
  }),
};

export class BackendManager {
  private activeBackend: SearchBackend | null = null;
  private nextInstance = 1;
  private activation: Promise<SearchBackend | null> = Promise.resolve(null);
  private activationEpoch = 0;
  private closed = false;
  private disposal: Promise<void> | null = null;

  constructor(
    private readonly factories: BackendFactories,
    private readonly diagnostics: BackendManagerDiagnostics = NOOP_DIAGNOSTICS,
  ) {}

  activate(profile: BackendProfile): Promise<SearchBackend> {
    if (this.closed) return Promise.reject(managerDisposedError());
    const epoch = ++this.activationEpoch;
    const previousNow = this.activeBackend;
    if (this.activeBackend === previousNow) this.activeBackend = null;
    const immediateDisposal = previousNow?.dispose();
    const priorActivation = this.activation;
    const activation = this.diagnostics.capture("info", "backend.activate", {
      profile,
      activationEpoch: epoch,
      subsystem: "backend_manager",
      operation: "activate",
    }, async (event) => {
      const previous = await priorActivation.catch(() => {
        event.increment("warningCount");
        return null;
      });
      if (immediateDisposal) await immediateDisposal;
      if (previous && previous !== previousNow) await previous.dispose();
      if (this.closed || epoch !== this.activationEpoch) {
        event.set({ outcome: "cancelled", code: "disposed" });
        throw managerDisposedError();
      }

      const instanceId = `${profile}-${this.nextInstance}`;
      this.nextInstance += 1;
      const backend = this.factories[profile](instanceId);
      this.activeBackend = backend;
      try {
        await backend.initialize();
      } catch (error) {
        if (this.activeBackend === backend) this.activeBackend = null;
        try {
          await backend.dispose();
        } catch (disposeError) {
          event.increment("warningCount");
          event.set(diagnosticError(disposeError));
          throw disposeError;
        }
        event.set(diagnosticError(error));
        if (this.closed || epoch !== this.activationEpoch) {
          event.set({ outcome: "cancelled", code: "disposed" });
          throw managerDisposedError();
        }
        throw error;
      }
      if (this.closed || epoch !== this.activationEpoch) {
        if (this.activeBackend === backend) this.activeBackend = null;
        await backend.dispose();
        event.set({ outcome: "cancelled", code: "disposed" });
        throw managerDisposedError();
      }
      return backend;
    });
    this.activation = activation;
    return activation;
  }

  async current(): Promise<SearchBackend> {
    if (this.closed) throw managerDisposedError();
    const backend = await this.activation.catch(() => this.activeBackend);
    if (this.closed) throw managerDisposedError();
    if (!backend || backend !== this.activeBackend) {
      throw new KwiryBackendError(
        "backend_not_selected",
        "daemon",
        "lifecycle",
        true,
        "Select a search backend.",
      );
    }
    return backend;
  }

  dispose(): Promise<void> {
    if (this.disposal) return this.disposal;
    this.closed = true;
    const epoch = ++this.activationEpoch;
    const active = this.activeBackend;
    this.activeBackend = null;
    const pending = this.activation;
    this.disposal = this.diagnostics.capture("info", "backend.dispose", {
      activationEpoch: epoch,
      subsystem: "backend_manager",
      operation: "dispose",
    }, async (event) => {
      if (active) await active.dispose();
      const late = await pending.catch(() => {
        event.increment("warningCount");
        return null;
      });
      if (late && late !== active) await late.dispose();
    });
    return this.disposal;
  }
}

function diagnosticError(error: unknown): Readonly<DiagnosticDetails> {
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
    case "disposed":
    case "daemon_unreachable":
    case "mode_unavailable":
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

function managerDisposedError(): KwiryBackendError {
  return new KwiryBackendError(
    "disposed",
    "daemon",
    "lifecycle",
    false,
    "The backend manager is disposed.",
  );
}
