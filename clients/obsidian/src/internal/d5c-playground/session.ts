// SPDX-FileCopyrightText: 2026 cybersader
// SPDX-License-Identifier: GPL-3.0-only

import type { D5cExplanationLevel } from "./settings";

const PROTOCOL_VERSION = 1;
const WORKER_NAME = "internalD5cPlayground" as const;
const SCENARIO_ID = "balanced-playground-v1" as const;
const MAX_WORKER_SOURCE_BYTES = 8 * 1024 * 1024;
const MAX_FIXTURE_BYTES = 1024 * 1024;
const MAX_CASES = 64;
const MAX_RESPONSE_BYTES = 256 * 1024;
const REQUEST_TIMEOUT_MS = 60_000;

type EvidenceTier =
  | "explicit"
  | "exact_metadata"
  | "exact_phrase"
  | "all_terms"
  | "partial_coverage"
  | "prefix";

export type PlaygroundDisposition =
  | { kind: "strict_balanced" }
  | {
      kind: "neutralized_counterfactual";
      neutralized_states: readonly DiscrepancyState[];
    }
  | { kind: "fatal"; reasons: readonly FatalReason[] };

export type DiscrepancyState = "unsupported" | "stale" | "unavailable" | "untrusted";
export type FatalReason = "malformed" | "conflicting" | "missing" | "unauthorized" | "over_limit";

export interface ComparisonRankingEntry {
  candidate_ordinal: number;
  tier: EvidenceTier;
  metadata_points: number;
}

export interface ComparisonRanking {
  label: "text" | "strict_balanced" | "neutralized_counterfactual";
  ordered_candidate_ordinals: number[];
  entries: ComparisonRankingEntry[];
}

export interface ExplanationSummary {
  candidate_count: number;
  moved_candidate_count: number;
  matched_signal_count: number;
  nonmatched_signal_count: number;
  absent_signal_count: number;
  neutralized_signal_count: number;
}

export interface RuleExplanation {
  rule: { kind: "recency" | "authority" | "archive" } | { kind: "property"; ordinal: number };
  outcome: "matched" | "nonmatched" | "absent" | "neutralized";
  points: number;
}

export interface CandidateRuleExplanation {
  candidate_ordinal: number;
  metadata_points: number;
  rules: RuleExplanation[];
}

export interface BalancedExplanation {
  schema_version: 1;
  level: D5cExplanationLevel;
  summary: ExplanationSummary;
  rules: CandidateRuleExplanation[];
}

export interface BalancedComparison {
  schema_version: 1;
  scenario_id: typeof SCENARIO_ID;
  configuration_hash: string;
  case_hash: string;
  disposition: PlaygroundDisposition;
  text_results: ComparisonRanking;
  balanced_results?: ComparisonRanking;
  explanation?: BalancedExplanation;
}

interface FixtureEvaluation {
  id: string;
  engine: "native_tantivy" | "portable_fts5" | "shared_contract";
  expected_disposition: "strict_balanced" | "neutralized_counterfactual" | "fatal";
  request: {
    case: Record<string, unknown> & { explanation_level: D5cExplanationLevel };
    [key: string]: unknown;
  };
  judgments: unknown[];
}

export interface FixtureCorpus {
  schema_version: 1;
  scenario_id: typeof SCENARIO_ID;
  sources: unknown[];
  evaluations: FixtureEvaluation[];
}

export interface PlaygroundCaseSummary {
  ordinal: number;
  engine: FixtureEvaluation["engine"];
  expectedDisposition: FixtureEvaluation["expected_disposition"];
  propertyRuleCount: number;
}

export interface PlaygroundRun {
  case: PlaygroundCaseSummary;
  comparison: BalancedComparison;
  deterministicRerun: boolean | null;
}

export interface RankingRowView {
  rank: number;
  candidateOrdinal: number;
  tier: string;
  metadataPoints: number;
  movement: number | null;
}

