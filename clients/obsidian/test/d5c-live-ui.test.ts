// SPDX-FileCopyrightText: 2026 cybersader
// SPDX-License-Identifier: GPL-3.0-only

import { describe, expect, it, vi } from "vitest";

import type {
  ActiveVaultSource,
  ExcerptRead,
  SourceInspection,
  StableSourceRead,
  VaultSourceEvent,
} from "../src/active-vault-source";
import type { IndexCounts } from "../src/backends/in-plugin-index-controller";
import {
  coverageMessage,
  formatOwnerStatus,
  noMatchesMessage,
  resultFocusKey,
} from "../src/internal/d5c-playground/live-view";
import { D5cOwnerService } from "../src/internal/d5c-playground/live-service";
import type { SourceRemoval, SourceUpsert } from "../src/worker/protocol";
import type { D5cWorkerSession } from "../src/worker/d5c-session";

const PRIVATE_PATH = "private-folder/private-title.md";
const PRIVATE_QUERY = "private-query-sentinel";
const PRIVATE_EXCERPT = "private excerpt sentinel";

class OneNoteSource implements ActiveVaultSource {
  private listener: ((event: VaultSourceEvent) => void) | null = null;
  excerptGate: Promise<void> | null = null;
  excerptReadStarted: (() => void) | null = null;

  subscribe(listener: (event: VaultSourceEvent) => void): () => void {
    this.listener = listener;
    return () => {
      this.listener = null;
    };
  }

  listMarkdownPaths(): readonly string[] {
    return [PRIVATE_PATH];
  }

  inspectMarkdown(path: string): SourceInspection {
    return { kind: "candidate", path, size: 10, mtime: 1 };
  }

  async readMarkdown(): Promise<StableSourceRead> {
    const bytes = new TextEncoder().encode(`# Private\n${PRIVATE_EXCERPT}`);
    return {
      kind: "source",
      source: {
        descriptor: {
          vault_id: "active-vault",
          path: PRIVATE_PATH,
          format: "markdown",
          byte_length: bytes.byteLength,
          mtime: 1,
          mtime_nanos: "1000000",
        },
        bytes,
      },
    };
  }

  async readExcerptText(): Promise<ExcerptRead> {
    this.excerptReadStarted?.();
    if (this.excerptGate) await this.excerptGate;
    return { kind: "text", path: PRIVATE_PATH, text: PRIVATE_EXCERPT };
  }

  emit(event: VaultSourceEvent): void {
    this.listener?.(event);
  }
}

class FakeLiveSession {
  forceDispose = vi.fn();

  async initialize(): Promise<void> {}

  async beginBuild(generation: string): Promise<IndexCounts> {
    return counts(generation, 0);
  }

  async addSourceBatch(generation: string, sources: SourceUpsert[]): Promise<IndexCounts> {
    return counts(generation, sources.length);
  }

  async applySourceChanges(
    generation: string,
    _nextGeneration: string | null,
    _upserts: SourceUpsert[],
    _removals: SourceRemoval[],
  ): Promise<IndexCounts> {
    return counts(generation, 1);
  }

  async commitBuild(generation: string): Promise<IndexCounts> {
    return counts(generation, 1);
  }

  async abortBuild(generation: string): Promise<IndexCounts> {
    return counts(generation, 0);
  }

  async compareD5c(generation: string) {
    return {
      schema_version: 2 as const,
      generation,
      publication: "active" as const,
      revision: null,
      candidate_pool_count: 1,
      display_candidates: [{
        ordinal: 0,
        hit: {
          path: PRIVATE_PATH,
          heading_path: ["Private"],
          frontmatter: { title: "Private title sentinel" },
        },
      }],
      text_order: [0],
      balanced_order: [0],
      aggregate: { moved_candidate_count: 0, top_n_overlap: 1 },
    };
  }
}

function counts(generation: string, documents: number): IndexCounts {
  return {
    generation,
    documents,
    chunks: documents,
    database_bytes: documents,
    database_byte_limit: 1_000,
    quarantined_sources: 0,
    quarantine_fields: [],
  };
}

