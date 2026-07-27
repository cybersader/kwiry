// SPDX-FileCopyrightText: 2026 cybersader
// SPDX-License-Identifier: GPL-3.0-only
//
// Kwiry Fixtures — a DEVELOPMENT-ONLY Obsidian plugin for field-testing the
// Kwiry Search in-plugin index. It never ships with Kwiry Search.
//
// Why a plugin instead of a script: notes written from outside Obsidian (for
// example a WSL script writing to /mnt/c) may not raise the vault events the
// plugin's live reconciliation listens for. Every operation here goes through
// Obsidian's own Vault API, so create/modify/rename/delete fire exactly as
// they would for a human editing notes.
//
// Content is deterministic and seeded: the same seed always produces the same
// notes, so a search has a KNOWN expected hit count rather than a vague one.

import {
  App,
  Modal,
  Notice,
  Plugin,
  PluginSettingTab,
  Setting,
  TFile,
  TFolder,
  normalizePath,
} from "obsidian";

const FIXTURE_ROOT = "kwiry-fixtures";
const BATCH_SIZE = 50;
const YIELD_MS = 0;

interface FixtureSettings {
  seed: number;
  noteCount: number;
  wordsPerNote: number;
}

const DEFAULT_SETTINGS: FixtureSettings = {
  seed: 1,
  noteCount: 200,
  wordsPerNote: 120,
};

