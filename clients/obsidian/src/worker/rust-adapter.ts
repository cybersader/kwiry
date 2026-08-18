// SPDX-FileCopyrightText: 2026 cybersader
// SPDX-License-Identifier: GPL-3.0-only

import rustWasmBytes from "virtual:kwiry-rust-wasm-bytes";
import {
  abi_identity,
  finalize_query,
  initSync,
  prepare_oversized_source,
  prepare_query,
  prepare_source,
} from "virtual:kwiry-rust-wasm-bindings";

import {
  FORMAT_IDENTITIES,
  FORMAT_IDENTITY_SCHEMA_VERSION,
  SECTION_LINK_FORMATS,
} from "../source-formats";
import {
  EXTRACTION_PROFILES,
  SOURCE_FORMATS,
} from "./protocol";
import type {
  ExtractionCoverage,
  ExtractionProfile,
  PropertyBag,
  SourceDescriptorInput,
  SourceFormat,
  SourceLocator,
} from "./protocol";

const ABI_VERSION = 3;
const SOURCE_SCHEMA_VERSION = 9;
const QUERY_SCHEMA_VERSION = 5;
const MATCH_PLAN_SCHEMA_VERSION = 4;

export interface RustIdentity {
  abi_version: 3;
  adapter: "kwiry-obsidian-wasm";
  adapter_version: string;
  source_preparation_schema_version: 9;
  /**
   * The extraction-policy identity the adapter compiles. Mirrored in
   * `source-formats.ts` because the host needs it before the adapter exists;
   * a Rust test asserts the two agree.
   */
  extraction_policy_fingerprint: string;
  extraction_policy: Record<string, string>;
  /** Core: the shape of a per-format identity. */
  format_identity_schema_version: number;
  /**
   * Per-row identity, one digest per compiled format. Mirrored in
   * `source-formats.ts` for the same reason as the policy fingerprint, and
   * compared here by exact equality rather than shape: this map decides which
   * cached rows are evicted, so a mirror that merely looks well-formed is not
   * enough.
   */
  format_identities: Record<string, string>;
  /**
   * Which formats have headings a link subpath can reach. Compared by exact
   * equality against the host mirror for the same reason as the identities: a
   * client that decides link behaviour itself silently refuses every format
   * admitted after it was written.
   */
  section_link_formats: Record<string, boolean>;
  lexical_query_plan_schema_version: 5;
  fts5_match_plan_schema_version: 4;
  /**
   * The chunking contract the adapter applies. Chunk rows carry it per chunk,
   * but a generation with no chunks still has to name the contract its cached
   * image was produced under, so the identity is the source of truth.
   */
  chunking_version: number;
  max_request_bytes: number;
  max_source_buffer_bytes: number;
  operations: ["prepare_source", "prepare_oversized_source", "prepare_query", "finalize_query"];
}

export type PreparedPropertyValue =
  | { type: "null" }
  | { type: "boolean"; value: boolean }
  | { type: "i64" | "u64"; value: string }
  | { type: "f64"; value: string }
  | { type: "string"; value: string }
  | { type: "sequence"; value: PreparedPropertyValue[] }
  | { type: "map"; value: PreparedFrontmatter };

export interface PreparedFrontmatter {
  [name: string]: PreparedPropertyValue;
}

export type ContentRole = "primary" | "supporting" | "latent";

export interface PreparedChunk {
  chunk: {
    chunk_id: string;
    vault_id: string;
    room: string | null;
    path: string;
    heading_path: string[];
    content: string;
    frontmatter: PropertyBag;
    links_out: string[];
    mtime: number;
    content_hash: string;
    chunking_version: number;
  };
  heading_text: string;
  normalized_heading: string | null;
  technical_identifiers: string[];
  content_role?: ContentRole;
  source_locator?: SourceLocator;
}

export interface SourcePreparation {
  schema_version: 9;
  source_key: string;
  vault_id: string;
  room?: string;
  path: string;
  format: SourceFormat;
  /** The extractor set that produced this preparation. */
  extraction_profile: ExtractionProfile;
  coverage: ExtractionCoverage;
  content_hash: string | null;
  byte_length: number;
  mtime: number;
  mtime_nanos: string;
  retrieval: {
    filename: string;
    stem: string;
    aliases: string[];
  };
  normalized_exact: {
    filename: string | null;
    stem: string | null;
    aliases: string[];
    title: string | null;
  };
  frontmatter: PreparedFrontmatter;
  chunks: PreparedChunk[];
  kind: "indexed" | "skipped";
  warning?: string;
}

export type QueryField =
  | "filename" | "stem" | "aliases" | "title" | "heading" | "tags" | "content"
  | "content_identifiers";
export type QueryFieldGroup =
  | "searchable_text" | "metadata" | "exact" | "phrase" | "prefix" | "prefix_metadata";
export type QueryEvidenceStageKind =
  | "exact_metadata" | "exact_phrase" | "all_terms" | "partial_coverage"
  | "prefix_metadata" | "prefix";

