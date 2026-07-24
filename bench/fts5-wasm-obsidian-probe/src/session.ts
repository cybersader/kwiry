// SPDX-License-Identifier: GPL-3.0-only
import {
  ProbeRpcClient,
  ProbeRpcError,
  type DisposeResult,
  type InitializeResult,
  type SmokeResult,
  type WorkerLike,
} from "./protocol";

export interface ProbeSummary {
  sqliteVersion: "3.53.0";
  fts5Enabled: 1;
  wasmBytes: 864752;
  zeroNetworkAttempts: true;
  syntheticQueryPassed: true;
  rollbackPassed: true;
  closed: true;
}

export class ProbeSession {
  private readonly client: ProbeRpcClient;
  private cleaned = false;

  constructor(
    private readonly worker: WorkerLike,
    private readonly objectUrl: string,
    private readonly revokeObjectUrl: (url: string) => void,
    timeoutMs?: number,
  ) {
    this.client = new ProbeRpcClient(worker, timeoutMs);
  }

  async run(): Promise<ProbeSummary> {
    try {
      const initialized = await this.client.request("initialize") as InitializeResult;
      const smoke = await this.client.request("probe") as SmokeResult;
      const disposed = await this.client.request("dispose") as DisposeResult;

      if (initialized.sqliteVersion !== "3.53.0"
        || initialized.fts5Enabled !== 1
        || initialized.wasmBytes !== 864_752
        || initialized.networkAttempts !== 0
        || initialized.persistenceAttempts !== 0
        || initialized.helperWorkerAttempts !== 0
        || smoke.expectedTitle !== "Synthetic Alpha"
        || smoke.finiteScore !== true
        || smoke.snippetMarked !== true
        || smoke.rollbackAbsent !== true
        || smoke.integrityPassed !== true
        || disposed.closed !== true) {
        throw new ProbeRpcError({
          code: "probe_failed",
          stage: "protocol",
          message: "Compatibility probe returned an invalid result.",
        });
      }

      return {
        sqliteVersion: initialized.sqliteVersion,
        fts5Enabled: initialized.fts5Enabled,
        wasmBytes: initialized.wasmBytes,
        zeroNetworkAttempts: true,
        syntheticQueryPassed: true,
        rollbackPassed: true,
        closed: true,
      };
    } finally {
      this.forceDispose();
    }
  }

  forceDispose(): void {
    if (this.cleaned) return;
    this.cleaned = true;
    this.client.stop();
    this.worker.terminate();
    this.revokeObjectUrl(this.objectUrl);
  }
}

export function createBrowserProbeSession(workerSource: string, timeoutMs?: number): ProbeSession {
  const blob = new Blob([workerSource], { type: "text/javascript" });
  const objectUrl = URL.createObjectURL(blob);
  try {
    const worker = new Worker(objectUrl, { name: "kwiry-fts5-wasm-probe" });
    return new ProbeSession(worker, objectUrl, (url) => URL.revokeObjectURL(url), timeoutMs);
  } catch (error) {
    URL.revokeObjectURL(objectUrl);
    throw error;
  }
}
