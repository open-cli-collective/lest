//! End-to-end tests of the `lest` binary: selecting and running several
//! flows, and the machine-readable outputs CI reads.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const PASSES: &str = r#"apiVersion: lest/v1
id: passes
name: Passes
tags: [smoke]
steps:
  - id: ok
    run: "true"
"#;

const FAILS: &str = r#"apiVersion: lest/v1
id: fails
name: Fails
tags: [smoke]
steps:
  - id: boom
    run: echo "it broke | badly" >&2; exit 1
"#;

const NEEDS_INPUT: &str = r#"apiVersion: lest/v1
id: needs-input
name: Needs input
inputs:
  - name: who
    required: true
steps:
  - id: greet
    run: echo "hello $who"
"#;

struct Project {
    dir: tempfile::TempDir,
}

impl Project {
    fn new() -> Project {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("lest.yaml"), "name: t\nflows: [flows]\n").unwrap();
        std::fs::create_dir(dir.path().join("flows")).unwrap();
        for (name, body) in [("passes", PASSES), ("fails", FAILS), ("needs-input", NEEDS_INPUT)] {
            std::fs::write(dir.path().join("flows").join(format!("{name}.lest.yaml")), body).unwrap();
        }
        Project { dir }
    }

    fn root(&self) -> &Path {
        self.dir.path()
    }

    fn path(&self, rel: &str) -> PathBuf {
        self.dir.path().join(rel)
    }

    fn cmd(&self, args: &[&str]) -> Command {
        let state = self.path(".state");
        let mut c = Command::new(env!("CARGO_BIN_EXE_lest"));
        c.args(args)
            .current_dir(self.root())
            .env("LEST_DATA_DIR", state.join("data"))
            .env("LEST_CONFIG_DIR", state.join("config"))
            .env("LEST_LOCK_DIR", state.join("locks"))
            .env("NO_COLOR", "1")
            .env_remove("GITHUB_STEP_SUMMARY")
            .stdin(std::process::Stdio::null());
        c
    }

    fn run(&self, args: &[&str]) -> Output {
        self.cmd(args).output().unwrap()
    }

    fn git(&self, args: &[&str]) {
        let ok = Command::new("git")
            .args(["-c", "user.name=t", "-c", "user.email=t@example.com"])
            .args(args)
            .current_dir(self.root())
            .output()
            .unwrap()
            .status
            .success();
        assert!(ok, "git {args:?}");
    }
}

