// SPDX-FileCopyrightText: 2026 cybersader
// SPDX-License-Identifier: GPL-3.0-only

import { describe, expect, it, vi } from "vitest";

import {
  ACTIVE_VAULT_ID,
  type ActiveVaultSource,
  type ExcerptRead,
  type SourceInspection,
  type StableSourceRead,
  type VaultSourceEvent,
} from "../src/active-vault-source";
import {
  InPluginIndexController,
  type IndexControllerStatus,
  type IndexCounts,
  type IndexWorkerPort,
  type SourceRemoval,
} from "../src/backends/in-plugin-index-controller";
import type { SourceInput } from "../src/worker/protocol";
import { WorkerRpcError } from "../src/worker/rpc-client";

class FakeSource implements ActiveVaultSource {
  readonly log: string[] = [];
  readonly records = new Map<string, { bytes: Uint8Array; mtime: number }>();
  listener: ((event: VaultSourceEvent) => void) | null = null;
  onRead: ((path: string) => void) | null = null;
  staleReads = new Set<string>();
  oversizedPaths = new Set<string>();
  unsubscribed = 0;

  subscribe(listener: (event: VaultSourceEvent) => void): () => void {
    this.log.push("subscribe");
    this.listener = listener;
    return () => {
      if (this.listener === null) return;
      this.listener = null;
      this.unsubscribed += 1;
    };
  }

  listMarkdownPaths(): readonly string[] {
    this.log.push("list");
    return [...this.records.keys()].reverse();
  }

  // Indexing never hydrates excerpts; this exists only to satisfy the port.
  async readExcerptText(path: string): Promise<ExcerptRead> {
    return { kind: "missing", path };
  }

  inspectMarkdown(path: string): SourceInspection {
    const record = this.records.get(path);
    if (!record) return { kind: "missing", path };
    if (this.oversizedPaths.has(path)) return { kind: "oversized", path };
    return {
      kind: "candidate",
      path,
      size: record.bytes.byteLength,
      mtime: record.mtime,
    };
  }

  async readMarkdown(
    inspection: Extract<SourceInspection, { kind: "candidate" }>,
  ): Promise<StableSourceRead> {
    this.log.push(`read:${inspection.path}`);
    this.onRead?.(inspection.path);
    if (this.staleReads.has(inspection.path)) return { kind: "stale", path: inspection.path };
    const record = this.records.get(inspection.path);
    if (!record) return { kind: "missing", path: inspection.path };
    if (record.mtime !== inspection.mtime || record.bytes.byteLength !== inspection.size) {
      return { kind: "stale", path: inspection.path };
    }
    return { kind: "source", source: sourceInput(inspection.path, record.bytes, record.mtime) };
  }

  emit(event: VaultSourceEvent): void {
    this.listener?.(event);
  }

  set(path: string, text: string, mtime = 1): void {
    this.records.set(path, { bytes: new TextEncoder().encode(text), mtime });
  }

  rename(oldPath: string, path: string): void {
    const record = this.records.get(oldPath);
    this.records.delete(oldPath);
    if (record) this.records.set(path, record);
    this.emit({ kind: "rename", oldPath, path });
  }
}

interface ApplyCall {
  generation: string;
  nextGeneration: string | null;
  upserts: string[];
  removals: string[];
}

class FakeWorker implements IndexWorkerPort {
  readonly calls: string[] = [];
  readonly applyCalls: ApplyCall[] = [];
  activeGeneration: string | null = null;
  stagingGeneration: string | null = null;
  activePaths = new Set<string>();
  stagingPaths = new Set<string>();

  async initialize(): Promise<void> {
    this.calls.push("initialize");
  }

  async beginBuild(generation: string): Promise<IndexCounts> {
    this.calls.push(`begin:${generation}`);
    this.stagingGeneration = generation;
    this.stagingPaths = new Set();
    return this.counts(generation, this.stagingPaths);
  }

  async addSourceBatch(generation: string, sources: SourceInput[]): Promise<IndexCounts> {
    this.calls.push(`add:${sources.map((source) => source.descriptor.path).join(",")}`);
    if (generation !== this.stagingGeneration) throw new Error("wrong staging generation");
    for (const source of sources) this.stagingPaths.add(source.descriptor.path);
    return this.counts(generation, this.stagingPaths);
  }

