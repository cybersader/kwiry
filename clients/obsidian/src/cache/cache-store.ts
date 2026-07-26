// SPDX-FileCopyrightText: 2026 cybersader
// SPDX-License-Identifier: GPL-3.0-only
//
// The main-thread cache port. The Worker is persistence-blind: it produces a
// sealed image plus the identity envelope that describes it, and this port is
// the only thing that touches a disk. Nothing here derives, defaults, or
// migrates a fact the Worker authored — the envelope is stored verbatim and
// compared by exact equality.
//
// This module must stay free of "fs" and of any import from "obsidian" so the
// contract, its bounds, and its validators are testable without either.

import { MAX_EXPORT_BLOB_BYTES } from "../worker/protocol";

export const CACHE_POINTER_VERSION = 1 as const;

/**
 * The store's ceiling IS the protocol's ceiling, by construction rather than by
 * coincidence. Written as two independent literals, a bump to one would leave
 * the other silently rejecting valid exports as `invalid_blob`.
 */
export const MAX_CACHE_BLOB_BYTES = MAX_EXPORT_BLOB_BYTES;
export const MAX_POINTER_BYTES = 8 * 1024;
export const MAX_RETAINED_GENERATIONS = 2 as const;
export const CACHE_IMAGE_EXTENSION = ".kwc";

/**
 * Worker-issued generation identifiers only. TypeScript validates and never
 * mints one. The bound matches the protocol's `MAX_GENERATION_CHARACTERS`, and
 * the shape rejects "..", "/", "\", a leading dot, and the empty string, which
 * is what makes a generation id safe to use as a path component.
 */
export const GENERATION_ID_PATTERN = /^[A-Za-z0-9][A-Za-z0-9._-]{0,127}$/u;
export const SHA256_HEX_PATTERN = /^[0-9a-f]{64}$/u;

/**
 * The Worker's export identity, stored verbatim. Field names match the Worker
 * result exactly so no renaming layer can silently re-author one of them.
 */
export interface CacheIdentityEnvelope {
  readonly protocol_version: number;
  readonly cache_schema_version: number;
  readonly chunking_version: number;
  readonly sqlite_version: string;
  readonly sqlite_wasm_sha256: string;
  readonly rust_wasm_sha256: string;
  readonly plugin_id: string;
  readonly plugin_version: string;
  readonly cache_identity: string;
}

export const CACHE_IDENTITY_KEYS: readonly (keyof CacheIdentityEnvelope)[] = [
  "protocol_version",
  "cache_schema_version",
  "chunking_version",
  "sqlite_version",
  "sqlite_wasm_sha256",
  "rust_wasm_sha256",
  "plugin_id",
  "plugin_version",
  "cache_identity",
];

export interface CacheRecord {
  readonly generationId: string;
  readonly byteLength: number;
  /** Worker-computed. The store records and returns it; it never computes it. */
  readonly sha256: string;
  readonly identity: CacheIdentityEnvelope;
}

export interface CacheWrite extends CacheRecord {
  /** Transferred from the Worker. The store keeps no reference after `put`. */
  readonly bytes: Uint8Array;
}

export type CacheMissReason =
  | "absent"
  | "pointer_unreadable"
  | "pointer_corrupt"
  | "identity_mismatch"
  | "image_absent"
  | "image_unreadable"
  | "image_length_mismatch";

/**
 * `digestVerified` is a literal `false`, not a boolean, and it is REQUIRED.
 *
 * The store checks the image's length against the pointer and nothing more; the
 * recorded `sha256` is carried through unverified, because a multi-hundred-
 * megabyte main-thread digest would stall Obsidian. That obligation belongs in
 * the type rather than in a comment: a restore consumer cannot destructure a
 * hit without meeting the field, so it cannot accidentally compile as a
 * trusting reader of bytes whose digest nobody checked.
 */
export type CacheLoad =
  | {
      readonly kind: "hit";
      readonly record: CacheRecord;
      readonly bytes: Uint8Array;
      readonly digestVerified: false;
    }
  | { readonly kind: "miss"; readonly reason: CacheMissReason };

export type CacheStoreUnavailableReason =
  | "unsupported_platform"
  | "no_machine_local_root"
  | "root_not_absolute"
  | "root_not_machine_local"
  | "root_inside_vault"
  | "root_not_a_directory"
  | "root_not_writable"
  | "root_probe_failed"
  | "vault_location_unavailable";

export type CacheStoreErrorCode =
  | "invalid_generation_id"
  | "invalid_identity"
  | "invalid_blob"
  | "write_failed"
  | "locked"
  | "disposed";

export class CacheStoreError extends Error {
  constructor(
    readonly code: CacheStoreErrorCode,
    message: string,
  ) {
    super(message);
    this.name = "CacheStoreError";
  }
}

export interface CacheStorePort {
  /** The opaque vault identity this store is scoped to. Never a path. */
  readonly vaultCacheIdentity: string;
  /**
   * Reads the pointed-to generation WITHOUT verifying its digest. The image is
   * length-checked against the pointer and the Worker-recorded `sha256` is
   * returned alongside the bytes; verifying it is the caller's obligation and
   * belongs off the main thread. A hit therefore carries `digestVerified:
   * false`, which no consumer can ignore by accident.
   */
  load(): Promise<CacheLoad>;
  put(write: CacheWrite): Promise<CacheRecord>;
  discard(reason: "corrupt" | "incompatible" | "requested"): Promise<void>;
  dispose(): Promise<void>;
}

export type CacheStoreAvailability =
  | { readonly kind: "available"; readonly store: CacheStorePort }
  | { readonly kind: "unavailable"; readonly reason: CacheStoreUnavailableReason };

export function isGenerationId(value: unknown): value is string {
  return typeof value === "string" && GENERATION_ID_PATTERN.test(value);
}

export function isSha256Hex(value: unknown): value is string {
  return typeof value === "string" && SHA256_HEX_PATTERN.test(value);
}

/**
 * Exact envelope validation. An unknown key, a missing key, a malformed digest
 * or a non-integer version is a refusal — never a defaulted or dropped field.
 */
export function isCacheIdentityEnvelope(value: unknown): value is CacheIdentityEnvelope {
  if (!isRecord(value)) return false;
  const keys = Object.keys(value);
  if (keys.length !== CACHE_IDENTITY_KEYS.length) return false;
  if (!keys.every((key) => (CACHE_IDENTITY_KEYS as readonly string[]).includes(key))) return false;
  return isNonNegativeSafeInteger(value.protocol_version)
    && isNonNegativeSafeInteger(value.cache_schema_version)
    && isNonNegativeSafeInteger(value.chunking_version)
    && isBoundedString(value.sqlite_version, 64)
    && isSha256Hex(value.sqlite_wasm_sha256)
    && isSha256Hex(value.rust_wasm_sha256)
    && isBoundedString(value.plugin_id, 128)
    && isBoundedString(value.plugin_version, 64)
    && isSha256Hex(value.cache_identity);
}

/** The image filename a pointer is allowed to name, derived rather than parsed. */
export function imageFileName(generationId: string): string {
  return `${generationId}${CACHE_IMAGE_EXTENSION}`;
}

export function imageRelativePath(generationId: string): string {
  return `generations/${imageFileName(generationId)}`;
}

export function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

export function isNonNegativeSafeInteger(value: unknown): value is number {
  return typeof value === "number" && Number.isSafeInteger(value) && value >= 0;
}

export function isBoundedString(value: unknown, maximum: number): value is string {
  return typeof value === "string" && value.length > 0 && value.length <= maximum;
}
