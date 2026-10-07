//! A project: a directory with a `lest.yaml` and the flows beneath it.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

pub const PROJECT_FILE: &str = "lest.yaml";

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProjectConfig {
    /// Display name (defaults to the directory name).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Directories searched for `*.lest.yaml`, relative to the project
    /// root (default: the whole project).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub flows: Vec<String>,
    /// Profiles for command-line tools flows declare in `tools:`.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub tools: BTreeMap<String, ToolProfile>,
    /// Where `${{ secrets.<name> }}` values come from, tried in order.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub secrets: Option<SecretsConfig>,
    /// Where run results are sent.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub notify: Vec<NotifyTarget>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub browser: Option<BrowserConfig>,
}

/// How Lest checks, signs in to and targets a command-line tool.
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ToolProfile {
    /// Prints the version (default `<name> --version`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    /// Exits 0 when the tool is signed in and usable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub check: Option<String>,
    /// Non-interactive recovery (a token refresh), tried once before login.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub heal: Option<String>,
    /// Interactive sign-in. Run only from a terminal or the UI's Sign in
    /// button, never in CI.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub login: Option<String>,
    /// Upgrades the tool.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub update: Option<String>,
    /// Shown when the tool is missing or not signed in.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
    /// Environment variables that pin the tool to the run's target, set on
    /// every step (values may use `${{ run.environment }}` and `vars`).
    /// Prefer this over commands that rewrite the tool's shared config,
    /// which would retarget every other process using it.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub env: BTreeMap<String, String>,
    /// Tools that must be ready first (a password manager, for example).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub requires: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SecretsConfig {
    pub backends: Vec<SecretBackend>,
}

/// One backend, written as a single-key map: `keyring: {}`,
/// `env: {prefix: ...}` or `command: {run: ...}`.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(untagged, deny_unknown_fields)]
pub enum SecretBackend {
    /// The OS keyring (`lest secrets create <name>`).
    Keyring { keyring: KeyringBackend },
    /// Environment variables `<prefix><NAME>` (default prefix `LEST_SECRET_`).
    Env { env: EnvBackend },
    /// A command that prints the secret; it receives the name in
    /// `LEST_SECRET_NAME`. Use it for any password manager CLI.
    Command { command: CommandBackend },
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct KeyringBackend {}

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EnvBackend {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prefix: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CommandBackend {
    pub run: String,
}

impl Default for SecretsConfig {
    fn default() -> Self {
        SecretsConfig {
            backends: vec![
                SecretBackend::Keyring { keyring: KeyringBackend::default() },
                SecretBackend::Env { env: EnvBackend::default() },
            ],
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NotifyTarget {
    /// URL to POST to; may contain `${{ secrets.<name> }}`.
    pub webhook: String,
    /// `failed` (default), `passed`, or `always`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on: Option<NotifyWhen>,
    /// `json` (default) or `slack` (an incoming-webhook message).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub format: Option<NotifyFormat>,
    /// Secret names the URL references.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub secrets: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum NotifyWhen {
    Failed,
    Passed,
    Always,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum NotifyFormat {
    Json,
    Slack,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BrowserConfig {
    /// Node binary (default `node` on PATH).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node: Option<String>,
    /// Extra Chromium arguments for every browser step.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub args: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct Project {
    pub root: PathBuf,
    pub config: ProjectConfig,
    /// Whether a `lest.yaml` exists (false: defaults for a bare directory).
    pub has_config: bool,
}

impl Project {
    /// Finds the project containing `start`: the nearest ancestor with a
    /// `lest.yaml`, or `start` itself with default settings.
    pub fn discover(start: &Path) -> anyhow::Result<Project> {
        let start = if start.is_file() { start.parent().unwrap_or(start).to_path_buf() } else { start.to_path_buf() };
        let start = std::fs::canonicalize(&start).unwrap_or(start);
        let mut dir = Some(start.as_path());
        while let Some(d) = dir {
            if d.join(PROJECT_FILE).is_file() {
                return Project::open(d);
            }
            dir = d.parent();
        }
        Ok(Project { root: start, config: ProjectConfig::default(), has_config: false })
    }

    /// Opens the project rooted at `root`.
    pub fn open(root: &Path) -> anyhow::Result<Project> {
        let path = root.join(PROJECT_FILE);
        let text =
            std::fs::read_to_string(&path).map_err(|e| anyhow::anyhow!("cannot read {}: {e}", path.display()))?;
        let config: ProjectConfig = if text.trim().is_empty() {
            ProjectConfig::default()
        } else {
            serde_yaml_ng::from_str(&text).map_err(|e| anyhow::anyhow!("{}: {e}", path.display()))?
        };
        Ok(Project { root: root.to_path_buf(), config, has_config: true })
    }

    pub fn name(&self) -> String {
        self.config.name.clone().unwrap_or_else(|| {
            self.root.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "project".to_string())
        })
    }

    pub fn secrets_config(&self) -> SecretsConfig {
        self.config.secrets.clone().unwrap_or_default()
    }

    /// The directories searched for flows.
    pub fn flow_roots(&self) -> Vec<PathBuf> {
        if self.config.flows.is_empty() {
            vec![self.root.clone()]
        } else {
            self.config.flows.iter().map(|d| self.root.join(d)).collect()
        }
    }
}

pub fn project_schema() -> serde_json::Value {
    serde_json::to_value(schemars::schema_for!(ProjectConfig)).expect("schema serializes")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secret_backends_parse_as_single_key_maps() {
        let cfg: ProjectConfig = serde_yaml_ng::from_str(
            "secrets:\n  backends:\n    - keyring: {}\n    - env: { prefix: CI_ }\n    - command: { run: 'vault read $LEST_SECRET_NAME' }\n",
        )
        .unwrap();
        let b = cfg.secrets.unwrap().backends;
        assert!(matches!(b[0], SecretBackend::Keyring { .. }));
        assert!(matches!(&b[1], SecretBackend::Env { env } if env.prefix.as_deref() == Some("CI_")));
        assert!(matches!(b[2], SecretBackend::Command { .. }));
        assert!(serde_yaml_ng::from_str::<ProjectConfig>("secrets:\n  backends:\n    - vault: {}\n").is_err());
    }
}
