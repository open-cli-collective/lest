mod args;
mod render;

use std::collections::{BTreeMap, BTreeSet};
use std::io::{IsTerminal, Read, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;

use anyhow::{Context as _, Result};
use clap::Parser;
use futures::StreamExt;
use lest_core::catalog::{Catalog, Severity};
use lest_core::paths::StateRoots;
use lest_core::project::{PROJECT_FILE, Project};
use lest_core::report::{CleanupStatus, RunReport, RunResult};
use lest_core::runner::{self, Engine, RunCancel, RunRequest};
use lest_core::secrets::{Keyring, Resolver, SystemKeyring};
use lest_core::store::Store;
use lest_core::validate::validate_catalog;
use tokio::sync::mpsc;

use args::*;
use render::{Progress, Style};

/// Exit codes: 0 passed, 1 failed, 2 usage or invalid flow, 3 could not run
/// (preflight, auth, configuration), 4 not found, 130 cancelled.
mod exit {
    pub const FAILED: u8 = 1;
    pub const USAGE: u8 = 2;
    pub const ERRORED: u8 = 3;
    pub const NOT_FOUND: u8 = 4;
    pub const CANCELLED: u8 = 130;
}

/// An error that carries its exit code.
#[derive(Debug)]
struct Exit(u8, String);
impl std::fmt::Display for Exit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.1)
    }
}
impl std::error::Error for Exit {}

fn fail(code: u8, msg: impl Into<String>) -> anyhow::Error {
    anyhow::Error::new(Exit(code, msg.into()))
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let style = Style::detect(cli.no_color);
    let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
    match rt.block_on(dispatch(cli, style)) {
        Ok(code) => ExitCode::from(code),
        Err(e) => {
            let code = e.downcast_ref::<Exit>().map(|x| x.0).unwrap_or(exit::FAILED);
            eprintln!("{} {e:#}", style.red("error:"));
            ExitCode::from(code)
        }
    }
}

struct Ctx {
    project: Project,
    cwd: PathBuf,
    roots: StateRoots,
}

impl Ctx {
    fn new(project: Option<&Path>) -> Result<Ctx> {
        let cwd = std::env::current_dir()?;
        let project = match project {
            Some(p) => Project::discover(p)?,
            None => Project::discover(&cwd)?,
        };
        Ok(Ctx { project, cwd, roots: StateRoots::standard()? })
    }

    fn catalog(&self) -> Catalog {
        let mut c = Catalog::load(&self.project);
        validate_catalog(&mut c);
        c
    }

    fn store(&self) -> Store {
        Store::new(self.roots.clone())
    }
}

async fn dispatch(cli: Cli, style: Style) -> Result<u8> {
    let Some(command) = cli.command else {
        use clap::CommandFactory;
        Cli::command().print_help()?;
        return Ok(0);
    };
    let ctx = Ctx::new(cli.project.as_deref())?;
    match command {
        Command::Run(a) => cmd_run(&ctx, a, style).await,
        Command::List(a) => cmd_list(&ctx, a),
        Command::Get(a) => cmd_get(&ctx, a),
        Command::Validate(a) => cmd_validate(&ctx, a, style),
        Command::Runs { command } => cmd_runs(&ctx, command, style),
        Command::Cleanup(a) => cmd_cleanup(&ctx, a, style).await,
        Command::Affected(a) => cmd_affected(&ctx, a),
        Command::Bundle(a) => cmd_bundle(&ctx, a),
        Command::Init(a) => cmd_init(&ctx, a),
        Command::Doctor => cmd_doctor(&ctx, style).await,
        Command::Secrets { command } => cmd_secrets(&ctx, command),
        Command::Schema { kind } => {
            let schema = match kind {
                SchemaKind::Flow => lest_core::spec::json_schema(),
                SchemaKind::Project => lest_core::project::project_schema(),
                SchemaKind::Report => lest_core::report::report_schema(),
            };
            println!("{}", serde_json::to_string_pretty(&schema)?);
            Ok(0)
        }
        Command::Data { command } => cmd_data(&ctx, command),
        Command::Ui(a) => cmd_ui(&ctx, a).await,
        Command::Explain(a) => cmd_explain(&ctx, a, false),
        Command::Context(a) => cmd_explain(&ctx, a, true),
        Command::Docs { command: DocsCommand::Agent } => {
            print!("{}", lest_core::docs::agent_guide());
            Ok(0)
        }
        Command::Config { command } => cmd_config(&ctx, command),
    }
}

/// Waits for Ctrl-C, or SIGTERM or SIGHUP (a closed terminal, a CI job
/// being stopped), which are treated the same way.
async fn interrupted() -> bool {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        let (Ok(mut term), Ok(mut hup)) = (signal(SignalKind::terminate()), signal(SignalKind::hangup())) else {
            return tokio::signal::ctrl_c().await.is_ok();
        };
        tokio::select! {
            r = tokio::signal::ctrl_c() => r.is_ok(),
            _ = term.recv() => true,
            _ = hup.recv() => true,
        }
    }
    #[cfg(not(unix))]
    {
        tokio::signal::ctrl_c().await.is_ok()
    }
}

fn parse_inputs(raw: &[String]) -> Result<BTreeMap<String, String>> {
    let mut out = BTreeMap::new();
    for item in raw {
        let Some((k, v)) = item.split_once('=') else {
            return Err(fail(exit::USAGE, format!("--input expects NAME=VALUE, got '{item}'")));
        };
        out.insert(k.trim().to_string(), v.to_string());
    }
    Ok(out)
}

/// Fails with the validation errors of the flow and every flow it calls.
fn check_runnable(catalog: &Catalog, flow_id: &str, style: Style) -> Result<()> {
    let mut ids = BTreeSet::new();
    let mut queue = vec![flow_id.to_string()];
    while let Some(id) = queue.pop() {
        if !ids.insert(id.clone()) {
            continue;
        }
        if let Some(f) = catalog.get(&id) {
            fn calls(steps: &[lest_core::spec::Step], out: &mut Vec<String>) {
                for s in steps {
                    if let Some(f) = &s.flow {
                        out.push(f.clone());
                    }
                    calls(s.children(), out);
                }
            }
            calls(&f.flow.steps, &mut queue);
            calls(&f.flow.finally, &mut queue);
        }
    }
    let errors: Vec<_> = catalog
        .diagnostics
        .iter()
        .filter(|d| d.severity == Severity::Error && d.flow.as_ref().is_some_and(|f| ids.contains(f)))
        .collect();
    if errors.is_empty() {
        return Ok(());
    }
    for d in &errors {
        eprintln!("{} {d}", style.red("✗"));
    }
    Err(fail(exit::USAGE, format!("{flow_id} has {} validation error(s)", errors.len())))
}

