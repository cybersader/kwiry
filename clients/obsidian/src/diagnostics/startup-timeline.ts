// SPDX-FileCopyrightText: 2026 cybersader
// SPDX-License-Identifier: GPL-3.0-only

export type StartupOutcome = "succeeded" | "degraded" | "failed" | "cancelled";

export type StartupReason =
  | "fully_current"
  | "sources_omitted"
  | "vault_unavailable"
  | "index_capacity"
  | "backend_unavailable"
  | "plugin_load_failed"
  | "activation_failed"
  | "plugin_unloaded";

export interface StartupAggregateDetails {
  readonly profile: "daemon" | "in_plugin";
  readonly outcome: StartupOutcome;
  readonly reason: StartupReason;
  readonly pluginEpoch: number;
  readonly activationEpoch: number;
  readonly pluginLoadCompleteMs: number | null;
  readonly layoutReadyMs: number | null;
  readonly firstProgressMs: number | null;
  readonly firstCacheSearchableMs: number | null;
  readonly fullyCurrentMs: number | null;
  readonly cacheHit: boolean;
  readonly cacheBytes?: number;
}

export interface StartupTimelineRecord {
  readonly startedAtMs: number;
  readonly durationMs: number;
  readonly details: Readonly<StartupAggregateDetails>;
}

export interface StartupTimelineOptions {
  profile: "daemon" | "in_plugin";
  pluginEpoch: number;
  activationEpoch?: number;
  wallNow?: () => number;
  monotonicNow?: () => number;
  record: (record: StartupTimelineRecord) => void;
}

/**
 * Records one privacy-safe aggregate for the initial plugin activation. Every
 * milestone is relative to one monotonic origin; the wall clock is retained
 * only so exported diagnostics can display when startup began.
 */
export class StartupTimeline {
  private readonly wallStartedAtMs: number;
  private readonly monotonicStartedAtMs: number;
  private readonly monotonicNow: () => number;
  private readonly record: StartupTimelineOptions["record"];
  private profile: "daemon" | "in_plugin";
  private readonly pluginEpoch: number;
  private activationEpoch: number;
  private pluginLoadCompleteMs: number | null = null;
  private layoutReadyMs: number | null = null;
  private firstProgressMs: number | null = null;
  private firstCacheSearchableMs: number | null = null;
  private fullyCurrentMs: number | null = null;
  private cacheBytes: number | undefined;
  private cacheHit = false;
  private lastElapsedMs = 0;
  private finished = false;

  constructor(options: StartupTimelineOptions) {
    const wallNow = options.wallNow ?? Date.now;
    this.monotonicNow = options.monotonicNow ?? defaultMonotonicNow;
    this.wallStartedAtMs = validWallTimestamp(wallNow());
    this.monotonicStartedAtMs = validMonotonicTimestamp(this.monotonicNow());
    this.record = options.record;
    this.profile = options.profile;
    this.pluginEpoch = nonNegativeInteger(options.pluginEpoch);
    this.activationEpoch = nonNegativeInteger(options.activationEpoch ?? 0);
  }

  setProfile(profile: "daemon" | "in_plugin"): void {
    if (!this.finished) this.profile = profile;
  }

  beginActivation(profile: "daemon" | "in_plugin", activationEpoch: number): void {
    if (this.finished) return;
    this.profile = profile;
    this.activationEpoch = nonNegativeInteger(activationEpoch);
  }

  markPluginLoadComplete(): void {
    if (this.pluginLoadCompleteMs === null && !this.finished) {
      this.pluginLoadCompleteMs = this.elapsedMs();
    }
  }

  markLayoutReady(): void {
    if (this.layoutReadyMs === null && !this.finished) {
      this.layoutReadyMs = this.elapsedMs();
    }
  }

  markFirstProgress(): void {
    if (this.firstProgressMs === null && !this.finished) {
      this.firstProgressMs = this.elapsedMs();
    }
  }

  markCacheSearchable(cacheBytes: number): void {
    if (this.finished || this.firstCacheSearchableMs !== null) return;
    this.firstCacheSearchableMs = this.elapsedMs();
    this.cacheBytes = nonNegativeInteger(cacheBytes);
    this.cacheHit = true;
  }

  markFullyCurrent(): void {
    if (this.finished) return;
    if (this.fullyCurrentMs === null) this.fullyCurrentMs = this.elapsedMs();
    this.finish("succeeded", "fully_current");
  }

  finish(outcome: StartupOutcome, reason: StartupReason): void {
    if (this.finished) return;
    this.finished = true;
    const durationMs = this.elapsedMs();
    const details: StartupAggregateDetails = {
      profile: this.profile,
      outcome,
      reason,
      pluginEpoch: this.pluginEpoch,
      activationEpoch: this.activationEpoch,
      pluginLoadCompleteMs: this.pluginLoadCompleteMs,
      layoutReadyMs: this.layoutReadyMs,
      firstProgressMs: this.firstProgressMs,
      firstCacheSearchableMs: this.firstCacheSearchableMs,
      fullyCurrentMs: this.fullyCurrentMs,
      cacheHit: this.cacheHit,
      ...(this.cacheBytes === undefined ? {} : { cacheBytes: this.cacheBytes }),
    };
    try {
      this.record({
        startedAtMs: this.wallStartedAtMs,
        durationMs,
        details,
      });
    } catch {
      // Diagnostics must never affect plugin activation or teardown.
    }
  }

  private elapsedMs(): number {
    let current = this.monotonicStartedAtMs;
    try {
      current = validMonotonicTimestamp(this.monotonicNow());
    } catch {
      // A broken observation clock cannot erase the aggregate. Preserve the last
      // valid elapsed value instead of introducing a negative or non-finite one.
    }
    const elapsed = Math.max(0, Math.round(current - this.monotonicStartedAtMs));
    this.lastElapsedMs = Math.max(this.lastElapsedMs, elapsed);
    return this.lastElapsedMs;
  }
}

function defaultMonotonicNow(): number {
  return typeof performance === "undefined" ? Date.now() : performance.now();
}

function validWallTimestamp(value: number): number {
  if (!Number.isFinite(value) || value < 0 || value > 8.64e15) {
    throw new TypeError("Invalid startup wall timestamp");
  }
  return value;
}

function validMonotonicTimestamp(value: number): number {
  if (!Number.isFinite(value) || value < 0) {
    throw new TypeError("Invalid startup monotonic timestamp");
  }
  return value;
}

function nonNegativeInteger(value: number): number {
  if (!Number.isSafeInteger(value) || value < 0) {
    throw new TypeError("Invalid startup aggregate value");
  }
  return value;
}
