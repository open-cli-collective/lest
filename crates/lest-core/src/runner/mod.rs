//! The runner: executes one flow and produces its report.

mod artifacts;
mod steps;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use futures::future::BoxFuture;
use futures::{FutureExt, StreamExt};
use serde_json::{Value as Json, json};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::catalog::{Catalog, LoadedFlow};
use crate::event::{EventBody, PlanNode, RunEvent, Stream};
use crate::expr::{self, Scope};
use crate::headline::headline;
use crate::paths::ensure_private_dir;
use crate::project::Project;
use crate::report::*;
use crate::secrets::{Keyring, Redactor, Resolver};
use crate::spec::{CleanupPolicy, Flow, Step, StepKind};
use crate::store::{Store, new_run_id};
use crate::tools::{self, Need};

pub use steps::AttemptOutcome;

/// Everything a run needs from its surroundings.
#[derive(Clone)]
pub struct Engine {
    pub project: Project,
    pub catalog: Arc<Catalog>,
    pub store: Store,
    pub keyring: Arc<dyn Keyring>,
    /// Hooks for browser steps; `None` makes browser steps error.
    pub browser: Option<Arc<dyn BrowserDriver>>,
}

/// Runs browser steps. Implemented by the browser module; a trait so the
/// runner can be tested without Node.
pub trait BrowserDriver: Send + Sync {
    fn run<'a>(&'a self, ctx: BrowserCall<'a>) -> BoxFuture<'a, AttemptOutcome>;
}

/// One browser step attempt.
pub struct BrowserCall<'a> {
    pub step: &'a Step,
    pub step_id: &'a str,
    pub scope: &'a Scope,
    pub env: Vec<(String, String)>,
    pub flow_dir: &'a Path,
    pub run: &'a RunCtx,
    pub timeout: Duration,
}

#[derive(Debug, Clone, Default)]
pub struct BrowserOptions {
    pub headed: bool,
    /// Expose a DevTools port so the UI can stream the page.
    pub live_view: bool,
}

#[derive(Debug, Clone, Default)]
pub struct RunRequest {
    pub flow_id: String,
    pub environment: Option<String>,
    pub inputs: BTreeMap<String, String>,
    /// Re-run from this top-level step, taking earlier results from `from_run`.
    pub resume: Option<(RunReport, String)>,
    /// Allows interactive tool sign-in from a terminal.
    pub interactive: bool,
    pub browser: BrowserOptions,
}

/// Two-stage cancel: `main` stops the steps, `cleanup` abandons cleanup.
#[derive(Clone, Default)]
pub struct RunCancel {
    pub main: CancellationToken,
    pub cleanup: CancellationToken,
}

/// Sends events for one run.
#[derive(Clone)]
pub struct Emitter {
    run_id: String,
    seq: Arc<AtomicU64>,
    tx: mpsc::UnboundedSender<RunEvent>,
}

impl Emitter {
    pub fn new(run_id: &str, tx: mpsc::UnboundedSender<RunEvent>) -> Self {
        Emitter { run_id: run_id.to_string(), seq: Arc::new(AtomicU64::new(0)), tx }
    }

    pub fn emit(&self, body: EventBody) {
        let ev = RunEvent {
            run_id: self.run_id.clone(),
            seq: self.seq.fetch_add(1, Ordering::SeqCst),
            at_ms: now_ms(),
            body,
        };
        let _ = self.tx.send(ev);
    }
}

pub fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

pub fn now_rfc3339() -> String {
    time::OffsetDateTime::now_utc().format(&time::format_description::well_known::Rfc3339).unwrap_or_default()
}

/// Per-run state shared by every step.
pub struct RunCtx {
    pub run_id: String,
    pub run_dir: PathBuf,
    pub emitter: Emitter,
    pub project: Project,
    pub environment: Option<String>,
    pub redactor: Redactor,
    pub secrets: BTreeMap<String, String>,
    pub cancel: RunCancel,
    pub started_at: String,
    pub started_at_ms: u64,
    pub flow_id: String,
    /// Environment variables pinning tools to the run's target.
    pub tool_env: Vec<(String, String)>,
    pub browser: BrowserOptions,
    pub sessions_dir: PathBuf,
    cleanups: Mutex<Vec<PendingCleanup>>,
    warnings: Mutex<Vec<String>>,
}

impl RunCtx {
    pub fn artifacts_dir(&self) -> PathBuf {
        self.run_dir.join("artifacts")
    }
    pub fn warn(&self, w: String) {
        self.warnings.lock().expect("lock").push(w);
    }
    fn run_json(&self) -> Json {
        json!({
            "id": self.run_id,
            "startedAt": self.started_at,
            "startedAtMs": self.started_at_ms,
            "environment": self.environment,
            "flowId": self.flow_id,
            "dir": self.artifacts_dir().display().to_string(),
            "projectDir": self.project.root.display().to_string(),
        })
    }
}

struct PendingCleanup {
    step_id: String,
    command: String,
    env: BTreeMap<String, String>,
    policy: CleanupPolicy,
    timeout: Duration,
    cwd: PathBuf,
    /// The step's run variables, so a cleanup sees what the step saw.
    base_vars: Vec<(String, String)>,
}

/// The scope of one flow instance (the top flow or a called flow).
struct Frame<'a> {
    flow: &'a LoadedFlow,
    /// Prefix for step ids inside a called flow (`call/`).
    prefix: String,
    inputs: BTreeMap<String, Json>,
    vars: BTreeMap<String, Json>,
    steps: Mutex<serde_json::Map<String, Json>>,
}

