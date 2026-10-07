//! Semantic validation: everything the schema cannot express.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::LazyLock;

use regex::Regex;

use crate::catalog::{Catalog, Diagnostic, LoadedFlow, error, warning};
use crate::expr;
use crate::spec::{API_VERSION, Flow, Step, StepKind};

static FLOW_ID: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[a-z0-9][a-z0-9-]*$").expect("re"));
static STEP_ID: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[a-z][a-z0-9_]*$").expect("re"));
static IDENT: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[A-Za-z_][A-Za-z0-9_]*$").expect("re"));

/// Fields of `steps.<id>` and `self`.
pub const STEP_FIELDS: &[&str] =
    &["status", "outputs", "stdout", "stderr", "exitCode", "durationMs", "json", "body", "headers", "error", "attempt"];
/// Fields of `run`.
pub const RUN_FIELDS: &[&str] = &["id", "startedAt", "startedAtMs", "environment", "flowId", "dir", "projectDir"];

/// Validates every flow in the catalog, appending to its diagnostics.
pub fn validate_catalog(catalog: &mut Catalog) {
    let mut out = Vec::new();
    for lf in &catalog.flows {
        out.extend(validate_flow(lf, catalog));
    }
    out.extend(call_cycles(catalog));
    catalog.diagnostics.extend(out);
}

#[derive(Clone, Copy)]
struct Allow {
    self_: bool,
    secrets: bool,
}

const PLAIN: Allow = Allow { self_: false, secrets: false };
const WITH_SELF: Allow = Allow { self_: true, secrets: false };
const WITH_SECRETS: Allow = Allow { self_: false, secrets: true };
const SELF_AND_SECRETS: Allow = Allow { self_: true, secrets: true };

struct Ctx<'a> {
    lf: &'a LoadedFlow,
    catalog: &'a Catalog,
    diags: Vec<Diagnostic>,
    inputs: BTreeSet<String>,
    vars: BTreeSet<String>,
    secrets: BTreeSet<String>,
    /// Every step id in the flow, with its definition.
    all_steps: BTreeMap<String, &'a Step>,
}

impl<'a> Ctx<'a> {
    fn err(&mut self, at: &str, message: impl Into<String>) {
        self.diags.push(error(&self.lf.rel_path, Some(&self.lf.flow.id), Some(at), message.into()));
    }
    fn warn(&mut self, at: &str, message: impl Into<String>) {
        self.diags.push(warning(&self.lf.rel_path, Some(&self.lf.flow.id), Some(at), message.into()));
    }

    /// Checks one CEL expression.
    fn check_expr(&mut self, at: &str, source: &str, allow: Allow, visible: &BTreeSet<String>) {
        let refs = match expr::references(source) {
            Ok(r) => r,
            Err(e) => return self.err(at, e.to_string()),
        };
        for r in refs {
            self.check_ref(at, &r, allow, visible);
        }
    }

    /// Checks every `${{ }}` in a template string.
    fn check_template(&mut self, at: &str, text: &str, allow: Allow, visible: &BTreeSet<String>) {
        for e in expr::template_exprs(text) {
            self.check_expr(at, e, allow, visible);
        }
    }

    fn check_json_templates(&mut self, at: &str, v: &serde_json::Value, allow: Allow, visible: &BTreeSet<String>) {
        match v {
            serde_json::Value::String(s) => self.check_template(at, s, allow, visible),
            serde_json::Value::Array(items) => {
                items.iter().for_each(|i| self.check_json_templates(at, i, allow, visible))
            }
            serde_json::Value::Object(m) => m.values().for_each(|i| self.check_json_templates(at, i, allow, visible)),
            _ => {}
        }
    }

