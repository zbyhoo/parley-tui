use std::sync::Arc;

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

#[derive(Clone)]
struct EchoServer {
    #[allow(dead_code)]
    tool_router: ToolRouter<Self>,
}

#[derive(Deserialize, schemars::JsonSchema)]
struct EchoArgs {
    text: String,
}

#[tool_router]
impl EchoServer {
    fn new() -> Self {
        Self { tool_router: Self::tool_router() }
    }

    #[tool(description = "Echo text back; logs the calling agent id")]
    async fn echo(
        &self,
        Parameters(EchoArgs { text }): Parameters<EchoArgs>,
        ctx: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        let agent = ctx
            .extensions
            .get::<http::request::Parts>()
            .and_then(|parts| parts.headers.get("x-agent-id"))
            .and_then(|v| v.to_str().ok())
            .unwrap_or("<unknown>")
            .to_string();
        eprintln!("[broker_spike] echo from agent='{agent}': {text}");
        Ok(CallToolResult::success(vec![Content::text(format!(
            "agent={agent} echo={text}"
        ))]))
    }
}

#[tool_handler]
impl ServerHandler for EchoServer {
    fn get_info(&self) -> ServerInfo {
        let mut info = ServerInfo::default();
        info.capabilities = ServerCapabilities::builder().enable_tools().build();
        info
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let service = StreamableHttpService::new(
        || Ok(EchoServer::new()),
        Arc::new(LocalSessionManager::default()),
        StreamableHttpServerConfig::default(),
    );
    let app = axum::Router::new().nest_service("/mcp", service);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let port = listener.local_addr()?.port();
    eprintln!("[broker_spike] MCP server on http://127.0.0.1:{port}/mcp");
    eprintln!("[broker_spike] configure a CLI to connect, then call the `echo` tool");
    axum::serve(listener, app).await?;
    Ok(())
}