impl Frame<'_> {
    fn scope(&self, run: &RunCtx) -> Scope {
        let mut s = Scope::new();
        s.set("inputs", Json::Object(self.inputs.clone().into_iter().collect()));
        s.set("vars", Json::Object(self.vars.clone().into_iter().collect()));
        s.set("steps", Json::Object(self.steps.lock().expect("lock").clone()));
        s.set("run", run.run_json());
        s
    }

    /// The scope plus secrets, for fields that may use them.
    fn scope_with_secrets(&self, run: &RunCtx) -> Scope {
        let mut s = self.scope(run);
        s.set("secrets", Json::Object(run.secrets.iter().map(|(k, v)| (k.clone(), Json::String(v.clone()))).collect()));
        s
    }

    fn qualified(&self, id: &str) -> String {
        format!("{}{id}", self.prefix)
    }

    /// Variables exported to every process step.
    fn process_env(&self, run: &RunCtx) -> Vec<(String, String)> {
        let mut env: Vec<(String, String)> = Vec::new();
        for (k, v) in self.vars.iter().chain(self.inputs.iter()) {
            env.push((k.clone(), expr::stringify(v)));
        }
        env.extend(run.tool_env.iter().cloned());
        env.push(("LEST_RUN_ID".into(), run.run_id.clone()));
        env.push(("LEST_RUN_DIR".into(), run.artifacts_dir().display().to_string()));
        env.push(("LEST_FLOW_ID".into(), self.flow.flow.id.clone()));
        env.push(("LEST_FLOW_DIR".into(), self.flow.dir().display().to_string()));
        env.push(("LEST_PROJECT_DIR".into(), run.project.root.display().to_string()));
        if let Some(e) = &run.environment {
            env.push(("LEST_ENVIRONMENT".into(), e.clone()));
        }
        env
    }
}

/// Named JSON values (inputs, vars).
type Values = BTreeMap<String, Json>;

/// Resolves the run's inputs and variables for a flow.
fn resolve_inputs_and_vars(
    flow: &Flow,
    environment: Option<&str>,
    given: &BTreeMap<String, String>,
) -> Result<(Values, Values), String> {
    let mut vars: BTreeMap<String, Json> = flow.vars.iter().map(|(k, v)| (k.clone(), v.to_json())).collect();
    if let Some(env) = environment {
        match flow.environments.get(env) {
            Some(block) => vars.extend(block.iter().map(|(k, v)| (k.clone(), v.to_json()))),
            None if !flow.environments.is_empty() => {
                return Err(format!(
                    "flow '{}' has no environment '{env}' (it has: {})",
                    flow.id,
                    flow.environments.keys().cloned().collect::<Vec<_>>().join(", ")
                ));
            }
            None => {}
        }
    }
    let mut inputs = BTreeMap::new();
    for input in &flow.inputs {
        let value = given.get(&input.name).cloned().or_else(|| input.default.as_ref().map(|d| d.as_env_string()));
        let Some(value) = value else {
            if input.required {
                return Err(format!("input '{}' is required (--input {}=<value>)", input.name, input.name));
            }
            inputs.insert(input.name.clone(), Json::String(String::new()));
            continue;
        };
        if !input.choices.is_empty() {
            let Some(choice) = input.choices.iter().find(|c| c.value == value) else {
                return Err(format!(
                    "input '{}' must be one of {}, got '{value}'",
                    input.name,
                    input.choices.iter().map(|c| c.value.as_str()).collect::<Vec<_>>().join(", ")
                ));
            };
            vars.extend(choice.sets.iter().map(|(k, v)| (k.clone(), v.to_json())));
        }
        inputs.insert(input.name.clone(), Json::String(value));
    }
    for k in given.keys() {
        if !flow.inputs.iter().any(|i| &i.name == k) {
            return Err(format!("flow '{}' has no input '{k}'", flow.id));
        }
    }
    Ok((inputs, vars))
}

fn plan(steps: &[Step], prefix: &str, catalog: &Catalog) -> Vec<PlanNode> {
    steps
        .iter()
        .map(|s| {
            let id = format!("{prefix}{}", s.id);
            let children = match s.kind() {
                StepKind::Group => plan(s.children(), prefix, catalog),
                StepKind::Flow => catalog
                    .get(s.flow.as_deref().unwrap_or_default())
                    .map(|callee| {
                        let p = format!("{id}/");
                        let mut c = plan(&callee.flow.steps, &p, catalog);
                        c.extend(plan(&callee.flow.finally, &p, catalog));
                        c
                    })
                    .unwrap_or_default(),
                _ => vec![],
            };
            PlanNode { id, name: s.display_name().to_string(), kind: s.kind(), notes: s.notes.clone(), children }
        })
        .collect()
}

/// Every flow reachable from `root` through flow steps (including root).
fn reachable<'a>(root: &'a LoadedFlow, catalog: &'a Catalog) -> Vec<&'a LoadedFlow> {
    fn calls(steps: &[Step], out: &mut Vec<String>) {
        for s in steps {
            if let Some(f) = &s.flow {
                out.push(f.clone());
            }
            calls(s.children(), out);
        }
    }
    let mut out = vec![root];
    let mut i = 0;
    while i < out.len() {
        let mut next = Vec::new();
        calls(&out[i].flow.steps, &mut next);
        calls(&out[i].flow.finally, &mut next);
        for id in next {
            if let Some(f) = catalog.get(&id)
                && !out.iter().any(|o| o.flow.id == f.flow.id)
            {
                out.push(f);
            }
        }
        i += 1;
    }
    out
}