/// Browser steps run through the Node harness.
fn browser_driver(
    project: &Project,
    roots: &StateRoots,
    allow_origin: Option<String>,
) -> Arc<dyn runner::BrowserDriver> {
    let cfg = project.config.browser.clone().unwrap_or_default();
    Arc::new(lest_core::browser::NodeBrowser {
        node: cfg.node.unwrap_or_else(|| "node".to_string()),
        harness_dir: roots.data_dir.join("harness"),
        args: cfg.args,
        allow_origin,
    })
}

fn keyring() -> Arc<dyn Keyring> {
    Arc::new(SystemKeyring)
}

/// The flows one `lest run` selected, in run order, with the project and
/// catalog they resolve against.
struct Selection {
    project: Project,
    catalog: Catalog,
    ids: Vec<String>,
}

/// Fails with every validation error in the catalog: a flow that does not
/// parse cannot be selected, and leaving it (and its callers) out quietly
/// would hide it.
fn require_valid(catalog: &Catalog) -> Result<()> {
    let broken: Vec<_> = catalog.diagnostics.iter().filter(|d| d.severity == Severity::Error).collect();
    if broken.is_empty() {
        return Ok(());
    }
    for d in &broken {
        eprintln!("{d}");
    }
    Err(fail(exit::USAGE, format!("{} validation error(s); fix them before selecting flows", broken.len())))
}

/// Resolves the named flows, then the `--tag` and `--affected` selections,
/// and checks every selected flow before any of them runs.
fn select_flows(ctx: &Ctx, a: &RunArgs, style: Style) -> Result<Selection> {
    if a.flows.is_empty() && a.tags.is_empty() && a.affected.is_none() {
        return Err(fail(exit::USAGE, "name a flow, or select flows with --tag or --affected"));
    }
    // A path outside the discovered project: use that file's project.
    let mut project = ctx.project.clone();
    if let Some(file) = a.flows.iter().map(|f| ctx.cwd.join(f)).find(|p| p.is_file()) {
        project = Project::discover(&file)?;
    }
    let mut catalog = Catalog::load(&project);
    for f in &a.flows {
        let candidate = ctx.cwd.join(f);
        if candidate.is_file()
            && lest_core::catalog::is_flow_file(&candidate)
            && catalog.resolve(&project, &ctx.cwd, f).is_err()
        {
            catalog.load_file(&project, &candidate);
        }
    }
    validate_catalog(&mut catalog);

    let mut ids: Vec<String> = Vec::new();
    let mut add = |id: &str| {
        if !ids.iter().any(|x| x == id) {
            ids.push(id.to_string());
        }
    };
    let mut missing = Vec::new();
    for f in &a.flows {
        match catalog.resolve(&project, &ctx.cwd, f) {
            Ok(lf) => add(&lf.flow.id),
            Err(e) => missing.push(e),
        }
    }
    if !missing.is_empty() {
        // A file that does not parse never makes it into the catalog;
        // show why instead of only "no flow".
        for d in catalog.diagnostics.iter().filter(|d| d.flow.is_none() && d.severity == Severity::Error) {
            eprintln!("{} {d}", style.red("✗"));
        }
        return Err(fail(exit::NOT_FOUND, missing.join("; ")));
    }
    for t in &a.tags {
        let tagged: Vec<&str> =
            catalog.flows.iter().filter(|f| f.flow.tags.contains(t)).map(|f| f.flow.id.as_str()).collect();
        if tagged.is_empty() {
            return Err(fail(exit::NOT_FOUND, format!("no flow has the tag {t}")));
        }
        tagged.into_iter().for_each(&mut add);
    }
    if let Some(base) = &a.affected {
        require_valid(&catalog)?;
        let changed = lest_core::affected::changed_since(&project.root, base).map_err(|e| fail(exit::ERRORED, e))?;
        let affected = lest_core::affected::affected(&catalog, &changed).map_err(|e| fail(exit::USAGE, e))?;
        eprintln!("{} changed file(s), {} affected flow(s)", changed.len(), affected.len());
        for f in &affected {
            add(&f.flow_id);
        }
    }
    for id in &ids {
        check_runnable(&catalog, id, style)?;
    }
    Ok(Selection { project, catalog, ids })
}

/// Ctrl-C (or SIGTERM, SIGHUP) cancels the runs: once to cancel, twice to
/// skip cleanup, three times to exit now.
fn handle_interrupts(cancel: &RunCancel) {
    let cancel = cancel.clone();
    tokio::spawn(async move {
        let mut presses = 0;
        while interrupted().await {
            presses += 1;
            match presses {
                1 => {
                    eprintln!("\nlest: cancelling; finally steps and cleanups still run (Ctrl-C again to skip them)");
                    cancel.main.cancel();
                }
                2 => {
                    eprintln!("\nlest: skipping cleanup (Ctrl-C again to exit now)");
                    cancel.cleanup.cancel();
                }
                _ => {
                    // A hard exit skips cleanup; still stop services.
                    lest_core::services::kill_all();
                    std::process::exit(exit::CANCELLED as i32)
                }
            }
        }
    });
}

/// Runs one flow, printing its progress unless `quiet`, and returns the
/// report and where it was written.
async fn run_one(
    engine: &Engine,
    req: RunRequest,
    cancel: RunCancel,
    style: Style,
    quiet: bool,
) -> (RunReport, String) {
    let (tx, mut rx) = mpsc::unbounded_channel::<lest_core::event::RunEvent>();
    let printer = tokio::spawn(async move {
        let mut progress = Progress::new(style, quiet);
        let mut report_path = String::new();
        while let Some(ev) = rx.recv().await {
            if let lest_core::event::EventBody::RunFinished { report_path: p, .. } = &ev.body {
                report_path = p.clone();
            }
            progress.event(&ev);
        }
        report_path
    });
    let report = runner::execute(engine, req, tx, cancel).await;
    (report, printer.await.unwrap_or_default())
}

