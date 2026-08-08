import { describe, expect, it } from "vitest";
import { DiagnosticLog } from "../src/diagnostics/log";
import { classifyFailure } from "../src/diagnostics/classify-failure";

describe("errorName field", () => {
  it("accepts a platform error class name the closed list never had", async () => {
    const log = new DiagnosticLog();
    await log.capture("error", "failure.caught", { errorName: "DataCloneError" }, () => undefined);
    expect(JSON.stringify(log.snapshot())).toContain("DataCloneError");
  });

  it("rejects anything that is not a bare identifier", async () => {
    for (const bad of [
      "Clients/Acme Q3.md",
      "failed to read note",
      "Error: /vault/secret.md",
      'a"b',
      "with space",
    ]) {
      const log = new DiagnosticLog();
      await expect(
        log.capture("error", "failure.caught", { errorName: bad } as never, () => undefined),
      ).rejects.toThrow("Invalid diagnostic details");
    }
  });

  it("classifier echoes an unknown identifier but not a sentence", () => {
    expect(classifyFailure(Object.assign(new Error("x"), { name: "QuotaExceededError" })).errorName)
      .toBe("QuotaExceededError");
    expect(classifyFailure(Object.assign(new Error("x"), { name: "Clients/Acme leak" })).errorName)
      .toBe("other");
  });
});
