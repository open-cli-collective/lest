import { useEffect, useState, type ReactNode } from "react";
import {
  IconDemos,
  IconFlows,
  IconRuns,
  IconSettings,
  IconSidebar,
  LogoMark,
} from "./components/icons";
import { Toaster } from "./components/ui";
import { linkHandler, matchRoute, usePath } from "./lib/router";
import { readPref, useProject, writePref } from "./lib/project";
import { useConnection, useLiveRunIds } from "./store/store";
import { FlowsPage } from "./pages/Flows";
import { FlowPage } from "./pages/Flow";
import { RunsPage } from "./pages/Runs";
import { RunPage } from "./pages/Run";
import { DemosPage } from "./pages/Demos";
import { DemoPage } from "./pages/Demo";
import { SettingsPage } from "./pages/Settings";

const RAIL_KEY = "lest.railCollapsed";

export function App() {
  const path = usePath();
  const route = matchRoute(path);
  const [collapsed, setCollapsed] = useState(() => readPref(RAIL_KEY, false));
  const project = useProject();

  useEffect(() => {
    const base = project?.project.name ?? "lest";
    const title: Record<string, string> = {
      flows: "Flows",
      flow: "Flow",
      runs: "Runs",
      run: "Run",
      demos: "Demos",
      demo: "Demo",
      settings: "Settings",
      notFound: "Not found",
    };
    document.title = `${title[route.name]} - ${base}`;
  }, [route.name, project]);

  let page: ReactNode;
  switch (route.name) {
    case "flows":
      page = <FlowsPage />;
      break;
    case "flow":
      page = <FlowPage key={route.id} id={route.id} />;
      break;
    case "runs":
      page = <RunsPage />;
      break;
    case "run":
      page = <RunPage key={route.runId} runId={route.runId} />;
      break;
    case "demos":
      page = <DemosPage />;
      break;
    case "demo":
      page = <DemoPage key={route.id} id={route.id} />;
      break;
    case "settings":
      page = <SettingsPage />;
      break;
    default:
      page = (
        <div className="page">
          <div className="empty">
            <h2>Nothing here</h2>
            <p>
              <a href="/flows" onClick={linkHandler("/flows")}>
                Go to flows
              </a>
            </p>
          </div>
        </div>
      );
  }

  const section =
    route.name === "flow" || route.name === "flows"
      ? "flows"
      : route.name === "run" || route.name === "runs"
        ? "runs"
        : route.name === "demo" || route.name === "demos"
          ? "demos"
          : route.name;

  return (
    <div className={`app${collapsed ? " rail-collapsed" : ""}`}>
      <Rail
        section={section}
        collapsed={collapsed}
        projectName={project?.project.name ?? null}
        onToggle={() => {
          writePref(RAIL_KEY, !collapsed);
          setCollapsed(!collapsed);
        }}
      />
      <main className="main">{page}</main>
      <Toaster />
    </div>
  );
}

function Rail({
  section,
  collapsed,
  projectName,
  onToggle,
}: {
  section: string;
  collapsed: boolean;
  projectName: string | null;
  onToggle: () => void;
}) {
  const live = useLiveRunIds();
  const liveCount = live ? live.split(",").length : 0;
  const conn = useConnection();
  const link = (to: string, id: string, label: string, icon: ReactNode, extra?: ReactNode) => (
    <a
      href={to}
      className="rail-link"
      aria-current={section === id ? "page" : undefined}
      onClick={linkHandler(to)}
      title={collapsed ? label : undefined}
    >
      {icon}
      <span className="rail-label">{label}</span>
      {extra}
    </a>
  );
  return (
    <nav className="rail" aria-label="Main">
      <div className="rail-head">
        <a href="/flows" className="wordmark" onClick={linkHandler("/flows")} aria-label="lest, flows">
          <LogoMark size={22} />
          <span className="wordmark-text">lest</span>
        </a>
      </div>
      {projectName && (
        <div className="rail-project" title={projectName}>
          Project
          <strong>{projectName}</strong>
        </div>
      )}
      {link("/flows", "flows", "Flows", <IconFlows />)}
      {link(
        "/runs",
        "runs",
        "Runs",
        <IconRuns />,
        liveCount > 0 ? (
          <span className="count" title={`${liveCount} running`}>
            <span className="dot dot-running" />
            <span className="count-text">{liveCount} running</span>
          </span>
        ) : undefined,
      )}
      {link("/demos", "demos", "Demos", <IconDemos />)}
      <div className="rail-spacer" />
      <div className="rail-foot">
        {conn !== "open" && (
          <div className="rail-toggle" role="status" title="The live event stream is reconnecting">
            <span className="dot dot-errored" />
            <span className="rail-label">Reconnecting</span>
          </div>
        )}
        {link("/settings", "settings", "Settings", <IconSettings />)}
        <button type="button" className="rail-toggle" onClick={onToggle} aria-pressed={collapsed}>
          <IconSidebar />
          <span className="rail-label">Collapse</span>
          <span className="visually-hidden">{collapsed ? "Expand the sidebar" : ""}</span>
        </button>
      </div>
    </nav>
  );
}
