// SPDX-FileCopyrightText: 2026 cybersader
// SPDX-License-Identifier: GPL-3.0-only
//
// Typed client for the kwiry daemon HTTP contract (CONTRACT.md §4).
// Pure module: the HTTP transport and token provider are injected so this is
// testable outside Obsidian; production wires Obsidian requestUrl and a fresh
// token-file read for every authenticated call.

import { normalizeDaemonBaseUrl, normalizeDaemonToken } from "./credentials";
import { SOURCE_FORMATS, type SourceFormat as PluginSourceFormat } from "./source-formats";

export type SearchMode = "lexical" | "semantic" | "hybrid";

export interface SearchFilters {
  vault_id?: string;
  room?: string;
  path_prefix?: string;
  tags?: string[];
  frontmatter_equals?: Record<string, string>;
}

export interface SearchRequest {
  q: string;
  mode: SearchMode;
  filters?: SearchFilters;
  limit?: number;
}

export interface Frontmatter {
  title?: string;
  description?: string;
  tags?: string[];
  status?: string;
  date?: string;
}

export type SourceFormat = PluginSourceFormat;

export const EXTRACTION_COVERAGES = [
  "indexed-complete",
  "indexed-partial",
  "skipped-no-extractable-text",
  "unreadable",
  "quarantined",
] as const;
export type ExtractionCoverage = typeof EXTRACTION_COVERAGES[number];
export type SourceFormatCounts = Record<
  SourceFormat,
  Record<ExtractionCoverage, number>
>;

/**
 * Mirrors `kwiry_core::extract::SourceLocator`: non-ranking navigation metadata
 * a chunk carries so a result can be opened where it was found. Never tokenized
 * and never part of scoring on either backend — Tantivy stores the field
 * `STORED`-only and FTS5 keeps it out of `chunk_search`.
 *
 * Each variant pairs with exactly one source format, which `locatorMatchesFormat`
 * enforces: a locator on the wrong format is an invalid daemon response, not a
 * hit to be shown without navigation.
 */
export type SourceLocator =
  | { kind: "base_view"; view: string }
  | { kind: "pdf_page"; page: number }
  | { kind: "excel_cell"; sheet: string; cell: string };

export interface SearchHit {
  chunk_id: string;
  vault_id: string;
  path: string;
  format: SourceFormat;
  coverage: ExtractionCoverage;
  locator: SourceLocator | null;
  heading_path: string[];
  score: number;
  excerpt: string;
  frontmatter: Frontmatter;
}

export interface SearchResponse {
  hits: SearchHit[];
  next_cursor: string | null;
}

export interface DaemonModelStatus {
  name: string;
  version: string;
}

export interface DaemonVaultStatus {
  vault_id: string;
  room: string | null;
  documents: number;
  chunks: number;
  last_sync: string | null;
  dirty: boolean;
  warning_count: number;
  last_error: string | null;
}

export interface DaemonStatus {
  state: "starting" | "ready" | "degraded";
  version: string;
  generation: string | null;
  chunking_version: number;
  chunks: number;
  documents: number;
  source_format_counts: SourceFormatCounts;
  last_sync: string | null;
  dirty: boolean;
  rebuilding: boolean;
  model: DaemonModelStatus | null;
  vaults: DaemonVaultStatus[];
}

export interface ApiErrorBody {
  code: string;
  message: string;
}

export class KwiryApiError extends Error {
  constructor(
    public readonly status: number,
    public readonly code: string,
    message: string,
  ) {
    super(message);
    this.name = "KwiryApiError";
  }
}

/** Injected transport; production uses Obsidian requestUrl (CORS-free). */
export interface Transport {
  (options: {
    url: string;
    method: string;
    headers: Record<string, string>;
    body?: string;
  }): Promise<{ status: number; text: string }>;
}

export interface KwiryClientOptions {
  baseUrl: string;
  tokenProvider: () => string;
  transport: Transport;
}

const MAX_RESPONSE_CHARACTERS = 4_000_000;
const MAX_HITS = 100;
const MAX_PATH_CHARACTERS = 4_096;
const MAX_EXCERPT_CHARACTERS = 262_144;
const MAX_SHORT_TEXT_CHARACTERS = 1_024;

export class KwiryClient {
  private readonly baseUrl: string;
  private readonly tokenProvider: () => string;
  private readonly transport: Transport;

