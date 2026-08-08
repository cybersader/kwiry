// SPDX-FileCopyrightText: 2026 cybersader
// SPDX-License-Identifier: GPL-3.0-only

import { describe, expect, it } from "vitest";

import {
  D5cPlaygroundSession,
  parseBalancedComparison,
  projectPlaygroundRun,
  type BalancedComparison,
  type PlaygroundWorker,
} from "../src/internal/d5c-playground/session";
import {
  DEFAULT_D5C_PLAYGROUND_SETTINGS,
  D5C_PLAYGROUND_SETTINGS_NAMESPACE,
  loadD5cPlaygroundSettings,
  prepareD5cStoredData,
} from "../src/internal/d5c-playground/settings";
import { DEFAULT_SETTINGS } from "../src/settings";

const HASH = "a".repeat(64);

function ranking(
  label: "text" | "strict_balanced" | "neutralized_counterfactual",
  order: number[],
) {
  return {
    label,
    ordered_candidate_ordinals: order,
    entries: order.map((candidate_ordinal) => ({
      candidate_ordinal,
      tier: candidate_ordinal === 0 ? "all_terms" : "all_terms",
      metadata_points: label === "text" ? 0 : candidate_ordinal === 1 ? 2 : 0,
    })),
  };
}

function comparison(
  disposition: "strict_balanced" | "neutralized_counterfactual" | "fatal" = "strict_balanced",
  explanationLevel: "off" | "summary" | "rules" = "summary",
): Record<string, unknown> {
  const base: Record<string, unknown> = {
    schema_version: 1,
    scenario_id: "balanced-playground-v1",
    configuration_hash: HASH,
    case_hash: HASH,
    disposition: disposition === "strict_balanced"
      ? { kind: "strict_balanced" }
      : disposition === "neutralized_counterfactual"
        ? { kind: "neutralized_counterfactual", neutralized_states: ["untrusted"] }
        : { kind: "fatal", reasons: ["malformed"] },
    text_results: ranking("text", [0, 1]),
  };
  if (disposition !== "fatal") {
    base.balanced_results = ranking(disposition, [1, 0]);
    if (explanationLevel !== "off") {
      base.explanation = {
        schema_version: 1,
        level: explanationLevel,
        summary: {
          candidate_count: 2,
          moved_candidate_count: 2,
          matched_signal_count: 1,
          nonmatched_signal_count: 1,
          absent_signal_count: 2,
          neutralized_signal_count: disposition === "neutralized_counterfactual" ? 1 : 0,
        },
        ...(explanationLevel === "rules" ? { rules: [] } : {}),
      };
    }
  }
  return base;
}

function corpus(): Record<string, unknown> {
  return {
    schema_version: 1,
    scenario_id: "balanced-playground-v1",
    sources: [{
      source: { authorization_scope: "fixture:test", source_key: "private-source" },
      path: "private/path.md",
    }],
    evaluations: [{
      id: "private-evaluation-id",
      engine: "shared_contract",
      expected_disposition: "strict_balanced",
      request: {
        configuration: {
          property_fixture_pack: {
            fixture_pack_id: "private-pack",
            rules: [{ property: "secret-property" }, { property: "another-property" }],
          },
        },
        case: { explanation_level: "off" },
      },
      judgments: [],
    }],
  };
}

class FakeWorker implements PlaygroundWorker {
  readonly posted: Array<Record<string, unknown>> = [];
  terminated = false;
  private explanationLevel: "off" | "summary" | "rules" = "summary";
  private readonly messages = new Set<(event: MessageEvent<unknown>) => void>();
  private readonly errors = new Set<(event: ErrorEvent) => void>();

  postMessage(value: unknown): void {
    const request = value as Record<string, unknown>;
    this.posted.push(structuredClone(request));
    if (request.operation === "fixture_build") {
      const fixture = request.fixture as { evaluations?: Array<{ request?: { case?: { explanation_level?: unknown } } }> };
      const level = fixture.evaluations?.[0]?.request?.case?.explanation_level;
      if (level === "off" || level === "summary" || level === "rules") this.explanationLevel = level;
    }
    queueMicrotask(() => {
      const result = request.operation === "fixture_evaluate"
        ? {
            evaluation_id: request.evaluation_id,
            comparison: comparison("strict_balanced", this.explanationLevel),
          }
        : request.operation === "fixture_dispose"
          ? { closed: true }
          : {};
      const event = { data: {
        version: 1,
        worker: "internalD5cPlayground",
        id: request.id,
        operation: request.operation,
        ok: true,
        result,
      } } as MessageEvent<unknown>;
      for (const listener of this.messages) listener(event);
    });
  }

  terminate(): void {
    this.terminated = true;
  }

  addEventListener(type: "message" | "error", listener: never): void {
    if (type === "message") this.messages.add(listener);
    else this.errors.add(listener);
  }

  removeEventListener(type: "message" | "error", listener: never): void {
    if (type === "message") this.messages.delete(listener);
    else this.errors.delete(listener);
  }
}

