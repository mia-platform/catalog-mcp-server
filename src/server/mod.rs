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
use crate::{context::AppState, signal, tracing::info};
use axum::{Router, ServiceExt as AxumServiceExt, extract::Request};
use std::{io, net::SocketAddr};
use tokio::net::TcpListener;
use tower::Layer;
use tower_http::normalize_path::NormalizePathLayer;

mod health;

/// Path prefix the operational endpoints are nested under, matching `catalog-engine`.
const OPERATIONAL_PATH_PREFIX: &str = "/-";

/// Assembles the router.
///
/// **Layer order is a correctness property, not a style choice.** The operational endpoints are
/// merged *outside* everything request-scoped: a probe must never need a token, and
/// `/-/healthz` behind a `401` is an outage that reads as a crash-loop. The MCP service, the
/// identity layer that wraps only it, the request-id layer and the `http.request` span layer
/// join this function in Step 1 — in that order, outermost first — and the comment moves with
/// them rather than being rewritten.
pub fn build_router(state: AppState) -> Router {
    Router::new()
        .nest(OPERATIONAL_PATH_PREFIX, health::routes())
        .with_state(state)
}

/// Binds the listener and serves until the shutdown signal, then drains.
pub async fn try_init(state: AppState) -> io::Result<()> {
    let ip = state.config.server.ip;
    let port = state.config.server.port;

    let router = build_router(state.clone());

    // Every startup condition holds by the time we get here: the configuration was validated
    // before the runtime started (D40). Readiness is raised now, and lowered again *before* the
    // drain begins, so the endpoint stops receiving traffic while in-flight calls finish (D43).
    state.readiness.mark_ready();

    let graceful = {
        let readiness = state.readiness.clone();
        async move {
            signal::shutdown_signal().await;
            info!("shutdown signal received, draining");
            readiness.mark_draining();
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
