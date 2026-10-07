//! Child process supervision.
//!
//! - stdout and stderr are drained concurrently while the child runs; reading
//!   only after exit deadlocks any child that fills a pipe buffer.
//! - Each stream keeps its first `cap` bytes plus a truncation marker.
//! - The child gets its own process group, and timeout or cancel kills the
//!   whole group, so grandchildren (a CLI's own subprocesses) cannot keep the
//!   pipes open.

use std::path::PathBuf;
use std::process::Stdio;
use std::time::{Duration, Instant};

use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWriteExt, BufReader};
use tokio::process::Command;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::event::Stream;

pub const DEFAULT_CAP: usize = 8 * 1024 * 1024;

#[derive(Debug, Clone)]
pub struct ProcessSpec {
    pub program: String,
    pub args: Vec<String>,
    pub cwd: PathBuf,
    /// Added to the inherited environment.
    pub env: Vec<(String, String)>,
    /// Removed from the inherited environment.
    pub env_remove: Vec<String>,
    pub stdin: Option<Vec<u8>>,
    pub timeout: Duration,
    pub cap: usize,
}

#[derive(Debug, Clone, Default)]
pub struct ProcessResult {
    pub exit_code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
    pub timed_out: bool,
    pub cancelled: bool,
    pub spawn_error: Option<String>,
    pub duration: Duration,
}

impl ProcessResult {
    pub fn success(&self) -> bool {
        self.exit_code == Some(0) && !self.timed_out && !self.cancelled && self.spawn_error.is_none()
    }
}

/// Runs a process to completion, calling `on_line` for each output line.
pub async fn run(
    spec: ProcessSpec,
    cancel: CancellationToken,
    on_line: impl FnMut(Stream, String) + Send,
) -> ProcessResult {
    let started = Instant::now();
    let mut cmd = Command::new(&spec.program);
    cmd.args(&spec.args)
        .current_dir(&spec.cwd)
        .stdin(if spec.stdin.is_some() { Stdio::piped() } else { Stdio::null() })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    for k in &spec.env_remove {
        cmd.env_remove(k);
    }
    for (k, v) in &spec.env {
        cmd.env(k, v);
    }
    #[cfg(unix)]
    cmd.process_group(0);

    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            let msg = if e.kind() == std::io::ErrorKind::NotFound {
                format!("cannot run '{}': not found on PATH", spec.program)
            } else {
                format!("cannot run '{}': {e}", spec.program)
            };
            return ProcessResult { spawn_error: Some(msg), duration: started.elapsed(), ..Default::default() };
        }
    };
    let pid = child.id();

    if let (Some(input), Some(mut stdin)) = (spec.stdin.clone(), child.stdin.take()) {
        tokio::spawn(async move {
            let _ = stdin.write_all(&input).await;
            let _ = stdin.shutdown().await;
        });
    }

    let (tx, mut rx) = mpsc::unbounded_channel::<(Stream, String)>();
    let out_task = tokio::spawn(drain(child.stdout.take().expect("stdout"), Stream::Stdout, spec.cap, tx.clone()));
    let err_task = tokio::spawn(drain(child.stderr.take().expect("stderr"), Stream::Stderr, spec.cap, tx));

    let mut on_line = on_line;
    let mut timed_out = false;
    let mut cancelled = false;
    let deadline = tokio::time::sleep(spec.timeout);
    tokio::pin!(deadline);
    let status = loop {
        tokio::select! {
            biased;
            Some((stream, line)) = rx.recv() => on_line(stream, line),
            status = child.wait() => break status.ok(),
            _ = &mut deadline => {
                timed_out = true;
                kill_group(pid, &mut child).await;
                break child.wait().await.ok();
            }
            _ = cancel.cancelled() => {
                cancelled = true;
                kill_group(pid, &mut child).await;
                break child.wait().await.ok();
            }
        }
    };
    // Readers finish once every holder of the pipes has exited; bound the
    // wait in case something outside the group still holds one.
    let grace = Duration::from_secs(2);
    let stdout = tokio::time::timeout(grace, out_task).await.ok().and_then(Result::ok).unwrap_or_default();
    let stderr = tokio::time::timeout(grace, err_task).await.ok().and_then(Result::ok).unwrap_or_default();
    while let Ok((stream, line)) = rx.try_recv() {
        on_line(stream, line);
    }
    ProcessResult {
        exit_code: status.and_then(|s| s.code()),
        stdout,
        stderr,
        timed_out,
        cancelled,
        spawn_error: None,
        duration: started.elapsed(),
    }
}