describe("private D5C playground settings", () => {
  it("defaults privately to summary and preserves only the versioned namespace", () => {
    expect(DEFAULT_D5C_PLAYGROUND_SETTINGS.explanation_level).toBe("summary");
    expect(loadD5cPlaygroundSettings(undefined)).toEqual(DEFAULT_D5C_PLAYGROUND_SETTINGS);
    expect(loadD5cPlaygroundSettings({
      [D5C_PLAYGROUND_SETTINGS_NAMESPACE]: {
        schema_version: 1,
        explanation_level: "rules",
        ignored: "value",
      },
    })).toEqual({ schema_version: 1, explanation_level: "rules" });

    const stored = prepareD5cStoredData(DEFAULT_SETTINGS, {
      schema_version: 1,
      explanation_level: "off",
    });
    expect(stored[D5C_PLAYGROUND_SETTINGS_NAMESPACE]).toEqual({
      schema_version: 1,
      explanation_level: "off",
    });
    expect(Object.keys(stored).filter((key) => key.startsWith("__kwiry")))
      .toEqual([D5C_PLAYGROUND_SETTINGS_NAMESPACE]);
  });
});

describe("private D5C playground session", () => {
  it("uses only fixture operations, applies the private explanation level, and reruns deterministically", async () => {
    const worker = new FakeWorker();
    const session = new D5cPlaygroundSession(
      "worker source",
      corpus(),
      "rules",
      () => worker,
    );
    await session.initialize();
    const first = await session.evaluate(0);
    const second = await session.evaluate(0);

    expect(first.deterministicRerun).toBeNull();
    expect(second.deterministicRerun).toBe(true);
    expect(worker.posted.map((request) => request.operation)).toEqual([
      "fixture_initialize",
      "fixture_build",
      "fixture_evaluate",
      "fixture_evaluate",
    ]);
    const build = worker.posted[1] as { fixture: { evaluations: Array<{ request: { case: unknown } }> } };
    expect(build.fixture.evaluations[0]?.request.case).toEqual({ explanation_level: "rules" });

    const aggregate = session.aggregateExport();
    expect(aggregate).toEqual({
      schema_version: 1,
      scenario_id: "balanced-playground-v1",
      fixture_case_count: 1,
      evaluated_case_count: 1,
      dispositions: { strict_balanced: 1, neutralized_counterfactual: 0, fatal: 0 },
      engines: { native_tantivy: 0, portable_fts5: 0, shared_contract: 1 },
      deterministic_reruns: { checked: 1, matching: 1 },
    });
    const serialized = JSON.stringify(aggregate);
    for (const forbidden of [
      "private-evaluation-id", "private/path.md", "private-source", "private-pack",
      "secret-property", "query", "excerpt",
    ]) expect(serialized).not.toContain(forbidden);

    await session.dispose();
    expect(worker.posted.at(-1)?.operation).toBe("fixture_dispose");
    expect(worker.terminated).toBe(true);
  });
});

describe("private D5C playground presentation", () => {
  it("keeps Text, strict Balanced, counterfactual partial, and fatal refusal distinct", () => {
    const fixtureCase = {
      ordinal: 0,
      engine: "shared_contract" as const,
      expectedDisposition: "strict_balanced" as const,
      propertyRuleCount: 2,
    };
    const strict = projectPlaygroundRun({
      case: fixtureCase,
      comparison: parseBalancedComparison(comparison()),
      deterministicRerun: true,
    });
    expect(strict.statusKind).toBe("strict");
    expect(strict.text.heading).toBe("Text");
    expect(strict.balanced?.heading).toBe("Strict Balanced");
    expect(strict.balanced?.rows[0]).toMatchObject({
      candidateOrdinal: 1,
      tier: "all terms",
      metadataPoints: 2,
      movement: 1,
    });
    expect(strict.propertyPack).toContain("2 bounded low-strength rules");
    expect(strict.deterministic).toContain("matched exactly");

    const counterfactual = projectPlaygroundRun({
      case: { ...fixtureCase, expectedDisposition: "neutralized_counterfactual" },
      comparison: parseBalancedComparison(comparison("neutralized_counterfactual")),
      deterministicRerun: null,
    });
    expect(counterfactual.statusKind).toBe("counterfactual");
    expect(counterfactual.balanced?.heading).toBe("Counterfactual partial");
    expect(counterfactual.discrepancySummary).toBe("Neutralized states: untrusted.");

    const fatal = projectPlaygroundRun({
      case: { ...fixtureCase, expectedDisposition: "fatal" },
      comparison: parseBalancedComparison(comparison("fatal")),
      deterministicRerun: null,
    });
    expect(fatal.statusKind).toBe("fatal");
    expect(fatal.text.heading).toBe("Text");
    expect(fatal.balanced).toBeNull();
    expect(fatal.status).toContain("Text remains independently available");
    expect(fatal.discrepancySummary).toContain("No counterfactual was produced");
  });

  it("rejects explanation fields outside the Rust-owned safe projection", () => {
    const unsafe = comparison();
    const explanation = unsafe.explanation as Record<string, unknown>;
    explanation.path = "private/path.md";
    expect(() => parseBalancedComparison(unsafe)).toThrow(/explanation projection is invalid/);

    const safe = parseBalancedComparison(comparison()) as BalancedComparison;
    const rendered = JSON.stringify(projectPlaygroundRun({
      case: {
        ordinal: 0,
        engine: "shared_contract",
        expectedDisposition: "strict_balanced",
        propertyRuleCount: 2,
      },
      comparison: safe,
      deterministicRerun: null,
    }));
    for (const forbidden of [
      "private/path.md", "private-source", "secret-property", "authorization_scope",
      "source_key", "query_time_epoch_seconds",
    ]) expect(rendered).not.toContain(forbidden);
  });
});
