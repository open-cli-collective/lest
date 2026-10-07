// Pure run state: folds a run's events (and, once it exists, its report) into
// a step tree the UI renders. No I/O here; store.ts owns fetching and SSE.

import type {
  Artifact,
  Beat,
  OutputStream,
  PlanNode,
  RunEvent,
  RunReport,
  RunResult,
  StepKind,
  StepReport,
  StepStatus,
  ToolReport,
} from "../types";

/** Per step, only the newest lines are kept in memory. */
export const MAX_LINES_PER_STEP = 5000;

export interface OutputLine {
  seq: number;
  stream: OutputStream;
  line: string;
}

export interface RetryState {
  attempt: number;
  maxAttempts: number;
  reason: string;
  /** Server clock (ms since epoch) when the next attempt starts. */
  nextAtMs: number;
}

export interface StepState {
  id: string;
  name: string;
  kind: StepKind;
  notes?: string;
  parent: string | null;
  children: string[];
  status: StepStatus;
  /** The attempt in progress or the last one made; 0 before the first. */
  attempt: number;
  maxAttempts: number | null;
  retry: RetryState | null;
  startedAtMs: number | null;
  finishedAtMs: number | null;
  durationMs: number | null;
  output: OutputLine[];
  droppedLines: number;
  artifacts: Artifact[];
  beats: Beat[];
  report: StepReport | null;
}

export interface BrowserState {
  stepId: string;
  cdpPort: number;
  targetId: string;
  url: string;
  active: boolean;
}

export interface FinishedState {
  result: RunResult;
  durationMs: number;
  error: string | null;
  atMs: number;
}

export interface RunState {
  runId: string;
  flowId: string | null;
  flowName: string | null;
  environment: string | null;
  /** The highest event seq applied; -1 before any. */
  lastSeq: number;
  /** An event arrived after a missing one; the store refetches the run. */
  gap: boolean;
  startedAtMs: number | null;
  steps: Record<string, StepState>;
  roots: string[];
  finallyRoots: string[];
  tools: ToolReport[];
  signIn: Record<string, string>;
  progress: { atMs: number; message: string }[];
  cleanups: Record<string, StepStatus | "running">;
  browser: BrowserState | null;
  finished: FinishedState | null;
  report: RunReport | null;
}

export function emptyRun(runId: string): RunState {
  return {
    runId,
    flowId: null,
    flowName: null,
    environment: null,
    lastSeq: -1,
    gap: false,
    startedAtMs: null,
    steps: {},
    roots: [],
    finallyRoots: [],
    tools: [],
    signIn: {},
    progress: [],
    cleanups: {},
    browser: null,
    finished: null,
    report: null,
  };
}

function newStep(id: string, name: string, kind: StepKind, parent: string | null, notes?: string): StepState {
  return {
    id,
    name,
    kind,
    notes,
    parent,
    children: [],
    status: "pending",
    attempt: 0,
    maxAttempts: null,
    retry: null,
    startedAtMs: null,
    finishedAtMs: null,
    durationMs: null,
    output: [],
    droppedLines: 0,
    artifacts: [],
    beats: [],
    report: null,
  };
}

function addPlan(steps: Record<string, StepState>, nodes: PlanNode[], parent: string | null): string[] {
  return nodes.map((n) => {
    const s = newStep(n.id, n.name, n.kind, parent, n.notes);
    steps[n.id] = s;
    s.children = addPlan(steps, n.children ?? [], n.id);
    return n.id;
  });
}

/** Copies the step (and its parent chain is untouched); returns the copy. */
function edit(run: RunState, id: string): StepState {
  const cur = run.steps[id];
  const s: StepState = cur
    ? { ...cur }
    : newStep(id, id.split("/").pop() ?? id, "run", null);
  if (!cur) run.roots = [...run.roots, id];
  run.steps = { ...run.steps, [id]: s };
  return s;
}

function applyStepReport(run: RunState, r: StepReport, atMs: number | null, overwrite: boolean): void {
  const existing = run.steps[r.id];
  if (existing && existing.report && !overwrite) return;
  const s = edit(run, r.id);
  if (r.kind) s.kind = r.kind;
  s.name = r.name || s.name;
  if (r.notes) s.notes = r.notes;
  s.status = r.status ?? s.status;
  s.attempt = Math.max(s.attempt, r.attempts);
  s.retry = null;
  s.durationMs = r.durationMs;
  if (r.startedAt && s.startedAtMs === null) s.startedAtMs = Date.parse(r.startedAt);
  s.finishedAtMs = r.finishedAt ? Date.parse(r.finishedAt) : atMs;
  if (r.artifacts && r.artifacts.length) s.artifacts = r.artifacts;
  if (r.beats && r.beats.length) s.beats = r.beats;
  s.report = r;
  for (const c of r.children ?? []) {
    if (!run.steps[c.id]) {
      const child = edit(run, c.id);
      child.parent = r.id;
      run.roots = run.roots.filter((x) => x !== c.id);
      if (!s.children.includes(c.id)) s.children = [...s.children, c.id];
    }
    applyStepReport(run, c, atMs, overwrite);
  }
}

