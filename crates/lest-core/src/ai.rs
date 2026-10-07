//! Optional AI. Lest never needs it: every place a model can help already
//! has deterministic content, and a model only adds to it.
//!
//! - [`explain`]: a short explanation of a failed step. Without a provider,
//!   or when the provider fails, the deterministic headline is the answer.
//! - [`handoff`]: the context and command to continue in the user's own
//!   agent CLI, which has tools, permissions and memory. Lest builds no chat.
//!
//! A provider is one headless call: prompt on stdin, text on stdout, no
//! tools, an empty working directory, and a hard timeout.

use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Duration;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::report::{RunReport, StepReport};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum ProviderKind {
    /// No AI (the default).
    #[default]
    None,
    /// The first agent CLI found on PATH (`claude`, then `codex`).
    Auto,
    Claude,
    Codex,
    /// Any program that reads a prompt on stdin and prints text.
    Command,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AiConfig {
    #[serde(default)]
    pub provider: ProviderKind,
    /// Model name passed to the provider (default: its small model).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// For `provider: command`: the program and arguments (run with `sh -c`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    /// The interactive agent to hand off to; `{prompt}` is replaced with the
    /// quoted prompt (default: the provider's CLI).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
    /// Model calls allowed per day (default 50).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub daily_limit: Option<u32>,
}

/// The provider a config resolves to on this machine.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Resolved {
    pub kind: ProviderKind,
    /// A short label for settings: `claude`, `codex`, `command`.
    pub label: String,
    pub model: Option<String>,
}

/// What the settings screen shows: the configured provider, what it
/// resolves to, and which agent CLIs exist on this machine.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Status {
    pub configured: ProviderKind,
    pub resolved: Option<Resolved>,
    pub available: Vec<String>,
    pub calls_today: u32,
    pub daily_limit: u32,
}

fn on_path(bin: &str) -> bool {
    which::which(bin).is_ok()
}

pub fn resolve(cfg: &AiConfig) -> Option<Resolved> {
    let make = |kind, label: &str| Some(Resolved { kind, label: label.to_string(), model: cfg.model.clone() });
    match cfg.provider {
        ProviderKind::None => None,
        ProviderKind::Auto => {
            if on_path("claude") {
                make(ProviderKind::Claude, "claude")
            } else if on_path("codex") {
                make(ProviderKind::Codex, "codex")
            } else {
                None
            }
        }
        ProviderKind::Claude => on_path("claude").then(|| make(ProviderKind::Claude, "claude")).flatten(),
        ProviderKind::Codex => on_path("codex").then(|| make(ProviderKind::Codex, "codex")).flatten(),
        ProviderKind::Command => cfg.command.as_ref().and_then(|_| make(ProviderKind::Command, "command")),
    }
}

pub fn status(cfg: &AiConfig, data_dir: &Path) -> Status {
    Status {
        configured: cfg.provider,
        resolved: resolve(cfg),
        available: ["claude", "codex"].into_iter().filter(|b| on_path(b)).map(str::to_string).collect(),
        calls_today: Ledger::new(data_dir).calls_today(),
        daily_limit: cfg.daily_limit.unwrap_or(50),
    }
}

