import { useState } from "react";
import { api, fileUrl } from "../api";
import { ArtifactGrid } from "../components/RunView";
import { IconDownload, IconFile } from "../components/icons";
import { LiveView } from "../components/LiveView";
import { RunControls } from "../components/RunControls";
import { ResultBadge } from "../components/status";
import { Lightbox, useNow } from "../components/ui";
import { formatBytes, formatDuration, formatTime, timeAgo } from "../lib/format";
import { useLoad } from "../lib/project";
import { linkHandler } from "../lib/router";
import { allStepIds, isLive, runBeats, type RunState } from "../store/runState";
import { useRun } from "../store/store";
import type { Artifact, FlowDetail } from "../types";
import { FlowDiagnostics, FlowHeader, useShownRun } from "./Flow";

function clock(ms: number): string {
  const s = Math.floor(ms / 1000);
  return `${Math.floor(s / 60)}:${String(s % 60).padStart(2, "0")}`;
}

interface Deliverable {
  video: { path: string; label: string; chapters: string | null } | null;
  extras: { label: string; path: string; bytes?: number }[];
  stills: Artifact[];
}

function deliverable(run: RunState): Deliverable {
  const arts = allStepIds(run).flatMap((id) => run.steps[id]?.artifacts ?? []);
  const demo = run.report?.demo;
  const recording = arts.find((a) => a.label === "Recording");
  const extras: Deliverable["extras"] = [];
  let video: Deliverable["video"] = null;
  if (demo?.video) {
    video = { path: demo.video, label: "Demo video", chapters: demo.chapters ?? null };
    if (demo.rawVideo) extras.push({ label: "Raw recording", path: demo.rawVideo });
  } else if (recording) {
    video = { path: recording.path, label: "Recording", chapters: null };
  }
  if (demo?.chapters) extras.push({ label: "Chapters (WebVTT)", path: demo.chapters });
  if (demo?.beatSheet) extras.push({ label: "Beat sheet", path: demo.beatSheet });
  for (const a of arts) {
    if (a.mime.startsWith("video/") && a.path !== video?.path) extras.push({ label: a.label, path: a.path, bytes: a.bytes });
  }
  if (recording && video?.path !== recording.path && !extras.some((e) => e.path === recording.path)) {
    extras.push({ label: "Raw recording", path: recording.path, bytes: recording.bytes });
  }
  return { video, extras, stills: arts.filter((a) => a.mime.startsWith("image/")) };
}

export function DemoPage({ id }: { id: string }) {
  const { data: flow, error } = useLoad(() => api.flow(id), [id]);
  const shown = useShownRun(id);
  if (error) {
    return (
      <div className="page">
        <div className="empty">
          <h2>Demo not found</h2>
          <p>{error}</p>
        </div>
      </div>
    );
  }
  if (!flow) return <div className="page"><div className="loading">Loading demo</div></div>;
  return (
    <div className="page page-wide">
      <FlowHeader flow={flow} section="demos" />
      <FlowDiagnostics flow={flow} />
      <RunControls flow={flow} runId={shown.runId} record />
      <DemoBody flow={flow} runId={shown.runId} />
    </div>
  );
}

