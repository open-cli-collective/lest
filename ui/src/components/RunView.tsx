import { useEffect, useRef, useState, type ReactNode } from "react";
import { api, fileUrl } from "../api";
import { buildContext, rerunCommand } from "../lib/context";
import { formatBytes, formatDuration, formatTime, plural, timeAgo } from "../lib/format";
import { navigate } from "../lib/router";
import {
  allStepIds,
  isLive,
  runArtifacts,
  topLevelOf,
  type RunState,
  type StepState,
} from "../store/runState";
import { useRun } from "../store/store";
import type { AiStatus, Artifact, Handoff, RunReport, StepReport, StepStatus, ToolReport } from "../types";
import {
  agentConfigured,
  explainSlot,
  explanationKey,
  requestExplanation,
  useAiSettings,
  type ExplainState,
} from "../lib/ai";
import {
  IconCopy,
  IconExternal,
  IconFile,
  IconFolder,
  IconTerminal,
  IconWarning,
} from "./icons";
import { LiveView } from "./LiveView";
import { ResultBadge, StatusDot, StatusGlyph } from "./status";
import { CopyButton, copyWithToast, Dialog, Lightbox, OverflowMenu, toast, useNow } from "./ui";

export interface RunViewProps {
  runId: string;
  projectRoot: string | null;
  /** Flow file path relative to the project, for Copy context before the
   * report is loaded. */
  flowPath?: string;
  showSide?: boolean;
}

type OpenImage = { src: string; caption: string } | null;

export function RunView({ runId, projectRoot, flowPath, showSide = true }: RunViewProps) {
  const { run, failed } = useRun(runId);
  const [image, setImage] = useState<OpenImage>(null);
  const live = run ? isLive(run) : false;
  const now = useNow(200, live);

  if (!run || (run.lastSeq < 0 && !run.report)) {
    return <div className="loading">{failed ? `Run ${runId} could not be loaded.` : "Loading run"}</div>;
  }
  const ctx: Ctx = { run, now, live, projectRoot, flowPath, openImage: setImage };
  return (
    <div className={`run-layout${showSide ? "" : " no-side"}`}>
      <div className="run-main">
        <SummaryStrip ctx={ctx} />
        {run.finished?.error && (
          <div className="banner banner-error" role="alert">
            <IconWarning />
            <div className="banner-body">
              <div className="banner-title">The run could not complete</div>
              <div>{run.finished.error}</div>
            </div>
          </div>
        )}
        <Setup ctx={ctx} />
        <StepTree ctx={ctx} />
      </div>
      {showSide && <SidePanel ctx={ctx} />}
      {image && <Lightbox src={image.src} caption={image.caption} onClose={() => setImage(null)} />}
    </div>
  );
}

interface Ctx {
  run: RunState;
  now: number;
  live: boolean;
  projectRoot: string | null;
  flowPath?: string;
  openImage: (img: OpenImage) => void;
}

function runStatus(run: RunState): StepStatus | RunReport["result"] {
  return run.finished ? run.finished.result : "running";
}

function SummaryStrip({ ctx }: { ctx: Ctx }) {
  const { run, now } = ctx;
  const status = runStatus(run);
  const duration = run.finished ? run.finished.durationMs : run.startedAtMs ? now - run.startedAtMs : null;
  const warnings = run.report?.warnings?.length ?? 0;
  return (
    <div className={`summary edge-${status}`}>
      <div className="item">
        <span className="label">Result</span>
        <span className="value">
          <ResultBadge status={status} />
        </span>
      </div>
      <div className="item">
        <span className="label">Duration</span>
        <span className="value big">{formatDuration(duration) || "0ms"}</span>
      </div>
      {run.startedAtMs && (
        <div className="item">
          <span className="label">Started</span>
          <span className="value" title={formatTime(run.startedAtMs)}>
            {timeAgo(run.startedAtMs, Math.max(now, Date.now()))}
          </span>
        </div>
      )}
      {run.environment && (
        <div className="item">
          <span className="label">Environment</span>
          <span className="value">{run.environment}</span>
        </div>
      )}
      <div className="item">
        <span className="label">Run</span>
        <span className="value mono">
          {run.runId}
          <CopyButton text={run.runId} label="run id" />
        </span>
      </div>
      {run.report?.resumedFrom && (
        <div className="item">
          <span className="label">Resumed from</span>
          <span className="value mono">
            {run.report.resumedFrom.step} of {run.report.resumedFrom.runId}
          </span>
        </div>
      )}
      <div className="grow" />
      {warnings > 0 && (
        <div className="item">
          <span className="label">Warnings</span>
          <span className="value warn">
            <IconWarning size={14} />
            {warnings}
          </span>
        </div>
      )}
    </div>
  );
}

