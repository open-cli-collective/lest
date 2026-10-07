//! Tool preflight: is each CLI a flow needs installed, new enough and
//! signed in? Runs check, then heal, then check again, in dependency order.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::LazyLock;
use std::time::Duration;

use regex::Regex;
use tokio_util::sync::CancellationToken;

use crate::process::{self, ProcessSpec};
use crate::project::ToolProfile;
use crate::report::{ToolReport, ToolStatus};

static VERSION: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\d+(?:\.\d+)+").expect("re"));

/// What a flow needs from one tool.
#[derive(Debug, Clone, PartialEq)]
pub struct Need {
    pub name: String,
    pub min_version: Option<String>,
    pub auth: Option<bool>,
}

/// Orders tools so every tool comes after the tools it requires, adding
/// required tools that were not named.
pub fn order(needs: &[Need], profiles: &BTreeMap<String, ToolProfile>) -> Vec<Need> {
    fn visit(
        name: &str,
        needs: &BTreeMap<String, Need>,
        profiles: &BTreeMap<String, ToolProfile>,
        seen: &mut BTreeSet<String>,
        out: &mut Vec<Need>,
    ) {
        if !seen.insert(name.to_string()) {
            return;
        }
        if let Some(p) = profiles.get(name) {
            for dep in &p.requires {
                visit(dep, needs, profiles, seen, out);
            }
        }
        out.push(needs.get(name).cloned().unwrap_or(Need { name: name.to_string(), min_version: None, auth: None }));
    }
    let by_name: BTreeMap<String, Need> = needs.iter().map(|n| (n.name.clone(), n.clone())).collect();
    let mut seen = BTreeSet::new();
    let mut out = Vec::new();
    for n in needs {
        visit(&n.name, &by_name, profiles, &mut seen, &mut out);
    }
    out
}

/// Compares dotted versions numerically; `None` when either is not a
/// version (a commit hash, a date).
pub fn compare_versions(found: &str, min: &str) -> Option<std::cmp::Ordering> {
    let parse = |s: &str| -> Option<Vec<u64>> {
        let m = VERSION.find(s).map(|m| m.as_str()).or_else(|| s.trim().parse::<u64>().ok().map(|_| s.trim()))?;
        m.split('.').map(|p| p.parse().ok()).collect()
    };
    let (mut a, mut b) = (parse(found)?, parse(min)?);
    let len = a.len().max(b.len());
    a.resize(len, 0);
    b.resize(len, 0);
    Some(a.cmp(&b))
}

/// Outcome of checking one tool.
#[derive(Debug, Clone)]
pub struct Checked {
    pub report: ToolReport,
    /// The interactive login command, when the tool is signed out and has one.
    pub login: Option<String>,
}

async fn sh(
    cmd: &str,
    cwd: &Path,
    env: &[(String, String)],
    env_remove: &[String],
    timeout: Duration,
    cancel: &CancellationToken,
) -> process::ProcessResult {
    let (program, args) = process::shell_command(Some("sh"), cmd);
    process::run(
        ProcessSpec {
            program,
            args,
            cwd: cwd.to_path_buf(),
            env: env.to_vec(),
            env_remove: env_remove.to_vec(),
            stdin: None,
            timeout,
            cap: 64 * 1024,
        },
        cancel.clone(),
        |_, _| {},
    )
    .await
}

fn first_line(r: &process::ProcessResult) -> String {
    r.spawn_error
        .clone()
        .or_else(|| r.stderr.lines().chain(r.stdout.lines()).map(str::trim).find(|l| !l.is_empty()).map(str::to_string))
        .unwrap_or_else(|| format!("exited {}", r.exit_code.unwrap_or(-1)))
}

