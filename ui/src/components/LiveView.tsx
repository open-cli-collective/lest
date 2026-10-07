// Streams the running browser step's page through the DevTools screencast:
// the UI talks to Chromium directly, acknowledges every frame, and draws only
// the newest one, at most once per animation frame.

import { useEffect, useRef, useState } from "react";
import type { BrowserState } from "../store/runState";

type Phase = "connecting" | "live" | "idle" | "unavailable" | "ended";

const IDLE_AFTER_MS = 1500;
const MIN_FRAME_GAP_MS = 33;

export function LiveView({ browser, big = false }: { browser: BrowserState; big?: boolean }) {
  const img = useRef<HTMLImageElement>(null);
  const box = useRef<HTMLDivElement>(null);
  const [phase, setPhase] = useState<Phase>("connecting");
  const [hasFrame, setHasFrame] = useState(false);
  const phaseRef = useRef<Phase>("connecting");
  const { cdpPort, targetId, active } = browser;

  useEffect(() => {
    const set = (p: Phase) => {
      if (phaseRef.current !== p) {
        phaseRef.current = p;
        setPhase(p);
      }
    };
    if (!active) {
      set("ended");
      return;
    }
    set("connecting");
    let ws: WebSocket | null = null;
    let latest: string | null = null;
    let raf = 0;
    let lastDraw = 0;
    let lastFrameAt = 0;
    let closed = false;
    let gotFrame = false;
    const draw = (t: number) => {
      raf = 0;
      if (!latest) return;
      if (t - lastDraw < MIN_FRAME_GAP_MS) {
        raf = requestAnimationFrame(draw);
        return;
      }
      lastDraw = t;
      if (img.current) img.current.src = `data:image/jpeg;base64,${latest}`;
      latest = null;
      if (!gotFrame) {
        gotFrame = true;
        setHasFrame(true);
      }
    };
    const idleTimer = setInterval(() => {
      if (lastFrameAt && Date.now() - lastFrameAt > IDLE_AFTER_MS && phaseRef.current === "live") set("idle");
    }, 500);
    const connect = () => {
      try {
        ws = new WebSocket(`ws://127.0.0.1:${cdpPort}/devtools/page/${targetId}`);
      } catch {
        set("unavailable");
        return;
      }
      const sock = ws;
      sock.onopen = () => {
        const width = Math.round((box.current?.clientWidth || 360) * (window.devicePixelRatio || 1));
        sock.send(
          JSON.stringify({
            id: 1,
            method: "Page.startScreencast",
            params: { format: "jpeg", quality: 70, maxWidth: width, maxHeight: width, everyNthFrame: 1 },
          }),
        );
      };
      sock.onmessage = (m) => {
        let msg: { method?: string; params?: { data: string; sessionId: number } };
        try {
          msg = JSON.parse(typeof m.data === "string" ? m.data : "");
        } catch {
          return;
        }
        if (msg.method !== "Page.screencastFrame" || !msg.params) return;
        sock.send(JSON.stringify({ id: 2, method: "Page.screencastFrameAck", params: { sessionId: msg.params.sessionId } }));
        latest = msg.params.data;
        lastFrameAt = Date.now();
        set("live");
        if (!raf) raf = requestAnimationFrame(draw);
      };
      sock.onerror = () => {
        if (!closed && !gotFrame) set("unavailable");
      };
      sock.onclose = () => {
        if (!closed) set(gotFrame ? "ended" : "unavailable");
      };
    };
    // A short step can report its page and close within milliseconds; wait
    // briefly so a browser that is already gone is not dialed.
    const delay = setTimeout(connect, 120);
    return () => {
      closed = true;
      clearTimeout(delay);
      clearInterval(idleTimer);
      if (raf) cancelAnimationFrame(raf);
      try {
        ws?.close();
      } catch {
        /* already closed */
      }
    };
  }, [cdpPort, targetId, active]);

  const badge =
    phase === "live" ? (
      <span className="live-badge">
        <span className="d" />
        LIVE
      </span>
    ) : phase === "idle" ? (
      <span className="live-badge idle" title="The page has not changed for a moment">
        <span className="d" />
        IDLE
      </span>
    ) : null;

  const message =
    phase === "connecting" && !hasFrame
      ? "Connecting to the browser"
      : phase === "unavailable" && !hasFrame
        ? "Live view unavailable"
        : phase === "ended" && !hasFrame
          ? "The browser closed"
          : null;

  return (
    <div>
      <div ref={box} className={`live${big ? " big" : ""}${phase === "ended" ? " ended" : ""}`}>
        <img ref={img} alt="Live view of the browser step" hidden={!hasFrame} />
        {message && <div className="live-msg">{message}</div>}
      </div>
      <div className="live-url">
        {badge}
        {phase === "ended" && hasFrame && <span className="muted">Last frame</span>}
        <span title={browser.url}>{browser.url}</span>
      </div>
    </div>
  );
}
