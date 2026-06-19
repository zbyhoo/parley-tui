//! Broker-demon headless: stan, rmcp handler, serwer axum.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{CallToolResult, Content, ServerCapabilities, ServerInfo};
use rmcp::service::RequestContext;
use rmcp::{tool, tool_handler, tool_router, RoleServer, ServerHandler};
use serde::Deserialize;

use crate::headless::peer::{RegisterError, Registry, SendOutcome};
use crate::timeline::{Entry, Kind, Timeline, now_ts};

pub struct HubState {
    pub reg: Mutex<Registry>,
    pub token: String,
    pub cwd: String,
    pub timeline: Mutex<Timeline>,
    pub pollers: Mutex<HashSet<String>>,
    pub receivers: Mutex<HashMap<String, Arc<tokio::sync::Mutex<tokio::sync::mpsc::Receiver<crate::headless::peer::IncomingMsg>>>>>,
}

/// Buduje tekst zwracany agentowi po `send_to_peer`.
pub fn send_reply_text(outcomes: &[SendOutcome], has_poller: &dyn Fn(&str) -> bool) -> String {
    if outcomes.is_empty() {
        return "no peers to deliver to".into();
    }
    if outcomes.len() == 1 {
        if let SendOutcome::NotRegistered = &outcomes[0] {
            return "error: you are not a registered peer".into();
        }
        if let SendOutcome::NoSuchPeer { to, present } = &outcomes[0] {
            return format!("no such peer '{to}'; present: [{}]", present.join(", "));
        }
    }
    let parts: Vec<String> = outcomes
        .iter()
        .map(|o| match o {
            SendOutcome::Delivered(id) => {
                if has_poller(id) {
                    format!("delivered to {id}")
                } else {
                    format!("queued for {id}")
                }
            }
            SendOutcome::Queued(id) => format!("queued for {id}"),
            SendOutcome::NoSuchPeer { to, .. } => format!("no such peer {to}"),
            SendOutcome::NotRegistered => "not registered".into(),
        })
        .collect();
    parts.join("; ")
}

#[derive(Clone)]
pub struct HubBroker {
    state: Arc<HubState>,
    #[allow(dead_code)]
    tool_router: ToolRouter<Self>,
}

#[derive(Deserialize, schemars::JsonSchema)]
struct SendArgs {
    /// Peer id to send to, or "all" to broadcast.
    to: String,
    /// Message content.
    message: String,
}

#[derive(Deserialize, schemars::JsonSchema)]
struct NoArgs {}

fn header(ctx: &RequestContext<RoleServer>, name: &str) -> Option<String> {
    ctx.extensions
        .get::<http::request::Parts>()
        .and_then(|p| p.headers.get(name))
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string())
}

#[tool_router]
impl HubBroker {
    pub fn new(state: Arc<HubState>) -> Self {
        Self { state, tool_router: Self::tool_router() }
    }

    #[tool(
        description = "Send a message to a peer agent. `to` is a peer id (see list_peers) or \"all\" \
                       to broadcast. Reply to an incoming message with a separate send_to_peer call \
                       addressed to its sender. Delivery is automatic (no moderation)."
    )]
    async fn send_to_peer(
        &self,
        Parameters(SendArgs { to, message }): Parameters<SendArgs>,
        ctx: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        if header(&ctx, "x-parley-token").as_deref() != Some(self.state.token.as_str()) {
            return Ok(CallToolResult::success(vec![Content::text("error: invalid parley token")]));
        }
        let from = match header(&ctx, "x-agent-id") {
            Some(f) => f,
            None => {
                return Ok(CallToolResult::success(vec![Content::text(
                    "error: missing X-Agent-Id",
                )]));
            }
        };
        // lazy MCP-only rejestracja nadawcy (ścieżka gołego CLI)
        {
            let mut reg = self.state.reg.lock().unwrap();
            if !reg.is_live(&from) {
                if let Err(RegisterError::Collision(id)) = reg.register_mcp_only(&from) {
                    return Ok(CallToolResult::success(vec![Content::text(format!(
                        "error: id {id} in use by a managed peer"
                    ))]));
                }
            }
        }
        let outcomes = self.state.reg.lock().unwrap().route(&from, &to, &message);
        // log: jeden wpis from→to (broadcast: to="all")
        {
            let mut tl = self.state.timeline.lock().unwrap();
            let _ = tl.append(Entry {
                ts: now_ts(),
                from: from.clone(),
                to: to.clone(),
                kind: Kind::Message,
                text: message.clone(),
            });
        }
        let pollers = self.state.pollers.lock().unwrap();
        let reply = send_reply_text(&outcomes, &|id: &str| pollers.contains(id));
        Ok(CallToolResult::success(vec![Content::text(reply)]))
    }

    #[tool(description = "List currently connected peers (id and binary).")]
    async fn list_peers(
        &self,
        Parameters(NoArgs {}): Parameters<NoArgs>,
        _ctx: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        let list = self.state.reg.lock().unwrap().list();
        let text = if list.is_empty() {
            "no peers connected".to_string()
        } else {
            list.iter().map(|(id, bin)| format!("{id} ({bin})")).collect::<Vec<_>>().join("\n")
        };
        Ok(CallToolResult::success(vec![Content::text(text)]))
    }
}

