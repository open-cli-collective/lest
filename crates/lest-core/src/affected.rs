//! Change-based selection: which flows does a set of changed files touch?
//!
//! A flow is affected when a changed path matches one of its `affects:`
//! globs, when its own file or a browser script it runs changed, when the
//! project's `lest.yaml` changed, or when a flow it calls is affected.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::process::Command;

use globset::{GlobBuilder, GlobSetBuilder};
use serde::Serialize;

use crate::catalog::Catalog;
use crate::project::PROJECT_FILE;
use crate::spec::Step;

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Affected {
    pub flow_id: String,
    /// Why: the matching paths, `flow file changed`, or `calls <flow>`.
    pub reasons: Vec<String>,
}

fn scripts(steps: &[Step], out: &mut Vec<String>) {
    for s in steps {
        if let Some(script) = s.browser.as_ref().and_then(|b| b.script.clone()) {
            out.push(script);
        }
        scripts(s.children(), out);
    }
}

/// Joins a path relative to a flow's directory onto the flow's project
/// path, resolving `..`.
fn project_relative(flow_rel: &str, script: &str) -> String {
    let mut parts: Vec<&str> = flow_rel.split('/').collect();
    parts.pop();
    for seg in script.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            s => parts.push(s),
        }
    }
    parts.join("/")
}

/// Flows affected by `changed` paths (relative to the project root).
///
/// In `affects:` globs, `*` stays within one path segment and `**` crosses
/// directories.
pub fn affected(catalog: &Catalog, changed: &[String]) -> Result<Vec<Affected>, String> {
    let config_changed = changed.iter().any(|c| c == PROJECT_FILE);
    let mut direct: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for lf in &catalog.flows {
        let mut reasons = Vec::new();
        if config_changed {
            reasons.push(format!("{PROJECT_FILE} changed"));
        }
        if changed.iter().any(|c| c == &lf.rel_path) {
            reasons.push("flow file changed".to_string());
        }
        let mut s = Vec::new();
        scripts(&lf.flow.steps, &mut s);
        scripts(&lf.flow.finally, &mut s);
        for script in s {
            let path = project_relative(&lf.rel_path, &script);
            if changed.contains(&path) {
                reasons.push(path);
            }
        }
        if !lf.flow.affects.is_empty() {
            let mut b = GlobSetBuilder::new();
            for g in &lf.flow.affects {
                let glob = GlobBuilder::new(g)
                    .literal_separator(true)
                    .build()
                    .map_err(|e| format!("{}: affects '{g}': {e}", lf.rel_path))?;
                b.add(glob);
            }
            let set = b.build().map_err(|e| e.to_string())?;
            reasons.extend(changed.iter().filter(|c| set.is_match(c.as_str())).cloned());
        }
        reasons.dedup();
        if !reasons.is_empty() {
            direct.insert(lf.flow.id.clone(), reasons);
        }
    }
    // Callers of affected flows are affected too, transitively.
    fn calls(steps: &[Step], out: &mut BTreeSet<String>) {
        for s in steps {
            if let Some(f) = &s.flow {
                out.insert(f.clone());
            }
            calls(s.children(), out);
        }
    }
    let callees: BTreeMap<String, BTreeSet<String>> = catalog
        .flows
        .iter()
        .map(|lf| {
            let mut c = BTreeSet::new();
            calls(&lf.flow.steps, &mut c);
            calls(&lf.flow.finally, &mut c);
            (lf.flow.id.clone(), c)
        })
        .collect();
    let mut result = direct;
    loop {
        let mut added = false;
        for (caller, cs) in &callees {
            for callee in cs {
                let reason = format!("calls {callee}");
                if result.contains_key(callee) && !result.get(caller).is_some_and(|r| r.contains(&reason)) {
                    result.entry(caller.clone()).or_default().push(reason);
                    added = true;
                }
            }
        }
        if !added {
            break;
        }
    }
    Ok(result.into_iter().map(|(flow_id, reasons)| Affected { flow_id, reasons }).collect())
}

fn git(dir: &Path, args: &[&str]) -> Result<Vec<u8>, String> {
    let out = Command::new("git").args(args).current_dir(dir).output().map_err(|e| format!("git: {e}"))?;
    if out.status.success() {
        Ok(out.stdout)
    } else {
        Err(format!("git {}: {}", args.join(" "), String::from_utf8_lossy(&out.stderr).trim()))
    }
}

