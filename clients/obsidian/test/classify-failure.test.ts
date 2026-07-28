// SPDX-FileCopyrightText: 2026 cybersader
// SPDX-License-Identifier: GPL-3.0-only

import { describe, expect, it } from "vitest";

import { classifyFailure } from "../src/diagnostics/classify-failure";

describe("classifyFailure", () => {
  it("names the subsystem for a worker failure", () => {
    expect(classifyFailure(new Error("worker terminated unexpectedly")))
      .toMatchObject({ subsystem: "worker", reason: "worker_failed" });
  });

  it("distinguishes a VFS failure from a generic worker failure", () => {
    expect(classifyFailure(new Error("BlockVfsUnavailableError: install failed")).subsystem)
      .toBe("vfs");
  });

  it("never returns the message text", () => {
    const secret = "Clients/Acme Q3.md could not be read";
    const result = classifyFailure(new Error(secret));
    expect(JSON.stringify(result)).not.toContain("Acme");
  });

  it("survives a throwing getter instead of adopting its failure", () => {
    // A rejected value can carry a hostile or broken getter. Reading it
    // naively replaces the real cause with the getter's error, which is the
    // one failure mode diagnostics must never introduce.
    const hostile = {
      get message(): string {
        throw new Error("getter exploded");
      },
    };
    expect(() => classifyFailure(hostile)).not.toThrow();
    expect(classifyFailure(hostile).nonError).toBe(true);
  });

  it("flags a non-Error rejection, which is itself a defect", () => {
    expect(classifyFailure("just a string").nonError).toBe(true);
    expect(classifyFailure(undefined).nonError).toBe(true);
  });
});

describe("classifyFailure error name", () => {
  it("names a known project error class exactly", () => {
    class RustAdapterError extends Error {
      override readonly name = "RustAdapterError";
    }
    expect(classifyFailure(new RustAdapterError("init")).errorName).toBe("RustAdapterError");
  });

  it("reports a bare runtime fault, which the patterns cannot recognise", () => {
    // The first field report classified as unknown/internal_error: a real
    // Error whose text matched no pattern. The class name distinguishes a
    // programming fault from a handled subsystem failure.
    expect(classifyFailure(new RangeError("out of bounds")).errorName).toBe("RangeError");
    expect(classifyFailure(new Error("something went wrong")).errorName).toBe("Error");
  });

  it("does not echo an unrecognised class name", () => {
    class SecretNamedError extends Error {
      override readonly name = "AcmeClientDataError";
    }
    const result = classifyFailure(new SecretNamedError("x"));
    expect(result.errorName).toBe("other");
    expect(JSON.stringify(result)).not.toContain("Acme");
  });
});

describe("classifyFailure worker protocol errors", () => {
  it("reads the protocol code from a plain WorkerError object", () => {
    // Regression: three field reports classified as "other" because a
    // WorkerError is {code, stage, message, retryable}, not an Error subclass,
    // so it carries no constructor name at all.
    const workerError = {
      code: "rust_init_failed",
      stage: "lifecycle",
      message: "adapter failed to start",
      retryable: false,
    };
    expect(classifyFailure(workerError)).toMatchObject({
      workerCode: "rust_init_failed",
      subsystem: "worker",
    });
  });

  it("routes a SQLite init failure to the vfs subsystem", () => {
    expect(classifyFailure({ code: "sqlite_init_failed" }).subsystem).toBe("vfs");
    expect(classifyFailure({ code: "fts5_unavailable" }).subsystem).toBe("vfs");
  });

  it("ignores an unrecognised code rather than echoing it", () => {
    const result = classifyFailure({ code: "Clients/Acme secret" });
    expect(result.workerCode).toBeUndefined();
    expect(JSON.stringify(result)).not.toContain("Acme");
  });
});