  constructor(options: KwiryClientOptions) {
    this.baseUrl = normalizeDaemonBaseUrl(options.baseUrl);
    this.tokenProvider = options.tokenProvider;
    this.transport = options.transport;
  }

  async search(request: SearchRequest): Promise<SearchResponse> {
    const body: SearchRequest = {
      q: request.q,
      mode: request.mode,
      limit: request.limit ?? 20,
    };
    if (request.filters && Object.keys(request.filters).length > 0) {
      body.filters = request.filters;
    }
    return parseSearchResponse(await this.call("POST", "/v0/search", body));
  }

  async status(): Promise<DaemonStatus> {
    return parseDaemonStatus(await this.call("GET", "/v0/status"));
  }

  async health(): Promise<boolean> {
    try {
      const value = await this.call("GET", "/v0/health", undefined, false);
      return isExactRecord(value, ["status"]) && value.status === "ok";
    } catch {
      return false;
    }
  }

  private async call(
    method: string,
    path: string,
    body?: unknown,
    authenticated = true,
  ): Promise<unknown> {
    const headers: Record<string, string> = {};
    if (authenticated) {
      headers["Authorization"] = `Bearer ${normalizeDaemonToken(this.tokenProvider())}`;
    }
    if (body !== undefined) {
      headers["Content-Type"] = "application/json";
    }

    let response: { status: number; text: string };
    try {
      response = await this.transport({
        url: `${this.baseUrl}${path}`,
        method,
        headers,
        body: body === undefined ? undefined : JSON.stringify(body),
      });
    } catch {
      throw new KwiryApiError(0, "daemon_unreachable", "Kwiry daemon is unreachable.");
    }
    if (response.text.length > MAX_RESPONSE_CHARACTERS) {
      throw invalidResponse("Daemon response exceeded the supported size.");
    }

    let parsed: unknown;
    try {
      parsed = JSON.parse(response.text);
    } catch {
      parsed = undefined;
    }
    if (response.status < 200 || response.status >= 300) {
      const code = parseErrorCode(parsed) ?? `http_${response.status}`;
      throw new KwiryApiError(response.status, code, safeApiMessage(code));
    }
    if (parsed === undefined) {
      throw invalidResponse("Daemon returned an invalid response.");
    }
    return parsed;
  }
}

function parseSearchResponse(value: unknown): SearchResponse {
  if (!isExactRecord(value, ["hits", "next_cursor"]) || !Array.isArray(value.hits)) {
    throw invalidResponse("Daemon returned an invalid search response.");
  }
  if (value.hits.length > MAX_HITS) {
    throw invalidResponse("Daemon returned too many search results.");
  }
  const hits = value.hits.map(parseSearchHit);
  const nextCursor = value.next_cursor;
  if (nextCursor !== null && !isBoundedString(nextCursor, MAX_PATH_CHARACTERS)) {
    throw invalidResponse("Daemon returned an invalid search cursor.");
  }
  return { hits, next_cursor: nextCursor };
}

function parseSearchHit(value: unknown): SearchHit {
  if (
    !isExactRecord(value, [
      "chunk_id",
      "vault_id",
      "path",
      "format",
      "coverage",
      "locator",
      "heading_path",
      "score",
      "excerpt",
      "frontmatter",
    ])
    || !isBoundedString(value.chunk_id, MAX_SHORT_TEXT_CHARACTERS)
    || !isBoundedString(value.vault_id, MAX_SHORT_TEXT_CHARACTERS)
    || !isBoundedString(value.path, MAX_PATH_CHARACTERS)
    || !isSourceFormat(value.format)
    || !isExtractionCoverage(value.coverage)
    || !isSourceLocator(value.locator)
    || !locatorMatchesFormat(value.locator, value.format)
    || !Array.isArray(value.heading_path)
    || value.heading_path.length > 64
    || !value.heading_path.every((heading) => isBoundedString(heading, MAX_SHORT_TEXT_CHARACTERS))
    || typeof value.score !== "number"
    || !Number.isFinite(value.score)
    || !isBoundedString(value.excerpt, MAX_EXCERPT_CHARACTERS, true)
  ) {
    throw invalidResponse("Daemon returned an invalid search hit.");
  }
  return {
    chunk_id: value.chunk_id,
    vault_id: value.vault_id,
    path: value.path,
    format: value.format,
    coverage: value.coverage,
    locator: value.locator,
    heading_path: value.heading_path,
    score: value.score,
    excerpt: value.excerpt,
    frontmatter: parseFrontmatter(value.frontmatter),
  };
}

