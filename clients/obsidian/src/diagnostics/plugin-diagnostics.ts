// SPDX-FileCopyrightText: 2026 cybersader
// SPDX-License-Identifier: GPL-3.0-only
//
// This adapter keeps Obsidian wiring independent of the storage primitive and
// makes diagnostics failures invisible to the operation being observed.

import {
  DiagnosticLog,
  formatDiagnosticLog,
  type DiagnosticDetails,
  type DiagnosticEventBuilder,
  type DiagnosticEventCode,
  type DiagnosticExportContext,
  type DiagnosticLevel,
  type DiagnosticReportOptions,
} from "./log";
import type { DiagnosticsLogLevel } from "../settings";
import type { StartupTimelineRecord } from "./startup-timeline";

const NOOP_EVENT: DiagnosticEventBuilder = {
  set: () => undefined,
  increment: () => undefined,
  setLevel: () => undefined,
};

export class PluginDiagnostics {
  private log: DiagnosticLog;

  constructor(
    private level: DiagnosticsLogLevel,
    private readonly createLog: () => DiagnosticLog = () => new DiagnosticLog(),
  ) {
    this.log = this.createLog();
  }

  setLevel(level: DiagnosticsLogLevel): void {
    this.level = level;
  }

  async capture<T>(
    level: DiagnosticLevel,
    code: DiagnosticEventCode,
    details: Readonly<DiagnosticDetails>,
    operation: (event: DiagnosticEventBuilder) => T | Promise<T>,
  ): Promise<T> {
    if (!this.shouldCapture(level)) return operation(NOOP_EVENT);

    let operationStarted = false;
    let operationSucceeded = false;
    let result: T | undefined;
    let operationError: unknown;
    try {
      return await this.log.capture(level, code, details, async (event) => {
        operationStarted = true;
        try {
          result = await operation(safeEvent(event));
          operationSucceeded = true;
          return result;
        } catch (error) {
          operationError = error;
          throw error;
        }
      });
    } catch (error) {
      if (operationError !== undefined) throw operationError;
      if (operationSucceeded) return result as T;
      if (operationStarted) throw error;
      return operation(NOOP_EVENT);
    }
  }

  recordStartup(record: StartupTimelineRecord): void {
    if (!this.shouldCapture("info")) return;
    try {
      this.log.record(
        "info",
        "startup.lifecycle",
        record.startedAtMs,
        record.durationMs,
        record.details,
      );
    } catch {
      // Diagnostics failures cannot affect startup or teardown.
    }
  }

  format(context: DiagnosticExportContext, options: DiagnosticReportOptions = {}): string {
    try {
      return formatDiagnosticLog(this.log, context, {
        // Capture level is still the floor: a report can narrow what was
        // recorded but can never widen it into events that were never kept.
        minimumLevel: widerOf(minimumLevel(this.level), options.minimumLevel),
        ...(options.categories === undefined ? {} : { categories: options.categories }),
        ...(options.includeStructuredRecords === undefined
          ? {}
          : { includeStructuredRecords: options.includeStructuredRecords }),
      });
    } catch {
      return "Kwiry diagnostics log\nreport_unavailable: true\n";
    }
  }

  clear(): void {
    // Replacing the ring also isolates it from operations that finish after
    // unload; their old captures cannot repopulate the cleared report.
    this.log = this.createLog();
  }

  private shouldCapture(level: DiagnosticLevel): boolean {
    if (this.level === "off") return false;
    // An operation that starts at info can still fail and be promoted to error
    // by the core, so error-only exports must keep those scoped captures.
    return level !== "debug";
  }
}

function safeEvent(event: DiagnosticEventBuilder): DiagnosticEventBuilder {
  return {
    set: (details) => {
      try {
        event.set(details);
      } catch {
        // Invalid diagnostics must not interrupt the observed operation.
      }
    },
    increment: (counter, amount) => {
      try {
        event.increment(counter, amount);
      } catch {
        // Invalid diagnostics must not interrupt the observed operation.
      }
    },
    setLevel: (level) => {
      try {
        event.setLevel(level);
      } catch {
        // Invalid diagnostics must not interrupt the observed operation.
      }
    },
  };
}

function minimumLevel(level: DiagnosticsLogLevel): DiagnosticLevel {
  return level === "error" || level === "off" ? "error" : "info";
}

const REPORT_LEVEL_ORDER: readonly DiagnosticLevel[] = ["debug", "info", "warn", "error"];

/// Returns whichever threshold keeps fewer events, so a requested report level
/// can narrow the capture floor but never reach below it.
function widerOf(
  capture: DiagnosticLevel,
  requested: DiagnosticLevel | undefined,
): DiagnosticLevel {
  if (requested === undefined) return capture;
  return REPORT_LEVEL_ORDER.indexOf(requested) > REPORT_LEVEL_ORDER.indexOf(capture)
    ? requested
    : capture;
}
