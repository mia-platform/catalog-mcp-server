use crate::{configuration::Configuration, signal};
use axum::{Router, ServiceExt, extract::Request};
use rmcp::transport::{
    StreamableHttpServerConfig, StreamableHttpService,
    streamable_http_server::session::local::LocalSessionManager,
};
use rmcp_openapi::Server as McpServer;
use std::{io, net::SocketAddr, sync::Arc};
use tokio::net::TcpListener;

static MCP_API_BASE_PATH: &str = "/mcp";

fn build_router_path(prefix: &str) -> String {
    if prefix.ends_with('/') {
        format!("{}{}", prefix, &MCP_API_BASE_PATH[1..])
    } else {
        format!("{}{}", prefix, MCP_API_BASE_PATH)
    }
}

pub async fn try_init(configuration: Configuration) -> io::Result<()> {
    tracing::info!(%configuration, "starting catalog MCP server");

    let ip = configuration.ip;
    let port = configuration.port;
    let prefix = configuration.api_prefix.clone();

    let mut mcp_server: McpServer = configuration
        .try_into_server()
        .await
        .map_err(io::Error::other)?;

    mcp_server.load_openapi_spec().map_err(io::Error::other)?;

    tracing::debug!(
        tools = %mcp_server.get_tool_names().join(", "),
        stats = %mcp_server.get_tool_stats(),
        "Available tools"
    );

    mcp_server.validate_registry().map_err(io::Error::other)?;

    let ct = tokio_util::sync::CancellationToken::new();
    let ct_clone = ct.clone();

    // Spawn shutdown signal handler
    tokio::spawn(async move {
        signal::shutdown_signal().await;
        ct_clone.cancel();
    });

    let service = StreamableHttpService::new(
        move || Ok(mcp_server.clone()),
        Arc::new(LocalSessionManager::default()),
        StreamableHttpServerConfig {
            stateful_mode: false,
            sse_keep_alive: None,
            cancellation_token: ct.clone(),
        },
    );

    let router_path = build_router_path(&prefix);
    let router = Router::new().nest_service(&router_path, service);

    axum::serve(
        TcpListener::bind(SocketAddr::new(ip, port))
            .await
            .inspect(|listener| {
                if let Ok(local_addr) = listener.local_addr() {
                    tracing::info!(%local_addr, "http server listening with MCP endpoint at {}", router_path)
                }
            })?,
        ServiceExt::<Request>::into_make_service(router),
    )
    .with_graceful_shutdown(ct.cancelled_owned())
    .await
}
