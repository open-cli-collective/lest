//! Terminal output: live run progress (stderr) and tables (stdout).

use std::collections::BTreeMap;
use std::io::IsTerminal;

use lest_core::event::{EventBody, PlanNode, RunEvent, Stream};
use lest_core::report::{RunReport, RunResult, StepStatus, ToolStatus};

#[derive(Clone, Copy)]
pub struct Style {
    pub color: bool,
}

impl Style {
    pub fn detect(no_color: bool) -> Style {
        Style { color: !no_color && std::env::var_os("NO_COLOR").is_none() && std::io::stderr().is_terminal() }
    }
    fn paint(&self, code: &str, s: &str) -> String {
        if self.color { format!("\x1b[{code}m{s}\x1b[0m") } else { s.to_string() }
    }
    pub fn green(&self, s: &str) -> String {
        self.paint("32", s)
    }
    pub fn red(&self, s: &str) -> String {
        self.paint("31", s)
    }
    pub fn yellow(&self, s: &str) -> String {
        self.paint("33", s)
    }
    pub fn dim(&self, s: &str) -> String {
        self.paint("2", s)
    }
    pub fn bold(&self, s: &str) -> String {
        self.paint("1", s)
    }
}

pub fn duration(ms: u64) -> String {
    if ms < 1000 {
        format!("{ms}ms")
    } else if ms < 60_000 {
        format!("{:.1}s", ms as f64 / 1000.0)
    } else {
        format!("{}m{:02}s", ms / 60_000, (ms % 60_000) / 1000)
    }
}

/// Prints run events as they arrive.
pub struct Progress {
    style: Style,
    depth: BTreeMap<String, usize>,
    names: BTreeMap<String, String>,
    containers: std::collections::BTreeSet<String>,
    width: usize,
    quiet: bool,
}

impl Progress {
    pub fn new(style: Style, quiet: bool) -> Self {
        Progress {
            style,
            depth: BTreeMap::new(),
            names: BTreeMap::new(),
            containers: Default::default(),
            width: 24,
            quiet,
        }
    }

    fn index(&mut self, nodes: &[PlanNode], depth: usize) {
        for n in nodes {
            self.depth.insert(n.id.clone(), depth);
            self.names.insert(n.id.clone(), n.name.clone());
            if !n.children.is_empty() {
                self.containers.insert(n.id.clone());
            }
            self.width = self.width.max(n.name.chars().count() + depth * 2).min(48);
            self.index(&n.children, depth + 1);
        }
    }

    fn indent(&self, id: &str) -> String {
        "  ".repeat(self.depth.get(id).copied().unwrap_or(0) + 1)
    }