export interface RankingPanelView {
  kind: "text" | "strict" | "counterfactual";
  heading: string;
  qualifier: string;
  rows: RankingRowView[];
}

export interface ComparisonView {
  statusKind: "strict" | "counterfactual" | "fatal";
  status: string;
  discrepancySummary: string;
  propertyPack: string;
  text: RankingPanelView;
  balanced: RankingPanelView | null;
  explanation: BalancedExplanation | null;
  deterministic: string;
}

export interface AggregateExport {
  schema_version: 1;
  scenario_id: typeof SCENARIO_ID;
  fixture_case_count: number;
  evaluated_case_count: number;
  dispositions: {
    strict_balanced: number;
    neutralized_counterfactual: number;
    fatal: number;
  };
  engines: {
    native_tantivy: number;
    portable_fts5: number;
    shared_contract: number;
  };
  deterministic_reruns: {
    checked: number;
    matching: number;
  };
}

export interface PlaygroundWorker {
  postMessage(value: unknown): void;
  terminate(): void;
  addEventListener(type: "message", listener: (event: MessageEvent<unknown>) => void): void;
  addEventListener(type: "error", listener: (event: ErrorEvent) => void): void;
  removeEventListener(type: "message", listener: (event: MessageEvent<unknown>) => void): void;
  removeEventListener(type: "error", listener: (event: ErrorEvent) => void): void;
}

export type PlaygroundWorkerFactory = (source: string) => PlaygroundWorker;

export class D5cPlaygroundSession {
  readonly cases: readonly PlaygroundCaseSummary[];
  private readonly corpus: FixtureCorpus;
  private readonly worker: PlaygroundWorker;
  private readonly latestRuns = new Map<number, PlaygroundRun>();
  private nextRequestId = 1;
  private initialized = false;
  private disposed = false;

  constructor(
    workerSource: string,
    fixtureCorpus: unknown,
    private readonly explanationLevel: D5cExplanationLevel,
    workerFactory: PlaygroundWorkerFactory = createBrowserWorker,
  ) {
    if (encodedBytes(workerSource) > MAX_WORKER_SOURCE_BYTES) {
      throw new Error("Private playground Worker source exceeds its bound.");
    }
    this.corpus = prepareFixtureCorpus(fixtureCorpus, explanationLevel);
    this.cases = Object.freeze(this.corpus.evaluations.map((evaluation, ordinal) => Object.freeze({
      ordinal,
      engine: evaluation.engine,
      expectedDisposition: evaluation.expected_disposition,
      propertyRuleCount: propertyRuleCount(evaluation),
    })));
    this.worker = workerFactory(workerSource);
  }

  async initialize(): Promise<void> {
    this.requireActive();
    if (this.initialized) return;
    await this.request("fixture_initialize", {});
    await this.request("fixture_build", { fixture: this.corpus });
    this.initialized = true;
  }

  async evaluate(ordinal: number): Promise<PlaygroundRun> {
    this.requireReady();
    const evaluation = this.corpus.evaluations[ordinal];
    const summary = this.cases[ordinal];
    if (!evaluation || !summary) throw new Error("Fixture case is unavailable.");
    const result = await this.request("fixture_evaluate", { evaluation_id: evaluation.id });
    if (!isRecord(result)
      || result.evaluation_id !== evaluation.id
      || !Object.hasOwn(result, "comparison")) {
      throw new Error("Private playground evaluation response is invalid.");
    }
    const comparison = parseBalancedComparison(result.comparison);
    if (comparison.disposition.kind !== evaluation.expected_disposition) {
      throw new Error("Private playground disposition differs from the fixture contract.");
    }
    if (comparison.disposition.kind !== "fatal") {
      if (this.explanationLevel === "off" && comparison.explanation !== undefined) {
        throw new Error("Rust comparison returned an explanation while explanations are off.");
      }
      if (this.explanationLevel !== "off"
        && comparison.explanation?.level !== this.explanationLevel) {
        throw new Error("Rust comparison returned the wrong explanation level.");
      }
    }
    const previous = this.latestRuns.get(ordinal);
    const deterministicRerun = previous === undefined
      ? null
      : JSON.stringify(previous.comparison) === JSON.stringify(comparison);
    const run = Object.freeze({ case: summary, comparison, deterministicRerun });
    this.latestRuns.set(ordinal, run);
    return run;
  }

