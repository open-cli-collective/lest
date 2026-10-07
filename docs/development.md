# Development

## Layout

| Path | Contents |
|---|---|
| `crates/lest-core` | Flow format, expressions, validation, runner, reports, store, secrets, tool preflight. No UI code. |
| `crates/lest` | The `lest` binary: argument parsing, terminal output, command glue. |
| `docs/` | Reference docs (`flow-format.md`) and the design record (`design.md`). |
| `scripts/` | Repository checks and git hooks. |

`docs/design.md` records why Lest is shaped the way it is. Read it before
changing the run model, the flow format, or how AI is used.

## Build and test

Requires a stable Rust toolchain (see `rust-toolchain.toml`).

```bash
make check     # what CI runs: lint, test, build
make test
make lint      # rustfmt check, clippy -D warnings, workflow pin check
cargo run -p lest -- run hello -p path/to/project
```

Tests never touch your real config, data or keyring: the runner takes a
`StateRoots` and a `Keyring` implementation, and integration tests use
temporary directories and an in-memory keyring.

## Invariants

- **The report is canonical.** Live events preview the report; every run path
  ends with a `run_finished` event and a report on disk, including preflight
  failures.
- **Every event names its run.** Consumers key state by `runId`.
- **Values are data.** `run:` scripts never contain `${{ }}`; values reach
  scripts through environment variables. Expressions are CEL over JSON values.
- **Secrets are redacted everywhere.** Any resolved secret value is replaced
  in streams, outputs, errors and reports, not only in the step that used it.
- **Cleanup runs on cancel.** The first cancel stops steps; `finally` and
  cleanups still run. Only a second cancel abandons cleanup.
- **Child processes are supervised.** Output is drained while the child runs,
  each stream keeps its first 8 MB, and timeouts and cancels kill the whole
  process group.
- **stdout is data.** Commands print results to stdout and progress,
  warnings and errors to stderr.

## Exit codes

0 passed, 1 failed, 2 usage or validation error, 3 could not run
(preflight, auth, configuration), 4 not found, 130 cancelled.

## Divergences from the shared CLI standards

- **No `me` command.** Lest talks to no upstream service, so there is no
  identity to check. `lest doctor` is the health check: it reports the
  project, flows, data directory, Node, ffmpeg, the keyring tool and every
  tool profile, and exits non-zero when something required is missing.
- **CI runs on Linux only.** macOS and Windows builds are not exercised in CI.
- **Secrets** go through the platform keyring's own CLI (`security` on macOS,
  `secret-tool` on Linux) so the binary links no keyring library; the `env`
  and `command` backends cover CI and password managers.

## Public repository hygiene

This is a public, company-neutral project. Examples use the bundled sample
app or placeholders (`example.com`, `acme`), never a real product, customer,
host or person. `scripts/check-denylist.sh` scans tracked files and commit
messages against a maintainer-private term list kept outside the repository;
the `pre-push` hook runs it. Enable the hooks once per clone:

```bash
git config core.hooksPath scripts/hooks
```

## Pull requests

Titles are conventional commits (`feat:`, `fix:`, `docs:`, ...); the squash
commit uses the PR title. Run `make check` before pushing.
