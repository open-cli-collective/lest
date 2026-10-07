//! One attempt of a run, http, assert or browser step.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use serde_json::Value as Json;
use tokio_util::sync::CancellationToken;

use super::{BrowserCall, Frame, LineSink, Runner};
use crate::expr::{self, Scope};
use crate::process::{self, ProcessSpec};
use crate::report::{Artifact, Beat};
use crate::secrets::Redactor;
use crate::spec::{Step, StepKind};

/// The result of one attempt, before `expect`, `until` and `outputs`.
#[derive(Debug, Clone, Default)]
pub struct AttemptOutcome {
    /// The default success check held (exit 0, HTTP < 400, assertions true).
    pub passed: bool,
    /// The attempt could not be evaluated (spawn error, timeout, bad
    /// expression) as opposed to running and being wrong.
    pub errored: bool,
    pub cancelled: bool,
    pub error: Option<String>,
    pub resolved: Option<String>,
    pub exit_code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
    pub http_status: Option<u16>,
    pub body: Option<Json>,
    pub headers: BTreeMap<String, String>,
    pub duration_ms: u64,
    pub outputs: BTreeMap<String, Json>,
    pub beats: Vec<Beat>,
    pub artifacts: Vec<Artifact>,
}

impl AttemptOutcome {
    pub fn errored(msg: impl Into<String>) -> Self {
        AttemptOutcome { errored: true, error: Some(msg.into()), ..Default::default() }
    }

    pub(crate) fn redact(&mut self, r: &Redactor) {
        if r.is_empty() {
            return;
        }
        self.stdout = r.redact(&self.stdout);
        self.stderr = r.redact(&self.stderr);
        self.error = self.error.as_deref().map(|e| r.redact(e));
        self.resolved = self.resolved.as_deref().map(|e| r.redact(e));
        self.body = self.body.as_ref().map(|b| r.redact_json(b));
        for v in self.headers.values_mut() {
            *v = r.redact(v);
        }
    }
}

pub(super) async fn attempt(
    runner: &Runner<'_>,
    frame: &Frame<'_>,
    step: &Step,
    step_id: &str,
    scope: &Scope,
    cancel: &CancellationToken,
) -> AttemptOutcome {
    let timeout = step.timeout.as_ref().and_then(|d| d.parse().ok());
    match step.kind() {
        StepKind::Run => run(runner, frame, step, step_id, timeout.unwrap_or(Duration::from_secs(60)), cancel).await,
        StepKind::Http => http(runner, frame, step, timeout.unwrap_or(Duration::from_secs(30)), cancel).await,
        StepKind::Assert => assert(step, scope),
        StepKind::Browser => {
            let Some(driver) = runner.engine.browser.clone() else {
                return AttemptOutcome::errored("browser steps are not available in this build");
            };
            let env = match step_env(runner, frame, step) {
                Ok(e) => e,
                Err(e) => return AttemptOutcome::errored(e),
            };
            let scope = frame.scope_with_secrets(&runner.ctx);
            driver
                .run(BrowserCall {
                    step,
                    step_id,
                    scope: &scope,
                    env,
                    flow_dir: frame.flow.dir(),
                    run: &runner.ctx,
                    timeout: timeout.unwrap_or(Duration::from_secs(120)),
                })
                .await
        }
        StepKind::Flow | StepKind::Group => unreachable!("not a leaf step"),
    }
}

/// The process environment for a step: run variables, then `env:`.
fn step_env(runner: &Runner<'_>, frame: &Frame<'_>, step: &Step) -> Result<Vec<(String, String)>, String> {
    let mut env = frame.process_env(&runner.ctx);
    if !step.env.is_empty() {
        let scope = frame.scope_with_secrets(&runner.ctx);
        for (k, t) in &step.env {
            let v = expr::interpolate(t, &scope).map_err(|e| format!("env.{k}: {e}"))?;
            env.push((k.clone(), v));
        }
    }
    Ok(env)
}

async fn run(
    runner: &Runner<'_>,
    frame: &Frame<'_>,
    step: &Step,
    step_id: &str,
    timeout: Duration,
    cancel: &CancellationToken,
) -> AttemptOutcome {
    let script = step.run.as_deref().unwrap_or_default();
    let env = match step_env(runner, frame, step) {
        Ok(e) => e,
        Err(e) => return AttemptOutcome::errored(e),
    };
    let cwd = match &step.cwd {
        Some(c) => frame.flow.dir().join(c),
        None => frame.flow.dir().to_path_buf(),
    };
    let (program, args) = process::shell_command(step.shell.as_deref(), script);
    let mut sink = LineSink::new(&runner.ctx, step_id);
    let res = process::run(
        ProcessSpec { program, args, cwd, env, env_remove: vec![], stdin: None, timeout, cap: process::DEFAULT_CAP },
        cancel.clone(),
        |stream, line| sink.line(stream, line),
    )
    .await;
    let mut o = AttemptOutcome {
        resolved: Some(script.trim().to_string()),
        exit_code: res.exit_code,
        stdout: res.stdout,
        stderr: res.stderr,
        duration_ms: res.duration.as_millis() as u64,
        cancelled: res.cancelled,
        ..Default::default()
    };
    if let Some(e) = res.spawn_error {
        o.errored = true;
        o.error = Some(e);
    } else if res.timed_out {
        o.errored = true;
        o.error = Some(format!("timed out after {}", humantime::format_duration(timeout)));
    } else {
        o.passed = res.exit_code == Some(0);
    }
    o
}

