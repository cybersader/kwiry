// SPDX-FileCopyrightText: 2026 cybersader
// SPDX-License-Identifier: GPL-3.0-only

import { Modal, Notice } from "obsidian";

import type KwiryPlugin from "../../main";
import {
  D5cPlaygroundSession,
  projectPlaygroundRun,
  type BalancedExplanation,
  type PlaygroundCaseSummary,
  type PlaygroundRun,
  type RankingPanelView,
} from "./session";
import type { D5cExplanationLevel } from "./settings";

export class D5cPlaygroundModal extends Modal {
  private readonly session: D5cPlaygroundSession;
  private selectedOrdinal = 0;
  private controls: HTMLButtonElement[] = [];
  private outputEl: HTMLElement | null = null;
  private statusEl: HTMLElement | null = null;
  private closed = false;

  constructor(
    private readonly plugin: KwiryPlugin,
    workerSource: string,
    fixtureCorpus: unknown,
    explanationLevel: D5cExplanationLevel,
    private readonly didClose: () => void,
  ) {
    super(plugin.app);
    this.session = new D5cPlaygroundSession(workerSource, fixtureCorpus, explanationLevel);
  }

  onOpen(): void {
    this.setTitle("Private D5C Balanced playground");
    this.contentEl.empty();
    this.contentEl.createEl("p", {
      text: "Fixture-only internal evaluation. It never reads the active vault or changes normal search.",
      cls: "kwiry-d5c-boundary",
    });
    this.renderControls();
    this.statusEl = this.contentEl.createDiv({ cls: "kwiry-d5c-status" });
    this.outputEl = this.contentEl.createDiv({ cls: "kwiry-d5c-output" });
    void this.start();
  }

  onClose(): void {
    if (this.closed) return;
    this.closed = true;
    this.contentEl.empty();
    void this.session.dispose();
    this.didClose();
  }

  private renderControls(): void {
    const controls = this.contentEl.createDiv({ cls: "kwiry-d5c-controls" });
    const selector = controls.createEl("select", { attr: { "aria-label": "Fixture case" } });
    for (const fixtureCase of this.session.cases) {
      selector.createEl("option", {
        value: String(fixtureCase.ordinal),
        text: caseLabel(fixtureCase),
      });
    }
    selector.addEventListener("change", () => {
      const ordinal = Number.parseInt(selector.value, 10);
      if (Number.isSafeInteger(ordinal)) this.selectedOrdinal = ordinal;
    });

    this.controls = [
      this.controlButton(controls, "Run selected", () => this.runSelected()),
      this.controlButton(controls, "Deterministic rerun", () => this.runSelected()),
      this.controlButton(controls, "Run all fixture cases", () => this.runAll()),
      this.controlButton(controls, "Copy aggregate export", () => this.copyAggregate()),
    ];
  }

  private controlButton(
    container: HTMLElement,
    label: string,
    action: () => Promise<void>,
  ): HTMLButtonElement {
    const button = container.createEl("button", { text: label });
    button.addEventListener("click", () => void this.withBusy(action));
    return button;
  }

  private async start(): Promise<void> {
    await this.withBusy(async () => {
      this.setStatus("Initializing bounded fixture corpus…");
      await this.session.initialize();
      await this.runSelected();
    });
  }

  private async runSelected(): Promise<void> {
    this.setStatus("Running Rust-owned comparison…");
    const run = await this.session.evaluate(this.selectedOrdinal);
    this.renderRun(run);
    this.setStatus("Fixture comparison ready.");
  }

  private async runAll(): Promise<void> {
    this.setStatus("Running all bounded fixture cases…");
    const runs = await this.session.runAll();
    const selected = runs[this.selectedOrdinal];
    if (selected) this.renderRun(selected);
    this.setStatus(`Completed ${runs.length} fixture comparisons.`);
  }

  private async copyAggregate(): Promise<void> {
    const aggregate = this.session.aggregateExport();
    await navigator.clipboard.writeText(`${JSON.stringify(aggregate, null, 2)}\n`);
    new Notice("Kwiry: aggregate-only playground export copied.");
  }

