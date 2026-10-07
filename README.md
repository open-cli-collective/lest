# lest

Run end-to-end flows against real systems, lest the path your users depend on
breaks unnoticed.

A flow is a YAML file of steps: scripts, HTTP requests, assertions, and calls
to other flows. Lest runs it against the environment you choose, checks the
tools it needs first, keeps every result in a report, and cleans up after
itself, even when you cancel.

```yaml
apiVersion: lest/v1
id: hello
name: Hello, Lest
vars:
  greeting: hello
steps:
  - id: say
    run: echo "$greeting from $LEST_FLOW_ID"
    outputs:
      text: self.stdout.trim()
  - id: check
    assert: steps.say.outputs.text == 'hello from hello'
```

```console
$ lest run hello
▶ Hello, Lest · run 20261007T123002Z-vhyf
  ✓ say                         10ms
  ✓ check                        0ms
passed hello 11ms 20261007T123002Z-vhyf
```

## Install

From source (Rust stable):

```bash
cargo install --git https://github.com/open-cli-collective/lest --locked lest
```

## Quick start

```bash
mkdir my-flows && cd my-flows
lest init            # creates lest.yaml and flows/hello.lest.yaml
lest run hello
lest list
lest runs list
```

## What a flow can do

- **Steps:** `run` a script, send an `http` request, `assert` conditions,
  drive a `browser`, call another `flow`, or group steps in order or in
  `parallel`.
- **Browsers:** declarative actions or a Playwright script, saved sign-in
  sessions, screenshots at failure, and recorded videos with a visible cursor.
- **Services:** start a dev server or mock before the steps, wait until it
  is ready, and stop it after.
- **Expressions:** one language, [CEL](https://github.com/google/cel-spec),
  for conditions, assertions and outputs, and `${{ expr }}` inside strings.
  Values reach scripts as environment variables, never as text spliced into
  shell code.
- **Retries and polling:** `retry: {attempts, delay, until}` on any step.
- **Environments and inputs:** named variable sets and per-run choices.
- **Secrets:** referenced by name, resolved from the OS keyring, environment
  variables or any password-manager command, and redacted from every output
  and report.
- **Tool preflight:** each CLI a flow needs is checked for presence, version
  and sign-in before anything runs.
- **Cleanup:** steps register commands that undo them; they run after the
  flow, on failure and on cancel, and can be replayed later with
  `lest cleanup <run>`.
- **Reports:** a JSON report per run (`lest schema report` describes it) and
  JUnit XML for CI.

## Examples

[`examples/`](examples) is a project with a small sample web app (Sprout, a
plant-care dashboard) and flows that exercise every feature: an API flow with
polling and cleanup, a browser sign-in that saves a session, a dashboard check
that reuses it, a suite that runs them in parallel, a recorded walkthrough
with a popup, and a flow that fails on purpose.

```bash
cd examples
npm ci && npx playwright install chromium
lest run everything
```

## Documentation

- [Flow format](docs/flow-format.md)
- [Browser steps](docs/browser.md)
- [Design](docs/design.md): why Lest works the way it does
- [Development](docs/development.md)

## License

MIT
