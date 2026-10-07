use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use lest_core::catalog::Catalog;
use lest_core::event::{EventBody, RunEvent};
use lest_core::paths::StateRoots;
use lest_core::project::Project;
use lest_core::report::{CleanupStatus, RunReport, RunResult, StepStatus};
use lest_core::runner::{self, Engine, RunCancel, RunRequest};
use lest_core::secrets::Keyring;
use lest_core::store::Store;
use lest_core::validate::validate_catalog;
use tokio::sync::mpsc;

struct MemKeyring(BTreeMap<String, String>);
impl Keyring for MemKeyring {
    fn get(&self, name: &str) -> Result<Option<String>, String> {
        Ok(self.0.get(name).cloned())
    }
    fn set(&self, _: &str, _: &str) -> Result<(), String> {
        Ok(())
    }
    fn delete(&self, _: &str) -> Result<bool, String> {
        Ok(false)
    }
}

struct Fixture {
    dir: tempfile::TempDir,
}

impl Fixture {
    fn new(files: &[(&str, &str)]) -> Fixture {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("lest.yaml"), "name: test\nflows: [flows]\n").unwrap();
        for (name, text) in files {
            let p = dir.path().join("flows").join(name);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, text).unwrap();
        }
        Fixture { dir }
    }

    fn engine(&self, secrets: &[(&str, &str)]) -> Engine {
        let project = Project::discover(self.dir.path()).unwrap();
        let mut catalog = Catalog::load(&project);
        validate_catalog(&mut catalog);
        let errors: Vec<String> = catalog.diagnostics.iter().map(|d| d.to_string()).collect();
        assert!(errors.is_empty(), "{errors:?}");
        Engine {
            project,
            catalog: Arc::new(catalog),
            store: Store::new(StateRoots::under(&self.dir.path().join(".state"))),
            keyring: Arc::new(MemKeyring(secrets.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect())),
            browser: None,
        }
    }

    fn path(&self) -> &Path {
        self.dir.path()
    }
}

async fn run(engine: &Engine, flow: &str, inputs: &[(&str, &str)]) -> (RunReport, Vec<RunEvent>) {
    run_with(engine, flow, inputs, RunCancel::default()).await
}

async fn run_with(
    engine: &Engine,
    flow: &str,
    inputs: &[(&str, &str)],
    cancel: RunCancel,
) -> (RunReport, Vec<RunEvent>) {
    let (tx, mut rx) = mpsc::unbounded_channel();
    let req = RunRequest {
        flow_id: flow.to_string(),
        inputs: inputs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect(),
        ..Default::default()
    };
    let report = runner::execute(engine, req, tx, cancel).await;
    let mut events = Vec::new();
    while let Ok(e) = rx.try_recv() {
        events.push(e);
    }
    (report, events)
}

fn status(r: &RunReport, id: &str) -> StepStatus {
    r.find_step(id).unwrap_or_else(|| panic!("no step {id}")).status.unwrap()
}

#[tokio::test]
async fn passes_outputs_between_steps() {
    let f = Fixture::new(&[(
        "a.lest.yaml",
        r#"
apiVersion: lest/v1
id: outputs
name: Outputs
vars: { greeting: hello }
steps:
  - id: make
    run: 'echo "{\"id\": \"u-42\", \"n\": 3}"'
    outputs: { id: self.json.id, n: self.json.n }
  - id: use
    env: { ID: "${{ steps.make.outputs.id }}" }
    run: 'test "$ID" = u-42 && test "$greeting" = hello && echo ok-$ID'
    outputs: { line: "self.stdout.capture('ok-(.*)')" }
  - id: check
    assert: ["steps.make.outputs.n + 1 == 4", "steps.use.outputs.line == 'u-42'"]
"#,
    )]);
    let (r, events) = run(&f.engine(&[]), "outputs", &[]).await;
    assert_eq!(r.result, RunResult::Passed, "{r:#?}");
    assert_eq!(r.find_step("make").unwrap().outputs["id"], "u-42");
    assert!(matches!(events.first().unwrap().body, EventBody::RunStarted { .. }));
    assert!(matches!(events.last().unwrap().body, EventBody::RunFinished { result: RunResult::Passed, .. }));
    let seqs: Vec<u64> = events.iter().map(|e| e.seq).collect();
    assert!(seqs.windows(2).all(|w| w[1] == w[0] + 1));
    assert!(f.path().join(".state/data/runs/outputs").join(&r.run_id).join("report.json").is_file());
}

