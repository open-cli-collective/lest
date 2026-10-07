import { describe, expect, it } from "vitest";
import { agentConfigured, aiStatusLine, explainSlot, providerOptions } from "../lib/ai";
import type { AiConfig, AiStatus } from "../types";

const headline = 'Expected the dashboard, but the page shows "Wrong email or password".';

const off: AiStatus = { configured: "none", resolved: null, available: [], callsToday: 0, dailyLimit: 50 };
const on: AiStatus = {
  configured: "command",
  resolved: { kind: "command", label: "local-model", model: null },
  available: [],
  callsToday: 3,
  dailyLimit: 50,
};

describe("explanation slot", () => {
  it("shows today's card when AI is off", () => {
    expect(explainSlot(headline, { kind: "off" })).toEqual({ primary: headline, marker: "", headline: null, note: null });
  });

  it("keeps the headline while the explanation is pending", () => {
    expect(explainSlot(headline, { kind: "pending" })).toEqual({
      primary: headline,
      marker: "Explaining…",
      headline: null,
      note: null,
    });
  });

  it("puts the model's text first and moves the headline under it", () => {
    const slot = explainSlot(headline, {
      kind: "done",
      explanation: { text: " The password is wrong. \n", source: "model", provider: "local-model" },
    });
    expect(slot).toEqual({
      primary: "The password is wrong.",
      marker: "Written by local-model",
      headline,
      note: null,
    });
  });

  it("keeps the headline and shows the note on a fallback", () => {
    const slot = explainSlot(headline, {
      kind: "done",
      explanation: { text: headline, source: "lest", note: "daily limit of 50 model calls reached" },
    });
    expect(slot).toEqual({
      primary: headline,
      marker: "",
      headline: null,
      note: "AI unavailable: daily limit of 50 model calls reached",
    });
  });

  it("treats an empty model answer and a failed request as fallbacks", () => {
    expect(explainSlot(headline, { kind: "done", explanation: { text: "  ", source: "model", provider: "x" } }).primary).toBe(
      headline,
    );
    expect(explainSlot(headline, { kind: "error", message: "500 Internal Server Error" })).toEqual({
      primary: headline,
      marker: "",
      headline: null,
      note: "AI unavailable: 500 Internal Server Error",
    });
  });
});

describe("AI settings", () => {
  const cfg = (provider: string, extra: Partial<AiConfig> = {}): AiConfig => ({ provider, ...extra });

  it("describes each state in one line", () => {
    expect(aiStatusLine(cfg("none"), off)).toBe("AI is off. Failure explanations come from the step's own output.");
    expect(aiStatusLine(cfg("auto"), { ...off, configured: "auto" })).toBe("No agent CLI found on PATH; AI stays off.");
    expect(aiStatusLine(cfg("command"), { ...off, configured: "command" })).toBe(
      "Enter a command; AI stays off until then.",
    );
    expect(aiStatusLine(cfg("command", { command: "/x/local-model" }), on)).toBe(
      "Using local-model. 3 of 50 model calls today.",
    );
    expect(
      aiStatusLine(cfg("command"), { ...on, resolved: { kind: "command", label: "local-model", model: "small" } }),
    ).toBe("Using local-model, model small. 3 of 50 model calls today.");
  });

  it("lists the detected CLIs between Automatic and Custom command", () => {
    const labels = (ai: AiConfig, st: AiStatus) => providerOptions(ai, st).map((o) => o.label);
    expect(labels(cfg("none"), off)).toEqual(["Off", "Automatic", "Custom command"]);
    expect(labels(cfg("none"), { ...off, available: ["agent-a", "agent-b"] })).toEqual([
      "Off",
      "Automatic",
      "agent-a",
      "agent-b",
      "Custom command",
    ]);
    // A configured CLI that is no longer on PATH stays visible.
    expect(labels(cfg("agent-b"), { ...off, available: ["agent-a"] })).toEqual([
      "Off",
      "Automatic",
      "agent-a",
      "agent-b",
      "Custom command",
    ]);
  });

  it("offers the agent items only when an agent or provider is configured", () => {
    expect(agentConfigured(off, null)).toBe(false);
    expect(agentConfigured(null, undefined)).toBe(false);
    expect(agentConfigured(on, null)).toBe(false);
    expect(agentConfigured(off, "cd '/p' && agent 'go'")).toBe(true);
  });
});
