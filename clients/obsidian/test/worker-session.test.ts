// SPDX-FileCopyrightText: 2026 cybersader
// SPDX-License-Identifier: GPL-3.0-only

import { describe, expect, it, vi } from "vitest";

import {
  CACHE_SCHEMA_VERSION,
  INITIAL_BUILD_CHECKPOINT_IMAGE_VERSION,
  INITIAL_BUILD_CHECKPOINT_RECORD_KIND,
  INITIAL_BUILD_CHECKPOINT_RECORD_VERSION,
  WORKER_PROTOCOL_VERSION,
  emptyRestoreEvictionReport,
  emptySourceFormatCounts,
  type WorkerRequest,
  type WorkerResult,
} from "../src/worker/protocol";
import type { IndexWorkerPort } from "../src/backends/in-plugin-index-controller";
import type { WorkerLike } from "../src/worker/rpc-client";
import { InPluginWorkerSession } from "../src/worker/session";

const SOURCE_POLICY_HASH = "d".repeat(64);

class MockWorker implements WorkerLike {
  terminate = vi.fn();
  readonly posted: WorkerRequest[] = [];
  readonly heldOperations = new Set<WorkerRequest["operation"]>();
  private readonly listeners = new Map<string, Set<(event: never) => void>>();

  postMessage(message: WorkerRequest): void {
    this.posted.push(message);
    if (this.heldOperations.has(message.operation)) return;
    queueMicrotask(() => this.respond(message));
  }

  respond(message: WorkerRequest): void {
    const result = resultFor(message);
    for (const listener of this.listeners.get("message") ?? []) {
      listener({
        data: {
          version: WORKER_PROTOCOL_VERSION,
          id: message.id,
          operation: message.operation,
          ok: true,
          result,
        },
      } as never);
    }
  }

  addEventListener(type: "message" | "error" | "messageerror", listener: (event: never) => void): void {
    const listeners = this.listeners.get(type) ?? new Set();
    listeners.add(listener);
    this.listeners.set(type, listeners);
  }

  removeEventListener(type: "message" | "error" | "messageerror", listener: (event: never) => void): void {
    this.listeners.get(type)?.delete(listener);
  }
}

