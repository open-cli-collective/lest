import { useEffect, useRef, useState, type ReactNode } from "react";
import { IconDisplay, IconMoon, IconSun } from "../components/icons";
import { CopyButton } from "../components/ui";
import { aiStatusLine, providerOptions, saveAi, useAiSettings } from "../lib/ai";
import { readTheme, saveTheme, useProject, type Theme } from "../lib/project";
import type { AiConfig, SettingsResponse } from "../types";

const THEMES: { id: Theme; label: string; icon: ReactNode }[] = [
  { id: "system", label: "System", icon: <IconDisplay size={14} /> },
  { id: "light", label: "Light", icon: <IconSun size={14} /> },
  { id: "dark", label: "Dark", icon: <IconMoon size={14} /> },
];

export function SettingsPage() {
  const [theme, setTheme] = useState<Theme>(readTheme);
  const state = useProject();
  return (
    <div className="page">
      <div className="page-head">
        <div>
          <h1>Settings</h1>
          <div className="sub">Appearance is saved in this browser; AI in your Lest settings file.</div>
        </div>
      </div>
      <div className="settings">
        <section className="panel" aria-labelledby="appearance">
          <div className="panel-head" id="appearance">
            Appearance
          </div>
          <div className="settings-row">
            <div>
              <div className="label">Theme</div>
              <div className="hint">System follows your operating system's light or dark setting.</div>
            </div>
            <div className="seg" role="radiogroup" aria-label="Theme">
              {THEMES.map((t) => (
                <button
                  key={t.id}
                  type="button"
                  role="radio"
                  aria-checked={theme === t.id}
                  onClick={() => {
                    setTheme(t.id);
                    saveTheme(t.id);
                  }}
                >
                  {t.icon}
                  {t.label}
                </button>
              ))}
            </div>
          </div>
        </section>
        <AiSection />
        <section className="panel" aria-labelledby="project">
          <div className="panel-head" id="project">
            Project
          </div>
          {state ? (
            <div className="kv">
              <div className="k">Name</div>
              <div className="v">{state.project.name}</div>
              <div className="k">Root</div>
              <div className="v mono">
                {state.project.root} <CopyButton text={state.project.root} label="project root" />
              </div>
              <div className="k">Config</div>
              <div className="v">{state.project.hasConfig ? <span className="mono">lest.yaml</span> : "No lest.yaml; defaults apply"}</div>
              <div className="k">Lest version</div>
              <div className="v mono">{state.version}</div>
            </div>
          ) : (
            <div className="panel-empty">Loading</div>
          )}
        </section>
      </div>
    </div>
  );
}

const TEXT_SAVE_DELAY_MS = 600;

function AiSection() {
  const settings = useAiSettings();
  return (
    <section className="panel" aria-labelledby="ai">
      <div className="panel-head" id="ai">
        AI
      </div>
      {settings ? <AiForm settings={settings} /> : <div className="panel-empty">Loading</div>}
    </section>
  );
}

