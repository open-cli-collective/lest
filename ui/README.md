# Lest UI

The web UI that `lest ui` serves. React and TypeScript, built with Vite into
`ui/dist`, which the `lest-server` crate embeds. What the screens do is in
[docs/ui.md](../docs/ui.md).

## Build

```bash
cd ui
npm ci
npm run build        # typecheck, then build ui/dist
```

Release builds of `lest` embed `ui/dist` at compile time, so build the UI
before `cargo build --release`. Debug builds read `ui/dist` from disk on each
request: rebuild the UI and reload the page, no Rust rebuild needed. Without a
built UI, the server embeds a placeholder page that says how to build it.
`ui/dist` is not committed.

## Develop with hot reload

Start `lest ui` against a project, then point the Vite dev server at it:

```bash
cd examples && lest ui --no-browser --port 4790
# prints: lest ui: http://127.0.0.1:4790/?token=<token>

cd ui
LEST_UI_URL='http://127.0.0.1:4790/?token=<token>' npm run dev
```

The dev server proxies `/api` to `LEST_UI_URL` and adds
`Authorization: Bearer <token>` (taken from `LEST_UI_TOKEN` if set, otherwise
from the URL). Open the address Vite prints. The live browser view does not
work through the dev server, because the browser accepts DevTools connections
only from the `lest ui` origin.

## Checks

```bash
npm run typecheck    # tsc --noEmit
npm test             # vitest: run state reducer, markdown, copy context
npm run build
```

The reducer tests replay real event streams captured from the example flows
(`src/test/fixtures/*.json`). To refresh them, run a flow while reading the
event stream and keep that run's events:

```bash
curl -sN -H "Authorization: Bearer <token>" http://127.0.0.1:4790/api/events
```

Each `data:` line is one event; a fixture is the JSON array of one run's
events in order. Replace machine-specific paths before committing.

## Screenshots

`npm run screenshots` drives the real UI with Playwright and writes the PNGs
in `docs/images/`. It needs `LEST_UI_URL` and a running `lest ui` on the
examples project; see [docs/ui.md](../docs/ui.md#updating-the-screenshots).

## Layout

| Path | Contents |
|---|---|
| `src/types.ts` | JSON shapes of the server API and run events. |
| `src/api.ts` | Fetch helpers for `/api`. |
| `src/store/runState.ts` | The pure reducer that folds events and reports into run state. |
| `src/store/store.ts` | The single SSE connection and the run store. |
| `src/components/RunView.tsx` | The run view: summary, setup, step tree, step bodies, side panel. |
| `src/components/LiveView.tsx` | The DevTools screencast view. |
| `src/pages/` | One file per screen. |
| `src/lib/` | Router, formatting, Markdown subset, copy context. |

Dependencies are kept to `react` and `react-dom`. No component or CSS
libraries; icons are inline SVG. Colors are CSS custom properties in
`src/styles.css`, redefined for the dark theme.
