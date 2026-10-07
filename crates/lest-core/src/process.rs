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
use std::sync::{Arc, Mutex};
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
    let out_buf = Arc::new(Mutex::new(Captured::default()));
    let err_buf = Arc::new(Mutex::new(Captured::default()));
    let out_task = tokio::spawn(drain(
        child.stdout.take().expect("stdout"),
        Stream::Stdout,
        spec.cap,
        out_buf.clone(),
        tx.clone(),
    ));
    let err_task =
        tokio::spawn(drain(child.stderr.take().expect("stderr"), Stream::Stderr, spec.cap, err_buf.clone(), tx));

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
    // A step does not outlive itself: anything it left running in its
    // process group (a background `&` job) is stopped, which also closes the
    // pipes so the readers finish. Long-running processes belong in
    // `services`.
    kill_group(pid, &mut child).await;
    let grace = Duration::from_secs(2);
    let _ = tokio::time::timeout(grace, out_task).await;
    let _ = tokio::time::timeout(grace, err_task).await;
    while let Ok((stream, line)) = rx.try_recv() {
        on_line(stream, line);
    }
    let stdout = out_buf.lock().expect("lock").text(spec.cap);
    let stderr = err_buf.lock().expect("lock").text(spec.cap);
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

/// Output kept so far; shared so it survives a reader that never finishes.
#[derive(Default)]
struct Captured {
    bytes: Vec<u8>,
    truncated: bool,
}

impl Captured {
    fn push(&mut self, chunk: &[u8], cap: usize) {
        let room = cap.saturating_sub(self.bytes.len());
        self.bytes.extend_from_slice(&chunk[..chunk.len().min(room)]);
        if chunk.len() > room {
            self.truncated = true;
        }
    }
    fn text(&self, cap: usize) -> String {
        let mut text = String::from_utf8_lossy(&self.bytes).into_owned();
        if self.truncated {
            text.push_str(&format!("\n[output truncated at {cap} bytes]\n"));
        }
        text
    }
}

/// Lines longer than this are streamed in pieces, so a newline-free stream
/// cannot grow memory without bound.
const MAX_LINE: usize = 64 * 1024;

async fn drain(
    reader: impl AsyncRead + Unpin,
    stream: Stream,
    cap: usize,
    kept: Arc<Mutex<Captured>>,
    tx: mpsc::UnboundedSender<(Stream, String)>,
) {
    let mut reader = BufReader::new(reader);
    let mut line: Vec<u8> = Vec::new();
    let send = |line: &mut Vec<u8>| {
        let text = String::from_utf8_lossy(line);
        let _ = tx.send((stream, text.trim_end_matches(['\n', '\r']).to_string()));
        line.clear();
    };
    loop {
        let chunk = match reader.fill_buf().await {
            Ok([]) | Err(_) => break,
            Ok(c) => c.to_vec(),
        };
        reader.consume(chunk.len());
        kept.lock().expect("lock").push(&chunk, cap);
        for piece in chunk.split_inclusive(|b| *b == b'\n') {
            line.extend_from_slice(piece);
            if piece.ends_with(b"\n") || line.len() >= MAX_LINE {
                send(&mut line);
            }
        }
    }
    if !line.is_empty() {
        send(&mut line);
    }
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
    async fn background_jobs_are_stopped_and_output_kept() {
        let started = Instant::now();
        let r = run(spec("echo id=42; sleep 30 &", Duration::from_secs(10)), CancellationToken::new(), |_, _| {}).await;
        assert!(r.success(), "{r:?}");
        assert_eq!(r.stdout, "id=42\n");
        assert!(started.elapsed() < Duration::from_secs(5), "took {:?}", started.elapsed());
    }

    #[tokio::test]
    async fn long_lines_are_streamed_in_pieces() {
        let mut longest = 0;
        let r = run(
            spec("head -c 200000 /dev/zero | tr '\\0' x", Duration::from_secs(10)),
            CancellationToken::new(),
            |_, l| longest = longest.max(l.len()),
        )
        .await;
        assert!(r.success());
        assert_eq!(r.stdout.len(), 200_000);
        assert!(longest <= MAX_LINE);
    }

    #[tokio::test]
    async fn sh_fails_on_first_failing_command() {
        let r = run(spec("false\necho after", Duration::from_secs(5)), CancellationToken::new(), |_, _| {}).await;
        assert_eq!(r.exit_code, Some(1));
        assert!(!r.stdout.contains("after"));
    }
}