/// Takes the resource locks a run needs, waiting while another run holds one.
fn lock_resources(
    names: &[String],
    dir: &Path,
    cancel: &CancellationToken,
    emitter: &Emitter,
) -> Result<Vec<std::fs::File>, String> {
    let mut held = Vec::new();
    if names.is_empty() {
        return Ok(held);
    }
    ensure_private_dir(dir).map_err(|e| format!("cannot create lock dir: {e}"))?;
    let mut sorted = names.to_vec();
    sorted.sort();
    sorted.dedup();
    for name in sorted {
        let safe: String =
            name.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' }).collect();
        let file = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(dir.join(format!("{safe}.lock")))
            .map_err(|e| format!("cannot open lock for '{name}': {e}"))?;
        let mut announced = false;
        loop {
            match file.try_lock() {
                Ok(()) => break,
                Err(std::fs::TryLockError::WouldBlock) => {
                    if !announced {
                        emitter.emit(EventBody::DemoProgress {
                            message: format!("waiting for resource '{name}', held by another run"),
                        });
                        announced = true;
                    }
                    if cancel.is_cancelled() {
                        return Err(format!("cancelled while waiting for resource '{name}'"));
                    }
                    std::thread::sleep(Duration::from_millis(500));
                }
                Err(std::fs::TryLockError::Error(e)) => return Err(format!("cannot lock '{name}': {e}")),
            }
        }
        held.push(file);
    }
    Ok(held)
}

/// Runs a flow to completion and writes its report. Always emits a final
/// `run_finished` event and always writes a report, whatever happens.
pub async fn execute(
    engine: &Engine,
    req: RunRequest,
    tx: mpsc::UnboundedSender<RunEvent>,
    cancel: RunCancel,
) -> RunReport {
    let run_id = new_run_id();
    execute_with_id(engine, req, tx, cancel, run_id).await
}