export interface LexicalQueryPlan {
  schema_version: 5;
  query: string;
  kind: "explicit" | "ordinary" | "identifier";
  match_operator: "explicit" | "any" | "all";
  assistance: "explicit_syntax_bypass" | "eligible";
  execution: "explicit_bypass" | "awaiting_evidence" | "ready" | "empty_no_evidence";
  terms: string[];
  term_intents: Array<{
    index: number;
    text: string;
    role: "required_identifier_anchor" | "optional_context";
    projection: "analyzed_text" | "exact_identifier";
    support: "unknown" | "useful" | "unsupported";
  }>;
  normalized_exact: string | null;
  exact_intent: { normalized: string; field_group: "exact" } | null;
  phrase_boost: boolean;
  phrase_intent: { terms: string[]; field_group: "phrase" } | null;
  field_groups: {
    searchable_text: QueryField[];
    metadata: QueryField[];
    exact: QueryField[];
    phrase: QueryField[];
    prefix: QueryField[];
    prefix_metadata: QueryField[];
  };
  bounds: {
    max_query_bytes: 4096;
    max_query_terms: 128;
    max_term_support_probes: 128;
    max_evidence_stages: 6;
    max_partial_coverage_terms: 128;
    min_prefix_chars: 3;
    max_prefix_terms: 8;
    max_prefix_expansions_per_term: 16;
    max_prefix_expansion_scan: 256;
    max_candidates_per_stage: 256;
    max_total_candidates: 512;
  };
  typo_stage: "disabled";
  support_probes: Array<{
    probe_id: number;
    term_index: number;
    term: string;
    field_group: "searchable_text";
  }>;
  evidence_stages: Array<{
    ordinal: number;
    kind: QueryEvidenceStageKind;
    field_group: QueryFieldGroup;
    required_term_indexes: number[];
    prefix_term_indexes: number[];
    max_candidates: number;
  }>;
  metadata_probe: {
    query: string;
    fields: ["filename", "stem", "aliases", "title", "heading", "tags"];
    conjunction: true;
  } | null;
}

export type EvidenceProbePlan =
  | {
      schema_version: 4;
      plan_id: "identifier_metadata_v3";
      match_value: string;
    }
  | {
      schema_version: 4;
      plan_id: "term_support_v3";
      probe_id: number;
      term_index: number;
      match_value?: string;
      exact_identifier?: string;
      prefix_pattern: string | null;
      max_prefix_expansions: 16;
      max_prefix_expansion_scan: 256;
      max_prefix_term_bytes: 96;
    };

export interface QueryEvidenceObservation {
  identifier_probe_matched: boolean | null;
  term_support: Array<{
    probe_id: number;
    term_index: number;
    document_frequency: number;
    prefix_expansions: number;
  }>;
  prefix_expansions: Array<{
    probe_id: number;
    term_index: number;
    terms: string[];
  }>;
}

export type StagePlanId =
  | "lexical_explicit_v3"
  | "lexical_exact_metadata_v3"
  | "lexical_exact_phrase_v3"
  | "lexical_all_terms_v3"
  | "lexical_partial_coverage_v3"
  | "lexical_prefix_metadata_v3"
  | "lexical_prefix_v3";

export interface StagePlan {
  ordinal: number;
  plan_id: StagePlanId;
  match_value?: string;
  exact_value?: string;
  required_identifiers?: string[];
  max_candidates: number;
}

export interface ExecutionPlan {
  schema_version: 4;
  profile_id: "lexical-v1";
  disposition: "explicit_bypass" | "ready" | "empty_no_evidence";
  max_total_candidates: 512;
  stages: StagePlan[];
}

export interface PreparedQuery {
  plan: LexicalQueryPlan;
  probes: EvidenceProbePlan[];
}

export interface FinalizedQuery {
  plan: LexicalQueryPlan;
  execution_plan: ExecutionPlan;
}

export type RustAdapterErrorCode =
  | "abi_mismatch"
  | "artifact_mismatch"
  | "explicit_query_unsupported"
  | "index_limit_exceeded"
  | "invalid_query"
  | "invalid_query_plan"
  | "invalid_request"
  | "invalid_response"
  | "invalid_source"
  | "rust_init_failed"
  | "source_too_large";

type ProductionAdapterOperation =
  | "prepare_source"
  | "prepare_oversized_source"
  | "prepare_query"
  | "finalize_query";

export class RustAdapterError extends Error {
  constructor(
    public readonly code: RustAdapterErrorCode,
    message: string,
  ) {
    super(message);
    this.name = "RustAdapterError";
  }
}

export function initializeRustAdapter(): RustIdentity {
  try {
    initSync({ module: rustWasmBytes.slice() });
  } catch {
    throw new RustAdapterError("rust_init_failed", "Portable Rust initialization failed.");
  }
  const identity = parseJson(abi_identity());
  if (!isRustIdentity(identity)) {
    throw new RustAdapterError("artifact_mismatch", "Portable Rust artifact identity mismatch.");
  }
  return identity;
}

import { sourcePreparationDefect } from "./source-defect";
export { sourcePreparationDefect } from "./source-defect";

