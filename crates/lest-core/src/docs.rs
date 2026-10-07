//! Documentation for coding agents (`lest docs agent`): how to write and
//! fix flows, printed by the installed version so it always matches.

const GUIDE: &str = r#"# Lest: a guide for coding agents

Lest runs end-to-end flows (`*.lest.yaml`) against real systems and records
demos from them. Use this guide to write, run and fix flows.

## Working loop

1. Find flows: `lest list` (ids, names, paths, last result).
2. Read a flow: `lest get <id>` or open its file.
3. Validate after every edit: `lest validate` (exit 0 means valid).
4. Run: `lest run <id>`; rerun from a failed top-level step with
   `lest run <id> --from <step>`.
5. Read results: `lest runs list`, `lest runs get <run>`, or the full report
   with `lest runs get <run> -o report.json`.
6. Explain a failure: `lest explain <run>` and `lest context <run>`.

## Rules that matter

- Step ids are `[a-z][a-z0-9_]*`; later steps read `steps.<id>.outputs.<name>`.
- `run:` scripts must not contain `${{ }}`: pass values through `env:` and
  read `$NAME`.
- Expressions are CEL. Use raw strings for regexes: `self.stdout.capture(r'id=(\w+)')`.
- Secrets: list them under `secrets:` and use `${{ secrets.x }}` only in
  `env:`, `http`, browser values and `cleanup.env`.
- Poll with `retry: {attempts, delay, until}`; do not lengthen waits to hide
  slowness. A slow or failing step is information: find out why.
- Register cleanup on the step that creates something (`cleanup:`).
- Never weaken an assertion to make a flow pass when it caught a real problem.

The full references follow.
"#;

const FLOW_FORMAT: &str = include_str!("../../../docs/flow-format.md");
const BROWSER: &str = include_str!("../../../docs/browser.md");
const DEMOS: &str = include_str!("../../../docs/demos.md");
const CI: &str = include_str!("../../../docs/ci.md");

/// The agent guide: rules first, then the reference docs.
pub fn agent_guide() -> String {
    let mut s = String::from(GUIDE);
    for doc in [FLOW_FORMAT, BROWSER, DEMOS, CI] {
        s.push_str("\n\n---\n\n");
        s.push_str(doc);
    }
    s
}