#[tokio::test]
async fn failure_skips_rest_runs_finally_and_cleanups_in_reverse() {
    let f = Fixture::new(&[(
        "a.lest.yaml",
        r#"
apiVersion: lest/v1
id: cleanup
name: Cleanup
steps:
  - id: first
    run: echo one
    cleanup: { run: 'echo first >> "$LOG"', env: { LOG: "${{ run.dir }}/log" } }
  - id: second
    run: echo two
    cleanup: { run: 'echo second >> "$LOG"', env: { LOG: "${{ run.dir }}/log" }, policy: on-failure }
  - id: keep
    run: echo keep
    cleanup: { run: 'echo never', policy: manual }
  - id: boom
    run: 'echo "error: it broke" >&2; exit 3' 
  - id: never
    run: echo never
finally:
  - id: always
    run: echo finally
"#,
    )]);
    let engine = f.engine(&[]);
    let (r, _) = run(&engine, "cleanup", &[]).await;
    assert_eq!(r.result, RunResult::Failed);
    assert_eq!(status(&r, "boom"), StepStatus::Failed);
    assert_eq!(r.find_step("boom").unwrap().headline.as_deref(), Some("exited 3: error: it broke"));
    assert_eq!(status(&r, "never"), StepStatus::Skipped);
    assert_eq!(status(&r, "always"), StepStatus::Passed);
    let order: Vec<(&str, CleanupStatus)> = r.cleanups.iter().map(|c| (c.step_id.as_str(), c.status)).collect();
    assert_eq!(
        order,
        vec![("keep", CleanupStatus::NotRun), ("second", CleanupStatus::Passed), ("first", CleanupStatus::Passed)]
    );
    let log = std::fs::read_to_string(engine.store.run_dir("cleanup", &r.run_id).join("artifacts/log")).unwrap();
    assert_eq!(log, "second\nfirst\n");
}

#[tokio::test]
async fn retries_until_condition_holds() {
    let f = Fixture::new(&[(
        "a.lest.yaml",
        r#"
apiVersion: lest/v1
id: poll
name: Poll
steps:
  - id: poll
    run: 'n=$(cat "$LEST_RUN_DIR/n" 2>/dev/null || echo 0); n=$((n+1)); echo $n > "$LEST_RUN_DIR/n"; echo "{\"state\": \"$( [ $n -ge 3 ] && echo ready || echo pending)\"}"'
    outputs: { state: self.json.state }
    retry: { attempts: 5, delay: 10ms, until: "self.outputs.state == 'ready'" }
  - id: exhaust
    run: echo pending
    retry: { attempts: 2, delay: 10ms, until: "self.stdout.trim() == 'ready'" }
    continueOnError: true
"#,
    )]);
    let (r, events) = run(&f.engine(&[]), "poll", &[]).await;
    let poll = r.find_step("poll").unwrap();
    assert_eq!(poll.status, Some(StepStatus::Passed));
    assert_eq!(poll.attempts, 3);
    let retries = events.iter().filter(|e| matches!(e.body, EventBody::StepRetrying { .. })).count();
    assert!(retries >= 2);
    let ex = r.find_step("exhaust").unwrap();
    assert!(ex.status.unwrap().is_failure());
    assert_eq!(ex.attempts, 2);
}

