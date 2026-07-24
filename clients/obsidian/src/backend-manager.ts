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

  constructor(private readonly factories: BackendFactories) {}

  activate(profile: BackendProfile): Promise<SearchBackend> {
    const activation = this.activation
      .catch(() => this.activeBackend)
      .then(async (previous) => {
        if (previous) await previous.dispose();
        const instanceId = `${profile}-${this.nextInstance}`;
        this.nextInstance += 1;
        const backend = this.factories[profile](instanceId);
        this.activeBackend = backend;
        await backend.initialize();
        return backend;
      });
    this.activation = activation;
    return activation;
  }

  async current(): Promise<SearchBackend> {
    const backend = await this.activation.catch(() => this.activeBackend);
    if (!backend) {
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

  async dispose(): Promise<void> {
    const backend = await this.activation.catch(() => this.activeBackend);
    this.activeBackend = null;
    if (backend) await backend.dispose();
  }
}