/// Deterministic PRNG (mulberry32): identical output for identical seeds, so
/// a generated corpus is reproducible across machines and runs.
function mulberry32(seed: number): () => number {
  let state = seed >>> 0;
  return () => {
    state = (state + 0x6d2b79f5) | 0;
    let t = Math.imul(state ^ (state >>> 15), 1 | state);
    t = (t + Math.imul(t ^ (t >>> 7), 61 | t)) ^ t;
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
}

const VOCABULARY = (
  "retrieval lexical semantic vault daemon generation manifest chunk heading " +
  "frontmatter wikilink markdown search index worker plugin obsidian knowledge " +
  "workspace reconcile watcher freshness stale current durable atomic rename " +
  "pointer partition capability audit latency throughput bounded deterministic " +
  "corpus evidence verdict measure operation storage machine local derived " +
  "authoritative hash metadata observation planner scope enumeration retention"
).split(" ");

const TOPICS = [
  "architecture", "operations", "meetings", "research", "incidents",
  "designs", "reviews", "journal", "reference", "projects",
];

/// Beacons: unique terms placed in exactly one note each, so a search for a
/// beacon must return exactly one result. These mirror the repository's
/// functional oracles so field results are comparable to the test suite.
const BEACONS = [
  { term: "filenamebeacon", where: "filename" },
  { term: "titlebeacon", where: "title" },
  { term: "aliasbeacon", where: "alias" },
  { term: "tagbeacon", where: "tag" },
  { term: "headingbeacon", where: "heading" },
  { term: "bodybeacon", where: "body" },
  { term: "KWIR-2048", where: "identifier" },
] as const;

function words(random: () => number, count: number): string {
  const parts: string[] = [];
  for (let index = 0; index < count; index += 1) {
    parts.push(VOCABULARY[Math.floor(random() * VOCABULARY.length)]);
  }
  return parts.join(" ");
}

interface GeneratedNote {
  path: string;
  body: string;
}

/// Builds the deterministic note set. Note 0 carries every beacon in its
/// designated position; the rest are ordinary filler with stable content.
function generateNotes(settings: FixtureSettings): GeneratedNote[] {
  const random = mulberry32(settings.seed);
  const notes: GeneratedNote[] = [];

  // One note per beacon so each beacon search has exactly one expected hit.
  for (const beacon of BEACONS) {
    const filename =
      beacon.where === "filename" ? `${beacon.term}.md` : `beacon-${beacon.where}.md`;
    const title = beacon.where === "title" ? `${beacon.term} note` : `beacon ${beacon.where}`;
    const alias = beacon.where === "alias" ? beacon.term : `alias-${beacon.where}`;
    const tag = beacon.where === "tag" ? beacon.term : `topic-${beacon.where}`;
    const heading = beacon.where === "heading" ? `${beacon.term} section` : "Section";
    const bodyBeacon =
      beacon.where === "body" || beacon.where === "identifier" ? `${beacon.term} ` : "";
    notes.push({
      path: `${FIXTURE_ROOT}/beacons/${filename}`,
      body:
        `---\ntitle: ${title}\naliases: [${alias}]\ntags: [${tag}]\n---\n\n` +
        `## ${heading}\n\n${bodyBeacon}${words(random, 40)}\n`,
    });
  }

  for (let index = 0; index < settings.noteCount; index += 1) {
    const topic = TOPICS[index % TOPICS.length];
    const stem = `note-${String(index).padStart(5, "0")}`;
    const sections = 1 + (index % 3);
    let body = `---\ntitle: ${topic} ${stem}\ntags: [${topic}]\n---\n\n`;
    for (let section = 0; section < sections; section += 1) {
      body += `## ${words(random, 3)}\n\n${words(random, settings.wordsPerNote)}\n\n`;
    }
    // A stable per-note unique term: searching it must return exactly one hit.
    body += `unique${index}marker\n`;
    notes.push({ path: `${FIXTURE_ROOT}/${topic}/${stem}.md`, body });
  }
  return notes;
}

async function ensureFolder(app: App, path: string): Promise<void> {
  const normalized = normalizePath(path);
  if (app.vault.getAbstractFileByPath(normalized) instanceof TFolder) return;
  const segments = normalized.split("/");
  let current = "";
  for (const segment of segments) {
    current = current ? `${current}/${segment}` : segment;
    if (!(app.vault.getAbstractFileByPath(current) instanceof TFolder)) {
      try {
        await app.vault.createFolder(current);
      } catch {
        // Folder raced into existence; harmless.
      }
    }
  }
}

function fixtureFiles(app: App): TFile[] {
  return app.vault
    .getMarkdownFiles()
    .filter((file) => file.path.startsWith(`${FIXTURE_ROOT}/`));
}

/// Yields to the event loop between batches so Obsidian's UI stays responsive
/// and the indexer observes events at a realistic pace.
async function inBatches<T>(
  items: T[],
  label: string,
  action: (item: T) => Promise<void>,
): Promise<number> {
  const notice = new Notice(`${label}: 0/${items.length}`, 0);
  let done = 0;
  try {
    for (const item of items) {
      await action(item);
      done += 1;
      if (done % BATCH_SIZE === 0) {
        notice.setMessage(`${label}: ${done}/${items.length}`);
        await new Promise((resolve) => setTimeout(resolve, YIELD_MS));
      }
    }
  } finally {
    notice.hide();
  }
  new Notice(`${label}: ${done} done`);
  return done;
}

export default class KwiryFixturesPlugin extends Plugin {
  settings: FixtureSettings = { ...DEFAULT_SETTINGS };

  async onload(): Promise<void> {
    this.settings = Object.assign({}, DEFAULT_SETTINGS, await this.loadData());
    this.addSettingTab(new FixtureSettingTab(this.app, this));

    this.addCommand({
      id: "generate",
      name: "Generate fixture notes",
      callback: () => void this.generate(),
    });
    this.addCommand({
      id: "modify",
      name: "Modify 10 random fixture notes",
      callback: () => void this.modify(10),
    });
    this.addCommand({
      id: "rename",
      name: "Rename 5 random fixture notes",
      callback: () => void this.rename(5),
    });
    this.addCommand({
      id: "delete",
      name: "Delete 5 random fixture notes",
      callback: () => void this.remove(5),
    });
    this.addCommand({
      id: "burst",
      name: "Burst: rapid mixed create/modify/rename/delete",
      callback: () => void this.burst(),
    });
    this.addCommand({
      id: "clear",
      name: "Delete ALL fixture notes",
      callback: () => new ConfirmModal(this.app, () => void this.clear()).open(),
    });
    this.addCommand({
      id: "oracles",
      name: "Show expected search results",
      callback: () => new OracleModal(this.app, this.settings).open(),
    });
  }

  async saveSettings(): Promise<void> {
    await this.saveData(this.settings);
  }

  private async generate(): Promise<void> {
    const notes = generateNotes(this.settings);
    const folders = new Set(notes.map((note) => note.path.replace(/\/[^/]+$/, "")));
    for (const folder of folders) await ensureFolder(this.app, folder);
    await inBatches(notes, "Generating", async (note) => {
      const path = normalizePath(note.path);
      const existing = this.app.vault.getAbstractFileByPath(path);
      if (existing instanceof TFile) await this.app.vault.modify(existing, note.body);
      else await this.app.vault.create(path, note.body);
    });
  }

  private sample(count: number): TFile[] {
    const files = fixtureFiles(this.app);
    const random = mulberry32(Date.now() >>> 0);
    const picked: TFile[] = [];
    const used = new Set<number>();
    while (picked.length < Math.min(count, files.length)) {
      const index = Math.floor(random() * files.length);
      if (used.has(index)) continue;
      used.add(index);
      picked.push(files[index]);
    }
    return picked;
  }

  private async modify(count: number): Promise<void> {
    const stamp = Date.now();
    await inBatches(this.sample(count), "Modifying", async (file) => {
      const text = await this.app.vault.read(file);
      // A unique, searchable marker proves the edit reached the index.
      await this.app.vault.modify(file, `${text}\nedited${stamp}marker\n`);
    });
    new Notice(`Search "edited${stamp}marker" — expect ${count} hits`, 8000);
  }

  private async rename(count: number): Promise<void> {
    const stamp = Date.now();
    await inBatches(this.sample(count), "Renaming", async (file) => {
      const next = file.path.replace(/\.md$/, `-renamed${stamp}.md`);
      await this.app.fileManager.renameFile(file, normalizePath(next));
    });
    new Notice(`Renamed ${count}; old paths must disappear from results`, 8000);
  }

  private async remove(count: number): Promise<void> {
    const targets = this.sample(count);
    const paths = targets.map((file) => file.path);
    await inBatches(targets, "Deleting", async (file) => {
      await this.app.fileManager.trashFile(file);
    });
    new Notice(`Deleted: ${paths.join(", ")}`, 10000);
  }

  /// Rapid mixed events with no pause: exercises debouncing, event
  /// accumulation, and reconciliation under churn.
  private async burst(): Promise<void> {
    const stamp = Date.now();
    await ensureFolder(this.app, `${FIXTURE_ROOT}/burst`);
    const notice = new Notice("Burst: running…", 0);
    try {
      for (let index = 0; index < 20; index += 1) {
        const path = normalizePath(`${FIXTURE_ROOT}/burst/burst-${stamp}-${index}.md`);
        await this.app.vault.create(path, `burst${stamp}marker note ${index}\n`);
      }
      const created = fixtureFiles(this.app).filter((file) =>
        file.path.includes(`burst-${stamp}-`),
      );
      for (const file of created.slice(0, 10)) {
        await this.app.vault.modify(file, `burst${stamp}marker edited\n`);
      }
      for (const file of created.slice(10, 15)) {
        await this.app.fileManager.renameFile(
          file,
          normalizePath(file.path.replace(".md", "-moved.md")),
        );
      }
      for (const file of created.slice(15, 20)) {
        await this.app.fileManager.trashFile(file);
      }
    } finally {
      notice.hide();
    }
    new Notice(`Burst done. Search "burst${stamp}marker" — expect 15 hits`, 10000);
  }

  private async clear(): Promise<void> {
    await inBatches(fixtureFiles(this.app), "Deleting", async (file) => {
      await this.app.fileManager.trashFile(file);
    });
    const root = this.app.vault.getAbstractFileByPath(FIXTURE_ROOT);
    if (root instanceof TFolder) await this.app.fileManager.trashFile(root);
  }
}

class ConfirmModal extends Modal {
  constructor(app: App, private readonly onConfirm: () => void) {
    super(app);
  }

  onOpen(): void {
    this.contentEl.createEl("h3", { text: `Delete everything under ${FIXTURE_ROOT}/?` });
    this.contentEl.createEl("p", {
      text: "Only generated fixture notes are removed. Other notes are untouched.",
    });
    new Setting(this.contentEl)
      .addButton((button) =>
        button
          .setButtonText("Delete")
          .setWarning()
          .onClick(() => {
            this.close();
            this.onConfirm();
          }),
      )
      .addButton((button) => button.setButtonText("Cancel").onClick(() => this.close()));
  }

  onClose(): void {
    this.contentEl.empty();
  }
}

class OracleModal extends Modal {
  constructor(app: App, private readonly settings: FixtureSettings) {
    super(app);
  }

  onOpen(): void {
    this.contentEl.createEl("h3", { text: "Expected search results" });
    this.contentEl.createEl("p", {
      text: "With the current seed and note count, these searches have known answers:",
    });
    const list = this.contentEl.createEl("ul");
    for (const beacon of BEACONS) {
      list.createEl("li", { text: `"${beacon.term}" → exactly 1 hit (${beacon.where})` });
    }
    list.createEl("li", { text: `"unique0marker" → exactly 1 hit` });
    list.createEl("li", {
      text: `"retrieval" → many hits (common filler word)`,
    });
    list.createEl("li", {
      text: `total fixture notes: ${this.settings.noteCount + BEACONS.length}`,
    });
  }

  onClose(): void {
    this.contentEl.empty();
  }
}

class FixtureSettingTab extends PluginSettingTab {
  constructor(app: App, private readonly plugin: KwiryFixturesPlugin) {
    super(app, plugin);
  }

  display(): void {
    this.containerEl.empty();
    this.containerEl.createEl("h2", { text: "Kwiry Fixtures (development only)" });

    new Setting(this.containerEl)
      .setName("Seed")
      .setDesc("Same seed produces identical notes. Change it for a different corpus.")
      .addText((text) =>
        text.setValue(String(this.plugin.settings.seed)).onChange(async (value) => {
          const parsed = Number.parseInt(value, 10);
          if (Number.isFinite(parsed) && parsed > 0) {
            this.plugin.settings.seed = parsed;
            await this.plugin.saveSettings();
          }
        }),
      );

    new Setting(this.containerEl)
      .setName("Note count")
      .setDesc("How many filler notes to generate, on top of the beacon notes.")
      .addText((text) =>
        text.setValue(String(this.plugin.settings.noteCount)).onChange(async (value) => {
          const parsed = Number.parseInt(value, 10);
          if (Number.isFinite(parsed) && parsed > 0 && parsed <= 50_000) {
            this.plugin.settings.noteCount = parsed;
            await this.plugin.saveSettings();
          }
        }),
      );

    new Setting(this.containerEl)
      .setName("Words per note")
      .setDesc("Bigger notes make a bigger index. 120 is a realistic average.")
      .addText((text) =>
        text.setValue(String(this.plugin.settings.wordsPerNote)).onChange(async (value) => {
          const parsed = Number.parseInt(value, 10);
          if (Number.isFinite(parsed) && parsed > 0 && parsed <= 5_000) {
            this.plugin.settings.wordsPerNote = parsed;
            await this.plugin.saveSettings();
          }
        }),
      );
  }
}