export function prepareSourceWithRust(
  descriptor: SourceDescriptorInput,
  bytes: Uint8Array,
): SourcePreparation {
  const response = parseResponse(
    prepare_source(JSON.stringify({
      abi_version: ABI_VERSION,
      operation: "prepare_source",
      descriptor,
    }), bytes),
    "prepare_source",
  );
  if (!isRecord(response.result) || !hasExactKeys(response.result, ["preparation"])) {
    throw new RustAdapterError("invalid_response", "Portable Rust returned invalid source data.");
  }
  return requireSourcePreparation(response.result.preparation);
}

function requireSourcePreparation(value: unknown): SourcePreparation {
  const defect = sourcePreparationDefect(value);
  if (defect !== null) {
    // Only the fixed structural identifier crosses the diagnostic boundary;
    // property names and values remain note content and are never reported.
    const failure = new RustAdapterError(
      "invalid_response",
      "Portable Rust returned invalid source data.",
    );
    (failure as { defectField?: string }).defectField = defect;
    throw failure;
  }
  return value as SourcePreparation;
}

export function prepareOversizedSourceWithRust(
  descriptor: SourceDescriptorInput,
): SourcePreparation {
  const response = parseResponse(
    prepare_oversized_source(JSON.stringify({
      abi_version: ABI_VERSION,
      operation: "prepare_oversized_source",
      descriptor,
    })),
    "prepare_oversized_source",
  );
  if (!isRecord(response.result) || !hasExactKeys(response.result, ["preparation"])) {
    throw new RustAdapterError("invalid_response", "Portable Rust returned invalid source data.");
  }
  const preparation = requireSourcePreparation(response.result.preparation);
  if (preparation.kind !== "skipped"
    || preparation.content_hash !== null
    || preparation.byte_length !== descriptor.byte_length) {
    throw new RustAdapterError("invalid_response", "Portable Rust returned invalid oversized source data.");
  }
  return preparation;
}

export function prepareQueryWithRust(query: string): PreparedQuery {
  const response = parseResponse(
    prepare_query(JSON.stringify({
      abi_version: ABI_VERSION,
      operation: "prepare_query",
      query,
    })),
    "prepare_query",
  );
  if (!isPreparedQuery(response.result)) {
    throw new RustAdapterError("invalid_response", "Portable Rust returned invalid query data.");
  }
  return response.result;
}

export function finalizeQueryWithRust(
  query: string,
  evidence: QueryEvidenceObservation,
): FinalizedQuery {
  const response = parseResponse(
    finalize_query(JSON.stringify({
      abi_version: ABI_VERSION,
      operation: "finalize_query",
      query,
      evidence_report: {
        schema_version: QUERY_SCHEMA_VERSION,
        identifier_probe_matched: evidence.identifier_probe_matched,
        term_support: evidence.term_support,
      },
      prefix_expansions: evidence.prefix_expansions,
    })),
    "finalize_query",
  );
  if (!isFinalizedQuery(response.result)) {
    throw new RustAdapterError("invalid_response", "Portable Rust returned invalid match data.");
  }
  return response.result;
}

interface SuccessResponse {
  result: unknown;
}

function parseResponse(source: string, operation: ProductionAdapterOperation): SuccessResponse {
  const value = parseJson(source);
  if (!isRecord(value)
    || value.abi_version !== ABI_VERSION
    || value.operation !== operation
    || (value.status !== "ok" && value.status !== "error")) {
    throw new RustAdapterError("invalid_response", "Portable Rust returned an invalid response.");
  }
  if (value.status === "error") {
    if (!hasExactKeys(value, ["status", "abi_version", "operation", "error"])
      || !isRecord(value.error)
      || !hasExactKeys(value.error, ["code", "message"])
      || typeof value.error.code !== "string"
      || typeof value.error.message !== "string"
      || value.error.message.length > 1_024) {
      throw new RustAdapterError("invalid_response", "Portable Rust returned an invalid error.");
    }
    const code = adapterErrorCode(operation, value.error.code);
    if (code === null) {
      throw new RustAdapterError("invalid_response", "Portable Rust returned an unknown error.");
    }
    throw new RustAdapterError(code, safeAdapterErrorMessage(code));
  }
  if (!hasExactKeys(value, ["status", "abi_version", "operation", "result"])) {
    throw new RustAdapterError("invalid_response", "Portable Rust returned an invalid response.");
  }
  return { result: value.result };
}

function adapterErrorCode(
  operation: ProductionAdapterOperation,
  code: string,
): RustAdapterErrorCode | null {
  if (code === "invalid_request" || code === "abi_mismatch") return code;
  if (operation === "prepare_source" || operation === "prepare_oversized_source") {
    return code === "invalid_source"
      || code === "index_limit_exceeded"
      || code === "source_too_large"
      ? code
      : null;
  }
  return code === "invalid_query"
    || code === "invalid_query_plan"
    || (operation === "finalize_query" && code === "explicit_query_unsupported")
    ? code
    : null;
}

