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

/// A started service, stopped when dropped or by [`Running::stop`].
pub struct Running {
    pub id: String,
    child: Option<Child>,
    pub log_path: PathBuf,
    /// True when an already-running instance was reused.
    pub reused: bool,
}

impl Running {
    pub async fn stop(&mut self) {
        if let Some(child) = self.child.as_mut() {
            #[cfg(unix)]
            if let Some(pid) = child.id() {
                // SAFETY: signals the process group the service leads; it was
                // spawned with process_group(0). TERM first so servers can
                // flush, then KILL.
                unsafe {
                    libc::kill(-(pid as i32), libc::SIGTERM);
                }
                if tokio::time::timeout(Duration::from_secs(3), child.wait()).await.is_err() {
                    unsafe {
                        libc::kill(-(pid as i32), libc::SIGKILL);
                    }
                }
            }
            let _ = child.start_kill();
            let _ = child.wait().await;
        }
        self.child = None;
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        if let Some(child) = self.child.as_mut() {
            #[cfg(unix)]
            if let Some(pid) = child.id() {
                // SAFETY: as in stop().
                unsafe {
                    libc::kill(-(pid as i32), libc::SIGKILL);
                }
            }
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

/// Starts a service and waits until it is ready.
pub async fn start(
    svc: &Service,
    ready_url: Option<String>,
    env: Vec<(String, String)>,
    env_remove: &[String],
    flow_dir: &Path,
    log_dir: &Path,
    cancel: &CancellationToken,
) -> Result<Running, String> {
    let log_path = log_dir.join(format!("service-{}.log", svc.id));
    if svc.reuse
        && let Some(url) = &ready_url
        && http_ready(url).await
    {
        return Ok(Running { id: svc.id.clone(), child: None, log_path, reused: true });
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
        readers.push(tokio::spawn(async move {
            let mut lines = BufReader::new(reader).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                let mut f = file.lock().await;
                let _ = f.write_all(format!("{line}\n").as_bytes()).await;
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
            return Ok(Running { id: svc.id.clone(), child: Some(child), log_path, reused: false });
        }
        if started.elapsed() > timeout || cancel.is_cancelled() {
            let mut r = Running { id: svc.id.clone(), child: Some(child), log_path: log_path.clone(), reused: false };
            r.stop().await;
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
