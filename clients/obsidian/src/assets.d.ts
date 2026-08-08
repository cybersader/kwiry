// SPDX-FileCopyrightText: 2026 cybersader
// SPDX-License-Identifier: GPL-3.0-only

declare const __KWIRY_D5C_OWNER_WORKER__: boolean;

declare module "virtual:kwiry-worker-source" {
  const source: string;
  export default source;
}

declare module "virtual:kwiry-internal-prototype" {
  export function createInternalPrototypeHandler(context: {
    scope: DedicatedWorkerGlobalScope;
    getActive(): { id: string; index: import("./worker/fts5-index").Fts5GenerationIndex } | null;
    requireInitialized(): void;
    search(query: string, limit: number): import("./worker/protocol").SearchResult;
    getLastRequestId(): number;
    setLastRequestId(id: number): void;
    mapError(error: unknown): import("./worker/protocol").WorkerError;
  }): (value: unknown) => Promise<boolean>;
}

declare module "virtual:kwiry-owner-worker-protocol" {
  export {
    isD5cOwnerWorkerOperation,
    parseD5cOwnerWorkerRequest,
    type D5cOwnerWorkerOperation,
    type D5cOwnerWorkerRequest,
    type D5cOwnerWorkerResponse,
  } from "./worker/d5c-owner-protocol";
}

declare module "virtual:kwiry-internal-d5c-preview" {
  export function createInternalD5cPreviewHandler(context: {
    scope: DedicatedWorkerGlobalScope;
    resolveTarget(generation: string, revision: number | null): {
      id: string;
      index: import("./worker/fts5-index").Fts5GenerationIndex;
      publication: "active" | "initial_staging";
      revision: number | null;
    } | null;
    getInitializedVaultId(): string;
    requireInitialized(): void;
    getLastRequestId(): number;
    setLastRequestId(id: number): void;
    mapError(error: unknown): import("./worker/protocol").WorkerError;
  }): (value: unknown) => Promise<boolean>;
}

declare module "virtual:kwiry-rust-wasm-bindings" {
  export function initSync(options: { module: BufferSource | WebAssembly.Module }): unknown;
  export function abi_identity(): string;
  export function prepare_source(requestJson: string, sourceBytes: Uint8Array): string;
  export function prepare_oversized_source(requestJson: string): string;
  export function prepare_query(requestJson: string): string;
  export function finalize_query(requestJson: string): string;
  export function prepare_typo_suggestion_probe(requestJson: string): string;
  export function finalize_typo_suggestion_probe(requestJson: string): string;
  export function prepare_d5c_preview(requestJson: string): string;
  export function finalize_d5c_preview(requestJson: string): string;
  export function internal_d5c_evaluate(requestJson: string): string;
}

declare module "virtual:kwiry-rust-wasm-bytes" {
  const bytes: Uint8Array;
  export default bytes;
}

declare module "virtual:kwiry-artifact-identities" {
  export const RUST_WASM_SIZE: number;
  export const RUST_WASM_SHA256: string;
  export const SQLITE_WASM_SIZE: number;
  export const SQLITE_WASM_SHA256: string;
  export const PLUGIN_ID: string;
  export const PLUGIN_VERSION: string;
}

declare module "@sqlite.org/sqlite-wasm/sqlite3.wasm" {
  const bytes: Uint8Array;
  export default bytes;
}