/// NUL-separated git output, so paths with any characters come through.
fn nul_paths(out: &[u8]) -> impl Iterator<Item = String> + '_ {
    out.split(|b| *b == 0).filter(|p| !p.is_empty()).map(|p| String::from_utf8_lossy(p).into_owned())
}

/// Files changed since `base` (committed on this branch, staged, unstaged
/// and untracked), relative to the project root. A rename counts as both
/// its old and its new path.
pub fn changed_since(project_root: &Path, base: &str) -> Result<Vec<String>, String> {
    let top = String::from_utf8_lossy(&git(project_root, &["rev-parse", "--show-toplevel"])?).trim().to_string();
    let top = std::fs::canonicalize(&top).map_err(|e| e.to_string())?;
    let root = std::fs::canonicalize(project_root).map_err(|e| e.to_string())?;
    let merge_base = String::from_utf8_lossy(&git(project_root, &["merge-base", base, "HEAD"])?).trim().to_string();
    let mut files = BTreeSet::new();
    // --no-renames: both paths of a rename; --no-relative: repository paths
    // whatever diff.relative is set to.
    let diff = git(project_root, &["diff", "--name-only", "--no-renames", "--no-relative", "-z", &merge_base])?;
    files.extend(nul_paths(&diff));
    let untracked = git(&top, &["ls-files", "--others", "--exclude-standard", "-z"])?;
    files.extend(nul_paths(&untracked));
    // Git paths are relative to the repository; flows use the project root.
    let prefix = root.strip_prefix(&top).map_err(|_| "the project is outside the git repository".to_string())?;
    let prefix = prefix.to_string_lossy().replace('\\', "/");
    Ok(files
        .into_iter()
        .filter_map(
            |f| if prefix.is_empty() { Some(f) } else { f.strip_prefix(&format!("{prefix}/")).map(str::to_string) },
        )
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::{LoadedFlow, parse_flow};

    fn catalog(flows: &[(&str, &str)]) -> Catalog {
        let mut c = Catalog::default();
        for (path, text) in flows {
            c.flows.push(LoadedFlow {
                path: format!("/p/{path}").into(),
                rel_path: path.to_string(),
                flow: parse_flow(text).unwrap(),
                sha256: String::new(),
            });
        }
        c
    }

    fn by(a: &[Affected]) -> BTreeMap<&str, Vec<String>> {
        a.iter().map(|x| (x.flow_id.as_str(), x.reasons.clone())).collect()
    }

    #[test]
    fn matches_globs_own_files_scripts_and_callers() {
        let c = catalog(&[
            (
                "flows/login.lest.yaml",
                "apiVersion: lest/v1\nid: login\nname: L\naffects: ['src/auth/**']\nsteps: [{id: a, browser: {script: ../scripts/login.mjs}}]\n",
            ),
            ("flows/suite.lest.yaml", "apiVersion: lest/v1\nid: suite\nname: S\nsteps: [{id: l, flow: login}]\n"),
            (
                "flows/other.lest.yaml",
                "apiVersion: lest/v1\nid: other\nname: O\naffects: ['docs/*.md']\nsteps: [{id: a, run: 'true'}]\n",
            ),
        ]);
        let a = affected(
            &c,
            &[
                "src/auth/session.ts".into(),
                "flows/other.lest.yaml".into(),
                "scripts/login.mjs".into(),
                "docs/deep/x.md".into(),
            ],
        )
        .unwrap();
        let m = by(&a);
        assert_eq!(m["login"], vec!["scripts/login.mjs", "src/auth/session.ts"]);
        assert_eq!(m["suite"], vec!["calls login"]);
        // `*` does not cross `/`, so docs/deep/x.md is not a match.
        assert_eq!(m["other"], vec!["flow file changed"]);
    }

    #[test]
    fn a_project_config_change_affects_every_flow() {
        let c =
            catalog(&[("flows/a.lest.yaml", "apiVersion: lest/v1\nid: a\nname: A\nsteps: [{id: x, run: 'true'}]\n")]);
        let a = affected(&c, &["lest.yaml".into()]).unwrap();
        assert_eq!(by(&a)["a"], vec!["lest.yaml changed"]);
    }

    #[test]
    fn script_paths_resolve_against_the_flow_directory() {
        assert_eq!(project_relative("flows/web/a.lest.yaml", "../../scripts/x.mjs"), "scripts/x.mjs");
        assert_eq!(project_relative("a.lest.yaml", "x.mjs"), "x.mjs");
    }
}
