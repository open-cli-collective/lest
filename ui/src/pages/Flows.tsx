import { useMemo, useState } from "react";
import { api } from "../api";
import { IconSearch, IconWarning, IconX } from "../components/icons";
import { StatusDot, type AnyStatus } from "../components/status";
import { useNow } from "../components/ui";
import { formatDuration, timeAgo } from "../lib/format";
import { plainFirstLine } from "../lib/markdown";
import { CARD_TAG_LIMIT, tagFilters } from "../lib/tags";
import { useLoad } from "../lib/project";
import { linkHandler } from "../lib/router";
import { getRun, useFinishedCount, useLiveRunIds } from "../store/store";
import type { Diagnostic, FlowSummary } from "../types";

/** Flow ids with a run in progress that this page has seen. */
export function useRunningFlows(): Set<string> {
  const ids = useLiveRunIds();
  return useMemo(() => {
    const out = new Set<string>();
    for (const id of ids ? ids.split(",") : []) {
      const f = getRun(id)?.flowId;
      if (f) out.add(f);
    }
    return out;
  }, [ids]);
}

export function FlowsPage() {
  const finished = useFinishedCount();
  const { data, error } = useLoad(() => api.flows(), [finished]);
  const [query, setQuery] = useState("");
  const [tags, setTags] = useState<string[]>([]);
  const [allTagsShown, setAllTagsShown] = useState(false);
  const [dismissed, setDismissed] = useState(false);
  const running = useRunningFlows();
  const now = useNow(30_000, true);

  if (error) return <div className="page"><div className="banner banner-error">Could not load flows: {error}</div></div>;
  if (!data) return <div className="page"><div className="loading">Loading flows</div></div>;

  const filters = tagFilters(
    data.flows.map((f) => f.tags),
    tags,
    allTagsShown,
  );
  const q = query.trim().toLowerCase();
  const visible = data.flows.filter(
    (f) =>
      (!q || f.name.toLowerCase().includes(q) || f.id.includes(q) || f.tags.some((t) => t.toLowerCase().includes(q))) &&
      tags.every((t) => f.tags.includes(t)),
  );
  // Folders are shown relative to the directory every flow shares (often
  // the project's single flows folder).
  const firsts = new Set(data.flows.map((f) => f.path.split("/")[0]));
  const shared = firsts.size === 1 && data.flows.every((f) => f.path.includes("/")) ? `${[...firsts][0]}/` : "";
  const label = (folder: string) => (folder + "/").slice(shared.length).replace(/\/$/, "");
  const folders = new Map<string, FlowSummary[]>();
  for (const f of visible) {
    const key = label(f.folder);
    const list = folders.get(key) ?? [];
    list.push(f);
    folders.set(key, list);
  }
  const diagsByFile = new Map<string, Diagnostic[]>();
  for (const d of data.diagnostics) diagsByFile.set(d.file, [...(diagsByFile.get(d.file) ?? []), d]);

  return (
    <div className="page">
      <div className="page-head">
        <div>
          <h1>Flows</h1>
          <div className="sub">
            {data.flows.length} {data.flows.length === 1 ? "flow" : "flows"} in this project
          </div>
        </div>
      </div>

      {data.diagnostics.length > 0 && !dismissed && (
        <div className={`banner ${data.diagnostics.some((d) => d.severity === "error") ? "banner-error" : "banner-warning"}`} role="alert">
          <IconWarning />
          <div className="banner-body">
            <div className="banner-title">
              {data.diagnostics.length === 1 ? "1 problem" : `${data.diagnostics.length} problems`} in flow files
            </div>
            <ul>
              {data.diagnostics.map((d, i) => (
                <li key={i}>
                  <span className="mono">{d.file}</span>
                  <span>
                    {d.at ? <span className="mono">{d.at}: </span> : null}
                    {d.message}
                  </span>
                </li>
              ))}
            </ul>
          </div>
          <button type="button" className="btn btn-ghost btn-sm btn-icon" onClick={() => setDismissed(true)}>
            <IconX />
            <span className="visually-hidden">Dismiss</span>
          </button>
        </div>
      )}

      {data.flows.length === 0 ? (
        <div className="empty">
          <h2>No flows yet</h2>
          <p>
            Lest looks for <code>*.lest.yaml</code> files in the folders listed under <code>flows:</code> in{" "}
            <code>lest.yaml</code> at the project root.
          </p>
          <p>
            Run <code>lest init</code> in your project to create <code>lest.yaml</code> and a first flow, then reload
            this page.
          </p>
        </div>
      ) : (
        <>
          <div className="toolbar">
            <label className="search">
              <IconSearch />
              <span className="visually-hidden">Search flows</span>
              <input
                className="input"
                type="search"
                placeholder="Search by name, id or tag"
                value={query}
                onChange={(e) => setQuery(e.target.value)}
              />
            </label>
            {filters.shown.length > 0 && (
              <div className="chips" role="group" aria-label="Filter by tag">
                {filters.shown.map((t) => (
                  <button
                    key={t}
                    type="button"
                    className="chip"
                    aria-pressed={tags.includes(t)}
                    onClick={() => setTags(tags.includes(t) ? tags.filter((x) => x !== t) : [...tags, t])}
                  >
                    {t}
                  </button>
                ))}
                {(filters.hidden > 0 || allTagsShown) && (
                  <button
                    type="button"
                    className="chip chip-more"
                    aria-expanded={allTagsShown}
                    onClick={() => setAllTagsShown(!allTagsShown)}
                  >
                    {allTagsShown ? "Fewer tags" : `${filters.hidden} more`}
                  </button>
                )}
              </div>
            )}
          </div>
          {visible.length === 0 && <div className="empty"><p>No flow matches.</p></div>}
          {[...folders.entries()]
            .sort(([a], [b]) => a.localeCompare(b))
            .map(([folder, flows]) => (
              <section key={folder} aria-label={folder || "Top level"}>
                <div className="section-label">
                  {folder || "top level"} <span className="n">{flows.length}</span>
                </div>
                <div className="card-grid">
                  {flows.map((f) => (
                    <FlowCard
                      key={f.id}
                      flow={f}
                      now={now}
                      running={running.has(f.id)}
                      problems={diagsByFile.get(f.path) ?? []}
                    />
                  ))}
                </div>
              </section>
            ))}
        </>
      )}
    </div>
  );
}

