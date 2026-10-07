//! Expressions: CEL everywhere (`when`, `until`, `assert`, `expect`,
//! `outputs`), and `${{ expr }}` interpolation in string fields.

use std::collections::BTreeSet;
use std::sync::{Arc, LazyLock};

use cel::common::ast::{Expr, IdedExpr};
use cel::extractors::This;
use cel::{Context, Env, ExecutionError, Program, Value, extensions};
use regex::Regex;
use serde_json::{Map, Value as Json};

static TEMPLATE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\$\{\{\s*(.*?)\s*\}\}").expect("template regex"));

/// The standard library plus the CEL string, list, math and encoder
/// extensions (`trim`, `split`, `replace`, `join`, `math.greatest`, ...).
static ENV: LazyLock<Arc<Env>> = LazyLock::new(|| {
    let mut env = Env::stdlib();
    env.add_extension(extensions::strings).expect("strings extension");
    env.add_extension(extensions::lists).expect("lists extension");
    env.add_extension(extensions::math).expect("math extension");
    env.add_extension(extensions::encoders).expect("encoders extension");
    Arc::new(env)
});

/// The variables an expression can read, as JSON values.
#[derive(Debug, Clone, Default)]
pub struct Scope {
    vars: Map<String, Json>,
}

impl Scope {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn set(&mut self, name: &str, value: Json) {
        self.vars.insert(name.to_string(), value);
    }

    pub fn get(&self, name: &str) -> Option<&Json> {
        self.vars.get(name)
    }

    /// A copy with `name` bound to `value` (for `self`).
    pub fn with(&self, name: &str, value: Json) -> Scope {
        let mut s = self.clone();
        s.set(name, value);
        s
    }
}

#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum ExprError {
    #[error("cannot parse `{expr}`: {message}")]
    Parse { expr: String, message: String },
    #[error("`{expr}`: {message}")]
    Eval { expr: String, message: String },
    #[error("`{expr}` must be true or false, got {got}")]
    NotBool { expr: String, got: String },
}

/// Parses an expression without evaluating it.
pub fn compile(expr: &str) -> Result<Program, ExprError> {
    ENV.compile(expr).map_err(|e| ExprError::Parse {
        expr: expr.to_string(),
        message: e.to_string().lines().next().unwrap_or("syntax error").to_string(),
    })
}

fn context(scope: &Scope) -> Result<Context<'static, 'static>, ExecutionError> {
    let mut ctx = Context::with_env(ENV.clone());
    for (name, value) in &scope.vars {
        ctx.add_variable_from_value(name.clone(), from_json(value));
    }
    ctx.add_function("capture", capture).ok();
    Ok(ctx)
}

/// JSON to CEL. Integers become `int` (not `uint`) so arithmetic and
/// comparisons with literals work.
fn from_json(v: &Json) -> Value {
    match v {
        Json::Null => Value::Null,
        Json::Bool(b) => Value::Bool(*b),
        Json::Number(n) => match (n.as_i64(), n.as_u64()) {
            (Some(i), _) => Value::Int(i),
            (None, Some(u)) => Value::UInt(u),
            _ => Value::Float(n.as_f64().unwrap_or(f64::NAN)),
        },
        Json::String(s) => Value::String(Arc::new(s.clone())),
        Json::Array(items) => Value::List(Arc::new(items.iter().map(from_json).collect())),
        Json::Object(map) => {
            let m: std::collections::HashMap<String, Value> =
                map.iter().map(|(k, v)| (k.clone(), from_json(v))).collect();
            Value::from(m)
        }
    }
}

/// `text.capture(regex)`: the first capture group of the first match, or
/// the whole match when the regex has no group. An error when nothing
/// matches, so a missing value fails the step instead of passing as "".
fn capture(This(text): This<Arc<String>>, pattern: Arc<String>) -> Result<Arc<String>, ExecutionError> {
    let re = Regex::new(&pattern).map_err(|e| ExecutionError::function_error("capture", e.to_string()))?;
    let caps = re
        .captures(&text)
        .ok_or_else(|| ExecutionError::function_error("capture", format!("no match for /{pattern}/")))?;
    let m = caps.get(1).or_else(|| caps.get(0)).map(|m| m.as_str()).unwrap_or_default();
    Ok(Arc::new(m.to_string()))
}

/// Evaluates an expression to a JSON value.
pub fn eval(expr: &str, scope: &Scope) -> Result<Json, ExprError> {
    let program = compile(expr)?;
    let err = |message: String| ExprError::Eval { expr: expr.to_string(), message };
    let ctx = context(scope).map_err(|e| err(e.to_string()))?;
    let value = program.execute(&ctx).map_err(|e| err(e.to_string()))?;
    to_json(&value).map_err(err)
}

