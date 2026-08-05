// SPDX-FileCopyrightText: 2026 cybersader
// SPDX-License-Identifier: GPL-3.0-only

import { readFileSync } from "node:fs";
import { beforeAll, describe, expect, it } from "vitest";

import { buildPlugin } from "../esbuild.config.mjs";
import {
  CACHE_SCHEMA_VERSION,
  WORKER_PROTOCOL_VERSION,
} from "../src/worker/protocol";

let normalBuild;
let internalBuild;

beforeAll(async () => {
  [normalBuild, internalBuild] = await Promise.all([
    buildPlugin({ write: false, production: true }),
    buildPlugin({
      write: false,
      production: true,
      internalD5cPlayground: true,
    }),
  ]);
}, 240_000);

function normalizedInputs(metafile) {
  return Object.keys(metafile.inputs).map((input) => input
    .replaceAll("\\", "/")
    .replace(/(pkg\/(?:production|internal-typo-prototype|internal-d5c-preview))\/build-[^/]+/u, "$1"));
}

describe("D5C playground Worker isolation", () => {
  it("keeps the primary Worker and production Rust identity byte-identical", () => {
    expect(WORKER_PROTOCOL_VERSION).toBe(10);
    expect(CACHE_SCHEMA_VERSION).toBe(9);
    expect(internalBuild.workerSource).toBe(normalBuild.workerSource);
    expect(internalBuild.identities.rust).toEqual(normalBuild.identities.rust);
    expect(internalBuild.identities.rust.sha256).toMatch(/^[0-9a-f]{64}$/u);
    expect(internalBuild.identities.rust.bytes).toBeGreaterThan(0);

    const normalInputs = normalizedInputs(normalBuild.workerMetafile);
    const internalPrimaryInputs = normalizedInputs(internalBuild.workerMetafile);
    expect(internalPrimaryInputs).toEqual(normalInputs);
    for (const input of internalPrimaryInputs) {
      expect(input).not.toContain("d5c-playground-worker.ts");
      expect(input).not.toContain("d5c-preview.ts");
      expect(input).not.toContain("pkg/internal-d5c-preview");
    }
  });

  it("compile-gates the private command, fixture corpus, modal, and settings out of normal main", () => {
    expect(internalBuild.mainText).not.toBe(normalBuild.mainText);
    const privateMarkers = [
      "open-private-d5c-balanced-playground",
      "Private D5C Balanced playground",
      "balanced-playground-v1",
      "internalD5cPlayground",
      "__kwiry_internal_d5c_playground",
    ];
    for (const marker of privateMarkers) {
      expect(normalBuild.mainText).not.toContain(marker);
      expect(internalBuild.mainText).toContain(marker);
    }
    const normalInputs = normalizedInputs(normalBuild.mainMetafile);
    const internalInputs = normalizedInputs(internalBuild.mainMetafile);
    expect(normalInputs.some((input) => input.includes("d5c-playground"))).toBe(false);
    expect(internalInputs.some((input) => input.endsWith("internal/d5c-playground/index.ts"))).toBe(true);
    expect(internalInputs.some((input) => input.endsWith("internal/d5c-playground/modal.ts"))).toBe(true);
    expect(internalInputs.some((input) => input.endsWith("internal/d5c-playground/session.ts"))).toBe(true);
    expect(internalInputs.some((input) => input.endsWith("internal/d5c-playground/settings.ts"))).toBe(true);
  });

  it("places fixture evaluation code only in the explicit second Worker", () => {
    const playground = internalBuild.internalD5cPlayground;
    expect(playground).not.toBeNull();
    expect(playground.identities.rust.sha256).not.toBe(internalBuild.identities.rust.sha256);
    expect(playground.identities.rust.bytes).toBeGreaterThan(internalBuild.identities.rust.bytes);

    const forbiddenInProduct = [
      "internalD5cPlayground",
      "internal_d5c_evaluate",
      "balanced-playground-v1",
      "fixture_initialize",
      "fixture_build",
      "fixture_evaluate",
      "fixture_dispose",
    ];
    for (const marker of forbiddenInProduct) {
      expect(normalBuild.workerSource).not.toContain(marker);
      expect(internalBuild.workerSource).not.toContain(marker);
      expect(normalBuild.mainText).not.toContain(marker);
    }
    for (const marker of forbiddenInProduct) {
      expect(playground.workerSource).toContain(marker);
    }

    const inputs = normalizedInputs(playground.workerMetafile);
    expect(inputs.some((input) => input.endsWith("src/worker/d5c-playground-worker.ts"))).toBe(true);
    expect(inputs.some((input) => input.endsWith("src/worker/d5c-evaluation.ts"))).toBe(true);
    expect(inputs.some((input) => input.includes("pkg/internal-d5c-preview"))).toBe(true);
    expect(inputs.some((input) => input.endsWith("src/worker/worker.ts"))).toBe(false);
    expect(inputs.some((input) => input.includes("@sqlite.org/sqlite-wasm"))).toBe(false);
  });

  it("exposes no normal Worker, cache, active-generation, connector, or ranking-computation surface", () => {
    const source = internalBuild.internalD5cPlayground.workerSource;
    for (const forbidden of [
      "export_generation",
      "restore_generation",
      "plan_reconciliation",
      "active_generation",
      "CACHE_SCHEMA_VERSION",
      "googleapis.com",
      "canva.com/api",
      "metadata_points=",
      "evidenceTier(",
      "computePoints",
      "matchRule",
      "explanationProjection",
    ]) {
      expect(source).not.toContain(forbidden);
    }
    expect(source).not.toMatch(/\bfetch\s*\(/u);
    expect(source).not.toMatch(/\bnew\s+Worker\s*\(/u);
  });

  it("keeps the production release package isolated from D5C assets", () => {
    const workflow = readFileSync(
      new URL("../../../.github/workflows/release-plugin.yml", import.meta.url),
      "utf8",
    );
    expect(workflow).toContain(
      "npm run package:release -- .tmp/release-candidate gate5.evidence.json",
    );
    expect(workflow).not.toContain("internal-d5c-playground-worker.js");
  });
});
