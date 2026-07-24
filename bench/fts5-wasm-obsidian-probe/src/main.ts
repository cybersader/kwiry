// SPDX-License-Identifier: GPL-3.0-only
import { Notice, Plugin } from "obsidian";
import workerSource from "virtual:kwiry-worker-source";

import { ProbeRpcError } from "./protocol";
import { ProbeSession, createBrowserProbeSession } from "./session";

function errorCode(error: unknown): string {
  return error instanceof ProbeRpcError ? error.code : "worker_crashed";
}

export default class KwiryFts5WasmProbePlugin extends Plugin {
  private activeSession: ProbeSession | undefined;
  private generation = 0;

  override onload(): void {
    this.addCommand({
      id: "run-synthetic-fts5-compatibility-probe",
      name: "Run embedded FTS5-WASM compatibility probe",
      callback: () => {
        void this.runProbe();
      },
    });
  }

  override onunload(): void {
    this.generation += 1;
    this.activeSession?.forceDispose();
    this.activeSession = undefined;
  }

  private async runProbe(): Promise<void> {
    if (this.activeSession) {
      new Notice("Kwiry compatibility probe is already running.");
      return;
    }

    const generation = this.generation + 1;
    this.generation = generation;
    let session: ProbeSession;
    try {
      session = createBrowserProbeSession(workerSource);
    } catch {
      new Notice("Kwiry compatibility probe failed (worker_crashed).", 10_000);
      return;
    }

    this.activeSession = session;
    new Notice("Running the synthetic Kwiry FTS5-WASM compatibility probe…");

    try {
      await session.run();
      if (this.generation === generation && this.activeSession === session) {
        new Notice(
          "Local compatibility probe passed: embedded Worker, SQLite 3.53.0, FTS5, fixed query, rollback, and clean close. BRAT field testing is still required.",
          15_000,
        );
      }
    } catch (error) {
      if (this.generation === generation && this.activeSession === session) {
        new Notice(`Kwiry compatibility probe failed (${errorCode(error)}).`, 10_000);
      }
    } finally {
      session.forceDispose();
      if (this.activeSession === session) this.activeSession = undefined;
    }
  }
}