function resultFor(message: WorkerRequest): WorkerResult {
  switch (message.operation) {
    case "initialize":
      return {
        rustAbiVersion: 3,
        sourceSchemaVersion: 9,
        querySchemaVersion: 6,
        matchPlanSchemaVersion: 5,
        sqliteVersion: "3.53.0",
        fts5Enabled: 1,
      };
    case "begin_build":
    case "add_source_batch":
    case "commit_build":
    case "abort_build":
      return {
        generation: message.generation,
        documents: 0,
        chunks: 0,
        database_bytes: 0,
        database_byte_limit: 1,
        quarantined_sources: 0,
        quarantine_fields: [],
        source_format_counts: emptySourceFormatCounts(),
      };
    case "restore_generation":
      return {
        generation: message.generation,
        documents: 0,
        chunks: 0,
        database_bytes: 0,
        database_byte_limit: 1,
        quarantined_sources: 0,
        quarantine_fields: [],
        source_format_counts: emptySourceFormatCounts(),
        evictions: emptyRestoreEvictionReport(),
      };
    case "restore_initial_build_checkpoint":
      return {
        generation: message.generation,
        documents: 0,
        chunks: 0,
        database_bytes: 0,
        database_byte_limit: 1,
        quarantined_sources: 0,
        quarantine_fields: [],
        source_format_counts: emptySourceFormatCounts(),
        record_kind: INITIAL_BUILD_CHECKPOINT_RECORD_KIND,
        publication: "initial_staging",
        searchable: false,
        cursor: message.cursor,
        evictions: emptyRestoreEvictionReport(),
      };
    case "apply_source_changes":
      return {
        generation: message.next_generation ?? message.generation,
        documents: message.upserts.length,
        chunks: message.upserts.length,
        database_bytes: 0,
        database_byte_limit: 1,
        quarantined_sources: 0,
        quarantine_fields: [],
        source_format_counts: emptySourceFormatCounts(),
      };
    case "plan_reconciliation":
      return {
        generation: message.generation,
        unchanged: [],
        audit: [],
        refresh: message.current_sources.map((source) => source.path),
        remove: [],
        stored_source_count: 0,
        matched_source_count: 0,
      };
    case "plan_initial_build_checkpoint_reconciliation":
      return {
        generation: message.generation,
        publication: "initial_staging",
        searchable: false,
        unchanged: [],
        audit: [],
        refresh: message.current_sources.map((source) => source.path),
        remove: [],
        stored_source_count: 0,
        matched_source_count: 0,
      };
    case "export_generation":
      return {
        generation: message.generation,
        documents: 1,
        chunks: 1,
        bytes: new Uint8Array([1, 2, 3]),
        blob_byte_length: 3,
        blob_sha256: "a".repeat(64),
        protocol_version: WORKER_PROTOCOL_VERSION,
        cache_schema_version: CACHE_SCHEMA_VERSION,
        chunking_version: 1,
        sqlite_version: "3.53.0",
        sqlite_wasm_sha256: "b".repeat(64),
        rust_wasm_sha256: "c".repeat(64),
        plugin_id: "kwiry-search",
        plugin_version: "0.1.0",
        cache_identity: message.cache_identity,
        source_policy_hash: SOURCE_POLICY_HASH,
      };
    case "export_initial_build_checkpoint":
      return {
        generation: message.generation,
        documents: 0,
        chunks: 0,
        database_bytes: 1,
        database_byte_limit: 2,
        quarantined_sources: 0,
        quarantine_fields: [],
        source_format_counts: emptySourceFormatCounts(),
        record_kind: INITIAL_BUILD_CHECKPOINT_RECORD_KIND,
        checkpoint_record_version: INITIAL_BUILD_CHECKPOINT_RECORD_VERSION,
        checkpoint_image_version: INITIAL_BUILD_CHECKPOINT_IMAGE_VERSION,
        publication: "initial_staging",
        searchable: false,
        cursor: message.cursor,
        bytes: new Uint8Array([1, 2, 3]),
        blob_byte_length: 3,
        blob_sha256: "a".repeat(64),
        protocol_version: WORKER_PROTOCOL_VERSION,
        cache_schema_version: CACHE_SCHEMA_VERSION,
        chunking_version: 1,
        sqlite_version: "3.53.0",
        sqlite_wasm_sha256: "b".repeat(64),
        rust_wasm_sha256: "c".repeat(64),
        plugin_id: "kwiry-search",
        plugin_version: "0.1.0",
        cache_identity: message.cache_identity,
        source_policy_hash: SOURCE_POLICY_HASH,
      };
    case "search":
      return {
        generation: "g1",
        hits: [],
        candidate_window: {
          state: "unknown",
          candidate_count: 0,
          candidate_limit: 512,
        },
      };
    case "status":
      return {
        phase: "ready",
        searchable: true,
        active_generation: "g1",
        staging_generation: null,
        documents: 0,
        chunks: 0,
        active_database_bytes: 0,
        staging_database_bytes: 0,
        database_byte_limit: 1,
        source_format_counts: emptySourceFormatCounts(),
        dirty: false,
        rebuilding: false,
      };
    case "dispose":
      return { closed: true };
  }
}