/// Checks one tool. `env` pins the tool to the run's target.
pub async fn check(
    need: &Need,
    profile: Option<&ToolProfile>,
    cwd: &Path,
    env: &[(String, String)],
    env_remove: &[String],
    cancel: &CancellationToken,
) -> Checked {
    let mut report = ToolReport {
        name: need.name.clone(),
        path: None,
        version: None,
        min_version: need.min_version.clone(),
        status: ToolStatus::Ready,
        message: None,
    };
    let hint = profile.and_then(|p| p.hint.clone());
    let Ok(path) = which::which(&need.name) else {
        report.status = ToolStatus::Missing;
        report.message = Some(match hint {
            Some(h) => format!("{} is not installed or not on PATH. {h}", need.name),
            None => format!("{} is not installed or not on PATH", need.name),
        });
        return Checked { report, login: None };
    };
    report.path = Some(path.display().to_string());

    let version_cmd =
        profile.and_then(|p| p.version.clone()).unwrap_or_else(|| format!("{} --version", shell_quote(&need.name)));
    let v = sh(&version_cmd, cwd, env, env_remove, Duration::from_secs(15), cancel).await;
    let text = format!("{}\n{}", v.stdout, v.stderr);
    report.version = VERSION.find(&text).map(|m| m.as_str().to_string());
    if let Some(min) = &need.min_version {
        match report.version.as_deref().and_then(|found| compare_versions(found, min)) {
            Some(std::cmp::Ordering::Less) => {
                report.status = ToolStatus::TooOld;
                let update = profile.and_then(|p| p.update.clone());
                report.message = Some(format!(
                    "{} {} is older than the required {min}{}",
                    need.name,
                    report.version.as_deref().unwrap_or("?"),
                    update.map(|u| format!("; update with `{u}`")).unwrap_or_default()
                ));
                return Checked { report, login: None };
            }
            Some(_) => {}
            None => {
                report.status = ToolStatus::Unknown;
                report.message = Some(format!(
                    "could not compare version '{}' with {min}",
                    report.version.as_deref().unwrap_or("?")
                ));
            }
        }
    }

    let Some(p) = profile else { return Checked { report, login: None } };
    let wants_auth = need.auth.unwrap_or(true);
    let Some(check_cmd) = p.check.as_deref().filter(|_| wants_auth) else {
        return Checked { report, login: None };
    };
    let timeout = Duration::from_secs(30);
    let first = sh(check_cmd, cwd, env, env_remove, timeout, cancel).await;
    if first.success() {
        return Checked { report, login: None };
    }
    let mut why = first_line(&first);
    if let Some(heal) = &p.heal {
        let healed = sh(heal, cwd, env, env_remove, Duration::from_secs(120), cancel).await;
        if healed.success() {
            let again = sh(check_cmd, cwd, env, env_remove, timeout, cancel).await;
            if again.success() {
                report.message = Some("signed in after refresh".to_string());
                return Checked { report, login: None };
            }
            why = first_line(&again);
        } else {
            why = format!("{why}; refresh failed: {}", first_line(&healed));
        }
    }
    report.status = ToolStatus::SignedOut;
    report.message = Some(match (&p.login, &hint) {
        (Some(l), _) => format!("{} is not signed in ({why}); sign in with `{l}`", need.name),
        (None, Some(h)) => format!("{} is not signed in ({why}). {h}", need.name),
        (None, None) => format!("{} is not signed in ({why})", need.name),
    });
    Checked { report, login: p.login.clone() }
}

fn shell_quote(s: &str) -> String {
    if s.chars().all(|c| c.is_ascii_alphanumeric() || "-_./".contains(c)) {
        s.to_string()
    } else {
        format!("'{}'", s.replace('\'', "'\\''"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cmp::Ordering;

    #[test]
    fn compares_versions_numerically() {
        assert_eq!(compare_versions("gh version 2.40.1 (2024-01-01)", "2.9"), Some(Ordering::Greater));
        assert_eq!(compare_versions("1.2", "1.2.0"), Some(Ordering::Equal));
        assert_eq!(compare_versions("v0.9.9", "1.0"), Some(Ordering::Less));
        assert_eq!(compare_versions("abc123", "1.0"), None);
    }

    #[test]
    fn orders_dependencies_first() {
        let mut profiles = BTreeMap::new();
        profiles.insert("api".to_string(), ToolProfile { requires: vec!["vault".into()], ..Default::default() });
        let needs = vec![Need { name: "api".into(), min_version: None, auth: None }];
        let names: Vec<String> = order(&needs, &profiles).into_iter().map(|n| n.name).collect();
        assert_eq!(names, vec!["vault", "api"]);
    }

    #[tokio::test]
    async fn check_heal_recheck() {
        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join("healed");
        let profile = ToolProfile {
            version: Some("echo 1.0.0".into()),
            check: Some(format!("test -f {}", marker.display())),
            heal: Some(format!("touch {}", marker.display())),
            login: Some("sh-login".into()),
            ..Default::default()
        };
        let need = Need { name: "sh".into(), min_version: Some("0.5".into()), auth: None };
        let c = check(&need, Some(&profile), dir.path(), &[], &[], &CancellationToken::new()).await;
        assert_eq!(c.report.status, ToolStatus::Ready, "{:?}", c.report);
        assert_eq!(c.report.version.as_deref(), Some("1.0.0"));

        std::fs::remove_file(&marker).unwrap();
        let broken = ToolProfile { heal: Some("false".into()), ..profile };
        let c = check(&need, Some(&broken), dir.path(), &[], &[], &CancellationToken::new()).await;
        assert_eq!(c.report.status, ToolStatus::SignedOut);
        assert_eq!(c.login.as_deref(), Some("sh-login"));
    }

    #[tokio::test]
    async fn missing_tool_is_reported() {
        let need = Need { name: "no-such-tool-lest".into(), min_version: None, auth: None };
        let c = check(&need, None, Path::new("."), &[], &[], &CancellationToken::new()).await;
        assert_eq!(c.report.status, ToolStatus::Missing);
    }
}