/// One headless call.
pub fn ask(cfg: &AiConfig, resolved: &Resolved, system: &str, input: &str, timeout: Duration) -> Result<String, String> {
    let dir = tempdir()?;
    let out_file = dir.join("answer.txt");
    let mut cmd = match resolved.kind {
        ProviderKind::Claude => {
            let mut c = std::process::Command::new("claude");
            c.args([
                "-p",
                "--output-format",
                "text",
                "--no-session-persistence",
                "--max-turns",
                "1",
                "--tools",
                "",
                "--strict-mcp-config",
                "--disable-slash-commands",
                "--system-prompt",
                system,
                "--model",
                cfg.model.as_deref().unwrap_or("haiku"),
            ]);
            c
        }
        ProviderKind::Codex => {
            let mut c = std::process::Command::new("codex");
            c.args(["exec", "--ephemeral", "--skip-git-repo-check", "-s", "read-only", "-o"])
                .arg(&out_file)
                .args(cfg.model.as_deref().map(|m| vec!["-m", m]).unwrap_or_default())
                // Codex has no system-prompt flag; the instruction leads the prompt.
                .arg(format!("{system}\n\nThe input follows on stdin."));
            c
        }
        ProviderKind::Command => {
            let mut c = std::process::Command::new("sh");
            c.arg("-c").arg(cfg.command.as_deref().unwrap_or("false"));
            c
        }
        _ => return Err("no provider".into()),
    };
    // The child must not think it is running inside the user's session.
    for (k, _) in std::env::vars_os() {
        let k = k.to_string_lossy();
        if k.starts_with("CLAUDE") || k.starts_with("CODEX_") || k.starts_with("LEST_SECRET_") {
            cmd.env_remove(k.as_ref());
        }
    }
    let stdin_text = if resolved.kind == ProviderKind::Command { format!("{system}\n\n{input}") } else { input.to_string() };
    cmd.current_dir(&dir)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    let mut child = cmd.spawn().map_err(|e| format!("cannot start {}: {e}", resolved.label))?;
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(stdin_text.as_bytes());
    }
    let pid = child.id();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(child.wait_with_output());
    });
    let output = match rx.recv_timeout(timeout) {
        Ok(r) => r.map_err(|e| e.to_string())?,
        Err(_) => {
            #[cfg(unix)]
            // SAFETY: signals the provider process we spawned.
            unsafe {
                libc::kill(pid as i32, libc::SIGKILL);
            }
            let _ = std::fs::remove_dir_all(&dir);
            return Err(format!("{} did not answer within {}s", resolved.label, timeout.as_secs()));
        }
    };
    let text = if resolved.kind == ProviderKind::Codex {
        std::fs::read_to_string(&out_file).unwrap_or_default()
    } else {
        String::from_utf8_lossy(&output.stdout).into_owned()
    };
    let _ = std::fs::remove_dir_all(&dir);
    if !output.status.success() {
        let err = String::from_utf8_lossy(&output.stderr);
        return Err(format!(
            "{} exited {}: {}",
            resolved.label,
            output.status.code().unwrap_or(-1),
            err.lines().next().unwrap_or("").trim()
        ));
    }
    let text = text.trim().to_string();
    if text.is_empty() { Err(format!("{} returned nothing", resolved.label)) } else { Ok(text) }
}

fn tempdir() -> Result<PathBuf, String> {
    let dir = std::env::temp_dir().join(format!("lest-ai-{}-{}", std::process::id(), rand::random::<u32>()));
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    Ok(dir)
}

/// Calls made per day, so a configured provider cannot run up cost.
struct Ledger {
    path: PathBuf,
}