const toolStatusText: Record<ToolReport["status"], string> = {
  ready: "ready",
  missing: "missing",
  "too-old": "too old",
  "signed-out": "signed out",
  unknown: "version unknown",
};

function toolDot(t: ToolReport) {
  return t.status === "ready" ? "passed" : t.status === "unknown" ? "skipped" : "failed";
}

function Setup({ ctx }: { ctx: Ctx }) {
  const { run } = ctx;
  const tools = run.tools;
  const services = run.lastSeq === Number.MAX_SAFE_INTEGER ? (run.report?.services ?? []) : [];
  if (!tools.length && !run.progress.length && !services.length) return null;
  return (
    <div className="panel">
      <div className="panel-head">{tools.length ? "Preflight" : "Setup"}</div>
      <div className="quiet-rows">
        {tools.map((t) => (
          <ToolRow key={t.name} tool={t} signIn={run.signIn[t.name]} />
        ))}
        {Object.entries(run.signIn)
          .filter(([name]) => !tools.some((t) => t.name === name))
          .map(([name, cmd]) => (
            <ToolRow key={name} tool={{ name, status: "signed-out" }} signIn={cmd} />
          ))}
        {run.progress.map((p, i) => (
          <div className="quiet-row" key={i}>
            <IconTerminal size={14} />
            <span className="msg">{p.message}</span>
            <span className="right" title={formatTime(p.atMs)}>
              {run.startedAtMs ? `+${formatDuration(p.atMs - run.startedAtMs)}` : ""}
            </span>
          </div>
        ))}
        {services.map((s) => (
          <div className="quiet-row" key={s.id}>
            <IconTerminal size={14} />
            <span className="msg">
              service <span className="mono">{s.id}</span> {s.reused ? "was already running and was reused" : "started"}
            </span>
            {s.log && (
              <span className="right">
                <a href={fileUrl(run.runId, s.log)} target="_blank" rel="noreferrer">
                  log
                </a>
              </span>
            )}
          </div>
        ))}
      </div>
    </div>
  );
}

function ToolRow({ tool, signIn }: { tool: ToolReport; signIn?: string }) {
  const [result, setResult] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const login = async () => {
    setBusy(true);
    try {
      const r = await api.toolLogin(tool.name);
      if (r.launched) {
        setResult(`Opened ${r.how ?? "a terminal"} running: ${r.command}`);
      } else {
        setResult(`No terminal could be opened. Run this yourself: ${r.command}`);
        void copyWithToast(r.command, "sign-in command");
      }
    } catch (e) {
      setResult(signIn ? `Could not start sign-in (${(e as Error).message}). Run: ${signIn}` : (e as Error).message);
    } finally {
      setBusy(false);
    }
  };
  return (
    <>
      <div className="quiet-row">
        <StatusDot status={toolDot(tool)} />
        <span className="mono">{tool.name}</span>
        {tool.version && <span className="muted mono">{tool.version}</span>}
        <span className="msg muted">{tool.message}</span>
        <span className="right">
          {toolStatusText[tool.status]}
          {signIn && (
            <button type="button" className="btn btn-sm" onClick={() => void login()} disabled={busy}>
              Sign in
            </button>
          )}
        </span>
      </div>
      {result && (
        <div className="quiet-row">
          <span className="msg mono" style={{ whiteSpace: "normal" }}>
            {result}
          </span>
        </div>
      )}
    </>
  );
}

