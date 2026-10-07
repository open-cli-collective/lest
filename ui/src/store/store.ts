// The app's run store: one SSE connection, runs keyed by id, read through
// useSyncExternalStore.

import { useEffect, useSyncExternalStore } from "react";
import { api } from "../api";
import type { RunEvent, RunReport } from "../types";
import { isLive, reduceRuns, type RunsAction, type RunsState, type RunState } from "./runState";

type Listener = () => void;

let state: RunsState = { runs: {} };
const listeners = new Set<Listener>();
const fetching = new Set<string>();
const failed = new Set<string>();
/** The newest run seen starting per flow since the page loaded. */
const latestByFlow: Record<string, string> = {};
/** Bumped when any run finishes, so lists that show last runs refetch. */
let finishedCount = 0;
let connection: "connecting" | "open" | "closed" = "connecting";

function emit() {
  for (const l of listeners) l();
}

function dispatch(action: RunsAction) {
  const next = reduceRuns(state, action);
  if (next === state) return;
  state = next;
  for (const run of Object.values(state.runs)) {
    if (run.gap) void hydrate(run.runId);
  }
  emit();
}

/** Loads a run from the server: events so far for a live run, the report
 * for a finished one. */
export async function hydrate(runId: string): Promise<void> {
  if (fetching.has(runId)) return;
  fetching.add(runId);
  try {
    const r = await api.run(runId);
    if (r.events.length) dispatch({ type: "events", runId, events: r.events });
    if (r.report) dispatch({ type: "report", report: r.report });
  } catch {
    // The run view shows that the run could not be loaded.
    failed.add(runId);
    emit();
  } finally {
    fetching.delete(runId);
  }
}

async function loadReport(runId: string): Promise<void> {
  // The report is written right before runFinished, so it is there now.
  try {
    const r = await api.run(runId);
    if (r.report) dispatch({ type: "report", report: r.report as RunReport });
  } catch {
    /* the events already carry the final step reports */
  }
}

let source: EventSource | null = null;

export function connect(): void {
  if (source || typeof EventSource === "undefined") return;
  source = new EventSource("/api/events");
  let dropped = false;
  source.onopen = () => {
    connection = "open";
    emit();
    // Events sent while the stream was down are gone: refetch every run
    // still shown as live.
    if (dropped) {
      dropped = false;
      for (const run of Object.values(state.runs)) if (isLive(run)) void hydrate(run.runId);
    }
  };
  source.onerror = () => {
    dropped = true;
    connection = source?.readyState === EventSource.CLOSED ? "closed" : "connecting";
    emit();
  };
  source.addEventListener("run", (m) => {
    let ev: RunEvent;
    try {
      ev = JSON.parse((m as MessageEvent<string>).data) as RunEvent;
    } catch {
      return;
    }
    if (ev.kind === "runStarted") latestByFlow[ev.flowId] = ev.runId;
    dispatch({ type: "event", event: ev });
    if (ev.kind === "runFinished") {
      finishedCount++;
      void loadReport(ev.runId);
      emit();
    }
  });
  source.addEventListener("resync", () => {
    for (const run of Object.values(state.runs)) {
      if (!run.finished) void hydrate(run.runId);
    }
    finishedCount++;
    emit();
  });
}

function subscribe(l: Listener) {
  listeners.add(l);
  return () => {
    listeners.delete(l);
  };
}

/** Marks a run as started from this page, so its flow view follows it before
 * the first event arrives. */
export function noteStarted(flowId: string, runId: string) {
  latestByFlow[flowId] = runId;
  emit();
}

export function useRun(runId: string | null | undefined): { run: RunState | null; failed: boolean } {
  const run = useSyncExternalStore(subscribe, () => (runId ? state.runs[runId] ?? null : null));
  const isFailed = useSyncExternalStore(subscribe, () => (runId ? failed.has(runId) : false));
  useEffect(() => {
    if (!runId) return;
    const cur = state.runs[runId];
    if (!cur || (cur.lastSeq < 0 && !cur.report)) void hydrate(runId);
  }, [runId]);
  return { run, failed: isFailed };
}

export function useLatestRunOf(flowId: string): string | null {
  return useSyncExternalStore(subscribe, () => latestByFlow[flowId] ?? null);
}

export function useFinishedCount(): number {
  return useSyncExternalStore(subscribe, () => finishedCount);
}

export function useConnection(): typeof connection {
  return useSyncExternalStore(subscribe, () => connection);
}

/** Live runs currently in the store (started or seen since page load). */
export function useLiveRunIds(): string {
  return useSyncExternalStore(subscribe, () =>
    Object.values(state.runs)
      .filter((r) => r.lastSeq >= 0 && !r.finished && r.lastSeq !== Number.MAX_SAFE_INTEGER)
      .map((r) => r.runId)
      .join(","),
  );
}

export function getRun(runId: string): RunState | null {
  return state.runs[runId] ?? null;
}