    fn check_ref(&mut self, at: &str, r: &[String], allow: Allow, visible: &BTreeSet<String>) {
        let root = r[0].as_str();
        let field = r.get(1).map(String::as_str);
        match root {
            "inputs" => {
                if let Some(f) = field
                    && !self.inputs.contains(f)
                {
                    self.err(at, format!("inputs.{f} is not a declared input"));
                }
            }
            "vars" => {
                if let Some(f) = field
                    && !self.vars.contains(f)
                {
                    self.err(at, format!("vars.{f} is not set in vars, environments or input choices"));
                }
            }
            "secrets" => {
                if !allow.secrets {
                    self.err(
                        at,
                        "secrets can only be used in env values, http requests, browser values and cleanup env, so they never reach reports",
                    );
                } else if let Some(f) = field
                    && !self.secrets.contains(f)
                {
                    self.err(at, format!("secrets.{f} is not listed in the flow's secrets"));
                }
            }
            "run" => {
                if let Some(f) = field
                    && !RUN_FIELDS.contains(&f)
                {
                    self.err(at, format!("run.{f} does not exist (fields: {})", RUN_FIELDS.join(", ")));
                }
            }
            "self" => {
                if !allow.self_ {
                    self.err(at, "self is only available in until, expect, outputs, artifacts and cleanup");
                } else if let Some(f) = field
                    && !STEP_FIELDS.contains(&f)
                {
                    self.err(at, format!("self.{f} does not exist (fields: {})", STEP_FIELDS.join(", ")));
                }
            }
            "steps" => {
                let Some(id) = field else { return };
                if !self.all_steps.contains_key(id) {
                    return self.err(at, format!("steps.{id}: no step with that id"));
                }
                if !visible.contains(id) {
                    return self.err(at, format!("steps.{id} has not run yet at this point"));
                }
                if let Some(sub) = r.get(2) {
                    if !STEP_FIELDS.contains(&sub.as_str()) {
                        return self
                            .err(at, format!("steps.{id}.{sub} does not exist (fields: {})", STEP_FIELDS.join(", ")));
                    }
                    if sub == "outputs"
                        && let Some(name) = r.get(3)
                    {
                        let step = self.all_steps[id];
                        if let Some(known) = self.known_outputs(step)
                            && !known.contains(name)
                        {
                            self.err(at, format!("steps.{id} has no output '{name}'"));
                        }
                    }
                }
            }
            other => self.err(at, format!("unknown name '{other}' (use inputs, vars, secrets, steps, run or self)")),
        }
    }

    /// The output names a step is known to produce, when they can be known
    /// statically.
    fn known_outputs(&self, step: &Step) -> Option<BTreeSet<String>> {
        match step.kind() {
            StepKind::Run | StepKind::Http | StepKind::Assert => Some(step.outputs.keys().cloned().collect()),
            StepKind::Group => Some(BTreeSet::new()),
            StepKind::Flow => {
                let callee = self.catalog.get(step.flow.as_deref()?)?;
                Some(callee.flow.outputs.keys().cloned().collect())
            }
            StepKind::Browser => {
                let b = step.browser.as_ref()?;
                if b.script.is_some() {
                    return None;
                }
                let mut names: BTreeSet<String> = step.outputs.keys().cloned().collect();
                names.extend(b.actions.iter().filter_map(|a| a.read.as_ref().map(|r| r.name.clone())));
                Some(names)
            }
        }
    }
}

fn collect_steps<'a>(steps: &'a [Step], out: &mut Vec<(&'a Step, String)>, prefix: &str) {
    for s in steps {
        let at = format!("{prefix}.{}", s.id);
        out.push((s, at.clone()));
        collect_steps(s.children(), out, &at);
    }
}

