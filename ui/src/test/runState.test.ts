import { describe, expect, it } from "vitest";
import {
  applyEvent,
  applyEvents,
  applyReport,
  emptyRun,
  firstFailure,
  reduceRuns,
  runBeats,
  topLevelOf,
  type RunState,
} from "../store/runState";
import type { RunEvent, RunReport, StepReport } from "../types";
import plantsApi from "./fixtures/plants-api.json";
import connectWeather from "./fixtures/connect-weather.json";
import wrongPassword from "./fixtures/wrong-password.json";
import everything from "./fixtures/everything.json";
import cancelled from "./fixtures/plants-api-cancelled.json";

const ev = (x: unknown) => x as RunEvent[];

function run(events: RunEvent[]): RunState {
  const id = events[0]!.runId;
  return applyEvents(emptyRun(id), events);
}

function statuses(r: RunState): Record<string, string> {
  return Object.fromEntries(Object.values(r.steps).map((s) => [s.id, s.status]));
}

describe("a passed API run", () => {
  const events = ev(plantsApi);
  const r = run(events);

  it("builds the tree from runStarted and finishes every step", () => {
    expect(r.roots).toEqual(["sign_in", "add_plant", "sensor_online", "listed"]);
    expect(statuses(r)).toEqual({
      sign_in: "passed",
      add_plant: "passed",
      sensor_online: "passed",
      listed: "passed",
    });
    expect(r.finished?.result).toBe("passed");
    expect(r.lastSeq).toBe(events.length - 1);
  });

  it("records attempts and the cumulative duration of a polled step", () => {
    const s = r.steps.sensor_online!;
    expect(s.attempt).toBe(3);
    expect(s.maxAttempts).toBe(6);
    expect(s.retry).toBeNull();
    expect(s.durationMs).toBe(622);
    expect(s.notes).toContain("pairing");
  });

  it("shows the retry countdown between attempts", () => {
    const upTo = events.filter((e) => e.seq <= 7);
    const mid = run(upTo);
    const s = mid.steps.sensor_online!;
    expect(s.status).toBe("running");
    expect(s.retry).toMatchObject({ attempt: 1, maxAttempts: 6 });
    expect(s.retry!.nextAtMs).toBe(upTo[7]!.atMs + 300);
    const next = applyEvent(mid, events[8]!);
    expect(next.steps.sensor_online!.retry).toBeNull();
    expect(next.steps.sensor_online!.attempt).toBe(2);
    expect(next.steps.sensor_online!.startedAtMs).toBe(s.startedAtMs);
  });

  it("collects live output and cleanup status", () => {
    expect(r.steps.listed!.output.length).toBe(1);
    expect(r.steps.listed!.output[0]!.stream).toBe("stdout");
    expect(r.cleanups).toEqual({ add_plant: "passed" });
    expect(r.progress.map((p) => p.message)).toEqual(["starting service sprout"]);
  });
});

describe("seq handling", () => {
  const events = ev(plantsApi);

  it("ignores duplicates and stale events", () => {
    const once = run(events);
    const twice = events.reduce(applyEvent, once);
    expect(twice).toBe(once);
    const replayed = run([...events, ...events.slice(0, 5)]);
    expect(statuses(replayed)).toEqual(statuses(once));
  });

  it("marks a gap instead of applying an event after a missing one", () => {
    const partial = events.slice(0, 4).reduce(applyEvent, emptyRun(events[0]!.runId));
    const skipped = applyEvent(partial, events[6]!);
    expect(skipped.gap).toBe(true);
    expect(skipped.lastSeq).toBe(3);
    expect(skipped.steps.sensor_online!.status).toBe("pending");
    // Hydrating with the full list fills the gap.
    const healed = reduceRuns({ runs: { [partial.runId]: skipped } }, {
      type: "events",
      runId: partial.runId,
      events,
    }).runs[partial.runId]!;
    expect(healed.gap).toBe(false);
    expect(healed.finished?.result).toBe("passed");
  });

  it("applies events delivered out of order once sorted by hydration", () => {
    const shuffled = [...events].reverse();
    expect(statuses(run(shuffled))).toEqual(statuses(run(events)));
  });

  it("never lets one run's events touch another run", () => {
    const a = ev(plantsApi);
    const b = ev(wrongPassword);
    let state = { runs: {} as Record<string, RunState> };
    const interleaved: RunEvent[] = [];
    for (let i = 0; i < Math.max(a.length, b.length); i++) {
      if (a[i]) interleaved.push(a[i]!);
      if (b[i]) interleaved.push(b[i]!);
    }
    for (const e of interleaved) state = reduceRuns(state, { type: "event", event: e });
    const ra = state.runs[a[0]!.runId]!;
    const rb = state.runs[b[0]!.runId]!;
    expect(Object.keys(ra.steps)).toEqual(["sign_in", "add_plant", "sensor_online", "listed"]);
    expect(Object.keys(rb.steps)).toEqual(["api_login", "page_login"]);
    expect(ra.finished?.result).toBe("passed");
    expect(rb.finished?.result).toBe("failed");
    // A foreign event passed straight to a run's state is ignored.
    expect(applyEvent(ra, b[3]!)).toBe(ra);
  });
});

