// SPDX-FileCopyrightText: 2026 cybersader
// SPDX-License-Identifier: GPL-3.0-only

import * as fs from "fs";
import * as path from "path";

const MAX_TOKEN_FILE_BYTES = 128;
const TOKEN_PATTERN = /^[A-Za-z0-9_-]{43}$/u;

export type DaemonCredentialErrorCode =
  | "invalid_daemon_url"
  | "token_unavailable";

export class DaemonCredentialError extends Error {
  constructor(
    public readonly code: DaemonCredentialErrorCode,
    message: string,
  ) {
    super(message);
    this.name = "DaemonCredentialError";
  }
}

export function normalizeDaemonBaseUrl(raw: string): string {
  const trimmed = raw.trim();
  let parsed: URL;
  try {
    parsed = new URL(trimmed);
  } catch {
    throw new DaemonCredentialError(
      "invalid_daemon_url",
      "Daemon URL must be a literal loopback HTTP origin with an explicit port.",
    );
  }

  const hostname = parsed.hostname.startsWith("[") && parsed.hostname.endsWith("]")
    ? parsed.hostname.slice(1, -1)
    : parsed.hostname;
  if (
    parsed.protocol !== "http:"
    || !parsed.port
    || parsed.username
    || parsed.password
    || parsed.pathname !== "/"
    || parsed.search
    || parsed.hash
    || !isLiteralLoopback(hostname)
  ) {
    throw new DaemonCredentialError(
      "invalid_daemon_url",
      "Daemon URL must be a literal loopback HTTP origin with an explicit port.",
    );
  }
  return parsed.origin;
}

export function normalizeDaemonToken(raw: string): string {
  const token = raw.trim();
  if (!TOKEN_PATTERN.test(token)) {
    throw new DaemonCredentialError(
      "token_unavailable",
      "Daemon token file does not contain a valid Kwiry token.",
    );
  }
  return token;
}

export function readDaemonToken(tokenFilePath: string): string {
  const configuredPath = tokenFilePath.trim();
  if (!configuredPath || !path.isAbsolute(configuredPath)) {
    throw new DaemonCredentialError(
      "token_unavailable",
      "Set an absolute daemon token file path in Kwiry Search settings.",
    );
  }

  let descriptor: number | null = null;
  try {
    const pathMetadata = fs.lstatSync(configuredPath);
    if (pathMetadata.isSymbolicLink() || !pathMetadata.isFile()) {
      throw new DaemonCredentialError(
        "token_unavailable",
        "Daemon token file must be a small regular file, not a link.",
      );
    }

    descriptor = fs.openSync(
      configuredPath,
      fs.constants.O_RDONLY | fs.constants.O_NOFOLLOW,
    );
    const openedMetadata = fs.fstatSync(descriptor);
    if (!openedMetadata.isFile() || openedMetadata.size > MAX_TOKEN_FILE_BYTES) {
      throw new DaemonCredentialError(
        "token_unavailable",
        "Daemon token file must be a small regular file, not a link.",
      );
    }

    const bounded = Buffer.alloc(MAX_TOKEN_FILE_BYTES + 1);
    const bytesRead = fs.readSync(descriptor, bounded, 0, bounded.length, 0);
    if (bytesRead > MAX_TOKEN_FILE_BYTES) {
      throw new DaemonCredentialError(
        "token_unavailable",
        "Daemon token file must be a small regular file, not a link.",
      );
    }
    return normalizeDaemonToken(bounded.toString("utf8", 0, bytesRead));
  } catch (error) {
    if (error instanceof DaemonCredentialError) throw error;
    throw new DaemonCredentialError("token_unavailable", "Daemon token file is unavailable.");
  } finally {
    if (descriptor !== null) fs.closeSync(descriptor);
  }
}

function isLiteralLoopback(hostname: string): boolean {
  if (hostname === "::1") return true;
  const octets = hostname.split(".");
  if (octets.length !== 4 || octets[0] !== "127") return false;
  return octets.every((octet) => {
    if (!/^\d{1,3}$/u.test(octet)) return false;
    const value = Number(octet);
    return value >= 0 && value <= 255 && String(value) === octet;
  });
}