impl Ledger {
    fn new(data_dir: &Path) -> Self {
        Ledger { path: data_dir.join("ai").join("usage.jsonl") }
    }
    fn today() -> String {
        time::OffsetDateTime::now_utc().date().to_string()
    }
    fn calls_today(&self) -> u32 {
        let today = Self::today();
        std::fs::read_to_string(&self.path)
            .unwrap_or_default()
            .lines()
            .filter(|l| l.contains(&format!("\"day\":\"{today}\"")))
            .count() as u32
    }
    fn record(&self, provider: &str) {
        if let Some(p) = self.path.parent() {
            let _ = crate::paths::ensure_private_dir(p);
        }
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&self.path) {
            let _ = writeln!(f, "{}", serde_json::json!({"day": Self::today(), "provider": provider}));
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum Source {
    /// Built from the step's own output, with no model involved.
    Lest,
    Model,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct Explanation {
    pub text: String,
    pub source: Source,
    /// The provider that wrote it, for `source: model`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    /// Why the model was not used, when it was configured but failed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

const EXPLAIN_SYSTEM: &str = "You explain why a step of an automated end-to-end test failed. \
You get the step definition, its command, its output and the run's context. \
Answer in at most three short sentences of plain text: the most likely cause, then the single next action to confirm or fix it. \
Name the concrete thing (the status code, the missing value, the command). Do not speculate beyond the evidence; \
if the output does not show the cause, say what to look at.";

/// Explains a failed step: the deterministic headline, or a model's
/// explanation when a provider is configured, answers in time, and is under
/// the daily limit. Cached by input.
pub fn explain(cfg: &AiConfig, data_dir: &Path, report: &RunReport, step_id: &str, flow_source: Option<&str>) -> Explanation {
    let Some(step) = report.find_step(step_id) else {
        return Explanation { text: format!("no step {step_id} in this run"), source: Source::Lest, provider: None, note: None };
    };
    let deterministic = Explanation {
        text: step.headline.clone().or_else(|| step.error.clone()).unwrap_or_else(|| "This step did not fail.".to_string()),
        source: Source::Lest,
        provider: None,
        note: None,
    };
    let Some(resolved) = resolve(cfg) else { return deterministic };
    if !step.status.is_some_and(|s| s.is_failure()) {
        return deterministic;
    }
    let input = context(report, step, flow_source);
    let key = hex::encode(Sha256::digest(format!("{}\n{}\n{input}", resolved.label, cfg.model.as_deref().unwrap_or("")).as_bytes()));
    let cache = data_dir.join("ai").join("explain").join(format!("{}.json", &key[..32]));
    if let Some(e) = std::fs::read(&cache).ok().and_then(|b| serde_json::from_slice::<Explanation>(&b).ok()) {
        return e;
    }
    let ledger = Ledger::new(data_dir);
    let limit = cfg.daily_limit.unwrap_or(50);
    if ledger.calls_today() >= limit {
        return Explanation { note: Some(format!("daily limit of {limit} model calls reached")), ..deterministic };
    }
    ledger.record(&resolved.label);
    match ask(cfg, &resolved, EXPLAIN_SYSTEM, &input, Duration::from_secs(90)) {
        Ok(text) => {
            let e = Explanation { text, source: Source::Model, provider: Some(resolved.label.clone()), note: None };
            if let Ok(bytes) = serde_json::to_vec(&e) {
                let _ = crate::paths::write_private(&cache, &bytes);
            }
            e
        }
        Err(err) => Explanation { note: Some(err), ..deterministic },
    }
}

fn tail(text: &str, lines: usize, max_chars: usize) -> String {
    let all: Vec<&str> = text.lines().collect();
    let t = all[all.len().saturating_sub(lines)..].join("\n");
    if t.chars().count() > max_chars {
        t.chars().rev().take(max_chars).collect::<Vec<_>>().into_iter().rev().collect()
    } else {
        t
    }
}

/// Everything someone (or an agent) needs to pick up a failure: where the
/// flow and report are, what failed, the output's tail, and how to rerun.
/// Built only from the redacted report.
pub fn context(report: &RunReport, step: &StepReport, flow_source: Option<&str>) -> String {
    let mut s = String::new();
    s.push_str(&format!("Flow: {} ({})\n", report.flow_name, report.flow_id));
    s.push_str(&format!("Flow file: {}/{}\n", report.project_dir, report.flow_path));
    s.push_str(&format!("Run: {} ({}", report.run_id, report.result.as_str()));
    if let Some(env) = &report.environment {
        s.push_str(&format!(", environment {env}"));
    }
    s.push_str(")\n");
    if !report.inputs.is_empty() {
        let inputs: Vec<String> = report.inputs.iter().map(|(k, v)| format!("{k}={v}")).collect();
        s.push_str(&format!("Inputs: {}\n", inputs.join(" ")));
    }
    s.push_str(&format!("\nFailed step: {} ({})\n", step.id, step.name));
    if let Some(k) = step.kind {
        s.push_str(&format!("Kind: {}\n", k.as_str()));
    }
    if let Some(r) = &step.resolved {
        s.push_str(&format!("Ran: {r}\n"));
    }
    if let Some(h) = &step.headline {
        s.push_str(&format!("Headline: {h}\n"));
    }
    if let Some(e) = step.error.as_ref().filter(|e| Some(*e) != step.headline.as_ref()) {
        s.push_str(&format!("Error: {e}\n"));
    }
    if let Some(c) = step.exit_code {
        s.push_str(&format!("Exit code: {c}\n"));
    }
    if let Some(c) = step.http_status {
        s.push_str(&format!("HTTP status: {c}\n"));
    }
    s.push_str(&format!("Attempts: {}, duration {} ms\n", step.attempts, step.duration_ms));
    if let Some(n) = &step.notes {
        s.push_str(&format!("Author's notes on this step: {n}\n"));
    }
    if !step.stderr.trim().is_empty() {
        s.push_str(&format!("\nstderr (end):\n{}\n", tail(&step.stderr, 40, 4000)));
    }
    if !step.stdout.trim().is_empty() {
        s.push_str(&format!("\nstdout (end):\n{}\n", tail(&step.stdout, 40, 4000)));
    }
    let earlier: Vec<String> = report
        .all_steps()
        .into_iter()
        .take_while(|x| x.id != step.id)
        .filter(|x| x.children.is_empty())
        .map(|x| format!("{} {} ({} ms)", x.id, x.status.map(|s| s.as_str()).unwrap_or("-"), x.duration_ms))
        .collect();
    if !earlier.is_empty() {
        s.push_str(&format!("\nEarlier steps: {}\n", earlier.join(", ")));
    }
    if let Some(src) = flow_source {
        s.push_str(&format!("\nFlow definition:\n{}\n", tail(src, 400, 16000)));
    }
    let top = report.steps.iter().find(|t| t.id == step.id || contains(t, &step.id)).map(|t| t.id.clone());
    s.push_str(&format!(
        "\nRerun: lest run {}{}\nFull report: lest runs get {} -o report.json\n",
        report.flow_id,
        top.map(|t| format!(" --from {t}")).unwrap_or_default(),
        report.run_id
    ));
    s
}

fn contains(s: &StepReport, id: &str) -> bool {
    s.children.iter().any(|c| c.id == id || contains(c, id))
}

/// How to continue in the user's own agent.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Handoff {
    /// The text to paste anywhere (always available).
    pub context: String,
    /// The prompt given to the agent.
    pub prompt: String,
    /// A shell command that starts the agent in the project, when an agent
    /// is configured.
    pub command: Option<String>,
}

fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

pub fn handoff(cfg: &AiConfig, report: &RunReport, step_id: Option<&str>) -> Handoff {
    let failed = step_id.and_then(|id| report.find_step(id)).or_else(|| report.first_failure());
    let context = match failed {
        Some(step) => context(report, step, None),
        None => format!(
            "Flow: {} ({})\nFlow file: {}/{}\nRun: {} ({})\nRerun: lest run {}\n",
            report.flow_name,
            report.flow_id,
            report.project_dir,
            report.flow_path,
            report.run_id,
            report.result.as_str(),
            report.flow_id
        ),
    };
    let prompt = match failed {
        Some(step) => format!(
            "The Lest flow {} failed at step {} in run {}. Read the flow at {}/{} and the run with `lest runs get {} -o report.json`, \
find the cause, and fix the flow or tell me what is wrong with the system under test. `lest docs agent` explains the flow format. \
Rerun with `lest run {}` to confirm.",
            report.flow_id, step.id, report.run_id, report.project_dir, report.flow_path, report.run_id, report.flow_id
        ),
        None => format!(
            "Look at the Lest flow {} ({}/{}). `lest docs agent` explains the flow format.",
            report.flow_id, report.project_dir, report.flow_path
        ),
    };
    let agent = cfg.agent.clone().or_else(|| match resolve(cfg).map(|r| r.kind) {
        Some(ProviderKind::Claude) => Some("claude {prompt}".to_string()),
        Some(ProviderKind::Codex) => Some("codex {prompt}".to_string()),
        _ => None,
    });
    let command = agent.map(|a| {
        let cmd = a.replace("{prompt}", &shell_quote(&prompt));
        format!("cd {} && {cmd}", shell_quote(&report.project_dir))
    });
    Handoff { context, prompt, command }
}

/// The user's settings file (`<config>/config.yml`).
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UserConfig {
    #[serde(default)]
    pub ai: AiConfig,
    /// Recently opened projects, newest first.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub projects: Vec<String>,
    /// Extra settings kept for forward compatibility.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty", flatten)]
    pub other: BTreeMap<String, serde_yaml_ng::Value>,
}

impl UserConfig {
    pub fn load(path: &Path) -> anyhow::Result<UserConfig> {
        match std::fs::read_to_string(path) {
            Ok(t) if t.trim().is_empty() => Ok(UserConfig::default()),
            Ok(t) => serde_yaml_ng::from_str(&t).map_err(|e| anyhow::anyhow!("{}: {e}", path.display())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(UserConfig::default()),
            Err(e) => Err(anyhow::anyhow!("{}: {e}", path.display())),
        }
    }

    pub fn save(&self, path: &Path) -> anyhow::Result<()> {
        let text = serde_yaml_ng::to_string(self)?;
        crate::paths::write_private(path, text.as_bytes())?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::report::*;

    fn report() -> RunReport {
        let failed = StepReport {
            id: "fetch".into(),
            name: "Fetch the user".into(),
            kind: Some(crate::spec::StepKind::Http),
            status: Some(StepStatus::Failed),
            attempts: 1,
            http_status: Some(404),
            resolved: Some("GET http://127.0.0.1:1/users/9".into()),
            headline: Some("HTTP 404 from GET http://127.0.0.1:1/users/9".into()),
            stdout: r#"{"error":"no such user"}"#.into(),
            ..Default::default()
        };
        RunReport {
            schema_version: 1,
            run_id: "r1".into(),
            flow_id: "users".into(),
            flow_name: "Users".into(),
            flow_path: "flows/users.lest.yaml".into(),
            flow_sha256: Default::default(),
            project_dir: "/work/app".into(),
            environment: Some("local".into()),
            started_at: String::new(),
            finished_at: None,
            duration_ms: 1,
            result: RunResult::Failed,
            error: None,
            warnings: vec![],
            inputs: Default::default(),
            host: HostInfo::current(),
            tools: vec![],
            steps: vec![StepReport { id: "setup".into(), status: Some(StepStatus::Passed), ..Default::default() }, failed],
            finally: vec![],
            cleanups: vec![],
            services: vec![],
            resumed_from: None,
            demo: None,
        }
    }

    #[test]
    fn without_a_provider_the_headline_is_the_explanation() {
        let dir = tempfile::tempdir().unwrap();
        let e = explain(&AiConfig::default(), dir.path(), &report(), "fetch", None);
        assert_eq!(e.source, Source::Lest);
        assert_eq!(e.text, "HTTP 404 from GET http://127.0.0.1:1/users/9");
    }

    #[test]
    fn a_command_provider_answers_and_is_cached() {
        let dir = tempfile::tempdir().unwrap();
        let counter = dir.path().join("calls");
        let cfg = AiConfig {
            provider: ProviderKind::Command,
            command: Some(format!("echo x >> {}; echo 'The user does not exist; create it first.'", counter.display())),
            ..Default::default()
        };
        let e = explain(&cfg, dir.path(), &report(), "fetch", None);
        assert_eq!(e.source, Source::Model, "{e:?}");
        assert_eq!(e.text, "The user does not exist; create it first.");
        let again = explain(&cfg, dir.path(), &report(), "fetch", None);
        assert_eq!(again.text, e.text);
        assert_eq!(std::fs::read_to_string(&counter).unwrap().lines().count(), 1);
    }

    #[test]
    fn a_failing_provider_falls_back_with_a_note() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = AiConfig { provider: ProviderKind::Command, command: Some("exit 7".into()), ..Default::default() };
        let e = explain(&cfg, dir.path(), &report(), "fetch", None);
        assert_eq!(e.source, Source::Lest);
        assert!(e.note.unwrap().contains("exited 7"));
    }

    #[test]
    fn context_names_files_failure_and_rerun() {
        let r = report();
        let c = context(&r, r.find_step("fetch").unwrap(), None);
        assert!(c.contains("Flow file: /work/app/flows/users.lest.yaml"));
        assert!(c.contains("HTTP status: 404"));
        assert!(c.contains("lest run users --from fetch"));
        assert!(c.contains("Earlier steps: setup passed"));
    }

    #[test]
    fn handoff_has_a_command_only_with_an_agent() {
        let r = report();
        assert!(handoff(&AiConfig::default(), &r, Some("fetch")).command.is_none());
        let cfg = AiConfig { agent: Some("my-agent --ask {prompt}".into()), ..Default::default() };
        let h = handoff(&cfg, &r, Some("fetch"));
        assert!(h.command.unwrap().starts_with("cd '/work/app' && my-agent --ask 'The Lest flow users failed"));
    }
}
