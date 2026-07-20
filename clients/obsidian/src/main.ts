// SPDX-FileCopyrightText: 2026 cybersader
// SPDX-License-Identifier: GPL-3.0-only
//
// Kwiry Search — Obsidian client for the kwiry search daemon.
// A dumb client by contract: query box, results renderer, status light.
// All chunking, ranking, and index logic lives in the daemon.
//
// Modal UX (keyboard flow, debounce, result-row structure) is informed by
// Omnisearch (https://github.com/scambier/obsidian-omnisearch),
// Copyright Simon Cambier and contributors, GPL-3.0. This file is a new
// implementation against the kwiry HTTP contract; ported code, where
// introduced, carries per-file provenance headers.

import { Notice, Plugin, SuggestModal, TFile, requestUrl } from "obsidian";
import * as fs from "fs";

import { KwiryApiError, KwiryClient, type SearchHit, type SearchMode, type Transport } from "./api";
import { flattenExcerpt, parseExcerpt } from "./excerpt";
import { DEFAULT_SETTINGS, loadSettings, type KwiryPluginSettings } from "./settings";
import { KwirySettingTab } from "./settings-tab";

const STATUS_POLL_MS = 30_000;

const obsidianTransport: Transport = async ({ url, method, headers, body }) => {
  const response = await requestUrl({ url, method, headers, body, throw: false });
  return { status: response.status, text: response.text };
};

export default class KwiryPlugin extends Plugin {
  settings: KwiryPluginSettings = DEFAULT_SETTINGS;
  private statusBar: HTMLElement | null = null;

  async onload(): Promise<void> {
    this.settings = loadSettings(await this.loadData());
    this.addSettingTab(new KwirySettingTab(this.app, this));

    this.addCommand({
      id: "open-search",
      name: "Search notes",
      callback: () => this.openSearch(),
    });
    if (this.settings.showRibbonIcon) {
      this.addRibbonIcon("search", "Kwiry search", () => this.openSearch());
    }

    this.statusBar = this.addStatusBarItem();
    this.statusBar.setText("kwiry: …");
    this.registerInterval(
      window.setInterval(() => void this.refreshStatus(), STATUS_POLL_MS),
    );
    void this.refreshStatus();
  }

  async saveSettings(): Promise<void> {
    await this.saveData(this.settings);
  }

  /** Reads the bearer token fresh from the configured file (never stored). */
  readToken(): string {
    const path = this.settings.tokenFilePath.trim();
    if (!path) {
      throw new Error("set the token file path in Kwiry Search settings");
    }
    return fs.readFileSync(path, "utf-8").trim();
  }

  makeClient(): KwiryClient {
    return new KwiryClient({
      baseUrl: this.settings.daemonUrl,
      token: this.readToken(),
      transport: obsidianTransport,
    });
  }

  private openSearch(): void {
    try {
      new KwirySearchModal(this, this.makeClient()).open();
    } catch (error) {
      new Notice(`Kwiry: ${error instanceof Error ? error.message : String(error)}`);
    }
  }

  async refreshStatus(): Promise<void> {
    if (!this.statusBar) return;
    try {
      const status = await this.makeClient().status();
      const semantic = status.model ? "+semantic" : "lexical";
      this.statusBar.setText(`kwiry: ${status.state} (${status.chunks} chunks, ${semantic})`);
    } catch {
      this.statusBar.setText("kwiry: offline");
    }
  }
}

interface ModalResult {
  hit: SearchHit;
}

class KwirySearchModal extends SuggestModal<ModalResult> {
  private mode: SearchMode;
  private generation = 0;

  constructor(
    private readonly plugin: KwiryPlugin,
    private readonly client: KwiryClient,
  ) {
    super(plugin.app);
    this.mode = plugin.settings.defaultMode;
    this.setPlaceholder("Search your notes with kwiry…");
    this.setInstructions([
      { command: "↵", purpose: "open" },
      { command: "ctrl ↵", purpose: "open in new tab" },
      { command: "tab", purpose: "cycle mode (lexical/semantic/hybrid)" },
    ]);
    this.scope.register([], "Tab", (event) => {
      event.preventDefault();
      this.mode = this.mode === "lexical" ? "semantic" : this.mode === "semantic" ? "hybrid" : "lexical";
      this.setPlaceholder(`Search (${this.mode})…`);
      // Re-trigger the current query under the new mode.
      this.inputEl.dispatchEvent(new Event("input"));
      return false;
    });
  }

  async getSuggestions(query: string): Promise<ModalResult[]> {
    const trimmed = query.trim();
    if (!trimmed) return [];
    const generation = ++this.generation;
    try {
      const filters =
        this.plugin.settings.vaultId.trim().length > 0
          ? { vault_id: this.plugin.settings.vaultId.trim() }
          : undefined;
      const response = await this.client.search({
        q: trimmed,
        mode: this.mode,
        limit: this.plugin.settings.resultLimit,
        filters,
      });
      // Drop stale completions: only the latest query may render.
      if (generation !== this.generation) return [];
      return response.hits.map((hit) => ({ hit }));
    } catch (error) {
      if (generation !== this.generation) return [];
      if (error instanceof KwiryApiError && error.code === "mode_unavailable") {
        new Notice(`Kwiry: ${this.mode} mode needs the daemon running with --semantic`);
      } else if (error instanceof KwiryApiError && error.code === "daemon_unreachable") {
        new Notice("Kwiry: daemon unreachable — is `kwiry serve` running?");
      } else if (error instanceof KwiryApiError) {
        new Notice(`Kwiry: ${error.message}`);
      }
      return [];
    }
  }

  renderSuggestion(result: ModalResult, el: HTMLElement): void {
    const { hit } = result;
    el.addClass("kwiry-result");
    const title = el.createDiv({ cls: "kwiry-result-title" });
    title.setText(hit.frontmatter.title ?? basename(hit.path));
    const meta = el.createDiv({ cls: "kwiry-result-meta" });
    const breadcrumb =
      hit.heading_path.length > 0 ? ` › ${hit.heading_path.join(" › ")}` : "";
    meta.setText(`${hit.path}${breadcrumb}`);
    const excerpt = el.createDiv({ cls: "kwiry-result-excerpt" });
    for (const segment of flattenExcerpt(parseExcerpt(hit.excerpt))) {
      if (segment.highlighted) {
        excerpt.createEl("mark", { text: segment.text });
      } else {
        excerpt.appendText(segment.text);
      }
    }
  }

  onChooseSuggestion(result: ModalResult, event: MouseEvent | KeyboardEvent): void {
    const file = this.app.vault.getAbstractFileByPath(result.hit.path);
    if (!(file instanceof TFile)) {
      new Notice(
        `Kwiry: "${result.hit.path}" is not in this vault (registered tree "${result.hit.vault_id}")`,
      );
      return;
    }
    const newTab = event.ctrlKey || event.metaKey;
    const heading = result.hit.heading_path.at(-1);
    const leaf = this.app.workspace.getLeaf(newTab);
    void leaf.openFile(file, {
      eState: heading ? { subpath: `#${heading}` } : undefined,
    });
  }
}

function basename(path: string): string {
  const slash = path.lastIndexOf("/");
  const name = slash >= 0 ? path.slice(slash + 1) : path;
  return name.replace(/\.md$/, "");
}