function DemoBody({ flow, runId }: { flow: FlowDetail; runId: string | null }) {
  const { run } = useRun(runId);
  const [image, setImage] = useState<{ src: string; caption: string } | null>(null);
  const live = run ? isLive(run) : false;
  const now = useNow(250, live);
  const seen = run ? runBeats(run) : [];
  const beats = flow.demo?.beats ?? [];
  const litMarkers = new Set(seen.map((b) => b.marker));
  const lastLit = [...beats].reverse().find((b) => litMarkers.has(b.marker))?.marker;
  const d = run ? deliverable(run) : null;

  let stage;
  if (run && live && run.browser) {
    stage = <LiveView browser={run.browser} big />;
  } else if (run && live) {
    stage = (
      <div className="demo-none">
        <div>
          <strong>Preparing the recording</strong>
          {run.progress.length ? run.progress[run.progress.length - 1]!.message : "Running the steps before the recording"}
        </div>
      </div>
    );
  } else if (run && d?.video) {
    stage = (
      <div className="demo-video">
        <video controls preload="metadata" src={fileUrl(run.runId, d.video.path)}>
          {d.video.chapters && <track kind="chapters" src={fileUrl(run.runId, d.video.chapters)} default />}
        </video>
      </div>
    );
  } else {
    stage = (
      <div className="demo-none">
        <div>
          <strong>No recording yet</strong>
          {run
            ? `The latest run (${run.finished?.result ?? "unknown"}) saved no recording.`
            : "Press Record to run the flow and record the walkthrough."}
        </div>
      </div>
    );
  }

  return (
    <div className="demo-layout">
      <div className="demo-stage">
        <div className="panel">
          <div className="panel-head">
            {live ? "Recording" : "Recording"}
            <span className="right">
              {run && d?.video && !live && (
                <a href={fileUrl(run.runId, d.video.path)} download className="btn btn-sm btn-ghost">
                  <IconDownload size={14} />
                  Download
                </a>
              )}
            </span>
          </div>
          {stage}
        </div>
        {run && !live && d && (d.extras.length > 0 || d.stills.length > 0) && (
          <div className="panel">
            <div className="panel-head">Extras</div>
            {d.extras.length > 0 && (
              <ul className="extras">
                {d.extras.map((e) => (
                  <li key={e.path}>
                    <IconFile size={14} />
                    <a href={fileUrl(run.runId, e.path)} download>
                      {e.label}
                    </a>
                    <span className="muted">{e.bytes ? formatBytes(e.bytes) : ""}</span>
                  </li>
                ))}
              </ul>
            )}
            {d.stills.length > 0 && (
              <ArtifactGrid runId={run.runId} artifacts={d.stills} report={run.report} onOpen={setImage} />
            )}
          </div>
        )}
      </div>
      <aside className="side" aria-label="Story">
        <div className="panel">
          <div className="panel-head">
            Story
            <span className="right">{beats.length ? `${seen.filter((b) => beats.some((x) => x.marker === b.marker)).length} of ${beats.length}` : ""}</span>
          </div>
          {beats.length ? (
            <ol className="story big">
              {beats.map((b) => {
                const at = seen.find((s) => s.marker === b.marker);
                return (
                  <li
                    key={b.marker}
                    className={`${at ? "lit" : ""}${live && b.marker === lastLit ? " current" : ""}`}
                  >
                    {b.label}
                    {at && <span className="at">{clock(at.atMs)}</span>}
                  </li>
                );
              })}
            </ol>
          ) : (
            <div className="panel-empty">This flow labels no beats.</div>
          )}
        </div>
        {run && (
          <div className="panel">
            <div className="panel-head">
              {live ? "This recording" : "Latest recording"}
              <span className="right">
                <a href={`/runs/${run.runId}`} onClick={linkHandler(`/runs/${run.runId}`)}>
                  Open run
                </a>
              </span>
            </div>
            <div className="kv">
              <div className="k">Result</div>
              <div className="v">
                <ResultBadge status={run.finished?.result ?? "running"} />
              </div>
              <div className="k">Started</div>
              <div className="v" title={run.startedAtMs ? formatTime(run.startedAtMs) : undefined}>
                {run.startedAtMs ? timeAgo(run.startedAtMs, Math.max(now, Date.now())) : ""}
              </div>
              <div className="k">Duration</div>
              <div className="v">
                {formatDuration(run.finished ? run.finished.durationMs : run.startedAtMs ? now - run.startedAtMs : 0)}
              </div>
              {run.environment && (
                <>
                  <div className="k">Environment</div>
                  <div className="v">{run.environment}</div>
                </>
              )}
              {Object.entries(run.report?.inputs ?? {}).map(([k, v]) => (
                <div key={k} style={{ display: "contents" }}>
                  <div className="k mono">{k}</div>
                  <div className="v">{v}</div>
                </div>
              ))}
              {run.report?.demo?.error && (
                <>
                  <div className="k">Post-production</div>
                  <div className="v">{run.report.demo.error}</div>
                </>
              )}
              {run.finished?.error && (
                <>
                  <div className="k">Error</div>
                  <div className="v">{run.finished.error}</div>
                </>
              )}
            </div>
          </div>
        )}
      </aside>
      {image && <Lightbox src={image.src} caption={image.caption} onClose={() => setImage(null)} />}
    </div>
  );
}
