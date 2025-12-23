use axum::{Router, ServiceExt, extract::Request};
use configuration::Configuration;
use rmcp::transport::streamable_http_server::session::local::LocalSessionManager;
use rmcp::transport::{StreamableHttpServerConfig, StreamableHttpService};
use rmcp_openapi::Server;
use std::{fs::File, io, net::SocketAddr, path::Path, sync::Arc};
use tokio::net::TcpListener;
use tracing::info;
use url::Url;

pub mod config;
pub mod signal;

static MCP_API_BASE_PATH: &str = "/mcp";

fn build_server() -> io::Result<Server> {
    let openapi_file_path = Path::new("static/catalog-engine.openapi.json");
    let openapi_file = File::open(openapi_file_path).unwrap_or_else(|_| {
        panic!(
            "unable to open OpenAPI specification file at {}",
            openapi_file_path.display()
        )
    });

    let openapi_json = serde_json::from_reader::<_, serde_json::Value>(openapi_file)
        .unwrap_or_else(|_| {
            panic!(
                "unable to parse OpenAPI specification {}",
                openapi_file_path.display()
            )
        });

    let mut server = Server::builder()
        .openapi_spec(openapi_json)
        .base_url(Url::parse("http://localhost:3000/api").unwrap())
        .build();

    server.load_openapi_spec().unwrap_or_else(|e| {
        panic!(
            "unable to load OpenAPI specification from {}: {}",
            openapi_file_path.display(),
            e
        )
    });

    tracing::info!(
        toolCount = server.tool_count(),
        toolList = ?server.get_tool_names(),
        "MCP tools loaded from OpenAPI specification"
    );

    Ok(server)
}

pub async fn try_init(configuration: Configuration) -> io::Result<()> {
    let ip = configuration.server.ip;
    let port = configuration.server.port;

    let server = Arc::new(build_server()?);

    // Create cancellation token for graceful shutdown
    let ct = tokio_util::sync::CancellationToken::new();
    let ct_clone = ct.clone();

    // Spawn shutdown signal handler
    tokio::spawn(async move {
        signal::shutdown_signal().await;
        ct_clone.cancel();
    });

    let service = StreamableHttpService::new(
        move || {
            let server = server.clone();
            Ok((*server).clone())
        },
        Arc::new(LocalSessionManager::default()),
        StreamableHttpServerConfig {
            stateful_mode: false,
            sse_keep_alive: None,
            cancellation_token: ct.clone(),
        },
    );

    let base_url = match configuration.server.api_prefix.as_str() {
        "/" => MCP_API_BASE_PATH.to_string(),
        prefix => format!("{}{}", prefix, MCP_API_BASE_PATH),
    };

    let router = Router::new().nest_service(&base_url, service);

    axum::serve(
        TcpListener::bind(SocketAddr::new(ip, port))
            .await
            .inspect(|listener| {
                if let Ok(local_addr) = listener.local_addr() {
                    info!(%local_addr, "http server listening with MCP endpoint at {}", base_url)
                }
            })?,
        ServiceExt::<Request>::into_make_service(router),
    )
    .with_graceful_shutdown(ct.cancelled_owned())
    .await
}