  async runAll(): Promise<readonly PlaygroundRun[]> {
    this.requireReady();
    const runs: PlaygroundRun[] = [];
    for (let ordinal = 0; ordinal < this.cases.length; ordinal += 1) {
      runs.push(await this.evaluate(ordinal));
    }
    return runs;
  }

  aggregateExport(): AggregateExport {
    const aggregate: AggregateExport = {
      schema_version: 1,
      scenario_id: SCENARIO_ID,
      fixture_case_count: this.cases.length,
      evaluated_case_count: this.latestRuns.size,
      dispositions: {
        strict_balanced: 0,
        neutralized_counterfactual: 0,
        fatal: 0,
      },
      engines: {
        native_tantivy: 0,
        portable_fts5: 0,
        shared_contract: 0,
      },
      deterministic_reruns: { checked: 0, matching: 0 },
    };
    for (const run of this.latestRuns.values()) {
      aggregate.dispositions[run.comparison.disposition.kind] += 1;
      aggregate.engines[run.case.engine] += 1;
      if (run.deterministicRerun !== null) {
        aggregate.deterministic_reruns.checked += 1;
        if (run.deterministicRerun) aggregate.deterministic_reruns.matching += 1;
      }
    }
    return aggregate;
  }

  async dispose(): Promise<void> {
    if (this.disposed) return;
    this.disposed = true;
    if (this.initialized) {
      try {
        await this.requestUnchecked("fixture_dispose", {});
      } catch {
        // Termination below is authoritative and does not retain fixture state.
      }
    }
    this.worker.terminate();
  }

  private async request(operation: string, fields: Record<string, unknown>): Promise<unknown> {
    this.requireActive();
    return this.requestUnchecked(operation, fields);
  }

  private requestUnchecked(operation: string, fields: Record<string, unknown>): Promise<unknown> {
    const id = this.nextRequestId++;
    return new Promise((resolve, reject) => {
      const timeout = globalThis.setTimeout(() => {
        cleanup();
        reject(new Error("Private playground Worker request timed out."));
      }, REQUEST_TIMEOUT_MS);
      const onMessage = (event: MessageEvent<unknown>): void => {
        const response = event.data;
        if (!isRecord(response) || response.id !== id) return;
        cleanup();
        if (encodedBytes(response) > MAX_RESPONSE_BYTES
          || response.version !== PROTOCOL_VERSION
          || response.worker !== WORKER_NAME
          || response.operation !== operation
          || typeof response.ok !== "boolean") {
          reject(new Error("Private playground Worker response is invalid."));
          return;
        }
        if (!response.ok) {
          reject(new Error(workerErrorMessage(response.error)));
          return;
        }
        resolve(response.result);
      };
      const onError = (): void => {
        cleanup();
        reject(new Error("Private playground Worker failed."));
      };
      const cleanup = (): void => {
        globalThis.clearTimeout(timeout);
        this.worker.removeEventListener("message", onMessage);
        this.worker.removeEventListener("error", onError);
      };
      this.worker.addEventListener("message", onMessage);
      this.worker.addEventListener("error", onError);
      this.worker.postMessage({
        version: PROTOCOL_VERSION,
        worker: WORKER_NAME,
        id,
        operation,
        ...fields,
      });
    });
  }

  private requireReady(): void {
    this.requireActive();
    if (!this.initialized) throw new Error("Private playground session is not initialized.");
  }

  private requireActive(): void {
    if (this.disposed) throw new Error("Private playground session is disposed.");
  }
}

