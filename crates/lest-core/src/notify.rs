//! Webhook notifications after a run: a JSON summary, or a Slack-compatible
//! incoming-webhook message.

use std::time::Duration;

use serde_json::{Value as Json, json};

use crate::expr::{self, Scope};
use crate::project::{NotifyFormat, NotifyTarget, NotifyWhen, Project};
use crate::report::{RunReport, RunResult};
use crate::secrets::{Keyring, Resolver};

fn wanted(t: &NotifyTarget, result: RunResult) -> bool {
    match t.on.unwrap_or(NotifyWhen::Failed) {
        NotifyWhen::Always => true,
        NotifyWhen::Passed => result == RunResult::Passed,
        NotifyWhen::Failed => result != RunResult::Passed && result != RunResult::Cancelled,
    }
}

/// The JSON summary posted for `format: json`.
pub fn summary(report: &RunReport) -> Json {
    let failure = report.first_failure().map(|s| {
        json!({
            "step": s.id,
            "name": s.name,
            "headline": s.headline,
        })
    });
    json!({
        "flowId": report.flow_id,
        "flowName": report.flow_name,
        "runId": report.run_id,
        "result": report.result.as_str(),
        "environment": report.environment,
        "startedAt": report.started_at,
        "durationMs": report.duration_ms,
        "error": report.error,
        "failure": failure,
        "warnings": report.warnings,
    })
}

/// A Slack incoming-webhook message.
pub fn slack_message(report: &RunReport) -> Json {
    let icon = match report.result {
        RunResult::Passed => ":white_check_mark:",
        RunResult::Failed | RunResult::Errored => ":x:",
        RunResult::Cancelled => ":black_square_for_stop:",
    };
    let env = report.environment.as_deref().map(|e| format!(" ({e})")).unwrap_or_default();
    let mut text = format!(
        "{icon} *{}*{env} {} in {:.1}s",
        report.flow_name,
        report.result.as_str(),
        report.duration_ms as f64 / 1000.0
    );
    if let Some(f) = report.first_failure() {
        text.push_str(&format!("\n`{}`: {}", f.id, f.headline.as_deref().unwrap_or("failed")));
    } else if let Some(e) = &report.error {
        text.push_str(&format!("\n{e}"));
    }
    for w in &report.warnings {
        text.push_str(&format!("\n:warning: {w}"));
    }
    text.push_str(&format!("\nrun `{}` · rerun: `lest run {}`", report.run_id, report.flow_id));
    json!({ "text": text })
}

/// Sends every configured notification for this result. Returns one line
/// per target: what was sent, or why it failed.
pub async fn send(project: &Project, keyring: &dyn Keyring, report: &RunReport) -> Vec<Result<String, String>> {
    let mut out = Vec::new();
    for (i, t) in project.config.notify.iter().enumerate() {
        if !wanted(t, report.result) {
            continue;
        }
        let label = format!("notify[{i}]");
        let resolver = Resolver::new(project.secrets_config(), keyring);
        let secrets = match resolver.resolve_all(&t.secrets) {
            Ok(s) => s,
            Err(e) => {
                out.push(Err(format!("{label}: {e}")));
                continue;
            }
        };
        let mut scope = Scope::new();
        scope.set("secrets", Json::Object(secrets.into_iter().map(|(k, v)| (k, Json::String(v))).collect()));
        let url = match expr::interpolate(&t.webhook, &scope) {
            Ok(u) => u,
            Err(e) => {
                out.push(Err(format!("{label}: webhook: {e}")));
                continue;
            }
        };
        let body = match t.format.unwrap_or(NotifyFormat::Json) {
            NotifyFormat::Json => summary(report),
            NotifyFormat::Slack => slack_message(report),
        };
        let client = match reqwest::Client::builder().timeout(Duration::from_secs(10)).build() {
            Ok(c) => c,
            Err(e) => {
                out.push(Err(format!("{label}: {e}")));
                continue;
            }
        };
        // The URL may embed a secret: never echo it.
        out.push(match client.post(&url).json(&body).send().await {
            Ok(r) if r.status().is_success() => Ok(format!("{label}: sent ({})", r.status().as_u16())),
            Ok(r) => Err(format!("{label}: the webhook answered {}", r.status().as_u16())),
            Err(e) => Err(format!(
                "{label}: {}",
                if e.is_timeout() { "timed out".to_string() } else { "could not reach the webhook".to_string() }
            )),
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_failures_only() {
        let t = NotifyTarget { webhook: "x".into(), on: None, format: None, secrets: vec![] };
        assert!(wanted(&t, RunResult::Failed));
        assert!(wanted(&t, RunResult::Errored));
        assert!(!wanted(&t, RunResult::Passed));
        assert!(!wanted(&t, RunResult::Cancelled));
    }
}
