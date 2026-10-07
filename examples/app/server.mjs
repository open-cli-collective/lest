// Sprout: a small plant-care web app used by Lest's examples and demos.
// No dependencies. Start with `node examples/app/server.mjs` (PORT, default 4173).
//
// Sign in with demo@example.com and the password in SPROUT_PASSWORD
// (default "sprout-demo"). State lives in memory and resets on restart.

import { createServer } from "node:http";
import { randomUUID } from "node:crypto";

const PORT = Number(process.env.PORT || 4173);
const PASSWORD = process.env.SPROUT_PASSWORD || "sprout-demo";
const USER = { id: "u-1", email: "demo@example.com", name: "Robin" };
// Slow the dashboard's data a little, the way a real backend would be.
const DATA_DELAY_MS = Number(process.env.SPROUT_DATA_DELAY_MS ?? 700);

const sessions = new Map(); // token -> { weather: bool }
const plants = new Map();
const statusPolls = new Map(); // plant id -> polls seen
let nextId = 1;
for (const [name, room, moisture] of [
  ["Monstera", "Living room", 62],
  ["Fiddle-leaf fig", "Hallway", 38],
  ["Snake plant", "Bedroom", 21],
]) {
  const id = `p-${nextId++}`;
  plants.set(id, { id, name, room, moisture });
}

const json = (res, status, body, headers = {}) => {
  res.writeHead(status, { "content-type": "application/json", ...headers });
  res.end(body === undefined ? "" : JSON.stringify(body));
};
const html = (res, body, status = 200) => {
  res.writeHead(status, { "content-type": "text/html; charset=utf-8" });
  res.end(body);
};
const readBody = (req) =>
  new Promise((resolve) => {
    let data = "";
    req.on("data", (c) => (data += c));
    req.on("end", () => {
      const type = req.headers["content-type"] || "";
      try {
        if (type.includes("json")) return resolve(JSON.parse(data || "{}"));
        if (type.includes("form")) return resolve(Object.fromEntries(new URLSearchParams(data)));
      } catch {
        return resolve(null);
      }
      resolve({ raw: data });
    });
  });
const cookieToken = (req) => /(?:^|;\s*)sprout=([^;]+)/.exec(req.headers.cookie || "")?.[1];
const bearerToken = (req) => /^Bearer (.+)$/.exec(req.headers.authorization || "")?.[1];
const session = (req) => {
  const t = cookieToken(req) || bearerToken(req);
  return t && sessions.has(t) ? { token: t, ...sessions.get(t) } : null;
};

const STYLE = `
  :root { --bg:#f6f7f2; --card:#fff; --ink:#1f2a1f; --muted:#667066; --line:#e3e6dc; --accent:#2f7a4f; --warn:#b7791f; }
  * { box-sizing: border-box; }
  body { margin:0; font: 15px/1.45 system-ui, -apple-system, "Segoe UI", sans-serif; background:var(--bg); color:var(--ink); }
  header { display:flex; align-items:center; gap:12px; padding:14px 28px; background:var(--card); border-bottom:1px solid var(--line); }
  header .brand { font-weight:700; font-size:18px; color:var(--accent); }
  header nav { margin-left:auto; display:flex; gap:16px; color:var(--muted); }
  main { max-width: 960px; margin: 32px auto; padding: 0 24px; }
  h1 { font-size: 26px; margin: 0 0 6px; }
  .muted { color: var(--muted); }
  .grid { display:grid; grid-template-columns: repeat(auto-fill, minmax(260px, 1fr)); gap:16px; margin-top:20px; }
  .card { background:var(--card); border:1px solid var(--line); border-radius:12px; padding:18px; }
  .card h3 { margin:0 0 4px; font-size:17px; }
  .bar { height:8px; border-radius:4px; background:#edf0e6; margin-top:12px; overflow:hidden; }
  .bar > span { display:block; height:100%; background:var(--accent); }
  .bar.low > span { background: var(--warn); }
  .skeleton { background: linear-gradient(90deg, #eef0ea 0%, #f7f8f4 50%, #eef0ea 100%); background-size: 200% 100%; animation: shimmer 1.2s infinite; border-radius: 8px; height: 104px; }
  @keyframes shimmer { from { background-position: 200% 0 } to { background-position: -200% 0 } }
  button, .button { font: inherit; border:0; border-radius:8px; padding:10px 16px; background:var(--accent); color:#fff; cursor:pointer; }
  button.secondary { background:#e9eee3; color:var(--ink); }
  form.login { max-width: 360px; margin: 80px auto; background: var(--card); border:1px solid var(--line); border-radius: 14px; padding: 28px; }
  label { display:block; font-weight:600; margin: 14px 0 6px; }
  input { width:100%; font:inherit; padding:10px 12px; border:1px solid var(--line); border-radius:8px; background:#fcfdfb; }
  .error { color:#b42318; margin-top:12px; min-height: 1.4em; }
  .weather { display:flex; align-items:center; justify-content:space-between; gap:16px; margin-top:24px; }
  .pill { display:inline-block; padding:2px 10px; border-radius:999px; background:#e9eee3; font-size:13px; }
  .pill.ok { background:#dff3e6; color:#1d5e38; }
`;

