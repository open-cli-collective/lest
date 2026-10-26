# Running Lest in CI

Everything the UI does is available from the CLI, so a CI job runs flows the
same way you do locally.

## A GitHub Actions job

One job that runs the flows a change affects:

```yaml
jobs:
  flows:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v5
        with: { fetch-depth: 0 }          # --affected needs history
      - uses: actions/setup-node@v5
        with: { node-version: 22 }
      - run: npm ci
      - run: sudo apt-get install -y ffmpeg   # only for demo recordings
      - uses: open-cli-collective/lest@v0.1.0   # pin a release tag or commit
        with:
          version: v0.1.0
          install-browsers: true              # only for browser steps
      - name: Run the flows this change affects
        env:
          LEST_SECRET_API_TOKEN: ${{ secrets.API_TOKEN }}
        run: lest run --affected origin/main --non-interactive -o reports --junit junit.xml
      - if: always()
        uses: actions/upload-artifact@v4
        with:
          name: lest-reports
          path: |
            reports/
            junit.xml
```

`lest run` takes any mix of flow ids, `--tag <tag>` (repeatable) and
`--affected <ref>`, and runs every selected flow once, in that order. When
`GITHUB_STEP_SUMMARY` is set, as it is in GitHub Actions, it appends a table
of the runs (flow, result, duration, run id, failing step and why) to the
job's summary page.

To spread flows over parallel jobs, select them in one job and run each in a
matrix job:

```yaml
jobs:
  select:
    runs-on: ubuntu-latest
    outputs:
      flows: ${{ steps.affected.outputs.flows }}
    steps:
      - uses: actions/checkout@v5
        with: { fetch-depth: 0 }
      - uses: open-cli-collective/lest@v0.1.0
        with: { version: v0.1.0 }
      - id: affected
        run: echo "flows=$(lest affected --base origin/main --json)" >> "$GITHUB_OUTPUT"
  flow:
    needs: select
    if: needs.select.outputs.flows != '[]'
    runs-on: ubuntu-latest
    strategy:
      fail-fast: false
      matrix:
        flow: ${{ fromJSON(needs.select.outputs.flows) }}
    steps:
      # checkout, Node, Playwright and lest as above
      - env: { FLOW: "${{ matrix.flow.id }}" }
        run: lest run "$FLOW" --non-interactive --junit junit.xml
```

`lest list --json` prints the same shape for a tag or name filter. Both print
one line, a JSON array of objects with an `id`: `lest list` adds `name`,
`path` and `tags`, `lest affected` adds `reasons`.

## Running several flows

- **Validation first:** every selected flow, and every flow it calls, is
  checked before any of them runs. A flow that is not found or a tag no flow
  has exits 4; a validation error exits 2.
- **Order and concurrency:** flows run one at a time by default.
  `--max-parallel <n>` runs up to n at once; each run then prints only its
  result line, not its steps, and no tool sign-in waits for input. Flows that
  share something (an account, a port, test data) declare it under
  `resources:` so they do not overlap.
- **Inputs:** each flow gets the `--input` values it declares. An input that
  no selected flow declares is a usage error.
- **Outputs:** `-o` names a directory that gets one `<flow>.json` report per
  flow, and `--junit` writes one file with a `<testsuite>` per flow inside
  `<testsuites>`. This holds for any `--tag` or `--affected` selection, even
  one that matches a single flow; naming exactly one flow keeps `-o` and
  `--junit` as single-report files.
- **Exit code:** the worst result wins: cancelled (130) over could not run
  (3) over failed (1) over passed (0). Ctrl-C cancels the running flows and
  the queued ones never start. A `--affected` selection that matches nothing
  exits 0.
- `--from` resumes a single named flow only.

## Installing Lest

The `open-cli-collective/lest` action installs a release binary on a Linux
runner (x86_64 or arm64), checks it against the release's `checksums.txt`,
caches it, and puts `lest` on `PATH`.

| Input | Default | |
|---|---|---|
| `version` | `latest` | A release tag (`v0.1.3` or `0.1.3`), or `latest`. Pin one so a new release cannot change a passing job. |
| `install-browsers` | `false` | Run `npx playwright install --with-deps chromium` in `working-directory`. Playwright itself comes from the project's `package.json`. |
| `working-directory` | `.` | The flows project. |
| `github-token` | `github.token` | Reads the release. |

The `version` output is the installed version. The action downloads with the
`gh` CLI, which GitHub-hosted runners include; a self-hosted runner needs it
installed. On other platforms, install from source (see the README).

## Choosing what to run

`lest affected --base <ref>` lists the flows a change touches:

- a changed file matches one of the flow's `affects:` globs (`*` stays
  within a directory, `**` crosses directories),
- the flow's own file, or a browser `script:` it runs, changed,
- `lest.yaml` changed (every flow is affected), or
- the flow calls an affected flow (suites run when any member is affected).

Changes are counted from the merge base of `<ref>` and `HEAD`, plus staged,
unstaged and untracked files; a rename counts as both paths. Paths are
relative to the project root, so a project in a subdirectory of the
repository works. On the base branch itself the merge base is `HEAD`, so
only uncommitted changes count: run everything there instead (for example
`lest run everything`). If any flow fails to validate, `lest affected` exits
2 rather than leaving it out.

```console
$ lest affected --base origin/main
FLOW            | WHY
dashboard       | app/server.mjs, calls sign-in
plants-api      | app/server.mjs
sign-in         | app/server.mjs
```

`--id` prints one id per line for scripts, and `--json` one JSON line for a
CI matrix.

## Results

- **Exit codes:** 0 passed, 1 failed, 2 invalid flow or usage, 3 could not
  run (a tool missing or signed out, a missing secret), 4 not found, 130
  cancelled. With several flows, the worst one wins (see above).
- **JUnit:** `--junit <path>` writes one test case per step, for CI test
  views. A run that could not start is one errored `preflight` test case.
- **JSON:** `-o <path>` writes the full report (`lest schema report`
  describes it).
- **Evidence:** `lest bundle <run> -o evidence.zip` (or `--flow <id>` for
  the flow's newest run) zips the report and the
  files it lists (step artifacts, service logs, the demo cut) with a
  `manifest.json` of SHA-256 checksums, ready to attach to a ticket, release
  or audit. Anything else a step wrote into the run directory stays out. Text
  artifacts and the report are redacted; binary artifacts (images, videos)
  are included as they are, so keep secrets off screen.

## Secrets

In CI, give secrets as environment variables: `${{ secrets.api_token }}` in a
flow reads `LEST_SECRET_API_TOKEN` with the default backends. Resolved values
are redacted from all output, and env-backend variables are removed from the
environment of every step, so a step only sees the secrets its flow passes
through `env:`.

`--non-interactive` makes sure no tool sign-in waits for input.

## Notifications

Send a summary to a webhook after each run:

```yaml
# lest.yaml
notify:
  - webhook: "https://hooks.example.com/services/${{ secrets.hook_path }}"
    secrets: [hook_path]
    on: failed          # failed (default), passed, or always
    format: slack       # json (default) or slack
```

`json` posts the run summary (flow, run id, result, environment, duration,
the failing step and its headline, warnings). `slack` posts an
incoming-webhook message, with flow names and errors escaped. Notifications
go out after the report is written, all at once, so the slowest one bounds
the wait at 10 seconds; they never print the URL, which may embed a secret.