pub fn validate_flow(lf: &LoadedFlow, catalog: &Catalog) -> Vec<Diagnostic> {
    let flow = &lf.flow;
    let mut vars: BTreeSet<String> = flow.vars.keys().cloned().collect();
    for env in flow.environments.values() {
        vars.extend(env.keys().cloned());
    }
    for input in &flow.inputs {
        for c in &input.choices {
            vars.extend(c.sets.keys().cloned());
        }
    }
    let mut ctx = Ctx {
        lf,
        catalog,
        diags: Vec::new(),
        inputs: flow.inputs.iter().map(|i| i.name.clone()).collect(),
        vars,
        secrets: flow.secrets.iter().cloned().collect(),
        all_steps: BTreeMap::new(),
    };

    if flow.api_version != API_VERSION {
        ctx.err("apiVersion", format!("must be '{API_VERSION}', got '{}'", flow.api_version));
    }
    if !FLOW_ID.is_match(&flow.id) {
        ctx.err("id", format!("'{}' must match [a-z0-9][a-z0-9-]*", flow.id));
    }
    check_names(&mut ctx, flow);

    // Step ids: unique across steps, nested steps and finally.
    let mut all = Vec::new();
    collect_steps(&flow.steps, &mut all, "steps");
    collect_steps(&flow.finally, &mut all, "finally");
    for (s, at) in &all {
        if !STEP_ID.is_match(&s.id) {
            ctx.err(
                at,
                format!("step id '{}' must match [a-z][a-z0-9_]* so expressions can read steps.{}", s.id, s.id),
            );
        }
        if ctx.all_steps.insert(s.id.clone(), s).is_some() {
            ctx.err(at, format!("step id '{}' is used more than once", s.id));
        }
    }
    if flow.steps.is_empty() {
        ctx.err("steps", "a flow needs at least one step");
    }

    let mut visible = BTreeSet::new();
    check_steps(&mut ctx, &flow.steps, "steps", &mut visible);
    let mut after = visible.clone();
    check_steps(&mut ctx, &flow.finally, "finally", &mut after);
    for (name, source) in &flow.outputs {
        if !IDENT.is_match(name) {
            ctx.err(&format!("outputs.{name}"), "output names must be identifiers");
        }
        ctx.check_template(&format!("outputs.{name}"), source, PLAIN, &visible);
        if !expr::has_template(source) {
            ctx.check_expr(&format!("outputs.{name}"), source, PLAIN, &visible);
        }
    }
    check_demo(&mut ctx, flow, &all);
    check_services(&mut ctx, flow);
    ctx.diags
}

fn check_names(ctx: &mut Ctx, flow: &Flow) {
    if !flow.environments.is_empty()
        && let Some(d) = &flow.default_environment
        && !flow.environments.contains_key(d)
    {
        ctx.err("defaultEnvironment", format!("'{d}' is not one of the environments"));
    }
    if flow.environments.is_empty() && flow.default_environment.is_some() {
        ctx.err("defaultEnvironment", "set without any environments");
    }
    let mut names = BTreeSet::new();
    for input in &flow.inputs {
        let at = format!("inputs.{}", input.name);
        if !IDENT.is_match(&input.name) {
            ctx.err(&at, "input names must be identifiers ([A-Za-z_][A-Za-z0-9_]*)");
        }
        if !names.insert(input.name.clone()) {
            ctx.err(&at, "declared more than once");
        }
        if flow.vars.contains_key(&input.name) {
            ctx.err(&at, "an input and a var share this name");
        }
        if !input.choices.is_empty() {
            let values: BTreeSet<&str> = input.choices.iter().map(|c| c.value.as_str()).collect();
            if values.len() != input.choices.len() {
                ctx.err(&at, "choice values must be unique");
            }
            if let Some(d) = &input.default
                && !values.contains(d.as_env_string().as_str())
            {
                ctx.err(&at, format!("default '{}' is not one of the choices", d.as_env_string()));
            }
        }
    }
    let var_names: Vec<String> = ctx.vars.iter().cloned().collect();
    for v in var_names {
        if !IDENT.is_match(&v) {
            ctx.err(&format!("vars.{v}"), "variable names must be identifiers, since they are exported to processes");
        }
    }
    for s in &flow.secrets {
        if !IDENT.is_match(s) {
            ctx.err(&format!("secrets.{s}"), "secret names must be identifiers");
        }
    }
}

fn check_steps(ctx: &mut Ctx, steps: &[Step], prefix: &str, visible: &mut BTreeSet<String>) {
    for step in steps {
        let at = format!("{prefix}.{}", step.id);
        check_step(ctx, step, &at, visible);
        visible.insert(step.id.clone());
    }
}

fn check_duration(ctx: &mut Ctx, at: &str, d: &Option<crate::spec::DurationSpec>) {
    if let Some(d) = d
        && let Err(e) = d.parse()
    {
        ctx.err(at, e);
    }
}

