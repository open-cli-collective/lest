//! JSON API routes.

use std::collections::BTreeMap;
use std::convert::Infallible;
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::Json;
use axum::Router;
use axum::extract::{Path, Query, Request, State};
use axum::http::StatusCode;
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use futures::Stream;
use lest_core::catalog::{Catalog, Diagnostic, LoadedFlow};
use lest_core::event::{EventBody, PlanNode};
use lest_core::project::Project;
use lest_core::report::RunReport;
use lest_core::runner::{self, BrowserOptions, Engine, RunCancel, RunRequest};
use lest_core::spec::{Demo, Input, StepKind};
use lest_core::store::{RunSummary, new_run_id};
use lest_core::validate::validate_catalog;
use serde::{Deserialize, Serialize};
use serde_json::{Value as Json_, json};
use tokio::sync::mpsc;
use tokio_stream::StreamExt;
use tokio_stream::wrappers::BroadcastStream;
use tower_http::services::ServeFile;

use crate::{AppState, LiveRun, run_dir_of};

type AppResult<T> = Result<T, ApiError>;

pub(crate) struct ApiError(StatusCode, String);

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.0, Json(json!({"error": self.1}))).into_response()
    }
}

fn not_found(msg: impl Into<String>) -> ApiError {
    ApiError(StatusCode::NOT_FOUND, msg.into())
}

fn bad_request(msg: impl Into<String>) -> ApiError {
    ApiError(StatusCode::BAD_REQUEST, msg.into())
}

pub(crate) fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/state", get(state))
        .route("/flows", get(flows))
        .route("/flows/{id}", get(flow))
        .route("/runs", get(runs).post(start_run))
        .route("/runs/{id}", get(run))
        .route("/runs/{id}/cancel", post(cancel_run))
        .route("/runs/{id}/files/{*path}", get(run_file))
        .route("/runs/{id}/reveal", post(reveal))
        .route("/tools/{name}/login", post(tool_login))
        .route("/settings", get(settings))
        .route("/settings/ai", axum::routing::put(save_ai))
        .route("/runs/{id}/explain", post(explain))
        .route("/runs/{id}/handoff", get(handoff))
        .route("/runs/{id}/agent", post(launch_agent))
        .route("/events", get(events))
}

