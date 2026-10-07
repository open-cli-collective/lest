//! Deterministic one-line failure descriptions. Every failed step gets one;
//! an AI provider, when configured, may add a longer explanation beside it.

use crate::report::{StepReport, StepStatus};
use crate::spec::StepKind;

/// A short description of why the step failed, or `None` if it did not.
pub fn headline(step: &StepReport) -> Option<String> {
    let status = step.status?;
    if !status.is_failure() || !step.children.is_empty() {
        return None;
    }
    let error = step.error.as_deref().unwrap_or("");
    if !error.is_empty() && step.kind != Some(StepKind::Run) {
        return Some(clip(error));
    }
    if let Some(code) = step.http_status
        && code >= 400
    {
        let detail = json_message(&step.stdout).map(|m| format!(": {m}")).unwrap_or_default();
        let what = step.resolved.as_deref().unwrap_or("request");
        return Some(clip(&format!("HTTP {code} from {what}{detail}")));
    }
    if !error.is_empty() && step.exit_code.is_none() {
        return Some(clip(error));
    }
    if let Some(code) = step.exit_code
        && code != 0
    {
        let line = last_meaningful_line(&step.stderr).or_else(|| last_meaningful_line(&step.stdout));
        return Some(clip(&match line {
            Some(l) => format!("exited {code}: {l}"),
            None => format!("exited {code} with no output"),
        }));
    }
    if !error.is_empty() {
        return Some(clip(error));
    }
    Some(match status {
        StepStatus::Errored => "could not be evaluated".to_string(),
        _ => "failed".to_string(),
    })
}

/// The last line that looks like an error rather than noise.
fn last_meaningful_line(text: &str) -> Option<String> {
    let lines: Vec<&str> = text.lines().map(str::trim).filter(|l| !l.is_empty()).collect();
    let errorish = lines.iter().rev().find(|l| {
        let lower = l.to_lowercase();
        [
            "error",
            "fail",
            "denied",
            "unauthorized",
            "forbidden",
            "not found",
            "expired",
            "invalid",
            "refused",
            "timeout",
            "timed out",
        ]
        .iter()
        .any(|w| lower.contains(w))
    });
    errorish.or(lines.last()).map(|s| s.to_string())
}

/// `message` or `error` from a JSON body.
fn json_message(body: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(body).ok()?;
    for key in ["message", "error", "detail", "title"] {
        match v.get(key) {
            Some(serde_json::Value::String(s)) => return Some(s.clone()),
            Some(serde_json::Value::Object(o)) => {
                if let Some(serde_json::Value::String(s)) = o.get("message") {
                    return Some(s.clone());
                }
            }
            _ => {}
        }
    }
    None
}

fn clip(s: &str) -> String {
    let one = s.lines().next().unwrap_or("").trim();
    if one.chars().count() > 240 {
        format!("{}…", one.chars().take(240).collect::<String>())
    } else {
        one.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn step(kind: StepKind, status: StepStatus) -> StepReport {
        StepReport { id: "s".into(), kind: Some(kind), status: Some(status), ..Default::default() }
    }

    #[test]
    fn prefers_errorish_stderr_lines() {
        let mut s = step(StepKind::Run, StepStatus::Failed);
        s.exit_code = Some(2);
        s.stderr = "loading config\nerror: token expired\nbye\n".into();
        assert_eq!(headline(&s).unwrap(), "exited 2: error: token expired");
    }

    #[test]
    fn http_status_with_body_message() {
        let mut s = step(StepKind::Http, StepStatus::Failed);
        s.http_status = Some(404);
        s.resolved = Some("GET http://x/users/1".into());
        s.stdout = r#"{"error":{"message":"no such user"}}"#.into();
        assert_eq!(headline(&s).unwrap(), "HTTP 404 from GET http://x/users/1: no such user");
    }

    #[test]
    fn passed_steps_have_none() {
        assert!(headline(&step(StepKind::Run, StepStatus::Passed)).is_none());
    }
}
