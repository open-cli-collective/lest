import { useEffect, useRef, useState, useSyncExternalStore, type ReactNode } from "react";
import { copyText } from "../lib/context";
import { IconCopy, IconMore, IconX } from "./icons";

/** Re-renders every `ms` while `active`; returns the current time. */
export function useNow(ms: number, active: boolean): number {
  const [now, setNow] = useState(() => Date.now());
  useEffect(() => {
    if (!active) return;
    setNow(Date.now());
    const t = setInterval(() => setNow(Date.now()), ms);
    return () => clearInterval(t);
  }, [ms, active]);
  return active ? now : Date.now();
}

// Toasts: one short message at a time.
let toastMsg: string | null = null;
let toastTimer: ReturnType<typeof setTimeout> | undefined;
const toastListeners = new Set<() => void>();
export function toast(msg: string) {
  toastMsg = msg;
  clearTimeout(toastTimer);
  toastTimer = setTimeout(() => {
    toastMsg = null;
    toastListeners.forEach((l) => l());
  }, 2200);
  toastListeners.forEach((l) => l());
}
export function Toaster() {
  const msg = useSyncExternalStore(
    (l) => {
      toastListeners.add(l);
      return () => toastListeners.delete(l);
    },
    () => toastMsg,
  );
  return msg ? (
    <div className="toast" role="status">
      {msg}
    </div>
  ) : null;
}

export async function copyWithToast(text: string, what: string) {
  const ok = await copyText(text);
  toast(ok ? `Copied ${what}` : `Could not copy ${what}`);
}

export function CopyButton({ text, label }: { text: string; label: string }) {
  return (
    <button type="button" className="copy-btn" title={`Copy ${label}`} onClick={() => void copyWithToast(text, label)}>
      <IconCopy size={13} />
      <span className="visually-hidden">Copy {label}</span>
    </button>
  );
}

export interface MenuItem {
  label: string;
  icon?: ReactNode;
  onSelect: () => void;
}

export function OverflowMenu({ items, label = "More actions" }: { items: MenuItem[]; label?: string }) {
  const [open, setOpen] = useState(false);
  const wrap = useRef<HTMLDivElement>(null);
  const button = useRef<HTMLButtonElement>(null);
  useEffect(() => {
    if (!open) return;
    const onDown = (e: MouseEvent) => {
      if (wrap.current && !wrap.current.contains(e.target as Node)) setOpen(false);
    };
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        setOpen(false);
        button.current?.focus();
      }
    };
    document.addEventListener("mousedown", onDown);
    document.addEventListener("keydown", onKey);
    wrap.current?.querySelector<HTMLButtonElement>(".menu button")?.focus();
    return () => {
      document.removeEventListener("mousedown", onDown);
      document.removeEventListener("keydown", onKey);
    };
  }, [open]);
  return (
    <div className="menu-wrap" ref={wrap}>
      <button
        ref={button}
        type="button"
        className="btn btn-sm btn-icon"
        aria-haspopup="menu"
        aria-expanded={open}
        title={label}
        onClick={() => setOpen((o) => !o)}
      >
        <IconMore />
        <span className="visually-hidden">{label}</span>
      </button>
      {open && (
        <div className="menu" role="menu">
          {items.map((it) => (
            <button
              key={it.label}
              type="button"
              role="menuitem"
              onClick={() => {
                setOpen(false);
                it.onSelect();
              }}
            >
              {it.icon}
              {it.label}
            </button>
          ))}
        </div>
      )}
    </div>
  );
}

export function Lightbox({ src, caption, onClose }: { src: string; caption: string; onClose: () => void }) {
  const closeRef = useRef<HTMLButtonElement>(null);
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => e.key === "Escape" && onClose();
    document.addEventListener("keydown", onKey);
    closeRef.current?.focus();
    return () => document.removeEventListener("keydown", onKey);
  }, [onClose]);
  return (
    <div className="lightbox" role="dialog" aria-modal="true" aria-label={caption} onClick={onClose}>
      <button ref={closeRef} type="button" className="btn btn-icon close" onClick={onClose}>
        <IconX />
        <span className="visually-hidden">Close</span>
      </button>
      <figure onClick={(e) => e.stopPropagation()}>
        <img src={src} alt={caption} />
        <figcaption>{caption}</figcaption>
      </figure>
    </div>
  );
}

export function Switch({
  checked,
  onChange,
  label,
}: {
  checked: boolean;
  onChange: (v: boolean) => void;
  label: string;
}) {
  return (
    <label className="switch">
      <input type="checkbox" role="switch" checked={checked} onChange={(e) => onChange(e.target.checked)} />
      <span className="switch-track" />
      {label}
    </label>
  );
}
