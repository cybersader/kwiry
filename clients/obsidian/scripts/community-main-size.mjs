// SPDX-FileCopyrightText: 2026 cybersader
// SPDX-License-Identifier: GPL-3.0-only

const COMMUNITY_MAIN_LIMIT = 8 * 1024 * 1024;

export function assertCommunityMainSize(bytes) {
  if (bytes > COMMUNITY_MAIN_LIMIT) {
    throw new Error(`Production main.js is ${bytes} bytes; the community install limit is ${COMMUNITY_MAIN_LIMIT} bytes`);
  }
}
