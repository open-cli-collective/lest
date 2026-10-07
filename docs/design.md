# Lest design

Lest runs declarative end-to-end flows against real systems and records
product demos from the same flows. The name is the conjunction: you run a
flow *lest* a release breaks the path a user depends on, and you record a
demo *lest* the walkthrough drifts from what the product does.

This document records the decisions behind Lest's shape and the reasons for
each. Behavior is specified in the reference docs (start with
[flow-format.md](flow-format.md)); this file explains why.

## Principles

1. **A run is deterministic and reproducible.** Nothing a model says can
   change a run's outcome. AI sits around runs, never inside them.
2. **One run model.** A suite is a flow whose steps call other flows. A demo
   is a flow that records a browser step. Every surface (CLI, web UI,
   UI, notifications) consumes one event stream and one report format.
3. **The core owns everything; the shells are thin.** The CLI and the UI
   call the same library. No behavior exists only in one shell.
4. **Values are data, never source text.** Template values reach a shell
   through environment variables, not by splicing text into a script.
   Expressions are typed and evaluated over values.
5. **Flow speed is a correctness signal.** A slow flow either was not tested
   step by step or found a real problem. Lest reports durations prominently
   and its docs tell authors not to widen waits to hide slowness.
6. **A failing check that caught a real problem is the result.** Lest gives
   authors no mechanism to mark a failed step as passed after the fact.
7. **Company-neutral.** No vendor names in the file format. Tool profiles,
   secret backends, AI providers and notification targets are data the user
   supplies.

## Architecture

```
crates/lest-core     flow spec, expressions, validation, runner, reports,
                     store, secrets, tool profiles, browser bridge, media,
                     AI provider, notifications
crates/lest-server   local HTTP API + server-sent events + embedded UI
crates/lest          the `lest` binary: CLI and `lest ui`
ui/                  React + TypeScript UI (served by lest-server)
harness/             the Node browser harness the runner launches
examples/            a self-contained sample app and flows exercising every feature
```

### Server-first UI

The UI is a web app served by `lest ui` on `127.0.0.1` and talks to the core
over HTTP and server-sent events. `lest ui --app` opens it as its own window
(a Chromium-family browser in app mode, with a separate profile).

Why not a desktop webview with IPC:

- **One transport.** IPC commands plus hand-written TypeScript mirrors drift
  from the Rust types. One small HTTP API serves the UI, scripts and tests.
- **The real app is testable.** Playwright can drive the actual UI in CI on
  Linux (WebKit webviews expose no automation protocol), so screenshots in
  docs and PRs come from the shipped UI, not from a separate fixture harness.
- **Files are URLs.** Artifacts, screenshots and videos are served with range
  requests instead of base64 strings over IPC, so a 200 MB recording plays
  without loading into memory.
- **No webview to build.** One binary with the UI embedded, on every
  platform, with no webview libraries; CI builds and tests it on Linux. A
  native shell can wrap the same URL later if a window manager integration
  ever needs it; `--app` covers a standalone window today.

The server binds loopback only, requires a per-launch random token (passed in
the URL once, then held in an HttpOnly SameSite=Strict cookie named for the
port), checks the `Host` header against loopback names to block DNS
rebinding, accepts changes only from its own origin, serves run files in a
sandbox, and sets a strict Content-Security-Policy. Holding the token is
equivalent to running commands as the user ([ai.md](ai.md), Trust).

### Runner

The runner is async (tokio). Each step type is an executor behind one trait.
A process supervisor owns child processes: concurrent draining of stdout and
stderr with a per-stream cap that keeps the head, a process group per step,
and group-kill on timeout or cancel. Every run path ends with a terminal
`run_finished` event and a report on disk, including preflight failures.

Cleanup runs on cancel. The first Ctrl-C (or Cancel in the UI) stops the main
steps and runs cleanups with their own timeouts; a second one abandons
cleanup. A failed cleanup or a failed step marked `continueOnError` adds a
warning to the report, which every surface shows next to the result, instead
of vanishing.

Every event carries `run_id`, so the UI can hold several concurrent runs and
switching views never paints one run's events into another.

### Flow format

YAML with `apiVersion: lest/v1`. The JSON Schema is generated from the Rust
types, so the schema, the validator and the docs cannot disagree. Step objects
reject unknown keys.

- **Step types:** `run` (a process), `http`, `assert`, `browser`, `flow`
  (call another flow), `group` (sequential or `parallel`).
- **Expressions:** one language everywhere, CEL. `when:`, `until:` and
  `assert:` are CEL; `${{ expr }}` interpolates a CEL value into a string
  field. CEL is typed, side-effect free, has numbers, comparisons, negation,
  parentheses, `size()`, `contains()` and `matches()`, and has a specification
  outside this project.
- **Shell safety:** `run:` scripts cannot contain `${{ }}`. Values reach a
  script through `env:` (whose values may be templates) and through `vars`,
  which are exported automatically. Secrets reach a script only through
  `env:`, so they never appear on a command line.
- **Outputs** are CEL expressions over the step's result: `self.json.user.id`
  reads stdout parsed as JSON, `self.stdout.capture(r'id=(\w+)')` takes a
  regex group, `self.body.id` reads an HTTP response. No second extraction
  mini-language.
- **Step ids** are identifiers (`[a-z][a-z0-9_]*`) so `steps.sign_in.outputs`
  is a plain CEL path.
- **Composition:** a `flow` step calls another flow with typed `with:` inputs
  and exposes the callee's declared `outputs`. This one mechanism replaces
  include-fragments and suites. A group with `parallel: true` runs its
  children concurrently up to `maxParallel`.