fn to_json(value: &Value) -> Result<Json, String> {
    value.json().map_err(|e| e.to_string())
}

pub fn eval_bool(expr: &str, scope: &Scope) -> Result<bool, ExprError> {
    match eval(expr, scope)? {
        Json::Bool(b) => Ok(b),
        other => Err(ExprError::NotBool { expr: expr.to_string(), got: describe(&other) }),
    }
}

/// For a failed comparison `a op b`, the values of both sides, so a failure
/// reads "left: 404, right: 200" instead of "false".
pub fn explain_false(expr: &str, scope: &Scope) -> Option<String> {
    let program = compile(expr).ok()?;
    let Expr::Call(call) = &program.expression().expr else {
        return None;
    };
    let op = match call.func_name.as_str() {
        "_==_" => "==",
        "_!=_" => "!=",
        "_<_" => "<",
        "_<=_" => "<=",
        "_>_" => ">",
        "_>=_" => ">=",
        _ => return None,
    };
    if call.args.len() != 2 {
        return None;
    }
    let ctx = context(scope).ok()?;
    let side = |e: &IdedExpr| -> String {
        match ctx.resolve(e).ok().and_then(|v| to_json(&v).ok()) {
            Some(v) => describe(&v),
            None => "<error>".to_string(),
        }
    };
    Some(format!("left {op} right was false: left = {}, right = {}", side(&call.args[0]), side(&call.args[1])))
}

/// A short rendering of a value for messages.
pub fn describe(v: &Json) -> String {
    let s = match v {
        Json::String(s) => format!("{s:?}"),
        other => other.to_string(),
    };
    if s.chars().count() > 200 { format!("{}…", s.chars().take(200).collect::<String>()) } else { s }
}

/// The string form of a value when interpolated into text.
pub fn stringify(v: &Json) -> String {
    match v {
        Json::Null => String::new(),
        Json::String(s) => s.clone(),
        other => other.to_string(),
    }
}

/// Replaces every `${{ expr }}` in `template` with the expression's value.
pub fn interpolate(template: &str, scope: &Scope) -> Result<String, ExprError> {
    let mut out = String::with_capacity(template.len());
    let mut last = 0;
    for caps in TEMPLATE.captures_iter(template) {
        let whole = caps.get(0).expect("match");
        out.push_str(&template[last..whole.start()]);
        let value = eval(caps.get(1).expect("group").as_str(), scope)?;
        out.push_str(&stringify(&value));
        last = whole.end();
    }
    out.push_str(&template[last..]);
    Ok(out)
}

/// Interpolates every string inside a JSON value.
pub fn interpolate_json(value: &Json, scope: &Scope) -> Result<Json, ExprError> {
    Ok(match value {
        Json::String(s) => {
            // A field that is exactly one template keeps the value's type.
            if let Some(caps) = TEMPLATE.captures(s)
                && caps.get(0).map(|m| m.as_str().len()) == Some(s.trim().len())
            {
                eval(caps.get(1).expect("group").as_str(), scope)?
            } else {
                Json::String(interpolate(s, scope)?)
            }
        }
        Json::Array(items) => Json::Array(items.iter().map(|v| interpolate_json(v, scope)).collect::<Result<_, _>>()?),
        Json::Object(map) => Json::Object(
            map.iter().map(|(k, v)| Ok((k.clone(), interpolate_json(v, scope)?))).collect::<Result<_, ExprError>>()?,
        ),
        other => other.clone(),
    })
}

/// The expressions inside `${{ }}` in a string.
pub fn template_exprs(template: &str) -> Vec<&str> {
    TEMPLATE.captures_iter(template).filter_map(|c| c.get(1).map(|m| m.as_str())).collect()
}

pub fn has_template(text: &str) -> bool {
    TEMPLATE.is_match(text)
}

/// A dotted path an expression reads, up to four segments
/// (`steps.sign_in.outputs.user_id`).
pub type Reference = Vec<String>;

/// The variable paths an expression reads.
pub fn references(expr: &str) -> Result<BTreeSet<Reference>, ExprError> {
    let program = compile(expr)?;
    let mut refs = BTreeSet::new();
    let mut bound = Vec::new();
    walk(program.expression(), &mut refs, &mut bound);
    Ok(refs)
}

fn select_path(e: &IdedExpr) -> Option<Vec<String>> {
    match &e.expr {
        Expr::Ident(name) => Some(vec![name.clone()]),
        Expr::Select(s) if !s.test => {
            let mut p = select_path(&s.operand)?;
            p.push(s.field.clone());
            Some(p)
        }
        _ => None,
    }
}

