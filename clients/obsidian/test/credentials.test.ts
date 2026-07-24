// SPDX-FileCopyrightText: 2026 cybersader
// SPDX-License-Identifier: GPL-3.0-only

import * as fs from "fs";
import * as os from "os";
import * as path from "path";
import { afterEach, describe, expect, it } from "vitest";

import {
  normalizeDaemonBaseUrl,
  normalizeDaemonToken,
  readDaemonToken,
} from "../src/credentials";

const TOKEN = "A".repeat(43);
const temporaryDirectories: string[] = [];

afterEach(() => {
  for (const directory of temporaryDirectories.splice(0)) {
    fs.rmSync(directory, { recursive: true, force: true });
  }
});

function temporaryDirectory(): string {
  const directory = fs.mkdtempSync(path.join(os.tmpdir(), "kwiry-credentials-"));
  temporaryDirectories.push(directory);
  return directory;
}

describe("daemon credential validation", () => {
  it("accepts only literal loopback HTTP origins with explicit ports", () => {
    expect(normalizeDaemonBaseUrl("http://127.0.0.1:32189/")).toBe(
      "http://127.0.0.1:32189",
    );
    expect(normalizeDaemonBaseUrl("http://127.1.2.3:1234")).toBe(
      "http://127.1.2.3:1234",
    );
    expect(normalizeDaemonBaseUrl("http://[::1]:32189")).toBe(
      "http://[::1]:32189",
    );
    for (const rejected of [
      "http://localhost:32189",
      "https://127.0.0.1:32189",
      "http://127.0.0.1",
      "http://user@127.0.0.1:32189",
      "http://127.0.0.1:32189/v0",
      "http://127.0.0.1:32189?next=remote",
      "http://192.168.1.4:32189",
    ]) {
      expect(() => normalizeDaemonBaseUrl(rejected)).toThrow(/loopback/i);
    }
  });

  it("accepts only the daemon token encoding", () => {
    expect(normalizeDaemonToken(` ${TOKEN}\n`)).toBe(TOKEN);
    expect(() => normalizeDaemonToken("short")).toThrow(/valid Kwiry token/);
    expect(() => normalizeDaemonToken("/".repeat(43))).toThrow(/valid Kwiry token/);
  });

  it("reads a bounded regular token file", () => {
    const directory = temporaryDirectory();
    const tokenPath = path.join(directory, "config.token");
    fs.writeFileSync(tokenPath, `${TOKEN}\n`);
    expect(readDaemonToken(tokenPath)).toBe(TOKEN);
  });

  it("rejects links, directories, oversized files, and relative paths", () => {
    const directory = temporaryDirectory();
    const tokenPath = path.join(directory, "config.token");
    const linkPath = path.join(directory, "token-link");
    const oversizedPath = path.join(directory, "oversized.token");
    fs.writeFileSync(tokenPath, TOKEN);
    fs.symlinkSync(tokenPath, linkPath);
    fs.writeFileSync(oversizedPath, "A".repeat(129));

    expect(() => readDaemonToken(linkPath)).toThrow(/regular file/);
    expect(() => readDaemonToken(directory)).toThrow(/regular file/);
    expect(() => readDaemonToken(oversizedPath)).toThrow(/regular file/);
    expect(() => readDaemonToken("relative.token")).toThrow(/absolute/);
  });
});
