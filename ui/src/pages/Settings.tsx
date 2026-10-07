import { useState, type ReactNode } from "react";
import { IconDisplay, IconMoon, IconSun } from "../components/icons";
import { CopyButton } from "../components/ui";
import { readTheme, saveTheme, useProject, type Theme } from "../lib/project";

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
          <div className="sub">Saved in this browser.</div>
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