// Step tree

function autoOpen(s: StepState, live: boolean): boolean {
  if (s.children.length) {
    if (s.status === "running" || s.status === "failed" || s.status === "errored") return true;
    return live && s.status === "pending";
  }
  return (s.status === "failed" || s.status === "errored") && !s.report?.tolerated;
}

function StepTree({ ctx }: { ctx: Ctx }) {
  const { run } = ctx;
  const [toggles, setToggles] = useState<Record<string, boolean>>({});
  const toggle = (id: string, open: boolean) => setToggles((t) => ({ ...t, [id]: !open }));
  if (!run.roots.length && !run.finallyRoots.length) {
    return (
      <div className="panel">
        <div className="panel-empty">{run.finished ? "This run executed no steps." : "Waiting for the first step"}</div>
      </div>
    );
  }
  const node = (id: string, depth: number) => (
    <StepNode key={id} id={id} depth={depth} ctx={ctx} toggles={toggles} onToggle={toggle} />
  );
  return (
    <div className="panel">
      <div className="panel-head">
        Steps
        <span className="right">{stepCounts(run)}</span>
      </div>
      <div className="tree" role="tree" aria-label="Steps">
        {run.roots.map((id) => node(id, 0))}
        {run.finallyRoots.length > 0 && (
          <>
            <div className="tree-label">Finally</div>
            {run.finallyRoots.map((id) => node(id, 0))}
          </>
        )}
      </div>
    </div>
  );
}

function stepCounts(run: RunState): string {
  const leaves = allStepIds(run)
    .map((id) => run.steps[id])
    .filter((s): s is StepState => !!s && s.children.length === 0);
  const done = leaves.filter((s) => s.status !== "pending" && s.status !== "running").length;
  return `${done} of ${leaves.length}`;
}

function StepNode({
  id,
  depth,
  ctx,
  toggles,
  onToggle,
}: {
  id: string;
  depth: number;
  ctx: Ctx;
  toggles: Record<string, boolean>;
  onToggle: (id: string, open: boolean) => void;
}) {
  const s = ctx.run.steps[id];
  const rowRef = useRef<HTMLButtonElement>(null);
  const isContainer = !!s && s.children.length > 0;
  const status = s?.status;
  useEffect(() => {
    if (ctx.live && status === "running" && !isContainer && rowRef.current) {
      rowRef.current.scrollIntoView({ block: "nearest", behavior: "smooth" });
    }
  }, [status, ctx.live, isContainer]);
  if (!s) return null;
  const open = toggles[id] ?? autoOpen(s, ctx.live);
  const dur =
    s.status === "running" && s.startedAtMs !== null ? ctx.now - s.startedAtMs : s.durationMs;
  return (
    <div className={`step step-status-${s.status}${open ? " open" : ""}`} role="treeitem" aria-expanded={open}>
      <button
        ref={rowRef}
        type="button"
        className={`step-row edge-${s.report?.tolerated ? "skipped" : s.status}`}
        style={{ ["--depth" as string]: depth }}
        onClick={() => onToggle(id, open)}
      >
        <span className="chev">
          <svg width="10" height="10" viewBox="0 0 10 10" aria-hidden="true">
            <path d="m3.5 2 3 3-3 3" fill="none" stroke="currentColor" strokeWidth="1.5" strokeLinecap="round" />
          </svg>
        </span>
        <StatusGlyph status={s.status} />
        <span className="step-name">{s.name}</span>
        {s.name !== id && <span className="step-id mono">{id}</span>}
        <span className="right">
          {s.report?.tolerated && <span className="tolerated">failure tolerated</span>}
          <Attempts s={s} now={ctx.now} />
          <span className="kind">{s.kind}</span>
          <span className="dur">{s.status === "pending" || s.status === "skipped" ? "" : formatDuration(dur)}</span>
        </span>
      </button>
      {open && isContainer && (
        <div className="step-children" role="group" style={{ ["--depth" as string]: depth }}>
          {s.children.map((c) => (
            <StepNode key={c} id={c} depth={depth + 1} ctx={ctx} toggles={toggles} onToggle={onToggle} />
          ))}
        </div>
      )}
      {open && !isContainer && <StepBody s={s} depth={depth} ctx={ctx} />}
    </div>
  );
}

