// SPDX-FileCopyrightText: 2026 cybersader
// SPDX-License-Identifier: GPL-3.0-only

import type { SearchMode, SearchRequest, SearchResponse } from "./api";
import type { ExcerptSegment } from "./excerpt";

export type BackendProfile = "daemon" | "in_plugin";
export type BackendPhase =
  | "connecting"
  | "starting"
  | "building"
  | "ready"
  | "degraded"
  | "unavailable"
  | "disposed";
export type BackendLiveness = "unknown" | "alive" | "unreachable" | "terminated";

export interface BackendIdentity {
  profile: BackendProfile;
  instanceId: string;
  label: "Daemon" | "In-plugin";
  boundVaultId: string | null;
}

export interface BackendCapabilities {
  supportedModes: readonly SearchMode[];
  sourceScope: "registered_trees" | "active_vault";
  manualRebuild: boolean;
}

export interface BackendIssue {
  code: string;
  safeMessage: string;
  recoverable: boolean;
}

export interface BackendStatus {
  identity: BackendIdentity;
  phase: BackendPhase;
  liveness: BackendLiveness;
  searchable: boolean;
  generation: string | null;
  capabilities: BackendCapabilities;
  documents: number;
  chunks: number;
  dirty: boolean;
  rebuilding: boolean;
  progress?: {
    stage: "snapshot" | "replay" | "rebuild";
    completed: number;
    total: number | null;
    /// Most recently processed source path, when the backend reports one.
    path?: string;
  };
  issue?: BackendIssue;
}

export interface ResultOrigin {
  profile: BackendProfile;
  backendInstanceId: string;
  vaultId: string;
}

export interface BackendSearchHit extends Omit<SearchResponse["hits"][number], "excerpt"> {
  excerpt: readonly ExcerptSegment[];
  origin: ResultOrigin;
}

export interface BackendSearchResponse {
  hits: BackendSearchHit[];
  next_cursor: string | null;
}

export interface SearchExecution {
  backend: BackendIdentity;
  requestedMode: SearchMode;
  effectiveMode: SearchMode;
  generation: string | null;
  response: BackendSearchResponse;
}

export interface SearchBackend {
  readonly identity: BackendIdentity;
  initialize(): Promise<void>;
  status(): Promise<BackendStatus>;
  subscribeStatus?(listener: (status: BackendStatus) => void): () => void;
  rebuild?(): Promise<void>;
  search(request: SearchRequest): Promise<SearchExecution>;
  dispose(): Promise<void>;
}

export class KwiryBackendError extends Error {
  constructor(
    public readonly code: string,
    public readonly profile: BackendProfile,
    public readonly stage:
      | "configuration"
      | "transport"
      | "protocol"
      | "index"
      | "query"
      | "lifecycle",
    public readonly retryable: boolean,
    public readonly safeMessage: string,
  ) {
    super(safeMessage);
    this.name = "KwiryBackendError";
  }
}