async fn http(
    runner: &Runner<'_>,
    frame: &Frame<'_>,
    step: &Step,
    timeout: Duration,
    cancel: &CancellationToken,
) -> AttemptOutcome {
    let h = step.http.as_ref().expect("http step");
    let scope = frame.scope_with_secrets(&runner.ctx);
    let method = h.method.as_deref().unwrap_or("GET").to_uppercase();
    let url = match expr::interpolate(&h.url, &scope) {
        Ok(u) => u,
        Err(e) => return AttemptOutcome::errored(format!("url: {e}")),
    };
    let resolved = format!("{method} {url}");
    let client = match reqwest::Client::builder().timeout(timeout).build() {
        Ok(c) => c,
        Err(e) => return AttemptOutcome::errored(e.to_string()),
    };
    let Ok(m) = reqwest::Method::from_bytes(method.as_bytes()) else {
        return AttemptOutcome::errored(format!("unsupported method {method}"));
    };
    let mut req = client.request(m, &url);
    for (k, t) in &h.headers {
        match expr::interpolate(t, &scope) {
            Ok(v) => req = req.header(k, v),
            Err(e) => return AttemptOutcome::errored(format!("headers.{k}: {e}")),
        }
    }
    if let Some(j) = &h.json {
        match expr::interpolate_json(j, &scope) {
            Ok(v) => req = req.json(&v),
            Err(e) => return AttemptOutcome::errored(format!("json: {e}")),
        }
    } else if let Some(b) = &h.body {
        match expr::interpolate(b, &scope) {
            Ok(v) => req = req.body(v),
            Err(e) => return AttemptOutcome::errored(format!("body: {e}")),
        }
    }
    let started = Instant::now();
    let resp = tokio::select! {
        r = req.send() => r,
        _ = cancel.cancelled() => {
            return AttemptOutcome { cancelled: true, resolved: Some(resolved), ..Default::default() };
        }
    };
    let resp = match resp {
        Ok(r) => r,
        Err(e) => {
            let msg = if e.is_timeout() {
                format!("timed out after {}", humantime::format_duration(timeout))
            } else if e.is_connect() {
                format!("could not connect to {url}: {}", root_cause(&e))
            } else {
                root_cause(&e)
            };
            return AttemptOutcome {
                errored: true,
                error: Some(msg),
                resolved: Some(resolved),
                duration_ms: started.elapsed().as_millis() as u64,
                ..Default::default()
            };
        }
    };
    let status = resp.status().as_u16();
    let headers: BTreeMap<String, String> = resp
        .headers()
        .iter()
        .map(|(k, v)| (k.as_str().to_lowercase(), String::from_utf8_lossy(v.as_bytes()).into_owned()))
        .collect();
    let text = resp.text().await.unwrap_or_default();
    let text = if text.len() > process::DEFAULT_CAP {
        let mut cut = process::DEFAULT_CAP;
        while !text.is_char_boundary(cut) {
            cut -= 1;
        }
        text[..cut].to_string()
    } else {
        text
    };
    let body = serde_json::from_str::<Json>(&text).unwrap_or_else(|_| Json::String(text.clone()));
    AttemptOutcome {
        passed: status < 400,
        resolved: Some(resolved),
        http_status: Some(status),
        stdout: text,
        body: Some(body),
        headers,
        duration_ms: started.elapsed().as_millis() as u64,
        ..Default::default()
    }
}

fn root_cause(e: &dyn std::error::Error) -> String {
    let mut msg = e.to_string();
    let mut src = e.source();
    while let Some(s) = src {
        msg = s.to_string();
        src = s.source();
    }
    msg
}

fn assert(step: &Step, scope: &Scope) -> AttemptOutcome {
    let items = step.assert.as_ref().map(|a| a.items()).unwrap_or_default();
    let mut o = AttemptOutcome { resolved: Some(items.join(" && ")), passed: true, ..Default::default() };
    for e in items {
        match expr::eval_bool(e, scope) {
            Ok(true) => {}
            Ok(false) => {
                let why = expr::explain_false(e, scope).map(|w| format!(" ({w})")).unwrap_or_default();
                o.passed = false;
                o.error = Some(format!("assertion `{e}` was false{why}"));
                return o;
            }
            Err(err) => {
                o.passed = false;
                o.errored = true;
                o.error = Some(err.to_string());
                return o;
            }
        }
    }
    o
}
