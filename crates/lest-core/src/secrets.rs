//! Secret resolution and redaction.
//!
//! Values are resolved once per run, held in memory, and every resolved
//! value is redacted from every captured stream and report field.

use std::collections::BTreeMap;
use std::process::Stdio;

use crate::project::{SecretBackend, SecretsConfig};

pub const KEYRING_SERVICE: &str = "lest";

/// Reads and writes secrets in the OS keyring.
pub trait Keyring: Send + Sync {
    fn get(&self, name: &str) -> Result<Option<String>, String>;
    fn set(&self, name: &str, value: &str) -> Result<(), String>;
    fn delete(&self, name: &str) -> Result<bool, String>;
}

/// The platform keyring through its own CLI, so Lest links no keyring
/// library: `security` on macOS, `secret-tool` on Linux.
pub struct SystemKeyring;

impl Keyring for SystemKeyring {
    fn get(&self, name: &str) -> Result<Option<String>, String> {
        let out = if cfg!(target_os = "macos") {
            std::process::Command::new("/usr/bin/security")
                .args(["find-generic-password", "-s", KEYRING_SERVICE, "-a", name, "-w"])
                .output()
        } else {
            std::process::Command::new("secret-tool")
                .args(["lookup", "service", KEYRING_SERVICE, "account", name])
                .output()
        };
        match out {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e.to_string()),
            Ok(o) if o.status.success() => {
                Ok(Some(String::from_utf8_lossy(&o.stdout).trim_end_matches('\n').to_string()))
            }
            Ok(_) => Ok(None),
        }
    }

    fn set(&self, name: &str, value: &str) -> Result<(), String> {
        use std::io::Write;
        // The value goes on stdin, never on the command line.
        let mut child = if cfg!(target_os = "macos") {
            // `-w` as the last option reads the password from stdin.
            std::process::Command::new("/usr/bin/security")
                .args(["add-generic-password", "-U", "-s", KEYRING_SERVICE, "-a", name, "-w"])
                .stdin(Stdio::piped())
                .stdout(Stdio::null())
                .stderr(Stdio::piped())
                .spawn()
        } else {
            std::process::Command::new("secret-tool")
                .args(["store", "--label", &format!("lest: {name}"), "service", KEYRING_SERVICE, "account", name])
                .stdin(Stdio::piped())
                .stdout(Stdio::null())
                .stderr(Stdio::piped())
                .spawn()
        }
        .map_err(|e| format!("cannot start the keyring tool: {e}"))?;
        {
            let stdin = child.stdin.as_mut().ok_or("no stdin")?;
            stdin.write_all(value.as_bytes()).map_err(|e| e.to_string())?;
            if cfg!(target_os = "macos") {
                // security prompts twice (password, retype).
                stdin.write_all(b"\n").map_err(|e| e.to_string())?;
                stdin.write_all(value.as_bytes()).map_err(|e| e.to_string())?;
                stdin.write_all(b"\n").map_err(|e| e.to_string())?;
            }
        }
        let out = child.wait_with_output().map_err(|e| e.to_string())?;
        if out.status.success() { Ok(()) } else { Err(String::from_utf8_lossy(&out.stderr).trim().to_string()) }
    }

    fn delete(&self, name: &str) -> Result<bool, String> {
        let out = if cfg!(target_os = "macos") {
            std::process::Command::new("/usr/bin/security")
                .args(["delete-generic-password", "-s", KEYRING_SERVICE, "-a", name])
                .output()
        } else {
            std::process::Command::new("secret-tool")
                .args(["clear", "service", KEYRING_SERVICE, "account", name])
                .output()
        };
        out.map(|o| o.status.success()).map_err(|e| e.to_string())
    }
}

type EnvLookup<'a> = Box<dyn Fn(&str) -> Option<String> + Send + Sync + 'a>;

/// Resolves secret names through the configured backends, in order.
pub struct Resolver<'a> {
    config: SecretsConfig,
    keyring: &'a dyn Keyring,
    env: EnvLookup<'a>,
}

impl<'a> Resolver<'a> {
    pub fn new(config: SecretsConfig, keyring: &'a dyn Keyring) -> Self {
        Resolver { config, keyring, env: Box::new(|k| std::env::var(k).ok()) }
    }

    /// Overrides environment lookup (tests).
    pub fn with_env(mut self, env: impl Fn(&str) -> Option<String> + Send + Sync + 'a) -> Self {
        self.env = Box::new(env);
        self
    }