    pub fn event(&mut self, e: &RunEvent) {
        if self.quiet {
            return;
        }
        let s = self.style;
        match &e.body {
            EventBody::RunStarted { flow_name, environment, steps, finally, .. } => {
                self.index(steps, 0);
                self.index(finally, 0);
                let env = environment.as_deref().map(|e| format!(" ({e})")).unwrap_or_default();
                eprintln!("{} {}{env} {}", s.bold("▶"), s.bold(flow_name), s.dim(&format!("· run {}", e.run_id)));
            }
            EventBody::Tool { tool } => {
                let version = tool.version.as_deref().map(|v| format!(" {v}")).unwrap_or_default();
                match tool.status {
                    ToolStatus::Ready => eprintln!("  {} {}{}", s.dim("tool"), tool.name, s.dim(&version)),
                    ToolStatus::Unknown => eprintln!(
                        "  {} {}{} {}",
                        s.dim("tool"),
                        tool.name,
                        version,
                        s.yellow(tool.message.as_deref().unwrap_or(""))
                    ),
                    _ => eprintln!("  {} {} {}", s.red("✗"), tool.name, tool.message.as_deref().unwrap_or("")),
                }
            }
            EventBody::StepStarted { step_id, attempt: 1 } if self.containers.contains(step_id) => {
                let name = self.names.get(step_id).cloned().unwrap_or_else(|| step_id.clone());
                eprintln!("{}{} {}", self.indent(step_id), s.dim("▸"), s.bold(&name));
            }
            EventBody::StepRetrying { step_id, attempt, max_attempts, reason, next_attempt_in_ms } => {
                let name = self.names.get(step_id).cloned().unwrap_or_else(|| step_id.clone());
                eprintln!(
                    "{}{} {} {}",
                    self.indent(step_id),
                    s.yellow("↻"),
                    name,
                    s.dim(&format!(
                        "attempt {attempt}/{max_attempts}: {reason} (next in {})",
                        duration(*next_attempt_in_ms)
                    ))
                );
            }
            EventBody::StepOutput { step_id, stream, line } if *stream == Stream::Browser => {
                eprintln!("{}  {}", self.indent(step_id), s.dim(line));
            }
            EventBody::StepFinished { step } => {
                let status = step.status.unwrap_or(StepStatus::Pending);
                let (glyph, detail) = match status {
                    StepStatus::Passed => (s.green("✓"), String::new()),
                    StepStatus::Skipped => (s.dim("○"), s.dim(step.error.as_deref().unwrap_or("skipped"))),
                    StepStatus::Failed | StepStatus::Errored => {
                        (s.red("✗"), s.red(step.headline.as_deref().or(step.error.as_deref()).unwrap_or("failed")))
                    }
                    _ => (" ".into(), String::new()),
                };
                let indent = self.indent(&step.id);
                let pad = self.width.saturating_sub((indent.len() - 2) + step.name.chars().count());
                let dur = if status == StepStatus::Skipped { String::new() } else { duration(step.duration_ms) };
                eprintln!("{indent}{glyph} {}{} {:>7}  {detail}", step.name, " ".repeat(pad), s.dim(&dur));
            }
            EventBody::StepArtifact { step_id, artifact } => {
                eprintln!("{}  {} {}", self.indent(step_id), s.dim("artifact"), artifact.path);
            }
            EventBody::CleanupFinished { step_id, status } => {
                let glyph = if *status == StepStatus::Passed { s.green("✓") } else { s.red("✗") };
                eprintln!("  {glyph} {} {step_id}", s.dim("cleanup"));
            }
            EventBody::SignInNeeded { tool, command } => {
                eprintln!("  {} {tool} needs a sign-in: {command}", s.yellow("!"));
            }
            EventBody::DemoProgress { message } => eprintln!("  {}", s.dim(message)),
            EventBody::Notified { ok, message } => {
                let glyph = if *ok { s.green("✓") } else { s.yellow("!") };
                eprintln!("  {glyph} {}", s.dim(message));
            }
            _ => {}
        }
    }
}

pub fn result_word(style: Style, r: RunResult) -> String {
    match r {
        RunResult::Passed => style.green("passed"),
        RunResult::Failed => style.red("failed"),
        RunResult::Errored => style.red("errored"),
        RunResult::Cancelled => style.yellow("cancelled"),
    }
}

/// The closing summary of a run (stderr) for a human reading the terminal.
pub fn summary(style: Style, report: &RunReport, report_path: &str) {
    if let Some(e) = &report.error {
        eprintln!("  {} {e}", style.red("error:"));
    }
    for w in &report.warnings {
        eprintln!("  {} {w}", style.yellow("warning:"));
    }
    if let Some(f) = report.first_failure() {
        // --from takes a top-level step: the one containing the failure.
        fn contains(s: &lest_core::report::StepReport, id: &str) -> bool {
            s.id == id || s.children.iter().any(|c| contains(c, id))
        }
        if let Some(top) = report.steps.iter().find(|s| contains(s, &f.id)) {
            eprintln!("  {} lest run {} --from {}", style.dim("rerun from the failure:"), report.flow_id, top.id);
        }
    }
    eprintln!("  {} {report_path}", style.dim("report:"));
}