pub async fn execute_with_id(
    engine: &Engine,
    req: RunRequest,
    tx: mpsc::UnboundedSender<RunEvent>,
    cancel: RunCancel,
    run_id: String,
) -> RunReport {
    let emitter = Emitter::new(&run_id, tx);
    let started = Instant::now();
    let started_at = now_rfc3339();
    let started_at_ms = now_ms();

    let Some(lf) = engine.catalog.get(&req.flow_id) else {
        let report = RunReport {
            schema_version: REPORT_SCHEMA_VERSION,
            run_id: run_id.clone(),
            flow_id: req.flow_id.clone(),
            flow_name: req.flow_id.clone(),
            flow_path: String::new(),
            flow_sha256: BTreeMap::new(),
            project_dir: engine.project.root.display().to_string(),
            environment: req.environment.clone(),
            started_at,
            finished_at: Some(now_rfc3339()),
            duration_ms: 0,
            result: RunResult::Errored,
            error: Some(format!("no flow '{}'", req.flow_id)),
            warnings: vec![],
            inputs: req.inputs.clone(),
            host: HostInfo::current(),
            tools: vec![],
            steps: vec![],
            finally: vec![],
            cleanups: vec![],
            resumed_from: None,
            demo: None,
        };
        return finish(engine, &emitter, report, started);
    };
    let flow = &lf.flow;
    let environment = req.environment.clone().or_else(|| flow.default_environment.clone());
    let flows = reachable(lf, &engine.catalog);
    let run_dir = engine.store.run_dir(&flow.id, &run_id);

    let mut report = RunReport {
        schema_version: REPORT_SCHEMA_VERSION,
        run_id: run_id.clone(),
        flow_id: flow.id.clone(),
        flow_name: flow.name.clone(),
        flow_path: lf.rel_path.clone(),
        flow_sha256: flows.iter().map(|f| (f.rel_path.clone(), f.sha256.clone())).collect(),
        project_dir: engine.project.root.display().to_string(),
        environment: environment.clone(),
        started_at: started_at.clone(),
        finished_at: None,
        duration_ms: 0,
        result: RunResult::Passed,
        error: None,
        warnings: vec![],
        inputs: req.inputs.clone(),
        host: HostInfo::current(),
        tools: vec![],
        steps: vec![],
        finally: vec![],
        cleanups: vec![],
        resumed_from: req.resume.as_ref().map(|(r, s)| ResumeInfo { run_id: r.run_id.clone(), step: s.clone() }),
        demo: None,
    };

    // Tools from every reachable flow, merged by name.
    let mut needs: Vec<Need> = Vec::new();
    for f in &flows {
        for t in &f.flow.tools {
            match needs.iter_mut().find(|n| n.name == t.name()) {
                Some(n) => {
                    if n.min_version.is_none() {
                        n.min_version = t.min_version().map(str::to_string);
                    }
                }
                None => needs.push(Need {
                    name: t.name().to_string(),
                    min_version: t.min_version().map(str::to_string),
                    auth: t.auth(),
                }),
            }
        }
    }
    let needs = tools::order(&needs, &engine.project.config.tools);

    emitter.emit(EventBody::RunStarted {
        flow_id: flow.id.clone(),
        flow_name: flow.name.clone(),
        environment: environment.clone(),
        run_dir: run_dir.display().to_string(),
        steps: plan(&flow.steps, "", &engine.catalog),
        finally: plan(&flow.finally, "", &engine.catalog),
        tools: needs.iter().map(|n| n.name.clone()).collect(),
    });

    let fail = |mut report: RunReport, msg: String| {
        report.result = RunResult::Errored;
        report.error = Some(msg);
        report
    };

    if let Err(e) = ensure_private_dir(&run_dir.join("artifacts")) {
        return finish(engine, &emitter, fail(report, format!("cannot create run directory: {e}")), started);
    }

    let (inputs, vars) = match resolve_inputs_and_vars(flow, environment.as_deref(), &req.inputs) {
        Ok(v) => v,
        Err(e) => return finish(engine, &emitter, fail(report, e), started),
    };
    report.inputs = inputs.iter().map(|(k, v)| (k.clone(), expr::stringify(v))).collect();

    // Secrets from every reachable flow and notify target, resolved once.
    let mut secret_names: Vec<String> = flows.iter().flat_map(|f| f.flow.secrets.iter().cloned()).collect();
    secret_names.sort();
    secret_names.dedup();
    let resolver = Resolver::new(engine.project.secrets_config(), engine.keyring.as_ref());
    let secrets = match resolver.resolve_all(&secret_names) {
        Ok(s) => s,
        Err(e) => return finish(engine, &emitter, fail(report, e), started),
    };
    let redactor = Redactor::new(&secrets);

    let mut resources: Vec<String> = flows.iter().flat_map(|f| f.flow.resources.iter().cloned()).collect();
    resources.sort();
    resources.dedup();
    let locks = {
        let dir = engine.store.roots().locks_dir();
        let cancel = cancel.main.clone();
        let em = emitter.clone();
        tokio::task::spawn_blocking(move || lock_resources(&resources, &dir, &cancel, &em))
            .await
            .unwrap_or_else(|e| Err(e.to_string()))
    };
    let _locks = match locks {
        Ok(l) => l,
        Err(e) => return finish(engine, &emitter, fail(report, e), started),
    };

    // Tool env pins, then preflight with those pins applied.
    let mut tool_env = Vec::new();
    {
        let mut scope = Scope::new();
        scope.set("vars", Json::Object(vars.clone().into_iter().collect()));
        scope.set("run", json!({"environment": environment, "id": run_id}));
        for need in &needs {
            if let Some(p) = engine.project.config.tools.get(&need.name) {
                for (k, tmpl) in &p.env {
                    match expr::interpolate(tmpl, &scope) {
                        Ok(v) => tool_env.push((k.clone(), v)),
                        Err(e) => {
                            return finish(
                                engine,
                                &emitter,
                                fail(report, format!("tool {} env {k}: {e}", need.name)),
                                started,
                            );
                        }
                    }
                }
            }
        }
    }
    for need in &needs {
        let profile = engine.project.config.tools.get(&need.name);
        let mut checked = tools::check(need, profile, &engine.project.root, &tool_env, &cancel.main).await;
        if checked.report.status == crate::report::ToolStatus::SignedOut
            && req.interactive
            && let Some(login) = &checked.login
        {
            eprintln!("lest: {} is not signed in; running `{login}`", need.name);
            let status = tokio::process::Command::new("sh")
                .arg("-c")
                .arg(login)
                .current_dir(&engine.project.root)
                .envs(tool_env.iter().cloned())
                .status()
                .await;
            if status.is_ok_and(|s| s.success()) {
                checked = tools::check(need, profile, &engine.project.root, &tool_env, &cancel.main).await;
            }
        }
        emitter.emit(EventBody::Tool { tool: checked.report.clone() });
        let status = checked.report.status;
        let message = checked.report.message.clone();
        report.tools.push(checked.report);
        match status {
            crate::report::ToolStatus::Ready | crate::report::ToolStatus::Unknown => {}
            _ => {
                if status == crate::report::ToolStatus::SignedOut
                    && let Some(login) = checked.login
                {
                    emitter.emit(EventBody::SignInNeeded { tool: need.name.clone(), command: login });
                }
                let msg = message.unwrap_or_else(|| format!("{} is not ready", need.name));
                return finish(engine, &emitter, fail(report, msg), started);
            }
        }
    }

    let ctx = Arc::new(RunCtx {
        run_id: run_id.clone(),
        run_dir: run_dir.clone(),
        emitter: emitter.clone(),
        project: engine.project.clone(),
        environment: environment.clone(),
        redactor,
        secrets,
        cancel: cancel.clone(),
        started_at,
        started_at_ms,
        flow_id: flow.id.clone(),
        tool_env,
        browser: req.browser.clone(),
        sessions_dir: engine.store.roots().sessions_dir(),
        cleanups: Mutex::new(Vec::new()),
        warnings: Mutex::new(Vec::new()),
    });

    let frame = Frame { flow: lf, prefix: String::new(), inputs, vars, steps: Mutex::new(serde_json::Map::new()) };

    // Resume: earlier top-level steps take their results from the old run.
    let mut resume_index = 0;
    let mut seeded = Vec::new();
    if let Some((old, from)) = &req.resume {
        match flow.steps.iter().position(|s| &s.id == from || from.starts_with(&format!("{}/", s.id))) {
            Some(i) => {
                resume_index = i;
                for s in &flow.steps[..i] {
                    if let Some(prev) = old.steps.iter().find(|r| r.id == s.id) {
                        seed_step(&frame, prev);
                        seeded.push(prev.clone());
                        emitter.emit(EventBody::StepFinished { step: Box::new(prev.clone()) });
                    }
                }
            }
            None => {
                return finish(
                    engine,
                    &emitter,
                    fail(report, format!("no top-level step '{from}' to resume from")),
                    started,
                );
            }
        }
    }

    let runner = Runner { engine, ctx: ctx.clone() };
    let mut main_reports = seeded;
    let (mut rest, failed) = runner.run_sequence(&frame, &flow.steps[resume_index..], &ctx.cancel.main).await;
    main_reports.append(&mut rest);
    report.steps = main_reports;

    let cancelled = ctx.cancel.main.is_cancelled();
    let (finally_reports, finally_failed) = runner.run_sequence(&frame, &flow.finally, &ctx.cancel.cleanup).await;
    report.finally = finally_reports;

    report.result = if cancelled {
        RunResult::Cancelled
    } else if failed || finally_failed {
        RunResult::Failed
    } else {
        RunResult::Passed
    };

    report.cleanups = runner.run_cleanups(report.result).await;
    report.warnings = ctx.warnings.lock().expect("lock").clone();
    finish(engine, &emitter, report, started)
}

