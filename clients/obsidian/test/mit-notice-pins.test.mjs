// SPDX-FileCopyrightText: 2026 cybersader
// SPDX-License-Identifier: GPL-3.0-only

import { spawnSync } from "node:child_process";
import { readFile } from "node:fs/promises";
import { resolve } from "node:path";
import { describe, expect, it } from "vitest";

import { collectRustReleaseGraph } from "../scripts/validate-d5c-rust-licenses.mjs";
import {
  checkMitNotices,
  crateKey,
  extractNoticeSections,
  normalizeLicenseText,
  sha256,
  unifiedDiff,
  validateNoticePins,
  verifyMitNotices,
} from "../scripts/verify-mit-notices.mjs";

const root = resolve(import.meta.dirname, "..");
const bundlePath = resolve(root, "licenses/Rust-DEPENDENCY-LICENSES.md");
const pinsPath = resolve(root, "licenses/mit-notice-pins.json");

// Two genuinely different MIT texts. One names a holder, one does not — the
// same asymmetry quick-xml and rawzip have, which is what made the shipped swap
// survive every check the digest pin could perform on its own.
const HOLDER_TEXT = `The MIT License (MIT)

Copyright (c) 2016 Someone Named

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction.
`;
const ANONYMOUS_TEXT = `MIT License

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software.
`;

function world() {
  // A miniature but structurally identical world: pin file, notice bundle, two
  // inventories, and a vendored registry.
  const vendored = new Map([
    [crateKey("holderful", "1.0.0"), { LICENSE: HOLDER_TEXT }],
    [crateKey("anonymous", "2.0.0"), { "LICENSE.txt": ANONYMOUS_TEXT }],
  ]);
  const pins = {
    schema_version: 1,
    notices: [
      {
        heading: "anonymous 2.0.0 — MIT",
        crates: [{ name: "anonymous", version: "2.0.0", license_file: "LICENSE.txt" }],
        sha256: sha256(ANONYMOUS_TEXT),
        holder: null,
        reviewed_by: "owner",
        reviewed_at: "2026-08-08",
      },
      {
        heading: "holderful 1.0.0 — MIT",
        crates: [{ name: "holderful", version: "1.0.0", license_file: "LICENSE" }],
        sha256: sha256(HOLDER_TEXT),
        holder: "Copyright (c) 2016 Someone Named",
        reviewed_by: "owner",
        reviewed_at: "2026-08-08",
      },
    ],
  };
  const bodies = new Map([
    ["anonymous 2.0.0 — MIT", ANONYMOUS_TEXT],
    ["holderful 1.0.0 — MIT", HOLDER_TEXT],
  ]);
  const mitCrates = [
    { name: "anonymous", version: "2.0.0" },
    { name: "holderful", version: "1.0.0" },
  ];
  return { vendored, pins, bodies, mitCrates };
}

function renderBundle(bodies) {
  const sections = [...bodies].map(([heading, body]) => `## ${heading}\n\n\`\`\`text\n${body}\`\`\`\n`);
  return `# Rust dependency license notices\n\n## Release license selections\n\n- MIT: see below.\n\n${sections.join("\n")}`;
}

function readerFor(vendored) {
  return async ({ name, version, licenseFile }) => {
    const crate = vendored.get(crateKey(name, version));
    // Absence must throw. A reader that returned "" here would turn every
    // missing crate into a silent pass, which is the failure this gate exists
    // to make impossible.
    if (crate === undefined) throw new Error("crate is absent from the resolved Cargo graph");
    const text = crate[licenseFile];
    if (text === undefined) throw new Error(`ENOENT: ${licenseFile}`);
    return text;
  };
}

async function run({ vendored, pins, bodies, mitCrates }) {
  return checkMitNotices({
    pins,
    bundleText: renderBundle(bodies),
    inventories: [
      { label: "D5C", mitCrates },
      { label: "DOCX", mitCrates },
    ],
    readVendoredLicense: readerFor(vendored),
  });
}

