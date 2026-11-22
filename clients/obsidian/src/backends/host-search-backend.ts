// SPDX-FileCopyrightText: 2026 cybersader
// SPDX-License-Identifier: GPL-3.0-only

import type { BackendLifecycle } from "../backend";
import {
  HOST_SEARCH_LABEL,
  hostSearchFault,
  lookupHostSearch,
  type HostSearchOperations,
} from "../host-search";

export interface HostBackendIdentity {
  readonly profile: "host";
  readonly label: typeof HOST_SEARCH_LABEL;
  readonly instanceId: string;
}

export interface HostBackendStatus {
  readonly kind: "host";
  readonly identity: HostBackendIdentity;
  /** Protocol presence, not a readiness or entitlement check. */
  readonly available: boolean;
}

export class HostSearchBackend implements
  BackendLifecycle<HostBackendIdentity, HostBackendStatus>, HostSearchOperations {
  readonly identity: HostBackendIdentity;
  private operations: HostSearchOperations | null = null;
  private disposed = false;

  constructor(instanceId: string) {
    this.identity = { profile: "host", label: HOST_SEARCH_LABEL, instanceId };
  }

  async initialize(): Promise<void> {
    if (!this.disposed) this.operations = lookupHostSearch();
  }

  async status(): Promise<HostBackendStatus> {
    return { kind: "host", identity: this.identity, available: this.operations !== null };
  }

  async page(query: string, signal: AbortSignal, continuation?: string) {
    return this.requireOperations(signal).page(query, signal, continuation);
  }

  async resolveNote(ref: string, signal: AbortSignal): Promise<string> {
    return this.requireOperations(signal).resolveNote(ref, signal);
  }

  async dispose(): Promise<void> {
    this.disposed = true;
    this.operations = null;
  }

  private requireOperations(signal: AbortSignal): HostSearchOperations {
    if (this.disposed || signal.aborted) throw hostSearchFault.cancelled();
    if (!this.operations) throw hostSearchFault.unavailable();
    return this.operations;
  }
}