/// A pipe-delimited table with ALL_CAPS headers.
pub fn table(headers: &[&str], rows: &[Vec<String>]) -> String {
    let clean = |s: &str| {
        let s = s.replace(" | ", " ").replace(['\n', '\r'], " ");
        if s.trim().is_empty() { "-".to_string() } else { s }
    };
    let rows: Vec<Vec<String>> = rows.iter().map(|r| r.iter().map(|c| clean(c)).collect()).collect();
    let mut widths: Vec<usize> = headers.iter().map(|h| h.len()).collect();
    for r in &rows {
        for (i, c) in r.iter().enumerate() {
            widths[i] = widths[i].max(c.chars().count());
        }
    }
    let line = |cells: Vec<String>| {
        cells
            .iter()
            .enumerate()
            .map(|(i, c)| {
                if i + 1 == cells.len() {
                    c.clone()
                } else {
                    format!("{c}{}", " ".repeat(widths[i] - c.chars().count()))
                }
            })
            .collect::<Vec<_>>()
            .join(" | ")
    };
    let mut out = line(headers.iter().map(|h| h.to_string()).collect());
    out.push('\n');
    for r in rows {
        out.push_str(&line(r));
        out.push('\n');
    }
    out
}

/// A JUnit XML rendering of a report, for CI systems.
pub fn junit(report: &RunReport) -> String {
    fn esc(s: &str) -> String {
        s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
    }
    let leaves: Vec<_> = report.all_steps().into_iter().filter(|s| s.children.is_empty()).collect();
    let failures = leaves.iter().filter(|s| s.status == Some(StepStatus::Failed)).count();
    let errors = leaves.iter().filter(|s| s.status == Some(StepStatus::Errored)).count();
    let skipped = leaves.iter().filter(|s| s.status == Some(StepStatus::Skipped)).count();
    let mut out = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<testsuite name=\"{}\" tests=\"{}\" failures=\"{failures}\" errors=\"{errors}\" skipped=\"{skipped}\" time=\"{:.3}\">\n",
        esc(&report.flow_id),
        leaves.len(),
        report.duration_ms as f64 / 1000.0
    );
    for s in leaves {
        out.push_str(&format!(
            "  <testcase classname=\"{}\" name=\"{}\" time=\"{:.3}\">",
            esc(&report.flow_id),
            esc(&s.id),
            s.duration_ms as f64 / 1000.0
        ));
        let msg = esc(s.headline.as_deref().or(s.error.as_deref()).unwrap_or(""));
        match s.status {
            Some(StepStatus::Failed) => {
                out.push_str(&format!("<failure message=\"{msg}\">{}</failure>", esc(&s.stderr)))
            }
            Some(StepStatus::Errored) => out.push_str(&format!("<error message=\"{msg}\">{}</error>", esc(&s.stderr))),
            Some(StepStatus::Skipped) => out.push_str("<skipped/>"),
            _ => {}
        }
        out.push_str("</testcase>\n");
    }
    if let Some(e) = &report.error {
        out.push_str(&format!(
            "  <testcase classname=\"{}\" name=\"preflight\"><error message=\"{}\"/></testcase>\n",
            esc(&report.flow_id),
            esc(e)
        ));
    }
    out.push_str("</testsuite>\n");
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn table_is_pipe_delimited_with_dashes_for_empty() {
        let t = table(&["ID", "NAME"], &[vec!["a".into(), "".into()], vec!["long-id".into(), "x | y".into()]]);
        assert_eq!(t, "ID      | NAME\na       | -\nlong-id | x y\n");
    }

    #[test]
    fn durations_read_naturally() {
        assert_eq!(duration(15), "15ms");
        assert_eq!(duration(1500), "1.5s");
        assert_eq!(duration(125_000), "2m05s");
    }
}
