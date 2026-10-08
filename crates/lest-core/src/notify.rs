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

/// Slack's three control characters, so a flow name or error text cannot
/// mention a channel or forge a link.
fn slack_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

/// A Slack incoming-webhook message.
pub fn slack_message(report: &RunReport) -> Json {
    let icon = match report.result {
        RunResult::Passed => ":white_check_mark:",
        RunResult::Failed | RunResult::Errored => ":x:",
        RunResult::Cancelled => ":black_square_for_stop:",
    };
    let env = report.environment.as_deref().map(|e| format!(" ({})", slack_escape(e))).unwrap_or_default();
    let mut text = format!(
        "{icon} *{}*{env} {} in {:.1}s",
        slack_escape(&report.flow_name),
        report.result.as_str(),
        report.duration_ms as f64 / 1000.0
    );
    if let Some(f) = report.first_failure() {
        text.push_str(&format!(
            "\n`{}`: {}",
            slack_escape(&f.id),
            slack_escape(f.headline.as_deref().unwrap_or("failed"))
        ));
    } else if let Some(e) = &report.error {
        text.push_str(&format!("\n{}", slack_escape(e)));
    }
    for w in &report.warnings {
        text.push_str(&format!("\n:warning: {}", slack_escape(w)));
    }
    text.push_str(&format!("\nrun `{}` · rerun: `lest run {}`", report.run_id, report.flow_id));
    json!({ "text": text })
}

/// Sends every configured notification for this result, all at once, so
/// the slowest webhook bounds the wait at 10 seconds. Returns one line per
/// target: what was sent, or why it failed.
pub async fn send(project: &Project, keyring: &dyn Keyring, report: &RunReport) -> Vec<Result<String, String>> {
    let mut prepared = Vec::new();
    let mut out = Vec::new();
    for (i, t) in project.config.notify.iter().enumerate() {
        if !wanted(t, report.result) {
            continue;
        }
        let label = format!("notify[{i}]");
        let resolver = Resolver::new(project, keyring);
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
        prepared.push((label, url, body));
    }
    let Ok(client) = reqwest::Client::builder().timeout(Duration::from_secs(10)).build() else {
        out.push(Err("notify: cannot create an HTTP client".into()));
        return out;
    };
    let sends = prepared.into_iter().map(|(label, url, body)| {
        let client = client.clone();
        async move {
            // The URL may embed a secret: never echo it.
            match client.post(&url).json(&body).send().await {
                Ok(r) if r.status().is_success() => Ok(format!("{label}: sent ({})", r.status().as_u16())),
                Ok(r) => Err(format!("{label}: the webhook answered {}", r.status().as_u16())),
                Err(e) => Err(format!(
                    "{label}: {}",
                    if e.is_timeout() { "timed out".to_string() } else { "could not reach the webhook".to_string() }
                )),
            }
        }
    });
    out.extend(futures::future::join_all(sends).await);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slack_text_cannot_ping_or_link() {
        assert_eq!(slack_escape("<!channel> a & <https://x|y>"), "&lt;!channel&gt; a &amp; &lt;https://x|y&gt;");
    }

    #[test]
    fn default_is_failures_only() {
        let t = NotifyTarget { webhook: "x".into(), on: None, format: None, secrets: vec![] };
        assert!(wanted(&t, RunResult::Failed));
        assert!(wanted(&t, RunResult::Errored));
        assert!(!wanted(&t, RunResult::Passed));
        assert!(!wanted(&t, RunResult::Cancelled));
    }
}