describe("InPluginWorkerSession", () => {
  it("serializes active and staging source changes", async () => {
    const worker = new MockWorker();
    const session = new InPluginWorkerSession(worker, "blob:kwiry", vi.fn(), 1_000);
    const port: IndexWorkerPort = session;
    expect(port).toBe(session);

    await expect(session.applySourceChanges(
      "g1",
      "g2",
      [],
      [{ vault_id: "active", path: "old.md" }],
    )).resolves.toMatchObject({ generation: "g2" });
    await expect(session.applySourceChanges(
      "g3",
      null,
      [],
      [{ vault_id: "active", path: "old.md" }],
    )).resolves.toMatchObject({ generation: "g3" });

    expect(worker.posted).toEqual([
      {
        version: WORKER_PROTOCOL_VERSION,
        id: 1,
        operation: "apply_source_changes",
        generation: "g1",
        next_generation: "g2",
        upserts: [],
        removals: [{ vault_id: "active", path: "old.md" }],
      },
      {
        version: WORKER_PROTOCOL_VERSION,
        id: 2,
        operation: "apply_source_changes",
        generation: "g3",
        next_generation: null,
        upserts: [],
        removals: [{ vault_id: "active", path: "old.md" }],
      },
    ]);
  });

  // The Worker never learns the vault path: the host derives an opaque
  // identity and the session forwards only that digest.
  it("posts an exact export request carrying only the opaque cache identity", async () => {
    const worker = new MockWorker();
    const session = new InPluginWorkerSession(worker, "blob:kwiry", vi.fn(), 1_000);
    const identity = "9".repeat(64);

    await expect(session.exportGeneration("g1", identity)).resolves.toMatchObject({
      generation: "g1",
      cache_identity: identity,
    });

    expect(worker.posted).toEqual([{
      version: WORKER_PROTOCOL_VERSION,
      id: 1,
      operation: "export_generation",
      generation: "g1",
      cache_identity: identity,
    }]);
  });

  it("maps a B6.2 unverified hit into the exact restore request", async () => {
    const worker = new MockWorker();
    const session = new InPluginWorkerSession(worker, "blob:kwiry", vi.fn(), 1_000);
    const identity = "8".repeat(64);
    const bytes = new Uint8Array([1, 2, 3]);
    await expect(session.restoreGeneration({
      kind: "hit",
      record: {
        generationId: "g1",
        byteLength: 3,
        sha256: "a".repeat(64),
        identity: {
          protocol_version: WORKER_PROTOCOL_VERSION,
          cache_schema_version: CACHE_SCHEMA_VERSION,
          chunking_version: 1,
          sqlite_version: "3.53.0",
          sqlite_wasm_sha256: "b".repeat(64),
          rust_wasm_sha256: "c".repeat(64),
          plugin_id: "kwiry-search",
          plugin_version: "0.1.0",
          cache_identity: identity,
          source_policy_hash: SOURCE_POLICY_HASH,
        },
      },
      bytes,
      digestVerified: false,
    }, identity, SOURCE_POLICY_HASH)).resolves.toMatchObject({ generation: "g1" });

    expect(worker.posted[0]).toEqual({
      version: WORKER_PROTOCOL_VERSION,
      id: 1,
      operation: "restore_generation",
      generation: "g1",
      bytes,
      blob_byte_length: 3,
      blob_sha256: "a".repeat(64),
      digest_verified: false,
      protocol_version: WORKER_PROTOCOL_VERSION,
      cache_schema_version: CACHE_SCHEMA_VERSION,
      chunking_version: 1,
      sqlite_version: "3.53.0",
      sqlite_wasm_sha256: "b".repeat(64),
      rust_wasm_sha256: "c".repeat(64),
      plugin_id: "kwiry-search",
      plugin_version: "0.1.0",
      cache_identity: identity,
      source_policy_hash: SOURCE_POLICY_HASH,
      expected_cache_identity: identity,
      expected_source_policy_hash: SOURCE_POLICY_HASH,
    });
  });

  it("posts an exact bounded reconciliation snapshot", async () => {
    const worker = new MockWorker();
    const session = new InPluginWorkerSession(worker, "blob:kwiry", vi.fn(), 1_000);
    const current = [{
      path: "note.md",
      byte_length: 4,
      mtime_nanos: "1000000",
      indexable: true,
    }];

    await expect(session.planReconciliation("g1", "active-vault", current)).resolves.toMatchObject({
      generation: "g1",
    });
    expect(worker.posted).toEqual([{
      version: WORKER_PROTOCOL_VERSION,
      id: 1,
      operation: "plan_reconciliation",
      generation: "g1",
      vault_id: "active-vault",
      current_sources: current,
    }]);
  });

  it("posts exact initial-build checkpoint export, restore, and planning requests", async () => {
    const worker = new MockWorker();
    const session = new InPluginWorkerSession(worker, "blob:kwiry", vi.fn(), 1_000);
    const identity = "7".repeat(64);
    const cursor = {
      snapshot_source_count: 2,
      acknowledged_add_batches: 1,
      acknowledged_prefix_sources: 1,
      last_acknowledged_path: "alpha.md",
    };

    await expect(session.exportInitialBuildCheckpoint("g1", identity, cursor)).resolves.toMatchObject({
      record_kind: INITIAL_BUILD_CHECKPOINT_RECORD_KIND,
      publication: "initial_staging",
      searchable: false,
      cursor,
    });
    const bytes = new Uint8Array([1, 2, 3]);
    await expect(session.restoreInitialBuildCheckpoint({
      kind: "hit",
      record: {
        recordKind: INITIAL_BUILD_CHECKPOINT_RECORD_KIND,
        recordVersion: INITIAL_BUILD_CHECKPOINT_RECORD_VERSION,
        imageVersion: INITIAL_BUILD_CHECKPOINT_IMAGE_VERSION,
        orderingVersion: 1,
        generationId: "g1",
        byteLength: 3,
        sha256: "a".repeat(64),
        identity: {
          protocol_version: WORKER_PROTOCOL_VERSION,
          cache_schema_version: CACHE_SCHEMA_VERSION,
          chunking_version: 1,
          sqlite_version: "3.53.0",
          sqlite_wasm_sha256: "b".repeat(64),
          rust_wasm_sha256: "c".repeat(64),
          plugin_id: "kwiry-search",
          plugin_version: "0.1.0",
          cache_identity: identity,
          source_policy_hash: SOURCE_POLICY_HASH,
        },
        cursor,
      },
      bytes,
      digestVerified: false,
    }, identity, SOURCE_POLICY_HASH)).resolves.toMatchObject({
      record_kind: INITIAL_BUILD_CHECKPOINT_RECORD_KIND,
      publication: "initial_staging",
      searchable: false,
    });
    const current = [{
      path: "alpha.md",
      byte_length: 4,
      mtime_nanos: "1000000",
      indexable: true,
    }];
    await expect(session.planInitialBuildCheckpointReconciliation(
      "g1",
      "active-vault",
      current,
    )).resolves.toMatchObject({ publication: "initial_staging", searchable: false });

    expect(worker.posted).toEqual([
      {
        version: WORKER_PROTOCOL_VERSION,
        id: 1,
        operation: "export_initial_build_checkpoint",
        generation: "g1",
        cache_identity: identity,
        cursor,
      },
      {
        version: WORKER_PROTOCOL_VERSION,
        id: 2,
        operation: "restore_initial_build_checkpoint",
        record_kind: INITIAL_BUILD_CHECKPOINT_RECORD_KIND,
        checkpoint_record_version: INITIAL_BUILD_CHECKPOINT_RECORD_VERSION,
        checkpoint_image_version: INITIAL_BUILD_CHECKPOINT_IMAGE_VERSION,
        generation: "g1",
        cursor,
        bytes,
        blob_byte_length: 3,
        blob_sha256: "a".repeat(64),
        digest_verified: false,
        protocol_version: WORKER_PROTOCOL_VERSION,
        cache_schema_version: CACHE_SCHEMA_VERSION,
        chunking_version: 1,
        sqlite_version: "3.53.0",
        sqlite_wasm_sha256: "b".repeat(64),
        rust_wasm_sha256: "c".repeat(64),
        plugin_id: "kwiry-search",
        plugin_version: "0.1.0",
        cache_identity: identity,
        source_policy_hash: SOURCE_POLICY_HASH,
        expected_cache_identity: identity,
        expected_source_policy_hash: SOURCE_POLICY_HASH,
      },
      {
        version: WORKER_PROTOCOL_VERSION,
        id: 3,
        operation: "plan_initial_build_checkpoint_reconciliation",
        generation: "g1",
        vault_id: "active-vault",
        current_sources: current,
      },
    ]);
  });

  it("keeps ordinary requests on the configured deadline", async () => {
    vi.useFakeTimers();
    try {
      const worker = new MockWorker();
      worker.heldOperations.add("initialize");
      const session = new InPluginWorkerSession(worker, "blob:kwiry", vi.fn(), 10);
      const pending = session.initialize("active-vault");
      const rejected = expect(pending).rejects.toMatchObject({ code: "timeout" });

      await vi.advanceTimersByTimeAsync(11);

      await rejected;
    } finally {
      vi.useRealTimers();
    }
  });

  it("gives full-corpus operations the bounded five-minute deadline", async () => {
    vi.useFakeTimers();
    try {
      const worker = new MockWorker();
      worker.heldOperations.add("commit_build");
      const session = new InPluginWorkerSession(worker, "blob:kwiry", vi.fn(), 10);
      let settled = false;
      const pending = session.commitBuild("g1");
      void pending.then(
        () => { settled = true; },
        () => { settled = true; },
      );
      await Promise.resolve();

      await vi.advanceTimersByTimeAsync(299_999);
      expect(settled).toBe(false);
      const rejected = expect(pending).rejects.toMatchObject({ code: "timeout" });
      await vi.advanceTimersByTimeAsync(2);

      await rejected;
    } finally {
      vi.useRealTimers();
    }
  });

  it("starts an ordinary timeout only after the preceding long operation finishes", async () => {
    vi.useFakeTimers();
    try {
      const worker = new MockWorker();
      worker.heldOperations.add("commit_build");
      const session = new InPluginWorkerSession(worker, "blob:kwiry", vi.fn(), 10);
      const commit = session.commitBuild("g1");
      await Promise.resolve();
      const search = session.search("query", 20);

      await vi.advanceTimersByTimeAsync(50);
      expect(worker.posted.map((request) => request.operation)).toEqual(["commit_build"]);

      worker.respond(worker.posted[0]!);
      await expect(commit).resolves.toMatchObject({ generation: "g1" });
      await expect(search).resolves.toMatchObject({ generation: "g1" });
      expect(worker.posted.map((request) => request.operation)).toEqual([
        "commit_build",
        "search",
      ]);
    } finally {
      vi.useRealTimers();
    }
  });

  it("force disposal rejects active and barrier-waiting work without posting the queued request", async () => {
    const worker = new MockWorker();
    worker.heldOperations.add("commit_build");
    const session = new InPluginWorkerSession(worker, "blob:kwiry", vi.fn(), 1_000);
    const commit = session.commitBuild("g1");
    await Promise.resolve();
    const search = session.search("query", 20);
    const commitRejected = expect(commit).rejects.toMatchObject({ code: "disposed" });
    const searchRejected = expect(search).rejects.toMatchObject({ code: "disposed" });

    session.forceDispose();

    await Promise.all([commitRejected, searchRejected]);
    expect(worker.posted.map((request) => request.operation)).toEqual(["commit_build"]);
  });

  it("terminates its Worker and revokes the Blob URL exactly once", async () => {
    const worker = new MockWorker();
    const revoke = vi.fn();
    const session = new InPluginWorkerSession(worker, "blob:kwiry", revoke, 1_000);

    await expect(session.initialize("active-vault")).resolves.toMatchObject({ rustAbiVersion: 3 });
    await expect(session.dispose()).resolves.toEqual({ closed: true });
    session.forceDispose();

    expect(worker.terminate).toHaveBeenCalledTimes(1);
    expect(revoke).toHaveBeenCalledTimes(1);
    expect(revoke).toHaveBeenCalledWith("blob:kwiry");
  });

  it("force disposal rejects pending work", async () => {
    const worker = new MockWorker();
    worker.postMessage = vi.fn();
    const session = new InPluginWorkerSession(worker, "blob:kwiry", vi.fn(), 1_000);
    const pending = session.initialize("active-vault");
    const rejected = expect(pending).rejects.toMatchObject({ code: "disposed" });
    session.forceDispose();
    await rejected;
  });
});
