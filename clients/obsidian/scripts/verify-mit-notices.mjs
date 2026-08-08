// SPDX-FileCopyrightText: 2026 cybersader
// SPDX-License-Identifier: GPL-3.0-only

// Assisted notice-pin path for MIT dependency notices.
//
// The pinned digest alone is self-referential: it is sha256 of the body that
// sits in licences/Rust-DEPENDENCY-LICENSES.md, not of the crate's own upstream
// file. A maintainer who pasted the wrong text and then derived the digest from
// what they pasted passed every check — which is exactly how beta.11 and
// beta.12 shipped rawzip's licence under the quick-xml heading.
//
// This module closes that by making the check three-way: the vendored upstream
// file, the bundled notice body, and the pinned digest must all agree. What it
// deliberately does NOT do is decide that a changed licence is acceptable.
// `check` only reports; `review` only shows; `apply` refuses to write unless the
// maintainer hands back a token naming the exact bytes they read.

import { createHash } from "node:crypto";
import { mkdir, readFile, readdir, rm, writeFile } from "node:fs/promises";
import { basename, dirname, resolve } from "node:path";
import { spawnSync } from "node:child_process";
import { fileURLToPath } from "node:url";

import {
  readD5cRustLicenseInventory,
  readDocxRustLicenseInventory,
} from "./validate-d5c-rust-licenses.mjs";

const root = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const manifestPath = resolve(root, "rust/kwiry-obsidian-wasm/Cargo.toml");
const noticeBundlePath = resolve(root, "licenses/Rust-DEPENDENCY-LICENSES.md");
const noticePinsPath = resolve(root, "licenses/mit-notice-pins.json");
const reviewRoot = resolve(root, ".tmp/notice-review");
const TARGET = "wasm32-unknown-unknown";
const ALL_FEATURES = Object.freeze(["internal-d5c-preview", "internal-docx-extractor"]);
const FENCE = "```";

export const NOTICE_PINS_PATH = noticePinsPath;
export const NOTICE_BUNDLE_PATH = noticeBundlePath;

// Upstream licence files are not uniform: quick-xml and generic-array ship
// CRLF, and ecb, generic-array and rawzip ship no trailing newline at all. The
// bundle stores one canonical form so a byte comparison is meaningful.
export function normalizeLicenseText(text) {
  return text
    .replace(/^﻿/u, "")
    .replaceAll("\r\n", "\n")
    .replace(/\n*$/u, "\n");
}

export function sha256(value) {
  return createHash("sha256").update(value, "utf8").digest("hex");
}

export function crateKey(name, version) {
  return `${name} ${version}`;
}

