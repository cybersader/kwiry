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