fn exit_code(result: RunResult) -> u8 {
    match result {
        RunResult::Passed => 0,
        RunResult::Failed => exit::FAILED,
        RunResult::Errored => exit::ERRORED,
        RunResult::Cancelled => exit::CANCELLED,
    }
}

/// The exit code of several runs: the worst one, where cancelled (130)
/// outranks could not run (3), which outranks failed (1).
fn worst_exit(codes: impl IntoIterator<Item = u8>) -> u8 {
    let rank = |c: u8| match c {
        exit::CANCELLED => 3,
        exit::ERRORED => 2,
        exit::FAILED => 1,
        _ => 0,
    };
    codes.into_iter().max_by_key(|&c| rank(c)).unwrap_or(0)
}

async fn cmd_run(ctx: &Ctx, a: RunArgs, style: Style) -> Result<u8> {
    // Several flows, or a --tag or --affected selection whatever it matches,
    // turn -o into a directory and --junit into one file of several suites.
    let single = a.flows.len() == 1 && a.tags.is_empty() && a.affected.is_none();
    if !single && a.from.is_some() {
        return Err(fail(exit::USAGE, "--from resumes one flow; name a single flow"));
    }
    let Selection { project, catalog, ids } = select_flows(ctx, &a, style)?;
    let inputs = parse_inputs(&a.inputs)?;
    let declares = |id: &str, name: &str| catalog.get(id).is_some_and(|f| f.flow.inputs.iter().any(|i| i.name == name));
    if !single {
        // Each flow gets the inputs it declares; one that no selected flow
        // declares is a mistake, not something to drop silently.
        for k in inputs.keys() {
            if !ids.iter().any(|id| declares(id, k)) {
                return Err(fail(exit::USAGE, format!("no selected flow has an input '{k}'")));
            }
        }
    }

    let store = ctx.store();
    let mut resume = match &a.from {
        None => None,
        Some(step) => {
            let old = match &a.resume_run {
                Some(id) => store.find(id)?,
                None => store
                    .latest(&ids[0])
                    .ok_or_else(|| fail(exit::NOT_FOUND, format!("no earlier run of {} to resume from", ids[0])))?,
            };
            Some((old, step.clone()))
        }
    };
    // Concurrent runs cannot share the terminal for a sign-in.
    let interactive = !a.non_interactive && a.max_parallel == 1 && std::io::stdin().is_terminal();
    let reqs: Vec<RunRequest> = ids
        .iter()
        .map(|id| RunRequest {
            flow_id: id.clone(),
            environment: a.environment.clone(),
            inputs: inputs
                .iter()
                .filter(|(k, _)| single || declares(id, k))
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect(),
            resume: resume.take(),
            interactive,
            browser: runner::BrowserOptions { headed: a.headed, live_view: false },
        })
        .collect();
    let engine = Engine {
        browser: Some(browser_driver(&project, &ctx.roots, None)),
        project,
        catalog: Arc::new(catalog),
        store: store.clone(),
        keyring: keyring(),
    };

    if ids.is_empty() {
        eprintln!("no flows selected");
    }
    let cancel = RunCancel::default();
    handle_interrupts(&cancel);
    // Step lines from concurrent runs would interleave: above one at a time,
    // each run prints only its result.
    let quiet_steps = a.quiet || a.max_parallel > 1;
    let mut finished: Vec<(usize, Option<(RunReport, String)>)> =
        futures::stream::iter(reqs.into_iter().enumerate().map(|(i, req)| {
            let (engine, cancel, quiet) = (&engine, cancel.clone(), a.quiet);
            async move {
                // Flows still queued when the user cancels do not start.
                if cancel.main.is_cancelled() {
                    return (i, None);
                }
                let (report, path) = run_one(engine, req, cancel, style, quiet_steps).await;
                if !quiet {
                    render::summary(style, &report, &path);
                }
                println!(
                    "{} {} {} {}",
                    render::result_word(style, report.result),
                    report.flow_id,
                    render::duration(report.duration_ms),
                    report.run_id
                );
                (i, Some((report, path)))
            }
        }))
        .buffer_unordered(a.max_parallel as usize)
        .collect()
        .await;
    finished.sort_by_key(|(i, _)| *i);
    let reports: Vec<&RunReport> = finished.iter().filter_map(|(_, r)| r.as_ref().map(|r| &r.0)).collect();
    let not_run: Vec<&str> = finished.iter().filter(|(_, r)| r.is_none()).map(|(i, _)| ids[*i].as_str()).collect();

    if !single && !a.quiet && !ids.is_empty() {
        print!("{}", render::runs_table(&reports, &not_run));
    }
    if let Some(out) = &a.output {
        if single {
            if let Some(r) = reports.first() {
                std::fs::write(out, serde_json::to_vec_pretty(r)?)
                    .with_context(|| format!("writing {}", out.display()))?;
            }
        } else {
            std::fs::create_dir_all(out).with_context(|| format!("creating {}", out.display()))?;
            for r in &reports {
                let path = out.join(format!("{}.json", r.flow_id));
                std::fs::write(&path, serde_json::to_vec_pretty(r)?)
                    .with_context(|| format!("writing {}", path.display()))?;
            }
        }
    }
    if let Some(out) = &a.junit {
        let xml = match reports.first() {
            Some(r) if single => render::junit(r),
            _ => render::junit_suites(&reports),
        };
        std::fs::write(out, xml).with_context(|| format!("writing {}", out.display()))?;
    }
    if let Some(path) = std::env::var_os("GITHUB_STEP_SUMMARY").filter(|p| !p.is_empty()) {
        let written = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .and_then(|mut f| f.write_all(render::step_summary(&reports, &not_run).as_bytes()));
        if let Err(e) = written {
            eprintln!("{} cannot write the GitHub step summary: {e}", style.yellow("warning:"));
        }
    }
    let codes = reports.iter().map(|r| exit_code(r.result));
    Ok(worst_exit(codes.chain((!not_run.is_empty()).then_some(exit::CANCELLED))))
}

