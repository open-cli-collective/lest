// Calls to the Lest server. The page is served same-origin with the auth
// cookie already set, so plain fetch works.

import type { FlowDetail, FlowsResponse, RunResponse, RunsResponse, StateResponse } from "./types";

export class ApiError extends Error {
  constructor(
    public status: number,
    message: string,
  ) {
    super(message);
  }
}

async function call<T>(path: string, init?: RequestInit): Promise<T> {
  const res = await fetch(`/api${path}`, { credentials: "same-origin", ...init });
  const text = await res.text();
  let body: unknown = null;
  try {
    body = text ? JSON.parse(text) : null;
  } catch {
    body = null;
  }
  if (!res.ok) {
    const msg =
      body && typeof body === "object" && "error" in body ? String((body as { error: unknown }).error) : text;
    throw new ApiError(res.status, msg || `${res.status} ${res.statusText}`);
  }
  return body as T;
}

function post<T>(path: string, body?: unknown): Promise<T> {
  return call<T>(path, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: body === undefined ? "{}" : JSON.stringify(body),
  });
}

export interface StartRunRequest {
  flowId: string;
  environment?: string;
  inputs?: Record<string, string>;
  liveView?: boolean;
  fromStep?: string;
  resumeRunId?: string;
}

export interface LoginResponse {
  launched: boolean;
  how?: string;
  error?: string;
  command: string;
}

export const api = {
  state: () => call<StateResponse>("/state"),
  flows: () => call<FlowsResponse>("/flows"),
  flow: (id: string) => call<FlowDetail>(`/flows/${encodeURIComponent(id)}`),
  runs: (flow?: string, limit = 200) => {
    const q = new URLSearchParams({ limit: String(limit) });
    if (flow) q.set("flow", flow);
    return call<RunsResponse>(`/runs?${q}`);
  },
  run: (id: string) => call<RunResponse>(`/runs/${encodeURIComponent(id)}`),
  startRun: (req: StartRunRequest) => post<{ runId: string }>("/runs", req),
  cancel: (id: string, stage: "main" | "cleanup") =>
    post<{ cancelled: string }>(`/runs/${encodeURIComponent(id)}/cancel`, { stage }),
  reveal: (id: string, path?: string) => post<{ revealed: string }>(`/runs/${encodeURIComponent(id)}/reveal`, { path }),
  toolLogin: (name: string) => post<LoginResponse>(`/tools/${encodeURIComponent(name)}/login`),
};

export function fileUrl(runId: string, path: string): string {
  const parts = path.split("/").map(encodeURIComponent).join("/");
  return `/api/runs/${encodeURIComponent(runId)}/files/${parts}`;
}