function FlowCard({
  flow,
  now,
  running,
  problems,
}: {
  flow: FlowSummary;
  now: number;
  running: boolean;
  problems: Diagnostic[];
}) {
  const status: AnyStatus = running ? "running" : (flow.lastRun?.result ?? "none");
  const href = `/flows/${encodeURIComponent(flow.id)}`;
  return (
    <a className={`card edge-${status}`} href={href} onClick={linkHandler(href)}>
      <div className="card-title">
        <StatusDot status={status} />
        <span className="name">{flow.name}</span>
        {problems.length > 0 && (
          <span className="card-warn" title={problems.map((p) => p.message).join("\n")}>
            <IconWarning size={14} />
            <span className="visually-hidden">{problems.length} problems</span>
          </span>
        )}
      </div>
      <div className="card-desc">{plainFirstLine(flow.description) || <span className="muted">No description</span>}</div>
      <div className="card-foot">
        <div className="tags">
          {flow.tags.slice(0, CARD_TAG_LIMIT).map((t) => (
            <span key={t} className="chip">
              {t}
            </span>
          ))}
          {flow.tags.length > CARD_TAG_LIMIT && (
            <span
              className="chip chip-count"
              title={flow.tags.slice(CARD_TAG_LIMIT).join(", ")}
              aria-label={`Also tagged ${flow.tags.slice(CARD_TAG_LIMIT).join(", ")}`}
            >
              +{flow.tags.length - CARD_TAG_LIMIT}
            </span>
          )}
        </div>
        <div className="meta">
          {running ? (
            <span>running</span>
          ) : flow.lastRun ? (
            <>
              <span title={flow.lastRun.startedAt}>{timeAgo(flow.lastRun.startedAt, now)}</span>
              <span>{formatDuration(flow.lastRun.durationMs)}</span>
            </>
          ) : (
            <span>never run</span>
          )}
        </div>
      </div>
    </a>
  );
}