fn check_step(ctx: &mut Ctx, step: &Step, at: &str, visible: &mut BTreeSet<String>) {
    let kinds = step.declared_kinds();
    match kinds.len() {
        0 => ctx.err(at, "a step needs one of run, http, assert, browser, flow or steps"),
        1 => {}
        _ => ctx.err(
            at,
            format!(
                "a step can do only one thing, but this one sets {}",
                kinds.iter().map(StepKind::as_str).collect::<Vec<_>>().join(" and ")
            ),
        ),
    }
    let kind = step.kind();
    let v = visible.clone();

    if let Some(w) = &step.when {
        ctx.check_expr(&format!("{at}.when"), w, PLAIN, &v);
    }
    check_duration(ctx, &format!("{at}.timeout"), &step.timeout);
    if let Some(r) = &step.retry {
        if r.attempts == 0 {
            ctx.err(&format!("{at}.retry.attempts"), "must be at least 1");
        }
        check_duration(ctx, &format!("{at}.retry.delay"), &r.delay);
        if let Some(u) = &r.until {
            ctx.check_expr(&format!("{at}.retry.until"), u, WITH_SELF, &v);
            if r.attempts <= 1 {
                ctx.warn(&format!("{at}.retry"), "until with a single attempt never retries");
            }
        }
        if kind == StepKind::Group || kind == StepKind::Flow {
            ctx.err(&format!("{at}.retry"), "retry applies to run, http, assert and browser steps");
        }
    }
    for (k, val) in &step.env {
        if !IDENT.is_match(k) {
            ctx.err(&format!("{at}.env.{k}"), "environment variable names must be identifiers");
        }
        ctx.check_template(&format!("{at}.env.{k}"), val, WITH_SECRETS, &v);
    }
    for (name, source) in &step.outputs {
        if !IDENT.is_match(name) {
            ctx.err(&format!("{at}.outputs.{name}"), "output names must be identifiers");
        }
        ctx.check_expr(&format!("{at}.outputs.{name}"), source, WITH_SELF, &v);
    }
    if let Some(e) = &step.expect {
        for (i, x) in e.items().into_iter().enumerate() {
            ctx.check_expr(&format!("{at}.expect[{i}]"), x, WITH_SELF, &v);
        }
        if matches!(kind, StepKind::Group | StepKind::Flow | StepKind::Assert) {
            ctx.err(&format!("{at}.expect"), "expect applies to run, http and browser steps");
        }
    }
    for (i, a) in step.artifacts.iter().enumerate() {
        ctx.check_template(&format!("{at}.artifacts[{i}]"), &a.path, WITH_SELF, &v);
    }
    if let Some(c) = &step.cleanup {
        if expr::has_template(&c.run) {
            ctx.err(&format!("{at}.cleanup.run"), "use cleanup.env to pass values into the command, not ${{ }}");
        }
        for (k, val) in &c.env {
            if !IDENT.is_match(k) {
                ctx.err(&format!("{at}.cleanup.env.{k}"), "environment variable names must be identifiers");
            }
            ctx.check_template(&format!("{at}.cleanup.env.{k}"), val, SELF_AND_SECRETS, &v);
        }
        check_duration(ctx, &format!("{at}.cleanup.timeout"), &c.timeout);
    }
    if matches!(kind, StepKind::Group | StepKind::Flow) {
        let what = if kind == StepKind::Group { "groups" } else { "flow steps" };
        if !step.env.is_empty() {
            ctx.err(&format!("{at}.env"), format!("env does not apply to {what}; set it on the steps inside"));
        }
        if step.timeout.is_some() {
            ctx.err(&format!("{at}.timeout"), format!("timeout does not apply to {what}; set it on the steps inside"));
        }
        if !step.outputs.is_empty() {
            ctx.err(
                &format!("{at}.outputs"),
                if kind == StepKind::Group {
                    "groups have no outputs; read the steps inside directly (steps.<child>.outputs)"
                } else {
                    "a flow step's outputs are the called flow's outputs; declare them there"
                },
            );
        }
    }
    if kind != StepKind::Run && (step.shell.is_some() || step.cwd.is_some()) {
        ctx.err(at, "shell and cwd apply only to run steps");
    }
    if kind != StepKind::Flow && !step.with.is_empty() {
        ctx.err(&format!("{at}.with"), "with applies only to flow steps");
    }
    if kind != StepKind::Group && (step.parallel || step.max_parallel.is_some()) {
        ctx.err(at, "parallel and maxParallel apply only to groups (steps:)");
    }
    if step.max_parallel == Some(0) {
        ctx.err(&format!("{at}.maxParallel"), "must be at least 1");
    }

    match kind {
        StepKind::Run => {
            let run = step.run.as_deref().unwrap_or_default();
            if expr::has_template(run) {
                ctx.err(
                    &format!("{at}.run"),
                    "scripts cannot contain ${{ }}; pass values through env (env: {NAME: \"${{ ... }}\"}) and read $NAME, so values are never parsed as shell code",
                );
            }
            if run.trim().is_empty() {
                ctx.err(&format!("{at}.run"), "empty command");
            }
            if let Some(sh) = &step.shell
                && !["sh", "bash", "zsh", "pwsh", "none"].contains(&sh.as_str())
            {
                ctx.err(&format!("{at}.shell"), format!("'{sh}' is not one of sh, bash, zsh, pwsh, none"));
            }
        }
        StepKind::Http => {
            let h = step.http.as_ref().expect("http");
            if let Some(m) = &h.method
                && !["GET", "POST", "PUT", "PATCH", "DELETE", "HEAD"].contains(&m.to_uppercase().as_str())
            {
                ctx.err(&format!("{at}.http.method"), format!("unsupported method '{m}'"));
            }
            ctx.check_template(&format!("{at}.http.url"), &h.url, WITH_SECRETS, &v);
            for (k, val) in &h.headers {
                ctx.check_template(&format!("{at}.http.headers.{k}"), val, WITH_SECRETS, &v);
            }
            if let Some(j) = &h.json {
                ctx.check_json_templates(&format!("{at}.http.json"), j, WITH_SECRETS, &v);
            }
            if let Some(b) = &h.body {
                ctx.check_template(&format!("{at}.http.body"), b, WITH_SECRETS, &v);
            }
            if h.json.is_some() && h.body.is_some() {
                ctx.err(&format!("{at}.http"), "set json or body, not both");
            }
        }
        StepKind::Assert => {
            for (i, x) in step.assert.as_ref().expect("assert").items().into_iter().enumerate() {
                ctx.check_expr(&format!("{at}.assert[{i}]"), x, PLAIN, &v);
            }
        }
        StepKind::Browser => check_browser(ctx, step, at, &v),
        StepKind::Flow => check_call(ctx, step, at, &v),
        StepKind::Group => {
            let children = step.children();
            if children.is_empty() {
                ctx.err(&format!("{at}.steps"), "a group needs at least one step");
            }
            if step.parallel {
                let before = visible.clone();
                for child in children {
                    let mut own = before.clone();
                    check_step(ctx, child, &format!("{at}.{}", child.id), &mut own);
                    collect_ids(child, visible);
                }
            } else {
                for child in children {
                    let cat = format!("{at}.{}", child.id);
                    check_step(ctx, child, &cat, visible);
                    collect_ids(child, visible);
                }
            }
        }
    }
}

