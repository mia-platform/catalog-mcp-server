/**
 * Copyright 2026 Mia srl
 *
 * Licensed under the Apache License, Version 2.0 (the "License");
 * you may not use this file except in compliance with the License.
 * You may obtain a copy of the License at
 *
 *     http://www.apache.org/licenses/LICENSE-2.0
 *
 * Unless required by applicable law or agreed to in writing, software
 * distributed under the License is distributed on an "AS IS" BASIS,
 * WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
 * See the License for the specific language governing permissions and
 * limitations under the License.
 *
 * SPDX-License-Identifier: Apache-2.0
 */
use crate::{
    configuration::{Configuration, TransportMode},
    signal,
};
use axum::{Router, ServiceExt as AxumServiceExt, extract::Request};
use rmcp::{
    ServiceExt,
    transport::{
        StreamableHttpServerConfig, StreamableHttpService,
        streamable_http_server::session::local::LocalSessionManager,
    },
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

    match configuration.transport_mode {
        TransportMode::Stdio => run_stdio_server(mcp_server).await,
        TransportMode::Http => {
            run_http_server(
                mcp_server,
                configuration.ip,
                configuration.port,
                configuration.api_prefix,
            )
            .await
        }
    }
}

async fn run_stdio_server(mcp_server: McpServer) -> io::Result<()> {
    tracing::info!("starting MCP server in stdio mode");

    let transport = rmcp::transport::io::stdio();

    let running_service = mcp_server
        .serve(transport)
        .await
        .map_err(io::Error::other)?;

    // Wait for the service to complete (when stdio is closed) or shutdown signal
    tokio::select! {
        result = running_service.waiting() => {
            match result {
                Ok(quit_reason) => {
                    tracing::info!(?quit_reason, "stdio server stopped");
                }
                Err(err) => {
                    tracing::error!(?err, "stdio server task panicked");
                    return Err(io::Error::other(err));
                }
            }
        }
        _ = signal::shutdown_signal() => {
            tracing::info!("received shutdown signal");
        }
    }

    Ok(())
}

async fn run_http_server(
    mcp_server: McpServer,
    ip: std::net::IpAddr,
    port: u16,
    prefix: String,
) -> io::Result<()> {
    let ct = tokio_util::sync::CancellationToken::new();
    let ct_clone = ct.clone();

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
        AxumServiceExt::<Request>::into_make_service(router),
    )
    .with_graceful_shutdown(ct.cancelled_owned())
    .await
}
