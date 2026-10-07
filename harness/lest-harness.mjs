// Lest browser harness. The runner starts this file with Node for each
// browser step attempt and writes the step's configuration (JSON) to stdin.
// Structured results go to an events file as JSON lines; stdout and stderr
// are only for humans.
//
// Playwright is loaded from the project (npm i -D playwright), so the
// project pins its own browser version.

import { createRequire } from "node:module";
import { appendFileSync, chmodSync, existsSync, mkdirSync, readFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { pathToFileURL } from "node:url";

const config = JSON.parse(readFileSync(0, "utf8"));
const t0 = Date.now();
mkdirSync(config.artifactsDir, { recursive: true });

function emit(event) {
  // Private to the user: events can carry values the runner redacts later.
  appendFileSync(config.eventsFile, JSON.stringify({ atMs: Date.now() - t0, ...event }) + "\n", { mode: 0o600 });
}
function log(msg) {
  process.stderr.write(`[browser] ${msg}\n`);
}

async function loadPlaywright() {
  const bases = [config.projectDir, config.flowDir, process.cwd()];
  if (process.env.LEST_PLAYWRIGHT) bases.unshift(dirname(process.env.LEST_PLAYWRIGHT));
  for (const base of bases) {
    try {
      const req = createRequire(join(base, "noop.js"));
      const mod = await import(pathToFileURL(req.resolve("playwright")).href);
      // Playwright is CommonJS; its exports may arrive under `default`.
      return mod.chromium ? mod : mod.default;
    } catch {}
  }
  log(`Playwright was not found from ${config.projectDir}.`);
  log("Install it in the project: npm i -D playwright && npx playwright install chromium");
  process.exit(3);
}

// The recorded cursor: an arrow drawn above the page that eases between
// targets and ripples on click. The real mouse follows it, so hover states
// show on camera.
const CURSOR_SCRIPT = `(() => {
  if (window.__lestCursor) return;
  const install = () => {
    if (!document.documentElement || document.getElementById('__lest_cursor')) return;
    const el = document.createElement('div');
    el.id = '__lest_cursor';
    // Start mid-screen so the first move reads as a move, not an entrance.
    const cx = window.__lestCursor.x < 0 ? Math.round(innerWidth / 2) : window.__lestCursor.x;
    const cy = window.__lestCursor.y < 0 ? Math.round(innerHeight / 2) : window.__lestCursor.y;
    window.__lestCursor.x = cx; window.__lestCursor.y = cy;
    el.style.cssText = 'position:fixed;left:0;top:0;width:26px;height:26px;z-index:2147483647;pointer-events:none;transform:translate(' + cx + 'px,' + cy + 'px);transition:none;';
    el.innerHTML = '<svg width="26" height="26" viewBox="0 0 26 26"><path d="M3 2 L3 21 L8.2 16.2 L11.6 24 L15 22.5 L11.7 14.9 L18.8 14.9 Z" fill="#111" stroke="#fff" stroke-width="1.6" stroke-linejoin="round"/></svg>';
    document.documentElement.appendChild(el);
  };
  window.__lestCursor = {
    x: -100, y: -100,
    install,
    move(x, y, ms) {
      install();
      const el = document.getElementById('__lest_cursor');
      const sx = this.x, sy = this.y, start = performance.now();
      const ease = (t) => t < 0.5 ? 4*t*t*t : 1 - Math.pow(-2*t + 2, 3) / 2;
      return new Promise((done) => {
        const step = (now) => {
          const t = Math.min(1, (now - start) / ms), k = ease(t);
          this.x = sx + (x - sx) * k; this.y = sy + (y - sy) * k;
          el.style.transform = 'translate(' + this.x + 'px,' + this.y + 'px)';
          t < 1 ? requestAnimationFrame(step) : done();
        };
        requestAnimationFrame(step);
      });
    },
    press() {
      install();
      const el = document.getElementById('__lest_cursor');
      el.style.transition = 'transform 90ms'; el.style.transform += ' scale(0.86)';
      const r = document.createElement('div');
      r.style.cssText = 'position:fixed;z-index:2147483646;pointer-events:none;width:34px;height:34px;border-radius:50%;border:2px solid rgba(59,130,246,.8);left:' + (this.x - 17) + 'px;top:' + (this.y - 17) + 'px;transform:scale(.3);opacity:1;transition:transform 420ms ease-out, opacity 420ms ease-out;';
      document.documentElement.appendChild(r);
      requestAnimationFrame(() => { r.style.transform = 'scale(1.4)'; r.style.opacity = '0'; });
      setTimeout(() => { r.remove(); el.style.transition = 'none'; el.style.transform = 'translate(' + this.x + 'px,' + this.y + 'px)'; }, 160);
    },
  };
  install();
  document.addEventListener('DOMContentLoaded', install);
})();`;

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

function makeUi(getPage, recording) {
  const pace = recording ? { move: 700, settle: 120, key: 65, hold: 400 } : null;
  // Recorded actions are moments the demo cut always keeps.
  const acted = (name) => recording && emit({ type: "action", name });
  const locate = (sel) => getPage().locator(sel).first();

  async function moveTo(sel) {
    const page = getPage();
    const loc = await visible(sel);
    if (!pace) return loc;
    await loc.scrollIntoViewIfNeeded();
    const box = await loc.boundingBox();
    if (box) {
      const x = box.x + box.width / 2;
      const y = box.y + box.height / 2;
      try {
        await page.evaluate(() => window.__lestCursor && window.__lestCursor.install());
        await Promise.all([
          page.evaluate(([x, y, ms]) => window.__lestCursor?.move(x, y, ms), [x, y, pace.move]),
          page.mouse.move(x, y, { steps: Math.max(1, Math.round(pace.move / 16)) }),
        ]);
      } catch (e) {
        log(`cursor: ${e.message}`);
      }
      await sleep(pace.settle);
    }
    return loc;
  }

  // Waits for a selector, failing with what the page showed instead.
  async function visible(sel, why = "to be visible") {
    const loc = locate(sel);
    try {
      await loc.waitFor({ state: "visible" });
      return loc;
    } catch (e) {
      if (!/Timeout/i.test(String(e?.message))) throw e;
      const page = getPage();
      const title = await page.title().catch(() => "");
      const alert = await page.locator('[role=alert], .error, .alert').first().innerText({ timeout: 500 }).catch(() => "");
      const shown = alert.trim() ? `; the page shows "${alert.trim().slice(0, 120)}"` : "";
      throw new Error(`expected ${sel} ${why}, but it never appeared on ${page.url()}${title ? ` ("${title}")` : ""}${shown}`);
    }
  }

  return {
    async goto(url) {
      acted("goto");
      await getPage().goto(url);
      if (pace) await sleep(pace.hold);
    },
    async click(sel) {
      acted("click");
      const loc = await moveTo(sel);
      if (pace) await getPage().evaluate(() => window.__lestCursor?.press()).catch(() => {});
      await loc.click();
      if (pace) await sleep(150);
    },
    async fill(sel, value, { secret = false } = {}) {
      acted("fill");
      const loc = await moveTo(sel);
      // Typing key by key reads well on camera but only suits text fields;
      // dates, colors and other inputs are filled directly.
      const typeable = await loc
        .evaluate((el) => el.isContentEditable || el.tagName === "TEXTAREA" ||
          (el.tagName === "INPUT" && ["", "text", "email", "password", "search", "url", "tel", "number"].includes((el.getAttribute("type") || "").toLowerCase())))
        .catch(() => false);
      if (!pace || secret || !typeable) {
        await loc.fill(value);
        return;
      }
      await getPage().evaluate(() => window.__lestCursor?.press()).catch(() => {});
      await loc.click();
      await loc.fill("");
      await loc.pressSequentially(value, { delay: pace.key });
      if ((await loc.inputValue().catch(() => value)) !== value) await loc.fill(value);
    },
    async select(sel, value) {
      acted("select");
      const loc = await moveTo(sel);
      await loc.selectOption(value);
    },
    async hover(sel) {
      acted("hover");
      await moveTo(sel);
      await locate(sel).hover();
    },
    async press(key) {
      acted("press");
      await getPage().keyboard.press(key);
      if (pace) await sleep(pace.settle);
    },
    async waitFor(sel) {
      await visible(sel);
    },
    async expectText(sel, text) {
      // One 15 second budget for appearing and containing the text.
      const deadline = Date.now() + 15000;
      const loc = await visible(sel, `to contain ${JSON.stringify(text)}`);
      let last = "";
      while (Date.now() < deadline) {
        last = (await loc.innerText().catch(() => "")) || "";
        if (last.includes(text)) return;
        await sleep(150);
      }
      throw new Error(`expected ${sel} to contain ${JSON.stringify(text)}, but it shows ${JSON.stringify(last.slice(0, 200))}`);
    },
    async expectUrl(pattern) {
      const re = new RegExp(pattern);
      await getPage().waitForURL((u) => re.test(u.toString()));
    },
    async read(sel) {
      const loc = await visible(sel);
      return (await loc.innerText()).trim();
    },
    async dwell(ms) {
      await sleep(ms);
    },
  };
}

async function targetId(context, page) {
  try {
    const cdp = await context.newCDPSession(page);
    const info = await cdp.send("Target.getTargetInfo");
    await cdp.detach().catch(() => {});
    return info.targetInfo.targetId;
  } catch {
    return null;
  }
}

async function main() {
  const { chromium } = await loadPlaywright();
  const args = [...(config.args || [])];
  if (config.cdpPort) {
    args.push(`--remote-debugging-port=${config.cdpPort}`);
    if (config.allowOrigin) args.push(`--remote-allow-origins=${config.allowOrigin}`);
  }
  const viewport = { width: config.viewport[0], height: config.viewport[1] };
  const browser = await chromium.launch({ headless: !config.headed, args });
  const contextOptions = { viewport };
  if (config.record) contextOptions.recordVideo = { dir: join(config.artifactsDir, "video"), size: viewport };
  if (config.sessionIn) {
    if (!existsSync(config.sessionIn)) {
      log(`no saved session '${config.sessionName}'; run the flow that saves it first`);
      await browser.close();
      process.exit(1);
    }
    contextOptions.storageState = config.sessionIn;
  }
  const context = await browser.newContext(contextOptions);
  if (config.record) await context.addInitScript(CURSOR_SCRIPT);
  context.setDefaultTimeout(config.actionTimeoutMs || 15000);

  const pages = new Map(); // page -> { role, openedAt, closedAt }
  let current = null;
  const track = async (page, role) => {
    const info = { role, openedAt: Date.now() - t0, closedAt: null, size: null };
    pages.set(page, info);
    // A popup's own size: its recording is drawn at the top left of a
    // frame the size of the main viewport.
    page.waitForLoadState("domcontentloaded").then(() => page.evaluate(() => [innerWidth, innerHeight])).then((s) => (info.size = s)).catch(() => {});
    current = page;
    const report = async () => {
      const id = await targetId(context, page);
      if (id) emit({ type: "page", targetId: id, url: page.url(), role });
    };
    page.on("framenavigated", (frame) => {
      if (frame === page.mainFrame()) report();
    });
    page.on("close", () => {
      const info = pages.get(page);
      if (info) info.closedAt = Date.now() - t0;
      if (current === page) {
        const open = [...pages.keys()].filter((p) => !p.isClosed());
        current = open[open.length - 1] || null;
        if (current) targetId(context, current).then((id) => id && emit({ type: "page", targetId: id, url: current.url(), role: pages.get(current)?.role }));
      }
    });
    page.on("requestfailed", (r) => {
      const err = r.failure()?.errorText || "";
      if (err.includes("LOCAL_NETWORK_ACCESS")) {
        log(`blocked: ${r.url()} (${err}). A public page cannot reach a private address; add --disable-features=LocalNetworkAccessChecks to the step's args if this is intended.`);
      }
    });
    await report();
  };
  context.on("page", (p) => {
    if (!pages.has(p)) track(p, pages.size === 0 ? "main" : "popup");
  });

  const mainPage = await context.newPage();
  if (!pages.has(mainPage)) await track(mainPage, "main");
  const getPage = () => current || mainPage;
  const ui = makeUi(getPage, config.record);
  const outputs = {};
  let failedPhase = null;

  const api = {
    page: mainPage,
    context,
    browser,
    ui,
    vars: config.vars,
    inputs: config.inputs,
    baseUrl: config.url || null,
    get current() {
      return getPage();
    },
    beat(marker) {
      emit({ type: "beat", marker: String(marker) });
    },
    output(name, value) {
      outputs[name] = value;
      emit({ type: "output", name, value });
    },
    async screenshot(name) {
      const file = join(config.artifactsDir, name.endsWith(".png") ? name : `${name}.png`);
      await getPage().screenshot({ path: file });
      emit({ type: "artifact", path: file, label: name });
      return file;
    },
    async phase(name, fn) {
      try {
        return await fn();
      } catch (e) {
        if (!failedPhase) {
          failedPhase = name;
          emit({ type: "phaseFailed", name, message: String(e?.message || e).replace(/\s+/g, " ").slice(0, 300) });
        }
        throw e;
      }
    },
    log,
    // Waits for a popup opened by `action` (for example a click), listening
    // before the action runs so the popup cannot be missed.
    async popup(action) {
      const [p] = await Promise.all([context.waitForEvent("page"), action()]);
      await p.waitForLoadState("domcontentloaded");
      if (!pages.has(p)) await track(p, "popup");
      current = p;
      return p;
    },
  };

  const resolveUrl = (u) => (config.url && !/^[a-z]+:/i.test(u) ? new URL(u, config.url).toString() : u);
  let ok = false;
  try {
    if (config.url) await ui.goto(config.url);
    for (const [i, action] of (config.actions || []).entries()) {
      const [kind, value] = Object.entries(action)[0];
      const label = `actions[${i}] ${kind}`;
      await api.phase(label, async () => {
        switch (kind) {
          case "goto": return ui.goto(resolveUrl(value));
          case "click": return ui.click(value);
          case "fill": return ui.fill(value.selector, value.value, { secret: !!value.secret });
          case "select": return ui.select(value.selector, value.value);
          case "press": return ui.press(value);
          case "hover": return ui.hover(value);
          case "waitFor": return ui.waitFor(value);
          case "expectText": return ui.expectText(value.selector, value.text);
          case "expectUrl": return ui.expectUrl(value);
          case "screenshot": return api.screenshot(value);
          case "beat": return api.beat(value);
          case "wait": return ui.dwell(value);
          case "read": return api.output(value.name, await ui.read(value.selector));
          default: throw new Error(`unknown action ${kind}`);
        }
      });
      log(`${label} ok`);
    }
    if (config.script) {
      const mod = await import(pathToFileURL(resolve(config.script)).href);
      if (typeof mod.default !== "function") throw new Error(`${config.script} must export a default async function`);
      await mod.default(api);
    }
    ok = true;
  } catch (e) {
    process.stderr.write(`${e?.stack || e}\n`);
    try {
      const file = join(config.artifactsDir, "failure.png");
      await getPage().screenshot({ path: file });
      emit({ type: "artifact", path: file, label: "Screenshot at failure" });
    } catch {}
    emit({ type: "error", message: String(e?.message || e).split("\n")[0].slice(0, 500), phase: failedPhase });
  }

  if (ok && config.sessionOut) {
    mkdirSync(dirname(config.sessionOut), { recursive: true });
    await context.storageState({ path: config.sessionOut });
    chmodSync(config.sessionOut, 0o600);
    emit({ type: "session", name: config.sessionName });
  }

  const videos = [];
  for (const [page, info] of pages) {
    const v = page.video();
    if (v) videos.push({ v, info });
  }
  await context.close();
  for (const { v, info } of videos) {
    try {
      const path = await v.path();
      emit({ type: "video", path, role: info.role, openedAtMs: info.openedAt, closedAtMs: info.closedAt, size: info.size });
    } catch (e) {
      log(`video: ${e.message}`);
    }
  }
  await browser.close();
  process.exit(ok ? 0 : 1);
}

main().catch((e) => {
  process.stderr.write(`${e?.stack || e}\n`);
  emit({ type: "error", message: String(e?.message || e).split("\n")[0] });
  process.exit(1);
});
