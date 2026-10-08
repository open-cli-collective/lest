// Optional AI: the settings store, the client-side explanation cache, and
// the pure decisions about what the failure card and Settings show.

import { useEffect, useSyncExternalStore } from "react";
import { api } from "../api";
import type { AiConfig, AiProvider, AiStatus, Explanation, SettingsResponse } from "../types";

// Settings, fetched once per page load and replaced on every save.

let settings: SettingsResponse | null = null;
let loading: Promise<void> | null = null;
const listeners = new Set<() => void>();

function emit() {
  listeners.forEach((l) => l());
}

function load(): Promise<void> {
  loading ??= api.settings().then(
    (s) => {
      settings = s;
      emit();
    },
    () => {
      // AI stays off for this page when settings cannot be read.
      loading = null;
    },
  );
  return loading;
}

export function useAiSettings(): SettingsResponse | null {
  const s = useSyncExternalStore(
    (l) => {
      listeners.add(l);
      return () => listeners.delete(l);
    },
    () => settings,
  );
  useEffect(() => {
    if (!settings) void load();
  }, []);
  return s;
}

/** Saves the AI settings and shares the result with every view. */
export async function saveAi(ai: AiConfig): Promise<SettingsResponse> {
  const s = await api.saveAi(ai);
  settings = s;
  emit();
  return s;
}

// Explanations, requested once per provider, run and step.

const explanations = new Map<string, Promise<Explanation>>();

export function explanationKey(status: AiStatus, runId: string, stepId: string): string {
  return [status.resolved?.label ?? "", status.resolved?.model ?? "", runId, stepId].join("\n");
}

export function requestExplanation(key: string, runId: string, stepId: string): Promise<Explanation> {
  let p = explanations.get(key);
  if (!p) {
    p = api.explain(runId, stepId);
    // A failed request is not cached, so a later view can try again.
    p.catch(() => explanations.delete(key));
    explanations.set(key, p);
  }
  return p;
}

// What the failure card's explanation slot shows.

export type ExplainState =
  | { kind: "off" }
  | { kind: "pending" }
  | { kind: "done"; explanation: Explanation }
  | { kind: "error"; message: string };

export interface Slot {
  /** The main line. */
  primary: string;
  /** The small text in the marker position, empty when AI is off. */
  marker: string;
  /** The deterministic headline when a model wrote the main line. */
  headline: string | null;
  /** Why the model's text is not shown, in small muted text. */
  note: string | null;
}

export function explainSlot(headline: string, state: ExplainState): Slot {
  const base: Slot = { primary: headline, marker: "", headline: null, note: null };
  switch (state.kind) {
    case "off":
      return base;
    case "pending":
      return { ...base, marker: "Explaining…" };
    case "error":
      return { ...base, note: `AI unavailable: ${state.message}` };
    case "done": {
      const e = state.explanation;
      if (e.source === "model" && e.text.trim()) {
        return { primary: e.text.trim(), marker: `Written by ${e.provider ?? "the model"}`, headline, note: null };
      }
      return { ...base, note: e.note ? `AI unavailable: ${e.note}` : null };
    }
  }
}

// Settings copy.

export function aiStatusLine(ai: AiConfig, status: AiStatus): string {
  const r = status.resolved;
  if (r) {
    const model = r.model ? `, model ${r.model}` : "";
    return `Using ${r.label}${model}. ${status.callsToday} of ${status.dailyLimit} model calls today.`;
  }
  switch (ai.provider) {
    case "none":
      return "AI is off. Failure explanations come from the step's own output.";
    case "auto":
      return "No agent CLI found on PATH; AI stays off.";
    case "command":
      return "Enter a command; AI stays off until then.";
    default:
      return `${ai.provider} is not on PATH; AI stays off.`;
  }
}

export interface ProviderOption {
  id: AiConfig["provider"];
  label: string;
}

/** Off, Automatic, each agent CLI found on PATH, and Custom command. A
 * configured CLI that is not on PATH stays listed so the choice is visible. */
export function providerOptions(ai: AiConfig, status: AiStatus): ProviderOption[] {
  const clis = [...status.available];
  if (!["none", "auto", "command"].includes(ai.provider) && !clis.includes(ai.provider)) clis.push(ai.provider);
  return [
    { id: "none", label: "Off" },
    { id: "auto", label: "Automatic" },
    ...clis.map((c) => ({ id: c, label: c })),
    { id: "command", label: "Custom command" },
  ];
}

export interface ModelChoice {
  /** Passed as the model; empty means the CLI's default. */
  value: string;
  label: string;
}

/** The models offered for an agent CLI, its default first, or null when
 * the provider takes no model (off, or a custom command). Names are the
 * aliases each CLI accepts; any other name can still be typed. */
export function modelChoices(kind: AiProvider | null | undefined): ModelChoice[] | null {
  switch (kind) {
    case "claude":
      return [
        { value: "", label: "Haiku (default)" },
        { value: "sonnet", label: "Sonnet" },
        { value: "opus", label: "Opus" },
        { value: "fable", label: "Fable" },
      ];
    case "codex":
      return [{ value: "", label: "Codex default" }];
    default:
      return null;
  }
}

/** Whether the step menu offers the agent items. */
export function agentConfigured(status: AiStatus | null | undefined, handoffCommand: string | null | undefined): boolean {
  // Only a command Lest can run counts: a provider alone (a custom
  // command) has no interactive agent to open.
  void status;
  return !!handoffCommand;
}