  private async withBusy(action: () => Promise<void>): Promise<void> {
    this.setBusy(true);
    try {
      await action();
    } catch {
      this.setStatus("Private playground operation failed.");
      new Notice("Kwiry: the private fixture playground could not complete the operation.");
    } finally {
      this.setBusy(false);
    }
  }

  private setBusy(busy: boolean): void {
    for (const control of this.controls) control.disabled = busy;
  }

  private setStatus(text: string): void {
    this.statusEl?.setText(text);
  }

  private renderRun(run: PlaygroundRun): void {
    const output = this.outputEl;
    if (!output) return;
    output.empty();
    const view = projectPlaygroundRun(run);
    output.createEl("h3", { text: view.status, cls: `kwiry-d5c-${view.statusKind}` });
    output.createEl("p", { text: view.discrepancySummary });
    output.createEl("p", { text: view.propertyPack });
    output.createEl("p", { text: view.deterministic });

    const panels = output.createDiv({ cls: "kwiry-d5c-ranking-panels" });
    renderRankingPanel(panels, view.text);
    if (view.balanced) renderRankingPanel(panels, view.balanced);
    renderExplanation(output, view.explanation);
  }
}

function renderRankingPanel(container: HTMLElement, panel: RankingPanelView): void {
  const section = container.createDiv({ cls: `kwiry-d5c-ranking kwiry-d5c-ranking-${panel.kind}` });
  section.createEl("h4", { text: panel.heading });
  section.createEl("p", { text: panel.qualifier });
  const list = section.createEl("ol");
  for (const row of panel.rows) {
    const item = list.createEl("li");
    item.createEl("span", { text: `Candidate ${row.candidateOrdinal + 1}` });
    item.createEl("span", { text: row.tier, cls: "kwiry-d5c-tier-badge" });
    item.createEl("span", { text: `Metadata ${signed(row.metadataPoints)}` });
    if (row.movement !== null) {
      item.createEl("span", { text: movementLabel(row.movement) });
    }
  }
}

function renderExplanation(container: HTMLElement, explanation: BalancedExplanation | null): void {
  const section = container.createDiv({ cls: "kwiry-d5c-explanation" });
  section.createEl("h4", { text: "Rust-owned explanation" });
  if (!explanation) {
    section.createEl("p", { text: "No explanation projection." });
    return;
  }
  const summary = explanation.summary;
  section.createEl("p", {
    text: [
      `${summary.candidate_count} candidates`,
      `${summary.moved_candidate_count} moved`,
      `${summary.matched_signal_count} matched`,
      `${summary.nonmatched_signal_count} nonmatched`,
      `${summary.absent_signal_count} absent`,
      `${summary.neutralized_signal_count} neutralized`,
    ].join(" · "),
  });
  if (explanation.level !== "rules") return;
  for (const candidate of explanation.rules) {
    const block = section.createDiv({ cls: "kwiry-d5c-rule-candidate" });
    block.createEl("strong", { text: `Candidate ${candidate.candidate_ordinal + 1}` });
    const list = block.createEl("ul");
    for (const rule of candidate.rules) {
      const ruleName = rule.rule.kind === "property"
        ? `Property rule ${rule.rule.ordinal + 1}`
        : humanize(rule.rule.kind);
      list.createEl("li", {
        text: `${ruleName} · ${humanize(rule.outcome)} · ${signed(rule.points)}`,
      });
    }
  }
}

function caseLabel(fixtureCase: PlaygroundCaseSummary): string {
  const engine = fixtureCase.engine === "native_tantivy"
    ? "Native Tantivy"
    : fixtureCase.engine === "portable_fts5"
      ? "Portable FTS5"
      : "Shared contract";
  const expected = fixtureCase.expectedDisposition === "strict_balanced"
    ? "strict"
    : fixtureCase.expectedDisposition === "neutralized_counterfactual"
      ? "counterfactual"
      : "fatal";
  return `Case ${fixtureCase.ordinal + 1} · ${engine} · ${expected}`;
}

function movementLabel(movement: number): string {
  if (movement === 0) return "No rank movement";
  return movement > 0 ? `Moved up ${movement}` : `Moved down ${Math.abs(movement)}`;
}

function signed(value: number): string {
  return value > 0 ? `+${value}` : String(value);
}

function humanize(value: string): string {
  return value.replaceAll("_", " ");
}