function Attempts({ s, now }: { s: StepState; now: number }) {
  if (s.retry && s.status === "running") {
    const wait = Math.max(0, s.retry.nextAtMs - now);
    return (
      <span className="attempt retrying" title={s.retry.reason}>
        attempt {s.retry.attempt + 1} of {s.retry.maxAttempts}, next in {Math.ceil(wait / 1000)}s
      </span>
    );
  }
  const max = s.maxAttempts;
  if (s.status === "running" && max && s.attempt > 1) {
    return (
      <span className="attempt">
        attempt {s.attempt} of {max}
      </span>
    );
  }
  if (s.status !== "running" && s.attempt > 1) {
    return <span className="attempt">{plural(s.attempt, "attempt")}</span>;
  }
  return null;
}

type Tab = "output" | "details" | "artifacts";

function StepBody({ s, depth, ctx }: { s: StepState; depth: number; ctx: Ctx }) {
  const [tab, setTab] = useState<Tab>("output");
  const failedHere = (s.status === "failed" || s.status === "errored") && s.report;
  const tabs: { id: Tab; label: string; n?: number }[] = [
    { id: "output", label: "Output" },
    { id: "details", label: "Details" },
    { id: "artifacts", label: "Artifacts", n: s.artifacts.length },
  ];
  return (
    <div className="step-body" style={{ ["--depth" as string]: depth }}>
      {failedHere && <FailedCard s={s} ctx={ctx} />}
      <div className="tabs" role="tablist">
        {tabs.map((t) => (
          <button
            key={t.id}
            type="button"
            role="tab"
            className="tab"
            aria-selected={tab === t.id}
            id={`tab-${s.id}-${t.id}`}
            onClick={() => setTab(t.id)}
          >
            {t.label}
            {t.n ? <span className="n">{t.n}</span> : null}
          </button>
        ))}
      </div>
      <div className="tab-panel" role="tabpanel" aria-labelledby={`tab-${s.id}-${tab}`}>
        {tab === "output" && <OutputPane s={s} live={ctx.live} />}
        {tab === "details" && <DetailsPane s={s} />}
        {tab === "artifacts" &&
          (s.artifacts.length ? (
            <ArtifactGrid runId={ctx.run.runId} artifacts={s.artifacts} report={ctx.run.report} onOpen={ctx.openImage} />
          ) : (
            <div className="output-empty">This step saved no artifacts.</div>
          ))}
      </div>
    </div>
  );
}

const MAX_RENDERED_LINES = 2000;

function prettyIfJson(text: string): string {
  const t = text.trim();
  if (t.length > 200_000 || !(t.startsWith("{") || t.startsWith("["))) return text;
  try {
    return JSON.stringify(JSON.parse(t), null, 2);
  } catch {
    return text;
  }
}

function tail(text: string): { text: string; dropped: number } {
  const lines = text.replace(/\n+$/, "").split("\n");
  if (lines.length <= MAX_RENDERED_LINES) return { text: lines.join("\n"), dropped: 0 };
  return { text: lines.slice(-MAX_RENDERED_LINES).join("\n"), dropped: lines.length - MAX_RENDERED_LINES };
}

