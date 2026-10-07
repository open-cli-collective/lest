//! The run report: the canonical record of a run. Every surface (CLI, UI,
//! notifications) reads this; live events are a preview of it.

use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::spec::{CleanupPolicy, StepKind};

pub const REPORT_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum RunResult {
    Passed,
    Failed,
    /// Could not run or could not be evaluated (preflight, invalid input).
    Errored,
    Cancelled,
}

impl RunResult {
    pub fn as_str(&self) -> &'static str {
        match self {
            RunResult::Passed => "passed",
            RunResult::Failed => "failed",
            RunResult::Errored => "errored",
            RunResult::Cancelled => "cancelled",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum StepStatus {
    Pending,
    Running,
    Passed,
    /// Ran and the result was wrong.
    Failed,
    /// Could not be evaluated: spawn error, timeout, cancel, expression error.
    Errored,
    Skipped,
}

impl StepStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            StepStatus::Pending => "pending",
            StepStatus::Running => "running",
            StepStatus::Passed => "passed",
            StepStatus::Failed => "failed",
            StepStatus::Errored => "errored",
            StepStatus::Skipped => "skipped",
        }
    }
    pub fn is_failure(&self) -> bool {
        matches!(self, StepStatus::Failed | StepStatus::Errored)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct RunReport {
    pub schema_version: u32,
    pub run_id: String,
    pub flow_id: String,
    pub flow_name: String,
    /// Relative to the project root.
    pub flow_path: String,
    /// SHA-256 of every flow file the run loaded, by path.
    pub flow_sha256: BTreeMap<String, String>,
    pub project_dir: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub environment: Option<String>,
    pub started_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub finished_at: Option<String>,
    pub duration_ms: u64,
    pub result: RunResult,
    /// Why the run errored before or outside a step (preflight, inputs).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// Problems that did not fail the run (a failed cleanup, a failed step
    /// with `continueOnError`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
    pub inputs: BTreeMap<String, String>,
    pub host: HostInfo,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tools: Vec<ToolReport>,
    pub steps: Vec<StepReport>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub finally: Vec<StepReport>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub cleanups: Vec<CleanupReport>,
    /// Set when the run was resumed from a step of an earlier run.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resumed_from: Option<ResumeInfo>,
    /// The demo deliverables, when the flow recorded a demo.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub demo: Option<DemoReport>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ResumeInfo {
    pub run_id: String,
    pub step: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct HostInfo {
    pub lest_version: String,
    pub os: String,
    pub arch: String,
}

impl HostInfo {
    pub fn current() -> Self {
        HostInfo {
            lest_version: env!("CARGO_PKG_VERSION").to_string(),
            os: std::env::consts::OS.to_string(),
            arch: std::env::consts::ARCH.to_string(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ToolReport {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub min_version: Option<String>,
    pub status: ToolStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum ToolStatus {
    Ready,
    Missing,
    TooOld,
    SignedOut,
    /// Found, but the version could not be compared.
    Unknown,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct StepReport {
    /// Path-qualified id: `sign_in`, or `call/inner` inside a called flow.
    pub id: String,
    pub name: String,
    pub kind: Option<StepKind>,
    pub status: Option<StepStatus>,
    pub attempts: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub started_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub finished_at: Option<String>,
    /// Cumulative across attempts.
    pub duration_ms: u64,
    /// What ran: the command, `GET <url>`, the expression.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resolved: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    #[serde(skip_serializing_if = "String::is_empty", default)]
    pub stdout: String,
    #[serde(skip_serializing_if = "String::is_empty", default)]
    pub stderr: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub http_status: Option<u16>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub outputs: BTreeMap<String, serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// A one-line, deterministic description of the failure.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub headline: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub artifacts: Vec<Artifact>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub beats: Vec<Beat>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub children: Vec<StepReport>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Artifact {
    pub label: String,
    /// Relative to the run directory.
    pub path: String,
    pub mime: String,
    pub bytes: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Beat {
    pub marker: String,
    /// Milliseconds since the recording started.
    pub at_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct CleanupReport {
    pub step_id: String,
    pub command: String,
    /// The cleanup's own `env`, resolved; secret values are redacted.
    pub env: BTreeMap<String, String>,
    /// The run variables the step saw (vars, inputs, `LEST_*`), so a later
    /// `lest cleanup` replays in the same environment.
    #[serde(default)]
    pub context: BTreeMap<String, String>,
    /// Working directory.
    #[serde(default)]
    pub cwd: String,
    pub policy: CleanupPolicy,
    pub status: CleanupStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    #[serde(skip_serializing_if = "String::is_empty", default)]
    pub output: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum CleanupStatus {
    Pending,
    Passed,
    Failed,
    /// Not run: manual policy, or on-failure after a pass.
    NotRun,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct DemoReport {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub video: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub raw_video: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub chapters: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub beat_sheet: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub raw_duration_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl RunReport {
    /// Every step report, depth first.
    pub fn all_steps(&self) -> Vec<&StepReport> {
        fn walk<'a>(s: &'a StepReport, out: &mut Vec<&'a StepReport>) {
            out.push(s);
            for c in &s.children {
                walk(c, out);
            }
        }
        let mut out = Vec::new();
        for s in self.steps.iter().chain(self.finally.iter()) {
            walk(s, &mut out);
        }
        out
    }

    pub fn find_step(&self, id: &str) -> Option<&StepReport> {
        self.all_steps().into_iter().find(|s| s.id == id)
    }

    /// The first failed leaf step.
    pub fn first_failure(&self) -> Option<&StepReport> {
        self.all_steps().into_iter().find(|s| s.status.is_some_and(|st| st.is_failure()) && s.children.is_empty())
    }
}

pub fn report_schema() -> serde_json::Value {
    serde_json::to_value(schemars::schema_for!(RunReport)).expect("schema serializes")
}