function safeAdapterErrorMessage(code: RustAdapterErrorCode): string {
  switch (code) {
    case "explicit_query_unsupported":
      return "This explicit query is unavailable in the in-plugin backend.";
    case "invalid_query":
      return "The query is invalid or exceeds the supported limits.";
    case "invalid_query_plan":
    case "invalid_request":
    case "abi_mismatch":
      return "Portable Rust returned invalid query data.";
    case "index_limit_exceeded":
      return "Portable Rust reached the index capacity limit.";
    case "invalid_source":
    case "source_too_large":
      return "Portable Rust rejected source data.";
    case "artifact_mismatch":
      return "Portable Rust artifact identity mismatch.";
    case "rust_init_failed":
      return "Portable Rust initialization failed.";
    case "invalid_response":
      return "Portable Rust returned an invalid response.";
  }
}

function parseJson(source: string): unknown {
  // The in-process Rust call has already allocated the complete response before
  // this point. A second arbitrary length cap cannot protect allocation; it can
  // only reject valid large property bags, especially when note-level metadata
  // is repeated across several chunks.
  try {
    return JSON.parse(source);
  } catch {
    throw new RustAdapterError("invalid_response", "Portable Rust returned invalid JSON.");
  }
}

function isHexDigest(value: unknown): boolean {
  return typeof value === "string" && /^[0-9a-f]{64}$/u.test(value);
}

function isExtractionPolicy(value: unknown): boolean {
  if (!isRecord(value)) return false;
  const entries = Object.entries(value);
  return entries.length > 0
    && entries.length <= 32
    && entries.every(([format, profile]) =>
      (SOURCE_FORMATS as readonly string[]).includes(format)
      && (EXTRACTION_PROFILES as readonly string[]).includes(profile as string));
}

/**
 * Exact equality against the host's mirrored map, both directions.
 *
 * A shape check would accept an adapter that compiles a different extractor
 * for a format the host thinks it knows, and every cached row of that format
 * would then be reused under an identity it was not built with. It would also
 * accept an adapter that compiles a format the host cannot name at all.
 */
function mirrorsCompiledFormatIdentities(value: unknown): boolean {
  if (!isRecord(value)) return false;
  const reported = Object.keys(value).sort();
  const mirrored = Object.keys(FORMAT_IDENTITIES).sort();
  return reported.length === mirrored.length
    && reported.every((format, index) => format === mirrored[index])
    && mirrored.every((format) => value[format] === FORMAT_IDENTITIES[format as SourceFormat]);
}

function mirrorsCompiledSectionLinkFormats(value: unknown): boolean {
  if (!isRecord(value)) return false;
  const reported = Object.keys(value).sort();
  const mirrored = Object.keys(SECTION_LINK_FORMATS).sort();
  return reported.length === mirrored.length
    && reported.every((format, index) => format === mirrored[index])
    && mirrored.every((format) =>
      value[format] === SECTION_LINK_FORMATS[format as SourceFormat]);
}

function isRustIdentity(value: unknown): value is RustIdentity {
  return isRecord(value)
    && hasExactKeys(value, [
      "abi_version",
      "adapter",
      "adapter_version",
      "source_preparation_schema_version",
      "extraction_policy_fingerprint",
      "extraction_policy",
      "format_identity_schema_version",
      "format_identities",
      "section_link_formats",
      "lexical_query_plan_schema_version",
      "fts5_match_plan_schema_version",
      "chunking_version",
      "max_request_bytes",
      "max_source_buffer_bytes",
      "operations",
    ])
    && value.abi_version === ABI_VERSION
    && value.adapter === "kwiry-obsidian-wasm"
    && isBoundedString(value.adapter_version, 128)
    && value.source_preparation_schema_version === SOURCE_SCHEMA_VERSION
    // The adapter's own policy identity, checked for shape here. The host's
    // mirrored constant is what the cache is keyed on; a Rust test asserts the
    // two agree, so this only has to reject a malformed identity.
    && isHexDigest(value.extraction_policy_fingerprint)
    && isExtractionPolicy(value.extraction_policy)
    && value.format_identity_schema_version === FORMAT_IDENTITY_SCHEMA_VERSION
    && mirrorsCompiledFormatIdentities(value.format_identities)
    && mirrorsCompiledSectionLinkFormats(value.section_link_formats)
    && value.lexical_query_plan_schema_version === QUERY_SCHEMA_VERSION
    && value.fts5_match_plan_schema_version === MATCH_PLAN_SCHEMA_VERSION
    && isNonNegativeSafeInteger(value.chunking_version)
    && isPositiveSafeInteger(value.max_request_bytes)
    && isPositiveSafeInteger(value.max_source_buffer_bytes)
    && Array.isArray(value.operations)
    && JSON.stringify(value.operations) === JSON.stringify([
      "prepare_source",
      "prepare_oversized_source",
      "prepare_query",
      "finalize_query",
    ]);
}