fn collect_ids(step: &Step, visible: &mut BTreeSet<String>) {
    visible.insert(step.id.clone());
    for c in step.children() {
        collect_ids(c, visible);
    }
}

fn check_browser(ctx: &mut Ctx, step: &Step, at: &str, v: &BTreeSet<String>) {
    let b = step.browser.as_ref().expect("browser");
    if b.actions.is_empty() && b.script.is_none() {
        ctx.err(&format!("{at}.browser"), "set actions, script, or both");
    }
    if let Some(u) = &b.url {
        ctx.check_template(&format!("{at}.browser.url"), u, WITH_SECRETS, v);
    }
    if let Some(script) = &b.script {
        let path = ctx.lf.dir().join(script);
        if !path.is_file() {
            ctx.err(&format!("{at}.browser.script"), format!("{} does not exist", path.display()));
        }
    }
    for name in [&b.session, &b.save_session].into_iter().flatten() {
        if !STEP_ID.is_match(name) && !FLOW_ID.is_match(name) {
            ctx.err(
                &format!("{at}.browser"),
                format!("session name '{name}' must be lowercase letters, digits, - or _"),
            );
        }
    }
    for (i, a) in b.actions.iter().enumerate() {
        let aat = format!("{at}.browser.actions[{i}]");
        let declared = a.declared();
        if declared.len() != 1 {
            ctx.err(
                &aat,
                if declared.is_empty() {
                    "an action needs exactly one of goto, click, fill, press, waitFor, expectText, expectUrl, screenshot, beat, wait, hover, select, read".to_string()
                } else {
                    format!("an action does one thing, but this one sets {}", declared.join(" and "))
                },
            );
        }
        for t in [&a.goto, &a.click, &a.wait_for, &a.hover].into_iter().flatten() {
            ctx.check_template(&aat, t, WITH_SECRETS, v);
        }
        if let Some(f) = a.fill.as_ref().or(a.select.as_ref()) {
            ctx.check_template(&aat, &f.value, WITH_SECRETS, v);
        }
        if let Some(t) = &a.expect_text {
            ctx.check_template(&aat, &t.text, WITH_SECRETS, v);
        }
        if let Some(re) = &a.expect_url
            && let Err(e) = Regex::new(re)
        {
            ctx.err(&aat, format!("expectUrl is not a valid regex: {e}"));
        }
        if let Some(w) = &a.wait
            && let Err(e) = w.parse()
        {
            ctx.err(&aat, e);
        }
        if let Some(r) = &a.read
            && !IDENT.is_match(&r.name)
        {
            ctx.err(&aat, "read.name must be an identifier");
        }
        if let Some(s) = &a.screenshot
            && (s.is_empty() || s.contains('/') || s.contains('\\'))
        {
            ctx.err(&aat, "screenshot names are plain file names");
        }
    }
}

