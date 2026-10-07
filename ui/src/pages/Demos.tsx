import { api, fileUrl } from "../api";
import { StatusDot } from "../components/status";
import { formatTime, timeAgo } from "../lib/format";
import { plainFirstLine } from "../lib/markdown";
import { useLoad } from "../lib/project";
import { linkHandler } from "../lib/router";
import { useFinishedCount } from "../store/store";
import type { FlowSummary, RunReport, StepReport } from "../types";

export function isDemo(f: FlowSummary): boolean {
  return !!f.demo || f.tags.includes("demo");
}

function walk(steps: StepReport[]): StepReport[] {
  return steps.flatMap((s) => [s, ...walk(s.children ?? [])]);
}

/** A still for a demo card: the beat sheet, else the recording step's last
 * screenshot, else any screenshot. */
export function coverOf(report: RunReport): string | null {
  if (report.demo?.beatSheet) return report.demo.beatSheet;
  const steps = walk([...report.steps, ...(report.finally ?? [])]);
  const rec = steps.find((s) => s.artifacts?.some((a) => a.label === "Recording"));
  const pick = (s?: StepReport) => s?.artifacts?.filter((a) => a.mime.startsWith("image/")).pop()?.path ?? null;
  return pick(rec) ?? steps.map(pick).filter(Boolean).pop() ?? null;
}

export function DemosPage() {
  const finished = useFinishedCount();
  const { data, error } = useLoad(() => api.flows(), [finished]);
  if (error) return <div className="page"><div className="banner banner-error">Could not load flows: {error}</div></div>;
  if (!data) return <div className="page"><div className="loading">Loading demos</div></div>;
  const demos = data.flows.filter(isDemo);
  return (
    <div className="page">
      <div className="page-head">
        <div>
          <h1>Demos</h1>
          <div className="sub">Flows that record a walkthrough. Recording one runs the flow.</div>
        </div>
      </div>
      {demos.length === 0 ? (
        <div className="empty">
          <h2>No demos in this project</h2>
          <p>
            A demo is a flow with a <code>demo:</code> block (title, summary, beat labels) and a browser step with{" "}
            <code>record: true</code>. Flows tagged <code>demo</code> are listed here too.
          </p>
        </div>
      ) : (
        <div className="card-grid" style={{ gridTemplateColumns: "repeat(auto-fill, minmax(340px, 1fr))" }}>
          {demos.map((f) => (
            <DemoCard key={f.id} flow={f} />
          ))}
        </div>
      )}
    </div>
  );
}

function DemoCard({ flow }: { flow: FlowSummary }) {
  const last = flow.lastRun;
  const { data } = useLoad(() => (last ? api.run(last.runId) : Promise.resolve(null)), [last?.runId]);
  const cover = data?.report ? coverOf(data.report) : null;
  const href = `/demos/${encodeURIComponent(flow.id)}`;
  const status = last?.result ?? "none";
  return (
    <a className={`card demo-card edge-${status}`} href={href} onClick={linkHandler(href)}>
      <div className="demo-cover">
        {cover && last ? <img src={fileUrl(last.runId, cover)} alt="" loading="lazy" /> : <span>No recording yet</span>}
      </div>
      <div className="demo-text">
        <h3>{flow.demo?.title ?? flow.name}</h3>
        <div className="card-desc" style={{ minHeight: 0 }}>
          {flow.demo?.summary ?? plainFirstLine(flow.description)}
        </div>
        {flow.demo?.beats && flow.demo.beats.length > 0 && (
          <ol className="story">
            {flow.demo.beats.map((b) => (
              <li key={b.marker}>{b.label}</li>
            ))}
          </ol>
        )}
        <div className="card-foot">
          <StatusDot status={status} />
          <span className="muted" style={{ fontSize: 12 }} title={last ? formatTime(last.startedAt) : undefined}>
            {last ? `Last recorded ${timeAgo(last.startedAt)}${last.result !== "passed" ? ` (${last.result})` : ""}` : "Never recorded"}
          </span>
        </div>
      </div>
    </a>
  );
}
