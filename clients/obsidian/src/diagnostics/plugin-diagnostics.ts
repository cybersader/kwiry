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
} from "./log";
import type { DiagnosticsLogLevel } from "../settings";

const NOOP_EVENT: DiagnosticEventBuilder = {
  set: () => undefined,
  increment: () => undefined,
  setLevel: () => undefined,
};

export class PluginDiagnostics {
  private log = new DiagnosticLog();

  constructor(private level: DiagnosticsLogLevel) {}

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

  format(context: DiagnosticExportContext): string {
    try {
      return formatDiagnosticLog(this.log, context, minimumLevel(this.level));
    } catch {
      return "Kwiry diagnostics log\nreport_unavailable: true\n";
    }
  }

  clear(): void {
    // Replacing the ring also isolates it from operations that finish after
    // unload; their old captures cannot repopulate the cleared report.
    this.log = new DiagnosticLog();
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