describe("a failed browser run", () => {
  const r = run(ev(wrongPassword));

  it("finds the failing leaf step, not the tolerated one", () => {
    expect(r.steps.api_login!.status).toBe("failed");
    expect(r.steps.api_login!.report?.tolerated).toBe(true);
    const f = firstFailure(r)!;
    expect(f.id).toBe("page_login");
    expect(f.report?.headline).toContain("Good morning");
  });

  it("keeps the failure screenshot and the browser output", () => {
    const s = r.steps.page_login!;
    expect(s.artifacts.map((a) => a.label)).toEqual(["Screenshot at failure"]);
    expect(s.output.filter((l) => l.stream === "browser").length).toBe(3);
    expect(s.output.some((l) => l.stream === "stderr")).toBe(true);
  });

  it("closes the live view when the browser closes", () => {
    expect(r.browser?.stepId).toBe("page_login");
    expect(r.browser?.active).toBe(false);
    const live = run(ev(wrongPassword).filter((e) => e.seq <= 10));
    expect(live.browser?.active).toBe(true);
    expect(live.browser?.cdpPort).toBeGreaterThan(0);
  });
});

describe("a demo recording", () => {
  const r = run(ev(connectWeather));

  it("nests a called flow's steps under the flow step", () => {
    expect(r.roots).toEqual(["sign_in", "record"]);
    expect(r.steps.sign_in!.children).toEqual(["sign_in/login"]);
    expect(r.steps["sign_in/login"]!.parent).toBe("sign_in");
    expect(topLevelOf(r, "sign_in/login")).toBe("sign_in");
  });

  it("collects beats and recordings", () => {
    expect(runBeats(r).map((b) => b.marker)).toEqual(["plants", "consent", "connected"]);
    const labels = r.steps.record!.artifacts.map((a) => a.label);
    expect(labels).toContain("Recording");
    expect(labels).toContain("Recording (popup)");
  });

  it("follows the active page across a popup", () => {
    const upToPopup = run(ev(connectWeather).filter((e) => e.seq <= 21));
    expect(upToPopup.browser?.url).toContain("/oauth/authorize");
    const back = run(ev(connectWeather).filter((e) => e.seq <= 23));
    expect(back.browser?.url).toMatch(/\/app$/);
  });
});

describe("a suite with parallel flows", () => {
  const r = run(ev(everything));

  it("tracks interleaved steps of parallel children", () => {
    expect(r.steps.checks!.children).toEqual(["api", "web"]);
    expect(r.steps.web!.children).toEqual(["web/sign_in", "web/open", "web/check"]);
    expect(topLevelOf(r, "web/sign_in/login")).toBe("checks");
    expect(Object.values(r.steps).every((s) => s.status === "passed")).toBe(true);
  });

  it("shows both branches running at once mid-run", () => {
    const mid = run(ev(everything).filter((e) => e.seq <= 14));
    expect(mid.steps.api!.status).toBe("running");
    expect(mid.steps["web/sign_in/login"]!.status).toBe("running");
    expect(mid.steps["api/sensor_online"]!.retry?.attempt).toBe(2);
    expect(mid.finished).toBeNull();
  });
});

describe("a cancelled run", () => {
  const r = run(ev(cancelled));

  it("marks steps that never ran as skipped and still runs cleanup", () => {
    expect(r.finished?.result).toBe("cancelled");
    expect(r.steps.listed!.status).toBe("skipped");
    expect(r.cleanups.add_plant).toBe("passed");
  });
});

describe("reports", () => {
  const events = ev(wrongPassword);
  const live = run(events);
  const steps = events
    .filter((e): e is Extract<RunEvent, { kind: "stepFinished" }> => e.kind === "stepFinished")
    .map((e) => e.step) as StepReport[];
  const report: RunReport = {
    schemaVersion: 1,
    runId: live.runId,
    flowId: "wrong-password",
    flowName: "Wrong password (fails on purpose)",
    flowPath: "flows/troubleshooting/wrong-password.lest.yaml",
    projectDir: "/work/lest/examples",
    environment: "local",
    startedAt: "2026-10-07T13:14:00.157Z",
    durationMs: 15838,
    result: "failed",
    warnings: ["step api_login failed (continueOnError)"],
    inputs: {},
    steps,
    cleanups: [],
  };

  it("builds a stored run from the report alone", () => {
    const stored = applyReport(emptyRun(report.runId), report);
    expect(stored.roots).toEqual(["api_login", "page_login"]);
    expect(stored.finished?.result).toBe("failed");
    expect(firstFailure(stored)?.id).toBe("page_login");
    // A stored run ignores late events.
    expect(applyEvent(stored, events[0]!)).toBe(stored);
  });

  it("keeps the live tree when the report arrives after the events", () => {
    const merged = applyReport(live, report);
    expect(merged.roots).toEqual(live.roots);
    expect(merged.report?.warnings?.length).toBe(1);
    expect(merged.steps.page_login!.output.length).toBe(live.steps.page_login!.output.length);
  });
});

describe("hydration during a gap", () => {
  it("keeps events newer than the snapshot", () => {
    const ev = (seq: number, body: Record<string, unknown>) => ({ runId: "r", seq, atMs: seq, ...body }) as never;
    const started = ev(0, {
      kind: "runStarted",
      flowId: "f",
      flowName: "F",
      environment: null,
      runDir: "/x",
      steps: [{ id: "a", name: "A", kind: "run" }],
      finally: [],
      tools: [],
    });
    // The stream delivers seq 2 before the snapshot (seq 0..1) arrives.
    let s = reduceRuns({ runs: {} }, { type: "event", event: ev(2, { kind: "stepStarted", stepId: "a", attempt: 1 }) });
    expect(s.runs.r!.gap).toBe(true);
    s = reduceRuns(s, { type: "events", runId: "r", events: [started, ev(1, { kind: "demoProgress", message: "x" })] });
    expect(s.runs.r!.lastSeq).toBe(2);
    expect(s.runs.r!.steps.a!.status).toBe("running");
  });
});
