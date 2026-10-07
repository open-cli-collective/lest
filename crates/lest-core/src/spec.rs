//! The flow file format (`*.lest.yaml`, `apiVersion: lest/v1`).
//!
//! These types are the single source of truth: the JSON Schema is generated
//! from them (`lest schema`) and the validator works on them.

use std::collections::BTreeMap;
use std::time::Duration;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

pub const API_VERSION: &str = "lest/v1";
pub const FLOW_SUFFIX: &str = ".lest.yaml";

/// A flow: an ordered list of steps run against one environment.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Flow {
    /// Must be `lest/v1`.
    pub api_version: String,
    /// Stable identifier, unique in the project (`[a-z0-9][a-z0-9-]*`).
    /// Run history is keyed on it, so files can move freely.
    pub id: String,
    /// Display name.
    pub name: String,
    /// Markdown shown in the UI.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub owners: Vec<String>,
    /// Path globs (relative to the project root) whose changes this flow
    /// covers. Used by `lest affected`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub affects: Vec<String>,
    /// Named shared resources (an account, a fixture). Two runs that name
    /// the same resource never overlap.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub resources: Vec<String>,
    /// Per-environment variables. The selected block is merged over `vars`.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub environments: BTreeMap<String, BTreeMap<String, Scalar>>,
    /// Environment used when none is given.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_environment: Option<String>,
    /// Constant variables, exported to every process step.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub vars: BTreeMap<String, Scalar>,
    /// Values chosen per run (`--input name=value`, or the UI).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub inputs: Vec<Input>,
    /// Secret names this flow reads through `${{ secrets.<name> }}`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub secrets: Vec<String>,
    /// Command-line tools this flow needs. Checked before any step runs.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tools: Vec<ToolRequirement>,
    /// Values this flow returns to a calling `flow` step (CEL expressions).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub outputs: BTreeMap<String, String>,
    /// Presents the flow as a recordable demo.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub demo: Option<Demo>,
    /// Long-running processes (a local dev server) started before the steps
    /// and stopped after cleanup.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub services: Vec<Service>,
    pub steps: Vec<Step>,
    /// Steps that always run after `steps`, including after a failure or a
    /// cancel.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub finally: Vec<Step>,
}

/// A process the run starts before its steps and stops at the end. A
/// service with the same id in a called flow is started once.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Service {
    /// Identifier, unique across the flows of a run.
    pub id: String,
    /// Command to start it (in `sh`), relative to the flow file.
    pub run: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    /// Environment variables; values may contain `${{ }}` (vars, inputs,
    /// secrets).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub env: BTreeMap<String, String>,
    /// When the service counts as started.
    pub ready: Ready,
    /// If the ready check already passes before starting, use the running
    /// instance instead of starting another (default true).
    #[serde(default = "yes")]
    pub reuse: bool,
}

