import { useEffect, useState } from "react";
import { api } from "../api";
import type { StateResponse } from "../types";

let cached: StateResponse | null = null;
let pending: Promise<StateResponse> | null = null;

/** The server's project and version, fetched once per page load. */
export function useProject(): StateResponse | null {
  const [state, setState] = useState<StateResponse | null>(cached);
  useEffect(() => {
    if (cached) return;
    pending ??= api.state();
    let alive = true;
    pending.then(
      (s) => {
        cached = s;
        if (alive) setState(s);
      },
      () => {
        pending = null;
      },
    );
    return () => {
      alive = false;
    };
  }, []);
  return state;
}

/** Runs an async loader and re-runs it when `deps` change. */
export function useLoad<T>(load: () => Promise<T>, deps: unknown[]): { data: T | null; error: string | null } {
  const [data, setData] = useState<T | null>(null);
  const [error, setError] = useState<string | null>(null);
  useEffect(() => {
    let alive = true;
    load().then(
      (d) => {
        if (!alive) return;
        setData(d);
        setError(null);
      },
      (e: Error) => alive && setError(e.message),
    );
    return () => {
      alive = false;
    };
  }, deps);
  return { data, error };
}

export type Theme = "system" | "light" | "dark";
const THEME_KEY = "lest.theme";

export function readTheme(): Theme {
  try {
    const t = localStorage.getItem(THEME_KEY);
    return t === "light" || t === "dark" ? t : "system";
  } catch {
    return "system";
  }
}

export function applyTheme(t: Theme) {
  const root = document.documentElement;
  if (t === "system") root.removeAttribute("data-theme");
  else root.setAttribute("data-theme", t);
}

export function saveTheme(t: Theme) {
  try {
    if (t === "system") localStorage.removeItem(THEME_KEY);
    else localStorage.setItem(THEME_KEY, t);
  } catch {
    /* storage unavailable: the choice lasts for this page only */
  }
  applyTheme(t);
}

export function readPref(key: string, fallback: boolean): boolean {
  try {
    const v = localStorage.getItem(key);
    return v === null ? fallback : v === "1";
  } catch {
    return fallback;
  }
}

export function writePref(key: string, v: boolean) {
  try {
    localStorage.setItem(key, v ? "1" : "0");
  } catch {
    /* ignore */
  }
}
