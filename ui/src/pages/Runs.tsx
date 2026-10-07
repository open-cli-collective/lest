import { useEffect, useState } from "react";
import { api } from "../api";
import { IconWarning } from "../components/icons";
import { StatusDot, statusLabel } from "../components/status";
import { useNow } from "../components/ui";
import { formatDuration, formatTime, timeAgo } from "../lib/format";
import { useLoad } from "../lib/project";
import { linkHandler, navigate } from "../lib/router";
import { getRun, hydrate, useFinishedCount, useLiveRunIds } from "../store/store";
import type { RunResult } from "../types";

const RESULTS: RunResult[] = ["passed", "failed", "errored", "cancelled"];

export function RunsPage() {
  const finished = useFinishedCount();
  const liveIds = useLiveRunIds();
  const { data, error } = useLoad(() => Promise.all([api.runs(undefined, 200), api.flows()]), [finished, liveIds]);
  const [flow, setFlow] = useState("");
  const [results, setResults] = useState<RunResult[]>([]);
  const now = useNow(1000, true);
  useEffect(() => {
    for (const r of data?.[0].running ?? []) if (!getRun(r.runId)) void hydrate(r.runId);
  }, [data]);

  if (error) return <div className="page"><div className="banner banner-error">Could not load runs: {error}</div></div>;
  if (!data) return <div className="page"><div className="loading">Loading runs</div></div>;
  const [runs, flows] = data;
  const names = new Map(flows.flows.map((f) => [f.id, f.name]));
  const flowIds = [...new Set([...runs.runs.map((r) => r.flowId), ...runs.running.map((r) => r.flowId)])].sort();
  const running = runs.running.filter((r) => !flow || r.flowId === flow);
  const stored = runs.runs.filter(
    (r) => (!flow || r.flowId === flow) && (!results.length || results.includes(r.result)),
  );
  const open = (id: string) => navigate(`/runs/${encodeURIComponent(id)}`);

  return (
    <div className="page">
      <div className="page-head">
        <div>
          <h1>Runs</h1>
          <div className="sub">Newest first. Reports are kept in the data directory.</div>
        </div>
      </div>
      <div className="toolbar">
        <label className="field" style={{ flexDirection: "row", alignItems: "center", gap: 8 }}>
          <span className="field-label">Flow</span>
          <select className="select" value={flow} onChange={(e) => setFlow(e.target.value)}>
            <option value="">All flows</option>
            {flowIds.map((id) => (
              <option key={id} value={id}>
                {names.get(id) ?? id}
              </option>
            ))}
          </select>
        </label>
        <div className="chips" role="group" aria-label="Filter by result">
          {RESULTS.map((r) => (
            <button
              key={r}
              type="button"
              className="chip"
              aria-pressed={results.includes(r)}
              onClick={() => setResults(results.includes(r) ? results.filter((x) => x !== r) : [...results, r])}
            >
              <StatusDot status={r} />
              {r}
            </button>
          ))}
        </div>
      </div>
      {running.length === 0 && stored.length === 0 ? (
        <div className="empty">
          <h2>{runs.runs.length ? "No run matches" : "No runs yet"}</h2>
          <p>{runs.runs.length ? "Change the filters to see more runs." : "Start a flow from the Flows page or with lest run."}</p>
        </div>
      ) : (
        <div className="table-wrap">
          <table className="runs">
            <thead>
              <tr>
                <th>Status</th>
                <th>Flow</th>
                <th>Run</th>
                <th>Environment</th>
                <th>Started</th>
                <th className="num">Duration</th>
                <th className="num">Warnings</th>
              </tr>
            </thead>
            <tbody>
              {!results.length &&
                running.map((r) => {
                  const live = getRun(r.runId);
                  const started = live?.startedAtMs ?? null;
                  return (
                    <tr key={r.runId} className="running edge-running" onClick={() => open(r.runId)}>
                      <td>
                        <span className="status-cell">
                          <StatusDot status="running" />
                          running
                        </span>
                      </td>
                      <td className="flow-cell">
                        <a href={`/runs/${r.runId}`} onClick={linkHandler(`/runs/${r.runId}`)}>
                          {names.get(r.flowId) ?? r.flowId}
                        </a>
                      </td>
                      <td className="id-cell">{r.runId}</td>
                      <td>{live?.environment ?? ""}</td>
                      <td title={started ? formatTime(started) : undefined}>{started ? timeAgo(started, now) : ""}</td>
                      <td className="num">{started ? formatDuration(now - started) : ""}</td>
                      <td className="num" />
                    </tr>
                  );
                })}
              {stored.map((r) => (
                <tr key={r.runId} className={`edge-${r.result}`} onClick={() => open(r.runId)}>
                  <td>
                    <span className="status-cell">
                      <StatusDot status={r.result} />
                      {statusLabel(r.result)}
                    </span>
                  </td>
                  <td className="flow-cell">
                    <a href={`/runs/${r.runId}`} onClick={linkHandler(`/runs/${r.runId}`)}>
                      {r.flowName}
                    </a>
                  </td>
                  <td className="id-cell">{r.runId}</td>
                  <td>{r.environment ?? ""}</td>
                  <td title={formatTime(r.startedAt)}>{timeAgo(r.startedAt, now)}</td>
                  <td className="num">{formatDuration(r.durationMs)}</td>
                  <td className="num">
                    {r.warnings > 0 ? (
                      <span style={{ color: "var(--warning)", display: "inline-flex", gap: 4, alignItems: "center" }}>
                        <IconWarning size={13} />
                        {r.warnings}
                      </span>
                    ) : (
                      <span className="muted">0</span>
                    )}
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      )}
    </div>
  );
}