function kinds(findings) {
  return [...new Set(findings.map(({ kind }) => kind))].sort();
}

describe("MIT notice three-way verification", () => {
  it("passes when every pin, bundle body, and vendored file agree", async () => {
    const { findings, checkedCrates } = await run(world());
    expect(findings).toEqual([]);
    expect(checkedCrates).toBe(2);
  });

  // Mutation 1: the bundle drifts away from what was reviewed.
  it("kills a drifted notice body", async () => {
    const fixture = world();
    fixture.bodies.set(
      "holderful 1.0.0 — MIT",
      HOLDER_TEXT.replace("Someone Named", "Someone Else"),
    );
    const { findings } = await run(fixture);
    expect(kinds(findings)).toContain("notice_body_digest_mismatch");
    expect(findings.some(({ subject }) => subject === "holderful 1.0.0 — MIT")).toBe(true);
  });

  // Mutation 2: the vendored crate is not there. Skipping would be worse than
  // having no check at all, so absence has to be a named, hard finding.
  it("fails closed when a vendored crate is absent", async () => {
    const fixture = world();
    fixture.vendored.delete(crateKey("holderful", "1.0.0"));
    const { findings } = await run(fixture);
    expect(kinds(findings)).toContain("vendored_license_unreadable");
    expect(findings.some(({ subject }) => subject === "holderful 1.0.0")).toBe(true);
  });

  it("fails closed when the vendored crate ships no such licence file", async () => {
    const fixture = world();
    fixture.vendored.set(crateKey("holderful", "1.0.0"), { "COPYING": HOLDER_TEXT });
    const { findings } = await run(fixture);
    expect(kinds(findings)).toContain("vendored_license_unreadable");
  });

  // Mutation 3: the defect that started this. Both bodies are swapped AND both
  // digests are re-derived from the swapped text, exactly as a maintainer who
  // computes the digest from what they pasted would leave it. Holders are moved
  // too, so neither the self-referential digest, the distinct-digest rule, nor
  // the holder substring can see it — only the vendored file can.
  it("kills a two-crate notice swap that is internally self-consistent", async () => {
    const fixture = world();
    fixture.bodies.set("holderful 1.0.0 — MIT", ANONYMOUS_TEXT);
    fixture.bodies.set("anonymous 2.0.0 — MIT", HOLDER_TEXT);
    for (const notice of fixture.pins.notices) {
      const swapped = fixture.bodies.get(notice.heading);
      notice.sha256 = sha256(swapped);
      notice.holder = notice.heading.startsWith("anonymous")
        ? "Copyright (c) 2016 Someone Named"
        : null;
    }

    const { findings } = await run(fixture);
    // Prove the older defences really are blind to this shape.
    expect(kinds(findings)).not.toContain("notice_body_digest_mismatch");
    expect(kinds(findings)).not.toContain("shared_notice_body");
    expect(kinds(findings)).not.toContain("holder_absent_from_notice");
    // And that the vendored comparison is what catches it, for both crates.
    expect(kinds(findings)).toContain("vendored_license_mismatch");
    expect(findings
      .filter(({ kind }) => kind === "vendored_license_mismatch")
      .map(({ subject }) => subject)
      .sort()).toEqual(["anonymous 2.0.0", "holderful 1.0.0"]);
  });

  it("kills a swap in only one direction through the distinct-body rule too", async () => {
    const fixture = world();
    fixture.bodies.set("holderful 1.0.0 — MIT", ANONYMOUS_TEXT);
    const holderful = fixture.pins.notices.find(({ heading }) => heading.startsWith("holderful"));
    holderful.sha256 = sha256(ANONYMOUS_TEXT);
    holderful.holder = null;
    const { findings } = await run(fixture);
    expect(kinds(findings)).toContain("vendored_license_mismatch");
    expect(kinds(findings)).toContain("shared_notice_body");
  });

  it("kills a MIT crate that enters the graph with no pinned notice", async () => {
    const fixture = world();
    fixture.mitCrates.push({ name: "newcomer", version: "0.1.0" });
    const { findings } = await run(fixture);
    expect(kinds(findings)).toContain("unpinned_mit_crate");
    expect(findings.some(({ subject }) => subject === "newcomer 0.1.0")).toBe(true);
  });

  it("kills a pinned notice that outlives its crate", async () => {
    const fixture = world();
    fixture.mitCrates = fixture.mitCrates.filter(({ name }) => name !== "anonymous");
    const { findings } = await run(fixture);
    expect(kinds(findings)).toContain("stale_pin");
  });

  it("kills a version bump that leaves the pin behind", async () => {
    const fixture = world();
    fixture.mitCrates = fixture.mitCrates
      .map((crate) => (crate.name === "holderful" ? { name: "holderful", version: "1.1.0" } : crate));
    const { findings } = await run(fixture);
    expect(kinds(findings)).toEqual(expect.arrayContaining(["stale_pin", "unpinned_mit_crate"]));
  });

  it("kills a copyright holder invented for a crate that names none", async () => {
    const fixture = world();
    const anonymous = fixture.pins.notices.find(({ heading }) => heading.startsWith("anonymous"));
    const invented = `${ANONYMOUS_TEXT}\nCopyright (c) 2026 Nobody In Particular\n`;
    fixture.bodies.set("anonymous 2.0.0 — MIT", invented);
    anonymous.sha256 = sha256(invented);
    const { findings } = await run(fixture);
    expect(kinds(findings)).toContain("invented_copyright_holder");
    expect(kinds(findings)).toContain("vendored_license_mismatch");
  });

  it("kills a heading whose notice body was removed from the bundle", async () => {
    const fixture = world();
    fixture.bodies.delete("holderful 1.0.0 — MIT");
    const { findings } = await run(fixture);
    expect(kinds(findings)).toContain("missing_notice_section");
  });

  it("kills two crates sharing a heading they do not share a licence with", async () => {
    const fixture = world();
    fixture.pins.notices = [{
      heading: "holderful 1.0.0 and anonymous 2.0.0 — MIT",
      crates: [
        { name: "holderful", version: "1.0.0", license_file: "LICENSE" },
        { name: "anonymous", version: "2.0.0", license_file: "LICENSE.txt" },
      ],
      sha256: sha256(HOLDER_TEXT),
      holder: "Copyright (c) 2016 Someone Named",
      reviewed_by: "owner",
      reviewed_at: "2026-08-08",
    }];
    fixture.bodies = new Map([["holderful 1.0.0 and anonymous 2.0.0 — MIT", HOLDER_TEXT]]);
    const { findings } = await run(fixture);
    expect(kinds(findings)).toContain("shared_heading_texts_differ");
  });

  it("rejects a pin whose licence file escapes the crate root", () => {
    const pins = world().pins;
    pins.notices[0].crates[0].license_file = "../../etc/passwd";
    expect(() => validateNoticePins(pins)).toThrow("MIT notice pin crate entry is invalid");
  });

  it("normalises CRLF, a byte-order mark, and trailing newlines", () => {
    expect(normalizeLicenseText("﻿a\r\nb")).toBe("a\nb\n");
    expect(normalizeLicenseText("a\n\n\n")).toBe("a\n");
    expect(normalizeLicenseText("a")).toBe("a\n");
  });

  it("shows a reviewable difference rather than deciding it is acceptable", () => {
    const diff = unifiedDiff(HOLDER_TEXT, ANONYMOUS_TEXT, "bundled", "vendored");
    expect(diff).toContain("--- bundled");
    expect(diff).toContain("-Copyright (c) 2016 Someone Named");
    expect(diff).toContain("+MIT License");
  });
});