fn catalog(project: &Project) -> Catalog {
    let mut c = Catalog::load(project);
    validate_catalog(&mut c);
    c
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ProjectInfo {
    name: String,
    root: String,
    has_config: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct StateResponse {
    project: ProjectInfo,
    version: String,
    live_runs: Vec<String>,
}

async fn state(State(s): State<Arc<AppState>>) -> Json<StateResponse> {
    let p = s.project.read().expect("lock").clone();
    let live_runs = s.runs.lock().expect("lock").iter().filter(|(_, r)| !r.done).map(|(k, _)| k.clone()).collect();
    Json(StateResponse {
        project: ProjectInfo { name: p.name(), root: p.root.display().to_string(), has_config: p.has_config },
        version: env!("CARGO_PKG_VERSION").to_string(),
        live_runs,
    })
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct FlowSummary {
    id: String,
    name: String,
    description: Option<String>,
    tags: Vec<String>,
    path: String,
    /// The directory part of `path`, for grouping.
    folder: String,
    steps: Vec<PlanNode>,
    finally: Vec<PlanNode>,
    inputs: Vec<Input>,
    environments: Vec<String>,
    default_environment: Option<String>,
    demo: Option<Demo>,
    tools: Vec<String>,
    secrets: Vec<String>,
    services: Vec<String>,
    uses_browser: bool,
    records: bool,
    last_run: Option<RunSummary>,
}

fn plan(steps: &[lest_core::spec::Step], prefix: &str, catalog: &Catalog) -> Vec<PlanNode> {
    steps
        .iter()
        .map(|s| {
            let id = format!("{prefix}{}", s.id);
            let children = match s.kind() {
                StepKind::Group => plan(s.children(), prefix, catalog),
                StepKind::Flow => catalog
                    .get(s.flow.as_deref().unwrap_or_default())
                    .map(|c| {
                        let p = format!("{id}/");
                        let mut v = plan(&c.flow.steps, &p, catalog);
                        v.extend(plan(&c.flow.finally, &p, catalog));
                        v
                    })
                    .unwrap_or_default(),
                _ => vec![],
            };
            PlanNode { id, name: s.display_name().to_string(), kind: s.kind(), notes: s.notes.clone(), children }
        })
        .collect()
}

fn any_step(steps: &[lest_core::spec::Step], f: &dyn Fn(&lest_core::spec::Step) -> bool) -> bool {
    steps.iter().any(|s| f(s) || any_step(s.children(), f))
}

fn summarize(lf: &LoadedFlow, catalog: &Catalog, s: &AppState) -> FlowSummary {
    let f = &lf.flow;
    let folder = lf.rel_path.rsplit_once('/').map(|(d, _)| d.to_string()).unwrap_or_default();
    let all: Vec<&lest_core::spec::Step> = f.steps.iter().chain(f.finally.iter()).collect();
    let steps: Vec<lest_core::spec::Step> = all.into_iter().cloned().collect();
    FlowSummary {
        id: f.id.clone(),
        name: f.name.clone(),
        description: f.description.clone(),
        tags: f.tags.clone(),
        path: lf.rel_path.clone(),
        folder,
        steps: plan(&f.steps, "", catalog),
        finally: plan(&f.finally, "", catalog),
        inputs: f.inputs.clone(),
        environments: f.environments.keys().cloned().collect(),
        default_environment: f.default_environment.clone(),
        demo: f.demo.clone(),
        tools: f.tools.iter().map(|t| t.name().to_string()).collect(),
        secrets: f.secrets.clone(),
        services: f.services.iter().map(|s| s.id.clone()).collect(),
        uses_browser: any_step(&steps, &|s| s.browser.is_some()),
        records: any_step(&steps, &|s| s.browser.as_ref().is_some_and(|b| b.record)),
        last_run: s.store.latest(&f.id).map(|r| RunSummary::from(&r)),
    }
}

#[derive(Serialize)]
struct FlowsResponse {
    flows: Vec<FlowSummary>,
    diagnostics: Vec<Diagnostic>,
}

async fn flows(State(s): State<Arc<AppState>>) -> Json<FlowsResponse> {
    let project = s.project.read().expect("lock").clone();
    let s2 = s.clone();
    let out = tokio::task::spawn_blocking(move || {
        let c = catalog(&project);
        FlowsResponse {
            flows: c.flows.iter().map(|lf| summarize(lf, &c, &s2)).collect(),
            diagnostics: c.diagnostics.clone(),
        }
    })
    .await
    .expect("catalog task");
    Json(out)
}

#[derive(Serialize)]
struct FlowDetail {
    #[serde(flatten)]
    summary: FlowSummary,
    source: String,
    diagnostics: Vec<Diagnostic>,
}

async fn flow(State(s): State<Arc<AppState>>, Path(id): Path<String>) -> AppResult<Json<FlowDetail>> {
    let project = s.project.read().expect("lock").clone();
    let c = catalog(&project);
    let lf = c.get(&id).ok_or_else(|| not_found(format!("no flow {id}")))?;
    let source = std::fs::read_to_string(&lf.path).unwrap_or_default();
    let diagnostics = c.diagnostics.iter().filter(|d| d.file == lf.rel_path).cloned().collect();
    Ok(Json(FlowDetail { summary: summarize(lf, &c, &s), source, diagnostics }))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct StartRun {
    flow_id: String,
    environment: Option<String>,
    #[serde(default)]
    inputs: BTreeMap<String, String>,
    /// Resume from this top-level step, reusing `resume_run_id` (default:
    /// the latest run).
    from_step: Option<String>,
    resume_run_id: Option<String>,
    #[serde(default = "yes")]
    live_view: bool,
    #[serde(default)]
    headed: bool,
}

fn yes() -> bool {
    true
}

async fn start_run(State(s): State<Arc<AppState>>, Json(req): Json<StartRun>) -> AppResult<Json<Json_>> {
    let project = s.project.read().expect("lock").clone();
    let c = catalog(&project);
    let lf = c.get(&req.flow_id).ok_or_else(|| not_found(format!("no flow {}", req.flow_id)))?;
    let errors: Vec<String> = c
        .diagnostics
        .iter()
        .filter(|d| d.severity == lest_core::catalog::Severity::Error && d.flow.as_deref() == Some(&lf.flow.id))
        .map(|d| d.to_string())
        .collect();
    if !errors.is_empty() {
        return Err(bad_request(format!("the flow has validation errors: {}", errors.join("; "))));
    }
    let resume = match &req.from_step {
        None => None,
        Some(step) => {
            let old = match &req.resume_run_id {
                Some(id) => s.store.find(id).map_err(|e| not_found(e.to_string()))?,
                None => s.store.latest(&req.flow_id).ok_or_else(|| not_found("no earlier run to resume from"))?,
            };
            Some((old, step.clone()))
        }
    };
    let run_id = new_run_id();
    let cancel = RunCancel::default();
    s.runs.lock().expect("lock").insert(
        run_id.clone(),
        LiveRun {
            flow_id: req.flow_id.clone(),
            started_at: runner::now_rfc3339(),
            environment: req.environment.clone().or_else(|| lf.flow.default_environment.clone()),
            cancel: cancel.clone(),
            events: Vec::new(),
            done: false,
            started: Instant::now(),
        },
    );
    let engine = Engine {
        project,
        catalog: Arc::new(c),
        store: s.store.clone(),
        keyring: s.keyring.clone(),
        browser: Some(s.browser.clone()),
    };
    let request = RunRequest {
        flow_id: req.flow_id,
        environment: req.environment,
        inputs: req.inputs,
        resume,
        interactive: false,
        browser: BrowserOptions { headed: req.headed, live_view: req.live_view },
    };
    let (tx, mut rx) = mpsc::unbounded_channel::<lest_core::event::RunEvent>();
    let forward = {
        let s = s.clone();
        let run_id = run_id.clone();
        tokio::spawn(async move {
            while let Some(ev) = rx.recv().await {
                let done = matches!(ev.body, EventBody::RunFinished { .. });
                {
                    let mut runs = s.runs.lock().expect("lock");
                    if let Some(live) = runs.get_mut(&run_id) {
                        live.events.push(ev.clone());
                        live.done |= done;
                    }
                }
                let _ = s.events.send(Arc::new(ev));
            }
            prune(&s);
        })
    };
    let id = run_id.clone();
    let s2 = s.clone();
    tokio::spawn(async move {
        runner::execute_with_id(&engine, request, tx, cancel, id.clone()).await;
        let _ = forward.await;
        // However the run ended, it is no longer live.
        if let Some(live) = s2.runs.lock().expect("lock").get_mut(&id) {
            live.done = true;
        }
    });
    Ok(Json(json!({"runId": run_id})))
}

/// Finished runs are kept in memory briefly for late subscribers; their
/// reports are on disk.
fn prune(s: &AppState) {
    let mut runs = s.runs.lock().expect("lock");
    runs.retain(|_, r| !r.done || r.started.elapsed() < Duration::from_secs(600));
}

#[derive(Deserialize)]
struct CancelBody {
    /// `main` (default): stop the steps; `cleanup`: also abandon cleanup.
    stage: Option<String>,
}

async fn cancel_run(
    State(s): State<Arc<AppState>>,
    Path(id): Path<String>,
    body: Option<Json<CancelBody>>,
) -> AppResult<Json<Json_>> {
    let runs = s.runs.lock().expect("lock");
    let live = runs.get(&id).ok_or_else(|| not_found(format!("no running run {id}")))?;
    let stage = body.and_then(|b| b.0.stage).unwrap_or_else(|| "main".into());
    live.cancel.main.cancel();
    if stage == "cleanup" {
        live.cancel.cleanup.cancel();
    }
    Ok(Json(json!({"cancelled": stage})))
}

#[derive(Deserialize)]
struct RunsQuery {
    flow: Option<String>,
    limit: Option<usize>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct RunsResponse {
    running: Vec<Json_>,
    runs: Vec<RunSummary>,
}

async fn runs(State(s): State<Arc<AppState>>, Query(q): Query<RunsQuery>) -> Json<RunsResponse> {
    let running = s
        .runs
        .lock()
        .expect("lock")
        .iter()
        .filter(|(_, r)| !r.done && q.flow.as_ref().is_none_or(|f| f == &r.flow_id))
        .map(|(id, r)| json!({"runId": id, "flowId": r.flow_id, "startedAt": r.started_at, "environment": r.environment}))
        .collect();
    let store = s.store.clone();
    let runs = tokio::task::spawn_blocking(move || store.list(q.flow.as_deref(), q.limit.unwrap_or(100)))
        .await
        .unwrap_or_default();
    Json(RunsResponse { running, runs })
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct RunResponse {
    run_id: String,
    flow_id: String,
    done: bool,
    /// Events so far, for a run still in progress.
    events: Vec<lest_core::event::RunEvent>,
    report: Option<RunReport>,
}

async fn run(State(s): State<Arc<AppState>>, Path(id): Path<String>) -> AppResult<Json<RunResponse>> {
    {
        let runs = s.runs.lock().expect("lock");
        if let Some(live) = runs.get(&id)
            && !live.done
        {
            return Ok(Json(RunResponse {
                run_id: id,
                flow_id: live.flow_id.clone(),
                done: false,
                events: live.events.clone(),
                report: None,
            }));
        }
    }
    let r = s.store.find(&id).map_err(|e| not_found(e.to_string()))?;
    Ok(Json(RunResponse {
        run_id: r.run_id.clone(),
        flow_id: r.flow_id.clone(),
        done: true,
        events: vec![],
        report: Some(r),
    }))
}

async fn run_file(
    State(s): State<Arc<AppState>>,
    Path((id, path)): Path<(String, String)>,
    req: Request,
) -> AppResult<Response> {
    let dir = run_dir_of(&s, &id).ok_or_else(|| not_found(format!("no run {id}")))?;
    let dir = std::fs::canonicalize(&dir).map_err(|_| not_found("run directory is gone"))?;
    let file = std::fs::canonicalize(dir.join(&path)).map_err(|_| not_found(format!("no file {path}")))?;
    // Only files inside the run directory, never through `..` or links out.
    if !file.starts_with(&dir) || !file.is_file() {
        return Err(not_found(format!("no file {path}")));
    }
    let mime = lest_core::runner::mime_for_name(&path);
    let resp = ServeFile::new_with_mime(&file, &mime.parse().expect("mime")).try_call(req).await;
    let mut resp = match resp {
        Ok(r) => r.map(axum::body::Body::new),
        Err(e) => return Err(ApiError(StatusCode::INTERNAL_SERVER_ERROR, e.to_string())),
    };
    // Run files are untrusted content from the system under test: they run
    // in a sandbox with no scripts and no access to the UI's origin, and
    // anything that could execute downloads instead of rendering.
    let h = resp.headers_mut();
    h.insert(
        axum::http::header::CONTENT_SECURITY_POLICY,
        axum::http::HeaderValue::from_static(
            "sandbox; default-src 'none'; img-src 'self' data:; media-src 'self'; style-src 'unsafe-inline'",
        ),
    );
    let ext = std::path::Path::new(&path).extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
    if matches!(ext.as_str(), "html" | "htm" | "svg" | "js" | "mjs" | "xhtml" | "xml") {
        h.insert(axum::http::header::CONTENT_DISPOSITION, axum::http::HeaderValue::from_static("attachment"));
    }
    Ok(resp)
}

#[derive(Deserialize)]
struct RevealBody {
    path: Option<String>,
}

/// Shows a run's file (or its folder) in the system file manager.
async fn reveal(
    State(s): State<Arc<AppState>>,
    Path(id): Path<String>,
    body: Option<Json<RevealBody>>,
) -> AppResult<Json<Json_>> {
    let dir = run_dir_of(&s, &id).ok_or_else(|| not_found(format!("no run {id}")))?;
    let target = match body.and_then(|b| b.0.path) {
        Some(p) => {
            let full = std::fs::canonicalize(dir.join(&p)).map_err(|_| not_found(format!("no file {p}")))?;
            if !full.starts_with(std::fs::canonicalize(&dir).unwrap_or(dir.clone())) {
                return Err(not_found(format!("no file {p}")));
            }
            full
        }
        None => dir,
    };
    crate::terminal::reveal(&target).map_err(|e| ApiError(StatusCode::INTERNAL_SERVER_ERROR, e))?;
    Ok(Json(json!({"revealed": target.display().to_string()})))
}

/// Opens a terminal running the tool's sign-in command, since sign-ins
/// prompt for input or open a browser.
async fn tool_login(State(s): State<Arc<AppState>>, Path(name): Path<String>) -> AppResult<Json<Json_>> {
    let project = s.project.read().expect("lock").clone();
    let profile = project.config.tools.get(&name);
    let login = profile
        .and_then(|p| p.login.clone())
        .ok_or_else(|| not_found(format!("tool {name} has no login command in lest.yaml")))?;
    // The profile's pins (a profile name, an environment), as the CLI
    // applies them; pins that need a run's vars are left out.
    let mut scope = lest_core::expr::Scope::new();
    scope.set("vars", json!({}));
    scope.set("run", json!({"environment": null}));
    let mut prefix = String::new();
    for (k, t) in profile.map(|p| p.env.clone()).unwrap_or_default() {
        if let Ok(v) = lest_core::expr::interpolate(&t, &scope)
            && !v.is_empty()
        {
            prefix.push_str(&format!("{k}='{}' ", v.replace('\'', "'\\''")));
        }
    }
    let login = format!("{prefix}{login}");
    match crate::terminal::open(&login, &project.root) {
        Ok(how) => Ok(Json(json!({"launched": true, "how": how, "command": login}))),
        Err(e) => Ok(Json(json!({"launched": false, "error": e, "command": login}))),
    }
}

fn user_config(s: &AppState) -> AppResult<lest_core::ai::UserConfig> {
    lest_core::ai::UserConfig::load(&s.roots.config_file())
        .map_err(|e| ApiError(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))
}

#[derive(Serialize)]
struct SettingsResponse {
    ai: lest_core::ai::AiConfig,
    status: lest_core::ai::Status,
}

async fn settings(State(s): State<Arc<AppState>>) -> AppResult<Json<SettingsResponse>> {
    let cfg = user_config(&s)?;
    let status = lest_core::ai::status(&cfg.ai, &s.roots.data_dir);
    Ok(Json(SettingsResponse { ai: cfg.ai, status }))
}

async fn save_ai(
    State(s): State<Arc<AppState>>,
    Json(ai): Json<lest_core::ai::AiConfig>,
) -> AppResult<Json<SettingsResponse>> {
    let mut cfg = user_config(&s)?;
    cfg.ai = ai;
    cfg.save(&s.roots.config_file()).map_err(|e| ApiError(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    settings(State(s)).await
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct StepBody {
    step_id: Option<String>,
}

fn stored_report(s: &AppState, id: &str) -> AppResult<RunReport> {
    s.store.find(id).map_err(|e| not_found(e.to_string()))
}

/// A failed step's explanation: the deterministic headline, or a model's
/// when the user configured one. Never blocks a run; computed on request.
async fn explain(
    State(s): State<Arc<AppState>>,
    Path(id): Path<String>,
    body: Option<Json<StepBody>>,
) -> AppResult<Json<lest_core::ai::Explanation>> {
    let report = stored_report(&s, &id)?;
    let step_id = match body.and_then(|b| b.0.step_id) {
        Some(id) => id,
        None => report.first_failure().map(|f| f.id.clone()).ok_or_else(|| not_found("the run has no failed step"))?,
    };
    let cfg = user_config(&s)?;
    let data_dir = s.roots.data_dir.clone();
    let source = std::fs::read_to_string(std::path::Path::new(&report.project_dir).join(&report.flow_path)).ok();
    let remove = lest_core::secrets::secret_env_names(&s.project.read().expect("lock").secrets_config());
    let e = tokio::task::spawn_blocking(move || {
        lest_core::ai::explain(&cfg.ai, &data_dir, &report, &step_id, source.as_deref(), &remove)
    })
    .await
    .map_err(|e| ApiError(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok(Json(e))
}

#[derive(Deserialize)]
struct StepQuery {
    step: Option<String>,
}

async fn handoff(
    State(s): State<Arc<AppState>>,
    Path(id): Path<String>,
    Query(q): Query<StepQuery>,
) -> AppResult<Json<lest_core::ai::Handoff>> {
    let report = stored_report(&s, &id)?;
    let cfg = user_config(&s)?;
    Ok(Json(lest_core::ai::handoff(&cfg.ai, &report, q.step.as_deref())))
}

/// Opens the user's agent CLI in a terminal, in the project, with the
/// failure's context. The command is built here, never taken from the
/// request.
async fn launch_agent(
    State(s): State<Arc<AppState>>,
    Path(id): Path<String>,
    body: Option<Json<StepBody>>,
) -> AppResult<Json<Json_>> {
    let report = stored_report(&s, &id)?;
    let cfg = user_config(&s)?;
    let h = lest_core::ai::handoff(&cfg.ai, &report, body.and_then(|b| b.0.step_id).as_deref());
    let Some(command) = h.command else {
        return Err(bad_request(
            "no agent is configured: set Agent command in Settings, or `lest config set ai.agent '<cli> {prompt}'`",
        ));
    };
    let cwd = std::path::PathBuf::from(&report.project_dir);
    match crate::terminal::open(&command, &cwd) {
        Ok(how) => Ok(Json(json!({"launched": true, "how": how, "command": command}))),
        Err(e) => Ok(Json(json!({"launched": false, "error": e, "command": command}))),
    }
}

async fn events(State(s): State<Arc<AppState>>) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let rx = s.events.subscribe();
    let stream = BroadcastStream::new(rx).map(|item| {
        Ok(match item {
            Ok(ev) => Event::default().event("run").json_data(ev.as_ref()).unwrap_or_else(|_| Event::default()),
            // A slow client missed events; it should reload what it shows.
            Err(_) => Event::default().event("resync").data("{}"),
        })
    });
    Sse::new(stream).keep_alive(KeepAlive::new().interval(Duration::from_secs(15)))
}
