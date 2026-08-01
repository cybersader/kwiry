// SPDX-FileCopyrightText: 2026 cybersader
// SPDX-License-Identifier: GPL-3.0-only

import { ACTIVE_VAULT_ID, type ActiveVaultSource } from "../../active-vault-source";
import type { ExcerptSegment } from "../../excerpt";
import {
  createExcerptHydrator,
  extractHighlightTerms,
  type ExcerptSource,
} from "../../hydrate-excerpt";
import {
  InPluginIndexController,
  type IndexControllerStatus,
  type InitialColdPreviewLease,
} from "../../backends/in-plugin-index-controller";
import type {
  D5cCompareResult,
  D5cDisplayHit,
} from "../../worker/d5c-compare-protocol";
import {
  D5cWorkerSession,
  createBrowserD5cWorkerSession,
} from "../../worker/d5c-session";

const MAX_CONCURRENT_EXCERPT_READS = 4;
const DEFAULT_RESULT_LIMIT = 20;
const FAILURE_CODES = new Set([
  "authorization_refused",
  "index_building",
  "index_changed",
  "index_limit_exceeded",
  "invalid_request",
  "query_rejected",
  "ranking_work_limit_exceeded",
  "timeout",
  "worker_crashed",
]);
const UNREADABLE_EXCERPT: ExcerptSource = { kind: "unavailable", reason: "unreadable" };

export type D5cCoverage =
  | { kind: "starting" }
  | {
      kind: "partial";
      processed: number;
      total: number;
      documents: number;
      chunks: number;
    }
  | { kind: "complete"; documents: number; chunks: number }
  | {
      kind: "updating";
      documents: number;
      chunks: number;
      omittedNotes: number;
    }
  | {
      kind: "incomplete";
      documents: number;
      chunks: number;
      omittedNotes: number;
    }
  | { kind: "unavailable" };

export interface D5cOwnerHit extends D5cDisplayHit {
  excerpt: readonly ExcerptSegment[];
}

export interface D5cOwnerCandidate {
  ordinal: number;
  hit: D5cOwnerHit;
}

export interface D5cOwnerComparison {
  targetKey: string;
  generation: string;
  publication: D5cCompareResult["publication"];
  revision: number | null;
  coverage: D5cCoverage;
  candidates: readonly D5cOwnerCandidate[];
  textOrder: readonly number[];
  balancedOrder: readonly number[];
  movedCandidateCount: number;
  topNOverlap: number;
}

export interface D5cTechnicalSummary {
  schema_version: 2;
  scenario_id: "live-text-balanced-v1";
  searches: {
    attempted: number;
    partial: number;
    complete: number;
    updating: number;
    incomplete: number;
    failed: number;
  };
  comparisons: {
    completed: number;
    moved_candidate_count: number;
    queries_with_movement: number;
    top_n_overlap: number;
  };
  index: {
    active_reached: boolean;
    final_state: D5cCoverage["kind"];
    processed: number | null;
    total: number | null;
    omitted_notes: number;
  };
  failures: Record<string, number>;
}

export class D5cOwnerServiceError extends Error {
  constructor(readonly code: string, message: string) {
    super(message);
    this.name = "D5cOwnerServiceError";
  }
}

export interface D5cOwnerServiceOptions {
  source: ActiveVaultSource;
  workerSource: string;
  createSession?: (workerSource: string) => D5cWorkerSession;
  nextGeneration?: () => string;
  yieldControl?: () => Promise<void>;
  nowEpochSeconds?: () => string;
  resultLimit?: number;
}

export class D5cOwnerService {
  private readonly source: ActiveVaultSource;
  private readonly session: D5cWorkerSession;
  private readonly controller: InPluginIndexController;
  private readonly nowEpochSeconds: () => string;
  private readonly resultLimit: number;
  private readonly listeners = new Set<(status: IndexControllerStatus) => void>();
  private readonly aggregate = new D5cAggregate();
  private currentStatus: IndexControllerStatus | null = null;
  private disposed = false;

  constructor(options: D5cOwnerServiceOptions) {
    this.source = options.source;
    this.session = (options.createSession ?? createBrowserD5cWorkerSession)(options.workerSource);
    this.nowEpochSeconds = options.nowEpochSeconds
      ?? (() => Math.floor(Date.now() / 1_000).toString());
    this.resultLimit = options.resultLimit ?? DEFAULT_RESULT_LIMIT;
    if (!Number.isSafeInteger(this.resultLimit) || this.resultLimit < 1 || this.resultLimit > 100) {
      throw new Error("D5C owner result limit is invalid.");
    }
    let generation = 0;
    const nextGeneration = options.nextGeneration ?? (() => `d5c-owner-generation-${++generation}`);
    this.controller = new InPluginIndexController({
      source: this.source,
      worker: this.session,
      nextGeneration,
      onStatus: (status) => {
        if (this.disposed) return;
        this.currentStatus = status;
        this.aggregate.observeStatus(status);
        for (const listener of this.listeners) listener(status);
      },
      onFailure: (error) => this.aggregate.recordOperationalFailure(failureCode(error)),
      yieldControl: options.yieldControl ?? yieldToBrowser,
      initialColdPreview: { enabled: true },
    });
  }