export function projectPlaygroundRun(run: PlaygroundRun): ComparisonView {
  const textRanks = rankMap(run.comparison.text_results);
  const disposition = run.comparison.disposition;
  const statusKind = disposition.kind === "strict_balanced"
    ? "strict"
    : disposition.kind === "neutralized_counterfactual"
      ? "counterfactual"
      : "fatal";
  const status = disposition.kind === "strict_balanced"
    ? "Strict Balanced result"
    : disposition.kind === "neutralized_counterfactual"
      ? "Neutralized counterfactual only — not a strict Balanced result"
      : "Balanced refused — Text remains independently available";
  const discrepancySummary = disposition.kind === "strict_balanced"
    ? "All required source facts are matched, nonmatched, or absent."
    : disposition.kind === "neutralized_counterfactual"
      ? `Neutralized states: ${disposition.neutralized_states.map(humanize).join(", ")}.`
      : `Fatal states: ${disposition.reasons.map(humanize).join(", ")}. No counterfactual was produced.`;
  const balanced = run.comparison.balanced_results === undefined
    ? null
    : rankingPanel(run.comparison.balanced_results, textRanks);
  return {
    statusKind,
    status,
    discrepancySummary,
    propertyPack: run.case.propertyRuleCount === 0
      ? "No property experiment pack"
      : `Property experiment pack · ${run.case.propertyRuleCount} bounded low-strength rule${run.case.propertyRuleCount === 1 ? "" : "s"}`,
    text: rankingPanel(run.comparison.text_results, textRanks),
    balanced,
    explanation: run.comparison.explanation ?? null,
    deterministic: run.deterministicRerun === null
      ? "Not rerun yet"
      : run.deterministicRerun
        ? "Deterministic rerun matched exactly"
        : "Deterministic rerun mismatch",
  };
}

export function parseBalancedComparison(value: unknown): BalancedComparison {
  if (!isRecord(value)
    || !hasExactOptionalKeys(value, [
      "schema_version", "scenario_id", "configuration_hash", "case_hash", "disposition",
      "text_results", "balanced_results", "explanation",
    ], ["balanced_results", "explanation"])
    || value.schema_version !== 1
    || value.scenario_id !== SCENARIO_ID
    || !isHash(value.configuration_hash)
    || !isHash(value.case_hash)) {
    throw new Error("Rust comparison envelope is invalid.");
  }
  const disposition = parseDisposition(value.disposition);
  const textResults = parseRanking(value.text_results, "text");
  const balancedResults = Object.hasOwn(value, "balanced_results")
    ? parseRanking(
        value.balanced_results,
        disposition.kind === "strict_balanced"
          ? "strict_balanced"
          : "neutralized_counterfactual",
      )
    : undefined;
  const explanation = Object.hasOwn(value, "explanation")
    ? parseExplanation(value.explanation)
    : undefined;
  if (disposition.kind === "fatal") {
    if (balancedResults !== undefined || explanation !== undefined) {
      throw new Error("Fatal comparison included a Balanced projection.");
    }
  } else if (balancedResults === undefined) {
    throw new Error("Nonfatal comparison omitted its Balanced projection.");
  }
  return Object.freeze({
    schema_version: 1,
    scenario_id: SCENARIO_ID,
    configuration_hash: value.configuration_hash,
    case_hash: value.case_hash,
    disposition,
    text_results: textResults,
    ...(balancedResults === undefined ? {} : { balanced_results: balancedResults }),
    ...(explanation === undefined ? {} : { explanation }),
  });
}

function prepareFixtureCorpus(value: unknown, explanationLevel: D5cExplanationLevel): FixtureCorpus {
  if (!isRecord(value)
    || value.schema_version !== 1
    || value.scenario_id !== SCENARIO_ID
    || !Array.isArray(value.sources)
    || !Array.isArray(value.evaluations)
    || value.evaluations.length < 1
    || value.evaluations.length > MAX_CASES
    || encodedBytes(value) > MAX_FIXTURE_BYTES) {
    throw new Error("Private playground fixture corpus is invalid or over limit.");
  }
  const clone = structuredClone(value) as Record<string, unknown>;
  const evaluations = clone.evaluations;
  if (!Array.isArray(evaluations)) throw new Error("Private playground fixture corpus is invalid.");
  for (const item of evaluations) {
    if (!isFixtureEvaluation(item)) throw new Error("Private playground fixture case is invalid.");
    item.request.case.explanation_level = explanationLevel;
  }
  if (encodedBytes(clone) > MAX_FIXTURE_BYTES) {
    throw new Error("Private playground fixture corpus is over limit.");
  }
  return clone as unknown as FixtureCorpus;
}