#[tokio::test]
async fn http_step_with_expect_and_outputs() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        loop {
            let (mut sock, _) = listener.accept().await.unwrap();
            tokio::spawn(async move {
                use tokio::io::{AsyncReadExt, AsyncWriteExt};
                let mut buf = vec![0u8; 4096];
                let n = sock.read(&mut buf).await.unwrap();
                let req = String::from_utf8_lossy(&buf[..n]).to_string();
                let (code, body) = if req.starts_with("GET /missing") {
                    ("404 Not Found", r#"{"error":"no such thing"}"#.to_string())
                } else {
                    let auth =
                        req.lines().find(|l| l.to_lowercase().starts_with("authorization:")).unwrap_or("").to_string();
                    ("200 OK", format!(r#"{{"user":{{"id":7}},"auth":"{}"}}"#, auth.trim()))
                };
                let resp = format!(
                    "HTTP/1.1 {code}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                    body.len()
                );
                sock.write_all(resp.as_bytes()).await.unwrap();
            });
        }
    });
    let f = Fixture::new(&[(
        "a.lest.yaml",
        &format!(
            r#"
apiVersion: lest/v1
id: http
name: HTTP
vars: {{ base: "http://127.0.0.1:{port}" }}
secrets: [token]
steps:
  - id: me
    http:
      url: "${{{{ vars.base }}}}/me"
      headers: {{ Authorization: "Bearer ${{{{ secrets.token }}}}" }}
    outputs: {{ id: self.body.user.id }}
  - id: missing_ok
    http: {{ url: "${{{{ vars.base }}}}/missing" }}
    expect: "self.status == 404"
  - id: missing
    http: {{ url: "${{{{ vars.base }}}}/missing" }}
"#
        ),
    )]);
    let (r, _) = run(&f.engine(&[("token", "s3cret-value")]), "http", &[]).await;
    let me = r.find_step("me").unwrap();
    assert_eq!(me.status, Some(StepStatus::Passed), "{me:#?}");
    assert_eq!(me.outputs["id"], 7);
    assert!(me.stdout.contains("[redacted:token]"), "{}", me.stdout);
    assert!(!serde_json::to_string(&r).unwrap().contains("s3cret-value"));
    assert_eq!(status(&r, "missing_ok"), StepStatus::Passed);
    let missing = r.find_step("missing").unwrap();
    assert_eq!(missing.status, Some(StepStatus::Failed));
    assert!(missing.headline.as_deref().unwrap().starts_with("HTTP 404 from GET"), "{:?}", missing.headline);
    assert!(missing.headline.as_deref().unwrap().ends_with(": no such thing"));
}

#[tokio::test]
async fn parallel_group_runs_concurrently() {
    let f = Fixture::new(&[(
        "a.lest.yaml",
        r#"
apiVersion: lest/v1
id: par
name: Par
steps:
  - id: both
    parallel: true
    steps:
      - { id: a, run: "sleep 0.6" }
      - { id: b, run: "sleep 0.6" }
      - { id: c, run: "sleep 0.6" }
  - id: after
    assert: "steps.a.status == 'passed' && steps.c.status == 'passed'"
"#,
    )]);
    let started = Instant::now();
    let (r, _) = run(&f.engine(&[]), "par", &[]).await;
    assert_eq!(r.result, RunResult::Passed, "{r:#?}");
    assert!(started.elapsed() < Duration::from_millis(1500), "{:?}", started.elapsed());
    assert_eq!(r.find_step("both").unwrap().children.len(), 3);
}

#[tokio::test]
async fn calls_another_flow_with_inputs_and_outputs() {
    let f = Fixture::new(&[
        (
            "caller.lest.yaml",
            r#"
apiVersion: lest/v1
id: caller
name: Caller
steps:
  - id: login
    flow: login
    with: { user: ada }
  - id: check
    assert: "steps.login.outputs.token == 'token-for-ada'"
"#,
        ),
        (
            "login.lest.yaml",
            r#"
apiVersion: lest/v1
id: login
name: Login
inputs: [{ name: user, required: true }]
outputs: { token: steps.issue.outputs.token }
steps:
  - id: issue
    run: echo "token-for-$user"
    outputs: { token: self.stdout.trim() }
"#,
        ),
    ]);
    let (r, _) = run(&f.engine(&[]), "caller", &[]).await;
    assert_eq!(r.result, RunResult::Passed, "{r:#?}");
    assert_eq!(r.find_step("login/issue").unwrap().outputs["token"], "token-for-ada");
}

#[tokio::test]
async fn called_flow_outputs_reach_caller() {
    let f = Fixture::new(&[
        (
            "caller.lest.yaml",
            r#"
apiVersion: lest/v1
id: caller
name: Caller
steps:
  - { id: login, flow: login, with: { user: ada } }
  - { id: check, assert: "steps.login.outputs.token == 'token-for-ada'" }
"#,
        ),
        (
            "login.lest.yaml",
            r#"
apiVersion: lest/v1
id: login
name: Login
inputs: [{ name: user, required: true }]
outputs: { token: steps.issue.outputs.token }
steps:
  - id: issue
    run: printf "token-for-%s" "$user"
    outputs: { token: self.stdout }
"#,
        ),
    ]);
    let (r, _) = run(&f.engine(&[]), "caller", &[]).await;
    assert_eq!(r.result, RunResult::Passed, "{r:#?}");
    assert_eq!(r.find_step("login").unwrap().children[0].id, "login/issue");
}