const SEARCHABLE_FIELDS = [
  "filename", "stem", "aliases", "title", "heading", "tags", "content",
] as const;
const METADATA_FIELDS = ["filename", "stem", "aliases", "title", "heading", "tags"] as const;
// The fields a person names a note by, and the only ones the metadata-scoped
// half of the prefix block may match.
const PREFIX_METADATA_FIELDS = ["filename", "stem", "aliases", "title"] as const;
const EXACT_FIELDS = [
  "filename", "stem", "aliases", "title", "heading", "content_identifiers",
] as const;
const QUERY_BOUNDS = Object.freeze({
  max_query_bytes: 4_096,
  max_query_terms: 128,
  max_term_support_probes: 128,
  max_evidence_stages: 6,
  max_partial_coverage_terms: 128,
  min_prefix_chars: 3,
  max_prefix_terms: 8,
  max_prefix_expansions_per_term: 16,
  max_prefix_expansion_scan: 256,
  max_candidates_per_stage: 256,
  max_total_candidates: 512,
});

function isPreparedQuery(value: unknown): value is PreparedQuery {
  return isRecord(value)
    && hasExactKeys(value, ["plan", "probes"])
    && isLexicalQueryPlan(value.plan)
    && Array.isArray(value.probes)
    && value.probes.length <= 129
    && value.probes.every(isEvidenceProbePlan)
    && probesMatchPlan(value.probes, value.plan);
}

function probesMatchPlan(probes: EvidenceProbePlan[], plan: LexicalQueryPlan): boolean {
  if (plan.assistance === "explicit_syntax_bypass" || plan.execution === "empty_no_evidence") {
    return probes.length === 0;
  }
  let cursor = 0;
  if (plan.metadata_probe !== null) {
    if (probes[0]?.plan_id !== "identifier_metadata_v3") return false;
    cursor = 1;
  }
  if (probes.length - cursor !== plan.support_probes.length) return false;
  return plan.support_probes.every((probe, index) => {
    const actual = probes[index + cursor];
    const intent = plan.term_intents[probe.term_index];
    return actual?.plan_id === "term_support_v3"
      && actual.probe_id === probe.probe_id
      && actual.term_index === probe.term_index
      && intent !== undefined
      && (intent.projection === "exact_identifier"
        ? actual.exact_identifier === intent.text
          && actual.match_value === undefined
          && actual.prefix_pattern === null
        : actual.match_value !== undefined
          && actual.exact_identifier === undefined);
  });
}

function isFinalizedQuery(value: unknown): value is FinalizedQuery {
  return isRecord(value)
    && hasExactKeys(value, ["plan", "execution_plan"])
    && isLexicalQueryPlan(value.plan)
    && isExecutionPlan(value.execution_plan, value.plan);
}

function isLexicalQueryPlan(value: unknown): value is LexicalQueryPlan {
  if (!isRecord(value)
    || !hasExactKeys(value, [
      "schema_version", "query", "kind", "match_operator", "assistance", "execution",
      "terms", "term_intents", "normalized_exact", "exact_intent", "phrase_boost",
      "phrase_intent", "field_groups", "bounds", "typo_stage", "support_probes",
      "evidence_stages", "metadata_probe",
    ])
    || value.schema_version !== QUERY_SCHEMA_VERSION
    || !isBoundedString(value.query, 4_096)
    || (value.kind !== "explicit" && value.kind !== "ordinary" && value.kind !== "identifier")
    || (value.match_operator !== "explicit" && value.match_operator !== "any"
      && value.match_operator !== "all")
    || (value.assistance !== "explicit_syntax_bypass" && value.assistance !== "eligible")
    || (value.execution !== "explicit_bypass" && value.execution !== "awaiting_evidence"
      && value.execution !== "ready" && value.execution !== "empty_no_evidence")
    || !isBoundedStrings(value.terms, 128, 4_096)
    || !Array.isArray(value.term_intents)
    || value.term_intents.length !== value.terms.length
    || !value.term_intents.every((intent, index) => isTermIntent(intent, index, (value.terms as string[])[index]))
    || !(value.normalized_exact === null
      || isBoundedUtf8String(value.normalized_exact, 4_096))
    || !isExactIntent(value.exact_intent, value.normalized_exact)
    || typeof value.phrase_boost !== "boolean"
    || !isPhraseIntent(value.phrase_intent, value.terms, value.phrase_boost)
    || !isFieldGroups(value.field_groups)
    || !isExactRecord(value.bounds, QUERY_BOUNDS)
    || value.typo_stage !== "disabled"
    || !Array.isArray(value.support_probes)
    || value.support_probes.length > 128
    || !value.support_probes.every((probe, index) => isSupportProbe(probe, index, (value.terms as string[])[index]))
    || !isEvidenceStages(
      value.evidence_stages,
      value.term_intents as LexicalQueryPlan["term_intents"],
      value.exact_intent !== null,
      value.phrase_intent !== null,
      value.execution as LexicalQueryPlan["execution"],
    )
    || !isMetadataProbe(value.metadata_probe)) {
    return false;
  }
  const kindOperator = value.kind === "explicit" ? "explicit" : value.kind === "ordinary" ? "any" : "all";
  if (value.match_operator !== kindOperator) return false;
  if (value.kind === "explicit") {
    return value.assistance === "explicit_syntax_bypass"
      && value.execution === "explicit_bypass"
      && value.terms.length === 0
      && value.term_intents.length === 0
      && value.normalized_exact === null
      && value.exact_intent === null
      && value.phrase_boost === false
      && value.phrase_intent === null
      && value.support_probes.length === 0
      && (value.evidence_stages as unknown[]).length === 0
      && value.metadata_probe === null;
  }
  if (value.assistance !== "eligible") return false;
  if (value.execution === "awaiting_evidence") {
    return value.terms.length > 0
      && value.support_probes.length === value.terms.length
      && (value.evidence_stages as unknown[]).length === 0
      && value.term_intents.every((intent) => intent.support === "unknown");
  }
  if (value.execution === "ready") {
    return value.support_probes.length === 0
      && (value.evidence_stages as unknown[]).length > 0
      && value.term_intents.every((intent) => intent.support !== "unknown");
  }
  return value.execution === "empty_no_evidence"
    && value.support_probes.length === 0
    && (value.evidence_stages as unknown[]).length === 0
    && (value.terms.length === 0 || value.term_intents.every((intent) => intent.support !== "unknown"));
}