function OutputPane({ s, live }: { s: StepState; live: boolean }) {
  const pre = useRef<HTMLPreElement>(null);
  const finished = s.report && s.status !== "running";
  const lines = s.output;
  useEffect(() => {
    if (live && !finished && pre.current) pre.current.scrollTop = pre.current.scrollHeight;
  }, [lines.length, live, finished]);

  if (finished && (s.report?.stdout || s.report?.stderr)) {
    const parts: ReactNode[] = [];
    const both = !!s.report?.stdout && !!s.report?.stderr;
    for (const stream of ["stdout", "stderr"] as const) {
      const raw = s.report?.[stream];
      if (!raw) continue;
      const { text, dropped } = tail(stream === "stdout" ? prettyIfJson(raw) : raw);
      parts.push(
        <div key={stream}>
          {both && <span className="output-label">{stream}</span>}
          {dropped > 0 && <div className="output-note">Showing the last {MAX_RENDERED_LINES} lines; {dropped} earlier lines are in the report.</div>}
          <pre className="output">
            {text.split("\n").map((l, i) => (
              <span
                key={i}
                className={stream === "stderr" ? (l.startsWith("[browser]") ? "l-browser" : "l-stderr") : undefined}
              >
                {l}
                {"\n"}
              </span>
            ))}
          </pre>
        </div>,
      );
    }
    return <>{parts}</>;
  }
  if (!lines.length) {
    return (
      <div className="output-empty">
        {s.status === "running" ? "Waiting for output" : s.status === "pending" ? "Not run yet." : "No output."}
      </div>
    );
  }
  const shown = lines.slice(-MAX_RENDERED_LINES);
  const dropped = s.droppedLines + lines.length - shown.length;
  return (
    <>
      {dropped > 0 && <div className="output-note">{dropped} earlier lines are not shown.</div>}
      <pre className="output" ref={pre} aria-live={live ? "polite" : undefined}>
        {shown.map((l) => (
          <span key={l.seq} className={`l-${l.stream}`}>
            {l.line}
            {"\n"}
          </span>
        ))}
      </pre>
    </>
  );
}

function DetailsPane({ s }: { s: StepState }) {
  const r: StepReport | null = s.report;
  const rows: [string, ReactNode][] = [];
  if (r?.resolved) rows.push(["Resolved", <pre className="mono">{r.resolved.trimEnd()}</pre>]);
  rows.push(["Status", s.status + (r?.tolerated ? " (failure tolerated by continueOnError)" : "")]);
  if (r?.exitCode !== undefined && r?.exitCode !== null) rows.push(["Exit code", <span className="mono">{r.exitCode}</span>]);
  if (r?.httpStatus) rows.push(["HTTP status", <span className="mono">{r.httpStatus}</span>]);
  if (s.attempt) rows.push(["Attempts", s.maxAttempts ? `${s.attempt} of up to ${s.maxAttempts}` : String(s.attempt)]);
  if (r?.startedAt) rows.push(["Started", formatTime(r.startedAt)]);
  if (s.durationMs !== null) rows.push(["Duration", formatDuration(s.durationMs)]);
  if (r?.error && r.error !== r.headline) rows.push(["Error", <pre className="mono">{r.error}</pre>]);
  if (r?.error && r.error === r.headline && !(s.status === "failed" || s.status === "errored"))
    rows.push(["Error", <pre className="mono">{r.error}</pre>]);
  if (s.notes) rows.push(["Notes", s.notes]);
  const outputs = Object.entries(r?.outputs ?? {});
  if (outputs.length) {
    rows.push([
      "Outputs",
      <table className="outputs-table">
        <tbody>
          {outputs.map(([k, v]) => (
            <tr key={k}>
              <td className="mono">{k}</td>
              <td>
                <pre className="mono">{typeof v === "string" ? v : JSON.stringify(v, null, 2)}</pre>
              </td>
            </tr>
          ))}
        </tbody>
      </table>,
    ]);
  }
  if (s.beats.length) rows.push(["Beats", s.beats.map((b) => `${b.marker} at ${formatDuration(b.atMs)}`).join(", ")]);
  if (!r && s.status === "running") rows.push(["", <span className="muted">Details appear when the step finishes.</span>]);
  return (
    <div className="kv">
      {rows.map(([k, v], i) => (
        <KvRow key={i} k={k} v={v} />
      ))}
    </div>
  );
}

function KvRow({ k, v }: { k: string; v: ReactNode }) {
  return (
    <>
      <div className="k">{k}</div>
      <div className="v">{v}</div>
    </>
  );
}

