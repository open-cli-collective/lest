//! The local server behind Lest's UI: a JSON API, a server-sent event stream
//! of run events, run files with range requests, and the embedded UI.
//!
//! It binds loopback only. Every request needs a loopback `Host` header,
//! which blocks DNS-rebinding pages. API requests need the per-launch token,
//! either as the `lest_token_<port>` cookie (set when the UI is opened with
//! `?token=`) or as a bearer token; the app shell and its assets load
//! without it and carry no data. Requests that change something and arrive
//! with the cookie must come from the UI's own origin, because a page on
//! another local port counts as same-site and would send the cookie too.

mod api;
mod terminal;

use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, RwLock};
use std::time::Instant;

use axum::Router;
use axum::body::Body;
use axum::extract::{Request, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Redirect, Response};
use lest_core::event::RunEvent;
use lest_core::paths::StateRoots;
use lest_core::project::Project;
use lest_core::runner::{BrowserDriver, RunCancel};
use lest_core::secrets::Keyring;
use lest_core::store::Store;
use rust_embed::RustEmbed;
use tokio::sync::broadcast;

#[derive(RustEmbed)]
#[folder = "../../ui/dist"]
struct Ui;

/// Builds the browser driver once the server's origin is known (the live
/// view's DevTools socket accepts only that origin).
pub type BrowserFactory = Arc<dyn Fn(Option<String>) -> Arc<dyn BrowserDriver> + Send + Sync>;

pub struct Options {
    pub project: Project,
    pub roots: StateRoots,
    /// 0 picks a free port.
    pub port: u16,
    pub token: String,
    pub keyring: Arc<dyn Keyring>,
    pub browser: BrowserFactory,
}

pub(crate) struct LiveRun {
    pub flow_id: String,
    pub started_at: String,
    pub environment: Option<String>,
    pub cancel: RunCancel,
    pub events: Vec<RunEvent>,
    pub done: bool,
    pub started: Instant,
}

pub(crate) struct AppState {
    pub project: RwLock<Project>,
    /// The cookie's name carries the port, so two `lest ui` processes do
    /// not overwrite each other's token.
    pub cookie: String,
    pub roots: StateRoots,
    pub store: Store,
    pub keyring: Arc<dyn Keyring>,
    pub browser: Arc<dyn BrowserDriver>,
    pub token: String,
    pub runs: Mutex<HashMap<String, LiveRun>>,
    pub events: broadcast::Sender<Arc<RunEvent>>,
}

pub struct Server {
    pub addr: SocketAddr,
    pub token: String,
    router: Router,
    listener: tokio::net::TcpListener,
}

impl Server {
    /// The URL that opens the UI and hands it the token.
    pub fn url(&self) -> String {
        format!("http://{}/?token={}", self.addr, self.token)
    }

    pub async fn serve(self) -> std::io::Result<()> {
        axum::serve(self.listener, self.router).await
    }
}

/// A random token for one launch.
pub fn new_token() -> String {
    (0..32).map(|_| format!("{:x}", rand::random_range(0..16u8))).collect()
}

pub async fn bind(opts: Options) -> anyhow::Result<Server> {
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", opts.port)).await?;
    let addr = listener.local_addr()?;
    let origin = format!("http://{addr}");
    let (events, _) = broadcast::channel(4096);
    let state = Arc::new(AppState {
        project: RwLock::new(opts.project),
        store: Store::new(opts.roots.clone()),
        roots: opts.roots,
        keyring: opts.keyring,
        cookie: format!("lest_token_{}", addr.port()),
        // The live view may run from either loopback name.
        browser: (opts.browser)(Some(format!("{origin},http://localhost:{}", addr.port()))),
        token: opts.token.clone(),
        runs: Mutex::new(HashMap::new()),
        events,
    });
    let router = Router::new()
        .nest("/api", api::routes())
        .fallback(static_file)
        .layer(middleware::from_fn_with_state(state.clone(), guard))
        .with_state(state);
    Ok(Server { addr, token: opts.token, router, listener })
}

fn is_loopback_host(headers: &HeaderMap) -> bool {
    let Some(host) = headers.get(header::HOST).and_then(|h| h.to_str().ok()) else { return false };
    let name = match host.rsplit_once(':') {
        Some((n, port)) if port.chars().all(|c| c.is_ascii_digit()) => n,
        _ => host,
    };
    matches!(name, "127.0.0.1" | "localhost" | "[::1]")
}

fn cookie_token(headers: &HeaderMap, name: &str) -> Option<String> {
    headers.get_all(header::COOKIE).iter().filter_map(|v| v.to_str().ok()).flat_map(|v| v.split(';')).find_map(|c| {
        let (k, v) = c.trim().split_once('=')?;
        (k == name).then(|| v.to_string())
    })
}

/// A cookie-authenticated request that changes something must come from
/// the UI itself: its `Origin` (or `Sec-Fetch-Site`) must match.
fn same_origin(headers: &HeaderMap) -> bool {
    let host = headers.get(header::HOST).and_then(|h| h.to_str().ok()).unwrap_or("");
    if let Some(origin) = headers.get(header::ORIGIN).and_then(|o| o.to_str().ok()) {
        return origin == format!("http://{host}");
    }
    match headers.get("sec-fetch-site").and_then(|v| v.to_str().ok()) {
        Some(site) => site == "same-origin",
        // No browser headers at all: not a cross-site browser request.
        None => true,
    }
}

fn bearer(headers: &HeaderMap) -> Option<String> {
    headers.get(header::AUTHORIZATION)?.to_str().ok()?.strip_prefix("Bearer ").map(str::to_string)
}

