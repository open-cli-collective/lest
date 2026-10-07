# Flow format

A flow is a YAML file named `*.lest.yaml`. `lest schema` prints the JSON
Schema generated from the same types the runner uses; point your editor's
YAML language server at it for completion:

```bash
lest schema > .lest-flow.schema.json
```

```yaml
# yaml-language-server: $schema=./.lest-flow.schema.json
apiVersion: lest/v1
id: sign-up
name: Sign up and see the dashboard
description: A new visitor creates an account and lands on the dashboard.
tags: [smoke]
environments:
  local: { base_url: "http://127.0.0.1:4173" }
  staging: { base_url: "https://staging.example.com" }
defaultEnvironment: local
inputs:
  - name: plan
    default: free
    choices:
      - { value: free, label: Free }
      - { value: team, label: Team, sets: { seats: 5 } }
secrets: [admin_token]
steps:
  - id: create_user
    http:
      method: POST
      url: "${{ vars.base_url }}/api/users"
      headers: { Authorization: "Bearer ${{ secrets.admin_token }}" }
      json: { plan: "${{ inputs.plan }}" }
    expect: self.status == 201
    outputs: { id: self.body.id }
    cleanup:
      run: |
        curl -fsS -X DELETE -H "Authorization: Bearer $TOKEN" "$base_url/api/users/$ID"
      env: { ID: "${{ self.outputs.id }}", TOKEN: "${{ secrets.admin_token }}" }
  - id: wait_ready
    env: { ID: "${{ steps.create_user.outputs.id }}" }
    run: curl -fsS "$base_url/api/users/$ID/status"
    outputs: { state: self.json.state }
    retry: { attempts: 10, delay: 1s, until: self.outputs.state == 'ready' }
  - id: check
    assert: steps.wait_ready.outputs.state == 'ready'
```

## Top-level keys

