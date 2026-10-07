# Running Lest in CI

Everything the UI does is available from the CLI, so a CI job runs flows the
same way you do locally.

## A GitHub Actions job

```yaml
jobs:
  flows:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v5
        with: { fetch-depth: 0 }          # `lest affected` needs history
      - uses: actions/setup-node@v5
        with: { node-version: 22 }
      - run: npm ci && npx playwright install --with-deps chromium
      - run: sudo apt-get install -y ffmpeg   # only for demo recordings
      - run: cargo install --git https://github.com/open-cli-collective/lest --locked lest
      - name: Run the flows this change affects
        env:
          LEST_SECRET_API_TOKEN: ${{ secrets.API_TOKEN }}
        run: |
          for flow in $(lest affected --base origin/main --id); do
            lest run "$flow" --non-interactive --junit "junit-$flow.xml" || failed=1
          done
          exit ${failed:-0}
```

## Choosing what to run

`lest affected --base <ref>` lists the flows a change touches:

- a changed file matches one of the flow's `affects:` globs,
- the flow's own file changed, or
- the flow calls an affected flow (suites run when any member is affected).

Changes are counted from the merge base of `<ref>` and `HEAD`, plus staged,
unstaged and untracked files. Paths are relative to the project root, so a
project in a subdirectory of the repository works.

```console
$ lest affected --base origin/main
FLOW            | WHY
dashboard       | app/server.mjs, calls sign-in
plants-api      | app/server.mjs
sign-in         | app/server.mjs
```

`--id` prints one id per line for scripts.

## Results

- **Exit codes:** 0 passed, 1 failed, 2 invalid flow or usage, 3 could not
  run (a tool missing or signed out, a missing secret), 4 not found, 130
  cancelled.
- **JUnit:** `--junit <path>` writes one test case per step, for CI test
  views.
- **JSON:** `-o <path>` writes the full report (`lest schema report`
  describes it).
- **Evidence:** `lest bundle <run> -o evidence.zip` zips the report and every
  artifact with a `manifest.json` of SHA-256 checksums, ready to attach to a
  ticket, release or audit. Secret values are already redacted in everything
  it contains.

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
incoming-webhook message. Notifications go out after the report is written,
time out after 10 seconds, and never print the URL, which may embed a secret.
