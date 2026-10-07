# Demos

A demo is a flow that records a browser step. Lest films the take with a
visible cursor and paced typing, then cuts it into a video you can send:
waits removed, popups placed where they happened, and chapters from the
story you wrote down.

Because a demo is an ordinary flow, it is also a test. When the product
changes, the demo fails at the step that no longer works, instead of a
walkthrough silently going stale.

## A demo flow

```yaml
apiVersion: lest/v1
id: connect-weather
name: Connect the weather service
tags: [demo]
resources: [demo-account]        # two takes never share the account at once
demo:
  title: Water by the forecast
  summary: Connect SkyCast in two clicks and Sprout starts watering by the weather.
  beats:                         # the story, in order, keyed by the marker the take emits
    - { marker: plants, label: The dashboard shows every plant }
    - { marker: consent, label: SkyCast asks for permission }
    - { marker: connected, label: The forecast appears }
  cut:
    maxGap: 4s                   # stretches with nothing happening longer than this...
    keep: 1.5s                   # ...are shortened to this
steps:
  - id: sign_in
    flow: sign-in                # signs in off camera and saves the session
  - id: record
    browser:
      url: "${{ vars.base_url }}/app"
      session: sprout            # the take opens signed in
      record: true
      viewport: [1440, 900]
      script: connect-weather.mjs
```

```js
// connect-weather.mjs
export default async function ({ ui, beat, phase, popup }) {
  await phase("dashboard", async () => {
    await ui.waitFor("[data-testid=plant]");   // real data, not placeholders
    beat("plants");
    await ui.dwell(1200);                      // hold the shot
  });
  await phase("consent", async () => {
    const consent = await popup(() => ui.click("#connect"));
    beat("consent");
    await ui.click("#allow");                  // acts in the popup
    await consent.waitForEvent("close");
  });
  await phase("connected", async () => {
    await ui.expectText("#weather-text", "Rain expected");
    beat("connected");
    await ui.dwell(1500);
  });
}
```

Declarative steps work too: `beat:` is an action, and the validator checks
that every labeled beat is marked by some recorded step.

## What a take produces

After a recorded step passes, Lest builds the deliverable with ffmpeg and
attaches everything to the step and to the run report's `demo` section:

| File | What it is |
|---|---|
| `demo/demo.mp4` | The cut: H.264, 30 fps, playable anywhere. |
| `demo/chapters.vtt` | A WebVTT chapter track with one chapter per labeled beat. |
| `demo/beat-NN-<marker>.jpg` | A still of each labeled beat, taken just after it. |
| `demo/beat-sheet.jpg` | The stills side by side, for review at a glance. |
| `video/page@*.webm` | The raw recordings (`Recording`, and `Recording (popup)` per popup). |
| `demo/cut-command.txt` | The exact ffmpeg command, to reproduce or adjust the cut. |

How the cut is made:

- **Popups in place.** A popup's recording replaces the main video for the
  time it was open, cropped to the popup's window and centered over a dimmed
  still of the page that opened it.
- **Waits removed.** Every user action (click, fill, select, hover, press,
  goto) and every beat is a moment to keep. Any stretch longer than `maxGap`
  without one keeps only `keep` after the last moment and resumes just before
  the next, so loading and syncing disappear while every action stays.
- **Context and payoff.** The cut opens 1.5 seconds before the first moment
  and ends 2.5 seconds after the last.

Without ffmpeg, the raw recording is the deliverable and the run reports a
warning. `lest doctor` checks for it.

## Recording well

- **Sign in off camera.** Call a flow that signs in with `saveSession`, and
  record with `session`. The login, and any credentials, never appear.
- **Hold shots on purpose.** `ui.dwell(ms)` (or the `wait` action) after a
  beat gives the viewer time to read. The cut never removes time right after
  an action or beat.
- **Wait for the real content.** A screen that renders placeholders first
  satisfies a selector before the data arrives; wait for the data.
- **Secrets are filled, not typed.** A value that holds a secret is filled at
  once, so its keystrokes never appear.
- **Mark beats where the story moves.** Beats are the chapters; markers
  without a label are cut points only.
- **One take per account.** `resources:` makes a second take wait instead of
  fighting over a shared demo account.

Every label lives next to what it labels: beat labels in the flow, keyed by
the marker the take emits, so renaming a marker without its label is a
validation error rather than a chapter that silently disappears.
