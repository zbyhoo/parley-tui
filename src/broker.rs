use std::net::TcpListener as StdTcpListener;
use std::sync::Arc;

use anyhow::Result;
use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{CallToolResult, Content, ServerCapabilities, ServerInfo};
use rmcp::service::RequestContext;
use rmcp::transport::streamable_http_server::session::local::LocalSessionManager;
use rmcp::transport::streamable_http_server::tower::{
    StreamableHttpServerConfig, StreamableHttpService,
};
use rmcp::{tool, tool_handler, tool_router, RoleServer, ServerHandler};
use serde::Deserialize;
use tokio::sync::oneshot;

use crate::pending::{Outcome, PendingMessage, PendingQueue};
use crate::router::AgentId;

#[derive(Clone)]
pub struct Broker {
    queue: PendingQueue,
    // Używane przez makra #[tool_router]/#[tool_handler] w runtime (analiza tego nie widzi).
    #[allow(dead_code)]
    tool_router: ToolRouter<Self>,
}

#[derive(Deserialize, schemars::JsonSchema)]
struct SendToPeerArgs {
    /// Message to deliver to the other agent.
    message: String,
}

#[tool_router]
impl Broker {
    fn new(queue: PendingQueue) -> Self {
        Self { queue, tool_router: Self::tool_router() }
    }

    #[tool(
        description = "Send a message to the other agent (your peer). Use this both to ask the \
                       peer something AND to reply when the peer messages you — every exchange is \
                       a separate send_to_peer call. The user moderates each message; this call \
                       blocks until they approve or reject, then returns the outcome."
    )]
    async fn send_to_peer(
        &self,
        Parameters(SendToPeerArgs { message }): Parameters<SendToPeerArgs>,
        ctx: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        let from = ctx
            .extensions
            .get::<http::request::Parts>()
            .and_then(|parts| parts.headers.get("x-agent-id"))
            .and_then(|v| v.to_str().ok())
            .and_then(AgentId::from_label);
        let from = match from {
            Some(a) => a,
            None => {
                return Ok(CallToolResult::success(vec![Content::text(
                    "error: missing or unknown X-Agent-Id header",
                )]));
            }
        };
        let to = from.other();
        let (tx, rx) = oneshot::channel();
        self.queue.lock().unwrap().push_back(PendingMessage {
            from,
            to,
            text: message,
            responder: tx,
        });
        let outcome = rx.await.unwrap_or(Outcome::Error("broker shut down".into()));
        let reply = match outcome {
            Outcome::Delivered => format!("delivered to {}", to.label()),
            Outcome::Rejected => "rejected by user".to_string(),
            Outcome::Error(e) => format!("not delivered: {e}"),
        };
        Ok(CallToolResult::success(vec![Content::text(reply)]))
    }
}

#[tool_handler]
impl ServerHandler for Broker {
    fn get_info(&self) -> ServerInfo {
        let mut info = ServerInfo::default();
        info.capabilities = ServerCapabilities::builder().enable_tools().build();
        info.instructions = Some(
            "Use send_to_peer to talk to the other agent. Messages are moderated by the user."
                .into(),
        );
        info
    }
}

/// Uchwyt do działającego brokera.
pub struct BrokerHandle {
    pub port: u16,
    shutdown: Option<oneshot::Sender<()>>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl BrokerHandle {
    /// Zatrzymuje serwer i czeka na zakończenie wątku.
    pub fn shutdown(mut self) {
        if let Some(tx) = self.shutdown.take() {
            let _ = tx.send(());
        }
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// Startuje broker w osobnym wątku z własnym runtime tokio.
/// Wiąże 127.0.0.1:0 synchronicznie, żeby zwrócić faktyczny port od razu.
pub fn start(queue: PendingQueue) -> Result<BrokerHandle> {
    let std_listener = StdTcpListener::bind("127.0.0.1:0")?;
    std_listener.set_nonblocking(true)?;
    let port = std_listener.local_addr()?.port();
    let (shutdown_tx, shutdown_rx) = oneshot::channel();

    let thread = std::thread::Builder::new().name("parley-broker".into()).spawn(move || {
        let rt = match tokio::runtime::Builder::new_current_thread().enable_all().build() {
            Ok(rt) => rt,
            Err(e) => {
                eprintln!("broker runtime failed: {e}");
                return;
            }
        };
        rt.block_on(async move {
            let listener = match tokio::net::TcpListener::from_std(std_listener) {
                Ok(l) => l,
                Err(e) => {
                    eprintln!("broker listener failed: {e}");
                    return;
                }
            };
            let q = queue.clone();
            let service = StreamableHttpService::new(
                move || Ok(Broker::new(q.clone())),
                Arc::new(LocalSessionManager::default()),
                StreamableHttpServerConfig::default(),
            );
            let app = axum::Router::new().nest_service("/mcp", service);
            // Po sygnale porzucamy serwer natychmiast (drop future → wszystkie połączenia
            // zamknięte). Graceful shutdown by zawisł — agenci trzymają trwałe sesje SSE
            // otwarte dopóki żyją, więc nigdy by się nie zdrenowały.
            tokio::select! {
                r = axum::serve(listener, app) => { let _ = r; }
                _ = shutdown_rx => {}
            }
        });
    })?;

    Ok(BrokerHandle { port, shutdown: Some(shutdown_tx), thread: Some(thread) })
}
