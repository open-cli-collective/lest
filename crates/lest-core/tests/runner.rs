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

#[tokio::test]
async fn env_backend_secrets_do_not_leak_into_steps() {
    // SAFETY: tests in this binary that read the environment do not race
    // with this variable; it has a unique name.
    unsafe { std::env::set_var("LEST_SECRET_LEAKCHECK_TOKEN", "leak-check-value") };
    let f = Fixture::new(&[(
        "a.lest.yaml",
        r#"
apiVersion: lest/v1
id: leak
name: Leak
steps:
  - id: look
    run: 'printenv LEST_SECRET_LEAKCHECK_TOKEN | rev; true'
"#,
    )]);
    let (r, _) = run(&f.engine(&[]), "leak", &[]).await;
    assert_eq!(r.result, RunResult::Passed);
    assert!(!r.find_step("look").unwrap().stdout.contains("eulav"), "{}", r.find_step("look").unwrap().stdout);
}

#[tokio::test]
async fn until_does_not_hide_a_failing_attempt() {
    let f = Fixture::new(&[(
        "a.lest.yaml",
        r#"
apiVersion: lest/v1
id: until
name: Until
steps:
  - id: fails
    run: 'echo ready; exit 3'
    retry: { attempts: 2, delay: 10ms, until: "self.stdout.trim() == 'ready'" }
"#,
    )]);
    let (r, _) = run(&f.engine(&[]), "until", &[]).await;
    let s = r.find_step("fails").unwrap();
    assert_eq!(s.status, Some(StepStatus::Failed), "{s:#?}");
    assert_eq!(s.attempts, 2);
    assert_eq!(r.result, RunResult::Failed);
}

#[tokio::test]
async fn resume_refuses_a_failed_earlier_step_and_other_environments() {
    let f = Fixture::new(&[(
        "a.lest.yaml",
        r#"
apiVersion: lest/v1
id: res
name: Res
environments: { a: { x: 1 }, b: { x: 2 } }
defaultEnvironment: a
steps:
  - { id: one, run: "exit 1" }
  - { id: two, run: "true" }
"#,
    )]);
    let engine = f.engine(&[]);
    let (first, _) = run(&engine, "res", &[]).await;
    let resume = |env: Option<&str>| RunRequest {
        flow_id: "res".into(),
        environment: env.map(str::to_string),
        resume: Some((first.clone(), "two".into())),
        ..Default::default()
    };
    let (tx, _rx) = mpsc::unbounded_channel();
    let r = runner::execute(&engine, resume(None), tx.clone(), RunCancel::default()).await;
    assert_eq!(r.result, RunResult::Errored);
    assert!(r.error.as_deref().unwrap().contains("step one failed"), "{:?}", r.error);
    let r = runner::execute(&engine, resume(Some("b")), tx, RunCancel::default()).await;
    assert!(r.error.as_deref().unwrap().contains("used environment a"), "{:?}", r.error);
}

#[tokio::test]
async fn text_artifacts_are_redacted() {
    let f = Fixture::new(&[(
        "a.lest.yaml",
        r#"
apiVersion: lest/v1
id: art
name: Art
secrets: [token]
steps:
  - id: write
    env: { TOKEN: "${{ secrets.token }}" }
    run: 'echo "token=$TOKEN" > out.log; echo "token=$TOKEN" > out-2.log'
    artifacts: [{ path: "out*.log" }]
"#,
    )]);
    let engine = f.engine(&[("token", "very-secret-token")]);
    let (r, _) = run(&engine, "art", &[]).await;
    let arts = &r.find_step("write").unwrap().artifacts;
    assert_eq!(arts.len(), 2);
    let text = std::fs::read_to_string(engine.store.run_dir("art", &r.run_id).join(&arts[0].path)).unwrap();
    assert_eq!(text.trim(), "token=[redacted:token]");
}

#[tokio::test]
async fn services_start_before_steps_and_stop_after() {
    let f = Fixture::new(&[(
        "a.lest.yaml",
        r#"
apiVersion: lest/v1
id: svc
name: Svc
services:
  - id: ticker
    run: 'echo "$$" > "$LEST_RUN_DIR/ticker.pid"; echo listening; while true; do sleep 1; done'
    ready: { log: listening, timeout: 5s }
steps:
  - id: alive
    run: 'kill -0 "$(cat "$LEST_RUN_DIR/ticker.pid")"'
"#,
    )]);
    let engine = f.engine(&[]);
    let (r, _) = run(&engine, "svc", &[]).await;
    assert_eq!(r.result, RunResult::Passed, "{r:#?}");
    assert_eq!(r.services[0].id, "ticker");
    let pid = std::fs::read_to_string(engine.store.run_dir("svc", &r.run_id).join("artifacts/ticker.pid")).unwrap();
    let alive = std::process::Command::new("kill").args(["-0", pid.trim()]).status().unwrap().success();
    assert!(!alive, "service still running after the run");
}

