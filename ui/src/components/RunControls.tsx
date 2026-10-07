import { useState } from "react";
import { api } from "../api";
import { isLive } from "../store/runState";
import { noteStarted, useRun } from "../store/store";
import type { FlowSummary, Scalar } from "../types";
import { IconPlay, IconRecord, IconStop } from "./icons";
import { Switch } from "./ui";

function defaultValue(v: Scalar | undefined): string {
  return v === undefined ? "" : String(v);
}

/** Environment, inputs, live view and Run/Cancel for one flow. */
export function RunControls({
  flow,
  runId,
  record = false,
  onStarted,
}: {
  flow: FlowSummary;
  /** The run shown under the controls; Cancel acts on it while it is live. */
  runId: string | null;
  record?: boolean;
  onStarted?: (runId: string) => void;
}) {
  const [env, setEnv] = useState(flow.defaultEnvironment ?? flow.environments[0] ?? "");
  const [inputs, setInputs] = useState<Record<string, string>>(() =>
    Object.fromEntries(flow.inputs.map((i) => [i.name, defaultValue(i.default)])),
  );
  const [liveView, setLiveView] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [starting, setStarting] = useState(false);
  const [cancelStage, setCancelStage] = useState<Record<string, "main" | "cleanup">>({});
  const { run } = useRun(runId);
  const live = run ? isLive(run) : false;
  const stage = runId ? cancelStage[runId] : undefined;

  const missing = flow.inputs.filter((i) => i.required && !inputs[i.name]);

  const start = async () => {
    setError(null);
    setStarting(true);
    try {
      const values = Object.fromEntries(Object.entries(inputs).filter(([, v]) => v !== ""));
      const r = await api.startRun({
        flowId: flow.id,
        environment: env || undefined,
        inputs: values,
        liveView: flow.usesBrowser ? liveView : false,
      });
      noteStarted(flow.id, r.runId);
      onStarted?.(r.runId);
    } catch (e) {
      setError((e as Error).message);
    } finally {
      setStarting(false);
    }
  };

  const cancel = async () => {
    if (!runId) return;
    const next = stage === "main" ? "cleanup" : "main";
    setCancelStage((c) => ({ ...c, [runId]: next }));
    try {
      await api.cancel(runId, next);
    } catch (e) {
      setError((e as Error).message);
    }
  };

  return (
    <form
      className="controls"
      onSubmit={(e) => {
        e.preventDefault();
        if (!live) void start();
      }}
    >
      {flow.environments.length > 0 && (
        <label className="field">
          <span className="field-label">Environment</span>
          <select className="select" value={env} onChange={(e) => setEnv(e.target.value)} disabled={live}>
            {flow.environments.map((e) => (
              <option key={e} value={e}>
                {e}
              </option>
            ))}
          </select>
        </label>
      )}
      {flow.inputs.map((input) => (
        <label className="field" key={input.name} title={input.description}>
          <span className="field-label">
            {input.label ?? input.name}
            {input.required && (
              <span className="req" aria-label="required">
                *
              </span>
            )}
          </span>
          {input.choices && input.choices.length > 0 ? (
            <select
              className="select"
              value={inputs[input.name] ?? ""}
              required={input.required}
              disabled={live}
              onChange={(e) => setInputs({ ...inputs, [input.name]: e.target.value })}
            >
              {!input.required && input.default === undefined && <option value="">(none)</option>}
              {input.choices.map((c) => (
                <option key={c.value} value={c.value} title={c.description}>
                  {c.label ?? c.value}
                </option>
              ))}
            </select>
          ) : (
            <input
              className="input"
              value={inputs[input.name] ?? ""}
              required={input.required}
              disabled={live}
              placeholder={input.description ?? ""}
              onChange={(e) => setInputs({ ...inputs, [input.name]: e.target.value })}
            />
          )}
        </label>
      ))}
      {flow.usesBrowser && (
        <Switch checked={liveView} onChange={setLiveView} label="Watch browser" />
      )}
      <div className="spacer" />
      {live && (
        <span className="controls-note" role="status">
          {stage === "main"
            ? "Stopping steps. Cleanup still runs."
            : stage === "cleanup"
              ? "Skipping cleanup."
              : null}
        </span>
      )}
      {live ? (
        <button type="button" className="btn btn-lg btn-danger" onClick={() => void cancel()} disabled={stage === "cleanup"}>
          <IconStop size={14} />
          {stage === "main" || stage === "cleanup" ? "Skip cleanup" : "Cancel"}
        </button>
      ) : (
        <button
          type="submit"
          className="btn btn-lg btn-primary"
          disabled={starting || missing.length > 0}
          title={missing.length ? `Fill in: ${missing.map((m) => m.label ?? m.name).join(", ")}` : undefined}
        >
          {record ? <IconRecord size={12} /> : <IconPlay size={13} />}
          {record ? "Record" : "Run"}
        </button>
      )}
      {error && (
        <div className="controls-error" role="alert">
          {error}
        </div>
      )}
    </form>
  );
}