describe("live Text vs Balanced owner UI", () => {
  it("uses explicit partial, complete, incomplete, and no-match language", () => {
    expect(coverageMessage({
      kind: "partial",
      processed: 42,
      total: 900,
      documents: 40,
      chunks: 80,
    })).toBe(
      "Partial results — indexing 42/900 (4%). Results cover only files indexed so far.",
    );
    expect(noMatchesMessage({
      kind: "partial",
      processed: 42,
      total: 900,
      documents: 40,
      chunks: 80,
    })).toBe("No matches in the partial index yet.");
    expect(coverageMessage({ kind: "complete", documents: 10, chunks: 20 }))
      .toBe("Complete index.");
    expect(coverageMessage({
      kind: "updating",
      documents: 10,
      chunks: 20,
      omittedNotes: 0,
    })).toBe("Current indexed results — vault changes are still being reconciled.");
    expect(coverageMessage({
      kind: "incomplete",
      documents: 9,
      chunks: 18,
      omittedNotes: 1,
    })).toBe("Incomplete index — 1 note could not be indexed.");
  });

  it("formats concise owner status without question marks or duplicated profile text", () => {
    const identity = {
      stage: "snapshot" as const,
      searchable: false,
      generation: null,
      documents: 0,
      chunks: 0,
      quarantinedSources: 0,
      unreadableSources: 0,
      quarantineValidatorFields: [],
      dirty: true,
      rebuilding: false,
    };
    const status = formatOwnerStatus({
      ...identity,
      progress: { completed: 42, total: 900 },
    });
    expect(status).toBe("Kwiry: Indexing 42/900 (4%)");
    expect(status).not.toContain("?");
    expect(status).not.toContain("In-plugin");
  });

  it("distinguishes duplicate chunk rows when restoring keyboard focus", () => {
    const first = resultFocusKey("text", 3, "note.md", ["Repeated"]);
    const second = resultFocusKey("text", 7, "note.md", ["Repeated"]);
    expect(first).not.toBe(second);
    expect(resultFocusKey("text", 3, "note.md", ["Repeated"])).toBe(first);
    expect(resultFocusKey("balanced", 3, "note.md", ["Repeated"])).not.toBe(first);
  });

  it("rejects a comparison when the vault mutates during excerpt hydration", async () => {
    const source = new OneNoteSource();
    let releaseExcerpt!: () => void;
    source.excerptGate = new Promise<void>((resolve) => {
      releaseExcerpt = resolve;
    });
    let excerptStarted!: () => void;
    const excerptReadStarted = new Promise<void>((resolve) => {
      excerptStarted = resolve;
    });
    source.excerptReadStarted = excerptStarted;
    const session = new FakeLiveSession();
    const service = new D5cOwnerService({
      source,
      workerSource: "unused",
      createSession: () => session as unknown as D5cWorkerSession,
      nextGeneration: (() => {
        let generation = 0;
        return () => `g${++generation}`;
      })(),
      yieldControl: () => Promise.resolve(),
      nowEpochSeconds: () => "2000000000",
    });
    service.start();
    await vi.waitFor(() => expect(service.status()).toMatchObject({
      stage: "ready",
      searchable: true,
      generation: "g1",
    }));

    const comparison = service.compare(PRIVATE_QUERY);
    await excerptReadStarted;
    source.emit({ kind: "upsert", path: PRIVATE_PATH });
    releaseExcerpt();

    await expect(comparison).rejects.toMatchObject({ code: "index_changed" });
    await service.dispose();
  });

  it("preserves a source-batch capacity code when staging cleanup also fails", async () => {
    class CapacityFailureSession extends FakeLiveSession {
      override async addSourceBatch(): Promise<IndexCounts> {
        throw Object.assign(new Error("capacity"), { code: "index_limit_exceeded" });
      }

      override async abortBuild(): Promise<IndexCounts> {
        throw Object.assign(new Error("already aborted"), { code: "invalid_state" });
      }
    }
    const session = new CapacityFailureSession();
    const service = new D5cOwnerService({
      source: new OneNoteSource(),
      workerSource: "unused",
      createSession: () => session as unknown as D5cWorkerSession,
      nextGeneration: () => "g1",
      yieldControl: () => Promise.resolve(),
    });
    service.start();
    await vi.waitFor(() => expect(service.status()?.stage).toBe("failed"));

    const summary = service.technicalSummary();
    expect(summary).toMatchObject({
      searches: { attempted: 0, failed: 0 },
      failures: { index_limit_exceeded: 1 },
    });
    expect(summary.failures.other).toBeUndefined();
    await service.dispose();
  });

  it("keeps query, path, title, heading, and excerpt out of the technical summary", async () => {
    const session = new FakeLiveSession();
    const service = new D5cOwnerService({
      source: new OneNoteSource(),
      workerSource: "unused",
      createSession: () => session as unknown as D5cWorkerSession,
      nextGeneration: () => "g1",
      yieldControl: () => Promise.resolve(),
      nowEpochSeconds: () => "2000000000",
    });
    service.start();
    await vi.waitFor(() => expect(service.status()).toMatchObject({
      stage: "ready",
      searchable: true,
      generation: "g1",
    }));

    await expect(service.compare(PRIVATE_QUERY)).resolves.toMatchObject({
      candidates: [{ hit: { path: PRIVATE_PATH } }],
    });
    const serialized = JSON.stringify(service.technicalSummary());
    for (const forbidden of [
      PRIVATE_QUERY,
      PRIVATE_PATH,
      "Private title sentinel",
      "Private",
      PRIVATE_EXCERPT,
      "source_key",
      "chunk_id",
      "points",
      "tier",
    ]) {
      expect(serialized).not.toContain(forbidden);
    }
    await service.dispose();
    expect(session.forceDispose).toHaveBeenCalledOnce();
  });
});