  start(): void {
    this.requireActive();
    this.controller.start();
  }

  status(): IndexControllerStatus | null {
    return this.currentStatus;
  }

  subscribe(listener: (status: IndexControllerStatus) => void): () => void {
    this.requireActive();
    this.listeners.add(listener);
    if (this.currentStatus) listener(this.currentStatus);
    let stopped = false;
    return () => {
      if (stopped) return;
      stopped = true;
      this.listeners.delete(listener);
    };
  }

  rebuild(): void {
    this.requireActive();
    this.controller.requestRebuild();
  }

  async compare(query: string): Promise<D5cOwnerComparison> {
    this.requireActive();
    const trimmed = query.trim();
    if (!trimmed) throw new D5cOwnerServiceError("empty_query", "Type a search query.");
    const target = comparisonTarget(this.currentStatus);
    if (!target) {
      throw new D5cOwnerServiceError("index_building", "The local index is still starting.");
    }
    this.aggregate.recordAttempt();
    try {
      const result = await this.session.compareD5c(
        target.generation,
        target.revision,
        trimmed,
        this.resultLimit,
        this.nowEpochSeconds(),
      );
      this.requireActive();
      const paths = result.display_candidates.map((candidate) => candidate.hit.path);
      const sources = await this.readExcerptSources(paths);
      this.requireActive();
      if (!targetMatchesStatus(target, this.currentStatus)) {
        throw new D5cOwnerServiceError("index_changed", "The index changed during this search.");
      }
      const hydrate = createExcerptHydrator(extractHighlightTerms(trimmed));
      const candidates = result.display_candidates.map((candidate) => ({
        ordinal: candidate.ordinal,
        hit: {
          ...candidate.hit,
          excerpt: hydrate(
            candidate.hit.path,
            sources.get(candidate.hit.path) ?? UNREADABLE_EXCERPT,
            candidate.hit.heading_path,
          ),
        },
      }));
      const coverage = coverageFromStatus(this.currentStatus);
      const comparison: D5cOwnerComparison = {
        targetKey: comparisonTargetIdentity(target),
        generation: result.generation,
        publication: result.publication,
        revision: result.revision,
        coverage,
        candidates,
        textOrder: result.text_order,
        balancedOrder: result.balanced_order,
        movedCandidateCount: result.aggregate.moved_candidate_count,
        topNOverlap: result.aggregate.top_n_overlap,
      };
      this.aggregate.recordComparison(comparison);
      return comparison;
    } catch (error) {
      this.aggregate.recordSearchFailure(failureCode(error));
      throw error;
    }
  }

  technicalSummary(): D5cTechnicalSummary {
    return this.aggregate.export();
  }

  async dispose(): Promise<void> {
    if (this.disposed) return;
    this.disposed = true;
    this.listeners.clear();
    this.controller.dispose();
    this.session.forceDispose();
    await this.controller.whenDisposed();
  }

  private async readExcerptSources(paths: readonly string[]): Promise<Map<string, ExcerptSource>> {
    const distinct = [...new Set(paths)];
    const sources = new Map<string, ExcerptSource>();
    let cursor = 0;
    const readers = Array.from(
      { length: Math.min(MAX_CONCURRENT_EXCERPT_READS, distinct.length) },
      async () => {
        for (;;) {
          const index = cursor++;
          if (index >= distinct.length) return;
          const path = distinct[index]!;
          try {
            const read = await this.source.readExcerptText(path);
            sources.set(path, read.kind === "text"
              ? { kind: "text", text: read.text }
              : { kind: "unavailable", reason: read.kind === "stale" ? "unstable" : read.kind });
          } catch {
            sources.set(path, UNREADABLE_EXCERPT);
          }
        }
      },
    );
    await Promise.all(readers);
    return sources;
  }

  private requireActive(): void {
    if (this.disposed) throw new D5cOwnerServiceError("disposed", "The local search is closed.");
  }
}

interface ComparisonTarget {
  generation: string;
  revision: number | null;
  mutationEpoch: number;
}

export function comparisonTarget(status: IndexControllerStatus | null): ComparisonTarget | null {
  if (!status || status.stage === "disposed" || status.stage === "failed") return null;
  const mutationEpoch = status.mutationEpoch ?? 0;
  if (status.searchable && status.generation !== null) {
    return { generation: status.generation, revision: null, mutationEpoch };
  }
  const preview = status.initialColdPreview;
  return preview
    ? { generation: preview.generation, revision: preview.revision, mutationEpoch }
    : null;
}

export function comparisonTargetKey(status: IndexControllerStatus | null): string | null {
  const target = comparisonTarget(status);
  return target ? comparisonTargetIdentity(target) : null;
}

function comparisonTargetIdentity(target: ComparisonTarget): string {
  return `${target.generation}:${target.revision ?? "active"}:${target.mutationEpoch}`;
}

