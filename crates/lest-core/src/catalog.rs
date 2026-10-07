//! Loading every flow in a project.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::project::Project;
use crate::spec::{FLOW_SUFFIX, Flow};

/// A parsed flow file.
#[derive(Debug, Clone)]
pub struct LoadedFlow {
    pub path: PathBuf,
    /// Path relative to the project root, with `/` separators.
    pub rel_path: String,
    pub flow: Flow,
    pub sha256: String,
}

impl LoadedFlow {
    pub fn dir(&self) -> &Path {
        self.path.parent().unwrap_or(Path::new("."))
    }
}

/// A problem found while loading or validating.
#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Diagnostic {
    pub file: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub flow: Option<String>,
    /// Where in the flow: `steps.sign_in.retry.until`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub at: Option<String>,
    pub message: String,
    pub severity: Severity,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Error,
    Warning,
}

impl std::fmt::Display for Diagnostic {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let sev = match self.severity {
            Severity::Error => "error",
            Severity::Warning => "warning",
        };
        write!(f, "{}: {sev}", self.file)?;
        if let Some(at) = &self.at {
            write!(f, " at {at}")?;
        }
        write!(f, ": {}", self.message)
    }
}

#[derive(Debug, Clone, Default)]
pub struct Catalog {
    pub flows: Vec<LoadedFlow>,
    pub diagnostics: Vec<Diagnostic>,
}

const SKIP_DIRS: &[&str] = &["node_modules", "target", "dist", ".git", ".lest"];

impl Catalog {
    /// Loads every `*.lest.yaml` under the project's flow roots.
    pub fn load(project: &Project) -> Catalog {
        let mut files = Vec::new();
        for root in project.flow_roots() {
            for entry in walkdir::WalkDir::new(&root)
                .follow_links(true)
                .into_iter()
                .filter_entry(|e| {
                    let name = e.file_name().to_string_lossy();
                    !(e.file_type().is_dir()
                        && e.depth() > 0
                        && (name.starts_with('.') || SKIP_DIRS.contains(&name.as_ref())))
                })
                .filter_map(Result::ok)
            {
                if entry.file_type().is_file() && is_flow_file(entry.path()) {
                    files.push(entry.path().to_path_buf());
                }
            }
        }
        files.sort();
        files.dedup();
        let mut catalog = Catalog::default();
        for path in files {
            catalog.load_file(project, &path);
        }
        catalog.check_unique_ids();
        catalog
    }

    /// Loads a single file into the catalog.
    pub fn load_file(&mut self, project: &Project, path: &Path) {
        let rel = rel_path(&project.root, path);
        match std::fs::read_to_string(path) {
            Err(e) => self.diagnostics.push(error(&rel, None, None, format!("cannot read: {e}"))),
            Ok(text) => match parse_flow(&text) {
                Err(message) => self.diagnostics.push(error(&rel, None, None, message)),
                Ok(flow) => self.flows.push(LoadedFlow {
                    path: path.to_path_buf(),
                    rel_path: rel,
                    flow,
                    sha256: hex::encode(Sha256::digest(text.as_bytes())),
                }),
            },
        }
    }

    fn check_unique_ids(&mut self) {
        let mut seen: BTreeMap<String, String> = BTreeMap::new();
        for lf in &self.flows {
            if let Some(first) = seen.get(&lf.flow.id) {
                self.diagnostics.push(error(
                    &lf.rel_path,
                    Some(&lf.flow.id),
                    None,
                    format!("flow id '{}' is also used by {first}", lf.flow.id),
                ));
            } else {
                seen.insert(lf.flow.id.clone(), lf.rel_path.clone());
            }
        }
    }

    pub fn get(&self, id: &str) -> Option<&LoadedFlow> {
        self.flows.iter().find(|f| f.flow.id == id)
    }

    /// Finds a flow by id, by path (relative to `cwd` or the project root),
    /// or by a unique id prefix.
    pub fn resolve(&self, project: &Project, cwd: &Path, query: &str) -> Result<&LoadedFlow, String> {
        // A path names one file, even when its id is also another flow's.
        for candidate in [cwd.join(query), project.root.join(query)] {
            if let Ok(canon) = std::fs::canonicalize(&candidate)
                && let Some(f) =
                    self.flows.iter().find(|f| std::fs::canonicalize(&f.path).ok().as_deref() == Some(canon.as_path()))
            {
                return Ok(f);
            }
        }
        if let Some(f) = self.get(query) {
            return Ok(f);
        }
        let matches: Vec<&LoadedFlow> = self.flows.iter().filter(|f| f.flow.id.starts_with(query)).collect();
        match matches.as_slice() {
            [one] => Ok(one),
            [] => Err(format!("no flow '{query}' in {}", project.root.display())),
            many => Err(format!(
                "'{query}' matches several flows: {}",
                many.iter().map(|f| f.flow.id.as_str()).collect::<Vec<_>>().join(", ")
            )),
        }
    }
}

pub fn is_flow_file(path: &Path) -> bool {
    let name = path.file_name().map(|n| n.to_string_lossy()).unwrap_or_default();
    name.ends_with(FLOW_SUFFIX) || name.ends_with(".lest.yml")
}

/// Parses flow YAML, keeping the location of a syntax or schema error.
pub fn parse_flow(text: &str) -> Result<Flow, String> {
    serde_yaml_ng::from_str::<Flow>(text).map_err(|e| e.to_string())
}

pub fn rel_path(root: &Path, path: &Path) -> String {
    let canon_root = std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
    let canon = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    canon
        .strip_prefix(&canon_root)
        .unwrap_or(&canon)
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join("/")
}

pub(crate) fn error(file: &str, flow: Option<&str>, at: Option<&str>, message: String) -> Diagnostic {
    Diagnostic {
        file: file.to_string(),
        flow: flow.map(str::to_string),
        at: at.map(str::to_string),
        message,
        severity: Severity::Error,
    }
}

pub(crate) fn warning(file: &str, flow: Option<&str>, at: Option<&str>, message: String) -> Diagnostic {
    Diagnostic { severity: Severity::Warning, ..error(file, flow, at, message) }
}
