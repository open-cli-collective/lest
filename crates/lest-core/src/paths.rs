//! Where Lest keeps configuration and run data.
//!
//! Config: the user config dir (`lest/config.yml`). Data (run reports,
//! artifacts, sessions, usage): Linux `$XDG_STATE_HOME/lest`, macOS
//! `~/Library/Application Support/lest/data`, Windows
//! `%LOCALAPPDATA%\lest\data`. Tests inject a [`StateRoots`] instead of
//! touching the developer's directories.

use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct StateRoots {
    pub config_dir: PathBuf,
    pub data_dir: PathBuf,
    /// Resource and service locks. They guard things every Lest process on
    /// the machine shares (ports, accounts), so the standard location does
    /// not follow `LEST_DATA_DIR`.
    pub locks_dir: PathBuf,
}

impl StateRoots {
    /// The standard locations, honoring `LEST_CONFIG_DIR` and
    /// `LEST_DATA_DIR` overrides.
    pub fn standard() -> anyhow::Result<StateRoots> {
        let config_dir = match std::env::var_os("LEST_CONFIG_DIR") {
            Some(p) => absolute("LEST_CONFIG_DIR", PathBuf::from(p))?,
            None => {
                check_xdg("XDG_CONFIG_HOME")?;
                dirs::config_dir().ok_or_else(|| anyhow::anyhow!("no user config directory"))?.join("lest")
            }
        };
        let data_dir = match std::env::var_os("LEST_DATA_DIR") {
            Some(p) => absolute("LEST_DATA_DIR", PathBuf::from(p))?,
            None => default_data_dir()?,
        };
        let locks_dir = match std::env::var_os("LEST_LOCK_DIR") {
            Some(p) => absolute("LEST_LOCK_DIR", PathBuf::from(p))?,
            None => {
                dirs::cache_dir().ok_or_else(|| anyhow::anyhow!("no user cache directory"))?.join("lest").join("locks")
            }
        };
        Ok(StateRoots { config_dir, data_dir, locks_dir })
    }

    /// Roots under one directory (tests, `--data-dir`).
    pub fn under(dir: &Path) -> StateRoots {
        StateRoots { config_dir: dir.join("config"), data_dir: dir.join("data"), locks_dir: dir.join("locks") }
    }

    pub fn runs_dir(&self) -> PathBuf {
        self.data_dir.join("runs")
    }
    pub fn sessions_dir(&self) -> PathBuf {
        self.data_dir.join("sessions")
    }
    pub fn locks_dir(&self) -> PathBuf {
        self.locks_dir.clone()
    }
    pub fn ai_dir(&self) -> PathBuf {
        self.data_dir.join("ai")
    }
    pub fn config_file(&self) -> PathBuf {
        self.config_dir.join("config.yml")
    }
}

fn absolute(var: &str, p: PathBuf) -> anyhow::Result<PathBuf> {
    if p.is_absolute() { Ok(p) } else { anyhow::bail!("{var} must be an absolute path, got {}", p.display()) }
}

/// A relative XDG value is an error rather than silently ignored.
fn check_xdg(var: &str) -> anyhow::Result<()> {
    if let Some(v) = std::env::var_os(var)
        && !v.is_empty()
        && !Path::new(&v).is_absolute()
    {
        anyhow::bail!("{var} must be an absolute path, got {}", Path::new(&v).display());
    }
    Ok(())
}

fn default_data_dir() -> anyhow::Result<PathBuf> {
    if cfg!(target_os = "macos") {
        Ok(dirs::config_dir().ok_or_else(|| anyhow::anyhow!("no user config directory"))?.join("lest").join("data"))
    } else if cfg!(windows) {
        Ok(dirs::data_local_dir().ok_or_else(|| anyhow::anyhow!("no local data directory"))?.join("lest").join("data"))
    } else {
        check_xdg("XDG_STATE_HOME")?;
        Ok(dirs::state_dir()
            .or_else(|| dirs::home_dir().map(|h| h.join(".local/state")))
            .ok_or_else(|| anyhow::anyhow!("no state directory"))?
            .join("lest"))
    }
}

/// Creates a directory (and parents) readable only by the user.
pub fn ensure_private_dir(dir: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

/// Writes a file atomically (temp file + rename), readable only by the user.
pub fn write_private(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        ensure_private_dir(parent)?;
    }
    let tmp = path.with_extension(format!("tmp-{}", std::process::id()));
    std::fs::write(&tmp, bytes)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600))?;
    }
    std::fs::rename(&tmp, path)
}
