import { describe, expect, it } from "vitest";
import { buildContext, rerunCommand } from "../lib/context";
import { formatDuration, shellQuote, timeAgo } from "../lib/format";
import { parseBlocks, parseInline, plainFirstLine } from "../lib/markdown";
import { tagFilters } from "../lib/tags";
import type { RunEvent, RunReport, StepReport } from "../types";
import wrongPassword from "./fixtures/wrong-password.json";

describe("markdown subset", () => {
  it("splits paragraphs and lists", () => {
    const blocks = parseBlocks("First line\ncontinues.\n\n- one\n- two\n  wrapped\n\n1. a\n2. b");
    expect(blocks).toEqual([
      { type: "p", text: "First line continues." },
      { type: "ul", items: ["one", "two wrapped"] },
      { type: "ol", items: ["a", "b"] },
    ]);
  });

  it("parses code, bold and safe links, and leaves the rest as text", () => {
    expect(parseInline("saves `sprout` **now** [docs](https://example.com)")).toEqual([
      { type: "text", text: "saves " },
      { type: "code", text: "sprout" },
      { type: "text", text: " " },
      { type: "strong", children: [{ type: "text", text: "now" }] },
      { type: "text", text: " " },
      { type: "link", href: "https://example.com", children: [{ type: "text", text: "docs" }] },
    ]);
    expect(parseInline("[x](javascript:alert(1)) <b>")).toEqual([
      { type: "text", text: "[x](javascript:alert(1)) <b>" },
    ]);
  });

  it("gives a plain first line for cards", () => {
    expect(plainFirstLine("Saves the session as `sprout`, so\nothers reuse it.\n\nMore.")).toBe(
      "Saves the session as sprout, so others reuse it.",
    );
  });
});

describe("format", () => {
  it("formats durations compactly", () => {
    expect(formatDuration(622)).toBe("622ms");
    expect(formatDuration(1718)).toBe("1.7s");
    expect(formatDuration(15_627)).toBe("16s");
    expect(formatDuration(125_000)).toBe("2m 5s");
  });
  it("formats ages", () => {
    expect(timeAgo(1_000_000 - 120_000, 1_000_000)).toBe("2 min ago");
  });
  it("quotes shell values only when needed", () => {
    expect(shellQuote("plants-api")).toBe("plants-api");
    expect(shellQuote("it's here")).toBe(`'it'\\''s here'`);
  });
});

describe("copy context", () => {
  const events = wrongPassword as unknown as RunEvent[];
  const steps = events
    .filter((e): e is Extract<RunEvent, { kind: "stepFinished" }> => e.kind === "stepFinished")
    .map((e) => e.step) as StepReport[];
  const report: RunReport = {
    schemaVersion: 1,
    runId: events[0]!.runId,
    flowId: "wrong-password",
    flowName: "Wrong password (fails on purpose)",
    flowPath: "flows/troubleshooting/wrong-password.lest.yaml",
    projectDir: "/work/lest/examples",
    environment: "local",
    startedAt: "2026-10-07T13:14:00.157Z",
    durationMs: 15838,
    result: "failed",
    inputs: { plan: "team plan" },
    steps,
  };
  const failed = steps.find((s) => s.id === "page_login")!;
  const text = buildContext({ report, step: failed, topLevel: "page_login" });

  it("names the flow file, run, failed step and headline", () => {
    expect(text).toContain("Flow file: /work/lest/examples/flows/troubleshooting/wrong-password.lest.yaml");
    expect(text).toContain(`Run: ${report.runId} (failed)`);
    expect(text).toContain('Failed step: page_login "Sign in on the login page" (browser)');
    expect(text).toContain("Headline: actions[3] expectText");
    expect(text).toContain("Exit code: 1");
    expect(text).toContain("Inputs: plan=team plan");
  });

  it("includes the stderr tail and earlier steps", () => {
    expect(text).toContain("lines of stderr:");
    expect(text).toContain("Earlier steps:\n  failed  api_login");
  });

  it("ends with a rerun command that reuses the run", () => {
    expect(text).toContain(
      `lest run wrong-password --from page_login --resume-run ${report.runId} --env local --input 'plan=team plan'`,
    );
    expect(rerunCommand({ flowId: "plants-api", fromStep: "sensor_online" })).toBe(
      "lest run plants-api --from sensor_online",
    );
  });
});

describe("tagFilters", () => {
  const flows = [["api", "smoke"], ["api", "slow"], ["api", "smoke", "ui"], ["zeta"]];
  it("orders by use, then name, and folds the rest", () => {
    expect(tagFilters(flows, [], false, 2)).toEqual({ shown: ["api", "smoke"], hidden: 3 });
  });
  it("keeps a selected tag visible when folded", () => {
    expect(tagFilters(flows, ["zeta"], false, 2)).toEqual({ shown: ["api", "smoke", "zeta"], hidden: 2 });
  });
  it("keeps a selected tag no flow has any more", () => {
    expect(tagFilters(flows, ["gone"], false, 2).shown).toEqual(["api", "smoke", "gone"]);
  });
  it("shows everything when expanded or under the limit", () => {
    expect(tagFilters(flows, [], true, 2).shown).toEqual(["api", "smoke", "slow", "ui", "zeta"]);
    expect(tagFilters(flows, [], false, 10).hidden).toBe(0);
  });
});