fn walk(e: &IdedExpr, refs: &mut BTreeSet<Reference>, bound: &mut Vec<String>) {
    if let Some(mut path) = select_path(e) {
        if !bound.contains(&path[0]) {
            path.truncate(4);
            refs.insert(path);
        }
        return;
    }
    match &e.expr {
        Expr::Select(s) => walk(&s.operand, refs, bound),
        Expr::Call(c) => {
            if let Some(t) = &c.target {
                walk(t, refs, bound);
            }
            for a in &c.args {
                walk(a, refs, bound);
            }
        }
        Expr::List(l) => {
            for el in &l.elements {
                walk(el, refs, bound);
            }
        }
        Expr::Map(m) => {
            for entry in &m.entries {
                if let cel::common::ast::EntryExpr::MapEntry(me) = &entry.expr {
                    walk(&me.key, refs, bound);
                    walk(&me.value, refs, bound);
                }
            }
        }
        Expr::Struct(s) => {
            for entry in &s.entries {
                if let cel::common::ast::EntryExpr::StructField(f) = &entry.expr {
                    walk(&f.value, refs, bound);
                }
            }
        }
        Expr::Comprehension(c) => {
            walk(&c.iter_range, refs, bound);
            walk(&c.accu_init, refs, bound);
            bound.push(c.iter_var.clone());
            bound.push(c.accu_var.clone());
            walk(&c.loop_cond, refs, bound);
            walk(&c.loop_step, refs, bound);
            walk(&c.result, refs, bound);
            bound.pop();
            bound.pop();
        }
        Expr::Ident(_) | Expr::Literal(_) | Expr::Unspecified => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn scope() -> Scope {
        let mut s = Scope::new();
        s.set("vars", json!({"base_url": "http://x", "n": 3}));
        s.set("steps", json!({"sign_in": {"outputs": {"user_id": "u-1"}, "status": "passed"}}));
        s
    }

    #[test]
    fn evaluates_paths_and_comparisons() {
        let s = scope();
        assert_eq!(eval("vars.n + 1", &s).unwrap(), json!(4));
        assert!(eval_bool("steps.sign_in.outputs.user_id == 'u-1'", &s).unwrap());
        assert!(eval_bool("vars.base_url.startsWith('http')", &s).unwrap());
    }

    #[test]
    fn interpolates_and_keeps_types_in_json() {
        let s = scope();
        assert_eq!(interpolate("${{ vars.base_url }}/api?n=${{ vars.n }}", &s).unwrap(), "http://x/api?n=3");
        let body = interpolate_json(&json!({"count": "${{ vars.n }}", "label": "n=${{ vars.n }}"}), &s).unwrap();
        assert_eq!(body, json!({"count": 3, "label": "n=3"}));
    }

    #[test]
    fn capture_takes_first_group_and_errors_on_no_match() {
        let s = Scope::new().with("self", json!({"stdout": "created id=abc-9\n"}));
        assert_eq!(eval("self.stdout.capture('id=([a-z0-9-]+)')", &s).unwrap(), json!("abc-9"));
        assert!(eval("self.stdout.capture('missing=(\\\\w+)')", &s).is_err());
    }

    #[test]
    fn missing_key_is_an_error() {
        let s = scope();
        assert!(eval("steps.sign_in.outputs.nope", &s).is_err());
    }

    #[test]
    fn non_bool_condition_is_reported() {
        let s = scope();
        assert!(matches!(eval_bool("vars.n", &s), Err(ExprError::NotBool { .. })));
    }

    #[test]
    fn explains_false_comparisons() {
        let s = Scope::new().with("self", json!({"status": 404}));
        let why = explain_false("self.status == 200", &s).unwrap();
        assert!(why.contains("left = 404"), "{why}");
        assert!(why.contains("right = 200"), "{why}");
    }

    #[test]
    fn collects_references_but_not_comprehension_variables() {
        let refs = references("steps.sign_in.outputs.user_id != '' && vars.items.all(x, x > 0)").unwrap();
        let refs: Vec<String> = refs.into_iter().map(|r| r.join(".")).collect();
        assert_eq!(refs, vec!["steps.sign_in.outputs.user_id", "vars.items"]);
    }

    #[test]
    fn finds_template_expressions() {
        assert_eq!(template_exprs("a ${{ x.y }} b ${{z}}"), vec!["x.y", "z"]);
        assert!(!has_template("plain $HOME"));
    }
}
