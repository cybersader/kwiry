// SPDX-FileCopyrightText: 2026 cybersader
// SPDX-License-Identifier: GPL-3.0-only

import type {
  BackendProfile,
  BackendSearchHit,
  CandidateWindowFacts,
  SearchExecution,
} from "./backend";

const GROUPED_SEARCH_HIT_LIMIT = 100;

export interface QualifiedSourceIdentity {
  profile: BackendProfile;
  backendInstanceId: string;
  vaultId: string;
  path: string;
}

export interface SourceSearchGroup {
  source: QualifiedSourceIdentity;
  representative: BackendSearchHit;
  sections: readonly BackendSearchHit[];
  firstRawIndex: number;
  observedSectionCount: number;
}

export interface GroupedSearchResult {
  groups: readonly SourceSearchGroup[];
  facts: {
    returnedSectionCount: number;
    observedSourceCount: number;
    displayedSourceCount: number;
    omittedObservedSourceCount: number;
    sourceLimit: number;
    candidateWindow: CandidateWindowFacts;
  };
}

interface MutableSourceSearchGroup {
  source: QualifiedSourceIdentity;
  representative: BackendSearchHit;
  sections: BackendSearchHit[];
  firstRawIndex: number;
  observedSectionCount: number;
}

type PathGroupIndex = Map<string, MutableSourceSearchGroup>;
type VaultGroupIndex = Map<string, PathGroupIndex>;
type BackendGroupIndex = Map<string, VaultGroupIndex>;

/**
 * Returns the existing maximum response depth used to discover source groups.
 * The user-facing source limit remains independent from this ranked hit window.
 */
export function groupedSearchHitLimit(sourceLimit: number): 100 {
  assertSourceLimit(sourceLimit);
  return GROUPED_SEARCH_HIT_LIMIT;
}

/**
 * Projects an already-ranked backend execution into stable source groups.
 * Raw hits are visited once and retained by reference without reranking or mutation.
 */
export function groupSearchExecution(
  execution: SearchExecution,
  sourceLimit: number,
): GroupedSearchResult {
  groupedSearchHitLimit(sourceLimit);

  const observedGroups: MutableSourceSearchGroup[] = [];
  const groupIndex = new Map<BackendProfile, BackendGroupIndex>();

  for (let rawIndex = 0; rawIndex < execution.response.hits.length; rawIndex += 1) {
    const hit = execution.response.hits[rawIndex]!;
    const { profile, backendInstanceId, vaultId } = hit.origin;

    let backendIndex = groupIndex.get(profile);
    if (!backendIndex) {
      backendIndex = new Map();
      groupIndex.set(profile, backendIndex);
    }

    let vaultIndex = backendIndex.get(backendInstanceId);
    if (!vaultIndex) {
      vaultIndex = new Map();
      backendIndex.set(backendInstanceId, vaultIndex);
    }

    let pathIndex = vaultIndex.get(vaultId);
    if (!pathIndex) {
      pathIndex = new Map();
      vaultIndex.set(vaultId, pathIndex);
    }

    const existing = pathIndex.get(hit.path);
    if (existing) {
      existing.sections.push(hit);
      existing.observedSectionCount += 1;
      continue;
    }

    const group: MutableSourceSearchGroup = {
      source: { profile, backendInstanceId, vaultId, path: hit.path },
      representative: hit,
      sections: [hit],
      firstRawIndex: rawIndex,
      observedSectionCount: 1,
    };
    pathIndex.set(hit.path, group);
    observedGroups.push(group);
  }

  const groups = observedGroups.slice(0, sourceLimit);
  const observedSourceCount = observedGroups.length;
  const displayedSourceCount = groups.length;

  return {
    groups,
    facts: {
      returnedSectionCount: execution.response.hits.length,
      observedSourceCount,
      displayedSourceCount,
      omittedObservedSourceCount: observedSourceCount - displayedSourceCount,
      sourceLimit,
      candidateWindow: execution.candidateWindow,
    },
  };
}

function assertSourceLimit(sourceLimit: number): void {
  if (!Number.isSafeInteger(sourceLimit) || sourceLimit < 1 || sourceLimit > 100) {
    throw new RangeError("Grouped search source limit must be an integer from 1 through 100.");
  }
}