#[tokio::test]
async fn cancel_stops_steps_but_runs_finally_and_cleanup() {
    let f = Fixture::new(&[(
        "a.lest.yaml",
        r#"
apiVersion: lest/v1
id: cancel
name: Cancel
steps:
  - id: setup
    run: echo setup
    cleanup: { run: 'echo cleaned > "$LEST_RUN_DIR/cleaned"' }
  - id: slow
    run: sleep 30
  - id: after
    run: echo never
finally:
  - id: restore
    run: echo restored > "$LEST_RUN_DIR/restored"
"#,
    )]);
    let engine = f.engine(&[]);
    let cancel = RunCancel::default();
    let c2 = cancel.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(500)).await;
        c2.main.cancel();
    });
    let started = Instant::now();
    let (r, _) = run_with(&engine, "cancel", &[], cancel).await;
    assert!(started.elapsed() < Duration::from_secs(10));
    assert_eq!(r.result, RunResult::Cancelled);
    assert_eq!(status(&r, "slow"), StepStatus::Errored);
    assert_eq!(status(&r, "after"), StepStatus::Skipped);
    assert_eq!(status(&r, "restore"), StepStatus::Passed, "{:#?}", r.find_step("restore"));
    assert_eq!(r.cleanups[0].status, CleanupStatus::Passed);
    let dir = engine.store.run_dir("cancel", &r.run_id).join("artifacts");
    assert!(dir.join("cleaned").is_file() && dir.join("restored").is_file());
}

#[tokio::test]
async fn input_choices_set_vars_and_bad_input_writes_an_errored_report() {
    let f = Fixture::new(&[(
        "a.lest.yaml",
        r#"
apiVersion: lest/v1
id: choices
name: Choices
inputs:
  - name: plan
    default: free
    choices:
      - { value: free, sets: { seats: 1 } }
      - { value: team, sets: { seats: 10 } }
steps:
  - { id: check, run: 'test "$seats" = 10 && test "$plan" = team' }
"#,
    )]);
    let engine = f.engine(&[]);
    let (r, _) = run(&engine, "choices", &[("plan", "team")]).await;
    assert_eq!(r.result, RunResult::Passed, "{r:#?}");
    let (bad, events) = run(&engine, "choices", &[("plan", "gold")]).await;
    assert_eq!(bad.result, RunResult::Errored);
    assert!(bad.error.as_deref().unwrap().contains("must be one of free, team"));
    assert!(matches!(events.last().unwrap().body, EventBody::RunFinished { result: RunResult::Errored, .. }));
    assert!(engine.store.report_path("choices", &bad.run_id).is_file());
}

#[tokio::test]
async fn missing_tool_errors_before_steps() {
    let f = Fixture::new(&[(
        "a.lest.yaml",
        r#"
apiVersion: lest/v1
id: tools
name: Tools
tools: [no-such-tool-for-lest]
steps:
  - { id: a, run: "touch should-not-exist" }
"#,
    )]);
    let (r, _) = run(&f.engine(&[]), "tools", &[]).await;
    assert_eq!(r.result, RunResult::Errored);
    assert!(r.error.as_deref().unwrap().contains("not installed"));
    assert!(r.steps.is_empty());
}

#[tokio::test]
async fn resumes_from_a_step_using_earlier_outputs() {
    let f = Fixture::new(&[(
        "a.lest.yaml",
        r#"
apiVersion: lest/v1
id: resume
name: Resume
steps:
  - id: make
    run: 'echo made-$(date +%s%N)'
    outputs: { v: self.stdout.trim() }
  - id: use
    env: { V: "${{ steps.make.outputs.v }}", MARK: "${{ run.dir }}/../../mark" }
    run: 'if [ -f "$MARK" ]; then echo "$V"; else touch "$MARK"; exit 1; fi'
    outputs: { v: self.stdout.trim() }
"#,
    )]);
    let engine = f.engine(&[]);
    let (first, _) = run(&engine, "resume", &[]).await;
    assert_eq!(first.result, RunResult::Failed);
    let (tx, _rx) = mpsc::unbounded_channel();
    let req =
        RunRequest { flow_id: "resume".into(), resume: Some((first.clone(), "use".into())), ..Default::default() };
    let second = runner::execute(&engine, req, tx, RunCancel::default()).await;
    assert_eq!(second.result, RunResult::Passed, "{second:#?}");
    assert_eq!(second.find_step("use").unwrap().outputs["v"], first.find_step("make").unwrap().outputs["v"]);
    assert_eq!(second.resumed_from.as_ref().unwrap().run_id, first.run_id);
}
