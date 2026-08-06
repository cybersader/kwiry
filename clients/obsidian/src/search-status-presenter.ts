// SPDX-FileCopyrightText: 2026 cybersader
// SPDX-License-Identifier: GPL-3.0-only

import type { BackendStatus, CandidateWindowFacts } from "./backend";
import {
  emptyStateMessage,
  searchErrorEmptyState,
  shouldNoticeSearchError,
} from "./empty-state";
import { progressLine } from "./progress-line";

export type QueryStatusFacts =
  | { phase: "prompt" }
  | { phase: "searching" }
  | {
    phase: "settled";
    resultCount: number;
    displayedSourceCount: number;
    omittedObservedSourceCount: number;
    candidateWindow: CandidateWindowFacts;
  }
  | { phase: "error"; code: string; safeMessage: string };

export type QueryStatusState =
  | "prompt"
  | "searching"
  | "results"
  | "no-match"
  | "error";

export interface QueryStatusPresentation {
  state: QueryStatusState;
  text: string;
  busy: boolean;
}

export type BackgroundIndexState = "quiet" | "indexing" | "attention";

export interface BackgroundIndexPresentation {
  state: BackgroundIndexState;
  text: string;
}

/**
 * Derives the complete query-status vocabulary from privacy-safe search facts.
 * Candidate-window wording deliberately describes candidates/completeness, not
 * a corpus total or an unproven count of matches.
 */
export function presentQueryStatus(facts: QueryStatusFacts): QueryStatusPresentation {
  switch (facts.phase) {
    case "prompt":
      return {
        state: "prompt",
        text: emptyStateMessage("prompt"),
        busy: false,
      };
    case "searching":
      return {
        state: "searching",
        text: "Searching…",
        busy: true,
      };
    case "error":
      return {
        state: "error",
        text: errorText(facts.code, facts.safeMessage),
        busy: false,
      };
    case "settled": {
      const windowText = candidateWindowText(facts.candidateWindow.state);
      if (facts.resultCount === 0) {
        return {
          state: "no-match",
          text: `No matches — ${windowText}`,
          busy: false,
        };
      }
      const noun = facts.resultCount === 1 ? "result" : "results";
      const sourceDisplayText = facts.omittedObservedSourceCount > 0
        ? `${countedNoun(facts.displayedSourceCount, "source")} shown; `
          + `${countedNoun(facts.omittedObservedSourceCount, "observed source")} `
          + "omitted by the source-row limit; "
        : "";
      return {
        state: "results",
        text: `${facts.resultCount} ${noun} returned — ${sourceDisplayText}${windowText}`,
        busy: false,
      };
    }
  }
}

/**
 * Background indexing is intentionally separate from the polite query live
 * region. Polling may update this aggregate text without repeatedly announcing
 * progress to assistive technology.
 */
export function presentBackgroundIndex(status: BackendStatus): BackgroundIndexPresentation {
  const text = progressLine(status);
  if (text === null) return { state: "quiet", text: "" };
  return {
    state: status.progress === undefined ? "attention" : "indexing",
    text: `Index · ${stabilizeInFlightWidth(text)}`,
  };
}

/**
 * A figure space occupies one tabular digit without displaying a glyph, so the
 * trailing "in flight" label does not jump when the count crosses 9 → 10.
 */
function stabilizeInFlightWidth(text: string): string {
  return text.replace(/ · ([0-9]) in flight/u, " ·  $1 in flight");
}

function countedNoun(count: number, noun: string): string {
  return `${count} ${noun}${count === 1 ? "" : "s"}`;
}

function candidateWindowText(state: CandidateWindowFacts["state"]): string {
  switch (state) {
    case "exhausted":
      return "search window complete.";
    case "more_available":
      return "more candidates are available.";
    case "candidate_limit_reached":
      return "candidate window limit reached.";
    case "unknown":
      return "window completeness is unknown.";
  }
}

function errorText(code: string, safeMessage: string): string {
  if (!shouldNoticeSearchError(code)) return searchErrorEmptyState(code);
  const trimmed = safeMessage.trim();
  return trimmed.length > 0 ? trimmed : "Search is unavailable.";
}
