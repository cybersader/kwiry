// SPDX-FileCopyrightText: 2026 cybersader
// SPDX-License-Identifier: GPL-3.0-only

import type { BackendLifecycle } from "../backend";
import {
  HOST_SEARCH_LABEL,
  lookupHostSearch,
  type HostSearchLookup,
  type HostSearchSituation,
} from "../host-search";

export interface HostBackendIdentity {
  readonly profile: "host";
  readonly label: typeof HOST_SEARCH_LABEL;
  readonly instanceId: string;
}

export interface HostBackendStatus {
  readonly kind: "host";
  readonly identity: HostBackendIdentity;
  /** Protocol presence at this look, not a readiness or entitlement check. */
  readonly situation: HostSearchSituation;
}

export interface HostSearchAccess {
  /** One fresh look. A view holds the returned session only while it is on screen. */
  lookup(): HostSearchLookup;
}

/// The host search profile. It holds no service object: each view looks the
/// service up when it opens and releases it when it closes.
export class HostSearchBackend implements
  BackendLifecycle<HostBackendIdentity, HostBackendStatus>, HostSearchAccess {
  readonly identity: HostBackendIdentity;
  private disposed = false;

  constructor(instanceId: string) {
    this.identity = { profile: "host", label: HOST_SEARCH_LABEL, instanceId };
  }

  async initialize(): Promise<void> {
    // Nothing to start: the host does the work.
  }

  async status(): Promise<HostBackendStatus> {
    return { kind: "host", identity: this.identity, situation: this.lookup().situation };
  }

  lookup(): HostSearchLookup {
    if (this.disposed) return { situation: "without_service" };
    return lookupHostSearch();
  }

  async dispose(): Promise<void> {
    this.disposed = true;
  }
}
