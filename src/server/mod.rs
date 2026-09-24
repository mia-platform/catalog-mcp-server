/*
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
use crate::{context::AppState, handler::CatalogHandler, signal, tracing::info};
use axum::{Router, ServiceExt as AxumServiceExt, extract::Request};
use rmcp::transport::{
    StreamableHttpServerConfig, StreamableHttpService,
    streamable_http_server::session::local::LocalSessionManager,
};
use std::{io, net::SocketAddr, sync::Arc, time::Duration};
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;
use tower::{Layer, ServiceBuilder};
use tower_http::{
    normalize_path::NormalizePathLayer,
    request_id::{MakeRequestUuid, SetRequestIdLayer},
};

mod health;
/// The identity extractor layer: `MiaIdentity` into the request's extensions (§6.2, D47).
pub mod identity;
mod metrics;
mod span;

/// Path prefix the operational endpoints are nested under, matching `catalog-engine`.
const OPERATIONAL_PATH_PREFIX: &str = "/-";

/// Builds the SDK transport configuration from ours (§6.1).
///
/// **Every knob is read from configuration, never hardcoded.** The struct is `#[non_exhaustive]`,
/// so these are the builders and never a struct literal.
///
/// Two of these are the difference between a working deployment and an outage:
/// `with_allowed_hosts`, without which every request behind an ingress is answered
/// `403 Forbidden: Host header is not allowed` — which is why an empty list never reaches this
/// function, config validation having already refused to start (D11) — and
/// `enforce_origin_validation`, which stays off while the allowlist is empty because a missing
/// `Origin` is exactly what our in-cluster client sends.
///
/// **`Origin` and `Host` are checked here and nowhere else.** Neither gets a layer of ours:
/// duplicating a security check in two places is how the two drift and the weaker one wins.
fn transport_config(state: &AppState, shutdown: &CancellationToken) -> StreamableHttpServerConfig {
    let server = &state.config.server;
    let transport = &state.config.transport;

    let mut config = StreamableHttpServerConfig::default()
        .with_json_response(transport.json_response)
        .with_legacy_session_mode(transport.legacy_session_mode)
        .with_sse_keep_alive(Some(Duration::from_secs(transport.sse_keep_alive_seconds)))
        .with_allowed_hosts(server.allowed_hosts.clone())
        .with_max_request_body_bytes(server.max_body_bytes)
        .with_cancellation_token(shutdown.child_token());

    if !server.allowed_origins.is_empty() {
        config = config
            .with_allowed_origins(server.allowed_origins.clone())
            .enforce_origin_validation();
    }

    config
}

/// Mounts the MCP service (§6.1).
///
/// The factory is `Fn`, not `FnOnce`, and is called **often** — per session, per request in
/// stateless mode, and once per tool name for the SDK's schema cache — so it clones `Arc`s and
/// does nothing else (D4).
fn mcp_service(
    state: AppState,
    shutdown: &CancellationToken,
) -> StreamableHttpService<CatalogHandler, LocalSessionManager> {
    let config = transport_config(&state, shutdown);

    StreamableHttpService::new(
        move || Ok(CatalogHandler::new(state.clone())),
        Arc::new(LocalSessionManager::default()),
        config,
    )
}

/// Assembles the router.
///
/// **Layer order is a correctness property, not a style choice.** Request id is outermost, so
/// every log line — including one from a request the transport rejects — carries it. The
/// `http.request` span sits inside it, so the id is on the span. The operational endpoints are
/// merged *inside* those two but *outside* everything request-scoped: a probe and a scrape must
/// never need a token, and `/-/healthz` behind a `401` is an outage that reads as a crash-loop.
/// The identity layer therefore wraps **only** the MCP service.
pub fn build_router(state: AppState, shutdown: &CancellationToken) -> Router {
    let auth = state.config.auth.clone();

    // The identity layer wraps **only** the MCP service: it is an extractor and never a gate
    // (D47), but a probe must not travel through anything request-scoped at all.
    let mcp = ServiceBuilder::new()
        .layer(axum::middleware::from_fn(move |request, next| {
            identity::identity_middleware(auth.clone(), request, next)
        }))
        .service(mcp_service(state.clone(), shutdown));

    Router::new()
        .nest(
            OPERATIONAL_PATH_PREFIX,
            health::routes().merge(metrics::routes()),
        )
        .with_state(state.clone())
        .nest_service(&state.config.server.mcp_path, mcp)
        .layer(axum::middleware::from_fn(span::http_request_span))
        .layer(SetRequestIdLayer::x_request_id(MakeRequestUuid))
}

/// Binds the listener and serves until the shutdown signal, then drains.
pub async fn try_init(state: AppState) -> io::Result<()> {
    let ip = state.config.server.ip;
    let port = state.config.server.port;

    // The same token is handed to the transport, so in-flight streams are torn down with the
    // server rather than outliving it (D42).
    let shutdown = CancellationToken::new();
    let router = build_router(state.clone(), &shutdown);

    info!(
        tools = state.registry.tools().len(),
        tools_list_bytes = state.registry.serialised_bytes(),
        tools_list_budget = state.registry.byte_budget(),
        mcp_path = %state.config.server.mcp_path,
        "tool registry built"
    );

    // Every startup condition holds by the time we get here: the configuration was validated
    // before the runtime started (D40) and the `tools/list` payload is prebuilt. Readiness is
    // raised now, and lowered again *before* the drain begins, so the endpoint stops receiving
    // traffic while in-flight calls finish (D43).
    state.readiness.mark_ready();

    let graceful = {
        let readiness = state.readiness.clone();
        let shutdown = shutdown.clone();
        async move {
            signal::shutdown_signal().await;
            info!("shutdown signal received, draining");
            readiness.mark_draining();
            shutdown.cancel();
        }
    };

    let make_service = AxumServiceExt::<Request>::into_make_service(
        NormalizePathLayer::trim_trailing_slash().layer(router),
    );

    axum::serve(
        TcpListener::bind(SocketAddr::new(ip, port))
            .await
            .inspect(|listener| {
                if let Ok(local_addr) = listener.local_addr() {
                    info!(%local_addr, "http server listening")
                }
            })?,
        make_service,
    )
    .with_graceful_shutdown(graceful)
    .await
}

#[cfg(test)]
mod tests;
