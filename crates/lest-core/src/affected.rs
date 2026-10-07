//! Change-based selection: which flows does a set of changed files touch?
//!
//! A flow is affected when a changed path matches one of its `affects:`
//! globs, when its own file changed, or when a flow it calls is affected.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::process::Command;

use globset::{Glob, GlobSetBuilder};
use serde::Serialize;

use crate::catalog::Catalog;
use crate::spec::Step;

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Affected {
    pub flow_id: String,
    /// Why: the matching paths, `flow file changed`, or `calls <flow>`.
    pub reasons: Vec<String>,
}

/// Flows affected by `changed` paths (relative to the project root).
pub fn affected(catalog: &Catalog, changed: &[String]) -> Result<Vec<Affected>, String> {
    let mut direct: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for lf in &catalog.flows {
        let mut reasons = Vec::new();
        if changed.iter().any(|c| c == &lf.rel_path) {
            reasons.push("flow file changed".to_string());
        }
        if !lf.flow.affects.is_empty() {
            let mut b = GlobSetBuilder::new();
            for g in &lf.flow.affects {
                b.add(Glob::new(g).map_err(|e| format!("{}: affects '{g}': {e}", lf.rel_path))?);
            }
            let set = b.build().map_err(|e| e.to_string())?;
            reasons.extend(changed.iter().filter(|c| set.is_match(c.as_str())).cloned());
        }
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
    let mut result: BTreeMap<String, Vec<String>> = direct.clone();
    loop {
        let mut added = false;
        for (caller, cs) in &callees {
            for callee in cs {
                if result.contains_key(callee)
                    && !result.get(caller).is_some_and(|r| r.contains(&format!("calls {callee}")))
                {
                    result.entry(caller.clone()).or_default().push(format!("calls {callee}"));
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

fn git(dir: &Path, args: &[&str]) -> Result<String, String> {
    let out = Command::new("git").args(args).current_dir(dir).output().map_err(|e| format!("git: {e}"))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        Err(format!("git {}: {}", args.join(" "), String::from_utf8_lossy(&out.stderr).trim()))
    }
}

/// Files changed since `base` (committed on this branch, staged, unstaged
/// and untracked), relative to the project root.
pub fn changed_since(project_root: &Path, base: &str) -> Result<Vec<String>, String> {
    let top = git(project_root, &["rev-parse", "--show-toplevel"])?;
    let top = std::fs::canonicalize(top.trim()).map_err(|e| e.to_string())?;
    let root = std::fs::canonicalize(project_root).map_err(|e| e.to_string())?;
    let merge_base = git(project_root, &["merge-base", base, "HEAD"])?;
    let mut files = BTreeSet::new();
    for out in [
        git(project_root, &["diff", "--name-only", merge_base.trim()])?,
        git(project_root, &["ls-files", "--others", "--exclude-standard", "--full-name"])?,
    ] {
        for line in out.lines().filter(|l| !l.is_empty()) {
            files.insert(line.to_string());
        }
    }
    // Git paths are relative to the repository; flows use the project root.
    let prefix = root.strip_prefix(&top).map_err(|_| "the project is outside the git repository".to_string())?;
    let prefix = prefix.to_string_lossy().replace('\\', "/");
    Ok(files
        .into_iter()
        .filter_map(
            |f| {
                if prefix.is_empty() { Some(f) } else { f.strip_prefix(&format!("{prefix}/")).map(str::to_string) }
            },
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

    #[test]
    fn matches_globs_own_files_and_callers() {
        let c = catalog(&[
            (
                "flows/login.lest.yaml",
                "apiVersion: lest/v1\nid: login\nname: L\naffects: ['src/auth/**']\nsteps: [{id: a, run: 'true'}]\n",
            ),
            ("flows/suite.lest.yaml", "apiVersion: lest/v1\nid: suite\nname: S\nsteps: [{id: l, flow: login}]\n"),
            (
                "flows/other.lest.yaml",
                "apiVersion: lest/v1\nid: other\nname: O\naffects: ['docs/**']\nsteps: [{id: a, run: 'true'}]\n",
            ),
        ]);
        let a = affected(&c, &["src/auth/session.ts".into(), "flows/other.lest.yaml".into()]).unwrap();
        let by: BTreeMap<_, _> = a.iter().map(|x| (x.flow_id.as_str(), x.reasons.clone())).collect();
        assert_eq!(by["login"], vec!["src/auth/session.ts"]);
        assert_eq!(by["suite"], vec!["calls login"]);
        assert_eq!(by["other"], vec!["flow file changed"]);
    }
}
