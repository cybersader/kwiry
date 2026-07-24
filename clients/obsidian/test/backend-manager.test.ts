// SPDX-FileCopyrightText: 2026 cybersader
// SPDX-License-Identifier: GPL-3.0-only

import { describe, expect, it } from "vitest";

import type { SearchRequest } from "../src/api";
import { BackendManager } from "../src/backend-manager";
import type {
  BackendIdentity,
  BackendProfile,
  BackendStatus,
  SearchBackend,
  SearchExecution,
} from "../src/backend";

class RecordingBackend implements SearchBackend {
  readonly identity: BackendIdentity;

  constructor(
    profile: BackendProfile,
    instanceId: string,
    private readonly events: string[],
    private readonly failInitialize = false,
  ) {
    this.identity = {
      profile,
      instanceId,
      label: profile === "daemon" ? "Daemon" : "In-plugin",
      boundVaultId: null,
    };
  }

  async initialize(): Promise<void> {
    this.events.push(`initialize:${this.identity.instanceId}`);
    if (this.failInitialize) throw new Error("initialization failed");
  }

  async status(): Promise<BackendStatus> {
    throw new Error("unused");
  }

  async search(_request: SearchRequest): Promise<SearchExecution> {
    throw new Error("unused");
  }

  async dispose(): Promise<void> {
    this.events.push(`dispose:${this.identity.instanceId}`);
  }
}

describe("BackendManager", () => {
  it("serializes explicit activation, disposes the previous backend, and assigns new identities", async () => {
    const events: string[] = [];
    const manager = new BackendManager({
      daemon: (instanceId) => new RecordingBackend("daemon", instanceId, events),
      in_plugin: (instanceId) => new RecordingBackend("in_plugin", instanceId, events),
    });

    const first = await manager.activate("daemon");
    const second = await manager.activate("in_plugin");

    expect(first.identity.instanceId).toBe("daemon-1");
    expect(second.identity.instanceId).toBe("in_plugin-2");
    expect(await manager.current()).toBe(second);
    expect(events).toEqual([
      "initialize:daemon-1",
      "dispose:daemon-1",
      "initialize:in_plugin-2",
    ]);

    await manager.dispose();
    expect(events.at(-1)).toBe("dispose:in_plugin-2");
  });

  it("never constructs the other profile when explicit activation fails", async () => {
    const events: string[] = [];
    let inPluginConstructions = 0;
    const manager = new BackendManager({
      daemon: (instanceId) => new RecordingBackend("daemon", instanceId, events, true),
      in_plugin: (instanceId) => {
        inPluginConstructions += 1;
        return new RecordingBackend("in_plugin", instanceId, events);
      },
    });

    await expect(manager.activate("daemon")).rejects.toThrow("initialization failed");
    expect(inPluginConstructions).toBe(0);
    expect(events).toEqual(["initialize:daemon-1"]);
  });

  it("requires an explicit backend selection", async () => {
    const manager = new BackendManager({
      daemon: (instanceId) => new RecordingBackend("daemon", instanceId, []),
      in_plugin: (instanceId) => new RecordingBackend("in_plugin", instanceId, []),
    });

    await expect(manager.current()).rejects.toMatchObject({ code: "backend_not_selected" });
  });
});