fn cmd_list(ctx: &Ctx, a: ListArgs) -> Result<u8> {
    let catalog = ctx.catalog();
    let store = ctx.store();
    let needle = a.name.as_deref().map(str::to_lowercase);
    let flows: Vec<_> = catalog
        .flows
        .iter()
        .filter(|f| a.tag.as_ref().is_none_or(|t| f.flow.tags.contains(t)))
        .filter(|f| {
            needle.as_ref().is_none_or(|n| f.flow.name.to_lowercase().contains(n) || f.flow.id.contains(n.as_str()))
        })
        .collect();
    if a.id {
        for f in flows {
            println!("{}", f.flow.id);
        }
        return Ok(0);
    }
    if a.json {
        let items: Vec<_> = flows
            .iter()
            .map(|f| serde_json::json!({"id": f.flow.id, "name": f.flow.name, "path": f.rel_path, "tags": f.flow.tags}))
            .collect();
        println!("{}", serde_json::Value::Array(items));
        return Ok(0);
    }
    let rows: Vec<Vec<String>> = flows
        .iter()
        .map(|f| {
            let last = store
                .latest(&f.flow.id)
                .map(|r| format!("{} {}", r.result.as_str(), r.started_at.get(..10).unwrap_or("")))
                .unwrap_or_default();
            vec![
                f.flow.id.clone(),
                f.flow.name.clone(),
                f.flow.steps.len().to_string(),
                f.flow.tags.join(","),
                last,
                f.rel_path.clone(),
            ]
        })
        .collect();
    print!("{}", render::table(&["ID", "NAME", "STEPS", "TAGS", "LAST_RUN", "PATH"], &rows));
    let errors = catalog.diagnostics.iter().filter(|d| d.severity == Severity::Error).count();
    if errors > 0 {
        eprintln!("{errors} validation error(s); run `lest validate`");
    }
    Ok(0)
}

fn cmd_get(ctx: &Ctx, a: GetArgs) -> Result<u8> {
    let catalog = ctx.catalog();
    let lf = catalog.resolve(&ctx.project, &ctx.cwd, &a.flow).map_err(|e| fail(exit::NOT_FOUND, e))?;
    let f = &lf.flow;
    println!("{}  {}", f.id, f.name);
    println!("Path: {}", lf.rel_path);
    if !f.tags.is_empty() {
        println!("Tags: {}", f.tags.join(", "));
    }
    if !f.environments.is_empty() {
        println!(
            "Environments: {}{}",
            f.environments.keys().cloned().collect::<Vec<_>>().join(", "),
            f.default_environment.as_deref().map(|d| format!(" (default {d})")).unwrap_or_default()
        );
    }
    for i in &f.inputs {
        let choices = if i.choices.is_empty() {
            String::new()
        } else {
            format!(" [{}]", i.choices.iter().map(|c| c.value.as_str()).collect::<Vec<_>>().join("|"))
        };
        let default = i.default.as_ref().map(|d| format!(" = {}", d.as_env_string())).unwrap_or_default();
        let mark = match (i.required, i.advanced) {
            (true, true) => " (required, advanced)",
            (true, false) => " (required)",
            (false, true) => " (advanced)",
            (false, false) => "",
        };
        println!("Input: {}{choices}{default}{mark}", i.name);
    }
    if !f.tools.is_empty() {
        println!("Tools: {}", f.tools.iter().map(|t| t.name()).collect::<Vec<_>>().join(", "));
    }
    if !f.secrets.is_empty() {
        println!("Secrets: {}", f.secrets.join(", "));
    }
    println!("Steps:");
    fn steps(list: &[lest_core::spec::Step], depth: usize) {
        for s in list {
            println!("{}{} ({}) {}", "  ".repeat(depth + 1), s.id, s.kind().as_str(), s.name.as_deref().unwrap_or(""));
            steps(s.children(), depth + 1);
        }
    }
    steps(&f.steps, 0);
    if !f.finally.is_empty() {
        println!("Finally:");
        steps(&f.finally, 0);
    }
    if let Some(d) = &f.description {
        println!("Description: {}", d.trim());
    }
    Ok(0)
}

fn cmd_validate(ctx: &Ctx, a: ValidateArgs, style: Style) -> Result<u8> {
    let mut catalog = Catalog::load(&ctx.project);
    for p in &a.paths {
        let full = ctx.cwd.join(p);
        if !full.exists() {
            return Err(fail(exit::NOT_FOUND, format!("{} does not exist", p.display())));
        }
        let rel = lest_core::catalog::rel_path(&ctx.project.root, &full);
        if !catalog.flows.iter().any(|f| f.rel_path == rel) {
            catalog.load_file(&ctx.project, &full);
        }
    }
    validate_catalog(&mut catalog);
    let wanted: Option<BTreeSet<String>> = if a.paths.is_empty() {
        None
    } else {
        Some(a.paths.iter().map(|p| lest_core::catalog::rel_path(&ctx.project.root, &ctx.cwd.join(p))).collect())
    };
    let diags: Vec<_> =
        catalog.diagnostics.iter().filter(|d| wanted.as_ref().is_none_or(|w| w.contains(&d.file))).collect();
    for d in &diags {
        let glyph = if d.severity == Severity::Error { style.red("✗") } else { style.yellow("!") };
        eprintln!("{glyph} {d}");
    }
    let errors = diags.iter().filter(|d| d.severity == Severity::Error).count();
    let checked = match &wanted {
        Some(w) => w.len(),
        None => catalog.flows.len(),
    };
    if errors > 0 {
        eprintln!("{errors} error(s) in {checked} file(s)");
        Ok(exit::USAGE)
    } else {
        eprintln!("{} {checked} flow(s) valid", style.green("✓"));
        Ok(0)
    }
}

