// Text a person can paste into a terminal or hand to someone (or to their own
// agent) to pick up a failure: built from the run report, client-side.

import type { RunReport, StepReport } from "../types";
import { shellQuote } from "./format";

export interface RerunOptions {
  flowId: string;
  fromStep: string;
  runId?: string;
  environment?: string | null;
  inputs?: Record<string, string>;
}

/** `lest run <flow> --from <step>`, with the environment, inputs and the run
 * whose earlier results it reuses. */
export function rerunCommand(o: RerunOptions): string {
  const parts = ["lest", "run", shellQuote(o.flowId), "--from", shellQuote(o.fromStep)];
  if (o.runId) parts.push("--resume-run", shellQuote(o.runId));
  if (o.environment) parts.push("--env", shellQuote(o.environment));
  for (const [k, v] of Object.entries(o.inputs ?? {})) parts.push("--input", shellQuote(`${k}=${v}`));
  return parts.join(" ");
}

function lastLines(text: string | undefined, n: number): string[] {
  if (!text) return [];
  const lines = text.replace(/\n+$/, "").split("\n");
  return lines.slice(Math.max(0, lines.length - n));
}

function walk(steps: StepReport[], out: StepReport[] = []): StepReport[] {
  for (const s of steps) {
    out.push(s);
    walk(s.children ?? [], out);
  }
  return out;
}

export interface ContextInput {
  report: RunReport;
  step: StepReport;
  /** The top-level step containing `step` (what --from takes). */
  topLevel: string;
}

export function buildContext({ report, step, topLevel }: ContextInput): string {
  const lines: string[] = [];
  const add = (label: string, value: string | number | undefined | null) => {
    if (value === undefined || value === null || value === "") return;
    lines.push(`${label}: ${value}`);
  };
  lines.push(`Lest flow failure`);
  lines.push("");
  add("Flow", `${report.flowName} (${report.flowId})`);
  add("Flow file", `${report.projectDir}/${report.flowPath}`);
  add("Run", `${report.runId} (${report.result})`);
  add("Environment", report.environment);
  const inputs = Object.entries(report.inputs ?? {});
  if (inputs.length) add("Inputs", inputs.map(([k, v]) => `${k}=${v}`).join(", "));
  lines.push("");
  add("Failed step", `${step.id} "${step.name}"${step.kind ? ` (${step.kind})` : ""}`);
  add("Status", step.status);
  add("Resolved", step.resolved);
  add("Headline", step.headline);
  if (step.error && step.error !== step.headline) add("Error", step.error);
  add("Exit code", step.exitCode);
  add("HTTP status", step.httpStatus);
  add("Attempts", step.attempts);
  if (step.notes) {
    lines.push("Notes:");
    for (const l of step.notes.trim().split("\n")) lines.push(`  ${l}`);
  }
  for (const [name, text] of [
    ["stderr", step.stderr],
    ["stdout", step.stdout],
  ] as const) {
    const tail = lastLines(text, 40);
    if (!tail.length) continue;
    lines.push("");
    lines.push(`Last ${tail.length} lines of ${name}:`);
    for (const l of tail) lines.push(`  ${l}`);
  }
  const all = walk([...report.steps, ...(report.finally ?? [])]);
  const idx = all.findIndex((s) => s.id === step.id);
  const earlier = idx < 0 ? all : all.slice(0, idx);
  if (earlier.length) {
    lines.push("");
    lines.push("Earlier steps:");
    for (const s of earlier) lines.push(`  ${s.status ?? "pending"}  ${s.id}`);
  }
  lines.push("");
  lines.push("Rerun from the failed step:");
  lines.push(
    `  ${rerunCommand({
      flowId: report.flowId,
      fromStep: topLevel,
      runId: report.runId,
      environment: report.environment,
      inputs: report.inputs,
    })}`,
  );
  return lines.join("\n") + "\n";
}

export async function copyText(text: string): Promise<boolean> {
  try {
    await navigator.clipboard.writeText(text);
    return true;
  } catch {
    // Fallback for contexts without the async clipboard API.
    const ta = document.createElement("textarea");
    ta.value = text;
    ta.setAttribute("readonly", "");
    ta.style.position = "fixed";
    ta.style.opacity = "0";
    document.body.appendChild(ta);
    ta.select();
    const ok = document.execCommand("copy");
    ta.remove();
    return ok;
  }
}
