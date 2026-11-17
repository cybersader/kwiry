import { describe, expect, it } from "vitest";
import { checkVersions } from "../scripts/check-obsidian-version.mjs";

function fixture(version = "1.2.3") {
  return [
    { version, minAppVersion: "1.7.2" },
    { version },
    { version, packages: { "": { version } } },
    { [version]: "1.7.2" },
  ];
}

describe("Obsidian release version gate", () => {
  it.each(["1.2.3", "1.2.3-beta.4"])("accepts matching stable and BRAT versions: %s", (version) => {
    expect(checkVersions(...fixture(version), version)).toBe(version);
  });

  it.each(["v1.2.3", "1.2.3-beta.5"])("rejects a different or prefixed tag: %s", (tag) => {
    expect(() => checkVersions(...fixture(), tag)).toThrow("release tag must equal");
  });

  it.each([0, 1, 2, 3])("rejects a mismatch in version source %i", (index) => {
    const values = fixture();
    if (index === 3) values[3]["1.2.3"] = "1.8.0";
    else values[index].version = "1.2.4";
    expect(() => checkVersions(...values)).toThrow("must agree");
  });

  it("rejects a mismatched package-lock root", () => {
    const values = fixture();
    values[2].packages[""].version = "1.2.4";
    expect(() => checkVersions(...values)).toThrow("must agree");
  });
});