fn cmd_runs(ctx: &Ctx, command: RunsCommand, style: Style) -> Result<u8> {
    let store = ctx.store();
    match command {
        RunsCommand::List { flow, max, id } => {
            let runs = store.list(flow.as_deref(), max);
            if id {
                for r in runs {
                    println!("{}", r.run_id);
                }
                return Ok(0);
            }
            let rows: Vec<Vec<String>> = runs
                .iter()
                .map(|r| {
                    vec![
                        r.run_id.clone(),
                        r.flow_id.clone(),
                        r.result.as_str().to_string(),
                        r.environment.clone().unwrap_or_default(),
                        r.started_at.clone(),
                        render::duration(r.duration_ms),
                    ]
                })
                .collect();
            print!("{}", render::table(&["ID", "FLOW", "RESULT", "ENV", "STARTED", "DURATION"], &rows));
            Ok(0)
        }
        RunsCommand::Get { run, output } => {
            let r = find_run(&store, &run)?;
            if let Some(out) = output {
                std::fs::write(&out, serde_json::to_vec_pretty(&r)?)?;
                eprintln!("wrote {}", out.display());
                return Ok(0);
            }
            println!("{}  {}", r.run_id, r.flow_name);
            println!("Flow: {}", r.flow_id);
            println!("Result: {}", render::result_word(style, r.result));
            if let Some(e) = &r.environment {
                println!("Environment: {e}");
            }
            println!("Started: {}", r.started_at);
            println!("Duration: {}", render::duration(r.duration_ms));
            if let Some(e) = &r.error {
                println!("Error: {e}");
            }
            for w in &r.warnings {
                println!("Warning: {w}");
            }
            println!("Report: {}", store.report_path(&r.flow_id, &r.run_id).display());
            fn print_steps(list: &[lest_core::report::StepReport], depth: usize) {
                for s in list {
                    let detail = s.headline.as_deref().unwrap_or("");
                    println!(
                        "{}{} {} {} {detail}",
                        "  ".repeat(depth + 1),
                        s.status.map(|x| x.as_str()).unwrap_or("-"),
                        s.id,
                        render::duration(s.duration_ms)
                    );
                    print_steps(&s.children, depth + 1);
                }
            }
            println!("Steps:");
            print_steps(&r.steps, 0);
            if !r.finally.is_empty() {
                println!("Finally:");
                print_steps(&r.finally, 0);
            }
            Ok(0)
        }
    }
}

fn confirm(question: &str, force: bool) -> Result<bool> {
    if force {
        return Ok(true);
    }
    if !std::io::stdin().is_terminal() {
        return Err(fail(exit::USAGE, "confirmation needed; pass --force to proceed without a terminal"));
    }
    eprint!("{question} [y/N] ");
    std::io::stderr().flush()?;
    let mut answer = String::new();
    std::io::stdin().read_line(&mut answer)?;
    Ok(matches!(answer.trim(), "y" | "Y" | "yes"))
}

async fn cmd_cleanup(ctx: &Ctx, a: CleanupArgs, style: Style) -> Result<u8> {
    let store = ctx.store();
    let mut report = find_run(&store, &a.run)?;
    let left =
        |c: &&mut lest_core::report::CleanupReport| matches!(c.status, CleanupStatus::NotRun | CleanupStatus::Failed);
    let count = report.cleanups.iter_mut().filter(left).count();
    if count == 0 {
        eprintln!("run {} has no cleanups left to run", report.run_id);
        return Ok(0);
    }
    for c in report.cleanups.iter_mut().filter(left) {
        let env: Vec<String> = c.env.iter().map(|(k, v)| format!("{k}={v}")).collect();
        println!("{}: {} {}", c.step_id, env.join(" "), c.command);
    }
    if a.dry_run {
        return Ok(0);
    }
    if !confirm(&format!("Run {count} cleanup command(s)?"), a.force)? {
        return Ok(exit::FAILED);
    }
    let project = Project::discover(Path::new(&report.project_dir)).unwrap_or_else(|_| ctx.project.clone());
    let fallback_dir =
        project.root.join(&report.flow_path).parent().map(Path::to_path_buf).unwrap_or_else(|| project.root.clone());
    let kr = keyring();
    let resolver = Resolver::new(&project, kr.as_ref());
    let marker = regex_redacted();
    let mut failed = 0;
    for c in report.cleanups.iter_mut().filter(left) {
        // The step's variables, then the cleanup's own env. Secret values
        // were redacted in the report; resolve them again.
        let mut vars = Vec::new();
        for (k, v) in c.context.iter().chain(c.env.iter()) {
            let mut value = v.clone();
            for cap in marker.captures_iter(v) {
                let secret = resolver.resolve(&cap[1]).map_err(|e| fail(exit::ERRORED, e))?.0;
                value = value.replace(&cap[0], &secret);
            }
            vars.push((k.clone(), value));
        }
        let cwd = if Path::new(&c.cwd).is_dir() { PathBuf::from(&c.cwd) } else { fallback_dir.clone() };
        let out = tokio::process::Command::new("sh")
            .arg("-e")
            .arg("-c")
            .arg(&c.command)
            .envs(vars)
            .current_dir(&cwd)
            .stdin(std::process::Stdio::null())
            .output()
            .await?;
        c.exit_code = out.status.code();
        c.output = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
        if out.status.success() {
            c.status = CleanupStatus::Passed;
            c.error = None;
            eprintln!("{} {}", style.green("✓"), c.step_id);
        } else {
            failed += 1;
            c.status = CleanupStatus::Failed;
            c.error = Some(format!("exited {}", out.status.code().unwrap_or(-1)));
            eprintln!("{} {} exited {}", style.red("✗"), c.step_id, out.status.code().unwrap_or(-1));
        }
    }
    // Record what ran, so a second `lest cleanup` does not repeat it.
    store.write(&report)?;
    Ok(if failed > 0 { exit::FAILED } else { 0 })
}

fn regex_redacted() -> regex::Regex {
    regex::Regex::new(r"\[redacted:([A-Za-z_][A-Za-z0-9_]*)\]").expect("re")
}

fn cmd_affected(ctx: &Ctx, a: AffectedArgs) -> Result<u8> {
    let catalog = ctx.catalog();
    require_valid(&catalog)?;
    let changed = lest_core::affected::changed_since(&ctx.project.root, &a.base).map_err(|e| fail(exit::ERRORED, e))?;
    let affected = lest_core::affected::affected(&catalog, &changed).map_err(|e| fail(exit::USAGE, e))?;
    if a.id {
        for f in &affected {
            println!("{}", f.flow_id);
        }
    } else if a.json {
        let items: Vec<_> =
            affected.iter().map(|f| serde_json::json!({"id": f.flow_id, "reasons": f.reasons})).collect();
        println!("{}", serde_json::Value::Array(items));
    } else {
        let rows: Vec<Vec<String>> = affected.iter().map(|f| vec![f.flow_id.clone(), f.reasons.join(", ")]).collect();
        print!("{}", render::table(&["FLOW", "WHY"], &rows));
        eprintln!("{} changed file(s), {} affected flow(s)", changed.len(), affected.len());
    }
    Ok(0)
}