const page = (title, body, { nav = true } = {}) => `<!doctype html>
<html lang="en"><head><meta charset="utf-8"><title>${title}</title>
<meta name="viewport" content="width=device-width, initial-scale=1"><style>${STYLE}</style></head>
<body>${nav ? `<header><span class="brand">🌱 Sprout</span><nav><a href="/app">Plants</a><span>Robin</span></nav></header>` : ""}${body}</body></html>`;

const loginPage = () =>
  page(
    "Sign in · Sprout",
    `<form class="login" id="login">
      <div class="brand" style="font-size:22px;font-weight:700;color:var(--accent)">🌱 Sprout</div>
      <p class="muted">Sign in to see how your plants are doing.</p>
      <label for="email">Email</label><input id="email" name="email" type="email" autocomplete="username">
      <label for="password">Password</label><input id="password" name="password" type="password" autocomplete="current-password">
      <div class="error" id="error" role="alert"></div>
      <button type="submit" style="width:100%;margin-top:8px">Sign in</button>
    </form>
    <script>
      document.getElementById("login").addEventListener("submit", async (e) => {
        e.preventDefault();
        const r = await fetch("/api/login", { method: "POST", headers: { "content-type": "application/json" },
          body: JSON.stringify({ email: email.value, password: password.value }) });
        if (r.ok) location.href = "/app"; else document.getElementById("error").textContent = "Wrong email or password";
      });
    </script>`,
    { nav: false },
  );

const appPage = () =>
  page(
    "Plants · Sprout",
    `<main>
      <h1>Good morning, Robin</h1>
      <div class="muted" id="summary">Checking on your plants…</div>
      <div class="grid" id="plants"><div class="skeleton"></div><div class="skeleton"></div><div class="skeleton"></div></div>
      <div class="card weather" id="weather-card">
        <div><h3>Weather</h3><div class="muted" id="weather-text">Connect SkyCast to water by the forecast.</div></div>
        <div><span class="pill" id="weather-status">Not connected</span> <button id="connect">Connect SkyCast</button></div>
      </div>
    </main>
    <script>
      async function load() {
        const [plants, me] = await Promise.all([fetch("/api/plants").then(r => r.json()), fetch("/api/me").then(r => r.json())]);
        const thirsty = plants.items.filter(p => p.moisture < 30).length;
        document.getElementById("summary").textContent = plants.items.length + " plants, " + thirsty + (thirsty === 1 ? " needs" : " need") + " water";
        document.getElementById("plants").innerHTML = plants.items.map(p =>
          '<div class="card" data-testid="plant"><h3>' + p.name + '</h3><div class="muted">' + p.room + ' · ' + p.moisture + '% moisture</div>' +
          '<div class="bar' + (p.moisture < 30 ? ' low' : '') + '"><span style="width:' + p.moisture + '%"></span></div></div>').join("");
        if (me.weather) {
          document.getElementById("weather-status").textContent = "Connected";
          document.getElementById("weather-status").className = "pill ok";
          document.getElementById("weather-text").textContent = "Rain expected Thursday: skip watering the balcony plants.";
          document.getElementById("connect").style.display = "none";
        }
      }
      document.getElementById("connect").addEventListener("click", () => {
        window.open("/oauth/authorize?client=sprout", "skycast", "width=520,height=640");
      });
      window.addEventListener("message", (e) => { if (e.data === "skycast-connected") load(); });
      load();
    </script>`,
  );