fn check_call(ctx: &mut Ctx, step: &Step, at: &str, v: &BTreeSet<String>) {
    let target = step.flow.as_deref().unwrap_or_default();
    for (k, val) in &step.with {
        ctx.check_template(&format!("{at}.with.{k}"), val, PLAIN, v);
    }
    let Some(callee) = ctx.catalog.get(target) else {
        return ctx.err(&format!("{at}.flow"), format!("no flow with id '{target}'"));
    };
    let declared: BTreeMap<&str, &crate::spec::Input> =
        callee.flow.inputs.iter().map(|i| (i.name.as_str(), i)).collect();
    for k in step.with.keys() {
        if !declared.contains_key(k.as_str()) {
            ctx.err(&format!("{at}.with.{k}"), format!("flow '{target}' has no input '{k}'"));
        }
    }
    for (name, input) in declared {
        if input.required && input.default.is_none() && !step.with.contains_key(name) {
            ctx.err(&format!("{at}.with"), format!("flow '{target}' requires input '{name}'"));
        }
    }
}

fn check_demo(ctx: &mut Ctx, flow: &Flow, all: &[(&Step, String)]) {
    let Some(demo) = &flow.demo else { return };
    let recorded: Vec<&Step> =
        all.iter().map(|(s, _)| *s).filter(|s| s.browser.as_ref().is_some_and(|b| b.record)).collect();
    if recorded.is_empty() {
        ctx.err("demo", "a demo needs a browser step with record: true");
    }
    let mut seen = BTreeSet::new();
    for (i, b) in demo.beats.iter().enumerate() {
        if !seen.insert(b.marker.as_str()) {
            ctx.err(&format!("demo.beats[{i}]"), format!("marker '{}' is labeled twice", b.marker));
        }
    }
    // Markers can be checked only when every recorded step is declarative.
    if !recorded.is_empty() && recorded.iter().all(|s| s.browser.as_ref().is_some_and(|b| b.script.is_none())) {
        let emitted: BTreeSet<&str> = recorded
            .iter()
            .flat_map(|s| s.browser.as_ref().map(|b| b.actions.as_slice()).unwrap_or(&[]))
            .filter_map(|a| a.beat.as_deref())
            .collect();
        for (i, b) in demo.beats.iter().enumerate() {
            if !emitted.contains(b.marker.as_str()) {
                ctx.err(
                    &format!("demo.beats[{i}]"),
                    format!("no recorded step marks beat '{}', so this label would never show", b.marker),
                );
            }
        }
    }
    if let Some(cut) = &demo.cut {
        check_duration(ctx, "demo.cut.maxGap", &cut.max_gap);
        check_duration(ctx, "demo.cut.keep", &cut.keep);
    }
}

