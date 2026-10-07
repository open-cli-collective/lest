// A minimal history router: the server serves the app shell for every
// non-API path, so plain paths work on reload.

import { useSyncExternalStore, type MouseEvent } from "react";

const listeners = new Set<() => void>();

function notify() {
  for (const l of listeners) l();
}

if (typeof window !== "undefined") window.addEventListener("popstate", notify);

export function navigate(to: string, opts: { replace?: boolean } = {}) {
  if (to === location.pathname + location.search) return;
  if (opts.replace) history.replaceState(null, "", to);
  else history.pushState(null, "", to);
  window.scrollTo(0, 0);
  notify();
}

export function usePath(): string {
  return useSyncExternalStore(
    (l) => {
      listeners.add(l);
      return () => listeners.delete(l);
    },
    () => location.pathname,
  );
}

export type Route =
  | { name: "flows" }
  | { name: "flow"; id: string }
  | { name: "runs" }
  | { name: "run"; runId: string }
  | { name: "demos" }
  | { name: "demo"; id: string }
  | { name: "settings" }
  | { name: "notFound" };

export function matchRoute(path: string): Route {
  const parts = path.split("/").filter(Boolean).map(decodeURIComponent);
  const [a, b] = parts;
  if (parts.length === 0) return { name: "flows" };
  if (a === "flows") return b ? { name: "flow", id: b } : { name: "flows" };
  if (a === "runs") return b ? { name: "run", runId: b } : { name: "runs" };
  if (a === "demos") return b ? { name: "demo", id: b } : { name: "demos" };
  if (a === "settings" && !b) return { name: "settings" };
  return { name: "notFound" };
}

/** onClick for <a href> that routes in-app for plain left clicks. */
export function linkHandler(to: string) {
  return (e: MouseEvent) => {
    if (e.defaultPrevented || e.button !== 0 || e.metaKey || e.ctrlKey || e.shiftKey || e.altKey) return;
    e.preventDefault();
    navigate(to);
  };
}