function isTermIntent(value: unknown, index: number, term: string | undefined): boolean {
  return isRecord(value)
    && hasExactKeys(value, ["index", "text", "role", "projection", "support"])
    && value.index === index
    && value.text === term
    && (value.role === "required_identifier_anchor" || value.role === "optional_context")
    && (value.projection === "analyzed_text" || value.projection === "exact_identifier")
    && (value.projection !== "exact_identifier" || value.role === "required_identifier_anchor")
    && (value.support === "unknown" || value.support === "useful" || value.support === "unsupported");
}

function isExactIntent(value: unknown, normalized: unknown): boolean {
  if (value === null) return normalized === null;
  return isRecord(value)
    && hasExactKeys(value, ["normalized", "field_group"])
    && value.normalized === normalized
    && value.field_group === "exact";
}

function isPhraseIntent(value: unknown, terms: unknown, boost: boolean): boolean {
  if (value === null) return boost === false;
  return boost === true
    && isRecord(value)
    && hasExactKeys(value, ["terms", "field_group"])
    && JSON.stringify(value.terms) === JSON.stringify(terms)
    && Array.isArray(value.terms)
    && value.terms.length >= 2
    && value.field_group === "phrase";
}

function isFieldGroups(value: unknown): boolean {
  return isRecord(value)
    && hasExactKeys(value, [
      "searchable_text", "metadata", "exact", "phrase", "prefix", "prefix_metadata",
    ])
    && JSON.stringify(value.searchable_text) === JSON.stringify(SEARCHABLE_FIELDS)
    && JSON.stringify(value.metadata) === JSON.stringify(METADATA_FIELDS)
    && JSON.stringify(value.exact) === JSON.stringify(EXACT_FIELDS)
    && JSON.stringify(value.phrase) === JSON.stringify(SEARCHABLE_FIELDS)
    && JSON.stringify(value.prefix) === JSON.stringify(SEARCHABLE_FIELDS)
    && JSON.stringify(value.prefix_metadata) === JSON.stringify(PREFIX_METADATA_FIELDS);
}

function isSupportProbe(value: unknown, index: number, term: string | undefined): boolean {
  return isRecord(value)
    && hasExactKeys(value, ["probe_id", "term_index", "term", "field_group"])
    && value.probe_id === index
    && value.term_index === index
    && value.term === term
    && value.field_group === "searchable_text";
}