function isSourceFormat(value: unknown): value is SourceFormat {
  return value === "markdown"
    || value === "text"
    || value === "base"
    || value === "canvas"
    || value === "docx"
    || value === "pdf"
    || value === "excalidraw"
    || value === "excel";
}

function isExtractionCoverage(value: unknown): value is ExtractionCoverage {
  return EXTRACTION_COVERAGES.includes(value as ExtractionCoverage);
}

function isSourceLocator(value: unknown): value is SourceLocator | null {
  if (value === null) return true;
  if (isExactRecord(value, ["kind", "view"])) {
    return value.kind === "base_view"
      && isBoundedString(value.view, MAX_SHORT_TEXT_CHARACTERS);
  }
  if (isExactRecord(value, ["kind", "page"])) {
    return value.kind === "pdf_page" && isPdfPageNumber(value.page);
  }
  if (isExactRecord(value, ["kind", "sheet", "cell"])) {
    return value.kind === "excel_cell"
      && isBoundedString(value.sheet, MAX_PATH_CHARACTERS)
      && isBoundedString(value.cell, 32);
  }
  return false;
}

/**
 * Bounds a page by the Rust field's type (`u32`, 1-based) rather than by the
 * extractor's current page ceiling. The ceiling is a policy the daemon owns and
 * may raise; pinning it here would turn a legitimate deep-page hit into a
 * rejected response the moment the two drift.
 */
function isPdfPageNumber(value: unknown): value is number {
  return typeof value === "number"
    && Number.isSafeInteger(value)
    && value >= 1
    && value <= 4_294_967_295;
}

function locatorMatchesFormat(locator: SourceLocator | null, format: SourceFormat): boolean {
  if (locator === null) return true;
  switch (locator.kind) {
    case "base_view": return format === "base";
    case "pdf_page": return format === "pdf";
    case "excel_cell": return format === "excel";
  }
}

function parseFrontmatter(value: unknown): Frontmatter {
  if (!isRecord(value)) {
    throw invalidResponse("Daemon returned invalid frontmatter.");
  }
  const allowed = ["title", "description", "tags", "status", "date"];
  if (Object.keys(value).some((key) => !allowed.includes(key))) {
    throw invalidResponse("Daemon returned invalid frontmatter.");
  }
  for (const key of ["title", "description", "status", "date"] as const) {
    const field = value[key];
    if (field !== undefined && !isBoundedString(field, MAX_SHORT_TEXT_CHARACTERS, true)) {
      throw invalidResponse("Daemon returned invalid frontmatter.");
    }
  }
  if (
    value.tags !== undefined
    && (!Array.isArray(value.tags)
      || value.tags.length > 256
      || !value.tags.every((tag) => isBoundedString(tag, MAX_SHORT_TEXT_CHARACTERS, true)))
  ) {
    throw invalidResponse("Daemon returned invalid frontmatter.");
  }
  return value as Frontmatter;
}

function parseDaemonStatus(value: unknown): DaemonStatus {
  if (
    !isExactRecord(value, [
      "state",
      "version",
      "generation",
      "chunking_version",
      "documents",
      "chunks",
      "source_format_counts",
      "last_sync",
      "dirty",
      "rebuilding",
      "model",
      "vaults",
    ])
    || (value.state !== "starting" && value.state !== "ready" && value.state !== "degraded")
    || !isBoundedString(value.version, MAX_SHORT_TEXT_CHARACTERS)
    || (value.generation !== null && !isBoundedString(value.generation, MAX_SHORT_TEXT_CHARACTERS))
    || !isNonNegativeInteger(value.chunking_version)
    || !isNonNegativeInteger(value.documents)
    || !isNonNegativeInteger(value.chunks)
    || (value.last_sync !== null && !isBoundedString(value.last_sync, MAX_SHORT_TEXT_CHARACTERS))
    || typeof value.dirty !== "boolean"
    || typeof value.rebuilding !== "boolean"
    || !Array.isArray(value.vaults)
    || value.vaults.length > 1_000
  ) {
    throw invalidResponse("Daemon returned an invalid status response.");
  }
  const sourceFormatCounts = parseSourceFormatCounts(value.source_format_counts);
  if (indexedSourceCount(sourceFormatCounts) !== value.documents) {
    throw invalidResponse("Daemon status format counts did not match its document total.");
  }
  const model = value.model === null ? null : parseModelStatus(value.model);
  const vaults = value.vaults.map(parseVaultStatus);
  return {
    state: value.state,
    version: value.version,
    generation: value.generation,
    chunking_version: value.chunking_version,
    documents: value.documents,
    chunks: value.chunks,
    source_format_counts: sourceFormatCounts,
    last_sync: value.last_sync,
    dirty: value.dirty,
    rebuilding: value.rebuilding,
    model,
    vaults,
  };
}