#[tool_handler]
impl ServerHandler for HubBroker {
    fn get_info(&self) -> ServerInfo {
        let mut info = ServerInfo::default();
        info.capabilities = ServerCapabilities::builder().enable_tools().build();
        info.instructions = Some(
            "You are connected to other peer agents. Use list_peers to see who is present. \
             Use send_to_peer(to, message) to talk to one peer (or to=\"all\" to broadcast). \
             When you receive an incoming message, reply with a separate send_to_peer call \
             addressed to its sender."
                .into(),
        );
        info
    }
}

// ─── axum control endpoints + serve() ────────────────────────────────────────

use std::path::PathBuf;
use std::time::Duration;

use axum::extract::{Path as AxPath, Query, State};
use axum::routing::{get, post};
use axum::Json;
use rmcp::transport::streamable_http_server::session::local::LocalSessionManager;
use rmcp::transport::streamable_http_server::tower::{
    StreamableHttpServerConfig, StreamableHttpService,
};
use serde::Serialize;
use std::net::TcpListener as StdTcpListener;

#[derive(serde::Deserialize)]
struct TokenQ {
    token: String,
}

#[derive(serde::Deserialize)]
struct RegisterReq {
    binary: String,
    #[serde(default)]
    r#as: Option<String>,
}

#[derive(Serialize)]
#[allow(dead_code)]
struct RegisterResp {
    id: String,
}

#[derive(serde::Deserialize)]
struct DeregisterReq {
    id: String,
}

#[derive(Serialize)]
struct PollResp {
    message: Option<crate::headless::peer::IncomingMsg>,
}

fn ok_token(state: &HubState, t: &str) -> bool {
    t == state.token
}

async fn health(
    State(state): State<Arc<HubState>>,
    Query(q): Query<TokenQ>,
) -> Json<serde_json::Value> {
    if !ok_token(&state, &q.token) {
        return Json(serde_json::json!({ "error": "invalid token" }));
    }
    Json(serde_json::json!({ "cwd": state.cwd }))
}

async fn register(
    State(state): State<Arc<HubState>>,
    Query(q): Query<TokenQ>,
    Json(req): Json<RegisterReq>,
) -> Json<serde_json::Value> {
    if !ok_token(&state, &q.token) {
        return Json(serde_json::json!({ "error": "invalid token" }));
    }
    let mut reg = state.reg.lock().unwrap();
    match reg.register(&req.binary, req.r#as.as_deref()) {
        Ok((id, rx)) => {
            state
                .receivers
                .lock()
                .unwrap()
                .insert(id.clone(), Arc::new(tokio::sync::Mutex::new(rx)));
            Json(serde_json::json!({ "id": id }))
        }
        Err(RegisterError::Collision(c)) => {
            Json(serde_json::json!({ "error": "collision", "id": c }))
        }
    }
}

