mod args;
mod render;

use std::collections::{BTreeMap, BTreeSet};
use std::io::{IsTerminal, Read, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;

use anyhow::{Context as _, Result};
use clap::Parser;
use lest_core::catalog::{Catalog, Severity};
use lest_core::paths::StateRoots;
use lest_core::project::{PROJECT_FILE, Project};
use lest_core::report::{CleanupStatus, RunResult};
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

fn keyring() -> Arc<dyn Keyring> {
    Arc::new(SystemKeyring)
}

async fn cmd_run(ctx: &Ctx, a: RunArgs, style: Style) -> Result<u8> {
    // A path outside the discovered project: use that file's project.
    let mut project = ctx.project.clone();
    let candidate = ctx.cwd.join(&a.flow);
    if candidate.is_file() {
        project = Project::discover(&candidate)?;
    }
    let mut catalog = Catalog::load(&project);
    if candidate.is_file()
        && lest_core::catalog::is_flow_file(&candidate)
        && catalog.resolve(&project, &ctx.cwd, &a.flow).is_err()
    {
        catalog.load_file(&project, &candidate);
    }
    validate_catalog(&mut catalog);
    let flow_id = match catalog.resolve(&project, &ctx.cwd, &a.flow) {
        Ok(f) => f.flow.id.clone(),
        Err(e) => {
            // A file that does not parse never makes it into the catalog;
            // show why instead of only "no flow".
            for d in catalog.diagnostics.iter().filter(|d| d.flow.is_none() && d.severity == Severity::Error) {
                eprintln!("{} {d}", style.red("✗"));
            }
            return Err(fail(exit::NOT_FOUND, e));
        }
    };
    check_runnable(&catalog, &flow_id, style)?;

    let store = ctx.store();
    let resume = match &a.from {
        None => None,
        Some(step) => {
            let old = match &a.resume_run {
                Some(id) => store.find(id)?,
                None => store
                    .latest(&flow_id)
                    .ok_or_else(|| fail(exit::NOT_FOUND, format!("no earlier run of {flow_id} to resume from")))?,
            };
            Some((old, step.clone()))
        }
    };
    let engine =
        Engine { project, catalog: Arc::new(catalog), store: store.clone(), keyring: keyring(), browser: None };
    let req = RunRequest {
        flow_id: flow_id.clone(),
        environment: a.environment.clone(),
        inputs: parse_inputs(&a.inputs)?,
        resume,
        interactive: !a.non_interactive && std::io::stdin().is_terminal(),
        browser: runner::BrowserOptions { headed: a.headed, live_view: false },
    };

    let cancel = RunCancel::default();
    {
        let cancel = cancel.clone();
        tokio::spawn(async move {
            let mut presses = 0;
            while tokio::signal::ctrl_c().await.is_ok() {
                presses += 1;
                match presses {
                    1 => {
                        eprintln!(
                            "\nlest: cancelling; finally steps and cleanups still run (Ctrl-C again to skip them)"
                        );
                        cancel.main.cancel();
                    }
                    2 => {
                        eprintln!("\nlest: skipping cleanup (Ctrl-C again to exit now)");
                        cancel.cleanup.cancel();
                    }
                    _ => std::process::exit(exit::CANCELLED as i32),
                }
            }
        });
    }

    let (tx, mut rx) = mpsc::unbounded_channel::<lest_core::event::RunEvent>();
    let quiet = a.quiet;
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
    let report = runner::execute(&engine, req, tx, cancel).await;
    let report_path = printer.await.unwrap_or_default();

    if !a.quiet {
        render::summary(style, &report, &report_path);
    }
    if let Some(out) = &a.output {
        std::fs::write(out, serde_json::to_vec_pretty(&report)?)
            .with_context(|| format!("writing {}", out.display()))?;
    }
    if let Some(out) = &a.junit {
        std::fs::write(out, render::junit(&report)).with_context(|| format!("writing {}", out.display()))?;
    }
    println!(
        "{} {} {} {}",
        render::result_word(style, report.result),
        report.flow_id,
        render::duration(report.duration_ms),
        report.run_id
    );
    Ok(match report.result {
        RunResult::Passed => 0,
        RunResult::Failed => exit::FAILED,
        RunResult::Errored => exit::ERRORED,
        RunResult::Cancelled => exit::CANCELLED,
    })
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
        println!("Input: {}{choices}{default}{}", i.name, if i.required { " (required)" } else { "" });
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
            let r = store.find(&run).map_err(|e| fail(exit::NOT_FOUND, e.to_string()))?;
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
    let mut report = store.find(&a.run).map_err(|e| fail(exit::NOT_FOUND, e.to_string()))?;
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
    let resolver = Resolver::new(project.secrets_config(), kr.as_ref());
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
    match version("node", &["--version"]) {
        Some(v) => row(Some(true), "node", v),
        None => row(None, "node", "not found; browser steps need Node.js 18+".into()),
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
            let resolver = Resolver::new(ctx.project.secrets_config(), &kr);
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