function parseSourceFormatCounts(value: unknown): SourceFormatCounts {
  if (!isExactRecord(value, SOURCE_FORMATS)) {
    throw invalidResponse("Daemon returned invalid source format counts.");
  }
  for (const format of SOURCE_FORMATS) {
    const counts = value[format];
    if (
      !isExactRecord(counts, EXTRACTION_COVERAGES)
      || !EXTRACTION_COVERAGES.every((coverage) => isNonNegativeInteger(counts[coverage]))
    ) {
      throw invalidResponse("Daemon returned invalid source format counts.");
    }
  }
  return value as SourceFormatCounts;
}

function indexedSourceCount(counts: SourceFormatCounts): number {
  return SOURCE_FORMATS.reduce(
    (total, format) => total
      + counts[format]["indexed-complete"]
      + counts[format]["indexed-partial"],
    0,
  );
}

function parseModelStatus(value: unknown): DaemonModelStatus {
  if (
    !isExactRecord(value, ["name", "version"])
    || !isBoundedString(value.name, MAX_SHORT_TEXT_CHARACTERS)
    || !isBoundedString(value.version, MAX_SHORT_TEXT_CHARACTERS)
  ) {
    throw invalidResponse("Daemon returned an invalid model status.");
  }
  return { name: value.name, version: value.version };
}

function parseVaultStatus(value: unknown): DaemonVaultStatus {
  if (
    !isExactRecord(value, [
      "vault_id",
      "room",
      "documents",
      "chunks",
      "last_sync",
      "dirty",
      "warning_count",
      "last_error",
    ])
    || !isBoundedString(value.vault_id, MAX_SHORT_TEXT_CHARACTERS)
    || (value.room !== null && !isBoundedString(value.room, MAX_SHORT_TEXT_CHARACTERS))
    || !isNonNegativeInteger(value.documents)
    || !isNonNegativeInteger(value.chunks)
    || (value.last_sync !== null && !isBoundedString(value.last_sync, MAX_SHORT_TEXT_CHARACTERS))
    || typeof value.dirty !== "boolean"
    || !isNonNegativeInteger(value.warning_count)
    || (value.last_error !== null && !isBoundedString(value.last_error, MAX_SHORT_TEXT_CHARACTERS, true))
  ) {
    throw invalidResponse("Daemon returned an invalid vault status.");
  }
  return value as unknown as DaemonVaultStatus;
}

function parseErrorCode(value: unknown): string | null {
  if (!isExactRecord(value, ["error"]) || !isExactRecord(value.error, ["code", "message"])) {
    return null;
  }
  if (
    !isBoundedString(value.error.code, 128)
    || !isBoundedString(value.error.message, MAX_SHORT_TEXT_CHARACTERS, true)
  ) {
    return null;
  }
  return value.error.code;
}

function safeApiMessage(code: string): string {
  switch (code) {
    case "mode_unavailable":
      return "The selected search mode is unavailable for this backend.";
    case "index_not_ready":
    case "index_building":
      return "The selected backend does not have a ready index.";
    case "invalid_query":
      return "The query is not valid for the selected backend.";
    case "invalid_filter":
      return "One or more search filters are not supported.";
    case "unauthorized":
    case "forbidden":
    case "http_401":
    case "http_403":
      return "Daemon authentication failed.";
    default:
      return "The daemon could not complete the request.";
  }
}

function invalidResponse(message: string): KwiryApiError {
  return new KwiryApiError(502, "invalid_response", message);
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function isExactRecord<K extends string>(
  value: unknown,
  keys: readonly K[],
): value is Record<K, unknown> {
  if (!isRecord(value)) return false;
  const actual = Object.keys(value);
  return actual.length === keys.length && actual.every((key) => keys.includes(key as K));
}

function isBoundedString(value: unknown, maximum: number, allowEmpty = false): value is string {
  return typeof value === "string"
    && value.length <= maximum
    && (allowEmpty || value.length > 0);
}

function isNonNegativeInteger(value: unknown): value is number {
  return typeof value === "number" && Number.isSafeInteger(value) && value >= 0;
}