async fn poll(
    State(state): State<Arc<HubState>>,
    AxPath(id): AxPath<String>,
    Query(q): Query<TokenQ>,
) -> Json<PollResp> {
    if !ok_token(&state, &q.token) {
        return Json(PollResp { message: None });
    }
    let rx = state.receivers.lock().unwrap().get(&id).cloned();
    let Some(rx) = rx else {
        return Json(PollResp { message: None });
    };
    state.pollers.lock().unwrap().insert(id.clone());
    let mut guard = rx.lock().await;
    let msg = match tokio::time::timeout(Duration::from_secs(25), guard.recv()).await {
        Ok(Some(m)) => Some(m),
        _ => None,
    };
    state.pollers.lock().unwrap().remove(&id);
    Json(PollResp { message: msg })
}

async fn deregister(
    State(state): State<Arc<HubState>>,
    Query(q): Query<TokenQ>,
    Json(req): Json<DeregisterReq>,
) -> Json<serde_json::Value> {
    if ok_token(&state, &q.token) {
        state.reg.lock().unwrap().deregister(&req.id);
        state.receivers.lock().unwrap().remove(&req.id);
        state.pollers.lock().unwrap().remove(&req.id);
    }
    Json(serde_json::json!({ "ok": true }))
}

/// Bind 127.0.0.1:0, build HubState (token, session timeline), write broker.json, serve forever.
/// Foreground; called by `parley __serve`.
pub fn serve(cwd: PathBuf) -> anyhow::Result<()> {
    use crate::headless::discovery::{broker_json_path, random_token, write_atomic, BrokerInfo};

    let std_listener = StdTcpListener::bind("127.0.0.1:0")?;
    std_listener.set_nonblocking(true)?;
    let port = std_listener.local_addr()?.port();
    let token = random_token();

    let cfg = crate::config::Config::load(&cwd)?;
    let state_dir = cfg.state_dir.clone().unwrap_or_else(|| cwd.join(".parley"));
    let session = format!("session-{}", chrono::Local::now().format("%Y%m%d-%H%M%S"));
    let timeline = Timeline::open(&state_dir.join(&session).join("timeline.jsonl"))?;

    let state = Arc::new(HubState {
        reg: Mutex::new(Registry::new()),
        token: token.clone(),
        cwd: cwd.to_string_lossy().into_owned(),
        timeline: Mutex::new(timeline),
        pollers: Mutex::new(HashSet::new()),
        receivers: Mutex::new(HashMap::new()),
    });

    write_atomic(
        &broker_json_path(&state_dir),
        &BrokerInfo {
            port,
            pid: std::process::id(),
            token,
            cwd: state.cwd.clone(),
        },
    )?;

    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
    rt.block_on(async move {
        let listener = tokio::net::TcpListener::from_std(std_listener)?;
        let mcp_state = state.clone();
        let mcp = StreamableHttpService::new(
            move || Ok(HubBroker::new(mcp_state.clone())),
            Arc::new(LocalSessionManager::default()),
            StreamableHttpServerConfig::default(),
        );
        let app = axum::Router::new()
            .nest_service("/mcp", mcp)
            .route("/health", get(health))
            .route("/register", post(register))
            .route("/poll/{id}", get(poll))
            .route("/deregister", post(deregister))
            .with_state(state.clone());
        axum::serve(listener, app).await?;
        Ok::<(), anyhow::Error>(())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reply_delivered_when_poller_present() {
        let live = |id: &str| id == "codex";
        let out = vec![SendOutcome::Delivered("codex".into())];
        assert_eq!(send_reply_text(&out, &live), "delivered to codex");
    }

    #[test]
    fn reply_queued_when_no_poller() {
        let none = |_: &str| false;
        let out = vec![SendOutcome::Delivered("codex".into())];
        assert_eq!(send_reply_text(&out, &none), "queued for codex");
    }

    #[test]
    fn reply_no_such_peer_lists_present() {
        let none = |_: &str| false;
        let out =
            vec![SendOutcome::NoSuchPeer { to: "x".into(), present: vec!["claude".into()] }];
        assert_eq!(send_reply_text(&out, &none), "no such peer 'x'; present: [claude]");
    }

    #[test]
    fn reply_not_registered() {
        let none = |_: &str| false;
        assert_eq!(
            send_reply_text(&[SendOutcome::NotRegistered], &none),
            "error: you are not a registered peer"
        );
    }
}
