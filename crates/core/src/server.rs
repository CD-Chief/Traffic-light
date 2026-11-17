use std::net::{Ipv4Addr, SocketAddr};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use axum::body::Bytes;
use axum::extract::rejection::BytesRejection;
use axum::extract::{DefaultBodyLimit, State as AxumState};
use axum::http::HeaderMap;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde_json::{json, Value};

use crate::state::{HookEvent, Session, Store, Transition};

pub const DEFAULT_PORT: u16 = 7890;
/// Optional shared-secret header (see README, "Optional hardening").
pub const TOKEN_HEADER: &str = "x-traffic-light-token";

/// Tool results can be large; accept them rather than answer 413, because the
/// widget must never be the reason a hook errors.
const MAX_BODY_BYTES: usize = 64 * 1024 * 1024;
const SWEEP_EVERY: Duration = Duration::from_secs(60);

type OnChange = dyn Fn(&[Session], Option<&Transition>) + Send + Sync;

/// State shared between the HTTP handlers, the stale-session sweeper and
/// whoever renders the lights.
pub struct Shared {
    store: Mutex<Store>,
    token: Option<String>,
    on_change: Box<OnChange>,
}

impl Shared {
    /// `on_change` is called with the full session list after every visible
    /// change. It runs on the server task, so keep it quick.
    pub fn new(
        token: Option<String>,
        on_change: impl Fn(&[Session], Option<&Transition>) + Send + Sync + 'static,
    ) -> Arc<Self> {
        Arc::new(Self {
            store: Mutex::new(Store::new()),
            token: token.filter(|t| !t.is_empty()),
            on_change: Box::new(on_change),
        })
    }

    pub fn sessions(&self) -> Vec<Session> {
        self.lock().list()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Store> {
        self.store.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn handle(&self, ev: &HookEvent) {
        let (transition, list) = {
            let mut store = self.lock();
            match store.apply(ev, now_ms()) {
                Some(t) => (t, store.list()),
                None => return,
            }
        };
        (self.on_change)(&list, Some(&transition));
    }

    fn sweep(&self) {
        let list = {
            let mut store = self.lock();
            if !store.sweep(now_ms()) {
                return;
            }
            store.list()
        };
        (self.on_change)(&list, None);
    }
}

pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

pub fn router(shared: Arc<Shared>) -> Router {
    Router::new()
        .route("/event", post(event))
        .route("/health", get(|| async { "ok" }))
        .layer(DefaultBodyLimit::max(MAX_BODY_BYTES))
        .with_state(shared)
}

/// Always `200 {}`, whatever arrives: Claude Code waits on this response and
/// an empty object means "carry on, no decision".
async fn event(
    AxumState(shared): AxumState<Arc<Shared>>,
    headers: HeaderMap,
    body: Result<Bytes, BytesRejection>,
) -> Json<Value> {
    let authorised = match &shared.token {
        None => true,
        Some(token) => headers
            .get(TOKEN_HEADER)
            .is_some_and(|v| v.as_bytes() == token.as_bytes()),
    };
    if authorised {
        if let Ok(body) = body {
            // The body is parsed straight into the handful of fields we keep
            // and then dropped. It is never logged or stored.
            if let Ok(ev) = serde_json::from_slice::<HookEvent>(&body) {
                shared.handle(&ev);
            }
        }
    }
    Json(json!({}))
}

/// Bind 127.0.0.1 only (never 0.0.0.0) and serve until the task is dropped.
pub async fn serve(port: u16, shared: Arc<Shared>) -> std::io::Result<()> {
    let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
    let listener = tokio::net::TcpListener::bind(addr).await?;

    let sweeper = shared.clone();
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(SWEEP_EVERY);
        loop {
            tick.tick().await;
            sweeper.sweep();
        }
    });

    axum::serve(listener, router(shared)).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::State;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt;

    async fn post_event(app: &Router, body: &str, token: Option<&str>) -> (StatusCode, String) {
        let mut req = Request::post("/event").header("content-type", "application/json");
        if let Some(t) = token {
            req = req.header(TOKEN_HEADER, t);
        }
        let res = app
            .clone()
            .oneshot(req.body(Body::from(body.to_owned())).unwrap())
            .await
            .unwrap();
        let status = res.status();
        let bytes = axum::body::to_bytes(res.into_body(), 1024).await.unwrap();
        (status, String::from_utf8(bytes.to_vec()).unwrap())
    }

    const PROMPT: &str = r#"{"session_id":"s1","hook_event_name":"UserPromptSubmit","cwd":"/code/app","prompt":"secret"}"#;

    #[tokio::test]
    async fn event_updates_state_and_answers_empty_object() {
        let shared = Shared::new(None, |_, _| {});
        let app = router(shared.clone());
        assert_eq!(
            post_event(&app, PROMPT, None).await,
            (StatusCode::OK, "{}".into())
        );
        let s = shared.sessions();
        assert_eq!((s[0].label.as_str(), s[0].state), ("app", State::Working));
    }

    #[tokio::test]
    async fn malformed_bodies_still_get_200() {
        let shared = Shared::new(None, |_, _| {});
        let app = router(shared.clone());
        for body in ["", "not json", "[1,2]", r#"{"session_id":7}"#] {
            assert_eq!(
                post_event(&app, body, None).await,
                (StatusCode::OK, "{}".into())
            );
        }
        assert!(shared.sessions().is_empty());
    }

    #[tokio::test]
    async fn token_is_enforced_quietly_when_configured() {
        let shared = Shared::new(Some("hunter2".into()), |_, _| {});
        let app = router(shared.clone());
        assert_eq!(post_event(&app, PROMPT, None).await.0, StatusCode::OK);
        assert_eq!(
            post_event(&app, PROMPT, Some("wrong")).await.0,
            StatusCode::OK
        );
        assert!(shared.sessions().is_empty());
        post_event(&app, PROMPT, Some("hunter2")).await;
        assert_eq!(shared.sessions().len(), 1);
    }

    #[tokio::test]
    async fn question_pre_tool_use_goes_amber_and_drops_tool_input() {
        let shared = Shared::new(None, |_, _| {});
        let app = router(shared.clone());
        let body = r#"{"session_id":"s1","hook_event_name":"PreToolUse","cwd":"/code/app","tool_name":"AskUserQuestion","tool_use_id":"toolu_01ABC","tool_input":{"questions":[{"question":"Which database should we use?","options":["pg","sqlite"]}]}}"#;
        assert_eq!(
            post_event(&app, body, None).await,
            (StatusCode::OK, "{}".into())
        );
        let s = shared.sessions();
        assert_eq!(s[0].state, State::NeedsYou);
        let json = serde_json::to_string(&s).unwrap();
        assert!(!json.contains("tool_input"));
        assert!(!json.contains("Which database"));
    }

    #[tokio::test]
    async fn health() {
        let app = router(Shared::new(None, |_, _| {}));
        let res = app
            .oneshot(Request::get("/health").body(Body::empty()).unwrap())
            .await
            .unwrap();
        let bytes = axum::body::to_bytes(res.into_body(), 16).await.unwrap();
        assert_eq!(&bytes[..], b"ok");
    }
}