function isEvidenceStages(
  value: unknown,
  termIntents: LexicalQueryPlan["term_intents"],
  hasExactIntent: boolean,
  hasPhraseIntent: boolean,
  execution: LexicalQueryPlan["execution"],
): boolean {
  if (!Array.isArray(value) || value.length > 6) return false;
  if (execution !== "ready") return value.length === 0;
  const termCount = termIntents.length;
  const kinds = [
    "exact_metadata",
    "exact_phrase",
    "all_terms",
    "prefix_metadata",
    "prefix",
    "partial_coverage",
  ];
  const allIndexes = termIntents.map((intent) => intent.index);
  const relaxedIndexes = termIntents
    .filter((intent) => intent.role === "required_identifier_anchor" || intent.support === "useful")
    .map((intent) => intent.index)
    .slice(0, 128);
  const hasUnsupportedContext = termIntents.some((intent) =>
    intent.role === "optional_context" && intent.support === "unsupported");
  const hasPrefix = value.some((stage) => isRecord(stage) && stage.kind === "prefix");
  const expectedKinds = [
    ...(hasExactIntent ? ["exact_metadata"] : []),
    ...(hasPhraseIntent ? ["exact_phrase"] : []),
    "all_terms",
    // Bounded prefix evidence is always an ordered pair, so a plan carrying
    // only one half fails the sequence comparison below.
    ...(hasPrefix ? ["prefix_metadata", "prefix"] : []),
    ...(hasUnsupportedContext
      && relaxedIndexes.length > 0
      && JSON.stringify(relaxedIndexes) !== JSON.stringify(allIndexes)
      ? ["partial_coverage"]
      : []),
  ];
  const actualKinds: string[] = [];
  let previousKind = -1;
  for (let ordinal = 0; ordinal < value.length; ordinal += 1) {
    const stage = value[ordinal];
    if (!isRecord(stage)
      || !hasExactKeys(stage, [
        "ordinal", "kind", "field_group", "required_term_indexes", "prefix_term_indexes",
        "max_candidates",
      ])
      || stage.ordinal !== ordinal
      || !kinds.includes(String(stage.kind))
      || kinds.indexOf(String(stage.kind)) <= previousKind
      || !isTermIndexes(stage.required_term_indexes, termCount, 128)
      || !isTermIndexes(stage.prefix_term_indexes, termCount, 8)
      || !isPositiveSafeInteger(stage.max_candidates)
      || stage.max_candidates > 256) {
      return false;
    }
    previousKind = kinds.indexOf(String(stage.kind));
    actualKinds.push(String(stage.kind));
    const required = stage.required_term_indexes as number[];
    const prefixes = stage.prefix_term_indexes as number[];
    if (stage.kind === "exact_metadata"
      && (stage.field_group !== "exact" || required.length !== 0 || prefixes.length !== 0)) {
      return false;
    }
    if (stage.kind === "exact_phrase"
      && (stage.field_group !== "phrase" || required.length !== 0 || prefixes.length !== 0)) {
      return false;
    }
    if (stage.kind === "all_terms"
      && (stage.field_group !== "searchable_text"
        || prefixes.length !== 0
        || required.length !== termCount
        || required.some((index, position) => index !== position))) {
      return false;
    }
    if (stage.kind === "partial_coverage"
      && (stage.field_group !== "searchable_text"
        || required.length === 0
        || required.length >= termCount
        || prefixes.length !== 0
        || JSON.stringify(required) !== JSON.stringify(relaxedIndexes))) {
      return false;
    }
    if ((stage.kind === "prefix" || stage.kind === "prefix_metadata")
      && (stage.field_group !== (stage.kind === "prefix" ? "prefix" : "prefix_metadata")
        || prefixes.length === 0
        || JSON.stringify(required) !== JSON.stringify(relaxedIndexes)
        || prefixes.some((index) => {
          const intent = termIntents[index];
          return intent === undefined
            || intent.role !== "optional_context"
            || intent.support !== "unsupported"
            || required.includes(index);
        }))) {
      return false;
    }
  }
  return JSON.stringify(actualKinds) === JSON.stringify(expectedKinds);
}

function isTermIndexes(value: unknown, termCount: number, maximum: number): boolean {
  return Array.isArray(value)
    && value.length <= maximum
    && value.every((item, index) => Number.isSafeInteger(item)
      && item >= 0 && item < termCount && (index === 0 || value[index - 1] < item));
}

function isMetadataProbe(value: unknown): boolean {
  return value === null || (isRecord(value)
    && hasExactKeys(value, ["query", "fields", "conjunction"])
    && isBoundedString(value.query, 4_096)
    && JSON.stringify(value.fields) === JSON.stringify(METADATA_FIELDS)
    && value.conjunction === true);
}

function isEvidenceProbePlan(value: unknown): value is EvidenceProbePlan {
  if (!isRecord(value) || value.schema_version !== MATCH_PLAN_SCHEMA_VERSION) return false;
  if (value.plan_id === "identifier_metadata_v3") {
    return hasExactKeys(value, ["plan_id", "schema_version", "match_value"])
      && isBoundedString(value.match_value, 16_384);
  }
  if (value.plan_id !== "term_support_v3"
    || !hasRequiredAndOptionalKeys(value, [
      "plan_id", "schema_version", "probe_id", "term_index", "prefix_pattern",
      "max_prefix_expansions", "max_prefix_expansion_scan", "max_prefix_term_bytes",
    ], ["match_value", "exact_identifier"])
    || isNonNegativeSafeInteger(value.probe_id) === false
    || value.probe_id >= 128
    || isNonNegativeSafeInteger(value.term_index) === false
    || value.term_index >= 128
    || (value.prefix_pattern !== null && !isBoundedString(value.prefix_pattern, 4_096))
    || value.max_prefix_expansions !== 16
    || value.max_prefix_expansion_scan !== 256
    || value.max_prefix_term_bytes !== 96) {
    return false;
  }
  const hasMatch = isBoundedString(value.match_value, 16_384);
  const hasIdentifier = isBoundedUtf8String(value.exact_identifier, 4_096);
  return hasMatch !== hasIdentifier
    && (hasIdentifier ? value.prefix_pattern === null : true);

}

