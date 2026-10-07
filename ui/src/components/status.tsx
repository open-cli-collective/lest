import type { RunResult, StepStatus } from "../types";

export type AnyStatus = StepStatus | RunResult | "none";

export function StatusDot({ status, label }: { status: AnyStatus; label?: string }) {
  return <span className={`dot dot-${status}`} role="img" aria-label={label ?? statusLabel(status)} />;
}

export function statusLabel(s: AnyStatus): string {
  switch (s) {
    case "none":
      return "never run";
    case "pending":
      return "not run";
    default:
      return s;
  }
}

export function ResultBadge({ status }: { status: AnyStatus }) {
  return (
    <span className={`badge badge-${status}`}>
      <StatusDot status={status} />
      {statusLabel(status)}
    </span>
  );
}

/** The step status glyph: a check, a cross, a bang, a dash, a spinner. */
export function StatusGlyph({ status }: { status: StepStatus }) {
  const common = {
    width: 16,
    height: 16,
    viewBox: "0 0 16 16",
    fill: "none",
    stroke: "currentColor",
    strokeWidth: 1.6,
    strokeLinecap: "round" as const,
    strokeLinejoin: "round" as const,
    "aria-hidden": true,
  };
  let body;
  switch (status) {
    case "passed":
      body = (
        <svg {...common}>
          <circle cx="8" cy="8" r="6.25" fill="currentColor" stroke="none" opacity="0.14" />
          <path d="m5.2 8.2 1.9 1.9 3.8-4.2" />
        </svg>
      );
      break;
    case "failed":
      body = (
        <svg {...common}>
          <circle cx="8" cy="8" r="6.25" fill="currentColor" stroke="none" />
          <path d="m5.8 5.8 4.4 4.4M10.2 5.8l-4.4 4.4" stroke="var(--surface)" />
        </svg>
      );
      break;
    case "errored":
      body = (
        <svg {...common}>
          <path d="M8 1.9 14.6 13.6H1.4z" fill="currentColor" stroke="currentColor" strokeWidth="1" />
          <path d="M8 6.2v3.4M8 11.6v.1" stroke="var(--surface)" />
        </svg>
      );
      break;
    case "skipped":
      body = (
        <svg {...common}>
          <circle cx="8" cy="8" r="6" strokeDasharray="2 2.2" />
          <path d="M5.5 8h5" />
        </svg>
      );
      break;
    case "running":
      body = (
        <svg {...common}>
          <circle cx="8" cy="8" r="6" opacity="0.2" />
          <path d="M8 2a6 6 0 0 1 6 6" className="spin" />
        </svg>
      );
      break;
    default:
      body = (
        <svg {...common}>
          <circle cx="8" cy="8" r="5.5" opacity="0.6" />
        </svg>
      );
  }
  return (
    <span className={`glyph glyph-${status}`} role="img" aria-label={statusLabel(status)}>
      {body}
    </span>
  );
}