  async applySourceChanges(
    generation: string,
    nextGeneration: string | null,
    upserts: SourceInput[],
    removals: SourceRemoval[],
  ): Promise<IndexCounts> {
    this.applyCalls.push({
      generation,
      nextGeneration,
      upserts: upserts.map((source) => source.descriptor.path),
      removals: removals.map((removal) => removal.path),
    });
    const paths = nextGeneration === null ? this.stagingPaths : this.activePaths;
    for (const removal of removals) paths.delete(removal.path);
    for (const source of upserts) paths.add(source.descriptor.path);
    if (nextGeneration === null) return this.counts(generation, paths);
    this.activeGeneration = nextGeneration;
    return this.counts(nextGeneration, paths);
  }

  async commitBuild(generation: string): Promise<IndexCounts> {
    this.calls.push(`commit:${generation}`);
    if (generation !== this.stagingGeneration) throw new Error("wrong staging generation");
    this.activeGeneration = generation;
    this.activePaths = new Set(this.stagingPaths);
    this.stagingGeneration = null;
    this.stagingPaths = new Set();
    return this.counts(generation, this.activePaths);
  }

  async abortBuild(generation: string): Promise<IndexCounts> {
    this.calls.push(`abort:${generation}`);
    if (generation === this.stagingGeneration) {
      this.stagingGeneration = null;
      this.stagingPaths = new Set();
    }
    const active = this.activeGeneration ?? generation;
    return this.counts(active, this.activePaths);
  }

  private counts(generation: string, paths: Set<string>): IndexCounts {
    return { generation, documents: paths.size, chunks: paths.size };
  }
}

function sourceInput(path: string, bytes: Uint8Array, mtime: number): SourceInput {
  return {
    descriptor: {
      vault_id: ACTIVE_VAULT_ID,
      path,
      format: "markdown",
      byte_length: bytes.byteLength,
      mtime: Math.floor(mtime / 1_000),
      mtime_nanos: (BigInt(mtime) * 1_000_000n).toString(),
    },
    bytes,
  };
}

function harness(
  source: FakeSource,
  worker = new FakeWorker(),
  limits: ConstructorParameters<typeof InPluginIndexController>[0]["limits"] = {},
): {
    controller: InPluginIndexController;
    worker: FakeWorker;
    statuses: IndexControllerStatus[];
    failures: unknown[];
  } {
  const statuses: IndexControllerStatus[] = [];
  const failures: unknown[] = [];
  let generation = 0;
  const controller = new InPluginIndexController({
    source,
    worker,
    nextGeneration: () => `generation-${++generation}`,
    onStatus: (status) => statuses.push(status),
    onFailure: (error) => failures.push(error),
    yieldControl: () => Promise.resolve(),
    limits,
  });
  return { controller, worker, statuses, failures };
}