// Sections are `## <heading>` followed by optional prose and one ```text block.
export function extractNoticeSections(bundleText) {
  const sections = new Map();
  const duplicates = [];
  for (const chunk of bundleText.split(/^## /mu).slice(1)) {
    const newline = chunk.indexOf("\n");
    const heading = newline === -1 ? chunk : chunk.slice(0, newline);
    const body = new RegExp(`${FENCE}text\\n([\\S\\s]*?)${FENCE}`, "u").exec(chunk)?.[1];
    if (body === undefined) continue;
    if (sections.has(heading)) {
      duplicates.push(heading);
      continue;
    }
    sections.set(heading, body);
  }
  return { sections, duplicates };
}

export function validateNoticePins(pins) {
  if (!isRecord(pins)
    || !hasExactKeys(pins, ["schema_version", "notices"])
    || pins.schema_version !== 1
    || !Array.isArray(pins.notices)
    || pins.notices.length === 0) {
    throw new Error("MIT notice pin file is invalid");
  }
  const seenHeadings = new Set();
  const seenCrates = new Set();
  for (const notice of pins.notices) {
    if (!isRecord(notice)
      || !hasExactKeys(notice, [
        "heading",
        "crates",
        "sha256",
        "holder",
        "reviewed_by",
        "reviewed_at",
      ])
      || !isBoundedString(notice.heading, 256)
      || !/^[0-9a-f]{64}$/u.test(notice.sha256)
      || !(notice.holder === null || isBoundedString(notice.holder, 256))
      || !isBoundedString(notice.reviewed_by, 128)
      || !/^\d{4}-\d{2}-\d{2}$/u.test(notice.reviewed_at)
      || !Array.isArray(notice.crates)
      || notice.crates.length === 0) {
      throw new Error("MIT notice pin entry is invalid");
    }
    if (seenHeadings.has(notice.heading)) {
      throw new Error(`MIT notice pin heading is duplicated: ${notice.heading}`);
    }
    seenHeadings.add(notice.heading);
    for (const crate of notice.crates) {
      if (!isRecord(crate)
        || !hasExactKeys(crate, ["name", "version", "license_file"])
        || !isBoundedString(crate.name, 128)
        || !isBoundedString(crate.version, 64)
        || !isBoundedString(crate.license_file, 128)
        // The recorded filename binds; discovery only ever proposes one. It must
        // stay a plain entry in the crate root so a pin can never reach outside
        // the vendored crate it claims to reproduce.
        || crate.license_file !== basename(crate.license_file)
        || crate.license_file === "."
        || crate.license_file === "..") {
        throw new Error("MIT notice pin crate entry is invalid");
      }
      const key = crateKey(crate.name, crate.version);
      if (seenCrates.has(key)) {
        throw new Error(`MIT notice pin crate is duplicated: ${key}`);
      }
      seenCrates.add(key);
    }
  }
  return pins;
}

// The core check, with every input injected so the failure modes below can be
// exercised without a Cargo registry.
//
//   pins                 validated pin file
//   bundleText           licences/Rust-DEPENDENCY-LICENSES.md
//   inventories          [{ label, mitCrates: [{ name, version }] }]
//   readVendoredLicense  async ({ name, version, licenseFile }) => string,
//                        and it must THROW when the crate or its licence file
//                        is not there. A check that skips what it cannot see is
//                        worse than no check, so absence is never a pass.
export async function checkMitNotices({
  pins,
  bundleText,
  inventories,
  readVendoredLicense,
}) {
  validateNoticePins(pins);
  const findings = [];
  const add = (kind, subject, detail) => findings.push({ kind, subject, detail });

  const { sections, duplicates } = extractNoticeSections(bundleText);
  for (const heading of duplicates) {
    add("duplicate_notice_section", heading, "the bundle declares this heading more than once");
  }

  const pinnedByCrate = new Map();
  for (const notice of pins.notices) {
    for (const crate of notice.crates) {
      pinnedByCrate.set(crateKey(crate.name, crate.version), notice);
    }
  }

  // Neither direction may drift: a MIT crate entering the graph without a pin
  // is an unnoticed crate, and a pin outliving its crate is a stale notice.
  const inventoried = new Map();
  for (const inventory of inventories) {
    for (const crate of inventory.mitCrates) {
      const key = crateKey(crate.name, crate.version);
      if (!inventoried.has(key)) inventoried.set(key, []);
      inventoried.get(key).push(inventory.label);
      if (!pinnedByCrate.has(key)) {
        add("unpinned_mit_crate", key, `no MIT notice is pinned (${inventory.label} inventory)`);
      }
    }
  }
  for (const key of pinnedByCrate.keys()) {
    if (!inventoried.has(key)) {
      add("stale_pin", key, "a MIT notice is pinned for a crate no inventory ships");
    }
  }

  const digestsByHeading = new Map();
  const reviewable = [];
  for (const notice of pins.notices) {
    const body = sections.get(notice.heading);
    if (body === undefined) {
      add("missing_notice_section", notice.heading, "the bundle has no notice body under this heading");
    } else if (sha256(body) !== notice.sha256) {
      // The bundle drifted away from what was reviewed.
      add(
        "notice_body_digest_mismatch",
        notice.heading,
        `bundle body is ${sha256(body)}, pinned ${notice.sha256}`,
      );
    }
    digestsByHeading.set(notice.heading, notice.sha256);

    let upstream = null;
    let upstreamDigest = null;
    for (const crate of notice.crates) {
      const key = crateKey(crate.name, crate.version);
      let text;
      try {
        text = await readVendoredLicense({
          name: crate.name,
          version: crate.version,
          licenseFile: crate.license_file,
        });
      } catch (error) {
        add(
          "vendored_license_unreadable",
          key,
          `cannot read ${crate.license_file} from the vendored crate: ${error?.message ?? error}`,
        );
        continue;
      }
      const normalized = normalizeLicenseText(text);
      if (normalized.includes(FENCE)) {
        add(
          "unrepresentable_license_text",
          key,
          "the upstream licence text contains a fenced-code delimiter",
        );
        continue;
      }
      const digest = sha256(normalized);
      if (upstream === null) {
        upstream = normalized;
        upstreamDigest = digest;
      } else if (digest !== upstreamDigest) {
        // A shared heading asserts the crates really do ship one notice.
        add(
          "shared_heading_texts_differ",
          notice.heading,
          `${key} does not ship the same licence text as the other crates under this heading`,
        );
        continue;
      }
      if (digest !== notice.sha256) {
        // This is the structural close on the beta.11 defect: the pinned body
        // now has to agree with the crate's own vendored file, so one crate's
        // notice can no longer stand in for another's.
        add(
          "vendored_license_mismatch",
          key,
          `vendored ${crate.license_file} is ${digest}, pinned ${notice.sha256}`,
        );
      }
    }

    if (upstream !== null) {
      reviewable.push({
        heading: notice.heading,
        notice,
        upstream,
        upstreamDigest,
        bundleBody: sections.get(notice.heading) ?? null,
      });
    }

    if (notice.holder === null) {
      if (body !== undefined && /^Copyright/mu.test(body)) {
        add(
          "invented_copyright_holder",
          notice.heading,
          "the pin records no holder but the bundled notice names one",
        );
      }
      if (upstream !== null && /^Copyright/mu.test(upstream)) {
        add(
          "unreviewed_copyright_holder",
          notice.heading,
          "the pin records no holder but the vendored licence names one",
        );
      }
    } else {
      if (body !== undefined && !body.includes(notice.holder)) {
        add("holder_absent_from_notice", notice.heading, `bundled notice omits ${notice.holder}`);
      }
      if (upstream !== null && !upstream.includes(notice.holder)) {
        add("holder_absent_upstream", notice.heading, `vendored licence omits ${notice.holder}`);
      }
    }
  }

  // Two crates not documented as sharing a notice must never carry the same
  // bytes; that is a copy, and it is how a swap hides in plain sight.
  const seenDigests = new Map();
  for (const [heading, digest] of digestsByHeading) {
    if (seenDigests.has(digest)) {
      add(
        "shared_notice_body",
        heading,
        `carries the same notice body as ${seenDigests.get(digest)}`,
      );
    } else {
      seenDigests.set(digest, heading);
    }
  }

  return { findings, reviewable, checkedCrates: pinnedByCrate.size };
}

export function formatFindings(findings) {
  return findings
    .map(({ kind, subject, detail }) => `  ${kind}: ${subject} — ${detail}`)
    .join("\n");
}

function resolveCrateRoots() {
  const result = spawnSync("cargo", [
    "metadata",
    "--locked",
    "--manifest-path",
    manifestPath,
    "--format-version",
    "1",
    "--features",
    ALL_FEATURES.join(","),
    "--filter-platform",
    TARGET,
  ], { cwd: root, encoding: "utf8", maxBuffer: 64 * 1024 * 1024 });
  if (result.status !== 0) {
    throw new Error("Cargo metadata failed while resolving vendored MIT licence files");
  }
  const metadata = JSON.parse(result.stdout);
  const roots = new Map();
  for (const candidate of metadata.packages) {
    // Cargo cannot report manifest_path without having extracted the crate, so
    // this directory is guaranteed present whenever metadata succeeds. Never
    // construct a registry path by hand and never stat-and-skip.
    roots.set(crateKey(candidate.name, candidate.version), dirname(candidate.manifest_path));
  }
  return roots;
}

export function vendoredLicenseReader(roots) {
  return async ({ name, version, licenseFile }) => {
    const crateRoot = roots.get(crateKey(name, version));
    if (crateRoot === undefined) {
      throw new Error("crate is absent from the resolved Cargo graph");
    }
    return readFile(resolve(crateRoot, licenseFile), "utf8");
  };
}

async function loadLocalInputs() {
  const [pinsText, bundleText, d5c, docx] = await Promise.all([
    readFile(noticePinsPath, "utf8"),
    readFile(noticeBundlePath, "utf8"),
    readD5cRustLicenseInventory(),
    readDocxRustLicenseInventory(),
  ]);
  return {
    pins: JSON.parse(pinsText),
    bundleText,
    inventories: [
      { label: "D5C", mitCrates: mitCratesOf(d5c) },
      { label: "DOCX", mitCrates: mitCratesOf(docx) },
    ],
  };
}

function mitCratesOf(inventory) {
  return inventory.dependencies
    .filter(({ release_license: license }) => license === "MIT")
    .map(({ name, version }) => ({ name, version }));
}

export async function verifyMitNotices() {
  const inputs = await loadLocalInputs();
  const outcome = await checkMitNotices({
    ...inputs,
    readVendoredLicense: vendoredLicenseReader(resolveCrateRoots()),
  });
  return { ...outcome, ...inputs };
}

// ---------------------------------------------------------------------------
// review — local only, writes nothing tracked, always exits non-zero.
// ---------------------------------------------------------------------------

export function unifiedDiff(before, after, beforeLabel, afterLabel) {
  const a = before === null ? [] : before.split("\n");
  const b = after.split("\n");
  const lengths = Array.from({ length: a.length + 1 }, () => new Uint32Array(b.length + 1));
  for (let i = a.length - 1; i >= 0; i -= 1) {
    for (let j = b.length - 1; j >= 0; j -= 1) {
      lengths[i][j] = a[i] === b[j]
        ? lengths[i + 1][j + 1] + 1
        : Math.max(lengths[i + 1][j], lengths[i][j + 1]);
    }
  }
  const lines = [`--- ${beforeLabel}`, `+++ ${afterLabel}`];
  let i = 0;
  let j = 0;
  while (i < a.length && j < b.length) {
    if (a[i] === b[j]) {
      lines.push(` ${a[i]}`);
      i += 1;
      j += 1;
    } else if (lengths[i + 1][j] >= lengths[i][j + 1]) {
      lines.push(`-${a[i]}`);
      i += 1;
    } else {
      lines.push(`+${b[j]}`);
      j += 1;
    }
  }
  while (i < a.length) {
    lines.push(`-${a[i]}`);
    i += 1;
  }
  while (j < b.length) {
    lines.push(`+${b[j]}`);
    j += 1;
  }
  return `${lines.join("\n")}\n`;
}

function slug(value) {
  return value.replace(/[^a-zA-Z0-9._-]+/gu, "-").replace(/^-|-$/gu, "").slice(0, 120);
}

// Discovery may propose a licence filename; only the pin record binds one.
// Crates ship LICENSE, LICENSE.md, LICENSE-MIT, LICENSE-MIT.md, LICENSE.txt and
// COPYING interchangeably, so guessing is exactly the decision a tool must not
// make on the maintainer's behalf.
export async function proposeLicenseFiles(crateRoot) {
  const entries = await readdir(crateRoot);
  return entries
    .filter((entry) => /^(?:licen[sc]e|copying|notice)/iu.test(entry))
    .sort();
}

async function runReview() {
  const { findings, reviewable, pins } = await verifyMitNotices();
  await rm(reviewRoot, { recursive: true, force: true });

  const affected = new Set(findings.map(({ subject }) => subject));
  const targets = [];
  for (const entry of reviewable) {
    const touchesHeading = affected.has(entry.heading);
    const touchesCrate = entry.notice.crates
      .some((crate) => affected.has(crateKey(crate.name, crate.version)));
    if (!touchesHeading && !touchesCrate) continue;
    targets.push({
      label: entry.heading,
      crates: entry.notice.crates.map((crate) => `${crate.name}@${crate.version}`),
      licenseFiles: [...new Set(entry.notice.crates.map((crate) => crate.license_file))],
      upstream: entry.upstream,
      token: entry.upstreamDigest,
      before: entry.bundleBody,
    });
  }

  // A version bump leaves the pin behind, so the crate the maintainer actually
  // has to review is the one with no pin at all. Resolve it, propose its licence
  // file, and diff it against whatever body the same-named crate had before.
  const roots = resolveCrateRoots();
  const unpinned = new Set(findings
    .filter(({ kind }) => kind === "unpinned_mit_crate")
    .map(({ subject }) => subject));
  for (const subject of unpinned) {
    const [name, version] = subject.split(" ");
    const crateRoot = roots.get(crateKey(name, version));
    if (crateRoot === undefined) continue;
    const previous = pins.notices.find((notice) => (
      notice.crates.some((crate) => crate.name === name)
    ));
    const proposed = await proposeLicenseFiles(crateRoot);
    const bound = previous?.crates.find((crate) => crate.name === name)?.license_file;
    const chosen = bound !== undefined && proposed.includes(bound) ? bound : proposed[0];
    if (chosen === undefined) {
      targets.push({
        label: `${name} ${version}`,
        crates: [`${name}@${version}`],
        licenseFiles: [],
        upstream: null,
        token: null,
        before: null,
        note: "this crate ships no licence file; it cannot be pinned and must not be skipped",
      });
      continue;
    }
    const upstream = normalizeLicenseText(await readFile(resolve(crateRoot, chosen), "utf8"));
    targets.push({
      label: `${name} ${version}`,
      crates: [`${name}@${version}`],
      licenseFiles: proposed,
      chosen,
      upstream,
      token: sha256(upstream),
      before: previous === null || previous === undefined
        ? null
        : extractNoticeSections(await readFile(noticeBundlePath, "utf8"))
          .sections.get(previous.heading) ?? null,
    });
  }

  if (findings.length === 0) {
    process.stdout.write("MIT notice review — nothing to review; every pin already agrees.\n");
    return;
  }

  await mkdir(reviewRoot, { recursive: true });
  process.stdout.write(`MIT notice review — ${findings.length} finding(s)\n`);
  process.stdout.write(`${formatFindings(findings)}\n`);
  for (const target of targets) {
    const name = slug(target.label);
    process.stdout.write(`\n## ${target.label}\n`);
    process.stdout.write(`  crates:   ${target.crates.join(", ")}\n`);
    if (target.note !== undefined) {
      process.stdout.write(`  ${target.note}\n`);
      continue;
    }
    process.stdout.write(`  file:     ${target.chosen ?? target.licenseFiles.join(", ")}`);
    if (target.licenseFiles.length > 1) {
      process.stdout.write(`  (crate also ships ${target.licenseFiles.join(", ")})`);
    }
    process.stdout.write("\n");
    const upstreamPath = resolve(reviewRoot, `${name}.upstream.txt`);
    const diffPath = resolve(reviewRoot, `${name}.diff`);
    await writeFile(upstreamPath, target.upstream, "utf8");
    await writeFile(
      diffPath,
      unifiedDiff(target.before, target.upstream, "bundled notice body", "vendored upstream"),
      "utf8",
    );
    process.stdout.write(`  upstream: ${upstreamPath}\n`);
    process.stdout.write(`  diff:     ${diffPath}\n`);
    process.stdout.write(`  accept with: --reviewed ${target.token}\n`);
  }
  process.stdout.write(
    "\nReview is not acceptance. Read the diff, then re-run with `apply` and the token above.\n",
  );
  process.exitCode = 1;
}

// ---------------------------------------------------------------------------
// apply — writes only against a token naming the exact reviewed bytes.
// ---------------------------------------------------------------------------

function parseArguments(argv) {
  const options = new Map();
  for (let index = 0; index < argv.length; index += 1) {
    const token = argv[index];
    if (!token.startsWith("--")) throw new Error(`unexpected argument: ${token}`);
    const key = token.slice(2);
    if (key === "holder-none") {
      options.set(key, true);
      continue;
    }
    const value = argv[index + 1];
    if (value === undefined || value.startsWith("--")) {
      throw new Error(`missing value for --${key}`);
    }
    options.set(key, value);
    index += 1;
  }
  return options;
}

function replaceSection(bundleText, heading, nextHeading, nextBody) {
  const marker = `\n## ${heading}\n`;
  const start = bundleText.indexOf(marker);
  if (start === -1) throw new Error(`the notice bundle has no section titled ${heading}`);
  const after = start + marker.length;
  const nextSection = bundleText.indexOf("\n## ", after);
  const end = nextSection === -1 ? bundleText.length : nextSection + 1;
  const section = bundleText.slice(after, end);
  const fenceStart = section.indexOf(`${FENCE}text\n`);
  if (fenceStart === -1) throw new Error(`the ${heading} section has no notice body`);
  const bodyStart = fenceStart + `${FENCE}text\n`.length;
  const fenceEnd = section.indexOf(FENCE, bodyStart);
  if (fenceEnd === -1) throw new Error(`the ${heading} section has an unterminated notice body`);
  // Prose between the heading and the fence is the maintainer's, not the tool's.
  const rewritten = section.slice(0, bodyStart) + nextBody + section.slice(fenceEnd);
  return `${bundleText.slice(0, start)}\n## ${nextHeading}\n${rewritten}${bundleText.slice(end)}`;
}

function insertSection(bundleText, pins, heading, body) {
  const known = new Set(pins.notices.map((notice) => notice.heading));
  const headings = [...bundleText.matchAll(/^## (.+)$/gmu)];
  const anchor = headings.find(({ 1: text }, index) => index > 0 && !known.has(text));
  const section = `## ${heading}\n\n${FENCE}text\n${body}${FENCE}\n\n`;
  if (anchor === undefined) return `${bundleText.replace(/\n*$/u, "\n\n")}${section}`.replace(/\n+$/u, "\n");
  return bundleText.slice(0, anchor.index) + section + bundleText.slice(anchor.index);
}

async function runApply(argv) {
  const options = parseArguments(argv);
  const target = options.get("crate");
  const reviewed = options.get("reviewed");
  if (typeof target !== "string" || !/^[^@]+@[^@]+$/u.test(target)) {
    throw new Error("apply requires --crate <name>@<version>");
  }
  if (typeof reviewed !== "string" || !/^[0-9a-f]{64}$/u.test(reviewed)) {
    throw new Error("apply requires --reviewed <sha256> naming the bytes you read in review");
  }
  const [name, version] = target.split("@");

  const pinsText = await readFile(noticePinsPath, "utf8");
  const pins = validateNoticePins(JSON.parse(pinsText));
  let bundleText = await readFile(noticeBundlePath, "utf8");

  let notice = pins.notices.find((entry) => entry.crates.some((crate) => crate.name === name));
  const created = notice === undefined;
  if (created) {
    const heading = options.get("heading");
    const licenseFile = options.get("license-file");
    if (typeof heading !== "string" || typeof licenseFile !== "string") {
      throw new Error(
        `${name} has no pinned notice; adding one requires --heading and --license-file`,
      );
    }
    notice = {
      heading,
      crates: [{ name, version, license_file: licenseFile }],
      sha256: "",
      holder: null,
      reviewed_by: options.get("reviewed-by") ?? "owner",
      reviewed_at: options.get("reviewed-at") ?? today(),
    };
    pins.notices.push(notice);
    pins.notices.sort((left, right) => (left.heading < right.heading ? -1 : 1));
  }

  const previousHeading = notice.heading;
  const crate = notice.crates.find((entry) => entry.name === name);
  const previousVersion = crate.version;
  crate.version = version;
  if (options.has("license-file")) crate.license_file = options.get("license-file");

  // A shared notice's heading names every member and their versions, and only
  // the maintainer can rewrite it: nothing here knows what the other members
  // are called in prose. So a version move on a shared notice must be refused
  // unless `--heading` supplies the new one.
  //
  // Guarding on `nextHeading !== previousHeading` — as this did — can never
  // fire: without `--heading`, a shared notice falls back to `previousHeading`
  // by the very expression the comparison reads, so the two sides are equal by
  // construction. The check that guards anything is the one on the version
  // that actually moved.
  if (previousVersion !== version && notice.crates.length > 1 && !options.has("heading")) {
    throw new Error("a shared notice needs an explicit --heading when a member version changes");
  }
  const nextHeading = options.get("heading")
    ?? (notice.crates.length === 1 ? `${name} ${version} — MIT` : previousHeading);

  const roots = resolveCrateRoots();
  const read = vendoredLicenseReader(roots);
  let body = null;
  for (const entry of notice.crates) {
    const normalized = normalizeLicenseText(await read({
      name: entry.name,
      version: entry.version,
      licenseFile: entry.license_file,
    }));
    if (normalized.includes(FENCE)) {
      throw new Error(`${entry.name} ${entry.version} licence text contains a fenced-code delimiter`);
    }
    if (body === null) body = normalized;
    else if (body !== normalized) {
      throw new Error(`${entry.name} ${entry.version} does not share the notice under ${nextHeading}`);
    }
  }

  const digest = sha256(body);
  if (digest !== reviewed) {
    // The token names bytes, not a crate. If the text moved since review, the
    // maintainer has not seen what would be written.
    throw new Error(
      `--reviewed ${reviewed} does not name the vendored licence text (${digest}); re-run review`,
    );
  }

  if (options.get("holder-none") === true) {
    notice.holder = null;
  } else if (options.has("holder")) {
    notice.holder = options.get("holder");
  }
  if (notice.holder === null) {
    if (/^Copyright/mu.test(body)) {
      throw new Error(
        `${nextHeading} names a copyright holder upstream; pass --holder "<line>" to record it`,
      );
    }
  } else if (!body.includes(notice.holder)) {
    throw new Error(
      `${nextHeading} no longer contains the recorded holder; pass --holder or --holder-none`,
    );
  }

  notice.heading = nextHeading;
  notice.sha256 = digest;
  notice.reviewed_by = options.get("reviewed-by") ?? notice.reviewed_by;
  notice.reviewed_at = options.get("reviewed-at") ?? today();

  bundleText = created
    ? insertSection(bundleText, pins, nextHeading, body)
    : replaceSection(bundleText, previousHeading, nextHeading, body);

  await writeFile(noticeBundlePath, bundleText, "utf8");
  await writeFile(noticePinsPath, `${JSON.stringify(pins, null, 2)}\n`, "utf8");
  await restampNoticeBundleDigest(bundleText);

  process.stdout.write(`${JSON.stringify({
    applied: `${name}@${version}`,
    heading: nextHeading,
    sha256: digest,
    holder: notice.holder,
    created,
  })}\n`);
  process.stdout.write(
    "Re-run `npm run verify:d5c:licenses`, `npm run verify:docx:licenses` and"
    + " `npm run verify:mit:notices`.\n",
  );
}

async function restampNoticeBundleDigest(bundleText) {
  const digest = createHash("sha256").update(bundleText, "utf8").digest("hex");
  for (const name of ["d5c-rust-license-inventory.json", "docx-rust-license-inventory.json"]) {
    const path = resolve(root, name);
    const inventory = JSON.parse(await readFile(path, "utf8"));
    inventory.notice_bundle_sha256 = digest;
    await writeFile(path, `${JSON.stringify(inventory, null, 2)}\n`, "utf8");
  }
}

function today() {
  return new Date().toISOString().slice(0, 10);
}

function isRecord(value) {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function hasExactKeys(value, keys) {
  const actual = Object.keys(value);
  return actual.length === keys.length && actual.every((key) => keys.includes(key));
}

function isBoundedString(value, maximum) {
  return typeof value === "string" && value.length > 0 && value.length <= maximum;
}

if (process.argv[1] === fileURLToPath(import.meta.url)) {
  const mode = process.argv[2] ?? "check";
  if (mode === "review") {
    await runReview();
  } else if (mode === "apply") {
    await runApply(process.argv.slice(3));
  } else if (mode === "check") {
    const { findings, checkedCrates, pins } = await verifyMitNotices();
    if (findings.length > 0) {
      process.stderr.write(`MIT notice verification failed:\n${formatFindings(findings)}\n`);
      process.stderr.write("Run `npm run verify:mit:notices -- review` to see the difference.\n");
      process.exitCode = 1;
    } else {
      process.stdout.write(`${JSON.stringify({
        notices: pins.notices.length,
        crates: checkedCrates,
      })}\n`);
    }
  } else {
    throw new Error(`unknown mode: ${mode}`);
  }
}
