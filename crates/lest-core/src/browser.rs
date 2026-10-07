//! Browser steps: the runner starts the Node harness (harness/lest-harness.mjs)
//! with the step's configuration and follows the events it writes.

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::Duration;

use futures::FutureExt;
use futures::future::BoxFuture;
use serde::Deserialize;
use serde_json::{Value as Json, json};

use crate::event::{EventBody, Stream};
use crate::expr;
use crate::process::{self, ProcessSpec};
use crate::report::{Artifact, Beat};
use crate::runner::{AttemptOutcome, BrowserCall, BrowserDriver, LineSink};
use crate::spec::BrowserAction;

const HARNESS: &str = include_str!("../../../harness/lest-harness.mjs");

/// Starts browser steps with Node and Playwright.
pub struct NodeBrowser {
    /// Node binary (`node` on PATH by default).
    pub node: String,
    /// Where the harness file is written.
    pub harness_dir: PathBuf,
    /// Extra Chromium arguments for every step.
    pub args: Vec<String>,
    /// The UI origin allowed to open the DevTools socket for the live view.
    pub allow_origin: Option<String>,
}

/// A video the harness recorded, with when its page was open.
#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Video {
    pub path: String,
    pub role: String,
    pub opened_at_ms: u64,
    pub closed_at_ms: Option<u64>,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
enum HarnessEvent {
    #[serde(rename_all = "camelCase")]
    Beat {
        marker: String,
        at_ms: u64,
    },
    #[serde(rename_all = "camelCase")]
    Output {
        name: String,
        value: Json,
    },
    #[serde(rename_all = "camelCase")]
    Artifact {
        path: String,
        label: Option<String>,
    },
    #[serde(rename_all = "camelCase")]
    Page {
        target_id: String,
        url: String,
    },
    #[serde(rename_all = "camelCase")]
    PhaseFailed {
        name: String,
        message: String,
    },
    #[serde(rename_all = "camelCase")]
    Error {
        message: String,
        phase: Option<String>,
    },
    #[serde(rename_all = "camelCase")]
    Video {
        path: String,
        role: String,
        opened_at_ms: u64,
        closed_at_ms: Option<u64>,
    },
    Session {},
}

impl NodeBrowser {
    fn harness_path(&self) -> Result<PathBuf, String> {
        use sha2::{Digest, Sha256};
        let hash = hex::encode(&Sha256::digest(HARNESS.as_bytes())[..6]);
        let path = self.harness_dir.join(format!("lest-harness-{hash}.mjs"));
        if !path.is_file() {
            std::fs::create_dir_all(&self.harness_dir).map_err(|e| e.to_string())?;
            std::fs::write(&path, HARNESS).map_err(|e| format!("cannot write the browser harness: {e}"))?;
        }
        Ok(path)
    }
}

fn free_port() -> Option<u16> {
    std::net::TcpListener::bind("127.0.0.1:0").ok()?.local_addr().ok().map(|a| a.port())
}

/// Actions as the harness expects them, with templates resolved and
/// durations in milliseconds.
fn harness_actions(actions: &[BrowserAction], scope: &expr::Scope) -> Result<Vec<Json>, String> {
    let mut out = Vec::new();
    for (i, a) in actions.iter().enumerate() {
        let interp = |s: &str| expr::interpolate(s, scope).map_err(|e| format!("actions[{i}]: {e}"));
        let v = if let Some(u) = &a.goto {
            json!({"goto": interp(u)?})
        } else if let Some(s) = &a.click {
            json!({"click": interp(s)?})
        } else if let Some(f) = &a.fill {
            let secret = expr::template_exprs(&f.value).iter().any(|e| e.contains("secrets."));
            json!({"fill": {"selector": interp(&f.selector)?, "value": interp(&f.value)?, "secret": secret}})
        } else if let Some(f) = &a.select {
            json!({"select": {"selector": interp(&f.selector)?, "value": interp(&f.value)?}})
        } else if let Some(k) = &a.press {
            json!({"press": k})
        } else if let Some(s) = &a.wait_for {
            json!({"waitFor": interp(s)?})
        } else if let Some(t) = &a.expect_text {
            json!({"expectText": {"selector": interp(&t.selector)?, "text": interp(&t.text)?}})
        } else if let Some(u) = &a.expect_url {
            json!({"expectUrl": u})
        } else if let Some(s) = &a.screenshot {
            json!({"screenshot": s})
        } else if let Some(b) = &a.beat {
            json!({"beat": b})
        } else if let Some(w) = &a.wait {
            json!({"wait": w.parse().map_err(|e| format!("actions[{i}]: {e}"))?.as_millis() as u64})
        } else if let Some(s) = &a.hover {
            json!({"hover": interp(s)?})
        } else if let Some(r) = &a.read {
            json!({"read": {"name": r.name, "selector": interp(&r.selector)?}})
        } else {
            return Err(format!("actions[{i}] does nothing"));
        };
        out.push(v);
    }
    Ok(out)
}

