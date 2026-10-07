// Captures the docs screenshots from the real UI against a running
// `lest ui` serving the examples project:
//
//   cd examples && lest ui --no-browser --port 4790
//   LEST_UI_URL='http://127.0.0.1:4790/?token=...' npm run screenshots
//
// Starts no server. Triggers runs through the API and waits for each state
// before capturing. Writes PNGs to docs/images/ at 2x.

import { mkdir } from "node:fs/promises";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { chromium } from "playwright";

const printed = process.env.LEST_UI_URL;
if (!printed) {
  console.error("Set LEST_UI_URL to the address `lest ui --no-browser` printed (with ?token=).");
  process.exit(2);
}
const url = new URL(printed);
const origin = url.origin;
const token = url.searchParams.get("token") ?? "";
const outDir = join(dirname(fileURLToPath(import.meta.url)), "..", "..", "docs", "images");
const VIEWPORT = { width: 1440, height: 900 };

async function api(path, init = {}) {
  const res = await fetch(`${origin}/api${path}`, {
    ...init,
    headers: { Authorization: `Bearer ${token}`, "content-type": "application/json", ...(init.headers ?? {}) },
  });
  if (!res.ok) throw new Error(`${init.method ?? "GET"} ${path}: ${res.status} ${await res.text()}`);
  return res.json();
}

const startRun = (flowId) =>
  api("/runs", { method: "POST", body: JSON.stringify({ flowId, liveView: true }) }).then((r) => r.runId);

async function waitDone(runId, timeoutMs = 180_000) {
  const end = Date.now() + timeoutMs;
  while (Date.now() < end) {
    const r = await api(`/runs/${runId}`);
    if (r.done && r.report) return r.report;
    await new Promise((res) => setTimeout(res, 400));
  }
  throw new Error(`run ${runId} did not finish`);
}

async function shot(page, name) {
  // Let transitions and lazy images settle.
  await page.waitForTimeout(400);
  await page.evaluate(() => Promise.all([...document.images].filter((i) => !i.complete).map((i) => new Promise((r) => { i.onload = i.onerror = r; }))));
  const path = join(outDir, name);
  await page.screenshot({ path });
  console.log(`wrote ${path}`);
}

async function open(context) {
  const page = await context.newPage();
  // Trades the token for the session cookie.
  await page.goto(printed);
  await page.waitForSelector(".rail");
  return page;
}

async function go(page, path) {
  await page.evaluate((p) => {
    history.pushState(null, "", p);
    dispatchEvent(new PopStateEvent("popstate"));
  }, path);
}

async function main() {
  await mkdir(outDir, { recursive: true });
  const browser = await chromium.launch();
  const light = await browser.newContext({ viewport: VIEWPORT, deviceScaleFactor: 2, colorScheme: "light" });
  const page = await open(light);

  console.log("running everything, plants-api and wrong-password");
  await waitDone(await startRun("everything"));
  const passed = await startRun("plants-api");
  await waitDone(passed);
  const failed = await startRun("wrong-password");
  await waitDone(failed);

  // A live run, captured mid-recording with a frame in the live view.
  console.log("recording connect-weather");
  await go(page, "/flows/connect-weather");
  await page.waitForSelector(".controls");
  const live = await startRun("connect-weather");
  await page.waitForSelector(`.summary .value.mono:has-text("${live}")`);
  await page.waitForSelector(".live img:not([hidden])", { timeout: 60_000 });
  await page.waitForSelector(".live-badge:not(.idle)", { timeout: 30_000 });
  // Wait for the consent popup or the dashboard to be on screen.
  await page.waitForFunction(() => document.querySelector(".live-url span[title]")?.textContent?.includes("oauth"), null, { timeout: 30_000 }).catch(() => {});
  await page.waitForTimeout(500);
  await shot(page, "flow-running-live.png");
  await waitDone(live);

  await go(page, "/flows");
  await page.waitForSelector(".card");
  await shot(page, "flows.png");

  await go(page, "/flows/plants-api");
  await page.waitForSelector(`.summary .value.mono:has-text("${passed}")`);
  await page.click('.step-row:has-text("Wait for the sensor to pair")');
  await page.click('.step.open [role="tab"]:has-text("Details")');
  await shot(page, "flow-passed.png");

  await go(page, "/flows/wrong-password");
  await page.waitForSelector(".explain");
  await page.click('.step.open [role="tab"]:has-text("Artifacts")');
  await page.waitForSelector(".step.open .thumb img");
  await shot(page, "flow-failed.png");

  await go(page, "/runs");
  await page.waitForSelector("table.runs tbody tr");
  await shot(page, "runs.png");

  await go(page, "/demos/connect-weather");
  await page.waitForSelector(".demo-video video");
  await page.waitForFunction(() => (document.querySelector(".demo-video video")?.readyState ?? 0) >= 2, null, { timeout: 15_000 }).catch(() => {});
  await page.evaluate(() => {
    const v = document.querySelector(".demo-video video");
    if (v && Number.isFinite(v.duration)) v.currentTime = Math.min(v.duration * 0.6, v.duration - 0.1);
  });
  await page.waitForTimeout(600);
  await shot(page, "demo.png");

  await go(page, "/settings");
  await page.waitForSelector(".seg");
  await page.waitForSelector(".kv");
  await shot(page, "settings.png");

  const dark = await browser.newContext({ viewport: VIEWPORT, deviceScaleFactor: 2, colorScheme: "dark" });
  const dpage = await open(dark);
  await go(dpage, "/flows/wrong-password");
  await dpage.waitForSelector(".explain");
  await dpage.click('.step.open [role="tab"]:has-text("Artifacts")');
  await dpage.waitForSelector(".step.open .thumb img");
  await shot(dpage, "flow-failed-dark.png");

  await browser.close();
}

main().catch((e) => {
  console.error(e);
  process.exit(1);
});