fn seed_step(frame: &Frame, prev: &StepReport) {
    let local = prev.id.rsplit('/').next().unwrap_or(&prev.id).to_string();
    frame.steps.lock().expect("lock").insert(local, step_json(prev, None));
    for c in &prev.children {
        seed_step(frame, c);
    }
}

fn finish(engine: &Engine, emitter: &Emitter, mut report: RunReport, started: Instant) -> RunReport {
    report.duration_ms = started.elapsed().as_millis() as u64;
    report.finished_at = Some(now_rfc3339());
    let path = match engine.store.write(&report) {
        Ok(p) => p.display().to_string(),
        Err(e) => {
            report.warnings.push(format!("could not write report: {e}"));
            String::new()
        }
    };
    emitter.emit(EventBody::RunFinished {
        result: report.result,
        duration_ms: report.duration_ms,
        error: report.error.clone(),
        report_path: path,
    });
    report
}

/// The JSON a later step reads as `steps.<id>`.
fn step_json(r: &StepReport, extra: Option<&AttemptOutcome>) -> Json {
    let mut m = serde_json::Map::new();
    m.insert("status".into(), json!(r.status.map(|s| s.as_str()).unwrap_or("pending")));
    m.insert("outputs".into(), Json::Object(r.outputs.clone().into_iter().collect()));
    m.insert("stdout".into(), json!(r.stdout));
    m.insert("stderr".into(), json!(r.stderr));
    m.insert("exitCode".into(), json!(r.exit_code));
    m.insert("durationMs".into(), json!(r.duration_ms));
    m.insert("error".into(), json!(r.error));
    m.insert("attempt".into(), json!(r.attempts));
    m.insert("json".into(), serde_json::from_str::<Json>(&r.stdout).unwrap_or(Json::Null));
    if let Some(o) = extra {
        m.insert("body".into(), o.body.clone().unwrap_or(Json::Null));
        m.insert("headers".into(), Json::Object(o.headers.iter().map(|(k, v)| (k.clone(), json!(v))).collect()));
        if let Some(s) = o.http_status {
            m.insert("status".into(), json!(s));
        }
    }
    Json::Object(m)
}

struct Runner<'e> {
    engine: &'e Engine,
    ctx: Arc<RunCtx>,
}

impl<'e> Runner<'e> {
    /// Runs steps in order; after a failure the rest are reported skipped.
    async fn run_sequence(
        &self,
        frame: &Frame<'_>,
        steps: &[Step],
        cancel: &CancellationToken,
    ) -> (Vec<StepReport>, bool) {
        let mut reports = Vec::new();
        let mut failed = false;
        for step in steps {
            if failed || cancel.is_cancelled() {
                let reason = if cancel.is_cancelled() { "cancelled" } else { "an earlier step failed" };
                let r = self.skipped(frame, step, reason);
                self.ctx.emitter.emit(EventBody::StepFinished { step: Box::new(r.clone()) });
                reports.push(r);
                continue;
            }
            let r = self.run_step(frame, step, cancel).await;
            if r.status.is_some_and(|s| s.is_failure()) {
                if step.continue_on_error {
                    self.ctx.warn(format!(
                        "step {} failed (continueOnError): {}",
                        r.id,
                        r.headline.clone().unwrap_or_default()
                    ));
                } else {
                    failed = true;
                }
            }
            reports.push(r);
        }
        (reports, failed)
    }

    /// A skipped report for a step and all of its children.
    fn skipped(&self, frame: &Frame<'_>, step: &Step, reason: &str) -> StepReport {
        let id = frame.qualified(&step.id);
        let children = match step.kind() {
            StepKind::Group => step.children().iter().map(|c| self.skipped(frame, c, reason)).collect(),
            _ => vec![],
        };
        let r = StepReport {
            id,
            name: step.display_name().to_string(),
            kind: Some(step.kind()),
            status: Some(StepStatus::Skipped),
            error: Some(format!("skipped: {reason}")),
            notes: step.notes.clone(),
            children,
            ..Default::default()
        };
        frame.steps.lock().expect("lock").insert(step.id.clone(), step_json(&r, None));
        r
    }