- **Cleanup:** `cleanup:` on a step registers a command, resolved at run time
  and recorded in the report, that runs after the flow in reverse order. With
  `policy: manual` it is recorded but not run, and `lest cleanup <run>` replays
  it later from the report, never from the current file.
- **Retry and polling:** `retry: {attempts, delay, until}` on any step. An
  exhausted `until` is a failure. Retries report the first attempt's start
  and the cumulative duration.
- **Resources:** `resources: [name]` takes a lock file per name for the run,
  so two runs that share an account or a fixture never overlap.
- **Affects:** `affects:` lists path globs. `lest affected --base <ref>`
  prints the flows a diff touches, for change-based selection in CI.

### Tools and preflight

A flow declares the CLIs it needs (`tools:`). A tool profile is data
(`lest.yaml` or `tools/*.yaml`): how to read its version, a minimum version,
an auth `check` command, a non-interactive `heal`, an interactive `login`,
`env` to pin the tool to the run's environment, and dependencies on other
tools. Preflight runs check, heal, recheck, then (only on a TTY or from the
UI's Sign in button) login, in dependency order, and shows each tool as a
pseudo-step on the same event stream. Tool targeting goes through per-run
environment variables, never through commands that rewrite a tool's shared
config, because those retarget every other process on the machine.

### Secrets

Flows reference secrets by name (`${{ secrets.api_token }}`). One resolver
with ordered backends: the OS keyring (service `lest`), an external command
(for any password manager CLI), and environment variables for CI. Values are
resolved once per run, held in memory, and every resolved value is redacted
from every captured stream, output and report field, not only the fields of
the step that used it.

### Browser

The runner owns browser launch. A `browser` step starts the Node harness,
which loads Playwright from the project, launches Chromium with the run's
options (headless, viewport, recording, saved session, DevTools port for the
live view), and then either interprets declarative `actions:` or calls the
default export of a script with a context object (`page`, `beat`, `phase`,
`output`, `screenshot`, `cursor`). The harness reports structured events
(outputs, beats, phases, artifacts, active page) as JSON lines to a file the
runner tails, so there are no stdout regex contracts. A failure screenshot is
taken automatically. Videos and screenshots land in the run's artifact
directory, never in the source tree.

Sessions: `session: <name>` reuses a saved storage state written by a login
flow, so recordings open signed in and login never appears on camera.

### Live view

The UI connects to the browser's DevTools endpoint directly and streams
`Page.startScreencast` frames into an image, acknowledging each frame before
drawing it. The browser is launched with `--remote-allow-origins` limited to
the UI's own origin. The harness reports which page is active, so the view
follows the page the step is driving instead of guessing.

### Demos

A demo is a flow with a `demo:` block (title, summary, beat labels) and a
browser step with `record: true`. While recording, the harness draws an
eased synthetic cursor that the real mouse follows, types at a readable
pace, and records popup windows separately. After the step, Lest builds the
deliverable with ffmpeg:

- **auto-cut:** gaps between beats longer than a threshold collapse to a
  short slice, so waits disappear and every user action stays;
- **popup compositing:** a popup's recording replaces the main recording for
  the interval it was open, so a third-party sign-in shows up in the cut
  without manual splicing;
- **chapters:** a WebVTT chapter track from the beat labels;
- **beat sheet:** a contact sheet with one frame per beat.

Every label lives next to what it labels: beat labels in the flow, keyed by
the marker the script emits, and validated at load time.

### AI

AI is optional and off by default. Turning it on or off changes no layout.

- **Provider:** one interface, a headless single-turn call with no tools,
  input on stdin, a hard timeout, an empty working directory. Built-in argv
  templates for common agent CLIs plus `command`, any program that reads a
  prompt on stdin and prints text. Calls are cached by input hash and capped
  per day.
- **Failure explanation:** every failed step shows a deterministic headline
  built from the error, the exit code and the output (HTTP status, timeouts,
  auth failures, the last stderr line). With a provider, a model-written
  explanation fills the same slot, marked as such; the deterministic headline
  stays in the card body.
- **Hand off to an agent:** a flow's and a failed step's menus offer Copy
  context (the flow path, report path, failing step and rerun command, ready
  to paste) and, with a provider, Open in agent, which launches the user's own
  agent CLI in the project with that context. Lest does not build a chat.
  The user's agent already has tools, permissions and memory; Lest gives it a
  good starting point.
- **Agent docs:** `lest docs agent` prints the authoring guide and schema for
  an agent to read on demand.

Rules: no screen region exists only for AI; AI fills slots that already have
deterministic content; AI actions live in menus, never as top-level buttons
that would error with AI off; AI never blocks or delays a run.

### Notifications

Run observers consume the event stream on their own task and can never block
the runner. Lest ships one: a webhook that posts the run summary as JSON
(with an optional Slack-compatible payload). Further targets are observers
implemented the same way.

### State and paths

Config in the user config dir (`lest/config.yml`), run data in the user state
dir (`lest/runs/<flow>/<run>/report.json` plus `artifacts/`), removed with
`lest data prune --keep <n>` or `lest data purge`. Projects are directories with a `lest.yaml`; the UI
opens projects instead of falling back to a global tests directory.

## Not built, on purpose

- **In-app chat or LLM steps.** A tool-less chat duplicates the user's agent
  and is worse at the job. Model output inside a run makes failures
  irreproducible; a `run` step can call any CLI if someone wants that.
- **Vendor-specific log queries in the core.** Polling a log source is a
  `run` step with `retry.until`, and `${{ run.startedAt }}` bounds the window
  so a previous run's line cannot satisfy it.
- **A separate verification product.** Change-based selection is `affects:`
  plus `lest affected`; the evidence is the ordinary run report.