fn check_services(ctx: &mut Ctx, flow: &Flow) {
    let mut ids = BTreeSet::new();
    let none = BTreeSet::new();
    for svc in &flow.services {
        let at = format!("services.{}", svc.id);
        if !STEP_ID.is_match(&svc.id) && !FLOW_ID.is_match(&svc.id) {
            ctx.err(&at, "service ids are lowercase letters, digits, - or _");
        }
        if !ids.insert(svc.id.as_str()) {
            ctx.err(&at, "service id is used more than once");
        }
        if expr::has_template(&svc.run) {
            ctx.err(&format!("{at}.run"), "pass values through env, not ${{ }}");
        }
        for (k, t) in &svc.env {
            if !IDENT.is_match(k) {
                ctx.err(&format!("{at}.env.{k}"), "environment variable names must be identifiers");
            }
            ctx.check_template(&format!("{at}.env.{k}"), t, WITH_SECRETS, &none);
        }
        match (&svc.ready.http, &svc.ready.log) {
            (None, None) => ctx.err(&format!("{at}.ready"), "set ready.http, ready.log, or both"),
            (http, log) => {
                if let Some(u) = http {
                    ctx.check_template(&format!("{at}.ready.http"), u, PLAIN, &none);
                }
                if let Some(re) = log
                    && let Err(e) = Regex::new(re)
                {
                    ctx.err(&format!("{at}.ready.log"), format!("invalid regex: {e}"));
                }
            }
        }
        check_duration(ctx, &format!("{at}.ready.timeout"), &svc.ready.timeout);
    }
}