    fn run_step<'a>(
        &'a self,
        frame: &'a Frame<'a>,
        step: &'a Step,
        cancel: &'a CancellationToken,
    ) -> BoxFuture<'a, StepReport> {
        async move {
            if let Some(cond) = &step.when {
                match expr::eval_bool(cond, &frame.scope(&self.ctx)) {
                    Ok(true) => {}
                    Ok(false) => {
                        let r = self.skipped(frame, step, &format!("when `{cond}` was false"));
                        self.ctx.emitter.emit(EventBody::StepFinished { step: Box::new(r.clone()) });
                        return r;
                    }
                    Err(e) => {
                        let mut r = self.base_report(frame, step);
                        r.status = Some(StepStatus::Errored);
                        r.error = Some(format!("when: {e}"));
                        return self.finish_step(frame, step, r, None);
                    }
                }
            }
            match step.kind() {
                StepKind::Group => self.run_group(frame, step, cancel).await,
                StepKind::Flow => self.run_call(frame, step, cancel).await,
                _ => self.run_leaf(frame, step, cancel).await,
            }
        }
        .boxed()
    }

    fn base_report(&self, frame: &Frame<'_>, step: &Step) -> StepReport {
        StepReport {
            id: frame.qualified(&step.id),
            name: step.display_name().to_string(),
            kind: Some(step.kind()),
            status: Some(StepStatus::Running),
            notes: step.notes.clone(),
            started_at: Some(now_rfc3339()),
            ..Default::default()
        }
    }

    /// Records the step's result in the frame, registers its cleanup,
    /// copies artifacts and emits `step_finished`.
    fn finish_step(
        &self,
        frame: &Frame<'_>,
        step: &Step,
        mut r: StepReport,
        outcome: Option<&AttemptOutcome>,
    ) -> StepReport {
        r.finished_at = Some(now_rfc3339());
        r.headline = headline(&r);
        let self_json = step_json(&r, outcome);
        if !step.artifacts.is_empty() && r.status != Some(StepStatus::Skipped) {
            let scope = frame.scope(&self.ctx).with("self", self_json.clone());
            for a in artifacts::collect(&step.artifacts, frame.flow.dir(), &scope, &self.ctx, &r.id) {
                match a {
                    Ok(art) => {
                        self.ctx.emitter.emit(EventBody::StepArtifact { step_id: r.id.clone(), artifact: art.clone() });
                        r.artifacts.push(art);
                    }
                    Err(e) => self.ctx.warn(format!("step {}: {e}", r.id)),
                }
            }
        }
        if let Some(c) = &step.cleanup
            && r.status != Some(StepStatus::Skipped)
        {
            let scope = frame.scope_with_secrets(&self.ctx).with("self", self_json.clone());
            let mut env = BTreeMap::new();
            let mut ok = true;
            for (k, t) in &c.env {
                match expr::interpolate(t, &scope) {
                    Ok(v) => {
                        env.insert(k.clone(), v);
                    }
                    Err(e) => {
                        ok = false;
                        self.ctx.warn(format!("cleanup for {} not registered: env {k}: {e}", r.id));
                    }
                }
            }
            if ok {
                self.ctx.cleanups.lock().expect("lock").push(PendingCleanup {
                    step_id: r.id.clone(),
                    command: c.run.clone(),
                    env,
                    policy: c.policy,
                    timeout: c.timeout.as_ref().and_then(|d| d.parse().ok()).unwrap_or(Duration::from_secs(120)),
                    cwd: frame.flow.dir().to_path_buf(),
                    base_vars: frame.process_env(&self.ctx),
                });
            }
        }
        frame.steps.lock().expect("lock").insert(step.id.clone(), step_json(&r, outcome));
        self.ctx.emitter.emit(EventBody::StepFinished { step: Box::new(r.clone()) });
        r
    }

    async fn run_group(&self, frame: &Frame<'_>, step: &Step, cancel: &CancellationToken) -> StepReport {
        let mut r = self.base_report(frame, step);
        let started = Instant::now();
        self.ctx.emitter.emit(EventBody::StepStarted { step_id: r.id.clone(), attempt: 1 });
        let children = step.children();
        let (reports, failed) = if step.parallel {
            let limit = step.max_parallel.unwrap_or(children.len().max(1));
            let mut futs: Vec<BoxFuture<'_, StepReport>> = Vec::with_capacity(children.len());
            for c in children {
                futs.push(self.run_step(frame, c, cancel));
            }
            let results: Vec<StepReport> = futures::stream::iter(futs).buffered(limit).collect().await;
            let failed = children
                .iter()
                .zip(&results)
                .any(|(c, r)| r.status.is_some_and(|s| s.is_failure()) && !c.continue_on_error);
            for (c, r) in children.iter().zip(&results) {
                if c.continue_on_error && r.status.is_some_and(|s| s.is_failure()) {
                    self.ctx.warn(format!("step {} failed (continueOnError)", r.id));
                }
            }
            (results, failed)
        } else {
            self.run_sequence(frame, children, cancel).await
        };
        r.duration_ms = started.elapsed().as_millis() as u64;
        r.status = Some(if failed {
            StepStatus::Failed
        } else if reports.iter().any(|c| c.status == Some(StepStatus::Passed)) {
            StepStatus::Passed
        } else {
            StepStatus::Skipped
        });
        if failed {
            let n = reports.iter().filter(|c| c.status.is_some_and(|s| s.is_failure())).count();
            r.error = Some(format!("{n} of {} steps failed", reports.len()));
        }
        r.children = reports;
        r.attempts = 1;
        self.finish_step(frame, step, r, None)
    }

    async fn run_call(&self, frame: &Frame<'_>, step: &Step, cancel: &CancellationToken) -> StepReport {
        let mut r = self.base_report(frame, step);
        let started = Instant::now();
        self.ctx.emitter.emit(EventBody::StepStarted { step_id: r.id.clone(), attempt: 1 });
        let target = step.flow.as_deref().unwrap_or_default();
        let Some(callee) = self.engine.catalog.get(target) else {
            r.status = Some(StepStatus::Errored);
            r.error = Some(format!("no flow '{target}'"));
            return self.finish_step(frame, step, r, None);
        };
        let scope = frame.scope(&self.ctx);
        let mut given = BTreeMap::new();
        for (k, t) in &step.with {
            match expr::interpolate(t, &scope) {
                Ok(v) => {
                    given.insert(k.clone(), v);
                }
                Err(e) => {
                    r.status = Some(StepStatus::Errored);
                    r.error = Some(format!("with.{k}: {e}"));
                    return self.finish_step(frame, step, r, None);
                }
            }
        }
        r.resolved = Some(format!("flow {target}"));
        let (inputs, vars) = match resolve_inputs_and_vars(&callee.flow, self.ctx.environment.as_deref(), &given) {
            Ok(v) => v,
            Err(e) => {
                r.status = Some(StepStatus::Errored);
                r.error = Some(e);
                return self.finish_step(frame, step, r, None);
            }
        };
        let child = Frame {
            flow: callee,
            prefix: format!("{}/", r.id),
            inputs,
            vars,
            steps: Mutex::new(serde_json::Map::new()),
        };
        let (mut reports, failed) = self.run_sequence(&child, &callee.flow.steps, cancel).await;
        let (mut fin, fin_failed) = self.run_sequence(&child, &callee.flow.finally, &self.ctx.cancel.cleanup).await;
        reports.append(&mut fin);
        r.children = reports;
        r.duration_ms = started.elapsed().as_millis() as u64;
        r.attempts = 1;
        if failed || fin_failed {
            r.status = Some(StepStatus::Failed);
            r.error = Some(format!("flow {target} failed"));
        } else if cancel.is_cancelled() {
            r.status = Some(StepStatus::Errored);
            r.error = Some("cancelled".into());
        } else {
            r.status = Some(StepStatus::Passed);
            let cscope = child.scope(&self.ctx);
            for (name, source) in &callee.flow.outputs {
                let value = if expr::has_template(source) {
                    expr::interpolate(source, &cscope).map(Json::String)
                } else {
                    expr::eval(source, &cscope)
                };
                match value {
                    Ok(v) => {
                        r.outputs.insert(name.clone(), self.ctx.redactor.redact_json(&v));
                    }
                    Err(e) => {
                        r.status = Some(StepStatus::Errored);
                        r.error = Some(format!("output {name}: {e}"));
                    }
                }
            }
        }
        self.finish_step(frame, step, r, None)
    }

    /// A step that does one thing, with retries.
    async fn run_leaf(&self, frame: &Frame<'_>, step: &Step, cancel: &CancellationToken) -> StepReport {
        let mut r = self.base_report(frame, step);
        let started = Instant::now();
        let retry = step.retry.as_ref();
        let max = retry.map(|r| r.attempts.max(1)).unwrap_or(1);
        let delay = retry.and_then(|r| r.delay.as_ref()).and_then(|d| d.parse().ok()).unwrap_or(Duration::from_secs(1));
        let mut last: Option<AttemptOutcome> = None;
        for attempt in 1..=max {
            r.attempts = attempt;
            self.ctx.emitter.emit(EventBody::StepStarted { step_id: r.id.clone(), attempt });
            let scope = frame.scope(&self.ctx);
            let mut outcome = steps::attempt(self, frame, step, &r.id, &scope, cancel).await;
            outcome.redact(&self.ctx.redactor);

            // Evaluate outputs, expect and until against this attempt.
            let mut self_json = outcome_json(&outcome, attempt);
            let self_scope = scope.with("self", self_json.clone());
            let mut outputs = BTreeMap::new();
            let mut output_error = None;
            for (name, source) in &step.outputs {
                match expr::eval(source, &self_scope) {
                    Ok(v) => {
                        outputs.insert(name.clone(), self.ctx.redactor.redact_json(&v));
                    }
                    Err(e) => {
                        output_error.get_or_insert(format!("output {name}: {e}"));
                    }
                }
            }
            outcome.outputs.extend(outputs);
            if let Json::Object(m) = &mut self_json {
                m.insert("outputs".into(), Json::Object(outcome.outputs.clone().into_iter().collect()));
            }
            let self_scope = scope.with("self", self_json);

            if !outcome.errored && !outcome.cancelled {
                if let Some(expect) = &step.expect {
                    outcome.passed = true;
                    outcome.error = None;
                    for e in expect.items() {
                        match expr::eval_bool(e, &self_scope) {
                            Ok(true) => {}
                            Ok(false) => {
                                outcome.passed = false;
                                let why =
                                    expr::explain_false(e, &self_scope).map(|w| format!(" ({w})")).unwrap_or_default();
                                outcome.error = Some(format!("expected `{e}`{why}"));
                                break;
                            }
                            Err(err) => {
                                outcome.passed = false;
                                outcome.errored = true;
                                outcome.error = Some(format!("expect: {err}"));
                                break;
                            }
                        }
                    }
                }
                if outcome.passed
                    && let Some(err) = output_error.take()
                {
                    outcome.passed = false;
                    outcome.errored = true;
                    outcome.error = Some(err);
                }
            }

            let mut retry_reason = None;
            if let Some(until) = retry.and_then(|r| r.until.as_ref()) {
                if !outcome.errored && !outcome.cancelled {
                    match expr::eval_bool(until, &self_scope) {
                        Ok(true) => {
                            outcome.passed = true;
                            outcome.error = None;
                        }
                        Ok(false) => {
                            outcome.passed = false;
                            retry_reason = Some(format!("`{until}` is not true yet"));
                            outcome.error = Some(format!(
                                "`{until}` was still false after {attempt} attempt{}",
                                if attempt == 1 { "" } else { "s" }
                            ));
                        }
                        // Fields may be missing until the system is ready.
                        Err(e) => {
                            outcome.passed = false;
                            retry_reason = Some(format!("until: {e}"));
                            outcome.error = Some(format!("until: {e}"));
                        }
                    }
                } else if !outcome.cancelled {
                    retry_reason = outcome.error.clone();
                }
            } else if !outcome.passed && !outcome.cancelled {
                retry_reason = Some(outcome.error.clone().unwrap_or_else(|| "attempt failed".into()));
            }

            let done = outcome.passed || outcome.cancelled || attempt == max || retry_reason.is_none();
            last = Some(outcome);
            if done {
                break;
            }
            self.ctx.emitter.emit(EventBody::StepRetrying {
                step_id: r.id.clone(),
                attempt,
                max_attempts: max,
                reason: retry_reason.unwrap_or_default(),
                next_attempt_in_ms: delay.as_millis() as u64,
            });
            tokio::select! {
                _ = tokio::time::sleep(delay) => {}
                _ = cancel.cancelled() => {}
            }
            if cancel.is_cancelled() {
                break;
            }
        }
        let o = last.unwrap_or_default();
        r.duration_ms = started.elapsed().as_millis() as u64;
        r.resolved = o.resolved.clone();
        r.exit_code = o.exit_code;
        r.stdout = o.stdout.clone();
        r.stderr = o.stderr.clone();
        r.http_status = o.http_status;
        r.outputs = o.outputs.clone();
        r.beats = o.beats.clone();
        r.artifacts = o.artifacts.clone();
        r.error = o.error.clone();
        r.status = Some(if o.cancelled {
            r.error = Some("cancelled".into());
            StepStatus::Errored
        } else if o.passed {
            StepStatus::Passed
        } else if o.errored {
            StepStatus::Errored
        } else {
            StepStatus::Failed
        });
        self.finish_step(frame, step, r, Some(&o))
    }

    async fn run_cleanups(&self, result: RunResult) -> Vec<CleanupReport> {
        let pending: Vec<PendingCleanup> = std::mem::take(&mut *self.ctx.cleanups.lock().expect("lock"));
        let mut out = Vec::new();
        for c in pending.into_iter().rev() {
            let redacted_env: BTreeMap<String, String> =
                c.env.iter().map(|(k, v)| (k.clone(), self.ctx.redactor.redact(v))).collect();
            let mut rep = CleanupReport {
                step_id: c.step_id.clone(),
                command: c.command.clone(),
                env: redacted_env,
                policy: c.policy,
                status: CleanupStatus::Pending,
                exit_code: None,
                output: String::new(),
                error: None,
            };
            let should_run = match c.policy {
                CleanupPolicy::Always => true,
                CleanupPolicy::OnFailure => result != RunResult::Passed,
                CleanupPolicy::Manual => false,
            };
            if !should_run || self.ctx.cancel.cleanup.is_cancelled() {
                rep.status = CleanupStatus::NotRun;
                if self.ctx.cancel.cleanup.is_cancelled() && should_run {
                    rep.error = Some("cleanup was abandoned".into());
                    self.ctx.warn(format!("cleanup for {} was abandoned", c.step_id));
                }
                out.push(rep);
                continue;
            }
            self.ctx.emitter.emit(EventBody::CleanupStarted { step_id: c.step_id.clone() });
            let (program, args) = crate::process::shell_command(Some("sh"), &c.command);
            let mut env = c.base_vars.clone();
            env.extend(c.env.clone());
            let res = crate::process::run(
                crate::process::ProcessSpec {
                    program,
                    args,
                    cwd: c.cwd.clone(),
                    env,
                    env_remove: vec![],
                    stdin: None,
                    timeout: c.timeout,
                    cap: 256 * 1024,
                },
                self.ctx.cancel.cleanup.clone(),
                |_, _| {},
            )
            .await;
            rep.exit_code = res.exit_code;
            rep.output = self.ctx.redactor.redact(&format!("{}{}", res.stdout, res.stderr));
            let ok = res.success();
            rep.status = if ok { CleanupStatus::Passed } else { CleanupStatus::Failed };
            if !ok {
                let why = res
                    .spawn_error
                    .clone()
                    .or_else(|| res.timed_out.then(|| "timed out".to_string()))
                    .unwrap_or_else(|| format!("exited {}", res.exit_code.unwrap_or(-1)));
                rep.error = Some(why.clone());
                self.ctx.warn(format!("cleanup for {} failed: {why}", c.step_id));
            }
            self.ctx.emitter.emit(EventBody::CleanupFinished {
                step_id: c.step_id.clone(),
                status: if ok { StepStatus::Passed } else { StepStatus::Failed },
            });
            out.push(rep);
        }
        out
    }
}

