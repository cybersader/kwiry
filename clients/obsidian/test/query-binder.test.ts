// SPDX-FileCopyrightText: 2026 cybersader
// SPDX-License-Identifier: GPL-3.0-only

import { describe, expect, it } from "vitest";

import { bindMetadataProbe, bindSearchPlan } from "../src/worker/query-binder";

describe("fixed FTS5 query binder", () => {
  it("binds Rust-owned values without interpolating them into SQL", () => {
    const sentinel = "\"term\" OR '); DROP TABLE chunks; --";
    const bound = bindSearchPlan({
      schema_version: 1,
      plan_id: "lexical_explicit_v1",
      match_value: sentinel,
    }, 20);
    expect(bound.sql).not.toContain("DROP TABLE");
    expect(bound.bind).toEqual([sentinel, 20]);
  });

  it("uses a separate fixed metadata probe statement", () => {
    const bound = bindMetadataProbe({
      schema_version: 1,
      plan_id: "metadata_probe_v1",
      match_value: "{title} : \"query\"",
    });
    expect(bound.sql).toContain("SELECT EXISTS");
    expect(bound.bind).toEqual(["{title} : \"query\""]);
  });

  it("rejects unknown plan identities and invalid limits", () => {
    expect(() => bindSearchPlan({
      schema_version: 1,
      plan_id: "unknown" as "lexical_any_v1",
      match_value: "query",
    }, 20)).toThrow(/unsupported/);
    expect(() => bindSearchPlan({
      schema_version: 1,
      plan_id: "lexical_any_v1",
      match_value: "query",
    }, 0)).toThrow(/limit/);
  });
});
