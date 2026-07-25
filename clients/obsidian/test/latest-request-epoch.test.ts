// SPDX-FileCopyrightText: 2026 cybersader
// SPDX-License-Identifier: GPL-3.0-only

import { describe, expect, it } from "vitest";

import { LatestRequestEpoch } from "../src/latest-request-epoch";

describe("LatestRequestEpoch", () => {
  it("accepts only the newest overlapping request", () => {
    const epoch = new LatestRequestEpoch();
    const older = epoch.begin();
    const newer = epoch.begin();

    expect(epoch.isCurrent(older)).toBe(false);
    expect(epoch.isCurrent(newer)).toBe(true);
  });

  it("invalidates a pending request when push status or lifecycle state changes", () => {
    const epoch = new LatestRequestEpoch();
    const pending = epoch.begin();
    epoch.invalidate();

    expect(epoch.isCurrent(pending)).toBe(false);
  });
});