function AiForm({ settings }: { settings: SettingsResponse }) {
  // The form edits a local copy; every change is saved, text fields after a pause.
  const [ai, setAi] = useState<AiConfig>(settings.ai);
  const [limitText, setLimitText] = useState(settings.ai.dailyLimit ? String(settings.ai.dailyLimit) : "");
  const [saved, setSaved] = useState<"" | "saving" | "saved">("");
  const [saveError, setSaveError] = useState<string | null>(null);
  const timer = useRef<ReturnType<typeof setTimeout> | undefined>(undefined);
  const pending = useRef<AiConfig | null>(null);
  const latest = useRef(ai);
  latest.current = ai;

  const save = async (next: AiConfig) => {
    pending.current = null;
    setSaved("saving");
    try {
      await saveAi(clean(next));
      setSaveError(null);
      if (latest.current === next) setSaved("saved");
    } catch (e) {
      setSaved("");
      setSaveError(`Not saved: ${(e as Error).message}`);
    }
  };
  const change = (patch: Partial<AiConfig>, debounce: boolean) => {
    const next = { ...latest.current, ...patch };
    setAi(next);
    clearTimeout(timer.current);
    if (debounce) {
      pending.current = next;
      timer.current = setTimeout(() => void save(next), TEXT_SAVE_DELAY_MS);
    } else void save(next);
  };
  // Navigating to another page saves an edit still waiting for its pause.
  useEffect(
    () => () => {
      clearTimeout(timer.current);
      if (pending.current) void saveAi(clean(pending.current)).catch(() => {});
    },
    [],
  );
  useEffect(() => {
    if (saved !== "saved") return;
    const t = setTimeout(() => setSaved(""), 2000);
    return () => clearTimeout(t);
  }, [saved]);

  const status = settings.status;
  const options = providerOptions(ai, status);
  return (
    <>
      <div className="settings-row">
        <div>
          <div className="label">Provider</div>
          <div className="hint">
            Explains failed steps in plain words and can open your own agent CLI in the project with the failure's
            context. Nothing is sent anywhere unless you choose a provider here.
          </div>
        </div>
        <div className="seg" role="radiogroup" aria-label="AI provider">
          {options.map((o) => (
            <button
              key={o.id}
              type="button"
              role="radio"
              aria-checked={ai.provider === o.id}
              onClick={() => change({ provider: o.id }, false)}
            >
              {o.label}
            </button>
          ))}
        </div>
      </div>
      {ai.provider === "command" && (
        <div className="settings-row">
          <div>
            <label className="label" htmlFor="ai-command">
              Command
            </label>
            <div className="hint">Reads the prompt on stdin and prints text.</div>
          </div>
          <input
            id="ai-command"
            className="input mono"
            value={ai.command ?? ""}
            placeholder="/path/to/program --flag"
            spellCheck={false}
            onChange={(e) => change({ command: e.target.value }, true)}
          />
        </div>
      )}
      <div className="settings-row">
        <div>
          <label className="label" htmlFor="ai-model">
            Model
          </label>
          <div className="hint">Passed to the agent CLI; a custom command does not receive it.</div>
        </div>
        <input
          id="ai-model"
          className="input"
          value={ai.model ?? ""}
          placeholder="the provider's small default"
          spellCheck={false}
          onChange={(e) => change({ model: e.target.value }, true)}
        />
      </div>
      <div className="settings-row">
        <div>
          <label className="label" htmlFor="ai-agent">
            Agent command
          </label>
          <div className="hint">
            What Open in agent runs in a terminal, in the project; <code>{"{prompt}"}</code> becomes the task. Empty:
            the detected agent CLI (a custom command provider has none).
          </div>
        </div>
        <input
          id="ai-agent"
          className="input mono"
          value={ai.agent ?? ""}
          placeholder="my-agent {prompt}"
          spellCheck={false}
          onChange={(e) => change({ agent: e.target.value }, true)}
        />
      </div>
      <div className="settings-row">
        <div>
          <label className="label" htmlFor="ai-limit">
            Daily limit
          </label>
          <div className="hint">Model calls allowed per day. Each failed step of a run is explained once and then cached.</div>
        </div>
        <input
          id="ai-limit"
          className="input"
          type="number"
          min={1}
          inputMode="numeric"
          value={limitText}
          placeholder="50"
          onChange={(e) => {
            setLimitText(e.target.value);
            change({ dailyLimit: parseLimit(e.target.value) }, true);
          }}
        />
      </div>
      <div className="settings-status" role="status">
        <span className="msg">{saveError ?? aiStatusLine(settings.ai, status)}</span>
        <span className="saved">{saved === "saved" ? "Saved" : ""}</span>
      </div>
    </>
  );
}

function parseLimit(text: string): number | undefined {
  const n = Number(text.trim());
  return Number.isInteger(n) && n > 0 ? n : undefined;
}

/** Drops empty optional fields so the settings file keeps only real values. */
function clean(ai: AiConfig): AiConfig {
  const out: AiConfig = { provider: ai.provider };
  if (ai.agent?.trim()) out.agent = ai.agent.trim();
  if (ai.model?.trim()) out.model = ai.model.trim();
  if (ai.command?.trim()) out.command = ai.command.trim();
  if (ai.dailyLimit) out.dailyLimit = ai.dailyLimit;
  return out;
}