/// Reads new complete lines from the events file.
struct Tail {
    path: PathBuf,
    pos: u64,
    partial: String,
}

impl Tail {
    fn read(&mut self) -> Vec<String> {
        let Ok(mut f) = std::fs::File::open(&self.path) else { return vec![] };
        if f.seek(SeekFrom::Start(self.pos)).is_err() {
            return vec![];
        }
        let mut reader = BufReader::new(f);
        let mut lines = Vec::new();
        let mut buf = String::new();
        while let Ok(n) = reader.read_line(&mut buf) {
            if n == 0 {
                break;
            }
            self.pos += n as u64;
            if buf.ends_with('\n') {
                let line = format!("{}{}", std::mem::take(&mut self.partial), buf.trim_end());
                lines.push(line);
            } else {
                self.partial.push_str(&buf);
            }
            buf.clear();
        }
        lines
    }
}

#[derive(Default)]
struct Collected {
    outputs: BTreeMap<String, Json>,
    beats: Vec<Beat>,
    artifacts: Vec<Artifact>,
    videos: Vec<Video>,
    error: Option<String>,
    failed_phase: Option<(String, String)>,
}

fn apply(line: &str, call: &BrowserCall<'_>, cdp_port: Option<u16>, out: &mut Collected) {
    let Ok(ev) = serde_json::from_str::<HarnessEvent>(line) else { return };
    let emitter = &call.run.emitter;
    match ev {
        HarnessEvent::Beat { marker, at_ms } => {
            let beat = Beat { marker, at_ms };
            emitter.emit(EventBody::StepBeat { step_id: call.step_id.to_string(), beat: beat.clone() });
            out.beats.push(beat);
        }
        HarnessEvent::Output { name, value } => {
            out.outputs.insert(name, call.run.redactor.redact_json(&value));
        }
        HarnessEvent::Artifact { path, label } => {
            let dest = call.run.artifacts_dir().join(call.step_id.replace('/', "__"));
            match crate::runner::store_artifact(
                Path::new(&path),
                &dest,
                &call.run.run_dir,
                label.as_deref(),
                Some(&call.run.redactor),
            ) {
                Ok(a) => {
                    emitter.emit(EventBody::StepArtifact { step_id: call.step_id.to_string(), artifact: a.clone() });
                    out.artifacts.push(a);
                }
                Err(e) => call.run.warn(format!("step {}: artifact {path}: {e}", call.step_id)),
            }
        }
        HarnessEvent::Page { target_id, url } => {
            if let Some(port) = cdp_port {
                emitter.emit(EventBody::BrowserPage {
                    step_id: call.step_id.to_string(),
                    cdp_port: port,
                    target_id,
                    url: call.run.redactor.redact(&url),
                });
            }
        }
        HarnessEvent::PhaseFailed { name, message } => {
            out.failed_phase.get_or_insert((name, call.run.redactor.redact(&message)));
        }
        HarnessEvent::Error { message, phase } => {
            let message = call.run.redactor.redact(&message);
            out.error = Some(match phase {
                Some(p) if !message.contains(&p) => format!("{p}: {message}"),
                _ => message,
            });
        }
        HarnessEvent::Video { path, role, opened_at_ms, closed_at_ms } => {
            out.videos.push(Video { path, role, opened_at_ms, closed_at_ms });
        }
        HarnessEvent::Session {} => {}
    }
}