/// The run a command names: by id or unique prefix, or the newest run of a
/// flow.
fn find_run(store: &Store, r: &RunRef) -> Result<lest_core::report::RunReport> {
    match (&r.run, &r.flow) {
        (_, Some(flow)) => store.latest(flow).ok_or_else(|| fail(exit::NOT_FOUND, format!("no runs of flow '{flow}'"))),
        (Some(run), None) => store.find(run).map_err(|e| fail(exit::NOT_FOUND, e.to_string())),
        (None, None) => Err(fail(exit::USAGE, "give a run id or --flow")),
    }
}

fn cmd_bundle(ctx: &Ctx, a: BundleArgs) -> Result<u8> {
    let store = ctx.store();
    let report = find_run(&store, &a.run)?;
    let out = a.output.unwrap_or_else(|| PathBuf::from(format!("lest-{}-{}.zip", report.flow_id, report.run_id)));
    let manifest = lest_core::bundle::bundle(&store.run_dir(&report.flow_id, &report.run_id), &report, &out)
        .map_err(|e| fail(exit::ERRORED, e))?;
    eprintln!("wrote {} ({} files)", out.display(), manifest.files.len());
    Ok(0)
}

const EXAMPLE_FLOW: &str = r#"apiVersion: lest/v1
id: hello
name: Hello, Lest
description: |
  A first flow. Run it with `lest run hello`.
vars:
  greeting: hello
steps:
  - id: say
    name: Say hello
    run: echo "$greeting from $LEST_FLOW_ID"
    outputs:
      text: self.stdout.trim()
  - id: check
    name: Check the greeting
    assert: steps.say.outputs.text == 'hello from hello'
"#;

fn cmd_init(ctx: &Ctx, a: InitArgs) -> Result<u8> {
    let root = &ctx.cwd;
    let config = root.join(PROJECT_FILE);
    if config.exists() {
        return Err(fail(exit::USAGE, format!("{} already exists", config.display())));
    }
    let name = match a.name {
        Some(n) => n,
        None if a.non_interactive || !std::io::stdin().is_terminal() => {
            root.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "project".into())
        }
        None => {
            let default = root.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            eprint!("Project name [{default}]: ");
            std::io::stderr().flush()?;
            let mut s = String::new();
            std::io::stdin().read_line(&mut s)?;
            if s.trim().is_empty() { default } else { s.trim().to_string() }
        }
    };
    std::fs::write(&config, format!("name: {name}\nflows: [flows]\n"))?;
    let flows = root.join("flows");
    std::fs::create_dir_all(&flows)?;
    let example = flows.join("hello.lest.yaml");
    if !example.exists() {
        std::fs::write(&example, EXAMPLE_FLOW)?;
    }
    eprintln!("created {} and {}", config.display(), example.display());
    eprintln!("next: lest run hello");
    Ok(0)
}

async fn cmd_doctor(ctx: &Ctx, style: Style) -> Result<u8> {
    let mut problems = 0;
    let mut row = |ok: Option<bool>, what: &str, detail: String| {
        let glyph = match ok {
            Some(true) => style.green("✓"),
            Some(false) => {
                problems += 1;
                style.red("✗")
            }
            None => style.yellow("!"),
        };
        println!("{glyph} {what:<14} {detail}");
    };
    row(
        Some(true),
        "project",
        format!(
            "{} ({}{})",
            ctx.project.name(),
            ctx.project.root.display(),
            if ctx.project.has_config { "" } else { ", no lest.yaml" }
        ),
    );
    let catalog = ctx.catalog();
    let errors = catalog.diagnostics.iter().filter(|d| d.severity == Severity::Error).count();
    row(Some(errors == 0), "flows", format!("{} flow(s), {errors} validation error(s)", catalog.flows.len()));
    row(Some(true), "data", ctx.roots.data_dir.display().to_string());
    let version = |cmd: &str, args: &[&str]| -> Option<String> {
        std::process::Command::new(cmd)
            .args(args)
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| String::from_utf8_lossy(&o.stdout).lines().next().unwrap_or("").trim().to_string())
    };
    let node = ctx.project.config.browser.as_ref().and_then(|b| b.node.clone()).unwrap_or_else(|| "node".into());
    match version(&node, &["--version"]) {
        Some(v) => {
            let major: u32 = v.trim_start_matches('v').split('.').next().and_then(|m| m.parse().ok()).unwrap_or(0);
            if major >= 18 {
                row(Some(true), "node", v);
            } else {
                row(None, "node", format!("{v}; browser steps need Node.js 18 or newer"));
            }
            // Playwright is loaded from the project, like the harness does.
            let probe = std::process::Command::new(&node)
                .args([
                    "-e",
                    "const r=require('module').createRequire(process.argv[1]+'/x.js');const p=r('playwright');const e=p.chromium.executablePath();process.stdout.write(p.chromium?(require('fs').existsSync(e)?'ok '+e:'nochrome '+e):'');",
                ])
                .arg(&ctx.project.root)
                .output();
            match probe.ok().map(|o| String::from_utf8_lossy(&o.stdout).into_owned()) {
                Some(out) if out.starts_with("ok ") => {
                    row(Some(true), "playwright", "installed, Chromium present".into())
                }
                Some(out) if out.starts_with("nochrome") => row(
                    None,
                    "playwright",
                    "installed, but Chromium is missing: run `npx playwright install chromium`".into(),
                ),
                _ => row(
                    None,
                    "playwright",
                    format!(
                        "not installed in {}; browser steps need `npm i -D playwright && npx playwright install chromium`",
                        ctx.project.root.display()
                    ),
                ),
            }
        }
        None => row(None, "node", format!("'{node}' not found; browser steps need Node.js 18 or newer")),
    }
    match version("ffmpeg", &["-version"]) {
        Some(v) => row(Some(true), "ffmpeg", v.split(" Copyright").next().unwrap_or(&v).to_string()),
        None => row(None, "ffmpeg", "not found; demo videos are kept uncut without it".into()),
    }
    let keyring_tool = if cfg!(target_os = "macos") { "/usr/bin/security" } else { "secret-tool" };
    row(
        if which(keyring_tool) { Some(true) } else { None },
        "keyring",
        if which(keyring_tool) {
            keyring_tool.to_string()
        } else {
            format!("{keyring_tool} not found; use the env or command secret backends")
        },
    );
    for (name, profile) in &ctx.project.config.tools {
        let need = lest_core::tools::Need { name: name.clone(), min_version: None, auth: None };
        let checked =
            lest_core::tools::check(&need, Some(profile), &ctx.project.root, &[], &[], &Default::default()).await;
        let ok = matches!(
            checked.report.status,
            lest_core::report::ToolStatus::Ready | lest_core::report::ToolStatus::Unknown
        );
        row(
            Some(ok),
            &format!("tool {name}"),
            checked.report.message.clone().unwrap_or_else(|| checked.report.version.clone().unwrap_or_default()),
        );
    }
    Ok(if problems > 0 { exit::ERRORED } else { 0 })
}

