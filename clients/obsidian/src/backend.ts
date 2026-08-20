// SPDX-FileCopyrightText: 2026 cybersader
// SPDX-License-Identifier: GPL-3.0-only

import type { SearchMode, SearchRequest, SearchResponse } from "./api";
import type { ExcerptSegment } from "./excerpt";
import type {
  SourceFormatCounts,
  SourcePreparationDefectField,
} from "./worker/protocol";

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
export type BackendIndexActivity = "inventory" | "read" | "prepare" | "apply";
export type BackendIndexStallCategory =
  | "source_read_timeout"
  | "source_read_capacity"
  | "worker_timeout";
export type UnreadableVaultSourceCause =
  | "source_inspect_failed"
  | "source_read_rejected"
  | "source_snapshot_unstable";
export interface UnreadableVaultSourceCauseCount {
  cause: UnreadableVaultSourceCause;
  count: number;
}
export type BackendRebuildResult = "scheduled" | "already_building";

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
  sourceFormatCounts?: SourceFormatCounts;
  quarantinedSources?: number;
  unreadableSources?: number;
  unreadableSourceCauses?: readonly UnreadableVaultSourceCauseCount[];
  quarantineValidatorFields?: readonly SourcePreparationDefectField[];
  dirty: boolean;
  rebuilding: boolean;
  progress?: {
    stage: "snapshot" | "replay" | "rebuild" | "degraded" | "failed";
    activity: BackendIndexActivity;
    subphase?: "planning" | "verifying" | "applying";
    completed: number;
    total: number | null;
    inFlight: number;
    stallCategory?: BackendIndexStallCategory;
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

export type CandidateWindowState =
  | "exhausted"
  | "more_available"
  | "candidate_limit_reached"
  | "unknown";

/**
 * Facts about the bounded candidate collection that produced this execution.
 * `candidateCount` is inspected work, never a corpus/result total. Null counts
 * mean the backend's frozen response does not expose candidate-window evidence.
 */
export interface CandidateWindowFacts {
  state: CandidateWindowState;
  candidateCount: number | null;
  candidateLimit: number | null;
}

export interface SearchExecution {
  backend: BackendIdentity;
  requestedMode: SearchMode;
  effectiveMode: SearchMode;
  generation: string | null;
  candidateWindow: CandidateWindowFacts;
  response: BackendSearchResponse;
}

export interface SearchBackend {
  readonly identity: BackendIdentity;
  initialize(): Promise<void>;
  status(): Promise<BackendStatus>;
  subscribeStatus?(listener: (status: BackendStatus) => void): () => void;
  rebuild?(): Promise<BackendRebuildResult>;
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
    /// Fixed classification of what failed, when the backend knows it. Never
    /// an exception message: those can quote SQL, the query, or vault text.
    public readonly failureCause?: "sqlite" | "plan_rejected" | "bounds_exceeded" | "internal",
  ) {
    super(safeMessage);
    this.name = "KwiryBackendError";
  }
}