/** Applies one event. Duplicates and stale events are ignored; an event
 * after a missing one marks a gap and is ignored. */
export function applyEvent(prev: RunState, ev: RunEvent): RunState {
  if (ev.runId !== prev.runId) return prev;
  if (ev.seq <= prev.lastSeq) return prev;
  if (ev.seq !== prev.lastSeq + 1) return prev.gap ? prev : { ...prev, gap: true };
  const run: RunState = { ...prev, lastSeq: ev.seq, gap: false };
  switch (ev.kind) {
    case "runStarted": {
      const steps: Record<string, StepState> = {};
      run.flowId = ev.flowId;
      run.flowName = ev.flowName;
      run.environment = ev.environment;
      run.startedAtMs = ev.atMs;
      run.roots = addPlan(steps, ev.steps, null);
      run.finallyRoots = addPlan(steps, ev.finally, null);
      run.steps = steps;
      break;
    }
    case "tool": {
      const i = run.tools.findIndex((t) => t.name === ev.tool.name);
      run.tools = i < 0 ? [...run.tools, ev.tool] : run.tools.map((t, j) => (j === i ? ev.tool : t));
      break;
    }
    case "signInNeeded":
      run.signIn = { ...run.signIn, [ev.tool]: ev.command };
      break;
    case "stepStarted": {
      const s = edit(run, ev.stepId);
      s.status = "running";
      s.attempt = ev.attempt;
      s.retry = null;
      if (s.startedAtMs === null) s.startedAtMs = ev.atMs;
      break;
    }
    case "stepOutput": {
      const s = edit(run, ev.stepId);
      const out = [...s.output, { seq: ev.seq, stream: ev.stream, line: ev.line }];
      const over = out.length - MAX_LINES_PER_STEP;
      if (over > 0) {
        out.splice(0, over);
        s.droppedLines += over;
      }
      s.output = out;
      break;
    }
    case "stepRetrying": {
      const s = edit(run, ev.stepId);
      s.maxAttempts = ev.maxAttempts;
      s.retry = {
        attempt: ev.attempt,
        maxAttempts: ev.maxAttempts,
        reason: ev.reason,
        nextAtMs: ev.atMs + ev.nextAttemptInMs,
      };
      break;
    }
    case "stepFinished":
      applyStepReport(run, ev.step, ev.atMs, true);
      break;
    case "stepArtifact": {
      const s = edit(run, ev.stepId);
      if (!s.artifacts.some((a) => a.path === ev.artifact.path)) s.artifacts = [...s.artifacts, ev.artifact];
      break;
    }
    case "stepBeat": {
      const s = edit(run, ev.stepId);
      s.beats = [...s.beats, ev.beat];
      break;
    }
    case "browserPage":
      run.browser = {
        stepId: ev.stepId,
        cdpPort: ev.cdpPort,
        targetId: ev.targetId,
        url: ev.url,
        active: true,
      };
      break;
    case "browserClosed":
      if (run.browser && run.browser.stepId === ev.stepId) run.browser = { ...run.browser, active: false };
      break;
    case "cleanupStarted":
      run.cleanups = { ...run.cleanups, [ev.stepId]: "running" };
      break;
    case "cleanupFinished":
      run.cleanups = { ...run.cleanups, [ev.stepId]: ev.status };
      break;
    case "demoProgress":
      run.progress = [...run.progress, { atMs: ev.atMs, message: ev.message }];
      break;
    case "runFinished":
      run.finished = { result: ev.result, durationMs: ev.durationMs, error: ev.error, atMs: ev.atMs };
      if (run.browser) run.browser = { ...run.browser, active: false };
      break;
  }
  return run;
}

export function applyEvents(run: RunState, events: RunEvent[]): RunState {
  const sorted = [...events].sort((a, b) => a.seq - b.seq);
  return sorted.reduce(applyEvent, run);
}

function planFromReport(steps: StepReport[]): PlanNode[] {
  return steps.map((s) => ({
    id: s.id,
    name: s.name,
    kind: s.kind ?? "run",
    notes: s.notes,
    children: planFromReport(s.children ?? []),
  }));
}

