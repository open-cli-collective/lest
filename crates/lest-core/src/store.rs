//! Run storage: `<data>/runs/<flow-id>/<run-id>/report.json` plus
//! `artifacts/`. Run ids sort by start time, so listing needs no index.

use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::paths::{StateRoots, write_private};
use crate::report::{RunReport, RunResult};

#[derive(Debug, Clone)]
pub struct Store {
    roots: StateRoots,
}

/// A run in history.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RunSummary {
    pub run_id: String,
    pub flow_id: String,
    pub flow_name: String,
    pub environment: Option<String>,
    pub started_at: String,
    pub duration_ms: u64,
    pub result: RunResult,
    pub warnings: usize,
    pub project_dir: String,
    pub has_video: bool,
}

impl From<&RunReport> for RunSummary {
    fn from(r: &RunReport) -> Self {
        RunSummary {
            run_id: r.run_id.clone(),
            flow_id: r.flow_id.clone(),
            flow_name: r.flow_name.clone(),
            environment: r.environment.clone(),
            started_at: r.started_at.clone(),
            duration_ms: r.duration_ms,
            result: r.result,
            warnings: r.warnings.len(),
            project_dir: r.project_dir.clone(),
            has_video: r.demo.as_ref().is_some_and(|d| d.video.is_some()),
        }
    }
}

impl Store {
    pub fn new(roots: StateRoots) -> Store {
        Store { roots }
    }

    pub fn roots(&self) -> &StateRoots {
        &self.roots
    }

    pub fn run_dir(&self, flow_id: &str, run_id: &str) -> PathBuf {
        self.roots.runs_dir().join(flow_id).join(run_id)
    }

    pub fn report_path(&self, flow_id: &str, run_id: &str) -> PathBuf {
        self.run_dir(flow_id, run_id).join("report.json")
    }

    pub fn write(&self, report: &RunReport) -> std::io::Result<PathBuf> {
        let path = self.report_path(&report.flow_id, &report.run_id);
        let json = serde_json::to_vec_pretty(report).map_err(std::io::Error::other)?;
        write_private(&path, &json)?;
        Ok(path)
    }

    pub fn read(&self, flow_id: &str, run_id: &str) -> anyhow::Result<RunReport> {
        read_report(&self.report_path(flow_id, run_id))
    }

    /// Finds a run by id or unique id prefix, across flows.
    pub fn find(&self, run_id: &str) -> anyhow::Result<RunReport> {
        let mut matches = Vec::new();
        for flow_dir in read_dirs(&self.roots.runs_dir()) {
            for run_dir in read_dirs(&flow_dir) {
                let name = run_dir.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                if name == run_id {
                    return read_report(&run_dir.join("report.json"));
                }
                if name.starts_with(run_id) {
                    matches.push(run_dir);
                }
            }
        }
        match matches.as_slice() {
            [one] => read_report(&one.join("report.json")),
            [] => anyhow::bail!("no run '{run_id}'"),
            _ => anyhow::bail!("'{run_id}' matches {} runs; give more of the id", matches.len()),
        }
    }

    /// Runs newest first, optionally for one flow.
    pub fn list(&self, flow_id: Option<&str>, limit: usize) -> Vec<RunSummary> {
        let flow_dirs: Vec<PathBuf> = match flow_id {
            Some(id) => vec![self.roots.runs_dir().join(id)],
            None => read_dirs(&self.roots.runs_dir()),
        };
        let mut runs: Vec<PathBuf> = flow_dirs.iter().flat_map(|d| read_dirs(d)).collect();
        runs.sort_by(|a, b| b.file_name().cmp(&a.file_name()));
        runs.into_iter()
            .filter_map(|d| read_report(&d.join("report.json")).ok())
            .take(limit)
            .map(|r| RunSummary::from(&r))
            .collect()
    }

    /// The newest run of a flow.
    pub fn latest(&self, flow_id: &str) -> Option<RunReport> {
        let mut runs = read_dirs(&self.roots.runs_dir().join(flow_id));
        runs.sort_by(|a, b| b.file_name().cmp(&a.file_name()));
        runs.into_iter().find_map(|d| read_report(&d.join("report.json")).ok())
    }

    /// Deletes all but the newest `keep` runs of each flow. Returns the
    /// removed run directories.
    pub fn prune(&self, keep: usize, dry_run: bool) -> std::io::Result<Vec<PathBuf>> {
        let mut removed = Vec::new();
        for flow_dir in read_dirs(&self.roots.runs_dir()) {
            let mut runs = read_dirs(&flow_dir);
            runs.sort_by(|a, b| b.file_name().cmp(&a.file_name()));
            for old in runs.into_iter().skip(keep) {
                if !dry_run {
                    std::fs::remove_dir_all(&old)?;
                }
                removed.push(old);
            }
        }
        Ok(removed)
    }
}

pub fn read_report(path: &Path) -> anyhow::Result<RunReport> {
    let text = std::fs::read_to_string(path).map_err(|e| anyhow::anyhow!("cannot read {}: {e}", path.display()))?;
    serde_json::from_str(&text).map_err(|e| anyhow::anyhow!("{}: {e}", path.display()))
}

fn read_dirs(dir: &Path) -> Vec<PathBuf> {
    std::fs::read_dir(dir)
        .map(|rd| {
            rd.filter_map(Result::ok).filter(|e| e.file_type().is_ok_and(|t| t.is_dir())).map(|e| e.path()).collect()
        })
        .unwrap_or_default()
}

/// A run id: UTC start time plus a random suffix, so ids sort by start.
pub fn new_run_id() -> String {
    let now = time::OffsetDateTime::now_utc();
    let fmt = time::macros::format_description!("[year][month][day]T[hour][minute][second]Z");
    let suffix: String = (0..4)
        .map(|_| {
            let n = rand::random_range(0..36u32);
            std::char::from_digit(n, 36).expect("digit")
        })
        .collect();
    format!("{}-{suffix}", now.format(&fmt).expect("format"))
}