async fn drain(
    reader: impl AsyncRead + Unpin,
    stream: Stream,
    cap: usize,
    tx: mpsc::UnboundedSender<(Stream, String)>,
) -> String {
    let mut reader = BufReader::new(reader);
    let mut kept: Vec<u8> = Vec::new();
    let mut truncated = false;
    let mut buf = Vec::new();
    loop {
        buf.clear();
        match reader.read_until(b'\n', &mut buf).await {
            Ok(0) | Err(_) => break,
            Ok(_) => {
                if kept.len() < cap {
                    let room = cap - kept.len();
                    kept.extend_from_slice(&buf[..buf.len().min(room)]);
                    if buf.len() > room {
                        truncated = true;
                    }
                } else {
                    truncated = true;
                }
                let line = String::from_utf8_lossy(&buf);
                let _ = tx.send((stream, line.trim_end_matches(['\n', '\r']).to_string()));
            }
        }
    }
    let mut text = String::from_utf8_lossy(&kept).into_owned();
    if truncated {
        text.push_str(&format!("\n[output truncated at {} bytes]\n", cap));
    }
    text
}

async fn kill_group(pid: Option<u32>, child: &mut tokio::process::Child) {
    #[cfg(unix)]
    if let Some(pid) = pid {
        // SAFETY: kill(2) with a negative pid signals the process group the
        // child leads (it was spawned with process_group(0)).
        unsafe {
            libc::kill(-(pid as i32), libc::SIGKILL);
        }
    }
    #[cfg(not(unix))]
    let _ = pid;
    let _ = child.start_kill();
}

/// The program and arguments for a script in the given shell.
pub fn shell_command(shell: Option<&str>, script: &str) -> (String, Vec<String>) {
    match shell.unwrap_or("sh") {
        "none" => {
            let mut parts = script.split_whitespace().map(str::to_string);
            let program = parts.next().unwrap_or_default();
            (program, parts.collect())
        }
        "pwsh" => ("pwsh".into(), vec!["-NoProfile".into(), "-Command".into(), script.into()]),
        // -e: a failing command in a multi-line script fails the step.
        "bash" => ("bash".into(), vec!["-e".into(), "-o".into(), "pipefail".into(), "-c".into(), script.into()]),
        "zsh" => ("zsh".into(), vec!["-e".into(), "-c".into(), script.into()]),
        _ => ("sh".into(), vec!["-e".into(), "-c".into(), script.into()]),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(script: &str, timeout: Duration) -> ProcessSpec {
        let (program, args) = shell_command(None, script);
        ProcessSpec {
            program,
            args,
            cwd: std::env::temp_dir(),
            env: vec![("GREETING".into(), "hi".into())],
            env_remove: vec![],
            stdin: None,
            timeout,
            cap: DEFAULT_CAP,
        }
    }

    #[tokio::test]
    async fn captures_streams_and_lines() {
        let mut lines = Vec::new();
        let r = run(spec("echo $GREETING; echo err >&2", Duration::from_secs(5)), CancellationToken::new(), |s, l| {
            lines.push((s, l))
        })
        .await;
        assert!(r.success());
        assert_eq!(r.stdout, "hi\n");
        assert_eq!(r.stderr, "err\n");
        assert!(lines.contains(&(Stream::Stdout, "hi".into())));
    }

    #[tokio::test]
    async fn drains_large_output_without_deadlock() {
        let r = run(
            spec(
                "head -c 300000 /dev/zero | tr '\\0' a; echo; head -c 300000 /dev/zero | tr '\\0' b >&2",
                Duration::from_secs(10),
            ),
            CancellationToken::new(),
            |_, _| {},
        )
        .await;
        assert!(r.success(), "{r:?}");
        assert!(r.stdout.len() > 300_000);
    }

    #[tokio::test]
    async fn timeout_kills_the_process_group() {
        let started = Instant::now();
        let r = run(spec("sleep 30 & sleep 30; wait", Duration::from_millis(300)), CancellationToken::new(), |_, _| {})
            .await;
        assert!(r.timed_out);
        assert!(started.elapsed() < Duration::from_secs(5), "took {:?}", started.elapsed());
    }

    #[tokio::test]
    async fn cancel_stops_the_process() {
        let token = CancellationToken::new();
        let t2 = token.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(200)).await;
            t2.cancel();
        });
        let r = run(spec("sleep 30", Duration::from_secs(60)), token, |_, _| {}).await;
        assert!(r.cancelled);
    }

    #[tokio::test]
    async fn missing_program_is_a_spawn_error() {
        let mut s = spec("x", Duration::from_secs(1));
        s.program = "definitely-not-a-real-program-lest".into();
        s.args.clear();
        let r = run(s, CancellationToken::new(), |_, _| {}).await;
        assert!(r.spawn_error.unwrap().contains("not found"));
    }

    #[tokio::test]
    async fn sh_fails_on_first_failing_command() {
        let r = run(spec("false\necho after", Duration::from_secs(5)), CancellationToken::new(), |_, _| {}).await;
        assert_eq!(r.exit_code, Some(1));
        assert!(!r.stdout.contains("after"));
    }
}