| Key | Meaning |
|---|---|
| `apiVersion` | Always `lest/v1`. |
| `id` | Stable id, unique in the project, `[a-z0-9][a-z0-9-]*`. Run history is keyed on it, so the file can move. |
| `name`, `description` | Display text. The description is Markdown. |
| `tags`, `owners` | Free-form labels. `lest list --tag` filters on tags. |
| `environments` | Named variable sets. The selected one (`--env`, or `defaultEnvironment`) is merged over `vars`. |
| `defaultEnvironment` | Used when no environment is given. |
| `vars` | Constants. Every var is exported to process steps under its own name. |
| `inputs` | Values chosen per run (`--input name=value`). See [Inputs](#inputs). |
| `secrets` | Secret names the flow reads as `${{ secrets.<name> }}`. See [Secrets](#secrets). |
| `tools` | CLIs the flow needs. See [Tools](#tools). |
| `services` | Processes started before the steps and stopped at the end. See [Services](#services). |
| `resources` | Shared things (an account, a fixture). Two runs that name the same resource never overlap; the second waits. |
| `affects` | Path globs this flow covers, for change-based selection. |
| `outputs` | Values returned to a calling `flow` step, as CEL expressions. |
| `demo` | Presents the flow as a demo. See [demos.md](demos.md). |
| `steps` | The steps, run in order. |
| `finally` | Steps that always run after `steps`, also after a failure or a cancel. |

Unknown keys are errors, on the flow and on every step, so a typo cannot be
silently ignored.

## Steps

Every step has an `id` (`[a-z][a-z0-9_]*`, unique in the flow) and exactly one
of these keys, which decides what it does:

| Key | Does |
|---|---|
| `run` | Runs a script. |
| `http` | Sends an HTTP request. |
| `assert` | Checks CEL conditions. |
| `browser` | Drives a browser. See [browser.md](browser.md). |
| `flow` | Calls another flow. |
| `steps` | Groups steps, in order or in parallel. |

Fields every step accepts:

| Field | Meaning |
|---|---|
| `name` | Display name (defaults to the id). |
| `notes` | Shown next to the step and with its failure. Write what a reader needs when this step is red. |
| `when` | CEL condition; the step is skipped when false. |
| `timeout` | Per attempt: `500ms`, `30s`, `5m`, or seconds. Defaults: 60s for `run`, 30s for `http`, 120s for `browser`. |
| `retry` | `{attempts, delay, until}`. See [Retries and polling](#retries-and-polling). |
| `continueOnError` | A failure does not stop the flow. It is reported as a warning, never hidden. |
| `env` | Environment variables for the step. Values may contain `${{ }}`. |
| `outputs` | Named values the step exposes, each a CEL expression over `self`. |
| `expect` | CEL conditions over `self` that decide success, replacing the default check. |
| `artifacts` | Files to keep with the run: `[{path, label}]`. Paths are relative to the flow file, may use `*`, and may contain `${{ }}`. |
| `cleanup` | A command that undoes the step. See [Cleanup](#cleanup). |

### run

```yaml
- id: migrate
  run: |
    ./scripts/reset-db.sh
    ./scripts/seed.sh --users 3
  shell: bash        # sh (default), bash, zsh, pwsh, or none
  cwd: ../..         # relative to the flow file; default: the flow's directory
```

The script runs with `-e`: the first failing command fails the step. It passes
when the exit code is 0 (unless `expect` says otherwise). When the script
exits, anything it left running in the background is stopped; declare
long-running processes as [services](#services).

A script cannot contain `${{ }}`. Values reach a script as environment
variables, never as text spliced into shell code:

```yaml
- id: fetch
  env:
    USER_ID: "${{ steps.create_user.outputs.id }}"
  run: curl -fsS "$base_url/api/users/$USER_ID"
```

Every process step also receives every var and input under its own name, and:

| Variable | Value |
|---|---|
| `LEST_RUN_ID` | The run id. |
| `LEST_RUN_DIR` | The run's artifact directory. Write files here to keep them. |
| `LEST_FLOW_ID`, `LEST_FLOW_DIR` | The flow's id and directory. |
| `LEST_PROJECT_DIR` | The project root. |
| `LEST_ENVIRONMENT` | The environment name, when set. |

### http

```yaml
- id: login
  http:
    method: POST                       # default GET
    url: "${{ vars.base_url }}/api/login"
    headers: { Accept: application/json }
    json: { user: "${{ vars.user }}", password: "${{ secrets.password }}" }
    # or body: "raw text"
  expect: self.status == 200
  outputs: { token: self.body.token }
```

Without `expect`, a status below 400 passes. `self.body` is the parsed JSON
body (or the text when it is not JSON), `self.headers` the lowercased response
headers, and `self.status` the status code. Inside `json:`, a string that is
exactly one `${{ }}` keeps the value's type, so `count: "${{ vars.n }}"` sends
a number.

### assert

```yaml
- id: check
  assert:
    - steps.login.status == 200
    - size(steps.list.outputs.items) >= 3
```

A false comparison reports both sides: ``assertion `self.status == 200` was
false (left == right was false: left = 404, right = 200)``.

### flow

```yaml
- id: sign_in
  flow: sign-in           # another flow's id
  with: { user: ada }     # its inputs; values may contain ${{ }}
- id: use
  assert: steps.sign_in.outputs.token != ''
```

The called flow runs inside this run, with its own inputs and vars and the
same environment, and its `finally` steps run when it ends. Its declared
`outputs` become `steps.<id>.outputs`. Its steps appear in reports and the UI
as `sign_in/<step>`. Calls cannot form a cycle.

This is also how suites are written: a flow whose steps call other flows.

### Groups

```yaml
- id: checks
  name: Independent checks
  parallel: true
  maxParallel: 4
  steps:
    - { id: api_health, http: { url: "${{ vars.base_url }}/health" } }
    - { id: docs_up, http: { url: "${{ vars.docs_url }}" } }
```

A sequential group stops at its first failure. A parallel group runs every
child; its children cannot read each other's results, and later steps can
read all of them.

## Services

```yaml
services:
  - id: app
    run: npm run dev
    cwd: ..                        # relative to the flow file
    env: { PORT: "4173" }
    ready:
      http: "http://127.0.0.1:4173/health"   # status below 400
      # log: "listening on"                  # or a line in its output
      timeout: 60s                           # default 30s
```

Services start after preflight and before the first step, and stop after
cleanups: the whole process group gets TERM, and whatever is still running
3 seconds later gets KILL. When the ready check
already passes before starting, Lest uses the running instance instead
(`reuse: false` to always start one), so a dev server you keep running is
used as is. A service declared with the same id in several called flows starts
once per run, and runs that need the same service take turns (keyed by the
ready URL's host and port, or by project and id), so one run never stops a
service another run is using. Locks live in the user's cache directory
(`LEST_LOCK_DIR` overrides it). Its output is kept as `services/service-<id>.log` in the run's
artifacts, with secret values redacted. Services receive vars (with the
environment applied), `LEST_PROJECT_DIR` and `LEST_RUN_DIR`; services of the
flow being run also receive its inputs. A called flow's services start
before it is called, so they see its vars and default inputs only.

## Expressions

`when`, `until`, `assert`, `expect` and `outputs` are
[CEL](https://github.com/google/cel-spec) expressions. In any other string
field, `${{ expr }}` inserts an expression's value (strings as-is, `null` as
empty, everything else as JSON).

| Name | Contents |
|---|---|
| `inputs.<name>` | The run's inputs (strings). |
| `vars.<name>` | `vars`, the selected environment, and values set by input choices. |
| `secrets.<name>` | Declared secrets. Allowed only in `env`, `http`, browser values and `cleanup.env`, so they never reach assertions or outputs. |
| `steps.<id>` | An earlier step: `status`, `outputs`, `stdout`, `stderr`, `exitCode`, `durationMs`, `json` (stdout parsed as JSON, or null), `body`, `headers`, `error`, `attempt`. For `http` steps `status` is the HTTP status. |
| `run` | `id`, `startedAt`, `startedAtMs`, `environment`, `flowId`, `dir`, `projectDir`. |
| `self` | The current step, in `until`, `expect`, `outputs`, `artifacts` and `cleanup`. |

Besides the CEL standard library (`size`, `contains`, `startsWith`,
`endsWith`, `matches`, `has`, `all`, `exists`, `map`, `filter`, ...), the
string, list and math extensions are available (`trim`, `split`, `replace`,
`lowerAscii`, `join`, `math.greatest`, ...), plus:

- `text.capture(regex)`: the first capture group of the first match (or the
  whole match). It is an error when nothing matches, so a missing value fails
  the step instead of passing as an empty string. Write regexes as raw
  strings, `r'id=(\d+)'`, so backslashes reach the regex.

Validation checks every reference: a misspelled input, an undeclared secret,
or a step that has not run yet at that point is an error before anything
runs.

## Outputs

```yaml
outputs:
  id: self.json.user.id                  # stdout parsed as JSON
  order: self.stdout.capture(r'order=(\w+)')   # r'' keeps backslashes
  token: self.body.token                 # http response body
  line: self.stdout.trim()
```

Outputs are evaluated after each attempt, so `until` can read
`self.outputs`. A failing output expression fails a step that otherwise
passed.

## Retries and polling

```yaml
retry:
  attempts: 20          # total attempts, including the first
  delay: 3s             # between attempts (default 1s)
  until: self.outputs.state == 'done'
```

Without `until`, a failed attempt is retried. With `until`, an attempt
counts only when it succeeds (exit code 0, or its `expect`) and the
condition is true; otherwise it is retried, and the step fails when attempts
run out. An expression error inside `until` (a field that does not exist
yet) counts as "not yet". The report keeps the first attempt's start time
and the total duration.

To wait for a log line, poll whatever reads your logs and bound the window
with the run's start, so a line from a previous run cannot satisfy it:

```yaml
- id: saw_event
  env: { SINCE: "${{ run.startedAt }}" }
  run: my-log-cli search 'order accepted' --since "$SINCE" --format json
  outputs: { count: size(self.json.results) }
  retry: { attempts: 12, delay: 5s, until: self.outputs.count > 0 }
```

Waits are evidence. When a step needs a long wait, find out why before
lengthening it: a slow step is either untested in isolation or a real
problem.

## Inputs

```yaml
inputs:
  - name: region
    label: Region
    description: Where to create the account
    default: eu
    required: false
    choices:
      - { value: eu, label: Europe, sets: { api_host: api.eu.example.com } }
      - { value: us, label: United States, sets: { api_host: api.us.example.com } }
```

`--input region=us` picks a value. A choice's `sets` adds variables, so one
choice can configure several values. Unknown inputs and values outside
`choices` stop the run before any step.

## Secrets

Flows reference secrets by name. Values come from the backends in
`lest.yaml`, tried in order (default: the OS keyring, then environment
variables):

```yaml
# lest.yaml
secrets:
  backends:
    - keyring: {}                   # lest secrets create <name> --stdin
    - env: { prefix: LEST_SECRET_ } # LEST_SECRET_ADMIN_TOKEN in CI
    - command: { run: 'my-vault read "app/$LEST_SECRET_NAME"' }
```

Every resolved value is replaced with `[redacted:<name>]` in captured output,
outputs, events and reports. Values shorter than four characters are not
redacted, since they would match ordinary text.

## Tools

```yaml
tools:
  - gh
  - { name: kubectl, minVersion: "1.29" }
  - { name: my-api-cli, auth: false }   # skip the sign-in check
```

Before any step runs, each tool must be on `PATH` and at least `minVersion`.
A tool profile in `lest.yaml` adds a sign-in check:

```yaml
# lest.yaml
tools:
  my-api-cli:
    version: my-api-cli version --short
    check: my-api-cli whoami
    heal: my-api-cli auth refresh        # tried once, non-interactively
    login: my-api-cli auth login         # interactive; terminal or UI only
    hint: Ask in #platform for access.
    env:                                  # pins the tool to this run's target
      MY_API_ENV: "${{ run.environment }}"
    requires: [vault]                     # checked first
```

Preflight runs check, then heal and check again, then (only from a terminal or
the UI's Sign in button, never in CI) the login command. Profiles pin a tool
to the run's environment through `env` rather than commands that rewrite the
tool's shared configuration, because those would retarget every other process
using the tool, including a person in another terminal.

## Cleanup

```yaml
- id: create_order
  run: ./create-order.sh
  outputs: { id: self.json.id }
  cleanup:
    run: ./delete-order.sh "$ORDER_ID"
    env: { ORDER_ID: "${{ self.outputs.id }}" }
    policy: always       # always | on-failure | manual
    timeout: 2m
```

Cleanups are registered as steps finish and run after `finally`, last
registered first. The resolved command, its environment (the step's
variables plus `cleanup.env`) and its directory are stored in the report,
with secret values redacted. `lest cleanup <run>` replays the cleanups that
did not run or failed, from the report rather than from a file that may have
changed, resolves redacted secrets again, and records the outcome so a second
replay does not repeat them.
`on-failure` keeps the state after a pass for inspection; `manual` only
records it.

On cancel (Ctrl-C or the UI's Cancel), the remaining steps are skipped and
`finally` and cleanups still run. A second Ctrl-C abandons cleanup.

## Results

| Run result | Meaning |
|---|---|
| `passed` | Every step passed or was skipped by `when`. |
| `failed` | A step failed. |
| `errored` | The run could not start or evaluate: invalid input, missing secret, a tool not ready. |
| `cancelled` | Stopped by the user. |

Step statuses are `passed`, `failed` (ran, and the result was wrong),
`errored` (could not be evaluated: timeout, spawn error, expression error),
and `skipped`. Warnings (a failed cleanup, a `continueOnError` failure) are
listed in the report next to the result.

`lest run` exits 0 when the run passed, 1 when it failed, 2 for a usage or
validation error, 3 when it errored, 4 when the flow is not found, and 130
when cancelled.