function isFixtureEvaluation(value: unknown): value is FixtureEvaluation {
  return isRecord(value)
    && typeof value.id === "string"
    && value.id.length > 0
    && value.id.length <= 256
    && (value.engine === "native_tantivy"
      || value.engine === "portable_fts5"
      || value.engine === "shared_contract")
    && (value.expected_disposition === "strict_balanced"
      || value.expected_disposition === "neutralized_counterfactual"
      || value.expected_disposition === "fatal")
    && isRecord(value.request)
    && isRecord(value.request.case)
    && (value.request.case.explanation_level === "off"
      || value.request.case.explanation_level === "summary"
      || value.request.case.explanation_level === "rules")
    && Array.isArray(value.judgments);
}

function propertyRuleCount(evaluation: FixtureEvaluation): number {
  const configuration = evaluation.request.configuration;
  if (!isRecord(configuration)) return 0;
  const pack = configuration.property_fixture_pack;
  if (!isRecord(pack) || !Array.isArray(pack.rules)) return 0;
  return Math.min(2, pack.rules.length);
}

function parseDisposition(value: unknown): PlaygroundDisposition {
  if (!isRecord(value) || typeof value.kind !== "string") {
    throw new Error("Rust discrepancy disposition is invalid.");
  }
  if (value.kind === "strict_balanced" && hasExactKeys(value, ["kind"])) {
    return Object.freeze({ kind: "strict_balanced" });
  }
  if (value.kind === "neutralized_counterfactual"
    && hasExactKeys(value, ["kind", "neutralized_states"])
    && Array.isArray(value.neutralized_states)
    && value.neutralized_states.length > 0
    && value.neutralized_states.every(isDiscrepancyState)) {
    if (new Set(value.neutralized_states).size !== value.neutralized_states.length) {
      throw new Error("Rust discrepancy disposition is invalid.");
    }
    return Object.freeze({
      kind: "neutralized_counterfactual",
      neutralized_states: Object.freeze([...value.neutralized_states]),
    });
  }
  if (value.kind === "fatal"
    && hasExactKeys(value, ["kind", "reasons"])
    && Array.isArray(value.reasons)
    && value.reasons.length > 0
    && value.reasons.every(isFatalReason)) {
    if (new Set(value.reasons).size !== value.reasons.length) {
      throw new Error("Rust discrepancy disposition is invalid.");
    }
    return Object.freeze({
      kind: "fatal",
      reasons: Object.freeze([...value.reasons]),
    });
  }
  throw new Error("Rust discrepancy disposition is invalid.");
}

function parseRanking(value: unknown, expectedLabel: ComparisonRanking["label"]): ComparisonRanking {
  if (!isRecord(value)
    || !hasExactKeys(value, ["label", "ordered_candidate_ordinals", "entries"])
    || value.label !== expectedLabel
    || !Array.isArray(value.ordered_candidate_ordinals)
    || !Array.isArray(value.entries)
    || value.entries.length !== value.ordered_candidate_ordinals.length) {
    throw new Error("Rust ranking projection is invalid.");
  }
  const entries = value.entries.map((entry) => parseRankingEntry(entry));
  const ordinals = value.ordered_candidate_ordinals;
  if (!ordinals.every((ordinal) => Number.isSafeInteger(ordinal) && Number(ordinal) >= 0)
    || new Set(ordinals).size !== ordinals.length
    || entries.some((entry, index) => entry.candidate_ordinal !== ordinals[index])) {
    throw new Error("Rust ranking projection is invalid.");
  }
  return Object.freeze({
    label: expectedLabel,
    ordered_candidate_ordinals: Object.freeze([...ordinals]) as number[],
    entries: Object.freeze(entries) as ComparisonRankingEntry[],
  });
}

