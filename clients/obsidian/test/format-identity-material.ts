// SPDX-FileCopyrightText: 2026 cybersader
// SPDX-License-Identifier: GPL-3.0-only

/**
 * The per-format identity derivation, spelled out in TypeScript.
 *
 * `src/source-formats.ts` cannot host this: `main.ts` needs the identities
 * during `onload()`, before the worker and the WASM module exist, and hashing
 * there is asynchronous. So the plugin ships the seven digests as literals — and
 * a literal that nothing checks is a claim, not a fact.
 *
 * Two things checked those literals before, and neither could catch the case
 * that matters here. `typescript_mirror.rs` compares them against the adapter,
 * which needs a WASM build; `settings.test.ts` compared them against a second
 * hand-copy of the same seven strings, which catches nothing a careful
 * copy-paste would not also get wrong twice. Neither states the *material*, so
 * neither could tell an identity built from a bumped extractor version apart
 * from one built from anything else.
 *
 * This module states the material. It is the exact byte layout of
 * `kwiry_core::policy::identity_for_material`: a domain separator, then four
 * length-prefixed components, each prefixed with its length as a little-endian
 * `u64`. Test-only on purpose — nothing in the plugin may derive an identity at
 * runtime, because a row's identity has to come from the compiled map or the
 * eviction predicate has nothing independent to compare against.
 */
import { createHash } from "node:crypto";

import {
  EXTRACTION_PROFILES,
  EXTRACTOR_VERSIONS,
  FORMAT_IDENTITY_SCHEMA_VERSION,
  type ExtractionProfile,
  type SourceFormat,
} from "../src/source-formats";

/** `kwiry_core::policy::update_component`: an 8-byte little-endian length, then the bytes. */
function lengthPrefixed(value: string): Buffer {
  const bytes = Buffer.from(value, "utf8");
  const prefix = Buffer.alloc(8);
  prefix.writeBigUInt64LE(BigInt(bytes.length));
  return Buffer.concat([prefix, bytes]);
}

/**
 * The identity digest of *stated* material, with nothing implicit — the mirror
 * of `kwiry_core::policy::identity_for_material`.
 *
 * Takes the version as an argument rather than reading `EXTRACTOR_VERSIONS`, so
 * a test can name the digest a bumped build would stamp.
 */
export function identityForMaterial(
  format: string,
  profile: ExtractionProfile,
  extractorVersion: number,
): string {
  return createHash("sha256")
    .update(Buffer.from("kwiry-format-identity-v1\0", "utf8"))
    .update(lengthPrefixed(String(FORMAT_IDENTITY_SCHEMA_VERSION)))
    .update(lengthPrefixed(`format=${format}`))
    .update(lengthPrefixed(`profile=${profile}`))
    .update(lengthPrefixed(`extractor=${extractorVersion}`))
    .digest("hex");
}

/**
 * The identity `format` would carry if its extractor version were `version` and
 * nothing else about this build had changed.
 *
 * `identityAtExtractorVersion(f, EXTRACTOR_VERSIONS[f])` is the identity this
 * build stamps; every other argument names a build whose extractor for that
 * format states different output for the same bytes.
 */
export function identityAtExtractorVersion(format: SourceFormat, version: number): string {
  return identityForMaterial(format, EXTRACTION_PROFILES[format], version);
}

/** The identity of the *next* version of `format`'s extractor. */
export function identityAfterBump(format: SourceFormat): string {
  return identityAtExtractorVersion(format, EXTRACTOR_VERSIONS[format] + 1);
}