fn yes() -> bool {
    true
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Ready {
    /// A URL that answers with a status below 400 once the service is up;
    /// may contain `${{ }}`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub http: Option<String>,
    /// A regex matched against the service's output lines.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub log: Option<String>,
    /// How long to wait (default 30s).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout: Option<DurationSpec>,
}

/// A YAML scalar used as a variable value.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(untagged)]
pub enum Scalar {
    String(String),
    Bool(bool),
    Int(i64),
    Float(f64),
}

impl Scalar {
    /// The value as it is exported to a process environment.
    pub fn as_env_string(&self) -> String {
        match self {
            Scalar::String(s) => s.clone(),
            Scalar::Bool(b) => b.to_string(),
            Scalar::Int(i) => i.to_string(),
            Scalar::Float(f) => f.to_string(),
        }
    }

    pub fn to_json(&self) -> serde_json::Value {
        match self {
            Scalar::String(s) => serde_json::Value::String(s.clone()),
            Scalar::Bool(b) => serde_json::Value::Bool(*b),
            Scalar::Int(i) => serde_json::Value::from(*i),
            Scalar::Float(f) => serde_json::Value::from(*f),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Input {
    /// Identifier (`[A-Za-z_][A-Za-z0-9_]*`); exported to process steps
    /// under the same name.
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<Scalar>,
    /// A run fails before any step when a required input has no value.
    #[serde(default)]
    pub required: bool,
    /// Restricts the value to one of these. A choice can set further
    /// variables with `sets`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub choices: Vec<Choice>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Choice {
    pub value: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Variables this choice sets, merged over `vars`.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub sets: BTreeMap<String, Scalar>,
}

/// `gh` or `{name: gh, minVersion: "2.40"}`.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(untagged)]
pub enum ToolRequirement {
    Name(String),
    Detailed(ToolRequirementDetail),
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ToolRequirementDetail {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_version: Option<String>,
    /// Run the tool profile's auth check before the flow (default: true
    /// when the profile defines a check).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auth: Option<bool>,
}

impl ToolRequirement {
    pub fn name(&self) -> &str {
        match self {
            ToolRequirement::Name(n) => n,
            ToolRequirement::Detailed(d) => &d.name,
        }
    }
    pub fn min_version(&self) -> Option<&str> {
        match self {
            ToolRequirement::Name(_) => None,
            ToolRequirement::Detailed(d) => d.min_version.as_deref(),
        }
    }
    pub fn auth(&self) -> Option<bool> {
        match self {
            ToolRequirement::Name(_) => None,
            ToolRequirement::Detailed(d) => d.auth,
        }
    }
}

/// A duration: `500ms`, `30s`, `5m`, `1h`, or a number of seconds.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(untagged)]
pub enum DurationSpec {
    Seconds(f64),
    Text(String),
}

impl DurationSpec {
    pub fn parse(&self) -> Result<Duration, String> {
        match self {
            DurationSpec::Seconds(s) if *s >= 0.0 && s.is_finite() => Ok(Duration::from_secs_f64(*s)),
            DurationSpec::Seconds(s) => Err(format!("invalid duration {s}")),
            DurationSpec::Text(t) => {
                humantime::parse_duration(t.trim()).map_err(|e| format!("invalid duration '{t}': {e}"))
            }
        }
    }
}

/// One step. Exactly one of `run`, `http`, `assert`, `browser`, `flow` or
/// `steps` sets what the step does.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Step {
    /// Identifier, unique in the flow (`[a-z][a-z0-9_]*`). Later steps read
    /// this step as `steps.<id>`.
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Shown next to the step in the UI and with its failure.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
    /// CEL condition; the step is skipped when it is false.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub when: Option<String>,
    /// Per-attempt limit (default 60s for process and browser steps).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout: Option<DurationSpec>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry: Option<Retry>,
    /// A failure does not stop the flow; it is reported as a warning.
    #[serde(default)]
    pub continue_on_error: bool,
    /// Environment variables for this step; values may contain `${{ }}`.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub env: BTreeMap<String, String>,
    /// Values this step exposes as `steps.<id>.outputs.<name>`, each a CEL
    /// expression over `self`.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub outputs: BTreeMap<String, String>,
    /// CEL conditions over `self` that decide success, replacing the
    /// default check (exit code 0, HTTP status below 400).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expect: Option<OneOrMany>,
    /// Files this step produces, copied into the run's artifacts.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub artifacts: Vec<ArtifactSpec>,
    /// A command that undoes this step, run after the flow.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cleanup: Option<Cleanup>,

    /// Run a process.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run: Option<String>,
    /// Shell for `run`: `sh` (default), `bash`, `zsh`, `pwsh`, or `none` to
    /// run the first word as a program with the rest as arguments.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shell: Option<String>,
    /// Working directory for `run`, relative to the flow file.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub http: Option<HttpStep>,
    /// CEL conditions that must all be true.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assert: Option<OneOrMany>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub browser: Option<BrowserStep>,

    /// Call another flow by id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub flow: Option<String>,
    /// Inputs for the called flow; values may contain `${{ }}`.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub with: BTreeMap<String, String>,

    /// A group of steps.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub steps: Option<Vec<Step>>,
    /// Run the group's steps concurrently.
    #[serde(default)]
    pub parallel: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_parallel: Option<usize>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum StepKind {
    Run,
    Http,
    Assert,
    Browser,
    Flow,
    Group,
}

impl StepKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            StepKind::Run => "run",
            StepKind::Http => "http",
            StepKind::Assert => "assert",
            StepKind::Browser => "browser",
            StepKind::Flow => "flow",
            StepKind::Group => "group",
        }
    }
}

impl Step {
    /// The kinds this step declares; valid steps declare exactly one.
    pub fn declared_kinds(&self) -> Vec<StepKind> {
        let mut kinds = Vec::new();
        if self.run.is_some() {
            kinds.push(StepKind::Run);
        }
        if self.http.is_some() {
            kinds.push(StepKind::Http);
        }
        if self.assert.is_some() {
            kinds.push(StepKind::Assert);
        }
        if self.browser.is_some() {
            kinds.push(StepKind::Browser);
        }
        if self.flow.is_some() {
            kinds.push(StepKind::Flow);
        }
        if self.steps.is_some() {
            kinds.push(StepKind::Group);
        }
        kinds
    }

    /// The step's kind. Only meaningful on a validated step.
    pub fn kind(&self) -> StepKind {
        self.declared_kinds().first().copied().unwrap_or(StepKind::Run)
    }

    pub fn display_name(&self) -> &str {
        self.name.as_deref().unwrap_or(&self.id)
    }

    pub fn children(&self) -> &[Step] {
        self.steps.as_deref().unwrap_or(&[])
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(untagged)]
pub enum OneOrMany {
    One(String),
    Many(Vec<String>),
}

impl OneOrMany {
    pub fn items(&self) -> Vec<&str> {
        match self {
            OneOrMany::One(s) => vec![s.as_str()],
            OneOrMany::Many(v) => v.iter().map(String::as_str).collect(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Retry {
    /// Total attempts, including the first (default 1).
    #[serde(default = "one")]
    pub attempts: u32,
    /// Pause between attempts (default 1s).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delay: Option<DurationSpec>,
    /// CEL condition over `self`: retry until it is true. When attempts run
    /// out first, the step fails.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub until: Option<String>,
}

fn one() -> u32 {
    1
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ArtifactSpec {
    /// File path or glob, relative to the flow file; may contain `${{ }}`.
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum CleanupPolicy {
    /// Run after the flow whatever the result.
    #[default]
    Always,
    /// Run only when the flow did not pass, keeping state for inspection
    /// after a pass.
    OnFailure,
    /// Record only; run later with `lest cleanup <run>`.
    Manual,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Cleanup {
    /// Command to run (in `sh`). Values reach it through `env`.
    pub run: String,
    /// Values may reference `self`, the finished step.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub env: BTreeMap<String, String>,
    #[serde(default)]
    pub policy: CleanupPolicy,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout: Option<DurationSpec>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct HttpStep {
    /// GET (default), POST, PUT, PATCH, DELETE, HEAD.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub method: Option<String>,
    /// May contain `${{ }}`.
    pub url: String,
    /// Values may contain `${{ }}`.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub headers: BTreeMap<String, String>,
    /// JSON body; string values anywhere in it may contain `${{ }}`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub json: Option<serde_json::Value>,
    /// Raw body; may contain `${{ }}`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BrowserStep {
    /// Page to open first; may contain `${{ }}`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// Declarative actions, run in order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub actions: Vec<BrowserAction>,
    /// A JavaScript module, relative to the flow file, whose default export
    /// receives the page and helpers. Runs after `actions`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub script: Option<String>,
    /// Record a video of the step (with a visible cursor and paced typing).
    #[serde(default)]
    pub record: bool,
    /// Open with the browser session saved under this name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<String>,
    /// Save the session (cookies, storage) under this name when the step
    /// passes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub save_session: Option<String>,
    /// `[width, height]`, default `[1280, 800]` (`[1920, 1080]` when recording).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub viewport: Option<[u32; 2]>,
    /// Extra Chromium arguments.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub args: Vec<String>,
}

/// One declarative browser action. Selectors use Playwright selector syntax
/// (`text=Sign in`, `role=button[name="Save"]`, CSS).
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BrowserAction {
    /// Navigate; relative URLs resolve against the step's `url`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub goto: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub click: Option<String>,
    /// `{selector, value}`; replaces the field's content.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fill: Option<FillAction>,
    /// Press a key (`Enter`, `Tab`, `Control+A`) on the focused element.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub press: Option<String>,
    /// Wait until the selector is visible.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wait_for: Option<String>,
    /// Wait until the selector contains the text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expect_text: Option<ExpectTextAction>,
    /// Wait until the URL matches this regex.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expect_url: Option<String>,
    /// Save a screenshot artifact with this name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub screenshot: Option<String>,
    /// Mark a story beat (used by demos).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub beat: Option<String>,
    /// Pause, e.g. `1s` (on camera, a held shot).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wait: Option<DurationSpec>,
    /// Hover over the selector.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hover: Option<String>,
    /// Select an option `{selector, value}`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub select: Option<FillAction>,
    /// Record an output `{name, selector}` from the element's text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub read: Option<ReadAction>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FillAction {
    pub selector: String,
    /// May contain `${{ }}`.
    pub value: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ExpectTextAction {
    pub selector: String,
    /// May contain `${{ }}`.
    pub text: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReadAction {
    pub name: String,
    pub selector: String,
}

impl BrowserAction {
    /// The names of the fields this action sets; valid actions set one.
    pub fn declared(&self) -> Vec<&'static str> {
        let mut v = Vec::new();
        macro_rules! check {
            ($($f:ident => $n:literal),*) => { $( if self.$f.is_some() { v.push($n); } )* };
        }
        check!(goto => "goto", click => "click", fill => "fill", press => "press",
            wait_for => "waitFor", expect_text => "expectText", expect_url => "expectUrl",
            screenshot => "screenshot", beat => "beat", wait => "wait", hover => "hover",
            select => "select", read => "read");
        v
    }
}

/// How a flow presents itself as a demo.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Demo {
    /// Card title (defaults to the flow name).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// One or two sentences on what the viewer sees.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    /// Labels for the beats the recording marks, keyed by marker, in story
    /// order. Markers without a label are cut points only.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub beats: Vec<DemoBeat>,
    /// How the recording is cut.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cut: Option<DemoCut>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DemoBeat {
    pub marker: String,
    pub label: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DemoCut {
    /// A gap between beats longer than this is shortened (default 8s).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_gap: Option<DurationSpec>,
    /// How much of a shortened gap is kept (default 1.5s).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub keep: Option<DurationSpec>,
}

/// Generated JSON Schema for flow files.
pub fn json_schema() -> serde_json::Value {
    let schema = schemars::schema_for!(Flow);
    serde_json::to_value(schema).expect("schema serializes")
}