impl BrowserDriver for NodeBrowser {
    fn run<'a>(&'a self, call: BrowserCall<'a>) -> BoxFuture<'a, AttemptOutcome> {
        async move {
            let b = call.step.browser.as_ref().expect("browser step");
            let harness = match self.harness_path() {
                Ok(p) => p,
                Err(e) => return AttemptOutcome::errored(e),
            };
            let url = match b.url.as_deref().map(|u| expr::interpolate(u, call.scope)).transpose() {
                Ok(u) => u,
                Err(e) => return AttemptOutcome::errored(format!("url: {e}")),
            };
            let actions = match harness_actions(&b.actions, call.scope) {
                Ok(a) => a,
                Err(e) => return AttemptOutcome::errored(e),
            };
            let safe_id = call.step_id.replace('/', "__");
            let artifacts_dir = call.run.artifacts_dir().join(&safe_id);
            let events_file = call.run.run_dir.join("browser").join(format!("{safe_id}.events.jsonl"));
            if let Some(parent) = events_file.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            let _ = std::fs::remove_file(&events_file);
            let cdp_port = if call.run.browser.live_view { free_port() } else { None };
            let session_path = |name: &str| call.run.sessions_dir.join(format!("{name}.json"));
            let viewport = b.viewport.unwrap_or(if b.record { [1920, 1080] } else { [1280, 800] });
            let mut args = self.args.clone();
            args.extend(b.args.iter().cloned());
            let vars = call.scope.get("vars").cloned().unwrap_or(Json::Null);
            let inputs = call.scope.get("inputs").cloned().unwrap_or(Json::Null);
            let config = json!({
                "stepId": call.step_id,
                "projectDir": call.run.project.root,
                "flowDir": call.flow_dir,
                "artifactsDir": artifacts_dir,
                "eventsFile": events_file,
                "url": url,
                "actions": actions,
                "script": b.script.as_ref().map(|s| call.flow_dir.join(s)),
                "record": b.record,
                "headed": call.run.browser.headed,
                "viewport": viewport,
                "args": args,
                "cdpPort": cdp_port,
                "allowOrigin": self.allow_origin,
                "sessionName": b.session.as_ref().or(b.save_session.as_ref()),
                "sessionIn": b.session.as_deref().map(session_path),
                "sessionOut": b.save_session.as_deref().map(session_path),
                "vars": vars,
                "inputs": inputs,
            });
            let _ = crate::paths::ensure_private_dir(&call.run.sessions_dir);

            let mut tail = Tail { path: events_file.clone(), pos: 0, partial: String::new() };
            let mut collected = Collected::default();
            let mut sink = LineSink::new(call.run, call.step_id);
            let spec = ProcessSpec {
                program: self.node.clone(),
                args: vec![harness.display().to_string()],
                cwd: call.flow_dir.to_path_buf(),
                env: call.env.clone(),
                env_remove: call.run.env_remove.clone(),
                stdin: Some(serde_json::to_vec(&config).expect("config serializes")),
                timeout: call.timeout,
                cap: process::DEFAULT_CAP,
            };
            let proc = process::run(spec, call.run.cancel.main.clone(), |stream, line| {
                let stream = if line.starts_with("[browser]") { Stream::Browser } else { stream };
                sink.line(stream, line);
            });
            tokio::pin!(proc);
            let mut ticker = tokio::time::interval(Duration::from_millis(150));
            let res = loop {
                tokio::select! {
                    r = &mut proc => break r,
                    _ = ticker.tick() => {
                        for line in tail.read() {
                            apply(&line, &call, cdp_port, &mut collected);
                        }
                    }
                }
            };
            for line in tail.read() {
                apply(&line, &call, cdp_port, &mut collected);
            }
            call.run.emitter.emit(EventBody::BrowserClosed { step_id: call.step_id.to_string() });

            // Videos become artifacts of the step (the raw takes).
            for v in &collected.videos {
                let label = if v.role == "main" { "Recording".to_string() } else { format!("Recording ({})", v.role) };
                if let Ok(a) = crate::runner::store_artifact(Path::new(&v.path), &artifacts_dir, &call.run.run_dir, Some(&label), None) {
                    call.run.emitter.emit(EventBody::StepArtifact { step_id: call.step_id.to_string(), artifact: a.clone() });
                    collected.artifacts.push(a);
                }
            }

            let mut o = AttemptOutcome {
                resolved: Some(match (&url, &b.script) {
                    (_, Some(s)) => format!("browser script {s}"),
                    (Some(u), None) => format!("browser {u} ({} actions)", b.actions.len()),
                    (None, None) => format!("browser ({} actions)", b.actions.len()),
                }),
                exit_code: res.exit_code,
                stdout: res.stdout,
                stderr: res.stderr,
                duration_ms: res.duration.as_millis() as u64,
                cancelled: res.cancelled,
                outputs: collected.outputs,
                beats: collected.beats,
                artifacts: collected.artifacts,
                recording: if b.record { Some(collected.videos.clone()) } else { None },
                ..Default::default()
            };
            if let Some(e) = res.spawn_error {
                o.errored = true;
                o.error = Some(if e.contains("not found") {
                    format!("{e}; browser steps need Node.js 18 or newer")
                } else {
                    e
                });
            } else if res.timed_out {
                o.errored = true;
                o.error = Some(format!("timed out after {}", humantime::format_duration(call.timeout)));
            } else if res.exit_code == Some(3) {
                o.errored = true;
                o.error = Some(format!(
                    "Playwright is not installed in {}; run `npm i -D playwright && npx playwright install chromium` there",
                    call.run.project.root.display()
                ));
            } else if res.exit_code == Some(0) {
                o.passed = true;
            } else {
                o.error = collected
                    .error
                    .or_else(|| collected.failed_phase.map(|(p, m)| format!("{p}: {m}")))
                    .or_else(|| o.stderr.lines().rev().find(|l| !l.trim().is_empty()).map(str::to_string));
            }
            o
        }
        .boxed()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn actions_are_resolved_for_the_harness() {
        let yaml = r##"
- goto: /login
- fill: { selector: "#email", value: "${{ vars.user }}" }
- fill: { selector: "#password", value: "${{ secrets.pw }}" }
- wait: 1.5s
- read: { name: title, selector: h1 }
"##;
        let actions: Vec<BrowserAction> = serde_yaml_ng::from_str(yaml).unwrap();
        let mut scope = expr::Scope::new();
        scope.set("vars", json!({"user": "a@example.com"}));
        scope.set("secrets", json!({"pw": "hunter22"}));
        let out = harness_actions(&actions, &scope).unwrap();
        assert_eq!(out[1], json!({"fill": {"selector": "#email", "value": "a@example.com", "secret": false}}));
        assert_eq!(out[2]["fill"]["secret"], json!(true));
        assert_eq!(out[3], json!({"wait": 1500}));
        assert_eq!(out[4], json!({"read": {"name": "title", "selector": "h1"}}));
    }

    #[test]
    fn tail_reads_only_complete_lines() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("e.jsonl");
        std::fs::write(&path, "{\"a\":1}\n{\"b\":").unwrap();
        let mut t = Tail { path: path.clone(), pos: 0, partial: String::new() };
        assert_eq!(t.read(), vec!["{\"a\":1}"]);
        use std::io::Write;
        std::fs::OpenOptions::new().append(true).open(&path).unwrap().write_all(b"2}\n").unwrap();
        assert_eq!(t.read(), vec!["{\"b\":2}"]);
    }
}
