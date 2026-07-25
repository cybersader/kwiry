// SPDX-FileCopyrightText: 2026 cybersader
// SPDX-License-Identifier: GPL-3.0-only

import {
  type BackendProfile,
  type SearchBackend,
  KwiryBackendError,
} from "./backend";

export interface BackendFactories {
  daemon: (instanceId: string) => SearchBackend;
  in_plugin: (instanceId: string) => SearchBackend;
}

export class BackendManager {
  private activeBackend: SearchBackend | null = null;
  private nextInstance = 1;
  private activation: Promise<SearchBackend | null> = Promise.resolve(null);
  private activationEpoch = 0;
  private closed = false;
  private disposal: Promise<void> | null = null;

  constructor(private readonly factories: BackendFactories) {}

  activate(profile: BackendProfile): Promise<SearchBackend> {
    if (this.closed) return Promise.reject(managerDisposedError());
    const epoch = ++this.activationEpoch;
    const previousNow = this.activeBackend;
    if (this.activeBackend === previousNow) this.activeBackend = null;
    const immediateDisposal = previousNow?.dispose();
    const priorActivation = this.activation;
    const activation = priorActivation
      .catch(() => null)
      .then(async (previous) => {
        if (immediateDisposal) await immediateDisposal;
        if (previous && previous !== previousNow) await previous.dispose();
        if (this.closed || epoch !== this.activationEpoch) throw managerDisposedError();

        const instanceId = `${profile}-${this.nextInstance}`;
        this.nextInstance += 1;
        const backend = this.factories[profile](instanceId);
        this.activeBackend = backend;
        try {
          await backend.initialize();
        } catch (error) {
          if (this.activeBackend === backend) this.activeBackend = null;
          await backend.dispose();
          if (this.closed || epoch !== this.activationEpoch) throw managerDisposedError();
          throw error;
        }
        if (this.closed || epoch !== this.activationEpoch) {
          if (this.activeBackend === backend) this.activeBackend = null;
          await backend.dispose();
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
    this.activationEpoch += 1;
    const active = this.activeBackend;
    this.activeBackend = null;
    const pending = this.activation;
    this.disposal = (async () => {
      if (active) await active.dispose();
      const late = await pending.catch(() => null);
      if (late && late !== active) await late.dispose();
    })();
    return this.disposal;
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
