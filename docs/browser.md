# Browser steps

A `browser` step drives Chromium through [Playwright](https://playwright.dev).
Lest launches the browser itself, so recording, saved sessions, screenshots
on failure and the UI's live view work the same for every step.

## Setup

Browser steps need Node.js 18+ and Playwright installed in the project, so the
project pins its own browser version:

```bash
npm i -D playwright
npx playwright install chromium
```

`lest doctor` checks the Node version, Playwright in the project, and its
Chromium.

## Declarative actions

Most checks need no code:

```yaml
- id: login
  browser:
    url: "${{ vars.base_url }}/login"
    actions:
      - fill: { selector: "#email", value: demo@example.com }
      - fill: { selector: "#password", value: "${{ secrets.password }}" }
      - click: "button[type=submit]"
      - expectText: { selector: h1, text: Welcome }
      - read: { name: greeting, selector: h1 }   # becomes steps.login.outputs.greeting
      - screenshot: home
```

| Action | Does |
|---|---|
| `goto: <url>` | Navigates. Relative URLs resolve against the step's `url`. |
| `click: <selector>` | Clicks. |
| `fill: {selector, value}` | Replaces a field's content. |
| `select: {selector, value}` | Picks an option. |
| `press: <key>` | Presses a key (`Enter`, `Tab`, `Control+A`). |
| `hover: <selector>` | Hovers. |
| `waitFor: <selector>` | Waits until visible. |
| `expectText: {selector, text}` | Waits until the element contains the text. |
| `expectUrl: <regex>` | Waits until the URL matches (a JavaScript regular expression). |
| `read: {name, selector}` | Records the element's text as an output. |
| `screenshot: <name>` | Saves `<name>.png` as an artifact. |
| `beat: <marker>` | Marks a moment in a recording. See [demos](demos.md). |
| `wait: <duration>` | Pauses (a held shot when recording). |

Selectors use Playwright's syntax: CSS, `text=Sign in`,
`role=button[name="Save"]`, `[data-testid=plant]`. Prefer test ids and roles
over text, which changes with copy and language. Each action has 15 seconds
to succeed, and a failed wait says what the page showed instead:

```
expected h1 to contain "Welcome", but it never appeared on
http://127.0.0.1:4173/login ("Sign in"); the page shows "Wrong email or password"
```

Wait for the data you need, not for the page to load: a list that renders
placeholders first satisfies `waitFor: .list` before any data arrives, so
wait for a row instead.

## Scripts

For anything the actions cannot express, point `script` at a JavaScript
module. Its default export receives the page and helpers:

```yaml
- id: checkout
  browser:
    url: "${{ vars.base_url }}"
    script: checkout.mjs
```

```js
// checkout.mjs
export default async function ({ page, ui, beat, phase, output, screenshot, popup, vars }) {
  await phase("cart", async () => {
    await ui.click("[data-testid=add-to-cart]");
    await ui.expectText("#cart-count", "1");
  });
  const consent = await popup(() => ui.click("#pay-with-provider"));
  await ui.click("#approve");             // acts in the popup while it is open
  await consent.waitForEvent("close");
  output("order", await ui.read("#order-number"));
  await screenshot("confirmation");
}
```

| Helper | Does |
|---|---|
| `page`, `context`, `browser` | Playwright objects, for anything else. |
| `ui` | `click`, `fill`, `select`, `hover`, `press`, `goto`, `waitFor`, `expectText`, `expectUrl`, `read`, `dwell`. When recording, these move a visible cursor and type at a readable pace. They act on the current page: the popup while one is open. |
| `popup(action)` | Runs `action` (usually a click) and returns the popup it opens. The listener is armed before the action, so the popup cannot be missed. |
| `phase(name, fn)` | Names a part of the script; a failure inside reports the phase. |
| `beat(marker)` | Marks a moment in a recording. |
| `output(name, value)` | Sets `steps.<id>.outputs.<name>`. |
| `screenshot(name)` | Saves an artifact of the current page. |
| `vars`, `inputs` | The run's values. Secrets reach a script through the step's `env:` (`process.env`). |

Actions, when present, run before the script.

## Sessions

Sign in once and reuse the session, so later steps (and recordings) start
signed in and never show the login:

```yaml
- id: sign_in
  browser:
    url: "${{ vars.base_url }}/login"
    saveSession: app-user          # saved when the step passes
    actions: [...]
- id: dashboard
  browser:
    url: "${{ vars.base_url }}/app"
    session: app-user              # opens with the saved cookies and storage
```

Saved sessions grant access like a password. Lest keeps them in its data
directory, readable only by you, never in the project or in reports.

## Failure evidence

When a browser step fails, Lest saves a screenshot of the page at that moment
(`Screenshot at failure`) next to the step's other artifacts, and the step's
headline names the action, the expectation and what the page showed.

## Options

| Field | Meaning |
|---|---|
| `record` | Record the step and cut a demo video from it. See [demos](demos.md). |
| `viewport` | `[width, height]`; default `[1280, 800]`, or `[1920, 1080]` when recording. |
| `args` | Extra Chromium arguments for this step; `browser.args` in `lest.yaml` applies to every step. |
| `timeout` | The whole step's limit (default 120s). |

`lest run --headed` shows the browser window.

Headless Chromium blocks a public page from reaching a private address (a
hosted sign-in page redirecting to a dev server on your network). When that is
intended, add `--disable-features=LocalNetworkAccessChecks` to the step's
`args`; Lest logs a hint when it sees the error.