/** The failure explanation slot. The primary line holds the deterministic
 * headline; with AI on, a model's explanation takes its place and the
 * headline moves to the line under it. The marker keeps its place either way. */
function FailedCard({ s, ctx }: { s: StepState; ctx: Ctx }) {
  const r = s.report as StepReport;
  const { run } = ctx;
  const top = topLevelOf(run, s.id);
  const flowId = run.flowId ?? run.report?.flowId ?? "";
  const report = run.report ?? syntheticReport(run, ctx);
  const rerun = rerunCommand({
    flowId,
    fromStep: top,
    runId: run.runId,
    environment: run.environment,
    inputs: report.inputs,
  });
  const headline = r.headline ?? r.error ?? `${s.name} ${s.status}`;
  const ai = useAiSettings();
  // Explanations and handoffs read the stored report, so they wait for the run to finish.
  const stored = !ctx.live && !!run.report;
  const explainState = useExplanation(ai?.status ?? null, stored ? run.runId : null, s.id);
  const slot = explainSlot(headline, explainState);
  const handoff = useHandoff(ai?.status ?? null, stored ? run.runId : null, s.id);
  const [launch, setLaunch] = useState<{ error: string; command: string | null } | null>(null);
  const openAgent = async () => {
    try {
      const res = await api.openAgent(run.runId, s.id);
      if (res.launched) toast(`Opened ${res.how ?? "a terminal"} with the agent`);
      else setLaunch({ error: res.error ?? "No terminal could be opened.", command: res.command });
    } catch (e) {
      setLaunch({ error: (e as Error).message, command: null });
    }
  };
  const agentItems =
    stored && agentConfigured(ai?.status, handoff?.command)
      ? [
          { label: "Open in agent", icon: <IconTerminal size={14} />, onSelect: () => void openAgent() },
          ...(handoff?.command
            ? [
                {
                  label: "Copy agent command",
                  icon: <IconCopy size={14} />,
                  onSelect: () => void copyWithToast(handoff.command as string, "agent command"),
                },
              ]
            : []),
        ]
      : [];
  return (
    <div className={`explain${s.status === "errored" ? " errored" : ""}${ai?.status.resolved ? " ai" : ""}`}>
      <div className="explain-top">
        <StatusGlyph status={s.status} />
        <div className="explain-primary">{slot.primary}</div>
        <span className="explain-marker">{slot.marker}</span>
      </div>
      {slot.headline && <div className="explain-secondary explain-headline">{slot.headline}</div>}
      {slot.note && <div className="explain-secondary explain-note">{slot.note}</div>}
      {s.notes && <div className="explain-secondary">{s.notes}</div>}
      <div className="explain-actions">
        <button type="button" className="btn btn-sm" onClick={() => void copyWithToast(rerun, "rerun command")}>
          <IconTerminal size={14} />
          Copy rerun command
        </button>
        <button
          type="button"
          className="btn btn-sm"
          onClick={() => void copyWithToast(buildContext({ report, step: r, topLevel: top }), "context")}
        >
          <IconCopy size={14} />
          Copy context
        </button>
        <OverflowMenu
          label="More actions for this step"
          items={[
            { label: "Copy headline", icon: <IconCopy size={14} />, onSelect: () => void copyWithToast(headline, "headline") },
            { label: "Copy step id", icon: <IconCopy size={14} />, onSelect: () => void copyWithToast(s.id, "step id") },
            ...agentItems,
            {
              label: "Show run folder",
              icon: <IconFolder size={14} />,
              onSelect: () =>
                void api.reveal(run.runId).then(
                  () => toast("Opened the run folder"),
                  (e: Error) => toast(e.message),
                ),
            },
            ...(location.pathname.startsWith("/runs/")
              ? []
              : [{ label: "Open this run", icon: <IconExternal size={14} />, onSelect: () => navigate(`/runs/${run.runId}`) }]),
          ]}
        />
      </div>
      {launch && (
        <Dialog title="Open in agent" onClose={() => setLaunch(null)}>
          <p>{launch.error}</p>
          {launch.command ? (
            <>
              <p className="muted">Run this in a terminal yourself:</p>
              <div className="dialog-command">
                <pre className="mono">{launch.command}</pre>
                <button
                  type="button"
                  className="btn btn-sm"
                  onClick={() => void copyWithToast(launch.command as string, "agent command")}
                >
                  <IconCopy size={14} />
                  Copy
                </button>
              </div>
            </>
          ) : (
            <p className="muted">
              Set the agent to start with <span className="mono">lest config set ai.agent '&lt;command&gt; {"{prompt}"}'</span>.
            </p>
          )}
        </Dialog>
      )}
    </div>
  );
}

