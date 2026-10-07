//! Opening a terminal for interactive commands (tool sign-ins, an agent
//! hand-off) and revealing files in the system file manager.

use std::path::Path;
use std::process::{Command, Stdio};

fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// Runs `command` in `cwd` in a new terminal window. Returns how, or an
/// error when no terminal could be found (the caller shows the command to
/// copy instead).
pub fn open(command: &str, cwd: &Path) -> Result<String, String> {
    let line = format!("cd {} && {command}", shell_quote(&cwd.display().to_string()));
    if cfg!(target_os = "macos") {
        let script = format!(
            "tell application \"Terminal\"\nactivate\ndo script \"{}\"\nend tell",
            line.replace('\\', "\\\\").replace('"', "\\\"")
        );
        let ok = Command::new("osascript").arg("-e").arg(script).stdout(Stdio::null()).status();
        return match ok {
            Ok(s) if s.success() => Ok("Terminal".into()),
            _ => Err("could not open Terminal".into()),
        };
    }
    let keep_open = format!("{line}; echo; echo '[press enter to close]'; read _");
    for (bin, args) in [
        ("x-terminal-emulator", vec!["-e", "sh", "-c"]),
        ("gnome-terminal", vec!["--", "sh", "-c"]),
        ("konsole", vec!["-e", "sh", "-c"]),
        ("xterm", vec!["-e", "sh", "-c"]),
    ] {
        if which_ok(bin) {
            let spawned = Command::new(bin).args(&args).arg(&keep_open).stdout(Stdio::null()).stderr(Stdio::null()).spawn();
            if spawned.is_ok() {
                return Ok(bin.to_string());
            }
        }
    }
    Err("no terminal emulator found".into())
}

fn which_ok(bin: &str) -> bool {
    std::env::var_os("PATH").is_some_and(|p| std::env::split_paths(&p).any(|d| d.join(bin).is_file()))
}

/// Shows a file or folder in the system file manager.
pub fn reveal(path: &Path) -> Result<(), String> {
    let status = if cfg!(target_os = "macos") {
        if path.is_dir() { Command::new("open").arg(path).status() } else { Command::new("open").arg("-R").arg(path).status() }
    } else if cfg!(windows) {
        Command::new("explorer").arg(format!("/select,{}", path.display())).status()
    } else {
        let dir = if path.is_dir() { path } else { path.parent().unwrap_or(path) };
        Command::new("xdg-open").arg(dir).status()
    };
    match status {
        Ok(s) if s.success() => Ok(()),
        Ok(s) => Err(format!("the file manager exited {}", s.code().unwrap_or(-1))),
        Err(e) => Err(e.to_string()),
    }
}