function isExecutionPlan(value: unknown, queryPlan: LexicalQueryPlan): value is ExecutionPlan {
  if (!isRecord(value)
    || !hasExactKeys(value, [
      "schema_version", "profile_id", "disposition", "max_total_candidates", "stages",
    ])
    || value.schema_version !== MATCH_PLAN_SCHEMA_VERSION
    || value.profile_id !== "lexical-v1"
    || (value.disposition !== "explicit_bypass" && value.disposition !== "ready"
      && value.disposition !== "empty_no_evidence")
    || value.max_total_candidates !== 512
    || !Array.isArray(value.stages)
    || value.stages.length > 6
    || !value.stages.every((stage, index) => isStagePlan(stage, index))) {
    return false;
  }
  if (value.disposition === "empty_no_evidence") {
    return queryPlan.execution === "empty_no_evidence" && value.stages.length === 0;
  }
  if (value.disposition === "explicit_bypass") {
    return queryPlan.execution === "explicit_bypass"
      && value.stages.length === 1
      && value.stages[0]?.plan_id === "lexical_explicit_v3";
  }
  const stagePlanIds: Readonly<Record<QueryEvidenceStageKind, StagePlanId>> = {
    exact_metadata: "lexical_exact_metadata_v3",
    exact_phrase: "lexical_exact_phrase_v3",
    all_terms: "lexical_all_terms_v3",
    partial_coverage: "lexical_partial_coverage_v3",
    prefix_metadata: "lexical_prefix_metadata_v3",
    prefix: "lexical_prefix_v3",
  };
  return queryPlan.execution === "ready"
    && value.stages.length === queryPlan.evidence_stages.length
    && value.stages.every((stage, index) => {
      const evidenceStage = queryPlan.evidence_stages[index];
      return evidenceStage !== undefined
        && stage.ordinal === evidenceStage.ordinal
        && stage.plan_id === stagePlanIds[evidenceStage.kind]
        && stage.max_candidates === evidenceStage.max_candidates
        && JSON.stringify(stage.required_identifiers ?? []) === JSON.stringify(
          queryPlan.term_intents
            .filter((intent) => intent.projection === "exact_identifier")
            .map((intent) => intent.text),
        );
    });
}

function isStagePlan(value: unknown, ordinal: number): value is StagePlan {
  if (!isRecord(value)
    || !hasRequiredAndOptionalKeys(
      value,
      ["ordinal", "plan_id", "max_candidates"],
      ["match_value", "exact_value", "required_identifiers"],
    )
    || value.ordinal !== ordinal
    || !isPositiveSafeInteger(value.max_candidates)
    || value.max_candidates > 512) return false;
  const requiredIdentifiers = value.required_identifiers ?? [];
  if (!Array.isArray(requiredIdentifiers)
    || requiredIdentifiers.length > 128
    || !requiredIdentifiers.every((identifier) => isBoundedUtf8String(identifier, 4_096))
    || new Set(requiredIdentifiers).size !== requiredIdentifiers.length) {
    return false;
  }
  const matchIds = [
    "lexical_explicit_v3", "lexical_exact_phrase_v3", "lexical_all_terms_v3",
    "lexical_partial_coverage_v3", "lexical_prefix_metadata_v3", "lexical_prefix_v3",
  ];
  if (value.plan_id === "lexical_exact_metadata_v3") {
    return value.match_value === undefined
      && isBoundedUtf8String(value.exact_value, 4_096);
  }
  if (!matchIds.includes(String(value.plan_id)) || value.exact_value !== undefined) return false;
  if (value.plan_id === "lexical_explicit_v3") {
    return requiredIdentifiers.length === 0 && isBoundedString(value.match_value, 16_384);
  }
  return isBoundedString(value.match_value, 16_384) || requiredIdentifiers.length > 0;
}

function isExactRecord(value: unknown, expected: Readonly<Record<string, number>>): boolean {
  return isRecord(value)
    && hasExactKeys(value, Object.keys(expected))
    && Object.entries(expected).every(([key, expectedValue]) => value[key] === expectedValue);
}

function isBoundedStrings(
  value: unknown,
  maximumItems: number,
  maximumCharacters: number,
  allowEmpty = false,
): value is string[] {
  return Array.isArray(value)
    && value.length <= maximumItems
    && value.every((item) => isBoundedString(item, maximumCharacters, allowEmpty));
}

function isBoundedString(value: unknown, maximum: number, allowEmpty = false): value is string {
  return typeof value === "string"
    && value.length <= maximum
    && (allowEmpty || value.length > 0);
}

function isBoundedUtf8String(value: unknown, maximumBytes: number): value is string {
  return typeof value === "string"
    && value.length > 0
    && new TextEncoder().encode(value).byteLength <= maximumBytes
    && !value.includes("\0");
}

function isPositiveSafeInteger(value: unknown): value is number {
  return typeof value === "number" && Number.isSafeInteger(value) && value > 0;
}

function isNonNegativeSafeInteger(value: unknown): value is number {
  return typeof value === "number" && Number.isSafeInteger(value) && value >= 0;
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function hasExactKeys(value: Record<string, unknown>, keys: readonly string[]): boolean {
  const actual = Object.keys(value);
  return actual.length === keys.length && actual.every((key) => keys.includes(key));
}

function hasRequiredAndOptionalKeys(
  value: Record<string, unknown>,
  required: readonly string[],
  optional: readonly string[],
): boolean {
  const actual = Object.keys(value);
  return required.every((key) => actual.includes(key))
    && actual.every((key) => required.includes(key) || optional.includes(key));
}