/// Flow calls must not form a cycle.
fn call_cycles(catalog: &Catalog) -> Vec<Diagnostic> {
    fn calls(steps: &[Step], out: &mut Vec<String>) {
        for s in steps {
            if let Some(f) = &s.flow {
                out.push(f.clone());
            }
            calls(s.children(), out);
        }
    }
    let graph: BTreeMap<&str, Vec<String>> = catalog
        .flows
        .iter()
        .map(|lf| {
            let mut v = Vec::new();
            calls(&lf.flow.steps, &mut v);
            calls(&lf.flow.finally, &mut v);
            (lf.flow.id.as_str(), v)
        })
        .collect();
    let mut diags = Vec::new();
    for lf in &catalog.flows {
        let mut stack = vec![(lf.flow.id.clone(), vec![lf.flow.id.clone()])];
        let mut reported = false;
        while let Some((node, path)) = stack.pop() {
            for next in graph.get(node.as_str()).into_iter().flatten() {
                if *next == lf.flow.id && !reported {
                    let mut cycle = path.clone();
                    cycle.push(next.clone());
                    diags.push(error(
                        &lf.rel_path,
                        Some(&lf.flow.id),
                        None,
                        format!("flow calls form a cycle: {}", cycle.join(" -> ")),
                    ));
                    reported = true;
                } else if !path.contains(next) {
                    let mut p = path.clone();
                    p.push(next.clone());
                    stack.push((next.clone(), p));
                }
            }
        }
    }
    diags
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::{Severity, parse_flow};

    fn catalog(flows: &[&str]) -> Catalog {
        let mut c = Catalog::default();
        for (i, text) in flows.iter().enumerate() {
            let flow = parse_flow(text).unwrap_or_else(|e| panic!("{e}"));
            c.flows.push(LoadedFlow {
                path: format!("/tmp/f{i}.lest.yaml").into(),
                rel_path: format!("f{i}.lest.yaml"),
                flow,
                sha256: String::new(),
            });
        }
        validate_catalog(&mut c);
        c
    }

    fn errors(c: &Catalog) -> Vec<String> {
        c.diagnostics
            .iter()
            .filter(|d| d.severity == Severity::Error)
            .map(|d| format!("{}: {}", d.at.clone().unwrap_or_default(), d.message))
            .collect()
    }

    #[test]
    fn accepts_a_well_formed_flow() {
        let c = catalog(&[r#"
apiVersion: lest/v1
id: ok
name: OK
environments:
  local: { base_url: "http://127.0.0.1:1" }
defaultEnvironment: local
inputs:
  - name: plan
    default: pro
    choices: [{ value: pro, sets: { seats: 5 } }, { value: free }]
secrets: [token]
steps:
  - id: create
    run: echo '{"id":"u1"}'
    outputs: { id: self.json.id }
    retry: { attempts: 3, until: "self.exitCode == 0" }
    cleanup: { run: 'echo delete "$ID"', env: { ID: "${{ self.outputs.id }}" } }
  - id: fetch
    http:
      url: "${{ vars.base_url }}/users/${{ steps.create.outputs.id }}"
      headers: { Authorization: "Bearer ${{ secrets.token }}" }
    expect: "self.status == 200"
  - id: check
    assert: ["steps.fetch.status == 200", "vars.seats == 5 || inputs.plan == 'free'"]
"#]);
        assert!(errors(&c).is_empty(), "{:?}", errors(&c));
    }

    #[test]
    fn rejects_templates_in_scripts_and_secrets_in_assertions() {
        let c = catalog(&[r#"
apiVersion: lest/v1
id: bad
name: Bad
secrets: [token]
steps:
  - id: a
    run: echo ${{ secrets.token }}
  - id: b
    assert: "secrets.token != ''"
"#]);
        let e = errors(&c).join("\n");
        assert!(e.contains("scripts cannot contain"), "{e}");
        assert!(e.contains("secrets can only be used"), "{e}");
    }

    #[test]
    fn rejects_forward_and_unknown_references() {
        let c = catalog(&[r#"
apiVersion: lest/v1
id: refs
name: Refs
steps:
  - id: a
    assert: "steps.b.status == 'passed'"
  - id: b
    assert: "steps.nope.status == 'passed' && vars.missing == 1 && inputs.x == 1"
  - id: c
    assert: "steps.a.outputs.missing == 1"
"#]);
        let e = errors(&c).join("\n");
        assert!(e.contains("steps.b has not run yet"), "{e}");
        assert!(e.contains("steps.nope: no step"), "{e}");
        assert!(e.contains("vars.missing"), "{e}");
        assert!(e.contains("inputs.x"), "{e}");
        assert!(e.contains("has no output 'missing'"), "{e}");
    }

    #[test]
    fn parallel_siblings_cannot_see_each_other() {
        let c = catalog(&[r#"
apiVersion: lest/v1
id: par
name: Par
steps:
  - id: both
    parallel: true
    steps:
      - { id: one, run: "true" }
      - { id: two, assert: "steps.one.status == 'passed'" }
  - id: after
    assert: "steps.one.status == 'passed' && steps.two.status == 'passed'"
"#]);
        let e = errors(&c);
        assert_eq!(e.len(), 1, "{e:?}");
        assert!(e[0].contains("steps.one has not run yet"));
    }

    #[test]
    fn checks_flow_calls_and_cycles() {
        let c = catalog(&[
            r#"
apiVersion: lest/v1
id: caller
name: Caller
steps:
  - { id: call, flow: callee, with: { unknown: x } }
  - { id: use, assert: "steps.call.outputs.token != ''" }
"#,
            r#"
apiVersion: lest/v1
id: callee
name: Callee
inputs: [{ name: who, required: true }]
outputs: { token: "steps.make.outputs.t" }
steps:
  - { id: make, run: echo t, outputs: { t: self.stdout } }
  - { id: back, flow: caller }
"#,
        ]);
        let e = errors(&c).join("\n");
        assert!(e.contains("has no input 'unknown'"), "{e}");
        assert!(e.contains("requires input 'who'"), "{e}");
        assert!(e.contains("cycle"), "{e}");
        assert!(!e.contains("no output 'token'"), "{e}");
    }

    #[test]
    fn rejects_steps_doing_two_things_and_bad_ids() {
        let c = catalog(&[r#"
apiVersion: lest/v1
id: Bad_Id
name: Two
steps:
  - { id: Sign-In, run: "true", assert: "true" }
"#]);
        let e = errors(&c).join("\n");
        assert!(e.contains("only one thing"), "{e}");
        assert!(e.contains("must match [a-z][a-z0-9_]*"), "{e}");
        assert!(e.contains("[a-z0-9][a-z0-9-]*"), "{e}");
    }

    #[test]
    fn demo_beats_must_be_marked() {
        let c = catalog(&[r#"
apiVersion: lest/v1
id: demo
name: Demo
demo:
  beats: [{ marker: home, label: Home }, { marker: never, label: Never }]
steps:
  - id: rec
    browser:
      url: http://127.0.0.1:1
      record: true
      actions: [{ beat: home }]
"#]);
        let e = errors(&c).join("\n");
        assert!(e.contains("beat 'never'"), "{e}");
        assert!(!e.contains("beat 'home'"), "{e}");
    }

    #[test]
    fn unknown_keys_fail_to_parse() {
        let err = parse_flow("apiVersion: lest/v1\nid: x\nname: x\nsteps:\n  - id: a\n    rnu: echo\n").unwrap_err();
        assert!(err.contains("rnu"), "{err}");
    }
}