export function coverageFromStatus(status: IndexControllerStatus | null): D5cCoverage {
  if (!status) return { kind: "starting" };
  if (status.stage === "failed" || status.stage === "disposed") return { kind: "unavailable" };
  const preview = status.initialColdPreview;
  if (!status.searchable && preview) {
    return {
      kind: "partial",
      processed: preview.processed,
      total: preview.total,
      documents: preview.documents,
      chunks: preview.chunks,
    };
  }
  if (status.searchable) {
    const omittedNotes = status.quarantinedSources + status.unreadableSources;
    if (status.dirty) {
      return {
        kind: "updating",
        documents: status.documents,
        chunks: status.chunks,
        omittedNotes,
      };
    }
    return omittedNotes === 0
      ? { kind: "complete", documents: status.documents, chunks: status.chunks }
      : {
          kind: "incomplete",
          documents: status.documents,
          chunks: status.chunks,
          omittedNotes,
        };
  }
  return { kind: "starting" };
}

function targetMatchesStatus(target: ComparisonTarget, status: IndexControllerStatus | null): boolean {
  const current = comparisonTarget(status);
  return current !== null
    && current.generation === target.generation
    && current.revision === target.revision
    && current.mutationEpoch === target.mutationEpoch;
}

class D5cAggregate {
  private attempted = 0;
  private partial = 0;
  private complete = 0;
  private updating = 0;
  private incomplete = 0;
  private failed = 0;
  private comparisons = 0;
  private movedCandidates = 0;
  private queriesWithMovement = 0;
  private topNOverlap = 0;
  private activeReached = false;
  private finalCoverage: D5cCoverage = { kind: "starting" };
  private readonly failures = new Map<string, number>();

  recordAttempt(): void {
    this.attempted += 1;
  }

  observeStatus(status: IndexControllerStatus): void {
    this.finalCoverage = coverageFromStatus(status);
    if (status.searchable) this.activeReached = true;
  }

  recordComparison(comparison: D5cOwnerComparison): void {
    this.comparisons += 1;
    this.movedCandidates += comparison.movedCandidateCount;
    this.topNOverlap += comparison.topNOverlap;
    if (comparison.movedCandidateCount > 0) this.queriesWithMovement += 1;
    switch (comparison.coverage.kind) {
      case "partial": this.partial += 1; break;
      case "complete": this.complete += 1; break;
      case "updating": this.updating += 1; break;
      case "incomplete": this.incomplete += 1; break;
      case "starting":
      case "unavailable":
        this.failed += 1;
        break;
    }
  }

  recordSearchFailure(code: string): void {
    this.failed += 1;
    this.recordOperationalFailure(code);
  }

  recordOperationalFailure(code: string): void {
    this.failures.set(code, (this.failures.get(code) ?? 0) + 1);
  }

  export(): D5cTechnicalSummary {
    const partial = this.finalCoverage.kind === "partial" ? this.finalCoverage : null;
    const incomplete = this.finalCoverage.kind === "incomplete"
      || this.finalCoverage.kind === "updating"
      ? this.finalCoverage
      : null;
    return {
      schema_version: 2,
      scenario_id: "live-text-balanced-v1",
      searches: {
        attempted: this.attempted,
        partial: this.partial,
        complete: this.complete,
        updating: this.updating,
        incomplete: this.incomplete,
        failed: this.failed,
      },
      comparisons: {
        completed: this.comparisons,
        moved_candidate_count: this.movedCandidates,
        queries_with_movement: this.queriesWithMovement,
        top_n_overlap: this.topNOverlap,
      },
      index: {
        active_reached: this.activeReached,
        final_state: this.finalCoverage.kind,
        processed: partial?.processed ?? null,
        total: partial?.total ?? null,
        omitted_notes: incomplete?.omittedNotes ?? 0,
      },
      failures: Object.fromEntries([...this.failures.entries()].sort(([left], [right]) =>
        left.localeCompare(right))),
    };
  }
}

function failureCode(error: unknown): string {
  return findFailureCode(error, new Set(), 0) ?? "other";
}

function findFailureCode(
  error: unknown,
  seen: Set<object>,
  depth: number,
): string | null {
  if (typeof error !== "object" || error === null || depth > 4 || seen.size >= 16) return null;
  if (seen.has(error)) return null;
  seen.add(error);

  const candidate = "code" in error ? (error as { code?: unknown }).code : null;
  if (typeof candidate === "string" && FAILURE_CODES.has(candidate)) return candidate;

  if (error instanceof AggregateError) {
    for (const nested of error.errors) {
      const code = findFailureCode(nested, seen, depth + 1);
      if (code !== null) return code;
    }
  }
  const cause = "cause" in error ? (error as { cause?: unknown }).cause : null;
  return findFailureCode(cause, seen, depth + 1);
}

function yieldToBrowser(): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, 0));
}

export function previewCoverage(lease: InitialColdPreviewLease): D5cCoverage {
  return {
    kind: "partial",
    processed: lease.processed,
    total: lease.total,
    documents: lease.documents,
    chunks: lease.chunks,
  };
}

export { ACTIVE_VAULT_ID };