function parseRankingEntry(value: unknown): ComparisonRankingEntry {
  if (!isRecord(value)
    || !hasExactKeys(value, ["candidate_ordinal", "tier", "metadata_points"])
    || !Number.isSafeInteger(value.candidate_ordinal)
    || Number(value.candidate_ordinal) < 0
    || !isEvidenceTier(value.tier)
    || !Number.isSafeInteger(value.metadata_points)
    || Math.abs(Number(value.metadata_points)) > 32) {
    throw new Error("Rust ranking entry is invalid.");
  }
  return Object.freeze({
    candidate_ordinal: Number(value.candidate_ordinal),
    tier: value.tier,
    metadata_points: Number(value.metadata_points),
  });
}

function parseExplanation(value: unknown): BalancedExplanation {
  if (!isRecord(value)
    || !hasExactOptionalKeys(value, ["schema_version", "level", "summary", "rules"], ["rules"])
    || value.schema_version !== 1
    || (value.level !== "summary" && value.level !== "rules")
    || !isExplanationSummary(value.summary)) {
    throw new Error("Rust explanation projection is invalid.");
  }
  const rulesValue = Object.hasOwn(value, "rules") ? value.rules : [];
  if (!Array.isArray(rulesValue)) throw new Error("Rust explanation projection is invalid.");
  const rules = rulesValue.map(parseCandidateRuleExplanation);
  if (value.level === "summary" && rules.length !== 0) {
    throw new Error("Summary explanation included rule details.");
  }
  return Object.freeze({
    schema_version: 1,
    level: value.level,
    summary: Object.freeze({ ...value.summary }),
    rules: Object.freeze(rules) as CandidateRuleExplanation[],
  });
}

function isExplanationSummary(value: unknown): value is ExplanationSummary {
  if (!isRecord(value) || !hasExactKeys(value, [
    "candidate_count", "moved_candidate_count", "matched_signal_count",
    "nonmatched_signal_count", "absent_signal_count", "neutralized_signal_count",
  ])) return false;
  return Object.values(value).every((count) => Number.isSafeInteger(count) && Number(count) >= 0);
}

function parseCandidateRuleExplanation(value: unknown): CandidateRuleExplanation {
  if (!isRecord(value)
    || !hasExactKeys(value, ["candidate_ordinal", "metadata_points", "rules"])
    || !Number.isSafeInteger(value.candidate_ordinal)
    || Number(value.candidate_ordinal) < 0
    || !Number.isSafeInteger(value.metadata_points)
    || Math.abs(Number(value.metadata_points)) > 32
    || !Array.isArray(value.rules)) {
    throw new Error("Rust rule explanation is invalid.");
  }
  return Object.freeze({
    candidate_ordinal: Number(value.candidate_ordinal),
    metadata_points: Number(value.metadata_points),
    rules: Object.freeze(value.rules.map(parseRuleExplanation)) as RuleExplanation[],
  });
}

function parseRuleExplanation(value: unknown): RuleExplanation {
  if (!isRecord(value)
    || !hasExactKeys(value, ["rule", "outcome", "points"])
    || !isRecord(value.rule)
    || !isRuleKind(value.rule)
    || !isRuleOutcome(value.outcome)
    || !Number.isSafeInteger(value.points)
    || Math.abs(Number(value.points)) > 32) {
    throw new Error("Rust rule explanation is invalid.");
  }
  return Object.freeze({
    rule: Object.freeze({ ...value.rule }),
    outcome: value.outcome,
    points: Number(value.points),
  });
}