/** The explanation for a failed step of a finished run, when AI is on. */
function useExplanation(status: AiStatus | null, runId: string | null, stepId: string): ExplainState {
  const key = status?.resolved && runId ? explanationKey(status, runId, stepId) : null;
  const [result, setResult] = useState<{ key: string; state: ExplainState } | null>(null);
  useEffect(() => {
    if (!key || !runId) return;
    let alive = true;
    requestExplanation(key, runId, stepId).then(
      (explanation) => alive && setResult({ key, state: { kind: "done", explanation } }),
      (e: Error) => alive && setResult({ key, state: { kind: "error", message: e.message } }),
    );
    return () => {
      alive = false;
    };
  }, [key, runId, stepId]);
  if (!key) return { kind: "off" };
  return result?.key === key ? result.state : { kind: "pending" };
}

/** The agent handoff for a finished run, fetched when AI may be on. */
function useHandoff(status: AiStatus | null, runId: string | null, stepId: string): Handoff | null {
  const [handoff, setHandoff] = useState<Handoff | null>(null);
  const resolved = status?.resolved?.label ?? "";
  useEffect(() => {
    if (!status || !runId || status.configured === "none") {
      setHandoff(null);
      return;
    }
    let alive = true;
    api.handoff(runId, stepId).then(
      (h) => alive && setHandoff(h),
      () => alive && setHandoff(null),
    );
    return () => {
      alive = false;
    };
  }, [status?.configured, resolved, runId, stepId]);
  return handoff;
}

/** A report-shaped view of a run whose report has not loaded yet. */
function syntheticReport(run: RunState, ctx: Ctx): RunReport {
  const toReport = (id: string): StepReport => {
    const s = run.steps[id]!;
    return (
      s.report ?? {
        id,
        name: s.name,
        kind: s.kind,
        status: s.status,
        attempts: s.attempt,
        durationMs: s.durationMs ?? 0,
        children: s.children.map(toReport),
      }
    );
  };
  return {
    schemaVersion: 1,
    runId: run.runId,
    flowId: run.flowId ?? "",
    flowName: run.flowName ?? "",
    flowPath: ctx.flowPath ?? "",
    projectDir: ctx.projectRoot ?? "",
    environment: run.environment ?? undefined,
    startedAt: new Date(run.startedAtMs ?? Date.now()).toISOString(),
    durationMs: run.finished?.durationMs ?? 0,
    result: run.finished?.result ?? "failed",
    inputs: {},
    steps: run.roots.map(toReport),
    finally: run.finallyRoots.map(toReport),
  };
}

// Artifacts