/// `self` for one attempt.
fn outcome_json(o: &AttemptOutcome, attempt: u32) -> Json {
    let status = if o.passed {
        "passed"
    } else if o.errored {
        "errored"
    } else {
        "failed"
    };
    let mut m = serde_json::Map::new();
    m.insert("status".into(), json!(o.http_status.map(|s| json!(s)).unwrap_or(json!(status))));
    m.insert("stdout".into(), json!(o.stdout));
    m.insert("stderr".into(), json!(o.stderr));
    m.insert("exitCode".into(), json!(o.exit_code));
    m.insert("durationMs".into(), json!(o.duration_ms));
    m.insert("error".into(), json!(o.error));
    m.insert("attempt".into(), json!(attempt));
    m.insert("json".into(), serde_json::from_str::<Json>(&o.stdout).unwrap_or(Json::Null));
    m.insert("body".into(), o.body.clone().unwrap_or(Json::Null));
    m.insert("headers".into(), Json::Object(o.headers.iter().map(|(k, v)| (k.clone(), json!(v))).collect()));
    m.insert("outputs".into(), Json::Object(o.outputs.clone().into_iter().collect()));
    Json::Object(m)
}

/// Emits a redacted output line, capping how many lines one step streams.
pub(crate) struct LineSink<'a> {
    ctx: &'a RunCtx,
    step_id: String,
    sent: usize,
}

const MAX_STREAMED_LINES: usize = 5000;

impl<'a> LineSink<'a> {
    pub(crate) fn new(ctx: &'a RunCtx, step_id: &str) -> Self {
        LineSink { ctx, step_id: step_id.to_string(), sent: 0 }
    }
    pub(crate) fn line(&mut self, stream: Stream, line: String) {
        self.sent += 1;
        if self.sent > MAX_STREAMED_LINES {
            if self.sent == MAX_STREAMED_LINES + 1 {
                self.ctx.emitter.emit(EventBody::StepOutput {
                    step_id: self.step_id.clone(),
                    stream,
                    line: "[lest: further output is in the report]".into(),
                });
            }
            return;
        }
        let line = if line.len() > 4000 {
            let mut cut = 4000;
            while !line.is_char_boundary(cut) {
                cut -= 1;
            }
            format!("{}…", &line[..cut])
        } else {
            line
        };
        self.ctx.emitter.emit(EventBody::StepOutput {
            step_id: self.step_id.clone(),
            stream,
            line: self.ctx.redactor.redact(&line),
        });
    }
}
