// SPDX-FileCopyrightText: 2026 cybersader
// SPDX-License-Identifier: GPL-3.0-only

export class LatestRequestEpoch {
  private epoch = 0;

  begin(): number {
    this.epoch += 1;
    return this.epoch;
  }

  invalidate(): void {
    this.epoch += 1;
  }

  isCurrent(epoch: number): boolean {
    return epoch === this.epoch;
  }
}