function isRuleKind(value: Record<string, unknown>): value is RuleExplanation["rule"] {
  if ((value.kind === "recency" || value.kind === "authority" || value.kind === "archive")
    && hasExactKeys(value, ["kind"])) return true;
  return value.kind === "property"
    && hasExactKeys(value, ["kind", "ordinal"])
    && Number.isSafeInteger(value.ordinal)
    && Number(value.ordinal) >= 0
    && Number(value.ordinal) < 2;
}

function isRuleOutcome(value: unknown): value is RuleExplanation["outcome"] {
  return value === "matched"
    || value === "nonmatched"
    || value === "absent"
    || value === "neutralized";
}

function isDiscrepancyState(value: unknown): value is DiscrepancyState {
  return value === "unsupported"
    || value === "stale"
    || value === "unavailable"
    || value === "untrusted";
}

function isFatalReason(value: unknown): value is FatalReason {
  return value === "malformed"
    || value === "conflicting"
    || value === "missing"
    || value === "unauthorized"
    || value === "over_limit";
}

function isEvidenceTier(value: unknown): value is EvidenceTier {
  return value === "explicit"
    || value === "exact_metadata"
    || value === "exact_phrase"
    || value === "all_terms"
    || value === "partial_coverage"
    || value === "prefix";
}

function isHash(value: unknown): value is string {
  return typeof value === "string" && /^[0-9a-f]{64}$/u.test(value);
}

function workerErrorMessage(value: unknown): string {
  if (!isRecord(value)
    || typeof value.code !== "string"
    || typeof value.stage !== "string"
    || value.retryable !== false) {
    return "Private playground Worker refused the request.";
  }
  return `Private playground Worker refused the request (${value.stage}/${value.code}).`;
}

function createBrowserWorker(source: string): PlaygroundWorker {
  const url = URL.createObjectURL(new Blob([source], { type: "text/javascript" }));
  try {
    return new Worker(url) as unknown as PlaygroundWorker;
  } finally {
    URL.revokeObjectURL(url);
  }
}

function encodedBytes(value: unknown): number {
  try {
    const serialized = typeof value === "string" ? value : JSON.stringify(value);
    return new TextEncoder().encode(serialized).byteLength;
  } catch {
    return Number.POSITIVE_INFINITY;
  }
}

function rankingPanel(
  ranking: ComparisonRanking,
  textRanks: ReadonlyMap<number, number>,
): RankingPanelView {
  const kind = ranking.label === "text"
    ? "text"
    : ranking.label === "strict_balanced"
      ? "strict"
      : "counterfactual";
  return {
    kind,
    heading: kind === "text"
      ? "Text"
      : kind === "strict"
        ? "Strict Balanced"
        : "Counterfactual partial",
    qualifier: kind === "text"
      ? "Independent lexical ordering"
      : kind === "strict"
        ? "Complete source-fact semantics"
        : "Neutralized source facts; not strict Balanced",
    rows: ranking.entries.map((entry, index) => {
      const textRank = textRanks.get(entry.candidate_ordinal);
      return {
        rank: index + 1,
        candidateOrdinal: entry.candidate_ordinal,
        tier: humanize(entry.tier),
        metadataPoints: entry.metadata_points,
        movement: kind === "text" || textRank === undefined ? null : textRank - (index + 1),
      };
    }),
  };
}

function rankMap(ranking: ComparisonRanking): ReadonlyMap<number, number> {
  return new Map(ranking.entries.map((entry, index) => [entry.candidate_ordinal, index + 1]));
}

function humanize(value: string): string {
  return value.replaceAll("_", " ");
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function hasExactKeys(value: Record<string, unknown>, keys: readonly string[]): boolean {
  const actual = Object.keys(value);
  return actual.length === keys.length && actual.every((key) => keys.includes(key));
}

function hasExactOptionalKeys(
  value: Record<string, unknown>,
  keys: readonly string[],
  optional: readonly string[],
): boolean {
  const actual = Object.keys(value);
  return actual.every((key) => keys.includes(key))
    && keys.every((key) => optional.includes(key) || actual.includes(key));
}
