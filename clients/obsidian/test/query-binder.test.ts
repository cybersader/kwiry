// SPDX-FileCopyrightText: 2026 cybersader
// SPDX-License-Identifier: GPL-3.0-only

import { describe, expect, it } from "vitest";

import {
  bindEvidenceProbe,
  bindSearchStage,
  requireExecutionPlanIdentity,
} from "../src/worker/query-binder";

describe("fixed FTS5 query binder", () => {
  it("binds Rust-owned values without interpolating them into SQL", () => {
    const sentinel = "\"term\" OR '); DROP TABLE chunks; --";
    const bound = bindSearchStage({
      ordinal: 0,
      plan_id: "lexical_explicit_v3",
      match_value: sentinel,
      max_candidates: 256,
    }, 20);
    expect(bound.sql).not.toContain("DROP TABLE");
    expect(bound.bind).toEqual([sentinel, 20]);
  });

  it("projects ranking and identity only, never index-derived excerpt text", () => {
    const bound = bindSearchStage({
      ordinal: 0,
      plan_id: "lexical_all_terms_v3",
      match_value: "\"quasar\"",
      max_candidates: 256,
    }, 20);
    expect(bound.sql).not.toContain("snippet(");
    expect(bound.sql).not.toContain("highlight(");
    expect(bound.sql).not.toContain("excerpt");
    expect(bound.sql).toContain("bm25(chunks_fts, 5, 6, 6, 6, 3, 1, 2, 1, 5)");
  });

  it("binds exact metadata through normalized equality, not FTS tokenization", () => {
    const bound = bindSearchStage({
      ordinal: 0,
      plan_id: "lexical_exact_metadata_v3",
      exact_value: "quasar guide",
      max_candidates: 256,
    }, 20);
    expect(bound.sql).toContain("WITH exact(value) AS (VALUES (?))");
    expect(bound.sql).not.toContain("chunks_fts MATCH");
    expect(bound.sql).not.toContain("bm25(");
    expect(bound.sql).toContain("SELECT c.rowid, 12.0");
    expect(bound.sql).toContain("s.exact_filename = exact.value");
    expect(bound.sql).toContain("source_exact_aliases AS alias");
    expect(bound.sql).toContain("c.exact_heading = exact.value");
    expect(bound.sql).toContain("chunk_exact_identifiers AS identifier");
    expect(bound.sql).not.toContain("json_each(");
    expect(bound.bind).toEqual(["quasar guide", 20]);
  });

  it("uses separate fixed support and bounded prefix statements", () => {
    const bound = bindEvidenceProbe({
      schema_version: 3,
      plan_id: "term_support_v3",
      probe_id: 0,
      term_index: 0,
      match_value: "{title} : \"query\"",
      prefix_pattern: "que%",
      max_prefix_expansions: 16,
      max_prefix_term_bytes: 96,
    });
    expect(bound.exists.sql).toContain("SELECT EXISTS");
    expect(bound.exists.bind).toEqual(["{title} : \"query\""]);
    expect(bound.prefix?.sql).toContain("chunks_fts_vocab");
    expect(bound.prefix?.bind).toEqual(["que%", 96, 17]);
  });

  it("binds exact identifier probes and intersections only through relational projections", () => {
    const probe = bindEvidenceProbe({
      schema_version: 3,
      plan_id: "term_support_v3",
      probe_id: 0,
      term_index: 0,
      exact_identifier: "rfc 9110",
      prefix_pattern: null,
      max_prefix_expansions: 16,
      max_prefix_term_bytes: 96,
    });
    expect(probe.exists.sql).toContain("FROM chunk_exact_identifiers");
    expect(probe.exists.sql).not.toContain("chunks_fts");
    expect(probe.exists.bind).toEqual(["rfc 9110"]);
    expect(probe.prefix).toBeNull();

    const combined = bindSearchStage({
      ordinal: 0,
      plan_id: "lexical_all_terms_v3",
      match_value: "{content} : (\"cache\")",
      required_identifiers: ["rfc 9110", "cve-2026-1234"],
      max_candidates: 256,
    }, 20);
    expect(combined.sql).toContain("json_each(?)");
    expect(combined.sql).toContain("chunk_exact_identifiers");
    expect(combined.sql).toContain("chunks_fts MATCH ?");
    expect(combined.bind).toEqual([
      JSON.stringify(["rfc 9110", "cve-2026-1234"]),
      "{content} : (\"cache\")",
      20,
    ]);

    const identifierOnly = bindSearchStage({
      ordinal: 0,
      plan_id: "lexical_partial_coverage_v3",
      required_identifiers: ["rfc 9110"],
      max_candidates: 256,
    }, 20);
    expect(identifierOnly.sql).not.toContain("chunks_fts MATCH");
    expect(identifierOnly.bind).toEqual([JSON.stringify(["rfc 9110"]), 20]);
  });

  it("rejects unknown plan identities, profiles, schemas, and invalid limits", () => {
    expect(() => requireExecutionPlanIdentity({
      schema_version: 2 as 3,
      profile_id: "lexical-v1",
      disposition: "empty_no_evidence",
      max_total_candidates: 512,
      stages: [],
    })).toThrow(/unsupported/);
    expect(() => requireExecutionPlanIdentity({
      schema_version: 3,
      profile_id: "unknown" as "lexical-v1",
      disposition: "empty_no_evidence",
      max_total_candidates: 512,
      stages: [],
    })).toThrow(/unsupported/);
    expect(() => bindSearchStage({
      ordinal: 0,
      plan_id: "unknown" as "lexical_all_terms_v3",
      match_value: "query",
      max_candidates: 256,
    }, 20)).toThrow(/unsupported/);
    expect(() => bindSearchStage({
      ordinal: 0,
      plan_id: "lexical_all_terms_v3",
      match_value: "query",
      max_candidates: 256,
    }, 0)).toThrow(/limit/);
    expect(() => bindSearchStage({
      ordinal: 0,
      plan_id: "lexical_exact_metadata_v3",
      match_value: "query",
      exact_value: "query",
      max_candidates: 256,
    }, 20)).toThrow(/exact stage/);
    expect(() => bindSearchStage({
      ordinal: 0,
      plan_id: "lexical_exact_metadata_v3",
      max_candidates: 256,
    }, 20)).toThrow(/exact stage/);
  });

  it("accepts complete Rust-authored exact values beyond the old lossy prefix", () => {
    const exact = "🚀".repeat(300);
    expect(bindSearchStage({
      ordinal: 0,
      plan_id: "lexical_exact_metadata_v3",
      exact_value: exact,
      max_candidates: 256,
    }, 20).bind).toEqual([exact, 20]);
    expect(() => bindSearchStage({
      ordinal: 0,
      plan_id: "lexical_exact_metadata_v3",
      exact_value: "🚀".repeat(1_025),
      max_candidates: 256,
    }, 20)).toThrow(/exact stage/u);
  });
});