fn which(cmd: &str) -> bool {
    if cmd.contains('/') { Path::new(cmd).is_file() } else { which::which(cmd).is_ok() }
}

fn cmd_secrets(ctx: &Ctx, command: SecretsCommand) -> Result<u8> {
    let kr = SystemKeyring;
    match command {
        SecretsCommand::Create { name, stdin, from_env, overwrite } => {
            let value = if stdin {
                let mut s = String::new();
                std::io::stdin().read_to_string(&mut s)?;
                s.trim_end_matches(['\n', '\r']).to_string()
            } else if let Some(var) = from_env {
                std::env::var(&var).map_err(|_| fail(exit::USAGE, format!("${var} is not set")))?
            } else {
                return Err(fail(exit::USAGE, "pass --stdin or --from-env VAR; values are never taken as arguments"));
            };
            if value.is_empty() {
                return Err(fail(exit::USAGE, "empty value"));
            }
            if !overwrite && kr.get(&name).map_err(|e| fail(exit::ERRORED, e))?.is_some() {
                return Err(fail(exit::USAGE, format!("{name} already exists; pass --overwrite to replace it")));
            }
            kr.set(&name, &value).map_err(|e| fail(exit::ERRORED, e))?;
            eprintln!("wrote {name} to the {} keyring", lest_core::secrets::KEYRING_SERVICE);
            Ok(0)
        }
        SecretsCommand::List => {
            let catalog = ctx.catalog();
            let mut needed: BTreeMap<String, Vec<String>> = BTreeMap::new();
            for f in &catalog.flows {
                for s in &f.flow.secrets {
                    needed.entry(s.clone()).or_default().push(f.flow.id.clone());
                }
            }
            let resolver = Resolver::new(&ctx.project, &kr);
            let rows: Vec<Vec<String>> = needed
                .iter()
                .map(|(name, flows)| {
                    let source = match resolver.resolve(name) {
                        Ok((_, src)) => src.to_string(),
                        Err(_) => "missing".to_string(),
                    };
                    vec![name.clone(), source, flows.join(",")]
                })
                .collect();
            print!("{}", render::table(&["NAME", "SOURCE", "FLOWS"], &rows));
            Ok(0)
        }
        SecretsCommand::Delete { name, force } => {
            if !confirm(&format!("Delete {name} from the keyring?"), force)? {
                return Ok(exit::FAILED);
            }
            if kr.delete(&name).map_err(|e| fail(exit::ERRORED, e))? {
                eprintln!("deleted {name}");
                Ok(0)
            } else {
                Err(fail(exit::NOT_FOUND, format!("{name} is not in the keyring")))
            }
        }
    }
}

fn user_config(ctx: &Ctx) -> Result<lest_core::ai::UserConfig> {
    lest_core::ai::UserConfig::load(&ctx.roots.config_file()).map_err(|e| fail(exit::ERRORED, e.to_string()))
}

fn cmd_explain(ctx: &Ctx, a: ExplainArgs, context_only: bool) -> Result<u8> {
    let report = find_run(&ctx.store(), &a.run)?;
    let step = match &a.step {
        Some(id) => report
            .find_step(id)
            .ok_or_else(|| fail(exit::NOT_FOUND, format!("no step {id} in run {}", report.run_id)))?,
        None => report
            .first_failure()
            .ok_or_else(|| fail(exit::NOT_FOUND, format!("run {} has no failed step", report.run_id)))?,
    };
    let flow_source = std::fs::read_to_string(std::path::Path::new(&report.project_dir).join(&report.flow_path)).ok();
    if context_only {
        print!("{}", lest_core::ai::context(&report, step, flow_source.as_deref()));
        return Ok(0);
    }
    let cfg = user_config(ctx)?;
    let remove = lest_core::secrets::secret_env_names(&ctx.project.secrets_config());
    let e = lest_core::ai::explain(&cfg.ai, &ctx.roots.data_dir, &report, &step.id, flow_source.as_deref(), &remove);
    println!("{}", e.text);
    match e.source {
        lest_core::ai::Source::Model => eprintln!("(written by {})", e.provider.as_deref().unwrap_or("a model")),
        lest_core::ai::Source::Lest => {}
    }
    if let Some(n) = e.note {
        eprintln!("note: {n}");
    }
    Ok(0)
}