export function ArtifactGrid({
  runId,
  artifacts,
  report,
  onOpen,
  compact = false,
}: {
  runId: string;
  artifacts: Artifact[];
  report: RunReport | null;
  onOpen: (img: OpenImage) => void;
  compact?: boolean;
}) {
  return (
    <div className="artifacts">
      {artifacts.map((a) => {
        const url = fileUrl(runId, a.path);
        if (a.mime.startsWith("image/")) {
          return (
            <button key={a.path} type="button" className="thumb" onClick={() => onOpen({ src: url, caption: a.label })}>
              <img src={url} alt={a.label} loading="lazy" />
              <span className="thumb-label">
                <span>{a.label}</span>
                {!compact && <span className="muted">{formatBytes(a.bytes)}</span>}
              </span>
            </button>
          );
        }
        if (a.mime.startsWith("video/")) {
          const chapters =
            report?.demo?.chaptersVtt && report.demo.video === a.path
              ? fileUrl(runId, report.demo.chaptersVtt)
              : null;
          return (
            <div key={a.path} className="video-art">
              <video controls preload="metadata" src={url}>
                {chapters && <track kind="chapters" src={chapters} default />}
              </video>
              <span className="thumb-label">
                <span>{a.label}</span>
                <a className="muted" href={url} download>
                  {formatBytes(a.bytes)}
                </a>
              </span>
            </div>
          );
        }
        return (
          <a key={a.path} className="file-art" href={url} target="_blank" rel="noreferrer">
            <IconFile size={14} />
            <span>{a.label}</span>
            <span className="muted">{formatBytes(a.bytes)}</span>
          </a>
        );
      })}
    </div>
  );
}

// Side panel

function SidePanel({ ctx }: { ctx: Ctx }) {
  const { run, live } = ctx;
  const groups = runArtifacts(run);
  const cleanups = cleanupRows(run);
  const warnings = run.report?.warnings ?? [];
  const showLive = live && run.browser;
  return (
    <aside className="side" aria-label="Run details">
      {showLive && run.browser && (
        <div className="panel">
          <div className="panel-head">
            Live browser
            <span className="right mono">{run.browser.stepId}</span>
          </div>
          <LiveView browser={run.browser} />
        </div>
      )}
      <div className="panel">
        <div className="panel-head">
          Artifacts
          <span className="right">{groups.reduce((n, g) => n + g.artifacts.length, 0) || ""}</span>
        </div>
        {groups.length ? (
          groups.map((g) => (
            <div key={g.step.id}>
              <div className="side-group-label">
                {g.step.name}
                {g.step.name !== g.step.id && <span className="mono">{g.step.id}</span>}
              </div>
              <ArtifactGrid runId={run.runId} artifacts={g.artifacts} report={run.report} onOpen={ctx.openImage} compact />
            </div>
          ))
        ) : (
          <div className="panel-empty">{live ? "Screenshots, recordings and files appear here as steps save them." : "This run saved no artifacts."}</div>
        )}
      </div>
      {cleanups.length > 0 && (
        <div className="panel">
          <div className="panel-head">Cleanups</div>
          <div className="quiet-rows">
            {cleanups.map((c) => (
              <div key={c.stepId} className="quiet-row" title={c.command}>
                <StatusDot status={c.dot} label={c.label} />
                <span className="mono">{c.stepId}</span>
                {c.command && <span className="cleanup-cmd">{c.command}</span>}
                <span className="right">{c.label}</span>
              </div>
            ))}
          </div>
        </div>
      )}
      {warnings.length > 0 && (
        <div className="panel">
          <div className="panel-head">Warnings</div>
          <ul className="warnings-list">
            {warnings.map((w, i) => (
              <li key={i}>
                <IconWarning size={14} />
                <span>{w}</span>
              </li>
            ))}
          </ul>
        </div>
      )}
    </aside>
  );
}

function cleanupRows(run: RunState) {
  const rows: { stepId: string; command: string; dot: StepStatus; label: string }[] = [];
  const seen = new Set<string>();
  for (const c of run.report?.cleanups ?? []) {
    seen.add(c.stepId);
    const dot: StepStatus =
      c.status === "passed" ? "passed" : c.status === "failed" ? "failed" : c.status === "pending" ? "pending" : "skipped";
    const label =
      c.status === "not-run" ? (c.policy === "manual" ? "manual, not run" : "not run") : c.status;
    rows.push({ stepId: c.stepId, command: c.command.trim().split("\n")[0] ?? "", dot, label });
  }
  for (const [stepId, st] of Object.entries(run.cleanups)) {
    if (seen.has(stepId)) continue;
    rows.push({ stepId, command: "", dot: st, label: st === "running" ? "running" : st });
  }
  return rows;
}
