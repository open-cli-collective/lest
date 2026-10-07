// JSON shapes of the Lest server API (crates/lest-server/src/api.rs) and the
// run model (crates/lest-core: event.rs, report.rs, store.rs, spec.rs).

export type StepKind = "run" | "http" | "assert" | "browser" | "flow" | "group";
export type StepStatus = "pending" | "running" | "passed" | "failed" | "errored" | "skipped";
export type RunResult = "passed" | "failed" | "errored" | "cancelled";
export type ToolStatus = "ready" | "missing" | "too-old" | "signed-out" | "unknown";
export type CleanupStatus = "pending" | "passed" | "failed" | "not-run";
export type OutputStream = "stdout" | "stderr" | "browser";
export type Scalar = string | number | boolean;

export interface PlanNode {
  id: string;
  name: string;
  kind: StepKind;
  notes?: string;
  children?: PlanNode[];
}

export interface Artifact {
  label: string;
  /** Relative to the run directory. */
  path: string;
  mime: string;
  bytes: number;
}

export interface Beat {
  marker: string;
  atMs: number;
}

export interface ToolReport {
  name: string;
  path?: string;
  version?: string;
  minVersion?: string;
  status: ToolStatus;
  message?: string;
}

export interface StepReport {
  id: string;
  name: string;
  kind: StepKind | null;
  status: StepStatus | null;
  attempts: number;
  startedAt?: string;
  finishedAt?: string;
  durationMs: number;
  resolved?: string;
  exitCode?: number;
  stdout?: string;
  stderr?: string;
  httpStatus?: number;
  outputs?: Record<string, unknown>;
  error?: string;
  headline?: string;
  notes?: string;
  tolerated?: boolean;
  artifacts?: Artifact[];
  beats?: Beat[];
  children?: StepReport[];
}

export interface CleanupReport {
  stepId: string;
  command: string;
  env: Record<string, string>;
  context?: Record<string, string>;
  cwd?: string;
  policy: string;
  status: CleanupStatus;
  exitCode?: number;
  output?: string;
  error?: string;
}

export interface DemoReport {
  video?: string;
  rawVideo?: string;
  chapters?: string;
  beatSheet?: string;
  durationMs?: number;
  rawDurationMs?: number;
  error?: string;
}

export interface ServiceReport {
  id: string;
  reused: boolean;
  log?: string;
}

export interface RunReport {
  schemaVersion: number;
  runId: string;
  flowId: string;
  flowName: string;
  flowPath: string;
  projectDir: string;
  environment?: string;
  startedAt: string;
  finishedAt?: string;
  durationMs: number;
  result: RunResult;
  error?: string;
  warnings?: string[];
  inputs: Record<string, string>;
  tools?: ToolReport[];
  steps: StepReport[];
  finally?: StepReport[];
  cleanups?: CleanupReport[];
  services?: ServiceReport[];
  resumedFrom?: { runId: string; step: string };
  demo?: DemoReport;
}

export interface RunSummary {
  runId: string;
  flowId: string;
  flowName: string;
  environment: string | null;
  startedAt: string;
  durationMs: number;
  result: RunResult;
  warnings: number;
  projectDir: string;
  hasVideo: boolean;
}

export interface Choice {
  value: string;
  label?: string;
  description?: string;
}

export interface Input {
  name: string;
  label?: string;
  description?: string;
  default?: Scalar;
  required: boolean;
  choices?: Choice[];
}

export interface Demo {
  title?: string;
  summary?: string;
  beats?: { marker: string; label: string }[];
}

export interface Diagnostic {
  file: string;
  flow?: string;
  at?: string;
  message: string;
  severity: "error" | "warning";
}

export interface FlowSummary {
  id: string;
  name: string;
  description: string | null;
  tags: string[];
  path: string;
  folder: string;
  steps: PlanNode[];
  finally: PlanNode[];
  inputs: Input[];
  environments: string[];
  defaultEnvironment: string | null;
  demo: Demo | null;
  tools: string[];
  secrets: string[];
  services: string[];
  usesBrowser: boolean;
  records: boolean;
  lastRun: RunSummary | null;
}

export interface FlowDetail extends FlowSummary {
  source: string;
  diagnostics: Diagnostic[];
}

export interface FlowsResponse {
  flows: FlowSummary[];
  diagnostics: Diagnostic[];
}

export interface StateResponse {
  project: { name: string; root: string; hasConfig: boolean };
  version: string;
  liveRuns: string[];
}

export interface RunsResponse {
  running: { runId: string; flowId: string; startedAt: string; environment: string | null }[];
  runs: RunSummary[];
}

export interface RunResponse {
  runId: string;
  flowId: string;
  done: boolean;
  events: RunEvent[];
  report: RunReport | null;
}

interface EventBase {
  runId: string;
  seq: number;
  atMs: number;
}

export type EventBody =
  | {
      kind: "runStarted";
      flowId: string;
      flowName: string;
      environment: string | null;
      runDir: string;
      steps: PlanNode[];
      finally: PlanNode[];
      tools: string[];
    }
  | { kind: "tool"; tool: ToolReport }
  | { kind: "signInNeeded"; tool: string; command: string }
  | { kind: "stepStarted"; stepId: string; attempt: number }
  | { kind: "stepOutput"; stepId: string; stream: OutputStream; line: string }
  | {
      kind: "stepRetrying";
      stepId: string;
      attempt: number;
      maxAttempts: number;
      reason: string;
      nextAttemptInMs: number;
    }
  | { kind: "stepFinished"; step: StepReport }
  | { kind: "stepArtifact"; stepId: string; artifact: Artifact }
  | { kind: "stepBeat"; stepId: string; beat: Beat }
  | { kind: "browserPage"; stepId: string; cdpPort: number; targetId: string; url: string }
  | { kind: "browserClosed"; stepId: string }
  | { kind: "cleanupStarted"; stepId: string }
  | { kind: "cleanupFinished"; stepId: string; status: StepStatus }
  | { kind: "demoProgress"; message: string }
  | {
      kind: "runFinished";
      result: RunResult;
      durationMs: number;
      error: string | null;
      reportPath: string;
    };

export type RunEvent = EventBase & EventBody;