describe("InPluginIndexController", () => {
  it("subscribes before the snapshot, sorts paths, and publishes only after commit", async () => {
    const source = new FakeSource();
    source.set("z.md", "z");
    source.set("a.md", "a");
    const { controller, worker, statuses } = harness(source);

    controller.start();
    await controller.whenIdle();

    expect(source.log[0]).toBe("subscribe");
    expect(source.log.indexOf("subscribe")).toBeLessThan(source.log.indexOf("list"));
    expect(worker.calls).toEqual([
      "initialize",
      "begin:generation-1",
      "add:a.md,z.md",
      "commit:generation-1",
    ]);
    expect(statuses.filter((status) => status.searchable).map((status) => status.stage)).toEqual([
      "ready",
    ]);
    expect(statuses.at(-1)).toMatchObject({
      stage: "ready",
      generation: "generation-1",
      documents: 2,
      dirty: false,
    });
  });

  it("reads up to four sources concurrently but emits deterministic sorted batches", async () => {
    const source = new FakeSource();
    for (const path of ["f.md", "e.md", "d.md", "c.md", "b.md", "a.md"]) {
      source.set(path, path.slice(0, 1));
    }
    const releases = new Map<string, () => void>();
    const started: string[] = [];
    let activeReads = 0;
    let peakReads = 0;
    const originalRead = source.readMarkdown.bind(source);
    source.readMarkdown = vi.fn(async (inspection) => {
      started.push(inspection.path);
      activeReads += 1;
      peakReads = Math.max(peakReads, activeReads);
      await new Promise<void>((resolve) => releases.set(inspection.path, resolve));
      activeReads -= 1;
      return originalRead(inspection);
    });
    const { controller, worker } = harness(source);

    controller.start();
    await vi.waitFor(() => expect(started).toEqual(["a.md", "b.md", "c.md", "d.md"]));
    for (const path of ["d.md", "a.md", "c.md", "b.md"]) releases.get(path)?.();
    await vi.waitFor(() => expect(started).toHaveLength(6));
    releases.get("f.md")?.();
    releases.get("e.md")?.();
    await controller.whenIdle();

    expect(peakReads).toBe(4);
    expect(worker.calls).toContain("add:a.md,b.md,c.md,d.md,e.md,f.md");
  });

  it("flushes retained bytes before starting a read that would exceed the batch budget", async () => {
    const source = new FakeSource();
    for (const path of ["a.md", "b.md", "c.md"]) source.set(path, "123456");
    const releases = new Map<string, () => void>();
    const started: string[] = [];
    const originalRead = source.readMarkdown.bind(source);
    source.readMarkdown = vi.fn(async (inspection) => {
      started.push(inspection.path);
      await new Promise<void>((resolve) => releases.set(inspection.path, resolve));
      return originalRead(inspection);
    });
    const { controller, worker } = harness(source, new FakeWorker(), {
      maxConcurrentReads: 4,
      maxBatchBytes: 10,
    });

    controller.start();
    await vi.waitFor(() => expect(started).toEqual(["a.md"]));
    releases.get("a.md")?.();
    await vi.waitFor(() => {
      expect(worker.calls).toContain("add:a.md");
      expect(started).toEqual(["a.md", "b.md"]);
    });
    releases.get("b.md")?.();
    await vi.waitFor(() => {
      expect(worker.calls).toContain("add:b.md");
      expect(started).toEqual(["a.md", "b.md", "c.md"]);
    });
    releases.get("c.md")?.();
    await controller.whenIdle();

    expect(worker.calls.filter((call) => call.startsWith("add:"))).toEqual([
      "add:a.md",
      "add:b.md",
      "add:c.md",
    ]);
  });

  it("replays a rename emitted during snapshot reads before publication", async () => {
    const source = new FakeSource();
    source.set("a.md", "a");
    source.set("b.md", "b");
    let renamed = false;
    source.onRead = () => {
      if (renamed) return;
      renamed = true;
      source.rename("b.md", "c.md");
    };
    const { controller, worker } = harness(source);

    controller.start();
    await controller.whenIdle();

    expect(worker.applyCalls).toContainEqual({
      generation: "generation-1",
      nextGeneration: null,
      upserts: ["c.md"],
      removals: ["b.md"],
    });
    expect([...worker.activePaths].sort()).toEqual(["a.md", "c.md"]);
  });

  it("continues replay when another event arrives during a staging mutation", async () => {
    const source = new FakeSource();
    source.set("a.md", "a");
    source.set("b.md", "b");
    let renamed = false;
    source.onRead = () => {
      if (renamed) return;
      renamed = true;
      source.rename("b.md", "c.md");
    };
    const worker = new FakeWorker();
    const originalApply = worker.applySourceChanges.bind(worker);
    let emitted = false;
    worker.applySourceChanges = vi.fn(async (
      generation,
      nextGeneration,
      upserts,
      removals,
    ) => {
      if (nextGeneration === null && !emitted) {
        emitted = true;
        source.set("late.md", "late");
        source.emit({ kind: "upsert", path: "late.md" });
      }
      return originalApply(generation, nextGeneration, upserts, removals);
    });
    const { controller } = harness(source, worker);

    controller.start();
    await controller.whenIdle();

    expect([...worker.activePaths].sort()).toEqual(["a.md", "c.md", "late.md"]);
    expect(worker.applyCalls.filter((call) => call.nextGeneration === null)).toHaveLength(2);
  });

  it("drains an event emitted during initial commit after publication", async () => {
    const source = new FakeSource();
    source.set("initial.md", "initial");
    const worker = new FakeWorker();
    const originalCommit = worker.commitBuild.bind(worker);
    let emitted = false;
    worker.commitBuild = vi.fn(async (generation) => {
      if (!emitted) {
        emitted = true;
        source.set("late.md", "late");
        source.emit({ kind: "upsert", path: "late.md" });
      }
      return originalCommit(generation);
    });
    const { controller } = harness(source, worker);

    controller.start();
    await controller.whenIdle();

    expect([...worker.activePaths].sort()).toEqual(["initial.md", "late.md"]);
    expect(worker.activeGeneration).toBe("generation-2");
  });

  it("coalesces modify bursts and advances the active generation once", async () => {
    const source = new FakeSource();
    source.set("note.md", "first", 1);
    const { controller, worker } = harness(source);
    controller.start();
    await controller.whenIdle();

    source.set("note.md", "final", 2);
    source.emit({ kind: "upsert", path: "note.md" });
    source.emit({ kind: "upsert", path: "note.md" });
    source.emit({ kind: "upsert", path: "note.md" });
    await controller.whenIdle();

    expect(worker.applyCalls.at(-1)).toEqual({
      generation: "generation-1",
      nextGeneration: "generation-2",
      upserts: ["note.md"],
      removals: [],
    });
  });

  it("retries a transient stale read and publishes the stable source", async () => {
    const source = new FakeSource();
    source.set("note.md", "stable");
    const originalRead = source.readMarkdown.bind(source);
    let attempts = 0;
    source.readMarkdown = vi.fn(async (inspection) => {
      attempts += 1;
      if (attempts === 1) return { kind: "stale" as const, path: inspection.path };
      return originalRead(inspection);
    });
    const { controller, worker } = harness(source);

    controller.start();
    await controller.whenIdle();

    expect(attempts).toBe(2);
    expect([...worker.activePaths]).toEqual(["note.md"]);
  });

  it("removes active sources that become oversized or vanish", async () => {
    const source = new FakeSource();
    source.set("oversized.md", "old");
    source.set("vanished.md", "old");
    const { controller, worker } = harness(source);
    controller.start();
    await controller.whenIdle();

    source.oversizedPaths.add("oversized.md");
    source.records.delete("vanished.md");
    source.emit({ kind: "upsert", path: "oversized.md" });
    source.emit({ kind: "upsert", path: "vanished.md" });
    await controller.whenIdle();

    expect([...worker.activePaths]).toEqual([]);
    expect(worker.applyCalls.slice(-2).flatMap((call) => call.removals).sort()).toEqual([
      "oversized.md",
      "vanished.md",
    ]);
  });

  it("keeps exactly one active mutation RPC in flight", async () => {
    const source = new FakeSource();
    source.set("base.md", "base");
    const worker = new FakeWorker();
    const { controller } = harness(source, worker);
    controller.start();
    await controller.whenIdle();

    let release!: () => void;
    const gate = new Promise<void>((resolve) => {
      release = resolve;
    });
    const originalApply = worker.applySourceChanges.bind(worker);
    let activeCalls = 0;
    let peakCalls = 0;
    let first = true;
    worker.applySourceChanges = vi.fn(async (
      generation,
      nextGeneration,
      upserts,
      removals,
    ) => {
      activeCalls += 1;
      peakCalls = Math.max(peakCalls, activeCalls);
      try {
        if (first) {
          first = false;
          await gate;
        }
        return await originalApply(generation, nextGeneration, upserts, removals);
      } finally {
        activeCalls -= 1;
      }
    });

    source.set("x.md", "x");
    source.set("y.md", "y");
    source.emit({ kind: "upsert", path: "x.md" });
    source.emit({ kind: "upsert", path: "y.md" });
    await vi.waitFor(() => expect(worker.applySourceChanges).toHaveBeenCalledTimes(1));
    expect(activeCalls).toBe(1);
    release();
    await controller.whenIdle();

    expect(peakCalls).toBe(1);
    expect(worker.applySourceChanges).toHaveBeenCalledTimes(2);
  });

  it("collapses rename chains into one atomic old-path removal and final upsert", async () => {
    const source = new FakeSource();
    source.set("a.md", "a");
    const { controller, worker } = harness(source);
    controller.start();
    await controller.whenIdle();

    source.rename("a.md", "b.md");
    source.rename("b.md", "c.md");
    await controller.whenIdle();

    expect(worker.applyCalls.at(-1)).toEqual({
      generation: "generation-1",
      nextGeneration: "generation-2",
      upserts: ["c.md"],
      removals: ["a.md"],
    });
    expect([...worker.activePaths]).toEqual(["c.md"]);
  });

  it("preserves causal order for dependent renames", async () => {
    const source = new FakeSource();
    source.set("a.md", "a");
    source.set("b.md", "b");
    const { controller, worker } = harness(source);
    controller.start();
    await controller.whenIdle();

    source.rename("b.md", "c.md");
    source.rename("a.md", "b.md");
    await controller.whenIdle();

    expect(worker.applyCalls.slice(-2)).toEqual([
      {
        generation: "generation-1",
        nextGeneration: "generation-2",
        upserts: ["c.md"],
        removals: ["b.md"],
      },
      {
        generation: "generation-2",
        nextGeneration: "generation-3",
        upserts: ["b.md"],
        removals: ["a.md"],
      },
    ]);
    expect([...worker.activePaths].sort()).toEqual(["b.md", "c.md"]);
  });

  it("rebuilds authoritative state for a live rename swap cycle", async () => {
    const source = new FakeSource();
    source.set("a.md", "a");
    source.set("b.md", "b");
    const { controller, worker } = harness(source);
    controller.start();
    await controller.whenIdle();

    source.rename("a.md", "temporary.md");
    source.rename("b.md", "a.md");
    source.rename("temporary.md", "b.md");
    await controller.whenIdle();

    expect(worker.calls.filter((call) => call.startsWith("begin:"))).toHaveLength(2);
    expect([...worker.activePaths].sort()).toEqual(["a.md", "b.md"]);
  });

  it("restarts an initial snapshot when replay contains a rename round trip", async () => {
    const source = new FakeSource();
    source.set("a.md", "a");
    source.set("b.md", "b");
    let renamed = false;
    source.onRead = () => {
      if (renamed) return;
      renamed = true;
      source.rename("a.md", "temporary.md");
      source.rename("temporary.md", "a.md");
    };
    const { controller, worker } = harness(source);

    controller.start();
    await controller.whenIdle();

    expect(worker.calls.filter((call) => call.startsWith("begin:"))).toHaveLength(2);
    expect([...worker.activePaths].sort()).toEqual(["a.md", "b.md"]);
  });

  it("rebuilds when a rename root is reused and its destination is then deleted", async () => {
    const source = new FakeSource();
    source.set("a.md", "a");
    source.set("b.md", "b");
    const { controller, worker } = harness(source);
    controller.start();
    await controller.whenIdle();

    source.rename("b.md", "c.md");
    source.rename("a.md", "b.md");
    source.records.delete("c.md");
    source.emit({ kind: "remove", path: "c.md" });
    await controller.whenIdle();

    expect(worker.calls.filter((call) => call.startsWith("begin:"))).toHaveLength(2);
    expect([...worker.activePaths]).toEqual(["b.md"]);
  });

  it("keeps chain updates in their original causal position", async () => {
    const source = new FakeSource();
    source.set("a.md", "a");
    source.set("b.md", "b");
    const { controller, worker } = harness(source);
    controller.start();
    await controller.whenIdle();

    source.rename("b.md", "c.md");
    source.rename("a.md", "b.md");
    source.rename("c.md", "d.md");
    await controller.whenIdle();

    expect(worker.applyCalls.slice(-2)).toEqual([
      {
        generation: "generation-1",
        nextGeneration: "generation-2",
        upserts: ["d.md"],
        removals: ["b.md"],
      },
      {
        generation: "generation-2",
        nextGeneration: "generation-3",
        upserts: ["b.md"],
        removals: ["a.md"],
      },
    ]);
    expect([...worker.activePaths].sort()).toEqual(["b.md", "d.md"]);
  });

  it("falls back to an authoritative rebuild when a renamed path is reused", async () => {
    const source = new FakeSource();
    source.set("a.md", "old");
    const { controller, worker } = harness(source);
    controller.start();
    await controller.whenIdle();

    source.rename("a.md", "b.md");
    source.set("a.md", "new");
    source.emit({ kind: "upsert", path: "a.md" });
    source.rename("a.md", "c.md");
    await controller.whenIdle();

    expect(worker.calls.filter((call) => call.startsWith("begin:"))).toHaveLength(2);
    expect([...worker.activePaths].sort()).toEqual(["b.md", "c.md"]);
  });

  it("coalesces repeated manual rebuild requests and resets progress immediately", async () => {
    const source = new FakeSource();
    source.set("a.md", "a");
    const { controller, worker, statuses } = harness(source);
    controller.start();
    await controller.whenIdle();

    controller.requestRebuild();
    controller.requestRebuild();
    controller.requestRebuild();
    expect(statuses.at(-1)).toMatchObject({
      stage: "rebuild",
      progress: { completed: 0, total: null },
    });
    await controller.whenIdle();

    expect(worker.calls.filter((call) => call.startsWith("begin:"))).toHaveLength(2);
  });

  it("turns pending-path overflow into one authoritative rebuild", async () => {
    const source = new FakeSource();
    source.set("base.md", "base");
    const { controller, worker } = harness(source, new FakeWorker(), { maxPendingPaths: 1 });
    controller.start();
    await controller.whenIdle();

    source.set("x.md", "x");
    source.set("y.md", "y");
    source.emit({ kind: "upsert", path: "x.md" });
    source.emit({ kind: "upsert", path: "y.md" });
    await controller.whenIdle();

    expect(worker.calls.filter((call) => call.startsWith("begin:"))).toHaveLength(2);
    expect([...worker.activePaths].sort()).toEqual(["base.md", "x.md", "y.md"]);
  });

  it("fails closed when an initial source never becomes stable", async () => {
    const source = new FakeSource();
    source.set("changing.md", "value");
    source.staleReads.add("changing.md");
    const { controller, worker, statuses } = harness(source, new FakeWorker(), {
      maxStableReadAttempts: 2,
    });

    controller.start();
    await controller.whenIdle();

    expect(worker.calls.some((call) => call.startsWith("commit:"))).toBe(false);
    expect(statuses.at(-1)).toMatchObject({
      stage: "failed",
      searchable: false,
      issue: "vault_read_failed",
    });
  });

  it("surfaces an uncertain abort failure instead of reusing that Worker", async () => {
    const source = new FakeSource();
    source.set("changing.md", "value");
    source.staleReads.add("changing.md");
    const worker = new FakeWorker();
    worker.abortBuild = vi.fn(async () => {
      throw new WorkerRpcError({
        code: "timeout",
        stage: "lifecycle",
        message: "private detail",
        retryable: true,
      });
    });
    const { controller, failures } = harness(source, worker, { maxStableReadAttempts: 1 });

    controller.start();
    await controller.whenIdle();

    const failure = failures.at(-1);
    expect(failure).toBeInstanceOf(AggregateError);
    expect((failure as AggregateError).errors).toEqual(expect.arrayContaining([
      expect.objectContaining({ code: "timeout" }),
    ]));
  });

  it("prevents a late active mutation from publishing after disposal", async () => {
    const source = new FakeSource();
    source.set("base.md", "base");
    const worker = new FakeWorker();
    const { controller, statuses } = harness(source, worker);
    controller.start();
    await controller.whenIdle();

    let release!: () => void;
    const gate = new Promise<void>((resolve) => {
      release = resolve;
    });
    const originalApply = worker.applySourceChanges.bind(worker);
    worker.applySourceChanges = vi.fn(async (
      generation,
      nextGeneration,
      upserts,
      removals,
    ) => {
      await gate;
      return originalApply(generation, nextGeneration, upserts, removals);
    });
    source.set("late.md", "late");
    source.emit({ kind: "upsert", path: "late.md" });
    await vi.waitFor(() => expect(worker.applySourceChanges).toHaveBeenCalledTimes(1));

    controller.dispose();
    release();
    await controller.whenIdle();

    expect(source.unsubscribed).toBe(1);
    expect(statuses.at(-1)).toMatchObject({ stage: "disposed", searchable: false });
  });

  it("detaches events and prevents late reads from committing after disposal", async () => {
    const source = new FakeSource();
    source.set("blocked.md", "value");
    let release!: () => void;
    const gate = new Promise<void>((resolve) => {
      release = resolve;
    });
    const originalRead = source.readMarkdown.bind(source);
    source.readMarkdown = vi.fn(async (inspection) => {
      await gate;
      return originalRead(inspection);
    });
    const { controller, worker, statuses } = harness(source);

    controller.start();
    await vi.waitFor(() => expect(source.readMarkdown).toHaveBeenCalled());
    controller.dispose();
    release();
    await controller.whenIdle();

    expect(source.unsubscribed).toBe(1);
    expect(worker.calls.some((call) => call.startsWith("commit:"))).toBe(false);
    expect(statuses.at(-1)).toMatchObject({ stage: "disposed", searchable: false });
  });
});
