import { api } from "../api";
import { RunControls } from "../components/RunControls";
import { RunView } from "../components/RunView";
import { IconWarning } from "../components/icons";
import { Markdown } from "../lib/markdown";
import { useLoad, useProject } from "../lib/project";
import { linkHandler } from "../lib/router";
import { useLatestRunOf } from "../store/store";
import type { FlowDetail } from "../types";

export function FlowHeader({ flow, section = "flows" }: { flow: FlowDetail; section?: "flows" | "demos" }) {
  return (
    <div className="flow-head">
      <div>
        <div className="crumbs">
          <a href={`/${section}`} onClick={linkHandler(`/${section}`)}>
            {section === "flows" ? "Flows" : "Demos"}
          </a>
        </div>
        <h1>{section === "demos" ? (flow.demo?.title ?? flow.name) : flow.name}</h1>
        <div className="flow-meta">
          <span className="mono id">{flow.id}</span>
          <span className="sep" />
          <span className="mono">{flow.path}</span>
          {flow.tags.length > 0 && (
            <>
              <span className="sep" />
              {flow.tags.map((t) => (
                <span key={t} className="chip">
                  {t}
                </span>
              ))}
            </>
          )}
        </div>
        {section === "demos" && flow.demo?.summary ? (
          <div className="flow-desc">{flow.demo.summary}</div>
        ) : (
          flow.description && <Markdown className="flow-desc" text={flow.description} />
        )}
      </div>
    </div>
  );
}

export function FlowDiagnostics({ flow }: { flow: FlowDetail }) {
  if (!flow.diagnostics.length) return null;
  const errors = flow.diagnostics.some((d) => d.severity === "error");
  return (
    <div className={`banner ${errors ? "banner-error" : "banner-warning"}`} role="alert">
      <IconWarning />
      <div className="banner-body">
        <div className="banner-title">{errors ? "This flow cannot run until these are fixed" : "Warnings in this flow"}</div>
        <ul>
          {flow.diagnostics.map((d, i) => (
            <li key={i}>
              {d.at && <span className="mono">{d.at}</span>}
              <span>{d.message}</span>
            </li>
          ))}
        </ul>
      </div>
    </div>
  );
}

/** The run a flow page shows: one started since the page loaded, else one in
 * progress on the server, else the latest stored run. */
export function useShownRun(flowId: string): { runId: string | null; loaded: boolean } {
  const started = useLatestRunOf(flowId);
  const { data } = useLoad(() => api.runs(flowId, 1), [flowId]);
  const initial = data ? (data.running[0]?.runId ?? data.runs[0]?.runId ?? null) : null;
  return { runId: started ?? initial, loaded: !!data || !!started };
}

export function FlowPage({ id }: { id: string }) {
  const { data: flow, error } = useLoad(() => api.flow(id), [id]);
  const project = useProject();
  const shown = useShownRun(id);

  if (error) {
    return (
      <div className="page">
        <div className="empty">
          <h2>Flow not found</h2>
          <p>{error}</p>
          <p>
            <a href="/flows" onClick={linkHandler("/flows")}>
              Back to flows
            </a>
          </p>
        </div>
      </div>
    );
  }
  if (!flow) return <div className="page"><div className="loading">Loading flow</div></div>;

  return (
    <div className="page page-wide">
      <FlowHeader flow={flow} />
      <FlowDiagnostics flow={flow} />
      <RunControls flow={flow} runId={shown.runId} />
      {shown.runId ? (
        <RunView runId={shown.runId} projectRoot={project?.project.root ?? null} flowPath={flow.path} />
      ) : shown.loaded ? (
        <div className="empty">
          <h2>No runs yet</h2>
          <p>Press Run to start this flow. Steps, output and artifacts appear here as it runs.</p>
        </div>
      ) : null}
    </div>
  );
}