const consentPage = () => `<!doctype html><html lang="en"><head><meta charset="utf-8"><title>SkyCast · Authorize</title>
<style>
  body { margin:0; font: 15px/1.45 system-ui, sans-serif; background: linear-gradient(160deg,#1e3a5f,#2c5d8a); color:#fff; min-height:100vh; display:grid; place-items:center; }
  .box { background:#fff; color:#1b2733; width: 400px; border-radius:16px; padding:28px; box-shadow: 0 20px 50px rgba(0,0,0,.25); }
  h1 { font-size:20px; margin:0 0 4px; } .muted { color:#5b6b7b; }
  ul { padding-left: 20px; } button { font:inherit; border:0; border-radius:8px; padding:10px 18px; cursor:pointer; }
  .allow { background:#2c6db3; color:#fff; } .deny { background:#e8edf2; color:#1b2733; margin-right:8px; }
</style></head><body>
<div class="box">
  <div style="font-weight:700;color:#2c6db3">☁ SkyCast</div>
  <h1>Sprout wants to use your forecast</h1>
  <p class="muted">Sprout will be able to:</p>
  <ul><li>Read the forecast for your saved location</li><li>See rain and frost alerts</li></ul>
  <form method="post" action="/oauth/approve" style="text-align:right;margin-top:24px">
    <button type="button" class="deny" onclick="window.close()">Cancel</button>
    <button type="submit" class="allow" id="allow">Allow</button>
  </form>
</div></body></html>`;

const server = createServer(async (req, res) => {
  const url = new URL(req.url, `http://${req.headers.host || "localhost"}`);
  const path = url.pathname;
  const s = session(req);

  if (path === "/health") return json(res, 200, { ok: true });
  if (path === "/") {
    res.writeHead(302, { location: s ? "/app" : "/login" });
    return res.end();
  }
  if (path === "/login" && req.method === "GET") return html(res, loginPage());
  if (path === "/api/login" && req.method === "POST") {
    const body = (await readBody(req)) || {};
    if (body.email !== USER.email || body.password !== PASSWORD) return json(res, 401, { error: "wrong email or password" });
    const token = randomUUID();
    sessions.set(token, { weather: false });
    return json(res, 200, { token, user: USER }, { "set-cookie": `sprout=${token}; Path=/; HttpOnly; SameSite=Lax` });
  }
  if (path === "/app") {
    if (!s) {
      res.writeHead(302, { location: "/login" });
      return res.end();
    }
    return html(res, appPage());
  }
  if (path === "/oauth/authorize") return html(res, consentPage());
  if (path === "/oauth/approve" && req.method === "POST") {
    if (s) sessions.set(s.token, { ...sessions.get(s.token), weather: true });
    return html(
      res,
      `<!doctype html><body style="font:15px system-ui;padding:40px">Connected. You can close this window.
      <script>window.opener && window.opener.postMessage("skycast-connected", "*"); setTimeout(() => window.close(), 600);</script></body>`,
    );
  }

  if (!path.startsWith("/api/")) return json(res, 404, { error: "not found" });
  if (!s) return json(res, 401, { error: "sign in first" });

  if (path === "/api/me") return json(res, 200, { ...USER, weather: !!s.weather });
  if (path === "/api/plants" && req.method === "GET") {
    await new Promise((r) => setTimeout(r, DATA_DELAY_MS));
    return json(res, 200, { items: [...plants.values()] });
  }
  if (path === "/api/plants" && req.method === "POST") {
    const body = (await readBody(req)) || {};
    if (!body.name) return json(res, 400, { error: "name is required" });
    const id = `p-${nextId++}`;
    const plant = { id, name: body.name, room: body.room || "Unassigned", moisture: Number(body.moisture ?? 50) };
    plants.set(id, plant);
    return json(res, 201, plant);
  }
  const m = /^\/api\/plants\/([^/]+)(\/status)?$/.exec(path);
  if (m) {
    const plant = plants.get(m[1]);
    if (!plant) return json(res, 404, { error: `no plant ${m[1]}` });
    if (m[2]) {
      // A new plant's sensor takes a few reads to report.
      const polls = (statusPolls.get(plant.id) || 0) + 1;
      statusPolls.set(plant.id, polls);
      return json(res, 200, { id: plant.id, sensor: polls >= 3 ? "online" : "pairing", polls });
    }
    if (req.method === "DELETE") {
      plants.delete(plant.id);
      return json(res, 204);
    }
    return json(res, 200, plant);
  }
  return json(res, 404, { error: "not found" });
});

server.listen(PORT, "127.0.0.1", () => console.log(`sprout listening on http://127.0.0.1:${PORT}`));
for (const sig of ["SIGINT", "SIGTERM"]) process.on(sig, () => server.close(() => process.exit(0)));