/// Equal-length comparison that does not stop at the first difference.
fn same(a: &str, b: &str) -> bool {
    a.len() == b.len() && a.bytes().zip(b.bytes()).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

async fn guard(State(state): State<Arc<AppState>>, req: Request, next: Next) -> Response {
    if !is_loopback_host(req.headers()) {
        return (StatusCode::FORBIDDEN, "lest only answers requests addressed to localhost").into_response();
    }
    // Opening the UI with ?token= trades it for a cookie and drops it from
    // the address bar.
    if let Some(q) = req.uri().query()
        && let Some(t) = q.split('&').find_map(|kv| kv.strip_prefix("token="))
    {
        if same(t, &state.token) {
            let mut resp = Redirect::to(req.uri().path()).into_response();
            let cookie = format!("{}={}; Path=/; HttpOnly; SameSite=Strict", state.cookie, state.token);
            resp.headers_mut().insert(header::SET_COOKIE, HeaderValue::from_str(&cookie).expect("cookie"));
            return resp;
        }
        return (StatusCode::UNAUTHORIZED, "wrong token").into_response();
    }
    let by_bearer = bearer(req.headers()).is_some_and(|t| same(&t, &state.token));
    let by_cookie = cookie_token(req.headers(), &state.cookie).is_some_and(|t| same(&t, &state.token));
    let path = req.uri().path().to_string();
    if !(by_bearer || by_cookie) && (path.starts_with("/api/") || path == "/" || path.ends_with(".html")) {
        return (
            StatusCode::UNAUTHORIZED,
            "Open Lest with the address `lest ui` printed (it carries this server's token).",
        )
            .into_response();
    }
    let changes = !matches!(*req.method(), axum::http::Method::GET | axum::http::Method::HEAD);
    if changes && !by_bearer && !same_origin(req.headers()) {
        return (StatusCode::FORBIDDEN, "requests that change something must come from the Lest UI").into_response();
    }
    let mut resp = next.run(req).await;
    let h = resp.headers_mut();
    // Routes can set a stricter policy (run files do).
    if !h.contains_key(header::CONTENT_SECURITY_POLICY) {
        h.insert(
            header::CONTENT_SECURITY_POLICY,
            HeaderValue::from_static(
                "default-src 'self'; img-src 'self' data: blob:; media-src 'self' blob:; style-src 'self' 'unsafe-inline'; \
                 connect-src 'self' ws://127.0.0.1:* ws://localhost:*; frame-ancestors 'none'; base-uri 'none'; form-action 'self'",
            ),
        );
    }
    h.insert(header::X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff"));
    h.insert("referrer-policy", HeaderValue::from_static("no-referrer"));
    resp
}

async fn static_file(req: Request) -> Response {
    let path = req.uri().path().trim_start_matches('/');
    let path = if path.is_empty() { "index.html" } else { path };
    let (file, name) = match Ui::get(path) {
        Some(f) => (f, path.to_string()),
        // Client-side routes all serve the app shell.
        None => match Ui::get("index.html") {
            Some(f) => (f, "index.html".to_string()),
            None => return StatusCode::NOT_FOUND.into_response(),
        },
    };
    let mime = lest_core::runner::mime_for_name(&name);
    let mut resp = Response::new(Body::from(file.data.into_owned()));
    resp.headers_mut().insert(header::CONTENT_TYPE, HeaderValue::from_str(mime).expect("mime"));
    if name.starts_with("assets/") {
        resp.headers_mut()
            .insert(header::CACHE_CONTROL, HeaderValue::from_static("public, max-age=31536000, immutable"));
    } else {
        resp.headers_mut().insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
    }
    resp
}

pub(crate) fn run_dir_of(state: &AppState, run_id: &str) -> Option<PathBuf> {
    if let Some(live) = state.runs.lock().expect("lock").get(run_id) {
        return Some(state.store.run_dir(&live.flow_id, run_id));
    }
    state.store.find(run_id).ok().map(|r| state.store.run_dir(&r.flow_id, &r.run_id))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_loopback_hosts_pass() {
        let mut h = HeaderMap::new();
        for (host, ok) in [
            ("127.0.0.1:4100", true),
            ("localhost:4100", true),
            ("[::1]:4100", true),
            ("evil.example.com", false),
            ("127.0.0.1.evil.com:80", false),
        ] {
            h.insert(header::HOST, HeaderValue::from_static(host));
            assert_eq!(is_loopback_host(&h), ok, "{host}");
        }
    }

    #[test]
    fn reads_the_token_cookie() {
        let mut h = HeaderMap::new();
        h.insert(header::COOKIE, HeaderValue::from_static("a=1; lest_token_4100=abc; lest_token_4200=def"));
        assert_eq!(cookie_token(&h, "lest_token_4200").as_deref(), Some("def"));
    }

    #[test]
    fn changes_need_the_ui_origin() {
        let mut h = HeaderMap::new();
        h.insert(header::HOST, HeaderValue::from_static("127.0.0.1:4100"));
        assert!(same_origin(&h));
        h.insert(header::ORIGIN, HeaderValue::from_static("http://127.0.0.1:4100"));
        assert!(same_origin(&h));
        h.insert(header::ORIGIN, HeaderValue::from_static("http://127.0.0.1:4200"));
        assert!(!same_origin(&h));
        h.remove(header::ORIGIN);
        h.insert("sec-fetch-site", HeaderValue::from_static("same-site"));
        assert!(!same_origin(&h));
    }
}