fn stdout(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

fn code(o: &Output) -> i32 {
    o.status.code().unwrap()
}

#[test]
fn several_flows_write_one_report_each_and_exit_with_the_worst() {
    let p = Project::new();
    let summary = p.path("summary.md");
    let o = p
        .cmd(&["run", "passes", "fails", "-o", "reports", "--junit", "junit.xml"])
        .env("GITHUB_STEP_SUMMARY", &summary)
        .output()
        .unwrap();
    assert_eq!(code(&o), 1, "{}", String::from_utf8_lossy(&o.stderr));
    let out = stdout(&o);
    assert!(out.contains("FLOW"), "summary table: {out}");
    assert!(out.contains("fails") && out.contains("boom"), "{out}");

    for flow in ["passes", "fails"] {
        let report: serde_json::Value =
            serde_json::from_slice(&std::fs::read(p.path(&format!("reports/{flow}.json"))).unwrap()).unwrap();
        assert_eq!(report["flowId"], flow);
    }
    let junit = std::fs::read_to_string(p.path("junit.xml")).unwrap();
    assert!(junit.contains("<testsuites>"));
    assert_eq!(junit.matches("<testsuite ").count(), 2);
    assert!(junit.contains("<failure"));

    let md = std::fs::read_to_string(&summary).unwrap();
    assert!(md.starts_with("### Lest: 1 passed, 1 failed"), "{md}");
    assert!(md.contains("| `fails` | failed |"), "{md}");
    assert!(md.contains("`boom`"), "{md}");
    assert!(md.contains(r"it broke \| badly"), "a pipe in a headline stays in its cell: {md}");
    assert_eq!(md.lines().filter(|l| l.starts_with("| `")).count(), 2, "{md}");
}

#[test]
fn a_run_that_could_not_start_outranks_a_failure() {
    let p = Project::new();
    // needs-input errors (a required input is missing), fails fails.
    let o = p.run(&["run", "fails", "needs-input"]);
    assert_eq!(code(&o), 3, "{}", stdout(&o));
}

#[test]
fn a_tag_selects_its_flows_and_they_can_run_in_parallel() {
    let p = Project::new();
    let o = p.run(&["run", "--tag", "smoke", "--max-parallel", "2", "-o", "reports"]);
    assert_eq!(code(&o), 1);
    assert!(p.path("reports/passes.json").is_file());
    assert!(p.path("reports/fails.json").is_file());
    assert!(!p.path("reports/needs-input.json").exists());
}

#[test]
fn selection_mistakes_fail_before_anything_runs() {
    let p = Project::new();
    assert_eq!(code(&p.run(&["run"])), 2);
    assert_eq!(code(&p.run(&["run", "--tag", "nope"])), 4);
    assert_eq!(code(&p.run(&["run", "passes", "missing"])), 4);
    assert_eq!(code(&p.run(&["run", "passes", "fails", "--from", "ok"])), 2);
    assert_eq!(code(&p.run(&["run", "passes", "fails", "--input", "who=x"])), 2);
    assert!(
        !p.path(".state/data/runs").exists() || std::fs::read_dir(p.path(".state/data/runs")).unwrap().next().is_none()
    );
}

#[test]
fn each_flow_gets_only_the_inputs_it_declares() {
    let p = Project::new();
    let o = p.run(&["run", "passes", "needs-input", "--input", "who=world"]);
    assert_eq!(code(&o), 0, "{}{}", stdout(&o), String::from_utf8_lossy(&o.stderr));
}

#[test]
fn one_named_flow_keeps_file_outputs() {
    let p = Project::new();
    let o = p.run(&["run", "passes", "-o", "report.json", "--junit", "junit.xml"]);
    assert_eq!(code(&o), 0);
    let report: serde_json::Value = serde_json::from_slice(&std::fs::read(p.path("report.json")).unwrap()).unwrap();
    assert_eq!(report["result"], "passed");
    let junit = std::fs::read_to_string(p.path("junit.xml")).unwrap();
    assert!(!junit.contains("<testsuites>") && junit.contains("<testsuite "));
}

#[test]
fn list_prints_json_for_a_matrix() {
    let p = Project::new();
    let o = p.run(&["list", "--tag", "smoke", "--json"]);
    assert_eq!(code(&o), 0);
    let out = stdout(&o);
    assert_eq!(out.trim().lines().count(), 1, "one line for GITHUB_OUTPUT");
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    let ids: Vec<&str> = v.as_array().unwrap().iter().map(|f| f["id"].as_str().unwrap()).collect();
    assert_eq!(ids, ["fails", "passes"]);
    assert_eq!(v[0]["tags"][0], "smoke");
}

#[test]
fn affected_selects_changed_flows_as_json_and_for_run() {
    let p = Project::new();
    p.git(&["init", "-q"]);
    p.git(&["add", "lest.yaml", "flows"]);
    p.git(&["commit", "-q", "-m", "init"]);

    let o = p.run(&["run", "--affected", "HEAD"]);
    assert_eq!(code(&o), 0, "nothing changed, nothing to run");

    std::fs::write(p.path("flows/passes.lest.yaml"), PASSES.replace("name: Passes", "name: Still passes")).unwrap();
    let o = p.run(&["affected", "--base", "HEAD", "--json"]);
    assert_eq!(code(&o), 0);
    let v: serde_json::Value = serde_json::from_str(&stdout(&o)).unwrap();
    assert_eq!(v[0]["id"], "passes");
    assert!(v[0]["reasons"].as_array().is_some_and(|r| !r.is_empty()));

    let o = p.run(&["run", "--affected", "HEAD", "-o", "reports"]);
    assert_eq!(code(&o), 0);
    assert!(p.path("reports/passes.json").is_file());
    assert!(!p.path("reports/fails.json").exists());
}