fn cmd_config(ctx: &Ctx, command: ConfigCommand) -> Result<u8> {
    let path = ctx.roots.config_file();
    let mut cfg = user_config(ctx)?;
    match command {
        ConfigCommand::Show => {
            println!("Config: {}", path.display());
            println!("Data: {}", ctx.roots.data_dir.display());
            let status = lest_core::ai::status(&cfg.ai, &ctx.roots.data_dir);
            println!("ai.provider: {}", serde_json::to_value(cfg.ai.provider)?.as_str().unwrap_or("none"));
            println!("ai.model: {}", cfg.ai.model.as_deref().unwrap_or("-"));
            println!("ai.command: {}", cfg.ai.command.as_deref().unwrap_or("-"));
            println!("ai.agent: {}", cfg.ai.agent.as_deref().unwrap_or("-"));
            println!("ai.dailyLimit: {}", status.daily_limit);
            println!(
                "AI: {}",
                match &status.resolved {
                    Some(r) => format!("{} ({} of {} calls today)", r.label, status.calls_today, status.daily_limit),
                    None => "off".to_string(),
                }
            );
            println!(
                "Agent CLIs found: {}",
                if status.available.is_empty() { "none".into() } else { status.available.join(", ") }
            );
            Ok(0)
        }
        ConfigCommand::Set { key, value } => {
            let v = if value.trim().is_empty() { None } else { Some(value.clone()) };
            match key.as_str() {
                "ai.provider" => {
                    cfg.ai.provider = serde_json::from_value(serde_json::Value::String(value.to_lowercase()))
                        .map_err(|_| fail(exit::USAGE, "ai.provider is one of none, auto, claude, codex, command"))?;
                }
                "ai.model" => cfg.ai.model = v,
                "ai.command" => cfg.ai.command = v,
                "ai.agent" => cfg.ai.agent = v,
                "ai.dailyLimit" => {
                    cfg.ai.daily_limit = match v {
                        Some(n) => Some(n.parse().map_err(|_| fail(exit::USAGE, "ai.dailyLimit is a number"))?),
                        None => None,
                    }
                }
                other => return Err(fail(exit::USAGE, format!("unknown setting '{other}'"))),
            }
            cfg.save(&path).map_err(|e| fail(exit::ERRORED, e.to_string()))?;
            eprintln!("saved {key} to {}", path.display());
            Ok(0)
        }
    }
}

/// Opens the UI as a standalone window: a Chromium-family browser in app
/// mode, with its own profile so it is a separate window and app.
fn open_app_window(url: &str, profile: &Path) -> bool {
    let app = format!("--app={url}");
    let data = format!("--user-data-dir={}", profile.display());
    if cfg!(target_os = "macos") {
        let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default();
        for name in ["Google Chrome", "Chromium", "Microsoft Edge", "Brave Browser"] {
            let bundle = format!("{name}.app");
            if Path::new("/Applications").join(&bundle).is_dir() || home.join("Applications").join(&bundle).is_dir() {
                return std::process::Command::new("open")
                    .args(["-na", name, "--args", &app, &data])
                    .status()
                    .is_ok_and(|s| s.success());
            }
        }
        false
    } else {
        let Some(browser) = ["google-chrome", "chromium", "chromium-browser", "microsoft-edge", "brave-browser"]
            .into_iter()
            .find(|b| which::which(b).is_ok())
        else {
            return false;
        };
        let mut cmd = std::process::Command::new(browser);
        cmd.args([&app, &data]).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null());
        // Its own process group, so Ctrl-C in this terminal does not close
        // the window (or other projects' windows sharing the profile).
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            cmd.process_group(0);
        }
        let Ok(mut child) = cmd.spawn() else { return false };
        // A browser that cannot start (no display, a locked profile) exits
        // at once with an error; one that hands the window to a running
        // browser exits at once with success.
        let deadline = std::time::Instant::now() + std::time::Duration::from_millis(1500);
        while std::time::Instant::now() < deadline {
            match child.try_wait() {
                Ok(Some(status)) => return status.success(),
                Ok(None) => std::thread::sleep(std::time::Duration::from_millis(50)),
                Err(_) => return false,
            }
        }
        std::thread::spawn(move || child.wait());
        true
    }
}

async fn cmd_ui(ctx: &Ctx, a: UiArgs) -> Result<u8> {
    let roots = ctx.roots.clone();
    let project = ctx.project.clone();
    let server = lest_server::bind(lest_server::Options {
        project: ctx.project.clone(),
        roots: ctx.roots.clone(),
        port: a.port,
        token: lest_server::new_token(),
        keyring: keyring(),
        browser: Arc::new(move |origin| browser_driver(&project, &roots, origin)),
    })
    .await?;
    let url = server.url();
    eprintln!("lest ui: {url}");
    eprintln!("project: {}", ctx.project.root.display());
    if a.app && open_app_window(&url, &ctx.roots.data_dir.join("ui-window")) {
        eprintln!("opened in its own window");
    } else if !a.no_browser {
        if a.app {
            eprintln!(
                "could not open an app window (no Chromium-family browser, or it failed to start); opening a browser tab"
            );
        }
        let opener = if cfg!(target_os = "macos") { "open" } else { "xdg-open" };
        if std::process::Command::new(opener).arg(&url).status().is_err() {
            eprintln!("open the address above in a browser");
        }
    }
    tokio::select! {
        r = server.serve() => r?,
        _ = tokio::signal::ctrl_c() => {}
    }
    Ok(0)
}

fn cmd_data(ctx: &Ctx, command: DataCommand) -> Result<u8> {
    let store = ctx.store();
    match command {
        DataCommand::Prune { keep, dry_run } => {
            let removed = store.prune(keep, dry_run)?;
            for r in &removed {
                println!("{}", r.display());
            }
            eprintln!("{} {} run(s)", if dry_run { "would remove" } else { "removed" }, removed.len());
            Ok(0)
        }
        DataCommand::Purge { dry_run, force } => {
            let dir = ctx.roots.runs_dir();
            if dry_run {
                println!("{}", dir.display());
                return Ok(0);
            }
            if !dir.exists() {
                eprintln!("nothing to purge");
                return Ok(0);
            }
            if !confirm(&format!("Delete every run in {}?", dir.display()), force)? {
                return Ok(exit::FAILED);
            }
            std::fs::remove_dir_all(&dir)?;
            eprintln!("purged {}", dir.display());
            Ok(0)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_worst_exit_code_wins() {
        assert_eq!(worst_exit([]), 0);
        assert_eq!(worst_exit([0, exit::FAILED, 0]), exit::FAILED);
        assert_eq!(worst_exit([exit::FAILED, exit::ERRORED]), exit::ERRORED);
        assert_eq!(worst_exit([exit::ERRORED, exit::CANCELLED, exit::FAILED]), exit::CANCELLED);
    }
}
