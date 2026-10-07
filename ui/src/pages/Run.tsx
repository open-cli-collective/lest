import { useState } from "react";
import { api } from "../api";
import { RunView } from "../components/RunView";
import { IconRefresh, IconStop } from "../components/icons";
import { useProject } from "../lib/project";
import { linkHandler, navigate } from "../lib/router";
import { isLive } from "../store/runState";
import { noteStarted, useRun } from "../store/store";

export function RunPage({ runId }: { runId: string }) {
  const { run } = useRun(runId);
  const project = useProject();
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [cancelled, setCancelled] = useState<"main" | "cleanup" | null>(null);
  const flowId = run?.flowId ?? run?.report?.flowId ?? null;
  const flowName = run?.flowName ?? run?.report?.flowName ?? flowId ?? "Run";
  const live = run ? isLive(run) : false;

  const again = async () => {
    if (!flowId) return;
    setBusy(true);
    setError(null);
    try {
      const r = await api.startRun({
        flowId,
        environment: run?.environment ?? undefined,
        inputs: run?.report?.inputs ?? {},
      });
      noteStarted(flowId, r.runId);
      navigate(`/flows/${encodeURIComponent(flowId)}`);
    } catch (e) {
      setError((e as Error).message);
    } finally {
      setBusy(false);
    }
  };

  const cancel = async () => {
    const stage = cancelled === "main" ? "cleanup" : "main";
    setCancelled(stage);
    try {
      await api.cancel(runId, stage);
    } catch (e) {
      setError((e as Error).message);
    }
  };

  const flowHref = flowId ? `/flows/${encodeURIComponent(flowId)}` : "/flows";
  return (
    <div className="page page-wide">
      <div className="flow-head">
        <div>
          <div className="crumbs">
            <a href="/runs" onClick={linkHandler("/runs")}>
              Runs
            </a>
            <span aria-hidden="true">/</span>
            <span className="mono">{runId}</span>
          </div>
          <h1>
            {flowId ? (
              <a href={flowHref} onClick={linkHandler(flowHref)} style={{ color: "inherit" }}>
                {flowName}
              </a>
            ) : (
              flowName
            )}
          </h1>
          {run?.report && (
            <div className="flow-meta">
              <span className="mono id">{run.report.flowId}</span>
              <span className="sep" />
              <span className="mono">{run.report.flowPath}</span>
            </div>
          )}
        </div>
        <div style={{ display: "flex", gap: 8, alignItems: "flex-start" }}>
          {live ? (
            <button type="button" className="btn btn-danger" onClick={() => void cancel()} disabled={cancelled === "cleanup"}>
              <IconStop size={13} />
              {cancelled ? "Skip cleanup" : "Cancel"}
            </button>
          ) : (
            <button type="button" className="btn" onClick={() => void again()} disabled={!flowId || busy}>
              <IconRefresh size={14} />
              Run again
            </button>
          )}
        </div>
        {error && (
          <div className="controls-error" role="alert">
            {error}
          </div>
        )}
      </div>
      <RunView runId={runId} projectRoot={project?.project.root ?? null} flowPath={run?.report?.flowPath} />
    </div>
  );
}