/** Folds the canonical report into the state. Builds the tree from the
 * report when no events were seen (a stored run). */
export function applyReport(prev: RunState, report: RunReport): RunState {
  const run: RunState = { ...prev, report };
  if (prev.lastSeq < 0) {
    const steps: Record<string, StepState> = {};
    run.roots = addPlan(steps, planFromReport(report.steps), null);
    run.finallyRoots = addPlan(steps, planFromReport(report.finally ?? []), null);
    run.steps = steps;
    run.flowId = report.flowId;
    run.flowName = report.flowName;
    run.environment = report.environment ?? null;
    run.startedAtMs = Date.parse(report.startedAt);
    run.tools = report.tools ?? [];
    // Stored runs take no further events.
    run.lastSeq = Number.MAX_SAFE_INTEGER;
  }
  for (const s of [...report.steps, ...(report.finally ?? [])]) applyStepReport(run, s, null, true);
  if (!run.finished) {
    run.finished = {
      result: report.result,
      durationMs: report.durationMs,
      error: report.error ?? null,
      atMs: report.finishedAt ? Date.parse(report.finishedAt) : Date.parse(report.startedAt) + report.durationMs,
    };
  }
  const cleanups: Record<string, StepStatus | "running"> = { ...run.cleanups };
  for (const c of report.cleanups ?? []) {
    cleanups[c.stepId] = c.status === "not-run" ? "skipped" : c.status;
  }
  run.cleanups = cleanups;
  if (run.browser) run.browser = { ...run.browser, active: false };
  return run;
}

export function runFromReport(report: RunReport): RunState {
  return applyReport(emptyRun(report.runId), report);
}

/** The top-level ancestor of a step (what `lest run --from` takes). */
export function topLevelOf(run: RunState, id: string): string {
  let cur = run.steps[id];
  let top = id;
  while (cur && cur.parent) {
    top = cur.parent;
    cur = run.steps[cur.parent];
  }
  return top;
}

/** Every step id in display order, depth first. */
export function allStepIds(run: RunState): string[] {
  const out: string[] = [];
  const walk = (id: string) => {
    out.push(id);
    for (const c of run.steps[id]?.children ?? []) walk(c);
  };
  for (const r of [...run.roots, ...run.finallyRoots]) walk(r);
  return out;
}

/** The leaf step that failed the run, if any (not a tolerated failure). */
export function firstFailure(run: RunState): StepState | null {
  for (const id of allStepIds(run)) {
    const s = run.steps[id];
    if (!s) continue;
    if ((s.status === "failed" || s.status === "errored") && s.children.length === 0 && !s.report?.tolerated) {
      return s;
    }
  }
  return null;
}

export function isLive(run: RunState): boolean {
  return run.lastSeq >= 0 && run.finished === null;
}

/** The labeled beats seen so far across the run, in order. */
export function runBeats(run: RunState): Beat[] {
  return allStepIds(run).flatMap((id) => run.steps[id]?.beats ?? []);
}

export function runArtifacts(run: RunState): { step: StepState; artifacts: Artifact[] }[] {
  return allStepIds(run)
    .map((id) => run.steps[id])
    .filter((s): s is StepState => !!s && s.artifacts.length > 0)
    .map((step) => ({ step, artifacts: step.artifacts }));
}

// The app-wide state: runs keyed by id. An event only ever touches its own
// run's entry.

export interface RunsState {
  runs: Record<string, RunState>;
}

export type RunsAction =
  | { type: "event"; event: RunEvent }
  | { type: "events"; runId: string; events: RunEvent[] }
  | { type: "report"; report: RunReport };

export function reduceRuns(state: RunsState, action: RunsAction): RunsState {
  switch (action.type) {
    case "event": {
      const id = action.event.runId;
      const prev = state.runs[id] ?? emptyRun(id);
      const next = applyEvent(prev, action.event);
      if (next === prev && state.runs[id]) return state;
      return { runs: { ...state.runs, [id]: next } };
    }
    case "events": {
      const prev = state.runs[action.runId] ?? emptyRun(action.runId);
      const mine = action.events.filter((e) => e.runId === action.runId);
      const next = applyEvents({ ...prev, gap: false }, mine);
      return { runs: { ...state.runs, [action.runId]: next } };
    }
    case "report": {
      const id = action.report.runId;
      const prev = state.runs[id] ?? emptyRun(id);
      return { runs: { ...state.runs, [id]: applyReport(prev, action.report) } };
    }
  }
}