describe("the tracked MIT notice pins", () => {
  it("agrees with the vendored crates, the bundle, and both inventories", async () => {
    const { findings, checkedCrates } = await verifyMitNotices();
    expect(findings).toEqual([]);
    expect(checkedCrates).toBe(10);
  });

  it("stays in the canonical form `apply` writes, so accepting a notice never churns it", async () => {
    const text = await readFile(pinsPath, "utf8");
    expect(text).toBe(`${JSON.stringify(JSON.parse(text), null, 2)}\n`);
  });

  // The review gate itself: acceptance is a token naming exact bytes, so no
  // automation can take whatever upstream happens to ship.
  it("refuses to apply a notice against a stale review token, and writes nothing", async () => {
    const before = await Promise.all([pinsPath, bundlePath].map(async (path) => (
      sha256(await readFile(path, "utf8"))
    )));
    const applied = spawnSync(process.execPath, [
      resolve(root, "scripts/verify-mit-notices.mjs"),
      "apply",
      "--crate",
      "quick-xml@0.41.0",
      "--reviewed",
      "0".repeat(64),
    ], { cwd: root, encoding: "utf8" });
    expect(applied.status).not.toBe(0);
    expect(applied.stderr).toContain("does not name the vendored licence text");
    const after = await Promise.all([pinsPath, bundlePath].map(async (path) => (
      sha256(await readFile(path, "utf8"))
    )));
    expect(after).toEqual(before);
  }, 30_000);

  // A shared notice's heading names every member crate and its version in
  // prose, and nothing here can rewrite that sentence. `check` only ever uses
  // the heading as a key, so a heading left naming the previous version passes
  // every gate and ships in the notice bundle regardless — the bundle would
  // then attribute a licence to a version the release does not carry.
  //
  // The guard that was supposed to stop this compared `nextHeading` with
  // `previousHeading`, but without `--heading` a shared notice takes
  // `previousHeading` as its `nextHeading` by the very expression the
  // comparison reads, so the two could never differ and the branch was dead.
  // It now guards on the version that actually moved.
  //
  // Mutation check: restore `nextHeading !== previousHeading` in place of
  // `previousVersion !== version` and this fails — the apply succeeds, and the
  // pins file comes back holding pulldown-cmark 0.14.0 under a heading that
  // still says 0.13.4.
  it("refuses to move a shared notice's member version without a new heading", async () => {
    const pins = validateNoticePins(JSON.parse(await readFile(pinsPath, "utf8")));
    const shared = pins.notices.find(({ crates }) => crates.length > 1);
    expect(shared).toBeDefined();
    const member = shared.crates[0];
    expect(shared.heading).toContain(member.version);

    const before = await Promise.all([pinsPath, bundlePath].map(async (path) => (
      sha256(await readFile(path, "utf8"))
    )));
    const applied = spawnSync(process.execPath, [
      resolve(root, "scripts/verify-mit-notices.mjs"),
      "apply",
      "--crate",
      `${member.name}@0.0.0-moved`,
      // The review token names the bytes that are actually vendored, so the
      // refusal cannot be the stale-token refusal wearing this test's name.
      "--reviewed",
      shared.sha256,
    ], { cwd: root, encoding: "utf8" });

    expect(applied.status).not.toBe(0);
    expect(applied.stderr).toContain("a shared notice needs an explicit --heading");
    const after = await Promise.all([pinsPath, bundlePath].map(async (path) => (
      sha256(await readFile(path, "utf8"))
    )));
    expect(after).toEqual(before);
  }, 30_000);

  // The other half: a single-crate notice derives its whole heading from the
  // crate and version, so it needs no such permission and must keep applying.
  it("still derives a sole crate's heading from the version being applied", async () => {
    const pins = validateNoticePins(JSON.parse(await readFile(pinsPath, "utf8")));
    const sole = pins.notices.find(({ crates }) => crates.length === 1);
    const applied = spawnSync(process.execPath, [
      resolve(root, "scripts/verify-mit-notices.mjs"),
      "apply",
      "--crate",
      `${sole.crates[0].name}@0.0.0-moved`,
      "--reviewed",
      "0".repeat(64),
    ], { cwd: root, encoding: "utf8" });
    // It gets as far as the review token, which is the check *after* the
    // heading permission — so the heading rule did not fire.
    expect(applied.status).not.toBe(0);
    expect(applied.stderr).not.toContain("a shared notice needs an explicit --heading");
  }, 30_000);

  it("records a reviewed licence filename for every pinned crate", async () => {
    const pins = validateNoticePins(JSON.parse(await readFile(pinsPath, "utf8")));
    // The crate-to-filename mapping is the one thing discovery may propose but
    // must never decide: crates ship LICENSE, LICENSE.md, LICENSE-MIT,
    // LICENSE-MIT.md and LICENSE.txt interchangeably.
    expect(pins.notices.flatMap(({ crates }) => crates.map((crate) => crate.license_file)).sort())
      .toEqual([
        "LICENSE",
        "LICENSE",
        "LICENSE",
        "LICENSE",
        "LICENSE",
        "LICENSE",
        "LICENSE-MIT",
        "LICENSE-MIT.md",
        "LICENSE.md",
        "LICENSE.txt",
      ]);
    for (const notice of pins.notices) {
      expect(notice.reviewed_by).toBe("owner");
    }
  });

  it("pins a body for every MIT section the bundle ships", async () => {
    const bundle = await readFile(bundlePath, "utf8");
    const pins = JSON.parse(await readFile(pinsPath, "utf8"));
    const { sections, duplicates } = extractNoticeSections(bundle);
    expect(duplicates).toEqual([]);
    const mitHeadings = [...sections.keys()].filter((heading) => heading.endsWith(" — MIT"));
    expect(mitHeadings.sort())
      .toEqual(pins.notices.map(({ heading }) => heading).sort());
  });

  it("keeps both inventories on one dependency graph and one notice bundle", async () => {
    const [d5c, docx] = await Promise.all([
      readFile(resolve(root, "d5c-rust-license-inventory.json"), "utf8"),
      readFile(resolve(root, "docx-rust-license-inventory.json"), "utf8"),
    ]).then((texts) => texts.map((text) => JSON.parse(text)));
    // The bundle header states this as prose; nothing derived it, so a MIT
    // crate reaching only the DOCX graph would have carried no pinned body.
    expect(d5c.dependencies).toEqual(docx.dependencies);
    expect(d5c.notice_bundle_sha256).toBe(docx.notice_bundle_sha256);

    // The bundle header states that the production, D5C and DOCX builds resolve
    // to one identical package set. Derive it instead of asserting it in prose.
    for (const features of [[], ["internal-d5c-preview"], ["internal-docx-extractor"]]) {
      expect(collectRustReleaseGraph(features), `graph drifted for ${features.join(",") || "default"}`)
        .toEqual(d5c.dependencies);
    }
  }, 30_000);

  // The "Release license selections" roll-up is prose, so nothing re-derived it
  // when the DOCX and PDF waves grew the graph and it silently under-reported 29
  // of the 95 shipped crates. Derive it from the validated inventory instead.
  it("keeps the release license roll-up derived from the locked inventory", async () => {
    const notices = await readFile(bundlePath, "utf8");
    const tracked = JSON.parse(
      await readFile(resolve(root, "d5c-rust-license-inventory.json"), "utf8"),
    );
    const grouped = new Map();
    for (const { name, version, release_license: license } of tracked.dependencies) {
      if (!grouped.has(license)) grouped.set(license, []);
      grouped.get(license).push(`${name} ${version}`);
    }
    expect([...grouped.keys()].sort()).toEqual([
      "Apache-2.0",
      "Apache-2.0 AND BSD-3-Clause",
      "Apache-2.0 AND Unicode-3.0",
      "GPL-3.0-only",
      "MIT",
      "Unlicense",
    ]);
    let covered = 0;
    for (const [license, crates] of grouped) {
      covered += crates.length;
      expect(notices, `roll-up line for ${license} does not match the inventory`)
        .toContain(`- ${license}: ${crates.join(", ")}.`);
    }
    expect(covered).toBe(95);
  });
});