#[tokio::test]
async fn a_service_that_exits_early_errors_the_run() {
    let f = Fixture::new(&[(
        "a.lest.yaml",
        r#"
apiVersion: lest/v1
id: svc-dies
name: Svc dies
services:
  - id: broken
    run: 'echo "port already in use" >&2; exit 1'
    ready: { log: listening }
steps:
  - { id: a, run: "true" }
"#,
    )]);
    let (r, _) = run(&f.engine(&[]), "svc-dies", &[]).await;
    assert_eq!(r.result, RunResult::Errored);
    let e = r.error.unwrap();
    assert!(e.contains("exited") && e.contains("port already in use"), "{e}");
}

#[tokio::test]
async fn services_that_ignore_term_are_killed_and_logs_are_redacted() {
    let f = Fixture::new(&[(
        "a.lest.yaml",
        r#"
apiVersion: lest/v1
id: stubborn
name: Stubborn
secrets: [token]
services:
  - id: stubborn
    run: |
      echo "token is $TOKEN"
      sh -c 'trap "" TERM; echo $$ > "$LEST_RUN_DIR/child.pid"; while true; do sleep 1; done' &
      echo listening
      wait
    env: { TOKEN: "${{ secrets.token }}" }
    ready: { log: listening }
steps:
  - { id: a, run: "sleep 0.3" }
"#,
    )]);
    let engine = f.engine(&[("token", "svc-secret-value")]);
    let (r, _) = run(&engine, "stubborn", &[]).await;
    assert_eq!(r.result, RunResult::Passed, "{r:#?}");
    let dir = engine.store.run_dir("stubborn", &r.run_id).join("artifacts");
    let pid = std::fs::read_to_string(dir.join("child.pid")).unwrap();
    let alive = std::process::Command::new("kill").args(["-0", pid.trim()]).status().unwrap().success();
    assert!(!alive, "a TERM-ignoring service process survived the run");
    let log = std::fs::read_to_string(dir.join("services/service-stubborn.log")).unwrap();
    assert!(log.contains("token is [redacted:token]"), "{log}");
}

#[tokio::test]
async fn cancel_during_a_retry_wait_is_not_a_failure() {
    let f = Fixture::new(&[(
        "a.lest.yaml",
        r#"
apiVersion: lest/v1
id: retry-cancel
name: Retry cancel
steps:
  - id: poll
    run: echo pending
    retry: { attempts: 50, delay: 200ms, until: "self.stdout.trim() == 'ready'" }
"#,
    )]);
    let engine = f.engine(&[]);
    let cancel = RunCancel::default();
    let c2 = cancel.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(500)).await;
        c2.main.cancel();
    });
    let (r, _) = run_with(&engine, "retry-cancel", &[], cancel).await;
    assert_eq!(r.result, RunResult::Cancelled);
    let s = r.find_step("poll").unwrap();
    assert_eq!(s.status, Some(StepStatus::Errored), "{s:#?}");
    assert_eq!(s.error.as_deref(), Some("cancelled"));
}

#[tokio::test]
async fn failed_runs_notify_webhooks_with_a_summary() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let (got_tx, mut got_rx) = mpsc::unbounded_channel::<String>();
    tokio::spawn(async move {
        loop {
            let (mut sock, _) = listener.accept().await.unwrap();
            let got_tx = got_tx.clone();
            tokio::spawn(async move {
                use tokio::io::{AsyncReadExt, AsyncWriteExt};
                let mut buf = vec![0u8; 16384];
                let mut req = String::new();
                loop {
                    let n = sock.read(&mut buf).await.unwrap();
                    req.push_str(&String::from_utf8_lossy(&buf[..n]));
                    if n == 0 || (req.contains("\r\n\r\n") && req.trim_end().ends_with('}')) {
                        break;
                    }
                }
                let _ = got_tx.send(req);
                sock.write_all(b"HTTP/1.1 204 No Content\r\ncontent-length: 0\r\nconnection: close\r\n\r\n")
                    .await
                    .unwrap();
            });
        }
    });
    let f = Fixture::new(&[(
        "a.lest.yaml",
        r#"
apiVersion: lest/v1
id: notify
name: Notify me
steps:
  - { id: boom, run: 'echo "error: broken" >&2; exit 1' }
"#,
    )]);
    std::fs::write(
        f.path().join("lest.yaml"),
        format!(
            "flows: [flows]\nsecrets:\n  backends: [env: {{}}]\nnotify:\n  - webhook: \"http://127.0.0.1:{port}/hook/${{{{ secrets.hook_token }}}}\"\n    secrets: [hook_token]\n"
        ),
    )
    .unwrap();
    // SAFETY: unique variable name; no other test reads it.
    unsafe { std::env::set_var("LEST_SECRET_HOOK_TOKEN", "t0ken-xyz") };
    let (r, events) = run(&f.engine(&[]), "notify", &[]).await;
    assert_eq!(r.result, RunResult::Failed);
    let req = got_rx.recv().await.unwrap();
    assert!(req.starts_with("POST /hook/t0ken-xyz "), "{req}");
    assert!(req.contains("\"result\":\"failed\""), "{req}");
    assert!(req.contains("exited 1: error: broken"), "{req}");
    let notified: Vec<_> = events.iter().filter(|e| matches!(e.body, EventBody::Notified { ok: true, .. })).collect();
    assert_eq!(notified.len(), 1);
    // The message never contains the secret URL.
    assert!(!format!("{:?}", notified[0].body).contains("t0ken"));
}
