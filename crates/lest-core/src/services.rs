//! Services: long-running processes a run starts before its steps (a local
//! dev server, a mock API) and stops after cleanup.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant};

use regex::Regex;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;

use crate::spec::Service;

/// Process groups of every running service, so a hard exit can still stop
/// them ([`kill_all`]).
static GROUPS: std::sync::Mutex<Vec<i32>> = std::sync::Mutex::new(Vec::new());

/// Kills every service still running (for an exit that skips cleanup).
pub fn kill_all() {
    #[cfg(unix)]
    for pgid in GROUPS.lock().map(|g| g.clone()).unwrap_or_default() {
        // SAFETY: each entry is the process group of a service we started.
        unsafe {
            libc::kill(-pgid, libc::SIGKILL);
        }
    }
}

#[cfg(unix)]
fn group_alive(pgid: i32) -> bool {
    // SAFETY: signal 0 only checks whether the group still exists.
    unsafe { libc::kill(-pgid, 0) == 0 }
}

/// A started service, stopped when dropped or by [`Running::stop`].
pub struct Running {
    pub id: String,
    child: Option<Child>,
    /// The service's process group (its leader's pid), recorded at start:
    /// the leader may exit while the group lives on.
    pgid: Option<i32>,
    pub log_path: PathBuf,
    /// True when an already-running instance was reused.
    pub reused: bool,
}

impl Running {
    /// TERM to the whole process group, then KILL to whatever is left
    /// after 3 seconds, even when the group leader exited on TERM.
    pub async fn stop(&mut self) {
        #[cfg(unix)]
        if let Some(pgid) = self.pgid.take() {
            // SAFETY: signals the group the service leads (spawned with
            // process_group(0)).
            unsafe {
                libc::kill(-pgid, libc::SIGTERM);
            }
            let deadline = Instant::now() + Duration::from_secs(3);
            while group_alive(pgid) && Instant::now() < deadline {
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
            unsafe {
                libc::kill(-pgid, libc::SIGKILL);
            }
            if let Ok(mut g) = GROUPS.lock() {
                g.retain(|p| *p != pgid);
            }
        }
        if let Some(child) = self.child.as_mut() {
            let _ = child.start_kill();
            let _ = child.wait().await;
        }
        self.child = None;
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        #[cfg(unix)]
        if let Some(pgid) = self.pgid.take() {
            // SAFETY: as in stop().
            unsafe {
                libc::kill(-pgid, libc::SIGKILL);
            }
            if let Ok(mut g) = GROUPS.lock() {
                g.retain(|p| *p != pgid);
            }
        }
        if let Some(child) = self.child.as_mut() {
            let _ = child.start_kill();
        }
    }
}

async fn http_ready(url: &str) -> bool {
    let Ok(client) = reqwest::Client::builder().timeout(Duration::from_secs(2)).build() else {
        return false;
    };
    client.get(url).send().await.is_ok_and(|r| r.status().as_u16() < 400)
}

/// Where and how a service runs.
pub struct Launch<'a> {
    /// Variables added to its environment.
    pub vars: Vec<(String, String)>,
    /// Variables removed from its environment (env-backend secrets).
    pub remove: &'a [String],
    /// Applied to every line written to its log.
    pub redactor: &'a crate::secrets::Redactor,
    /// The declaring flow's directory (`run` and `cwd` are relative to it).
    pub flow_dir: &'a Path,
    pub log_dir: &'a Path,
}