    /// Resolves one secret. Returns where it came from.
    pub fn resolve(&self, name: &str) -> Result<(String, &'static str), String> {
        let mut tried = Vec::new();
        for backend in &self.config.backends {
            match backend {
                SecretBackend::Keyring(_) => {
                    tried.push(format!("keyring ({KEYRING_SERVICE}/{name})"));
                    if let Some(v) = self.keyring.get(name)? {
                        return Ok((v, "keyring"));
                    }
                }
                SecretBackend::Env(e) => {
                    let var = format!("{}{}", e.prefix.as_deref().unwrap_or("LEST_SECRET_"), name.to_uppercase());
                    tried.push(format!("${var}"));
                    if let Some(v) = (self.env)(&var) {
                        return Ok((v, "env"));
                    }
                }
                SecretBackend::Command(c) => {
                    tried.push(format!("command `{}`", c.run));
                    let out = std::process::Command::new("sh")
                        .arg("-c")
                        .arg(&c.run)
                        .env("LEST_SECRET_NAME", name)
                        .stdin(Stdio::null())
                        .output()
                        .map_err(|e| format!("secret command: {e}"))?;
                    if out.status.success() {
                        let v = String::from_utf8_lossy(&out.stdout).trim_end_matches(['\n', '\r']).to_string();
                        if !v.is_empty() {
                            return Ok((v, "command"));
                        }
                    } else {
                        let err = String::from_utf8_lossy(&out.stderr);
                        let first = err.lines().next().unwrap_or("").trim();
                        tried.push(format!("(command exited {}: {first})", out.status.code().unwrap_or(-1)));
                    }
                }
            }
        }
        Err(format!(
            "secret '{name}' not found; tried {}. Store it with `lest secrets create {name} --stdin`",
            tried.join(", ")
        ))
    }

    pub fn resolve_all(&self, names: &[String]) -> Result<BTreeMap<String, String>, String> {
        let mut out = BTreeMap::new();
        for n in names {
            out.insert(n.clone(), self.resolve(n)?.0);
        }
        Ok(out)
    }
}

/// Replaces every known secret value with `[redacted:<name>]`.
#[derive(Debug, Clone, Default)]
pub struct Redactor {
    /// Longest values first, so a value containing another is replaced whole.
    values: Vec<(String, String)>,
}

impl Redactor {
    pub fn new(secrets: &BTreeMap<String, String>) -> Self {
        let mut values: Vec<(String, String)> = secrets
            .iter()
            // Very short values would redact ordinary text.
            .filter(|(_, v)| v.chars().count() >= 4)
            .map(|(k, v)| (v.clone(), k.clone()))
            .collect();
        values.sort_by(|a, b| b.0.len().cmp(&a.0.len()));
        Redactor { values }
    }

    pub fn redact(&self, text: &str) -> String {
        let mut out = text.to_string();
        for (value, name) in &self.values {
            if out.contains(value.as_str()) {
                out = out.replace(value.as_str(), &format!("[redacted:{name}]"));
            }
        }
        out
    }

    pub fn redact_json(&self, v: &serde_json::Value) -> serde_json::Value {
        match v {
            serde_json::Value::String(s) => serde_json::Value::String(self.redact(s)),
            serde_json::Value::Array(a) => serde_json::Value::Array(a.iter().map(|x| self.redact_json(x)).collect()),
            serde_json::Value::Object(m) => {
                serde_json::Value::Object(m.iter().map(|(k, x)| (k.clone(), self.redact_json(x))).collect())
            }
            other => other.clone(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.values.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::project::{CommandBackend, EnvBackend};

    struct NoKeyring;
    impl Keyring for NoKeyring {
        fn get(&self, _: &str) -> Result<Option<String>, String> {
            Ok(None)
        }
        fn set(&self, _: &str, _: &str) -> Result<(), String> {
            Ok(())
        }
        fn delete(&self, _: &str) -> Result<bool, String> {
            Ok(false)
        }
    }

    #[test]
    fn resolves_in_order_and_explains_misses() {
        let cfg = SecretsConfig {
            backends: vec![
                SecretBackend::Env(EnvBackend::default()),
                SecretBackend::Command(CommandBackend {
                    run: "test \"$LEST_SECRET_NAME\" = via_cmd && echo from-command".into(),
                }),
            ],
        };
        let kr = NoKeyring;
        let r = Resolver::new(cfg, &kr).with_env(|k| (k == "LEST_SECRET_TOKEN").then(|| "from-env".to_string()));
        assert_eq!(r.resolve("token").unwrap(), ("from-env".into(), "env"));
        assert_eq!(r.resolve("via_cmd").unwrap(), ("from-command".into(), "command"));
        let err = r.resolve("absent").unwrap_err();
        assert!(err.contains("$LEST_SECRET_ABSENT"), "{err}");
    }

    #[test]
    fn redacts_longest_first_and_skips_tiny_values() {
        let mut s = BTreeMap::new();
        s.insert("a".into(), "secret".into());
        s.insert("b".into(), "secret-long".into());
        s.insert("pin".into(), "12".into());
        let r = Redactor::new(&s);
        assert_eq!(r.redact("x secret-long y secret 12"), "x [redacted:b] y [redacted:a] 12");
    }
}
