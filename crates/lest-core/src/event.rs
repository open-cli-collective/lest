//! Live run events. Every event names its run, so any number of runs can be
//! watched at once. The report on disk stays canonical.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::report::{Artifact, Beat, RunResult, StepReport, StepStatus, ToolReport};
use crate::spec::StepKind;

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct RunEvent {
    pub run_id: String,
    /// Increases by one per event within a run.
    pub seq: u64,
    /// Milliseconds since the Unix epoch.
    pub at_ms: u64,
    #[serde(flatten)]
    pub body: EventBody,
}

/// The steps a run will execute, as a tree.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct PlanNode {
    pub id: String,
    pub name: String,
    pub kind: StepKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub children: Vec<PlanNode>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum EventBody {
    #[serde(rename_all = "camelCase")]
    RunStarted {
        flow_id: String,
        flow_name: String,
        environment: Option<String>,
        run_dir: String,
        steps: Vec<PlanNode>,
        finally: Vec<PlanNode>,
        tools: Vec<String>,
    },
    /// A preflight check of one tool.
    #[serde(rename_all = "camelCase")]
    Tool { tool: ToolReport },
    /// A tool needs an interactive sign-in; the UI offers a button.
    #[serde(rename_all = "camelCase")]
    SignInNeeded { tool: String, command: String },
    #[serde(rename_all = "camelCase")]
    StepStarted { step_id: String, attempt: u32 },
    /// One line of a step's output, secrets redacted.
    #[serde(rename_all = "camelCase")]
    StepOutput { step_id: String, stream: Stream, line: String },
    #[serde(rename_all = "camelCase")]
    StepRetrying { step_id: String, attempt: u32, max_attempts: u32, reason: String, next_attempt_in_ms: u64 },
    #[serde(rename_all = "camelCase")]
    StepFinished { step: Box<StepReport> },
    #[serde(rename_all = "camelCase")]
    StepArtifact { step_id: String, artifact: Artifact },
    #[serde(rename_all = "camelCase")]
    StepBeat { step_id: String, beat: Beat },
    /// The browser step's DevTools endpoint and active page, for the live
    /// view.
    #[serde(rename_all = "camelCase")]
    BrowserPage { step_id: String, cdp_port: u16, target_id: String, url: String },
    #[serde(rename_all = "camelCase")]
    BrowserClosed { step_id: String },
    #[serde(rename_all = "camelCase")]
    CleanupStarted { step_id: String },
    #[serde(rename_all = "camelCase")]
    CleanupFinished { step_id: String, status: StepStatus },
    /// Post-production of a demo recording.
    #[serde(rename_all = "camelCase")]
    DemoProgress { message: String },
    /// A notification was sent after the run (or failed to send). These
    /// are the only events that follow `run_finished`.
    #[serde(rename_all = "camelCase")]
    Notified { ok: bool, message: String },
    #[serde(rename_all = "camelCase")]
    RunFinished { result: RunResult, duration_ms: u64, error: Option<String>, report_path: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum Stream {
    Stdout,
    Stderr,
    Browser,
}

impl RunEvent {
    pub fn step_id(&self) -> Option<&str> {
        match &self.body {
            EventBody::StepStarted { step_id, .. }
            | EventBody::StepOutput { step_id, .. }
            | EventBody::StepRetrying { step_id, .. }
            | EventBody::StepArtifact { step_id, .. }
            | EventBody::StepBeat { step_id, .. }
            | EventBody::BrowserPage { step_id, .. }
            | EventBody::BrowserClosed { step_id }
            | EventBody::CleanupStarted { step_id }
            | EventBody::CleanupFinished { step_id, .. } => Some(step_id),
            EventBody::StepFinished { step } => Some(&step.id),
            _ => None,
        }
    }
}