/// Starts a service and waits until it is ready.
pub async fn start(
    svc: &Service,
    ready_url: Option<String>,
    launch: Launch<'_>,
    cancel: &CancellationToken,
) -> Result<Running, String> {
    let Launch { vars: env, remove: env_remove, redactor, flow_dir, log_dir } = launch;
    let log_path = log_dir.join(format!("service-{}.log", svc.id));
    if svc.reuse
        && let Some(url) = &ready_url
        && http_ready(url).await
    {
        return Ok(Running { id: svc.id.clone(), child: None, pgid: None, log_path, reused: true });
    }
    std::fs::create_dir_all(log_dir).map_err(|e| e.to_string())?;
    let cwd = svc.cwd.as_ref().map(|c| flow_dir.join(c)).unwrap_or_else(|| flow_dir.to_path_buf());
    let mut cmd = Command::new("sh");
    cmd.arg("-c")
        .arg(&svc.run)
        .current_dir(&cwd)
        .envs(env)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    for k in env_remove {
        cmd.env_remove(k);
    }
    #[cfg(unix)]
    cmd.process_group(0);
    let mut child = cmd.spawn().map_err(|e| format!("service {}: cannot start: {e}", svc.id))?;
    let pgid = child.id().map(|p| p as i32);
    if let (Some(p), Ok(mut g)) = (pgid, GROUPS.lock()) {
        g.push(p);
    }

    // Copy output to the log and watch for the ready line.
    let log_re = match &svc.ready.log {
        Some(p) => Some(Regex::new(p).map_err(|e| format!("service {}: ready.log: {e}", svc.id))?),
        None => None,
    };
    let (seen_tx, mut seen_rx) = watch::channel(false);
    let file = tokio::fs::File::create(&log_path).await.map_err(|e| e.to_string())?;
    let file = std::sync::Arc::new(tokio::sync::Mutex::new(file));
    let mut readers = Vec::new();
    for reader in [
        child.stdout.take().map(|s| Box::new(s) as Box<dyn tokio::io::AsyncRead + Unpin + Send>),
        child.stderr.take().map(|s| Box::new(s) as _),
    ]
    .into_iter()
    .flatten()
    {
        let file = file.clone();
        let re = log_re.clone();
        let tx = seen_tx.clone();
        let redactor = redactor.clone();
        readers.push(tokio::spawn(async move {
            let mut lines = BufReader::new(reader).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                let mut f = file.lock().await;
                // The log is kept as an artifact: never with secret values.
                let _ = f.write_all(format!("{}\n", redactor.redact(&line)).as_bytes()).await;
                let _ = f.flush().await;
                if re.as_ref().is_some_and(|r| r.is_match(&line)) {
                    let _ = tx.send(true);
                }
            }
        }));
    }

    let timeout = svc.ready.timeout.as_ref().and_then(|d| d.parse().ok()).unwrap_or(Duration::from_secs(30));
    let started = Instant::now();
    loop {
        if let Ok(Some(status)) = child.try_wait() {
            // Let the readers write the last lines before quoting them.
            for r in readers {
                let _ = tokio::time::timeout(Duration::from_secs(1), r).await;
            }
            drop(Running { id: svc.id.clone(), child: None, pgid, log_path: log_path.clone(), reused: false });
            let tail = tail(&log_path, 5);
            return Err(format!(
                "service {} exited ({}) before it was ready{}",
                svc.id,
                status.code().map(|c| format!("code {c}")).unwrap_or_else(|| "signal".into()),
                if tail.is_empty() { String::new() } else { format!(": {tail}") }
            ));
        }
        let mut ready = true;
        if let Some(url) = &ready_url {
            ready &= http_ready(url).await;
        }
        if log_re.is_some() {
            ready &= *seen_rx.borrow_and_update();
        }
        if ready {
            return Ok(Running { id: svc.id.clone(), child: Some(child), pgid, log_path, reused: false });
        }
        if started.elapsed() > timeout || cancel.is_cancelled() {
            let mut r =
                Running { id: svc.id.clone(), child: Some(child), pgid, log_path: log_path.clone(), reused: false };
            r.stop().await;
            if cancel.is_cancelled() {
                return Err(format!("cancelled while starting service {}", svc.id));
            }
            let tail = tail(&log_path, 5);
            return Err(format!(
                "service {} was not ready after {}{}",
                svc.id,
                humantime::format_duration(timeout),
                if tail.is_empty() { String::new() } else { format!(": {tail}") }
            ));
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

fn tail(path: &Path, n: usize) -> String {
    let text = std::fs::read_to_string(path).unwrap_or_default();
    let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
    lines[lines.len().saturating_sub(n)..].join(" / ")
}
